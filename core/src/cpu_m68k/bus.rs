//! M68k CPU - Bus access functionality

use crate::bus::{Address, Bus, BusResult, IrqSource};
use crate::cpu_m68k::cpu::{Breakpoint, BusBreakpoint, CpuError, CpuM68k, Group0Details};
use crate::cpu_m68k::FpuM68kType;
use crate::cpu_m68k::{CpuM68kType, CpuSized, M68000, M68020, TORDER_HIGHLOW, TORDER_LOWHIGH};
use crate::types::Long;
use crate::types::Word;

use anyhow::{anyhow, bail, Result};

// M68k UM 3.8
pub const FC_UNUSED: u8 = 0;
pub const FC_USER_DATA: u8 = 1;
pub const FC_USER_PROGRAM: u8 = 2;
pub const FC_SUPERVISOR_DATA: u8 = 5;
pub const FC_SUPERVISOR_PROGRAM: u8 = 6;
pub const FC_MASK: u8 = 0b1111;

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
    fn fc_program(&self) -> u8 {
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

    #[inline(always)]
    pub(in crate::cpu_m68k) fn read_ticks_generic<T: CpuSized, const PHYSICAL: bool>(
        &mut self,
        fc: u8,
        o_addr: Address,
    ) -> Result<T> {
        let len = std::mem::size_of::<T>();
        let mut result: T = T::zero();
        let addr = if CPU_TYPE == M68000 && len > 1 {
            o_addr & !1
        } else {
            o_addr
        };

        // Below converts from BE -> LE on the fly
        for a in 0..len {
            let byte_addr = if PHYSICAL || !PMMU {
                addr.wrapping_add(a as Address) & ADDRESS_MASK
            } else {
                self.pmmu_translate(fc, addr.wrapping_add(a as Address), false)
                    .map_err(|e| match e.downcast_ref() {
                        Some(CpuError::BusError(details)) => {
                            anyhow!(CpuError::BusError(Group0Details {
                                // Fill in the size of the faulting access
                                size: std::mem::size_of::<T>(),
                                // Replace the BYTE address with the original
                                address: addr,
                                ..*details
                            }))
                        }
                        _ => e,
                    })?
            };
            let b: T =
                loop {
                    match self.bus.read(byte_addr) {
                        BusResult::Ok(b) => {
                            // Trigger bus access breakpoints
                            if self.breakpoints.iter().any(|bp| {
                                *bp == Breakpoint::Bus(BusBreakpoint::Read, byte_addr)
                                    || *bp == Breakpoint::Bus(BusBreakpoint::ReadWrite, byte_addr)
                            }) {
                                log::info!(
                                "Breakpoint hit (bus read): ${:08X}, value: ${:02X}, PC: ${:08X}",
                                byte_addr, b, self.regs.pc,
                            );
                                self.breakpoint_hit.set();
                            }
                            break b.into();
                        }
                        BusResult::WaitState => {
                            // Insert wait states until bus access succeeds
                            self.history_current.waitstates = true;
                            self.advance_cycles(2)?;
                        }
                    }
                };
            result = result.wrapping_shl(8) | b;

            if CPU_TYPE < M68020 {
                self.advance_cycles(2)?;
            }

            if CPU_TYPE == M68000 && a == 1 {
                // Address errors occur AFTER the first Word was accessed and not at all if
                // it is a byte access, so this is the perfect time to check.
                //
                // 68000 only addresses physical pages so no translation needed here.
                self.verify_access_physical::<T>(o_addr, true)?;
            }
        }

        if CPU_TYPE < M68020 && len == 1 {
            // Minimum of 4 cycles
            self.advance_cycles(2)?;
        }

        if CPU_TYPE >= M68020 {
            // 68020+ has a 32-bit wide data bus with dynamic bus sizing.
            // We assume all accesses are of equivalent size of their ports;
            // e.g. the RAM is 32-bit wide ported and receives byte, word and
            // long access, but e.g. the SWIM is 16-bit ported but never
            // gets a 32-bit access (if that does happen, it will be faster).
            self.advance_cycles(4)?;

            // 1 bus cycle penalty for unaligned access
            // We check this on the virtual address, but that is fine, given the
            // PMMUs minimum page size is 256 bytes.
            if (len == 2 && (addr & 0b01) != 0) || (len == 4 && (addr & 0b11) != 0) {
                self.advance_cycles(4)?;
            }
        }

        Ok(result)
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

    pub(in crate::cpu_m68k) fn write_ticks_order_physical<T: CpuSized, const TORDER: usize>(
        &mut self,
        o_paddr: Address,
        value: T,
    ) -> Result<()> {
        self.write_ticks_order_generic::<T, TORDER, true>(FC_UNUSED, o_paddr, value)
    }

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
        let addr = if CPU_TYPE == 68000 && std::mem::size_of::<T>() > 1 {
            o_addr & !1
        } else {
            o_addr
        };
        let len = std::mem::size_of::<T>();

        match TORDER {
            TORDER_LOWHIGH => {
                let mut val: Long = value.to_be().into();
                for a in 0..len {
                    let byte_addr = if PHYSICAL || !PMMU {
                        addr.wrapping_add(a as Address) & ADDRESS_MASK
                    } else {
                        self.pmmu_translate(fc, addr.wrapping_add(a as Address), true)
                            .map_err(|e| match e.downcast_ref() {
                                Some(CpuError::BusError(details)) => {
                                    anyhow!(CpuError::BusError(Group0Details {
                                        // Fill in the size of the faulting access
                                        size: std::mem::size_of::<T>(),
                                        // Replace the BYTE address with the original
                                        address: addr,
                                        ..*details
                                    }))
                                }
                                _ => e,
                            })?
                    };
                    let b = val as u8;
                    val >>= 8;

                    while self.bus.write(byte_addr, b) == BusResult::WaitState {
                        // Insert wait states until bus access succeeds
                        self.history_current.waitstates = true;
                        self.advance_cycles(2)?;
                    }
                    if CPU_TYPE < M68020 {
                        self.advance_cycles(2)?;
                    }

                    // Trigger bus access breakpoints
                    if self.breakpoints.iter().any(|bp| {
                        *bp == Breakpoint::Bus(BusBreakpoint::Write, byte_addr)
                            || *bp == Breakpoint::Bus(BusBreakpoint::ReadWrite, byte_addr)
                    }) {
                        log::info!(
                            "Breakpoint hit (bus write): ${:08X}, value: ${:02X}, PC: ${:08X}",
                            byte_addr,
                            b,
                            self.regs.pc
                        );
                        self.breakpoint_hit.set();
                    }

                    if CPU_TYPE == M68000 && a == 1 {
                        // Address errors occur AFTER the first Word was accessed and not at all if
                        // it is a byte access, so this is the perfect time to check.
                        //
                        // 68000 only addresses physical pages so no translation needed here.
                        self.verify_access_physical::<T>(o_addr, true)?;
                    }
                }
            }
            TORDER_HIGHLOW => {
                let mut val: Long = value.into();
                for a in (0..len).rev() {
                    let byte_addr = if PHYSICAL || !PMMU {
                        addr.wrapping_add(a as Address) & ADDRESS_MASK
                    } else {
                        self.pmmu_translate(fc, addr.wrapping_add(a as Address), true)
                            .map_err(|e| match e.downcast_ref() {
                                Some(CpuError::BusError(details)) => {
                                    anyhow!(CpuError::BusError(Group0Details {
                                        // Fill in the size of the faulting access
                                        size: std::mem::size_of::<T>(),
                                        // Replace the BYTE address with the original
                                        address: addr,
                                        ..*details
                                    }))
                                }
                                _ => e,
                            })?
                    };
                    let b = val as u8;
                    val >>= 8;

                    while self.bus.write(byte_addr, b) == BusResult::WaitState {
                        // Insert wait states until bus access succeeds
                        self.history_current.waitstates = true;
                        self.advance_cycles(2)?;
                    }
                    if CPU_TYPE < M68020 {
                        self.advance_cycles(2)?;
                    }

                    // Trigger bus access breakpoints
                    if self.breakpoints.iter().any(|bp| {
                        *bp == Breakpoint::Bus(BusBreakpoint::Write, byte_addr)
                            || *bp == Breakpoint::Bus(BusBreakpoint::ReadWrite, byte_addr)
                    }) {
                        log::info!(
                            "Breakpoint hit (bus write): ${:08X}, value: ${:02X}, PC: ${:08X}",
                            byte_addr,
                            b,
                            self.regs.pc
                        );
                        self.breakpoint_hit.set();
                    }

                    if CPU_TYPE == M68000 && a == 2 {
                        // Address errors occur AFTER the first Word was accessed and not at all if
                        // it is a byte access, so this is the perfect time to check.
                        //
                        // 68000 only addresses physical pages so no translation needed here.
                        self.verify_access_physical::<T>(o_addr, true)?;
                    }
                }
            }
            _ => unreachable!(),
        }

        if CPU_TYPE < M68020 && len == 1 {
            // Minimum of 4 cycles
            self.advance_cycles(2)?;
        }

        if CPU_TYPE >= M68020 {
            // 68020+ has a 32-bit wide data bus with dynamic bus sizing.
            // We assume all accesses are of equivalent size of their ports;
            // e.g. the RAM is 32-bit wide ported and receives byte, word and
            // long access, but e.g. the SWIM is 16-bit ported but never
            // gets a 32-bit access (if that does happen, it will be faster).
            self.advance_cycles(4)?;

            // 1 bus cycle penalty for unaligned access
            // We check this on the virtual address, but that is fine, given the
            // PMMUs minimum page size is 256 bytes.
            if (len == 2 && (addr & 0b01) != 0) || (len == 4 && (addr & 0b11) != 0) {
                self.advance_cycles(4)?;
            }
        }

        Ok(())
    }
}
