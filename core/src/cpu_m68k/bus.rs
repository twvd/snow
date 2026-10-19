//! M68k CPU - Bus access functionality

use crate::bus::{Address, Bus, IrqSource};
use crate::cpu_m68k::FpuM68kType;
use crate::cpu_m68k::bus_transaction::{FaultInfo, bus_transaction};
use crate::cpu_m68k::cpu::{
    Breakpoint, BusBreakpoint, CpuError, CpuM68k, Group0Details, WriteBack040,
};
use crate::cpu_m68k::{CpuM68kType, CpuSized, M68000, M68020, TORDER_HIGHLOW, TORDER_LOWHIGH};
use crate::tickable::Ticks;
use crate::types::Long;
use crate::types::Word;

use anyhow::{Result, anyhow, bail};

// M68k UM 3.8
pub const FC_UNUSED: u8 = 0;
pub const FC_USER_DATA: u8 = 1;
pub const FC_USER_PROGRAM: u8 = 2;
pub const FC_SUPERVISOR_DATA: u8 = 5;
pub const FC_SUPERVISOR_PROGRAM: u8 = 6;
pub const FC_MASK: u8 = 0b1111;

/// What a CPU access is for
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessKind {
    /// Operand read or write, including PC relative operands and MOVES
    Data,
    /// Instruction prefetch
    Fetch,
    /// Access to a physical address: translation table searches and
    /// descriptor updates
    Physical,
}

/// The part of an operand access from logical address `laddr` on, as it is
/// translated and accessed
#[derive(Clone, Copy)]
pub struct CpuAccess {
    pub kind: AccessKind,
    /// The operand, for bus errors
    pub fault: FaultInfo,
    pub laddr: Address,
    /// Bytes left in the operand from `laddr`
    pub size: u32,
    /// Write data, right-aligned to `size`
    pub wdata: Option<Long>,
}

/// Low `bytes` bytes of `val`
pub(in crate::cpu_m68k) const fn low_bytes(val: Long, bytes: u32) -> Long {
    if bytes >= 4 {
        val
    } else {
        val & ((1 << (bytes * 8)) - 1)
    }
}

impl<
    TBus,
    const ADDRESS_MASK: Address,
    const CPU_TYPE: CpuM68kType,
    const FPU_TYPE: FpuM68kType,
    const PMMU: bool,
