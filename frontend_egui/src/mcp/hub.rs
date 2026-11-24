//! What the MCP server knows about the running emulator.
//!
//! The hub sits between the emulator thread and the GUI. When MCP is on, it
//! takes each frame and each event from the emulator, keeps what the tools
//! need (the last frame, the last status, serial output, the clipboard) and
//! passes everything on to the GUI unchanged. So the tools see the screen and
//! the status even while the window is hidden or does not redraw.

use crossbeam_channel::{Receiver, Sender};
use snow_core::bus::Address;
use snow_core::cpu_m68k::cpu::SystrapHistoryEntry;
use snow_core::emulator::MouseMode;
use snow_core::emulator::comm::{
    EmulatorCommand, EmulatorCommandSender, EmulatorEvent, EmulatorEventReceiver, EmulatorStatus,
};
use snow_core::mac::scc::SccCh;
use snow_core::renderer::DisplayBuffer;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Serial output kept per channel
const SERIAL_KEEP: usize = 256 * 1024;

/// Memory notifier ids from here on are the MCP tools' own; the GUI never
/// sees what they report
pub const NOTIFIER_BASE: u32 = 0x4D43_0000;

/// A copy of one frame, RGBA
#[derive(Clone)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub rgba: Arc<Vec<u8>>,
    /// Counts the frames the emulator made since it started
    pub seq: u64,
}

#[derive(Default)]
struct State {
    /// Bumped at every attach, so the pumps of an old emulator stop
    generation: u64,
    cmd: Option<EmulatorCommandSender>,
    status: Option<Box<EmulatorStatus>>,
    frame: Option<Frame>,
    mouse_mode: MouseMode,
    /// Where the tools last put the pointer (guest pixels)
    pointer: Option<(u16, u16)>,
    /// The answer to the last ReadScrapText
    scrap: Option<Option<String>>,
    serial: [VecDeque<u8>; 2],
    /// Total bytes ever received per channel, to find new output
    serial_total: [u64; 2],
    /// What the MCP memory notifiers reported
    memory: HashMap<u32, (Address, Vec<Option<u8>>)>,
    /// The last system trap history the emulator sent
    systraps: Vec<SystrapHistoryEntry>,
    /// The emulator thread has ended
    ended: bool,
}

pub struct Hub {
    state: Mutex<State>,
    changed: Condvar,
    stop_pumps: Mutex<Arc<AtomicBool>>,
    started: Instant,
    pub calls: AtomicU64,
}

pub fn serial_index(ch: SccCh) -> usize {
    match ch {
        SccCh::A => 0,
        SccCh::B => 1,
    }
}

