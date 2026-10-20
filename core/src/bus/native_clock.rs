//! Native clock of the devices of a bus, which the CPU may run faster than

use serde::{Deserialize, Serialize};

use crate::bus::access::{BusAccess, CpuBus};
use crate::tickable::Ticks;

/// Wait states, in CPU ticks
pub type WaitTicks = Ticks;

/// What a device access cycle does
pub enum DeviceCycle {
    /// Stall the CPU for this many CPU ticks, then it presents the cycle
    /// again
    Stall(Ticks),
    /// Do the access now. device_sync() and via_sync() return in WaitTicks the
    /// wait states, in CPU ticks, to give the CPU so that the access lasts
    /// as long as the corresponding device bus cycle: a native bus cycle
    /// for device_sync(), a VIA (E clock) cycle for via_sync(). The device
    /// can add any extra wait states on top of that with scale_dev_waits().
    Access(WaitTicks),
}

/// What a step of the native clock did
pub struct Step {
    /// Native ticks that elapsed
    pub native: Ticks,
    /// The VIA clock ticked at the end of them
    pub via_tick: bool,
    /// CPU ticks they consumed
    pub cpu_used: Ticks,
}

/// Native clock of a machine's device bus, which the CPU may run faster
/// than when overclocking.
///
/// The CPU runs on CPU ticks, the devices on native ticks. A native tick is
/// never shorter than a CPU tick. The clock also produces the VIA clock,
/// known as the E clock on the 68000 Macs, which ticks every `native_per_via`
/// native ticks.
///
/// A native tick lasts `cpu_per_native_num / cpu_per_native_den` CPU ticks,
/// which is the CPU frequency divided by the native frequency, kept as a
/// fraction so that the native clock keeps its exact frequency.
#[derive(Serialize, Deserialize)]
pub struct NativeClock {
    /// Native frequency, in Hz
    native_freq: u64,
    /// Numerator of the CPU ticks per native tick
    cpu_per_native_num: Ticks,
    /// Denominator of the CPU ticks per native tick
    cpu_per_native_den: Ticks,
    /// Native ticks per VIA tick
    native_per_via: Ticks,
    /// CPU ticks since the current native tick started, times
    /// `cpu_per_native_den` so that it is a whole number
    cpu_since_native_times_den: Ticks,
    /// Native ticks since the last VIA tick
    native_since_via: Ticks,
    /// Native ticks elapsed since boot
    native_ticks: Ticks,
}

impl NativeClock {
    /// Native clock at `native_freq` Hz, with a VIA tick every
    /// `native_per_via` native ticks
    pub fn new(native_freq: u64, native_per_via: Ticks) -> Self {
        Self {
            native_freq,
            cpu_per_native_num: 1,
            cpu_per_native_den: 1,
            native_per_via,
            cpu_since_native_times_den: 0,
            native_since_via: 0,
            native_ticks: 0,
        }
    }

    /// Sets the frequency of the CPU in Hz, which includes any overclocking
    pub fn set_cpu_frequency(&mut self, cpu_freq: u64) {
        let gcd = num::integer::gcd(cpu_freq, self.native_freq);
        self.cpu_per_native_num = cpu_freq / gcd;
        self.cpu_per_native_den = self.native_freq / gcd;
        self.cpu_since_native_times_den = 0;
    }

    /// True when the CPU runs faster than the native clock
    pub fn is_overclocking(&self) -> bool {
        self.cpu_per_native_num != self.cpu_per_native_den
    }

    /// Native ticks elapsed since boot
    pub fn native_ticks(&self) -> Ticks {
        self.native_ticks
    }

