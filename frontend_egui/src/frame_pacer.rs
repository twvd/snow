//! Paces eframe's redraws to the display's refresh on macOS, without vsync.
//!
//! With vsync, the OpenGL buffer swap blocks the main thread until the display
//! refreshes. Pointer events can only be handled between redraws then, so they
//! reach the emulator late and in bunches, which makes the cursor of the
//! absolute mouse mode judder. Instead, vsync is off and a display link lets at
//! most one redraw through per refresh, right after it. A redraw asked for
//! sooner is held back until the next refresh, as with vsync, but without
//! holding up the main thread.
//!
//! The display link is the window's CADisplayLink, which follows the window
//! from display to display and keeps to the actual refreshes of variable
//! refresh rate (ProMotion) displays, which CVDisplayLink does not. It needs
//! macOS 14; on older versions, vsync paces the redraws.

use std::cell::Cell;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::{MainThreadOnly, available, define_class, msg_send, sel};
use objc2_app_kit::NSWindow;
use objc2_foundation::{
    MainThreadMarker, NSObject, NSObjectProtocol, NSRunLoop, NSRunLoopCommonModes,
};
use objc2_quartz_core::{CACurrentMediaTime, CADisplayLink, CAFrameRateRange};

thread_local! {
    /// Refreshes of the display so far
    static REFRESHES: Cell<u64> = const { Cell::new(0) };
    /// When the display last refreshed, and how often it does
    static GRID: Cell<Option<(Instant, Duration)>> = const { Cell::new(None) };
}

/// When the display last refreshed, and how often it does, once known
pub fn refresh_grid() -> Option<(Instant, Duration)> {
    GRID.get()
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and Target has no Drop
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SnowFramePacerTarget"]
    struct Target;

    unsafe impl NSObjectProtocol for Target {}

    impl Target {
        #[unsafe(method(refresh:))]
        fn refresh(&self, link: &CADisplayLink) {
            // The link's times are in seconds of CACurrentMediaTime()
            let now = Instant::now();
            let ago = (CACurrentMediaTime() - link.timestamp()).max(0.0);
            let period = link.targetTimestamp() - link.timestamp();
            if period > 0.0 {
                GRID.set(Some((
                    now - Duration::from_secs_f64(ago),
                    Duration::from_secs_f64(period),
                )));
            }
            REFRESHES.set(REFRESHES.get() + 1);
        }
    }
);

impl Target {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        // SAFETY: NSObject's init
        unsafe { msg_send![Self::alloc(mtm), init] }
    }
}

pub struct FramePacer {
    /// Made with the window, at its first redraw
    link: Option<Retained<CADisplayLink>>,
    /// Refresh of the last redraw let through
    last_redraw: u64,
}

impl FramePacer {
    /// A frame pacer, where the display links it needs are to be had
    pub fn new() -> Option<Self> {
        available!(macos = 14.0).then_some(Self {
            link: None,
            last_redraw: 0,
        })
    }

    /// Starts following the refreshes of the display the window is on
    fn start(&mut self, window_id: winit::window::WindowId) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        // SAFETY: on macOS, winit's window id is the address of its NSWindow,
        // which lives as long as the window, and so the event loop running this
        let window = unsafe { &*(u64::from(window_id) as usize as *const NSWindow) };
        let target = Target::new(mtm);
        // SAFETY: Target has refresh:, which takes the link
        let link = unsafe { window.displayLinkWithTarget_selector(&target, sel!(refresh:)) };
        // Keep to the display's highest refresh rate, rather than the lower
        // ones a variable refresh rate display drops to with little motion
        link.setPreferredFrameRateRange(CAFrameRateRange::new(60.0, 120.0, 120.0));
        // SAFETY: the main run loop, in all its modes, so it goes on during
        // window resizes
        unsafe { link.addToRunLoop_forMode(&NSRunLoop::mainRunLoop(), NSRunLoopCommonModes) };
        self.link = Some(link);
    }

    /// Whether a redraw of the window can go ahead now: once per refresh of the
    /// display. If not, it is held back until the next refresh, which wakes the
    /// event loop.
    pub fn try_redraw(&mut self, window_id: winit::window::WindowId) -> bool {
        if self.link.is_none() {
            self.start(window_id);
            return true;
        }
        let refresh = REFRESHES.get();
        if refresh == self.last_redraw {
            return false;
        }
        self.last_redraw = refresh;
        true
    }
}

impl Drop for FramePacer {
    fn drop(&mut self) {
        if let Some(link) = self.link.take() {
            link.invalidate();
        }
    }
}
