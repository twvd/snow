//! Variable refresh rate: on a VRR display, redraw once per frame of the
//! machine, as each one comes, so the display refreshes at the machine's own
//! rate (60.15 Hz, 66.67 Hz, 75 Hz...) and shows every frame for as long.
//!
//! Each new frame wakes the event loop directly, and the redraw for it is done
//! right away. Any other redraw is held back until the next frame, unless the
//! machine has not made one for a while (stopped, or not running yet), so the
//! UI still responds then.
//!
//! The wake goes around egui's repaint requests: egui drops a request when one
//! is already pending, and each immediate request leaves one pending (it
//! repaints twice), so a frame would wait for whatever next woke the event
//! loop.
//!
//! Only in fullscreen: on Windows, a windowed OpenGL app is composed by the
//! desktop window manager, which keeps to the display's fixed refresh.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use eframe::UserEvent;
use eframe::egui::ViewportId;
use winit::event_loop::EventLoopProxy;
use winit::window::WindowId;

/// Without a frame of the machine for this long, redraws go ahead as usual
const IDLE_AFTER: Duration = Duration::from_millis(50);

/// Marks the wake-up events sent here, which eframe never sees
const WAKE_PASS_NR: u64 = u64::MAX;

static PROXY: OnceLock<Mutex<EventLoopProxy<UserEvent>>> = OnceLock::new();
static START: OnceLock<Instant> = OnceLock::new();
/// Whether redraws keep to the frames of the machine now
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Frames of the machine so far
static FRAMES: AtomicU64 = AtomicU64::new(0);
/// When the last one came, in microseconds since START
static LAST_FRAME_US: AtomicU64 = AtomicU64::new(0);

fn now_us() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_micros() as u64
}

pub fn set_proxy(proxy: EventLoopProxy<UserEvent>) {
    let _ = PROXY.set(Mutex::new(proxy));
}

/// Whether redraws keep to the frames of the machine now
pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

pub fn set_active(active: bool) {
    ACTIVE.store(active, Ordering::Relaxed);
}

/// Whether an event is a wake-up sent by [`frame_ready`]
pub const fn is_wake(event: &UserEvent) -> bool {
    matches!(
        event,
        UserEvent::RequestRepaint {
            cumulative_pass_nr: WAKE_PASS_NR,
            ..
        }
    )
}

/// A new frame of the machine is ready: from the emulator's thread
pub fn frame_ready() {
    LAST_FRAME_US.store(now_us(), Ordering::Relaxed);
    FRAMES.fetch_add(1, Ordering::Release);
    if active()
        && let Some(proxy) = PROXY.get()
    {
        let _ = proxy.lock().unwrap().send_event(UserEvent::RequestRepaint {
            viewport_id: ViewportId::ROOT,
            when: Instant::now(),
            cumulative_pass_nr: WAKE_PASS_NR,
        });
    }
}

/// Lets the redraws through once per frame of the machine, while active
#[derive(Default)]
pub struct VrrGate {
    /// Frames of the machine at the last redraw
    drawn: u64,
    /// The window to draw the frames in
    pub window: Option<WindowId>,
}

impl VrrGate {
    /// Whether a frame of the machine came since the last redraw
    pub fn new_frame(&self) -> bool {
        active() && FRAMES.load(Ordering::Acquire) != self.drawn
    }

    /// Whether a redraw can go ahead now: once per frame of the machine, or
    /// any time while it makes none, or while not active
    pub fn try_redraw(&mut self) -> bool {
        let frames = FRAMES.load(Ordering::Acquire);
        let new = frames != self.drawn;
        self.drawn = frames;
        new || self.idle_in().is_none()
    }

    /// How long until the machine counts as idle, if active and it does not
    /// yet
    pub fn idle_in(&self) -> Option<Duration> {
        if !active() {
            return None;
        }
        let since =
            Duration::from_micros(now_us().saturating_sub(LAST_FRAME_US.load(Ordering::Relaxed)));
        IDLE_AFTER.checked_sub(since).filter(|d| !d.is_zero())
    }
}