    /// Runs the clock for up to `cpu_ticks` CPU ticks, stopping at the next
    /// VIA tick or after `max` native ticks, whichever comes first.
    ///
    /// `max` is typically used to stop on the bus's own events, such as the
    /// hblank and vblank boundaries of the compact Macs' video: `None` for
    /// no limit, otherwise at least 1.
    ///
    /// Returns how many native ticks were advanced, how many CPU ticks they
    /// consumed, and whether the clock stopped on a VIA tick. When
    /// the CPU ticks don't complete a native tick, they are all consumed and
    /// 0 native ticks are advanced.
    #[inline]
    pub fn advance(&mut self, cpu_ticks: Ticks, max: Option<Ticks>) -> Step {
        debug_assert!(max != Some(0));
        // Native ticks to the next VIA tick, and to run at most
        let to_via = self.native_per_via - self.native_since_via;
        let limit = max.map_or(to_via, |max| max.min(to_via));

        // Native ticks advanced, and the CPU ticks they used
        let num = self.cpu_per_native_num;
        let den = self.cpu_per_native_den;
        let (native, cpu_used) = if num == den {
            let native = limit.min(cpu_ticks);
            (native, native)
        } else {
            // CPU ticks for `limit` native ticks to complete, times `den`
            let needed_times_den = limit * num - self.cpu_since_native_times_den;
            // Rounded up to whole CPU ticks
            let needed = needed_times_den.div_ceil(den);
            if needed <= cpu_ticks {
                // The last CPU tick may run into the next native tick
                self.cpu_since_native_times_den = needed * den - needed_times_den;
                (limit, needed)
            } else {
                self.cpu_since_native_times_den += cpu_ticks * den;
                let native = self.cpu_since_native_times_den / num;
                self.cpu_since_native_times_den -= native * num;
                (native, cpu_ticks)
            }
        };

        self.native_ticks += native;
        self.native_since_via += native;
        let via_tick = self.native_since_via == self.native_per_via;
        if via_tick {
            self.native_since_via = 0;
        }
        Step {
            native,
            via_tick,
            cpu_used,
        }
    }

    /// Start of a device access cycle.
    ///
    /// When overclocking, the CPU waits until the native clock has caught
    /// up with it and a native tick starts; the access then lasts a native
    /// bus cycle: the CPU's bus cycle (`CpuBus::CYCLE`) in native ticks.
    /// Without overclocking, the access goes ahead with no wait states.
    ///
    /// Returns a DeviceCycle, which indicates whether to stall, or the wait
    /// states of the native bus cycle.
    pub fn device_sync<C: CpuBus>(&self, acc: &BusAccess<C>) -> DeviceCycle {
        if !self.is_overclocking() {
            return DeviceCycle::Access(0);
        }
        let num = self.cpu_per_native_num;
        let den = self.cpu_per_native_den;
        // CPU ticks since the last native tick, at the access, times `den`
        let cpu_since_native_times_den =
            (self.cpu_since_native_times_den + acc.pending * den) % num;
        // The native tick started less than one CPU tick ago
        let on_tick = cpu_since_native_times_den < den;
        if on_tick && acc.pending == 0 {
            // CPU ticks to the end of the native bus cycle, rounded up
            let cycle = (C::CYCLE * num - cpu_since_native_times_den).div_ceil(den);
            // Less the CPU ticks the CPU counts itself for the cycle
            return DeviceCycle::Access(cycle - C::CYCLE);
        }
        if on_tick {
            // Only to let the clock catch up with the CPU
            return DeviceCycle::Stall(0);
        }
        // CPU ticks to the next native tick, rounded up
        DeviceCycle::Stall((num - cpu_since_native_times_den).div_ceil(den))
    }

