//! Native clock of the devices of a bus, which the CPU may run faster than

use serde::{Deserialize, Serialize};

use crate::tickable::Ticks;

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
        }
    }

    /// Sets the frequency of the CPU in Hz, which includes any overclocking
    pub fn set_cpu_frequency(&mut self, cpu_freq: u64) {
        let gcd = num::integer::gcd(cpu_freq, self.native_freq);
        self.cpu_per_native_num = cpu_freq / gcd;
        self.cpu_per_native_den = self.native_freq / gcd;
        self.cpu_since_native_times_den = 0;
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
