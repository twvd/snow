//! Native clock of the devices of a bus, which the CPU may run faster than

use serde::{Deserialize, Serialize};

use crate::bus::access::{BusAccess, CpuBus};
use crate::tickable::Ticks;

/// Wait states, in CPU ticks
pub type WaitTicks = Ticks;

/// Frequency of C16M (15.6672 MHz), the 16 MHz clock of the Macs
pub const C16M_FREQUENCY: Ticks = 15_667_200;

/// The VIA clock is C16M / 20 (783.36 kHz) on the Macs with C16M
const C16M_PER_VIA: Ticks = 20;

/// The VIA clock (E clock) is C8M / 10 on the compact Macs, which have no
/// C16M
const C8M_PER_VIA: Ticks = 10;

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

/// Helper struct used to scale a clock from an input frequency to an output
/// frequency.
///
/// Rational arithmetic is used to avoid errors.
#[derive(Copy, Clone, Serialize, Deserialize)]
struct ClockScaler {
    /// Represents a fractional number of elapsed ticks.
    ///
    /// On each input tick, this value increases by `out_freq`.
    /// The number of elapsed input ticks is `n / out_freq`.
    /// The number of elapsed output ticks is `n / in_freq`.
    n: Ticks,
    in_freq: Ticks,
    out_freq: Ticks,
}

impl ClockScaler {
    fn new(in_freq: Ticks, out_freq: Ticks) -> Self {
        Self {
            n: 0,
            in_freq,
            out_freq,
        }
    }

    fn plus_in_ticks(self, in_ticks: Ticks) -> Self {
        Self {
            n: self.n + in_ticks * self.out_freq,
            ..self
        }
    }

    fn minus_out_ticks(self, out_ticks: Ticks) -> Self {
        Self {
            n: self.n - out_ticks * self.in_freq,
            ..self
        }
    }

    /// Return a new scaler advanced by the given input ticks, then wrapped
    /// around to the next output tick. Returns (wrapped scaler, output ticks).
    fn advance_and_wrap(self, in_ticks: Ticks) -> (Self, Ticks) {
        let advanced = self.plus_in_ticks(in_ticks);
        let out_ticks = advanced.out_ticks();
        (advanced.minus_out_ticks(out_ticks), out_ticks)
    }

    /// Return the number of input ticks since the last output tick, rounded
    /// down, once the output ticks have been subtracted
    fn in_ticks(&self) -> Ticks {
        self.n / self.out_freq
    }

    /// Return the number of output ticks that have elapsed, rounded down
    fn out_ticks(&self) -> Ticks {
        self.n / self.in_freq
    }

    /// Return the number of output ticks that have elapsed, rounded up
    fn out_ticks_rounded_up(&self) -> Ticks {
        self.n.div_ceil(self.in_freq)
    }

    /// Return the number of input ticks needed until a given number of output ticks
    /// have elapsed
    fn in_ticks_until_out_ticks(&self, out_ticks: Ticks) -> Ticks {
        (out_ticks * self.in_freq)
            .saturating_sub(self.n)
            .div_ceil(self.out_freq)
    }
}

/// Native clock of a machine's device bus, which the CPU may run faster
/// than when overclocking.
///
/// The CPU runs on CPU ticks, the devices on native ticks. A native tick is
/// never shorter than a CPU tick. The clock also produces the VIA clock,
/// known as the E clock on the 68000 Macs.
///
/// With `C16M`, the clock also produces C16M, the 16 MHz clock of the Macs
/// from the Mac II on, which the native clock may differ from: the VIA clock
/// is then C16M / 20, and devices clocked by C16M can have their cycles and
/// time counted in C16M ticks. Without it, on the compact Macs, the native
/// clock is C8M and the VIA clock is C8M / 10.
#[derive(Serialize, Deserialize)]
pub struct NativeClock<const C16M: bool = true> {
    /// Native frequency, in Hz
    native_freq: Ticks,
    /// Frequency of the CPU in Hz, which includes any overclocking
    cpu_freq: Ticks,
    /// Native ticks per VIA tick, when C16M is not scaled
    native_per_via: Ticks,
    /// The CPU-to-native clock downscaler
    native_clockgen: ClockScaler,
    /// The CPU-to-C16M clock downscaler, when C16M is scaled
    c16m_clockgen: ClockScaler,
    /// Ticks since the last VIA tick: C16M ticks when C16M is scaled, native
    /// ticks otherwise
    since_via: Ticks,
    /// Native ticks elapsed since boot
    native_ticks: Ticks,
    /// C16M ticks elapsed since boot, when C16M is scaled
    c16m_ticks: Ticks,
}

