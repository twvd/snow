//! M68k CPU - Bus access functionality

use crate::bus::access::{BusAccess, BusResponse, CpuBus};
use crate::bus::{Address, Bus, BusResult, IrqSource};
use crate::cpu_m68k::FpuM68kType;
use crate::cpu_m68k::cpu::{Breakpoint, BusBreakpoint, CpuError, CpuM68k, Group0Details};
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

/// CPU cycles of a bus cycle without wait states: 4 on the 68000, 3 for
/// the asynchronous bus cycle of the 68020 and 68030
const fn bus_cycle_ticks(cpu: CpuM68kType) -> Ticks {
    if cpu >= M68020 { 3 } else { 4 }
}

/// Low `bytes` bytes of `val`
const fn low_bytes(val: Long, bytes: u32) -> Long {
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

    /// Triggers bus access breakpoints
    fn bus_breakpoint(&mut self, write: bool, addr: Address, val: u8) {
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

    /// Runs one cycle on a sized bus for the `size` bytes left of an operand
    /// at `o_addr`, from logical address `laddr`. Adds the duration of the
    /// cycle to `ticks` and returns the read data and the number of bytes
    /// transferred.
    #[inline(always)]
    fn bus_cycle<T: CpuSized, const PHYSICAL: bool>(
        &mut self,
        fc: u8,
        o_addr: Address,
        laddr: Address,
        size: u32,
        wdata: Option<Long>,
        ticks: &mut Ticks,
    ) -> Result<(Long, u32)> {
        let write = wdata.is_some();
        let paddr = if PHYSICAL || !PMMU {
            laddr
        } else {
            self.pmmu_translate(fc, laddr, write)
                .map_err(|e| match e.downcast_ref() {
                    Some(CpuError::BusError(details)) => {
                        anyhow!(CpuError::BusError(Group0Details {
                            size: std::mem::size_of::<T>(),
                            address: laddr,
                            ..*details
                        }))
                    }
                    _ => e,
                })?
        };

        // The bus has to be built for the data bus of this CPU
        const {
            assert!(<TBus::Cpu as CpuBus>::WIDTH == if CPU_TYPE >= M68020 { 4 } else { 2 });
        }

        // CPU cycles of this bus cycle already spent stalled
        let mut stalled = 0;
        let (width, data, waits) = loop {
            let pending = self.cycles + *ticks - self.last_bus_sync;
            let acc = match wdata {
                Some(data) => BusAccess::write(paddr, size, data, pending),
                None => BusAccess::read(paddr, size, pending),
            };
            match self.bus.access(acc)? {
                BusResponse::Ack { width, data, waits } => break (width, data, waits),
                // Wait with the bus ticked through the stall, then retry
                BusResponse::Stall(stall) => {
                    self.history_current.waitstates = true;
                    self.advance_cycles(std::mem::take(ticks) + stall)?;
                    self.sync_bus()?;
                    stalled += stall;
                }
                BusResponse::BusError => bail!(CpuError::BusError(Group0Details {
                    function_code: fc,
                    ir: 0,
                    size: std::mem::size_of::<T>(),
                    instruction: false,
                    read: !write,
                    address: o_addr,
                    // Filled in later
                    start_pc: 0,
                })),
            }
        };
        if waits > 0 {
            self.history_current.waitstates = true;
        }
        *ticks += (bus_cycle_ticks(CPU_TYPE) + waits).saturating_sub(stalled);

        let len = if CPU_TYPE >= M68020 {
            (width - paddr % width).min(size)
        } else {
            size
        };
        if !self.breakpoints.is_empty() {
            for i in 0..len {
                let val = match wdata {
                    Some(d) => d >> (8 * (size - 1 - i)),
                    None => data >> (8 * (len - 1 - i)),
                };
                self.bus_breakpoint(write, paddr.wrapping_add(i), val as u8);
            }
        }
        Ok((data, len))
    }

    /// Reads a value from a sized bus and spends ticks.
    #[inline(always)]
    fn read_sized<T: CpuSized, const PHYSICAL: bool>(
        &mut self,
        fc: u8,
        o_addr: Address,
    ) -> Result<T> {
        let len = std::mem::size_of::<T>() as u32;
        let mut result: u64 = 0;

        if CPU_TYPE < M68020 {
            let addr = if CPU_TYPE == M68000 && len > 1 {
                o_addr & !1
            } else {
                o_addr
            };
            for w in 0..len.div_ceil(2) {
                let mut ticks = 0;
                let laddr = addr.wrapping_add(2 * w) & ADDRESS_MASK;
                let (data, n) =
                    self.bus_cycle::<T, PHYSICAL>(fc, o_addr, laddr, len.min(2), None, &mut ticks)?;
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
                let laddr = o_addr.wrapping_add(done) & ADDRESS_MASK;
                let (data, n) =
                    self.bus_cycle::<T, PHYSICAL>(fc, o_addr, laddr, len - done, None, &mut ticks)?;
                result = (result << (8 * n)) | u64::from(data);
                done += n;
            }
            self.advance_cycles(ticks)?;
        }

        Ok(T::chop(result as Long))
    }

    /// Writes a value to a sized bus and spends ticks.
    #[inline(always)]
    fn write_sized<T: CpuSized, const TORDER: usize, const PHYSICAL: bool>(
        &mut self,
        fc: u8,
        o_addr: Address,
        value: T,
    ) -> Result<()> {
        let len = std::mem::size_of::<T>() as u32;
        let value = value.expand();

        if CPU_TYPE < M68020 {
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
                self.bus_cycle::<T, PHYSICAL>(fc, o_addr, laddr, size, Some(data), &mut ticks)?;
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
                let laddr = o_addr.wrapping_add(done) & ADDRESS_MASK;
                let data = low_bytes(value, len - done);
                let (_, n) = self.bus_cycle::<T, PHYSICAL>(
                    fc,
                    o_addr,
                    laddr,
                    len - done,
                    Some(data),
                    &mut ticks,
                )?;
                done += n;
            }
            self.advance_cycles(ticks)?;
        }

        Ok(())
    }

    #[inline(always)]
    pub(in crate::cpu_m68k) fn read_ticks_generic<T: CpuSized, const PHYSICAL: bool>(
        &mut self,
        fc: u8,
        o_addr: Address,
    ) -> Result<T> {
        if TBus::SIZED {
            return self.read_sized::<T, PHYSICAL>(fc, o_addr);
        }

        let len = std::mem::size_of::<T>();
        let mut result: T = T::zero();
        let addr = if CPU_TYPE == M68000 && len > 1 {
            o_addr & !1
        } else {
            o_addr
        };

        // Below converts from BE -> LE on the fly
        for a in 0..len {
            let logical_addr = addr.wrapping_add(a as Address) & ADDRESS_MASK;
            let byte_addr = if PHYSICAL || !PMMU {
                logical_addr
            } else {
                self.pmmu_translate(fc, logical_addr, false).map_err(|e| {
                    match e.downcast_ref() {
                        Some(CpuError::BusError(details)) => {
                            anyhow!(CpuError::BusError(Group0Details {
                                size: std::mem::size_of::<T>(),
                                address: logical_addr,
                                ..*details
                            }))
                        }
                        _ => e,
                    }
                })?
            };
            let b: T = loop {
                match self.bus.read(byte_addr) {
                    BusResult::Ok(b) => {
                        // Trigger bus access breakpoints
                        if self.breakpoints.iter().any(|bp| {
                            *bp == Breakpoint::Bus(BusBreakpoint::Read, byte_addr)
                                || *bp == Breakpoint::Bus(BusBreakpoint::ReadWrite, byte_addr)
                        }) {
                            log::info!(
                                "Breakpoint hit (bus read): ${:08X}, value: ${:02X}, PC: ${:08X}",
                                byte_addr,
                                b,
                                self.regs.pc,
                            );
                            self.breakpoint_hit.set();
                        }
                        break b.into();
                    }
                    BusResult::WaitState => {
                        // Insert wait states until bus access succeeds
                        self.history_current.waitstates = true;
                        self.advance_cycles(2)?;
                        self.sync_bus()?;
                    }
                    BusResult::BusError => {
                        bail!(CpuError::BusError(Group0Details {
                            function_code: fc,
                            ir: 0,
                            size: std::mem::size_of::<T>(),
                            instruction: false,
                            read: true,
                            address: o_addr,
                            // Filled in later
                            start_pc: 0,
                        }));
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
        if TBus::SIZED {
            return self.write_sized::<T, TORDER, PHYSICAL>(fc, o_addr, value);
        }

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
                    let logical_addr = addr.wrapping_add(a as Address) & ADDRESS_MASK;
                    let byte_addr = if PHYSICAL || !PMMU {
                        logical_addr
                    } else {
                        self.pmmu_translate(fc, logical_addr, true).map_err(|e| {
                            match e.downcast_ref() {
                                Some(CpuError::BusError(details)) => {
                                    anyhow!(CpuError::BusError(Group0Details {
                                        size: std::mem::size_of::<T>(),
                                        address: logical_addr,
                                        ..*details
                                    }))
                                }
                                _ => e,
                            }
                        })?
                    };
                    let b = val as u8;
                    val >>= 8;

                    loop {
                        match self.bus.write(byte_addr, b) {
                            BusResult::Ok(_) => break,
                            BusResult::WaitState => {
                                // Insert wait states until bus access succeeds
                                self.history_current.waitstates = true;
                                self.advance_cycles(2)?;
                                self.sync_bus()?;
                            }
                            BusResult::BusError => {
                                bail!(CpuError::BusError(Group0Details {
                                    function_code: fc,
                                    ir: 0,
                                    size: std::mem::size_of::<T>(),
                                    instruction: false,
                                    read: false,
                                    address: o_addr,
                                    // Filled in later
                                    start_pc: 0,
                                }));
                            }
                        }
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
                    let logical_addr = addr.wrapping_add(a as Address) & ADDRESS_MASK;
                    let byte_addr = if PHYSICAL || !PMMU {
                        logical_addr
                    } else {
                        self.pmmu_translate(fc, logical_addr, true).map_err(|e| {
                            match e.downcast_ref() {
                                Some(CpuError::BusError(details)) => {
                                    anyhow!(CpuError::BusError(Group0Details {
                                        size: std::mem::size_of::<T>(),
                                        address: logical_addr,
                                        ..*details
                                    }))
                                }
                                _ => e,
                            }
                        })?
                    };
                    let b = val as u8;
                    val >>= 8;

                    loop {
                        match self.bus.write(byte_addr, b) {
                            BusResult::Ok(_) => break,
                            BusResult::WaitState => {
                                // Insert wait states until bus access succeeds
                                self.history_current.waitstates = true;
                                self.advance_cycles(2)?;
                                self.sync_bus()?;
                            }
                            BusResult::BusError => {
                                bail!(CpuError::BusError(Group0Details {
                                    function_code: fc,
                                    ir: 0,
                                    size: std::mem::size_of::<T>(),
                                    instruction: false,
                                    read: false,
                                    address: o_addr,
                                    // Filled in later
                                    start_pc: 0,
                                }));
                            }
                        }
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
