//! M68k CPU - Bus access functionality

use crate::bus::{Address, Bus, BusResult, IrqSource};
use crate::cpu_m68k::cpu::{AddressError, Breakpoint, BusBreakpoint, CpuError, CpuM68k};
use crate::cpu_m68k::{CpuM68kType, CpuSized, M68000, M68020, TORDER_HIGHLOW, TORDER_LOWHIGH};
use crate::types::Long;
use crate::types::Word;

use anyhow::{bail, Result};

impl<TBus, const ADDRESS_MASK: Address, const CPU_TYPE: CpuM68kType, const PMMU: bool>
    CpuM68k<TBus, ADDRESS_MASK, CPU_TYPE, PMMU>
where
    TBus: Bus<Address, u8> + IrqSource,
{
    /// Checks if an access needs to fail and raise bus error on alignment errors
    fn verify_access<T: CpuSized>(&self, addr: Address, read: bool) -> Result<()> {
        if std::mem::size_of::<T>() >= 2 && (addr & 1) != 0 {
            // Unaligned access
            if CPU_TYPE < M68020 {
                log::warn!("Unaligned access: address {:08X}", addr);

                // TODO should still happen on 68020+ for PC
                bail!(CpuError::AddressError(AddressError {
                    function_code: 0,
                    ir: 0,

                    // TODO instruction bit
                    instruction: false,
                    read,
                    address: addr
                }));
            }
        }
        Ok(())
    }

    /// Reads a value from the bus and spends ticks.
    pub(in crate::cpu_m68k) fn read_ticks<T: CpuSized>(&mut self, oaddr: Address) -> Result<T> {
        let len = std::mem::size_of::<T>();
        let mut result: T = T::zero();
        let addr = if CPU_TYPE == M68000 && len > 1 {
            oaddr & !1
        } else {
            oaddr
        };

        // Below converts from BE -> LE on the fly
        for a in 0..len {
            let byte_addr = addr.wrapping_add(a as Address) & ADDRESS_MASK;
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

            self.advance_cycles(2)?;

            if a == 1 {
                // Address errors occur AFTER the first Word was accessed and not at all if
                // it is a byte access, so this is the perfect time to check.
                self.verify_access::<T>(oaddr, true)?;
            }
        }

        if len == 1 {
            // Minimum of 4 cycles
            self.advance_cycles(2)?;
        }

        Ok(result)
    }

    /// Writes a value to the bus (big endian) and spends ticks.
    pub(in crate::cpu_m68k) fn write_ticks<T: CpuSized>(
        &mut self,
        addr: Address,
        value: T,
    ) -> Result<()> {
        self.write_ticks_order::<T, TORDER_LOWHIGH>(addr, value)
    }

    /// Writes a value to the bus (big endian) and spends ticks, but writes
    /// the word in opposite order if the type is Long.
    pub(in crate::cpu_m68k) fn write_ticks_wflip<T: CpuSized>(
        &mut self,
        addr: Address,
        value: T,
    ) -> Result<()> {
        match std::mem::size_of::<T>() {
            4 => {
                let v: Long = value.expand();
                self.write_ticks_order::<Word, TORDER_LOWHIGH>(addr.wrapping_add(2), v as Word)?;
                self.write_ticks_order::<Word, TORDER_LOWHIGH>(addr, (v >> 16) as Word)
            }
            _ => self.write_ticks_order::<T, TORDER_LOWHIGH>(addr, value),
        }
    }

    pub(in crate::cpu_m68k) fn write_ticks_order<T: CpuSized, const TORDER: usize>(
        &mut self,
        oaddr: Address,
        value: T,
    ) -> Result<()> {
        let addr = if CPU_TYPE == 68000 && std::mem::size_of::<T>() > 1 {
            oaddr & !1
        } else {
            oaddr
        };

        match TORDER {
            TORDER_LOWHIGH => {
                let mut val: Long = value.to_be().into();
                for a in 0..std::mem::size_of::<T>() {
                    let byte_addr = addr.wrapping_add(a as Address) & ADDRESS_MASK;
                    let b = val as u8;
                    val >>= 8;

                    while self.bus.write(byte_addr, b) == BusResult::WaitState {
                        // Insert wait states until bus access succeeds
                        self.history_current.waitstates = true;
                        self.advance_cycles(2)?;
                    }
                    self.advance_cycles(2)?;

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

                    if a == 1 {
                        // Address errors occur AFTER the first Word was accessed and not at all if
                        // it is a byte access, so this is the perfect time to check.
                        self.verify_access::<T>(oaddr, true)?;
                    }
                }
            }
            TORDER_HIGHLOW => {
                let mut val: Long = value.into();
                for a in (0..std::mem::size_of::<T>()).rev() {
                    let byte_addr = addr.wrapping_add(a as Address) & ADDRESS_MASK;
                    let b = val as u8;
                    val >>= 8;

                    while self.bus.write(byte_addr, b) == BusResult::WaitState {
                        // Insert wait states until bus access succeeds
                        self.history_current.waitstates = true;
                        self.advance_cycles(2)?;
                    }
                    self.advance_cycles(2)?;

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

                    if a == 2 {
                        // Address errors occur AFTER the first Word was accessed and not at all if
                        // it is a byte access, so this is the perfect time to check.
                        self.verify_access::<T>(oaddr, true)?;
                    }
                }
            }
            _ => unreachable!(),
        }

        if std::mem::size_of::<T>() == 1 {
            // Minimum of 4 cycles
            self.advance_cycles(2)?;
        }

        Ok(())
    }
}