impl NativeClock<true> {
    /// C16M ticks elapsed since boot
    pub fn c16m_ticks(&self) -> Ticks {
        if self.scaled_c16m() {
            self.c16m_ticks
        } else {
            self.native_ticks
        }
    }

    /// Start of a device access cycle on C16M: the same as device_sync(),
    /// for a device clocked by C16M instead of the native clock. The access
    /// lasts the CPU's bus cycle in C16M ticks, or the CPU's own bus cycle
    /// if that is longer.
    pub fn device16_sync<C: CpuBus>(&self, acc: &BusAccess<C>) -> DeviceCycle {
        if !self.scaled_c16m() {
            return self.device_sync(acc);
        }
        Self::sync(acc, self.c16m_clockgen, 1, 0, C::CYCLE)
    }

    /// Adds the device's own wait states, `extra` C16M ticks, to the wait
    /// states `w` that device16_sync() returned, scaled to the CPU clock
    pub fn scale_dev16_waits(&self, w: WaitTicks, extra: Ticks) -> WaitTicks {
        w + (extra * self.cpu_freq).div_ceil(C16M_FREQUENCY)
    }
}

impl<const C16M: bool> NativeClock<C16M> {
    /// Native clock at `native_freq` Hz. With C16M, the VIA clock is
    /// C16M / 20. Without, the native clock is C8M and the VIA clock is
    /// C8M / 10.
    pub fn new(native_freq: Ticks) -> Self {
        Self {
            native_freq,
            cpu_freq: native_freq,
            native_per_via: if C16M { C16M_PER_VIA } else { C8M_PER_VIA },
            native_clockgen: ClockScaler::new(native_freq, native_freq),
            c16m_clockgen: ClockScaler::new(native_freq, C16M_FREQUENCY),
            since_via: 0,
            native_ticks: 0,
            c16m_ticks: 0,
        }
    }

    /// C16M is produced from the CPU clock by a clock scaler, when there is
    /// C16M and the native clock is not C16M
    #[inline(always)]
    fn scaled_c16m(&self) -> bool {
        C16M && self.native_freq != C16M_FREQUENCY
    }

    /// Sets the frequency of the CPU in Hz, which includes any overclocking
    pub fn set_cpu_frequency(&mut self, cpu_freq: Ticks) {
        self.cpu_freq = cpu_freq;
        self.native_clockgen = ClockScaler::new(cpu_freq, self.native_freq);
        self.c16m_clockgen = ClockScaler::new(cpu_freq, C16M_FREQUENCY);
    }

