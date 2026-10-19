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
        self.read_ticks_generic::<T, false>(self.fc_data(), vaddr)
    }

    /// Reads a value from the bus and spends ticks.
    /// Virtual address version, program FC
    pub(in crate::cpu_m68k) fn read_ticks_program<T: CpuSized>(
        &mut self,
        vaddr: Address,
    ) -> Result<T> {
        self.read_ticks_generic::<T, false>(self.fc_program(), vaddr)
    }

    /// Reads a value from the bus and spends ticks.
    /// Physical address version
    pub(in crate::cpu_m68k) fn read_ticks_physical<T: CpuSized>(
        &mut self,
        o_paddr: Address,
    ) -> Result<T> {
        // FC unused for physical addressing
        self.read_ticks_generic::<T, true>(FC_UNUSED, o_paddr)
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

    /// Translates logical address `laddr` of an operand access
    #[inline(always)]
    fn translate<const PHYSICAL: bool>(
        &mut self,
        fault: &FaultInfo,
        laddr: Address,
    ) -> Result<Address> {
        if PHYSICAL || !PMMU {
            return Ok(laddr);
        }
        self.pmmu_translate(fault.fc, laddr, fault.write)
            .map_err(|e| match e.downcast_ref() {
                Some(CpuError::BusError(details)) => {
                    anyhow!(CpuError::BusError(Group0Details {
                        size: fault.size,
                        address: laddr,
                        ..*details
                    }))
                }
                _ => e,
            })
    }

    /// Runs a CPU access for the `size` bytes left of an operand, from
    /// logical address `laddr`, up to the end of the long word at most:
    /// translates it and runs its bus cycles. Adds their duration to `ticks`
    /// and returns the read data and the number of bytes transferred.
    #[inline(always)]
    fn cpu_access<const PHYSICAL: bool>(
        &mut self,
        fault: &FaultInfo,
        laddr: Address,
        size: u32,
        wdata: Option<Long>,
        ticks: &mut Ticks,
    ) -> Result<(Long, u32)> {
        let paddr = self.translate::<PHYSICAL>(fault, laddr)?;
        let (data, len) = bus_transaction!(self, ticks).access(fault, paddr, size, wdata)?;

        // Data breakpoints, on the physical address of the access
        if !self.breakpoints.is_empty() {
            for i in 0..len {
                let val = match wdata {
                    Some(d) => d >> (8 * (size - 1 - i)),
                    None => data >> (8 * (len - 1 - i)),
                };
                self.data_breakpoint(fault.write, paddr.wrapping_add(i), val as u8);
            }
        }
        Ok((data, len))
    }

    /// Reads a value from the bus and spends ticks.
    #[inline(always)]
    pub(in crate::cpu_m68k) fn read_ticks_generic<T: CpuSized, const PHYSICAL: bool>(
        &mut self,
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
                let laddr = addr.wrapping_add(2 * w) & ADDRESS_MASK;
                let (data, n) =
                    self.cpu_access::<PHYSICAL>(&fault, laddr, len.min(2), None, &mut ticks)?;
                result = (result << (8 * n)) | u64::from(data);
                self.advance_cycles(ticks)?;

                if CPU_TYPE == M68000 && w == 0 && len > 1 {
                    // Address errors occur AFTER the first Word was accessed
                    self.verify_access_physical::<T>(o_addr, true)?;
                }
            }
        } else {
            let mut ticks = 0;
            let mut done = 0;
            while done < len {
                // An access goes at most to the end of a long word: the rest
                // of the operand may be on another page, it is translated on
                // its own. We pass cpu_access() the full size left in the
                // operand, not just up to the next long word boundary, as the
                // 68020 and 68030 show it on SIZ.
                let laddr = o_addr.wrapping_add(done) & ADDRESS_MASK;
                let (data, n) =
                    self.cpu_access::<PHYSICAL>(&fault, laddr, len - done, None, &mut ticks)?;
                result = (result << (8 * n)) | u64::from(data);
                done += n;
            }
            self.advance_cycles(ticks)?;
        }

        Ok(T::chop(result as Long))
    }

    /// Writes a value to the bus and spends ticks.
    #[inline(always)]
    pub(in crate::cpu_m68k) fn write_ticks_order_generic<
        T: CpuSized,
        const TORDER: usize,
        const PHYSICAL: bool,
    >(
        &mut self,
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
                let laddr = addr.wrapping_add(2 * w) & ADDRESS_MASK;
                let data = low_bytes(value >> (8 * (len - size - 2 * w)), size);
                self.cpu_access::<PHYSICAL>(&fault, laddr, size, Some(data), &mut ticks)?;
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
            let mut ticks = 0;
            let mut done = 0;
            while done < len {
                // A long word at a time, as for reads
                let laddr = o_addr.wrapping_add(done) & ADDRESS_MASK;
                let data = low_bytes(value, len - done);
                match self.cpu_access::<PHYSICAL>(&fault, laddr, len - done, Some(data), &mut ticks)
                {
                    Ok((_, n)) => done += n,
                    Err(e) if !PHYSICAL && self.wb040_defer => {
                        self.wb040_defer_write(e, o_addr, value, done > 0)?;
                        break;
                    }
                    Err(e) => return Err(e),
                }
            }
            self.advance_cycles(ticks)?;
        }

        Ok(())
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
        self.write_ticks_order_generic::<T, TORDER_LOWHIGH, false>(self.fc_data(), vaddr, value)
    }

    /// Writes a value to the bus (big endian) and spends ticks.
    /// Virtual address version
    pub(in crate::cpu_m68k) fn write_ticks_order<T: CpuSized, const TORDER: usize>(
        &mut self,
        vaddr: Address,
        value: T,
    ) -> Result<()> {
        self.write_ticks_order_generic::<T, TORDER, false>(self.fc_data(), vaddr, value)
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
                self.write_ticks_order_generic::<Word, TORDER_LOWHIGH, false>(
                    self.fc_data(),
                    addr.wrapping_add(2),
                    v as Word,
                )?;
                self.write_ticks_order_generic::<Word, TORDER_LOWHIGH, false>(
                    self.fc_data(),
                    addr,
                    (v >> 16) as Word,
                )
            }
            _ => self.write_ticks_order_generic::<T, TORDER_LOWHIGH, false>(
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
        self.write_ticks_order_generic::<T, TORDER, true>(FC_UNUSED, o_paddr, value)
    }
}