impl Hub {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            stop_pumps: Mutex::new(Arc::new(AtomicBool::new(false))),
            started: Instant::now(),
            calls: AtomicU64::new(0),
        }
    }

    pub fn uptime(&self) -> Duration {
        self.started.elapsed()
    }

    /// Puts the hub between a new emulator and the GUI. Returns the frame slot
    /// and the event receiver the GUI uses instead of the emulator's own.
    pub fn attach(
        self: &Arc<Self>,
        cmd: EmulatorCommandSender,
        frames: Arc<Mutex<Option<DisplayBuffer>>>,
        events: EmulatorEventReceiver,
        mouse_mode: MouseMode,
    ) -> (Arc<Mutex<Option<DisplayBuffer>>>, EmulatorEventReceiver) {
        let stop = Arc::new(AtomicBool::new(false));
        {
            let mut old = self.stop_pumps.lock().unwrap();
            old.store(true, Ordering::Relaxed);
            *old = stop.clone();
        }
        let generation = {
            let mut s = self.state.lock().unwrap();
            s.generation += 1;
            s.cmd = Some(cmd);
            s.status = None;
            s.frame = None;
            s.mouse_mode = mouse_mode;
            s.pointer = None;
            s.scrap = None;
            s.memory.clear();
            s.systraps.clear();
            s.ended = false;
            s.generation
        };
        self.changed.notify_all();

        let ui_frames: Arc<Mutex<Option<DisplayBuffer>>> = Default::default();
        let (ev_send, ev_recv) = crossbeam_channel::unbounded();

        {
            let hub = Arc::clone(self);
            let ui_frames = Arc::clone(&ui_frames);
            thread::Builder::new()
                .name("mcp-frames".into())
                .spawn(move || hub.frame_pump(generation, &frames, &ui_frames, &stop))
                .expect("spawn mcp-frames");
        }
        {
            let hub = Arc::clone(self);
            thread::Builder::new()
                .name("mcp-events".into())
                .spawn(move || hub.event_pump(generation, &events, &ev_send))
                .expect("spawn mcp-events");
        }
        (ui_frames, ev_recv)
    }

    /// The GUI stopped the emulator
    pub fn detach(&self) {
        self.stop_pumps
            .lock()
            .unwrap()
            .store(true, Ordering::Relaxed);
        let mut s = self.state.lock().unwrap();
        s.cmd = None;
        s.ended = true;
        drop(s);
        self.changed.notify_all();
    }

    fn frame_pump(
        &self,
        generation: u64,
        frames: &Mutex<Option<DisplayBuffer>>,
        ui_frames: &Mutex<Option<DisplayBuffer>>,
        stop: &AtomicBool,
    ) {
        let mut seq = 0;
        while !stop.load(Ordering::Relaxed) {
            let Some(frame) = frames.lock().unwrap().take() else {
                thread::sleep(Duration::from_millis(3));
                continue;
            };
            seq += 1;
            let copy = Frame {
                width: usize::from(frame.width()),
                height: usize::from(frame.height()),
                rgba: Arc::new(frame.to_vec()),
                seq,
            };
            // The GUI takes the newest frame; one it did not take yet is dropped
            *ui_frames.lock().unwrap() = Some(frame);
            let mut s = self.state.lock().unwrap();
            if s.generation != generation {
                return;
            }
            s.frame = Some(copy);
            drop(s);
            self.changed.notify_all();
        }
    }

    fn event_pump(
        &self,
        generation: u64,
        events: &Receiver<EmulatorEvent>,
        ui: &Sender<EmulatorEvent>,
    ) {
        for event in events {
            // Memory read for the tools: keep it, don't pass it on
            if let EmulatorEvent::MemoryNotify { id, addr, data } = &event
                && *id >= NOTIFIER_BASE
            {
                let mut s = self.state.lock().unwrap();
                if s.generation == generation {
                    s.memory.insert(*id, (*addr, data.clone()));
                }
                drop(s);
                self.changed.notify_all();
                continue;
            }
            {
                let mut s = self.state.lock().unwrap();
                if s.generation == generation {
                    match &event {
                        EmulatorEvent::Status(st) => s.status = Some(Box::new(clone_status(st))),
                        EmulatorEvent::ScrapText(t) => s.scrap = Some(t.clone()),
                        EmulatorEvent::SystrapHistory(h) => s.systraps.clone_from(h),
                        EmulatorEvent::SccTransmitData(ch, data) => {
                            let i = serial_index(*ch);
                            s.serial[i].extend(data.iter());
                            s.serial_total[i] += data.len() as u64;
                            let excess = s.serial[i].len().saturating_sub(SERIAL_KEEP);
                            s.serial[i].drain(..excess);
                        }
                        _ => (),
                    }
                }
            }
            self.changed.notify_all();
            if ui.send(event).is_err() {
                break;
            }
        }
        let mut s = self.state.lock().unwrap();
        if s.generation == generation {
            s.ended = true;
            s.cmd = None;
        }
        drop(s);
        self.changed.notify_all();
    }

    pub fn set_mouse_mode(&self, mode: MouseMode) {
        self.state.lock().unwrap().mouse_mode = mode;
    }

    pub fn mouse_mode(&self) -> MouseMode {
        self.state.lock().unwrap().mouse_mode
    }

    pub fn set_pointer(&self, p: (u16, u16)) {
        self.state.lock().unwrap().pointer = Some(p);
    }

    pub fn pointer(&self) -> Option<(u16, u16)> {
        self.state.lock().unwrap().pointer
    }

    pub fn is_attached(&self) -> bool {
        self.state.lock().unwrap().cmd.is_some()
    }

    /// Sends a command to the emulator
    pub fn send(&self, cmd: EmulatorCommand) -> Result<(), String> {
        let sender = self.state.lock().unwrap().cmd.clone();
        let Some(sender) = sender else {
            return Err("No machine is running: load a ROM or a workspace in Snow first".into());
        };
        sender
            .send(cmd)
            .map_err(|_| "The emulator has stopped".to_string())
    }

    pub fn frame(&self) -> Option<Frame> {
        self.state.lock().unwrap().frame.clone()
    }

    pub fn with_status<R>(&self, f: impl FnOnce(Option<&EmulatorStatus>) -> R) -> R {
        let s = self.state.lock().unwrap();
        f(s.status.as_deref())
    }

    /// Waits until the emulator has made `n` more frames, or for `timeout`.
    /// Returns false if it timed out (for example while the machine is paused).
    pub fn wait_frames(&self, n: u64, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut s = self.state.lock().unwrap();
        let start = s.frame.as_ref().map_or(0, |f| f.seq);
        let generation = s.generation;
        loop {
            let now_seq = s.frame.as_ref().map_or(0, |f| f.seq);
            if s.generation != generation || now_seq >= start + n {
                return true;
            }
            let now = Instant::now();
            if now >= deadline || s.ended {
                return false;
            }
            s = self.changed.wait_timeout(s, deadline - now).unwrap().0;
        }
    }

    /// Waits for anything to change (a frame, an event), or for `timeout`
    pub fn wait_change(&self, timeout: Duration) {
        let s = self.state.lock().unwrap();
        let _ = self.changed.wait_timeout(s, timeout).unwrap();
    }

    pub fn take_scrap(&self) -> Option<Option<String>> {
        self.state.lock().unwrap().scrap.take()
    }

    pub fn clear_scrap(&self) {
        self.state.lock().unwrap().scrap = None;
    }

    /// Serial output from `from` (a total byte count) on: the bytes still kept
    /// and the new total
    pub fn serial_since(&self, ch: SccCh, from: u64) -> (Vec<u8>, u64) {
        let s = self.state.lock().unwrap();
        let i = serial_index(ch);
        let total = s.serial_total[i];
        let kept = s.serial[i].len() as u64;
        let first_kept = total - kept;
        let skip = from.saturating_sub(first_kept).min(kept) as usize;
        (s.serial[i].iter().skip(skip).copied().collect(), total)
    }

    pub fn serial_total(&self, ch: SccCh) -> u64 {
        self.state.lock().unwrap().serial_total[serial_index(ch)]
    }

    pub fn take_memory(&self, id: u32) -> Option<(Address, Vec<Option<u8>>)> {
        self.state.lock().unwrap().memory.remove(&id)
    }

    pub fn systraps(&self) -> Vec<SystrapHistoryEntry> {
        self.state.lock().unwrap().systraps.clone()
    }

    pub fn has_ended(&self) -> bool {
        self.state.lock().unwrap().ended
    }
}

/// EmulatorStatus is not Clone; copy what the tools use
fn clone_status(st: &EmulatorStatus) -> EmulatorStatus {
    EmulatorStatus {
        regs: st.regs.clone(),
        running: st.running,
        breakpoints: st.breakpoints.clone(),
        cycles: st.cycles,
        fdd: st.fdd.clone(),
        model: st.model,
        has_pmmu: st.has_pmmu,
        speed: st.speed,
        ram_size: st.ram_size,
        effective_speed: st.effective_speed,
        scsi: st.scsi.clone(),
    }
}