    /// Start of a VIA access cycle: the same, on a VIA tick instead of a
    /// native tick, with or without overclocking. The access then lasts a
    /// VIA cycle, which is one cycle of the VIA clock (the E clock).
    ///
    /// Returns a DeviceCycle, which indicates whether to stall, or the wait
    /// states of the VIA cycle.
    pub fn via_sync<C: CpuBus>(&self, acc: &BusAccess<C>) -> DeviceCycle {
        let num = self.cpu_per_native_num;
        let den = self.cpu_per_native_den;
        // CPU ticks the access is ahead of the start of the clock's current
        // native tick, times `den`
        let cpu_ahead_times_den = self.cpu_since_native_times_den + acc.pending * den;
        // Native ticks completed by then
        let native_done = cpu_ahead_times_den / num;
        // CPU ticks since the last native tick, at the access, times `den`
        let cpu_since_native_times_den = cpu_ahead_times_den % num;
        // Native ticks since the last VIA tick, at the access
        let native_since_via = (self.native_since_via + native_done) % self.native_per_via;
        // The VIA tick started less than one CPU tick ago
        let on_via_tick = native_since_via == 0 && cpu_since_native_times_den < den;
        if on_via_tick && acc.pending == 0 {
            // CPU ticks to the end of the VIA cycle, rounded up
            let cycle = (self.native_per_via * num - cpu_since_native_times_den).div_ceil(den);
            // Less the CPU ticks the CPU counts itself for the cycle
            return DeviceCycle::Access(cycle - C::CYCLE);
        }
        if on_via_tick {
            // Only to let the clock catch up with the CPU
            return DeviceCycle::Stall(0);
        }
        // Native ticks to the next VIA tick
        let native_to_via = self.native_per_via - native_since_via;
        // CPU ticks to the next VIA tick, rounded up
        DeviceCycle::Stall((native_to_via * num - cpu_since_native_times_den).div_ceil(den))
    }

    /// Adds the device's own wait states to the wait states of a device
    /// cycle, and returns the resulting total wait states, scaled to the CPU
    /// clock.
    ///
    /// `w` is what device_sync() or via_sync() returned: the duration of the
    /// device access cycle itself, beyond the CPU's base cycle
    /// (`CpuBus::CYCLE`), already in CPU ticks. `extra` is the number of
    /// native wait states the device adds itself (wait states inserted by
    /// the GLUE, or a recovery time). Without overclocking, this is
    /// `w + extra`.
    pub fn scale_dev_waits(&self, w: WaitTicks, extra: Ticks) -> WaitTicks {
        w + (extra * self.cpu_per_native_num).div_ceil(self.cpu_per_native_den)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::access::Bus68000;

    /// Runs `cpu` CPU ticks through the clock, returns the native ticks and
    /// the VIA ticks
    fn run(clock: &mut NativeClock, mut cpu: Ticks) -> (Ticks, Ticks) {
        let (mut native, mut via) = (0, 0);
        while cpu > 0 {
            let step = clock.advance(cpu, None);
            if step.native == 0 {
                break;
            }
            cpu -= step.cpu_used;
            native += step.native;
            via += Ticks::from(step.via_tick);
        }
        (native, via)
    }

    #[test]
    fn native_speed() {
        let mut clock = NativeClock::new(1000, 10);
        assert_eq!(run(&mut clock, 25), (25, 2));
        let step = clock.advance(100, None);
        assert_eq!((step.native, step.via_tick, step.cpu_used), (5, true, 5));
    }

    #[test]
    fn twice_as_fast() {
        let mut clock = NativeClock::new(1000, 10);
        clock.set_cpu_frequency(2000);
        assert_eq!(run(&mut clock, 41), (20, 2));
        // The odd CPU tick is half a native tick: one more completes it
        assert_eq!(run(&mut clock, 1), (1, 0));
    }

    #[test]
    fn uneven_ratio() {
        let mut clock = NativeClock::new(3, 10);
        clock.set_cpu_frequency(7);
        // 70 CPU ticks are 30 native ticks, whatever the batches
        let mut native = 0;
        for cpu in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 15] {
            native += run(&mut clock, cpu).0;
        }
        assert_eq!(native, 30);
    }

