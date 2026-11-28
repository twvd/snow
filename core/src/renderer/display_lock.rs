//! Locks the machine's frames to the refresh of the host's display.
//!
//! A machine whose frame rate is just off the display's, like the 60.15 Hz of
//! the compact Macs on a 60 or 120 Hz display, drifts against the refreshes: a
//! frame is shown for one refresh too few or too many every few seconds, which
//! shows as judder in smooth motion. When the rates are close enough, this runs
//! the machine at a speed slightly off the real one (through [`PaceControl`]),
//! so its frames come at exactly a whole number of refreshes, and steers it so
//! each comes mid-way between two refreshes, out of reach of the timing jitter.

use std::time::{Duration, Instant};

use super::PaceControl;

/// Most the machine's speed can be off the real one for the lock, and with the
/// steering: the audio playback makes up for up to 0.5%
const MAX_SPEED_OFFSET: f64 = 0.004;
const MAX_STEERED_OFFSET: f64 = 0.005;

/// Where a frame should come between two refreshes, as a fraction of the time
/// between them
const TARGET_PHASE: f64 = 0.5;

/// How hard the speed is steered by the phase, per refresh a frame takes
const PHASE_GAIN: f64 = 0.01;

/// How fast the steering takes up a steady error in the speed
const INTEGRAL_GAIN: f64 = PHASE_GAIN / 50.0;

/// How fast the estimate of the machine's frame time follows new frames
const PERIOD_SMOOTHING: f64 = 0.02;

/// Frames further apart than this are not consecutive, as after a pause
const MAX_FRAME_GAP: Duration = Duration::from_millis(100);

pub struct DisplayLock {
    pace: PaceControl,
    enabled: bool,
    /// The machine's time between frames at its real speed, in seconds
    frame_period: Option<f64>,
    last_frame: Option<Instant>,
    /// The steering's accumulated speed offset
    integral: f64,
    locked: bool,
}

impl DisplayLock {
    pub fn new(pace: PaceControl) -> Self {
        Self {
            pace,
            enabled: true,
            frame_period: None,
            last_frame: None,
            integral: 0.0,
            locked: false,
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.unlock();
        }
    }

    /// Whether the machine's frames are locked to the display now
    pub const fn is_locked(&self) -> bool {
        self.locked
    }

    fn unlock(&mut self) {
        self.locked = false;
        self.integral = 0.0;
        self.pace.set(1.0);
    }

    /// Takes a new frame of the machine: when it was rendered, and when the
    /// display last refreshed and how often it does
    pub fn frame(&mut self, rendered: Instant, last_refresh: Instant, refresh_period: Duration) {
        let previous = self.last_frame.replace(rendered);
        if !self.enabled {
            return;
        }
        let Some(previous) = previous else {
            return;
        };
        let gap = rendered.saturating_duration_since(previous);
        if gap.is_zero() || gap > MAX_FRAME_GAP {
            return;
        }

        // At the real speed, frames would be this far apart
        let speed = self.pace.get();
        let period = gap.as_secs_f64() * speed;
        let frame_period = match self.frame_period {
            // A skipped frame, or the machine not paced by the clock
            Some(p) if (period - p).abs() > p * 0.25 => {
                self.unlock();
                self.frame_period = Some(period);
                return;
            }
            Some(p) => PERIOD_SMOOTHING.mul_add(period - p, p),
            None => period,
        };
        self.frame_period = Some(frame_period);

        // Refreshes per frame, and the speed that makes it exactly that
        let refresh = refresh_period.as_secs_f64();
        let refreshes = (frame_period / refresh).round();
        let base = frame_period / (refreshes * refresh);
        if refreshes < 1.0 || (base - 1.0).abs() > MAX_SPEED_OFFSET {
            if self.locked {
                self.unlock();
            }
            return;
        }
        self.locked = true;

        // Steer so frames come at the target phase: late ones speed it up
        let since = if rendered >= last_refresh {
            (rendered - last_refresh).as_secs_f64()
        } else {
            -(last_refresh - rendered).as_secs_f64()
        };
        let phase = (since / refresh).rem_euclid(1.0);
        let error = phase - TARGET_PHASE;
        let gain = PHASE_GAIN / refreshes;
        self.integral = (self.integral + error * INTEGRAL_GAIN / refreshes)
            .clamp(-MAX_SPEED_OFFSET, MAX_SPEED_OFFSET);
        let speed = base * (1.0 + error * gain + self.integral);
        self.pace
            .set(speed.clamp(1.0 - MAX_STEERED_OFFSET, 1.0 + MAX_STEERED_OFFSET));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs a machine with frames `frame_period` apart at its real speed on a
    /// display refreshing every `refresh_period`, for `frames` frames. Returns
    /// the phase of each frame between two refreshes.
    fn simulate(
        lock: &mut DisplayLock,
        frame_period: f64,
        refresh_period: f64,
        frames: usize,
    ) -> Vec<f64> {
        let start = Instant::now();
        let mut t = 0.0;
        let mut phases = vec![];
        for _ in 0..frames {
            t += frame_period / lock.pace.get();
            let refreshes = (t / refresh_period).floor();
            let last_refresh = start + Duration::from_secs_f64(refreshes * refresh_period);
            let rendered = start + Duration::from_secs_f64(t);
            phases.push((t / refresh_period).fract());
            lock.frame(
                rendered,
                last_refresh,
                Duration::from_secs_f64(refresh_period),
            );
        }
        phases
    }

    #[test]
    fn locks_compact_mac_to_120hz() {
        let pace = PaceControl::default();
        let mut lock = DisplayLock::new(pace.clone());
        let phases = simulate(&mut lock, 1.0 / 60.147, 1.0 / 120.0, 3000);
        assert!(lock.is_locked());
        // Runs at 60 frames per second
        assert!(pace.get().mul_add(60.147, -60.0).abs() < 0.01, "{}", pace.get());
        // Settled mid-way between refreshes
        for phase in &phases[2000..] {
            assert!((phase - TARGET_PHASE).abs() < 0.05, "{phase}");
        }
    }

    #[test]
    fn locks_compact_mac_to_60hz() {
        let pace = PaceControl::default();
        let mut lock = DisplayLock::new(pace);
        let phases = simulate(&mut lock, 1.0 / 60.147, 1.0 / 60.0, 3000);
        assert!(lock.is_locked());
        for phase in &phases[2000..] {
            assert!((phase - TARGET_PHASE).abs() < 0.05, "{phase}");
        }
    }

    #[test]
    fn leaves_mac_ii_alone_at_60hz() {
        let pace = PaceControl::default();
        let mut lock = DisplayLock::new(pace.clone());
        simulate(&mut lock, 1.0 / 66.67, 1.0 / 60.0, 500);
        assert!(!lock.is_locked());
        assert_eq!(pace.get(), 1.0);
    }

    #[test]
    fn disabled_runs_at_real_speed() {
        let pace = PaceControl::default();
        let mut lock = DisplayLock::new(pace.clone());
        simulate(&mut lock, 1.0 / 60.147, 1.0 / 120.0, 500);
        lock.set_enabled(false);
        assert!(!lock.is_locked());
        assert_eq!(pace.get(), 1.0);
    }
}