> CpuM68k<TBus, ADDRESS_MASK, CPU_TYPE, FPU_TYPE, PMMU>
where
    TBus: Bus<Address, u8> + IrqSource,
{
    #[inline(always)]
    fn fc_data(&self) -> u8 {
        if PMMU {
            if self.regs.sr.supervisor() {
                FC_SUPERVISOR_DATA
            } else {
                FC_USER_DATA
            }
        } else {
            FC_UNUSED
        }
    }

    #[inline(always)]
    pub(in crate::cpu_m68k) fn fc_program(&self) -> u8 {
        if PMMU {
            if self.regs.sr.supervisor() {
                FC_SUPERVISOR_PROGRAM
            } else {
                FC_USER_PROGRAM
            }
        } else {
            FC_UNUSED
        }
    }

    /// Checks if an access needs to fail and raise bus error on alignment errors
    fn verify_access_physical<T: CpuSized>(&self, paddr: Address, read: bool) -> Result<()> {
        if std::mem::size_of::<T>() >= 2 && (paddr & 1) != 0 {
            // Unaligned access
            if CPU_TYPE < M68020 {
                log::warn!("Unaligned access: address {:08X}", paddr);

                // TODO should still happen on 68020+ for PC
                bail!(CpuError::AddressError(Group0Details {
                    function_code: 0,
                    ir: 0,
                    size: std::mem::size_of::<T>(),
                    atc_fault: false,

                    // TODO instruction bit
                    instruction: false,
                    read,
                    address: paddr,

                    // Filled in later
                    start_pc: 0,
                }));
            }
        }
        Ok(())
    }

    /// Reads a value from the bus and spends ticks.
    /// Virtual address version, data FC
    pub(in crate::cpu_m68k) fn read_ticks<T: CpuSized>(&mut self, vaddr: Address) -> Result<T> {
        self.read_ticks_generic::<T>(AccessKind::Data, self.fc_data(), vaddr)
    }

    /// Reads a value from the bus and spends ticks.
    /// Virtual address version, program FC
    pub(in crate::cpu_m68k) fn read_ticks_program<T: CpuSized>(
        &mut self,
        vaddr: Address,
    ) -> Result<T> {
        self.read_ticks_generic::<T>(AccessKind::Fetch, self.fc_program(), vaddr)
    }

    /// Reads a value from the bus and spends ticks.
    /// Physical address version
    pub(in crate::cpu_m68k) fn read_ticks_physical<T: CpuSized>(
        &mut self,
        o_paddr: Address,
    ) -> Result<T> {
        // FC unused for physical addressing
        self.read_ticks_generic::<T>(AccessKind::Physical, FC_UNUSED, o_paddr)
    }

    /// Triggers data access breakpoints
    fn data_breakpoint(&mut self, write: bool, addr: Address, val: u8) {
        let kind = if write {
            BusBreakpoint::Write
        } else {
            BusBreakpoint::Read
        };
        if self.breakpoints.iter().any(|bp| {
            *bp == Breakpoint::Bus(kind, addr)
                || *bp == Breakpoint::Bus(BusBreakpoint::ReadWrite, addr)
        }) {
            log::info!(
                "Breakpoint hit (bus {}): ${:08X}, value: ${:02X}, PC: ${:08X}",
                if write { "write" } else { "read" },
                addr,
                val,
                self.regs.pc,
            );
            self.breakpoint_hit.set();
        }
    }

    /// Translates the logical address of an access
    #[inline(always)]
    fn translate(&mut self, acc: &CpuAccess) -> Result<Address> {
        if acc.kind == AccessKind::Physical || !PMMU {
            return Ok(acc.laddr);
        }
        self.pmmu_translate(acc.fault.fc, acc.laddr, acc.fault.write)
            .map_err(|e| match e.downcast_ref() {
                Some(CpuError::BusError(details)) => {
                    anyhow!(CpuError::BusError(Group0Details {
                        size: acc.fault.size,
                        address: acc.laddr,
                        ..*details
                    }))
                }
                _ => e,
            })
    }

    /// Runs a CPU access, up to the end of the long word at most: translates
    /// it and runs its bus cycles. Adds their duration to `ticks` and returns
    /// the read data and the number of bytes transferred.
    #[inline(always)]
    fn cpu_access(&mut self, acc: &CpuAccess, ticks: &mut Ticks) -> Result<(Long, u32)> {
        // The bus controller finishes the last write first
        self.write_credit = 0;
        let paddr = self.translate(acc)?;
        let (data, len) =
            bus_transaction!(self, ticks).access(&acc.fault, paddr, acc.size, acc.wdata)?;

        // Data breakpoints, on the physical address of the access
        if !self.breakpoints.is_empty() {
            for i in 0..len {
                let val = match acc.wdata {
                    Some(d) => d >> (8 * (acc.size - 1 - i)),
                    None => data >> (8 * (len - 1 - i)),
                };
                self.data_breakpoint(acc.fault.write, paddr.wrapping_add(i), val as u8);
            }
        }
        Ok((data, len))
    }

    /// Reads a value from the bus and spends ticks.
    #[inline(always)]
    pub(in crate::cpu_m68k) fn read_ticks_generic<T: CpuSized>(
        &mut self,
        kind: AccessKind,
        fc: u8,
        o_addr: Address,
    ) -> Result<T> {
        let len = std::mem::size_of::<T>() as u32;
        let mut result: u64 = 0;
        let fault = FaultInfo {
            fc,
            o_addr,
            size: std::mem::size_of::<T>(),
            write: false,
        };

        if CPU_TYPE < M68020 {
            // The 68000 moves a long operand as two word accesses, with the
            // address error check after the first one
            let addr = if CPU_TYPE == M68000 && len > 1 {
                o_addr & !1
            } else {
                o_addr
            };
            for w in 0..len.div_ceil(2) {
                let mut ticks = 0;
                let acc = CpuAccess {
                    kind,
                    fault,
                    laddr: addr.wrapping_add(2 * w) & ADDRESS_MASK,
                    size: len.min(2),
                    wdata: None,
                };
                let (data, n) = self.cpu_access(&acc, &mut ticks)?;
                result = (result << (8 * n)) | u64::from(data);
                self.advance_cycles(ticks)?;

                if CPU_TYPE == M68000 && w == 0 && len > 1 {
                    // Address errors occur AFTER the first Word was accessed
                    self.verify_access_physical::<T>(o_addr, true)?;
                }
            }
        } else {
            result = u64::from(self.read_operand(kind, fault, len)?);
        }

        Ok(T::chop(result as Long))
    }

    /// Reads an operand of `len` bytes (1-4, a 3-byte one included) with
    /// as many bus cycles as the port sizes need (68020+).
    #[inline(always)]
    fn read_operand(&mut self, kind: AccessKind, fault: FaultInfo, len: u32) -> Result<Long> {
        let o_addr = fault.o_addr;
        if (o_addr & 3) + len <= 4 {
            // The operand is within a long word: a single access
            let mut ticks = 0;
            let acc = CpuAccess {
                kind,
                fault,
                laddr: o_addr & ADDRESS_MASK,
                size: len,
                wdata: None,
            };
            let (data, _) = self.cpu_access(&acc, &mut ticks)?;
            self.advance_cycles(ticks)?;
            return Ok(data);
        }

        let mut ticks = 0;
        let mut done = 0;
        let mut result: u64 = 0;
        while done < len {
            // An access goes at most to the end of a long word: the rest
            // of the operand may be on another page, it is translated on
            // its own. We pass cpu_access() the full size left in the
            // operand, not just up to the next long word boundary, as the
            // 68020 and 68030 show it on SIZ.
            let acc = CpuAccess {
                kind,
                fault,
                laddr: o_addr.wrapping_add(done) & ADDRESS_MASK,
                size: len - done,
                wdata: None,
            };
            let (data, n) = self.cpu_access(&acc, &mut ticks)?;
            result = (result << (8 * n)) | u64::from(data);
            done += n;
        }
        self.advance_cycles(ticks)?;
        Ok(result as Long)
    }

    /// Reads a data operand of `len` bytes (1-4) and spends ticks (68020+).
    /// Virtual address version, data FC
    pub(in crate::cpu_m68k) fn read_ticks_len(&mut self, vaddr: Address, len: u32) -> Result<Long> {
        let fault = FaultInfo {
            fc: self.fc_data(),
            o_addr: vaddr,
            size: len as usize,
            write: false,
        };
        self.read_operand(AccessKind::Data, fault, len)
    }

    /// Writes a value to the bus and spends ticks.
    #[inline(always)]
    pub(in crate::cpu_m68k) fn write_ticks_order_generic<T: CpuSized, const TORDER: usize>(
        &mut self,
        kind: AccessKind,
        fc: u8,
        o_addr: Address,
        value: T,
    ) -> Result<()> {
        let len = std::mem::size_of::<T>() as u32;
        let value = value.expand();
        let fault = FaultInfo {
            fc,
            o_addr,
            size: std::mem::size_of::<T>(),
            write: true,
        };

        if CPU_TYPE < M68020 {
            // The 68000 moves a long operand as two word accesses. Their
            // order and the address error check after the first one belong
            // to the instruction, not to the bus: a long written to -(An)
            // has its low word written first (TORDER_HIGHLOW).
            let addr = if CPU_TYPE == M68000 && len > 1 {
                o_addr & !1
            } else {
                o_addr
            };
            let size = len.min(2);
            let words = len.div_ceil(2);
            for i in 0..words {
                let w = if TORDER == TORDER_HIGHLOW {
                    words - 1 - i
                } else {
                    i
                };
                let mut ticks = 0;
                let acc = CpuAccess {
                    kind,
                    fault,
                    laddr: addr.wrapping_add(2 * w) & ADDRESS_MASK,
                    size,
                    wdata: Some(low_bytes(value >> (8 * (len - size - 2 * w)), size)),
                };
                self.cpu_access(&acc, &mut ticks)?;
                self.advance_cycles(ticks)?;

                if CPU_TYPE == M68000
                    && i == 0
                    && (if TORDER == TORDER_HIGHLOW {
                        len == 4
                    } else {
                        len > 1
                    })
                {
                    // Address errors occur AFTER the first Word was accessed
                    self.verify_access_physical::<T>(o_addr, true)?;
                }
            }
        } else {
            self.write_operand(kind, fault, len, value)?;
        }

        Ok(())
    }

    /// Writes the low `len` bytes (1-4, a 3-byte operand included) of
    /// `value` with as many bus cycles as the port sizes need (68020+).
    #[inline(always)]
    fn write_operand(
        &mut self,
        kind: AccessKind,
        fault: FaultInfo,
        len: u32,
        value: Long,
    ) -> Result<()> {
        let mut ticks = 0;
        let mut done = 0;
        while done < len {
            // A long word at a time, as for reads
            let acc = CpuAccess {
                kind,
                fault,
                laddr: fault.o_addr.wrapping_add(done) & ADDRESS_MASK,
                size: len - done,
                wdata: Some(low_bytes(value, len - done)),
            };
            match self.cpu_access(&acc, &mut ticks) {
                Ok((_, n)) => done += n,
                Err(e) if kind != AccessKind::Physical && self.wb040_defer => {
                    self.wb040_defer_write(e, fault.o_addr, value, done > 0)?;
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        self.advance_cycles(ticks)?;
        self.write_credit = ticks;
        Ok(())
    }

    /// Writes the low `len` bytes (1-4) of `value` as a data operand and
    /// spends ticks (68020+). Virtual address version, data FC
    pub(in crate::cpu_m68k) fn write_ticks_len(
        &mut self,
        vaddr: Address,
        len: u32,
        value: Long,
    ) -> Result<()> {
        let fault = FaultInfo {
            fc: self.fc_data(),
            o_addr: vaddr,
            size: len as usize,
            write: true,
        };
        self.write_operand(AccessKind::Data, fault, len, value)
    }

    /// Writes a value to the bus (big endian) and spends ticks.
    /// Physical address version
    #[allow(dead_code)]
    pub(in crate::cpu_m68k) fn write_ticks_physical<T: CpuSized>(
        &mut self,
        paddr: Address,
        value: T,
    ) -> Result<()> {
        self.write_ticks_order_physical::<T, TORDER_LOWHIGH>(paddr, value)
    }

    /// Writes a value to the bus (big endian) and spends ticks.
    /// Virtual address version, data FC
    pub(in crate::cpu_m68k) fn write_ticks<T: CpuSized>(
        &mut self,
        vaddr: Address,
        value: T,
    ) -> Result<()> {
        self.write_ticks_order_generic::<T, TORDER_LOWHIGH>(
            AccessKind::Data,
            self.fc_data(),
            vaddr,
            value,
        )
    }

    /// Writes a value to the bus (big endian) and spends ticks.
    /// Virtual address version
    pub(in crate::cpu_m68k) fn write_ticks_order<T: CpuSized, const TORDER: usize>(
        &mut self,
        vaddr: Address,
        value: T,
    ) -> Result<()> {
        self.write_ticks_order_generic::<T, TORDER>(AccessKind::Data, self.fc_data(), vaddr, value)
    }

    /// Writes a value to the bus (big endian) and spends ticks, but writes
    /// the word in opposite order if the type is Long.
    /// Virtual address
    pub(in crate::cpu_m68k) fn write_ticks_wflip<T: CpuSized>(
        &mut self,
        addr: Address,
        value: T,
    ) -> Result<()> {
        match std::mem::size_of::<T>() {
            4 => {
                let v: Long = value.expand();
                self.write_ticks_order_generic::<Word, TORDER_LOWHIGH>(
                    AccessKind::Data,
                    self.fc_data(),
                    addr.wrapping_add(2),
                    v as Word,
                )?;
                self.write_ticks_order_generic::<Word, TORDER_LOWHIGH>(
                    AccessKind::Data,
                    self.fc_data(),
                    addr,
                    (v >> 16) as Word,
                )
            }
            _ => self.write_ticks_order_generic::<T, TORDER_LOWHIGH>(
                AccessKind::Data,
                self.fc_data(),
                addr,
                value,
            ),
        }
    }

    /// M68040: a data write of the instruction faulted. Unless the
    /// instruction is restarted on faults, the instruction completes and the
    /// write waits in a write-back slot for the access error handler
    /// (M68040UM 8.4.6). One write can wait that way, a second fault restarts
    /// the instruction.
    fn wb040_defer_write(
        &mut self,
        e: anyhow::Error,
        o_addr: Address,
        value: Long,
        second_page: bool,
    ) -> Result<()> {
        if self.wb040.is_some() {
            return Err(e);
        }
        let Some(CpuError::BusError(details)) = e.downcast_ref() else {
            return Err(e);
        };
        self.wb040 = Some(WriteBack040 {
            details: Group0Details {
                // The fault address is the start of the operand
                address: o_addr,
                ..*details
            },
            data: value,
            ma: second_page && details.atc_fault,
            trace: false,
        });
        Ok(())
    }

    pub(in crate::cpu_m68k) fn write_ticks_order_physical<T: CpuSized, const TORDER: usize>(
        &mut self,
        o_paddr: Address,
        value: T,
    ) -> Result<()> {
        self.write_ticks_order_generic::<T, TORDER>(AccessKind::Physical, FC_UNUSED, o_paddr, value)
    }
}