    #[test]
    fn stops_at_max() {
        let mut clock = NativeClock::new(1000, 10);
        clock.set_cpu_frequency(4000);
        let step = clock.advance(100, Some(3));
        assert_eq!((step.native, step.via_tick, step.cpu_used), (3, false, 12));
        let step = clock.advance(100, Some(20));
        assert_eq!((step.native, step.via_tick, step.cpu_used), (7, true, 28));
    }

    fn read(pending: Ticks) -> BusAccess<Bus68000> {
        BusAccess::read(0, 1, pending)
    }

    fn sync(clock: &mut NativeClock, cycle: &DeviceCycle) -> Option<WaitTicks> {
        match *cycle {
            DeviceCycle::Stall(n) => {
                run(clock, n);
                None
            }
            DeviceCycle::Access(w) => Some(w),
        }
    }

    #[test]
    fn device_sync_native_speed() {
        let clock = NativeClock::new(1000, 10);
        assert!(matches!(
            clock.device_sync(&read(7)),
            DeviceCycle::Access(0)
        ));
    }

    #[test]
    fn device_sync_overclocked() {
        let mut clock = NativeClock::new(1000, 10);
        clock.set_cpu_frequency(4000);
        // 6 CPU ticks in: half way through the second native tick
        let cycle = clock.device_sync(&read(6));
        assert!(matches!(cycle, DeviceCycle::Stall(2)));
        run(&mut clock, 8);
        // On a native tick: 4 native ticks of the 68000's cycle
        let cycle = clock.device_sync(&read(0));
        let w = sync(&mut clock, &cycle);
        assert_eq!(w, Some(16 - 4));
    }

    #[test]
    fn device_sync_uneven_ratio() {
        let mut clock = NativeClock::new(1000, 10);
        clock.set_cpu_frequency(2500);
        run(&mut clock, 3);
        // Native tick 1 started at 2.5 CPU ticks: on it, and the 4 native
        // ticks of the 68000's cycle end at 12.5, rounded up to 13
        assert!(matches!(
            clock.device_sync(&read(0)),
            DeviceCycle::Access(w) if w == 10 - 4
        ));
        run(&mut clock, 4);
        // 2 CPU ticks into native tick 2, native tick 3 starts at 7.5
        assert!(matches!(clock.device_sync(&read(0)), DeviceCycle::Stall(1)));
    }

    #[test]
    fn via_sync_native_speed() {
        let mut clock = NativeClock::new(1000, 10);
        // On a VIA tick, up to date: 10 native ticks of the VIA cycle
        assert!(matches!(clock.via_sync(&read(0)), DeviceCycle::Access(6)));
        // On a VIA tick, but the clock has to be brought up to date first
        assert!(matches!(clock.via_sync(&read(10)), DeviceCycle::Stall(0)));
        run(&mut clock, 3);
        assert!(matches!(clock.via_sync(&read(0)), DeviceCycle::Stall(7)));
        assert!(matches!(clock.via_sync(&read(2)), DeviceCycle::Stall(5)));
    }

    #[test]
    fn via_sync_overclocked() {
        let mut clock = NativeClock::new(1000, 10);
        clock.set_cpu_frequency(3000);
        run(&mut clock, 4);
        // A third of a native tick past native tick 1
        let cycle = clock.via_sync(&read(0));
        assert!(matches!(cycle, DeviceCycle::Stall(26)));
        assert_eq!(sync(&mut clock, &cycle), None);
        let cycle = clock.via_sync(&read(0));
        let w = sync(&mut clock, &cycle);
        assert_eq!(w, Some(30 - 4));
    }

    #[test]
    fn short_batch() {
        let mut clock = NativeClock::new(1000, 10);
        clock.set_cpu_frequency(4000);
        let step = clock.advance(3, None);
        assert_eq!((step.native, step.via_tick, step.cpu_used), (0, false, 3));
        let step = clock.advance(1, None);
        assert_eq!((step.native, step.via_tick, step.cpu_used), (1, false, 1));
    }
}
