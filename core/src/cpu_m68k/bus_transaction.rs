//! M68k CPU - Bus transactions: the bus cycles of a CPU access

use crate::bus::access::{BusAccess, BusResponse, CpuBus};
use crate::bus::{Address, Bus, IrqSource};
use crate::cpu_m68k::bus::low_bytes;
use crate::cpu_m68k::cpu::{CpuError, Group0Details, HistoryEntryInstruction};
use crate::cpu_m68k::{CpuM68kType, M68020, M68040};
use crate::tickable::Ticks;
use crate::types::Long;

use anyhow::{Result, bail};

/// Place of a bus cycle in a line transfer
#[derive(Clone, Copy, PartialEq, Eq)]
enum Beat {
    /// Not part of a burst
    Single,
    /// First long word of a line, asking for a burst
    First,
    /// Second to fourth long word of a burst the port took
    Next,
}

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

    /// Runs the bus cycles of an access for the `size` bytes left of the
    /// operand of `fault`, at physical address `paddr`, up to the end of the
    /// long word at most: the rest of the operand may be on another page.
    /// `size` is the full size left in the operand, not just up to the next
    /// long word boundary, as the 68020 and 68030 show it on SIZ. Adds the
    /// duration of the cycles to the access and returns the read data and
    /// the number of bytes transferred. A bus error ends the access.
    #[inline(always)]
    pub fn access(
        &mut self,
        fault: &FaultInfo,
        paddr: Address,
        size: u32,
        wdata: Option<Long>,
    ) -> Result<(Long, u32)> {
        if CPU_TYPE >= M68040 {
            self.access_68040(fault, paddr, size, wdata)
        } else if CPU_TYPE >= M68020 {
            self.access_68020(fault, paddr, size, wdata)
        } else {
            self.access_68000(fault, paddr, size, wdata)
        }
    }

    /// `access()` on the 68000: a fixed 16-bit bus, the CPU only asks for
    /// what it takes
    #[inline(always)]
    fn access_68000(
        &mut self,
        fault: &FaultInfo,
        paddr: Address,
        size: u32,
        wdata: Option<Long>,
    ) -> Result<(Long, u32)> {
        let (data, width, _) = self.run(fault, paddr, size, wdata, Beat::Single)?;
        debug_assert_eq!(width, 2, "68000 bus answered a {width}-byte port");
        Ok((data, size))
    }

    /// `access()` on the 68020 and 68030: each cycle shows the bytes left in
    /// the operand on SIZ, the port takes those that fit in its width at
    /// that address, and the CPU runs further cycles for the rest (dynamic
    /// bus sizing, MC68020UM 5.1)
    #[inline(always)]
    fn access_68020(
        &mut self,
        fault: &FaultInfo,
        paddr: Address,
        size: u32,
        wdata: Option<Long>,
    ) -> Result<(Long, u32)> {
        let len = size.min(4 - paddr % 4);
        let mut data: u64 = 0;
        let mut done = 0;
        while done < len {
            let addr = paddr.wrapping_add(done);
            let left = size - done;
            let wdata = wdata.map(|w| low_bytes(w, left));
            let (d, width, _) = self.run(fault, addr, left, wdata, Beat::Single)?;
            let n = (width - addr % width).min(left);
            data = (data << (8 * n)) | u64::from(d);
            done += n;
        }
        Ok((data as Long, len))
    }

    /// `access()` on the 68040: a 32-bit bus without dynamic bus sizing,
    /// narrower devices are adapted by the board (on the Quadras, the I/O
    /// bus adapter). A misaligned access runs as aligned byte, word and long
    /// word transfers, each showing its own size on SIZ (MC68040UM 7.2,
    /// 7.3).
    #[inline(always)]
    fn access_68040(
        &mut self,
        fault: &FaultInfo,
        paddr: Address,
        size: u32,
        wdata: Option<Long>,
    ) -> Result<(Long, u32)> {
        let len = size.min(4 - paddr % 4);
        let mut data: u64 = 0;
        let mut done = 0;
        while done < len {
            let addr = paddr.wrapping_add(done);
            let n = if addr % 4 == 0 && len - done == 4 {
                4
            } else if addr % 2 == 0 && len - done >= 2 {
                2
            } else {
                1
            };
            let left = size - done;
            let wdata = wdata.map(|w| low_bytes(w >> (8 * (left - n)), n));
            let (d, width, _) = self.run(fault, addr, n, wdata, Beat::Single)?;
            debug_assert_eq!(width, 4, "68040 bus answered a {width}-byte port");
            data = (data << (8 * n)) | u64::from(d);
            done += n;
        }
        Ok((data as Long, len))
    }

    /// Transfers the 16-byte line at physical address `paddr` (68040): reads
    /// it, or writes `wline`. It runs as a burst when the port takes one:
    /// two clocks for the first long word and one for each of the others,
    /// plus wait states. Otherwise (TBI) the rest of the line runs as long
    /// word transfers. Returns the read line.
    ///
    /// The line always starts at its first long word, where the 68040
    /// starts with the one it needs first: the time is the same, and
    /// nothing uses the data before the whole line is in.
    pub fn line_access(
        &mut self,
        fault: &FaultInfo,
        paddr: Address,
        wline: Option<&[Long; 4]>,
    ) -> Result<[Long; 4]> {
        let mut line = [0; 4];
        let mut beat = Beat::First;
        for (i, long) in line.iter_mut().enumerate() {
            let addr = paddr.wrapping_add(4 * i as Address);
            // This long word of the line, for a write
            let wdata = wline.map(|l| l[i]);
            let (data, width, burst) = self.run(fault, addr, 4, wdata, beat)?;
            debug_assert_eq!(width, 4, "68040 bus answered a {width}-byte port");
            *long = data;
            beat = if burst && beat != Beat::Single {
                Beat::Next
            } else {
                Beat::Single
            };
        }
        Ok(line)
    }

    /// Runs one bus cycle for the `size` bytes left of the operand of
    /// `fault`, at physical address `paddr`, as `beat` of a line transfer,
    /// and adds its duration to the access. Returns the read data, the
    /// width of the port and whether it takes a burst.
    #[inline(always)]
    fn run(
        &mut self,
        fault: &FaultInfo,
        paddr: Address,
        size: u32,
        wdata: Option<Long>,
        beat: Beat,
    ) -> Result<(Long, u32, bool)> {
        // The bus has to be built for the data bus of this CPU
        const {
            assert!(<TBus::Cpu as CpuBus>::WIDTH == if CPU_TYPE >= M68020 { 4 } else { 2 });
        }

        let (width, data, waits, burst_reject) = loop {
            let pending = *self.cycles + *self.ticks - *self.last_bus_sync;
            let mut acc = match wdata {
                Some(data) => BusAccess::write(paddr, size, data, pending),
                None => BusAccess::read(paddr, size, pending),
            };
            acc.burst = beat != Beat::Single;
            match self.bus.access(acc)? {
                BusResponse::Ack {
                    width,
                    data,
                    waits,
                    burst_reject,
                } => break (width, data, waits, burst_reject),
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
        let ticks = if beat == Beat::Next {
            1
        } else {
            <TBus::Cpu as CpuBus>::CYCLE
        };
        *self.ticks += ticks + waits;
        Ok((data, width, !burst_reject))
    }
}
