//! The refreshes of the display on Windows, from the Desktop Window Manager.
//!
//! The compositor (DWM) presents every window, and with vsync the redraws keep
//! to its refreshes. It tells when the display last refreshed and how often it
//! does, in QueryPerformanceCounter units, which are converted to `Instant`s
//! here. With several displays, these are the refreshes the compositor keeps
//! to, those of the main display.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows_sys::Win32::Graphics::Dwm::{DWM_TIMING_INFO, DwmGetCompositionTimingInfo};
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

/// QueryPerformanceCounter's ticks per second, which are fixed at boot
fn qpc_frequency() -> Option<u64> {
    static FREQUENCY: OnceLock<u64> = OnceLock::new();
    let frequency = *FREQUENCY.get_or_init(|| {
        let mut frequency = 0;
        // SAFETY: writes to the given i64
        unsafe { QueryPerformanceFrequency(&raw mut frequency) };
        u64::try_from(frequency).unwrap_or(0)
    });
    (frequency > 0).then_some(frequency)
}

fn ticks_to_duration(ticks: u64, frequency: u64) -> Duration {
    let nanos = u128::from(ticks) * 1_000_000_000 / u128::from(frequency);
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

/// When the display last refreshed, and how often it does, if the compositor
/// tells
pub fn refresh_grid() -> Option<(Instant, Duration)> {
    let frequency = qpc_frequency()?;
    let mut info = DWM_TIMING_INFO {
        cbSize: size_of::<DWM_TIMING_INFO>() as u32,
        ..Default::default()
    };
    // SAFETY: info is a DWM_TIMING_INFO with its size set. Since Windows 8.1,
    // the window must be null: the timing is the compositor's.
    if unsafe { DwmGetCompositionTimingInfo(std::ptr::null_mut(), &raw mut info) } < 0 {
        return None;
    }
    // Take the clocks together, to place the refresh on the Instant's clock
    let now = Instant::now();
    let mut now_qpc = 0;
    // SAFETY: writes to the given i64
    unsafe { QueryPerformanceCounter(&raw mut now_qpc) };
    let now_qpc = u64::try_from(now_qpc).ok()?;

    // Copied out, as the struct is packed
    let vblank = info.qpcVBlank;
    let period = info.qpcRefreshPeriod;
    if vblank == 0 || period == 0 {
        return None;
    }
    let last_refresh = if vblank <= now_qpc {
        now.checked_sub(ticks_to_duration(now_qpc - vblank, frequency))?
    } else {
        now + ticks_to_duration(vblank - now_qpc, frequency)
    };
    Some((last_refresh, ticks_to_duration(period, frequency)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_grid_is_plausible() {
        // No compositor (as in some service sessions): nothing to check
        let Some((last_refresh, period)) = refresh_grid() else {
            return;
        };
        println!(
            "refresh period {period:?}, {:.3} Hz",
            1.0 / period.as_secs_f64()
        );
        assert!(period > Duration::from_millis(2) && period < Duration::from_millis(100));
        // The last refresh is no more than a refresh or so from now
        let now = Instant::now();
        let off = if last_refresh <= now {
            now - last_refresh
        } else {
            last_refresh - now
        };
        assert!(off <= period * 2, "{off:?}");
    }
}
