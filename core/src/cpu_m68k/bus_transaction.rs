//! M68k CPU - Bus transactions: the bus cycles of a CPU access

use crate::bus::access::{BusAccess, BusResponse, CpuBus};
use crate::bus::{Address, Bus, IrqSource};
use crate::cpu_m68k::cpu::{CpuError, Group0Details, HistoryEntryInstruction};
use crate::cpu_m68k::{CpuM68kType, M68020};
use crate::tickable::Ticks;
use crate::types::Long;

use anyhow::{Result, bail};

/// The operand a bus cycle is for, as a bus error reports it
#[derive(Clone, Copy)]
pub struct FaultInfo {
    pub fc: u8,
    /// Address of the operand
    pub o_addr: Address,
    /// Size of the operand in bytes
    pub size: usize,
    pub write: bool,
}

/// The bus cycles of one CPU access: runs them as `BusAccess`es on the bus
/// and keeps track of their duration. It borrows the bus and the CPU state
/// that bus cycles use, for the time of the access. Build it with
/// `bus_transaction!()`, which borrows the fields one by one so the rest of
/// the CPU stays available.
pub struct BusTransaction<'a, TBus, const CPU_TYPE: CpuM68kType> {
    pub bus: &'a mut TBus,
    pub cycles: &'a mut Ticks,
    pub last_bus_sync: &'a mut Ticks,
    /// History entry of the instruction the access is for
    pub history: &'a mut HistoryEntryInstruction,
    /// Duration of the operand access so far, not yet spent
    pub ticks: &'a mut Ticks,
}

/// Borrows a `BusTransaction` from a `CpuM68k`, adding the duration of its bus
/// cycles to `$ticks`
macro_rules! bus_transaction {
    ($cpu:expr, $ticks:expr) => {
        $crate::cpu_m68k::bus_transaction::BusTransaction::<_, CPU_TYPE> {
            bus: &mut $cpu.bus,
            cycles: &mut $cpu.cycles,
            last_bus_sync: &mut $cpu.last_bus_sync,
            history: &mut $cpu.history_current,
            ticks: $ticks,
        }
    };
}
pub(in crate::cpu_m68k) use bus_transaction;

impl<TBus, const CPU_TYPE: CpuM68kType> BusTransaction<'_, TBus, CPU_TYPE>
where
    TBus: Bus<Address, u8> + IrqSource,
{
    /// Spends CPU cycles
    fn advance(&mut self, ticks: Ticks) -> Result<()> {
        *self.cycles += ticks;
        self.bus.cpu_tick(ticks)
    }

    /// Ticks the bus up to the current CPU cycle
    fn sync(&mut self) -> Result<()> {
        let ticks = *self.cycles - *self.last_bus_sync;
        if ticks > 0 {
            *self.last_bus_sync += ticks;
            self.bus.tick(ticks, ())?;
        }
        Ok(())
    }

    /// Runs the bus cycle of an access for the `size` bytes left of the
    /// operand of `fault`, at physical address `paddr`. Adds its duration to
    /// the access and returns the read data and the number of bytes
    /// transferred.
    #[inline(always)]
    pub fn access(
        &mut self,
        fault: &FaultInfo,
        paddr: Address,
        size: u32,
        wdata: Option<Long>,
    ) -> Result<(Long, u32)> {
        let (data, width) = self.run(fault, paddr, size, wdata)?;
        let len = if CPU_TYPE >= M68020 {
            (width - paddr % width).min(size)
        } else {
            size
        };
        Ok((data, len))
    }

    /// Runs one bus cycle for the `size` bytes left of the operand of
    /// `fault`, at physical address `paddr`, and adds its duration to the
    /// access. Returns the read data and the width of the port.
    #[inline(always)]
    fn run(
        &mut self,
        fault: &FaultInfo,
        paddr: Address,
        size: u32,
        wdata: Option<Long>,
    ) -> Result<(Long, u32)> {
        // The bus has to be built for the data bus of this CPU
        const {
            assert!(<TBus::Cpu as CpuBus>::WIDTH == if CPU_TYPE >= M68020 { 4 } else { 2 });
        }

        let (width, data, waits) = loop {
            let pending = *self.cycles + *self.ticks - *self.last_bus_sync;
            let acc = match wdata {
                Some(data) => BusAccess::write(paddr, size, data, pending),
                None => BusAccess::read(paddr, size, pending),
            };
            match self.bus.access(acc)? {
                BusResponse::Ack { width, data, waits } => break (width, data, waits),
                // Wait with the bus ticked through the stall, then retry
                BusResponse::Stall(stall) => {
                    self.history.waitstates = true;
                    let ticks = std::mem::take(self.ticks) + stall;
                    self.advance(ticks)?;
                    self.sync()?;
                }
                BusResponse::BusError => bail!(CpuError::BusError(Group0Details {
                    function_code: fault.fc,
                    ir: 0,
                    size: fault.size,
                    atc_fault: false,
                    instruction: false,
                    read: !fault.write,
                    address: fault.o_addr,
                    // Filled in later
                    start_pc: 0,
                })),
            }
        };
        if waits > 0 {
            self.history.waitstates = true;
        }
        *self.ticks += <TBus::Cpu as CpuBus>::CYCLE + waits;
        Ok((data, width))
    }
}