    /// True when the CPU runs faster than the native clock
    pub fn is_overclocking(&self) -> bool {
        self.cpu_freq != self.native_freq
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
        // Native ticks to the next VIA tick, or to the native tick in which
        // it happens, and to run at most
        let to_via = if self.scaled_c16m() {
            // The CPU tick that completes the C16M tick of the VIA tick, and
            // the native ticks up to the end of the one it falls in
            let cpu = self
                .c16m_clockgen
                .in_ticks_until_out_ticks(C16M_PER_VIA - self.since_via);
            self.native_clockgen
                .plus_in_ticks(cpu)
                .out_ticks_rounded_up()
        } else {
            self.native_per_via - self.since_via
        };
        let limit = max.map_or(to_via, |max| max.min(to_via));

        // Native ticks advanced, and the CPU ticks they used
        let (native, cpu_used) = if self.cpu_freq == self.native_freq {
            let native = limit.min(cpu_ticks);
            (native, native)
        } else {
            // CPU ticks for `limit` native ticks to complete
            let needed = self.native_clockgen.in_ticks_until_out_ticks(limit);
            if needed <= cpu_ticks {
                // The last CPU tick may run into the next native tick
                self.native_clockgen = self
                    .native_clockgen
                    .plus_in_ticks(needed)
                    .minus_out_ticks(limit);
                (limit, needed)
            } else {
                let native;
                (self.native_clockgen, native) = self.native_clockgen.advance_and_wrap(cpu_ticks);
                (native, cpu_ticks)
            }
        };

        self.native_ticks += native;
        let per_via = if self.scaled_c16m() {
            let c16m;
            (self.c16m_clockgen, c16m) = self.c16m_clockgen.advance_and_wrap(cpu_used);
            self.c16m_ticks += c16m;
            self.since_via += c16m;
            C16M_PER_VIA
        } else {
            self.since_via += native;
            self.native_per_via
        };
        let via_tick = self.since_via >= per_via;
        if via_tick {
            self.since_via -= per_via;
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
        Self::sync(acc, self.native_clockgen, 1, 0, C::CYCLE)
    }

    /// Start of a VIA access cycle: the same, on a VIA tick instead of a
    /// native tick, with or without overclocking. The access then lasts a
    /// VIA cycle, which is one cycle of the VIA clock (the E clock).
    ///
    /// Returns a DeviceCycle, which indicates whether to stall, or the wait
    /// states of the VIA cycle.
    pub fn via_sync<C: CpuBus>(&self, acc: &BusAccess<C>) -> DeviceCycle {
        if self.scaled_c16m() {
            Self::sync(
                acc,
                self.c16m_clockgen,
                C16M_PER_VIA,
                self.since_via,
                C16M_PER_VIA,
            )
        } else {
            let per_via = self.native_per_via;
            Self::sync(acc, self.native_clockgen, per_via, self.since_via, per_via)
        }
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
        w + (extra * self.cpu_freq).div_ceil(self.native_freq)
    }

    /// Start of an access cycle on the clock that `clockgen` produces from
    /// the CPU clock, synchronous with every `period` ticks of it, `phase`
    /// ticks into the period. The access then lasts `cycle` ticks.
    fn sync<C: CpuBus>(
        acc: &BusAccess<C>,
        clockgen: ClockScaler,
        period: Ticks,
        phase: Ticks,
        cycle: Ticks,
    ) -> DeviceCycle {
        // The clock after acc.pending CPU ticks: the ticks done, and
        // `partial_tick`, the progress into the following tick
        let (partial_tick, ticks_done) = clockgen.advance_and_wrap(acc.pending);
        let since_start = (phase + ticks_done) % period;
        // On the start of the period if less than one CPU tick into it
        let on_start = since_start == 0 && partial_tick.in_ticks() < 1;
        if on_start && acc.pending == 0 {
            return DeviceCycle::Access(
                partial_tick
                    .in_ticks_until_out_ticks(cycle)
                    .saturating_sub(C::CYCLE),
            );
        }
        if on_start {
            // Only to let the clock catch up with the CPU
            return DeviceCycle::Stall(0);
        }
        DeviceCycle::Stall(partial_tick.in_ticks_until_out_ticks(period - since_start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::access::Bus68000;

    /// Runs `cpu` CPU ticks through the clock, returns the native ticks and
    /// the VIA ticks
    fn run<const C16M: bool>(clock: &mut NativeClock<C16M>, mut cpu: Ticks) -> (Ticks, Ticks) {
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
        let mut clock = NativeClock::<false>::new(1000);
        assert_eq!(run(&mut clock, 25), (25, 2));
        let step = clock.advance(100, None);
        assert_eq!((step.native, step.via_tick, step.cpu_used), (5, true, 5));
    }

    #[test]
    fn twice_as_fast() {
        let mut clock = NativeClock::<false>::new(1000);
        clock.set_cpu_frequency(2000);
        assert_eq!(run(&mut clock, 41), (20, 2));
        // The odd CPU tick is half a native tick: one more completes it
        assert_eq!(run(&mut clock, 1), (1, 0));
    }

    #[test]
    fn uneven_ratio() {
        let mut clock = NativeClock::<false>::new(3);
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
        let mut clock = NativeClock::<false>::new(1000);
        clock.set_cpu_frequency(4000);
        let step = clock.advance(100, Some(3));
        assert_eq!((step.native, step.via_tick, step.cpu_used), (3, false, 12));
        let step = clock.advance(100, Some(20));
        assert_eq!((step.native, step.via_tick, step.cpu_used), (7, true, 28));
    }

    fn read(pending: Ticks) -> BusAccess<Bus68000> {
        BusAccess::read(0, 1, pending)
    }

    fn sync<const C16M: bool>(
        clock: &mut NativeClock<C16M>,
        cycle: &DeviceCycle,
    ) -> Option<WaitTicks> {
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
        let clock = NativeClock::<false>::new(1000);
        assert!(matches!(
            clock.device_sync(&read(7)),
            DeviceCycle::Access(0)
        ));
    }

    #[test]
    fn device_sync_overclocked() {
        let mut clock = NativeClock::<false>::new(1000);
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
        let mut clock = NativeClock::<false>::new(1000);
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
        let mut clock = NativeClock::<false>::new(1000);
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
        let mut clock = NativeClock::<false>::new(1000);
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
        let mut clock = NativeClock::<false>::new(1000);
        clock.set_cpu_frequency(4000);
        let step = clock.advance(3, None);
        assert_eq!((step.native, step.via_tick, step.cpu_used), (0, false, 3));
        let step = clock.advance(1, None);
        assert_eq!((step.native, step.via_tick, step.cpu_used), (1, false, 1));
    }

    /// 25 MHz and 33.33 MHz, whose ticks don't divide C16M
    const F25M: Ticks = 25_000_000;
    const F33M: Ticks = 33_333_333;

    #[test]
    fn via_from_c16m() {
        for freq in [F25M, F33M] {
            let mut clock = NativeClock::<true>::new(freq);
            assert_eq!(run(&mut clock, freq), (freq, C16M_FREQUENCY / 20));
            let mut clock = NativeClock::<true>::new(freq);
            clock.set_cpu_frequency(2 * freq);
            assert_eq!(run(&mut clock, 2 * freq), (freq, C16M_FREQUENCY / 20));
        }
    }

    #[test]
    fn via_sync_c16m() {
        // A VIA cycle is 31.91 CPU ticks
        let mut clock = NativeClock::<true>::new(F25M);
        assert!(matches!(clock.via_sync(&read(0)), DeviceCycle::Access(w) if w == 32 - 4));
        run(&mut clock, 5);
        assert!(matches!(clock.via_sync(&read(0)), DeviceCycle::Stall(27)));
        run(&mut clock, 27);
        // 0.09 CPU ticks past the VIA tick, the next one is at 63.83
        assert!(matches!(clock.via_sync(&read(0)), DeviceCycle::Access(w) if w == 32 - 4));
        assert!(matches!(clock.via_sync(&read(3)), DeviceCycle::Stall(29)));
    }

    #[test]
    fn via_sync_c16m_33m() {
        // A VIA cycle is 42.55 CPU ticks
        let mut clock = NativeClock::<true>::new(F33M);
        assert!(matches!(clock.via_sync(&read(0)), DeviceCycle::Access(w) if w == 43 - 4));
        run(&mut clock, 5);
        assert!(matches!(clock.via_sync(&read(0)), DeviceCycle::Stall(38)));
        run(&mut clock, 38);
        // 0.45 CPU ticks past the VIA tick, the next one is at 85.10
        assert!(matches!(clock.via_sync(&read(0)), DeviceCycle::Access(w) if w == 43 - 4));
    }

    #[test]
    fn device16_sync() {
        // A C16M tick is 1.60 CPU ticks
        let mut clock = NativeClock::<true>::new(F25M);
        // 4 C16M ticks end at 6.38
        assert!(matches!(clock.device16_sync(&read(0)), DeviceCycle::Access(w) if w == 7 - 4));
        run(&mut clock, 1);
        assert!(matches!(
            clock.device16_sync(&read(0)),
            DeviceCycle::Stall(1)
        ));
        run(&mut clock, 1);
        // 0.40 CPU ticks past the C16M tick at 1.60, 4 C16M ticks end at 7.98
        assert!(matches!(clock.device16_sync(&read(0)), DeviceCycle::Access(w) if w == 6 - 4));
        assert_eq!(clock.scale_dev16_waits(0, 2), 4);
    }

    #[test]
    fn device16_sync_33m() {
        // A C16M tick is 2.13 CPU ticks
        let mut clock = NativeClock::<true>::new(F33M);
        // 4 C16M ticks end at 8.51
        assert!(matches!(clock.device16_sync(&read(0)), DeviceCycle::Access(w) if w == 9 - 4));
        run(&mut clock, 1);
        assert!(matches!(
            clock.device16_sync(&read(0)),
            DeviceCycle::Stall(2)
        ));
        run(&mut clock, 2);
        // 0.87 CPU ticks past the C16M tick at 2.13, 4 C16M ticks end at 10.64
        assert!(matches!(clock.device16_sync(&read(0)), DeviceCycle::Access(w) if w == 8 - 4));
        assert_eq!(clock.scale_dev16_waits(0, 2), 5);
    }

    #[test]
    fn device16_sync_native_c16m() {
        let mut clock = NativeClock::<true>::new(C16M_FREQUENCY);
        assert!(matches!(
            clock.device16_sync(&read(5)),
            DeviceCycle::Access(0)
        ));
        clock.set_cpu_frequency(4 * C16M_FREQUENCY);
        run(&mut clock, 6);
        assert!(matches!(
            clock.device16_sync(&read(0)),
            DeviceCycle::Stall(2)
        ));
        assert_eq!(clock.scale_dev16_waits(0, 2), 8);
    }

    #[test]
    fn c16m_ticks() {
        let mut clock = NativeClock::<true>::new(F25M);
        run(&mut clock, F25M);
        assert_eq!(clock.c16m_ticks(), C16M_FREQUENCY);
        let mut clock = NativeClock::<true>::new(C16M_FREQUENCY);
        clock.set_cpu_frequency(2 * C16M_FREQUENCY);
        run(&mut clock, 2 * C16M_FREQUENCY);
        assert_eq!(clock.c16m_ticks(), C16M_FREQUENCY);
    }
}
