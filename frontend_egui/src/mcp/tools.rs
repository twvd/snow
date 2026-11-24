//! The MCP tools that run inside Snow

use super::hub::{Frame, Hub};
use super::image::{self, Rect};
use super::keys;
use serde_json::{Value, json};
use snow_core::emulator::MouseMode;
use snow_core::emulator::comm::{EmulatorCommand, EmulatorSpeed};
use snow_core::keymap::{KeyEvent, Keymap};
use snow_core::mac::scc::SccCh;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// The longest a single wait may take
const MAX_WAIT_MS: u64 = 25 * 60 * 1000;

/// What a tool call returns
#[derive(Default)]
pub struct Out {
    pub text: Vec<String>,
    pub image: Option<Vec<u8>>,
    pub error: bool,
    /// A wait in this call was not met
    pub unmet: bool,
    /// Snow should end after the answer went out
    pub exit: bool,
}

impl Out {
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            text: vec![s.into()],
            ..Default::default()
        }
    }

    fn err(s: impl Into<String>) -> Self {
        Self {
            text: vec![s.into()],
            error: true,
            ..Default::default()
        }
    }

    /// The MCP CallToolResult
    pub fn to_json(&self) -> Value {
        let mut content: Vec<Value> = self
            .text
            .iter()
            .map(|t| json!({"type": "text", "text": t}))
            .collect();
        if let Some(png) = &self.image {
            content.push(
                json!({"type": "image", "mimeType": "image/png", "data": image::base64(png)}),
            );
        }
        if content.is_empty() {
            content.push(json!({"type": "text", "text": "OK"}));
        }
        json!({"content": content, "isError": self.error})
    }
}

pub type R<T> = Result<T, String>;

pub fn int(args: &Value, key: &str) -> R<Option<i64>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_i64()
            .or_else(|| v.as_f64().map(|f| f.round() as i64))
            .map(Some)
            .ok_or_else(|| format!("'{}' must be a number", key)),
    }
}

fn need_int(args: &Value, key: &str) -> R<i64> {
    int(args, key)?.ok_or_else(|| format!("'{}' is missing", key))
}

pub fn num(args: &Value, key: &str) -> R<Option<f64>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .map(Some)
            .ok_or_else(|| format!("'{}' must be a number", key)),
    }
}

pub fn string<'a>(args: &'a Value, key: &str) -> R<Option<&'a str>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_str()
            .map(Some)
            .ok_or_else(|| format!("'{}' must be a string", key)),
    }
}

pub fn boolean(args: &Value, key: &str) -> R<Option<bool>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_bool()
            .map(Some)
            .ok_or_else(|| format!("'{}' must be true or false", key)),
    }
}

fn rect(v: &Value) -> R<Rect> {
    let get = |k: &str| -> R<usize> {
        let n = need_int(v, k)?;
        usize::try_from(n).map_err(|_| format!("'{}' must not be negative", k))
    };
    Ok(Rect {
        x: get("x")?,
        y: get("y")?,
        w: get("w")?,
        h: get("h")?,
    })
}

fn opt_rect(args: &Value, key: &str) -> R<Option<Rect>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => rect(v).map(Some),
    }
}

/// Runs a tool
pub fn call(hub: &Arc<Hub>, name: &str, args: &Value) -> Out {
    hub.calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let args = if args.is_null() { &json!({}) } else { args };
    let r = match name {
        "vm-info" => vm_info(hub),
        "vm-power" => vm_power(hub, args),
        "vm-speed" => vm_speed(hub, args),
        "screenshot" => screenshot(hub, args),
        "wait-screen" => wait_screen_tool(hub, args),
        "key" => key(hub, args),
        "type-text" => type_text(hub, args),
        "mouse-move" => mouse_move(hub, args),
        "mouse-click" => mouse_click(hub, args),
        "mouse-drag" => mouse_drag(hub, args),
        "mouse-button" => mouse_button(hub, args),
        "media" => media(hub, args),
        "clipboard-read" => clipboard_read(hub),
        "serial-write" => serial_write(hub, args),
        "serial-read" => serial_read(hub, args),
        "serial-expect" => serial_expect(hub, args),
        "steps" => steps(hub, args),
        "debug-registers" => super::debug::registers(hub),
        "debug-step" => super::debug::step(hub, args),
        "debug-run" => super::debug::run(hub, args),
        "debug-break" => super::debug::breaks(hub, args),
        "debug-traps" => super::debug::traps(hub, args),
        "memory-read" => super::debug::memory_read(hub, args),
        "memory-write" => super::debug::memory_write(hub, args),
        "disassemble" => super::debug::disassemble(hub, args),
        _ => Err(format!("Unknown tool '{}'", name)),
    };
    r.unwrap_or_else(Out::err)
}

// ---------------------------------------------------------------- machine

fn vm_info(hub: &Arc<Hub>) -> R<Out> {
    let frame = hub.frame();
    let mut info = json!({
        "name": super::vm_name(),
        "pid": std::process::id(),
        "snow": snow_core::build_version(),
        "machine-loaded": hub.is_attached(),
        "mouse-mode": format!("{}", hub.mouse_mode()),
        "pointer": hub.pointer().map(|(x, y)| json!({"x": x, "y": y})),
        "screen": frame.as_ref().map(|f| json!({"width": f.width, "height": f.height})),
        "frames": frame.as_ref().map_or(0, |f| f.seq),
        "uptime-s": hub.uptime().as_secs(),
    });
    hub.with_status(|st| {
        let Some(st) = st else {
            return;
        };
        info["model"] = json!(format!("{}", st.model));
        info["running"] = json!(st.running);
        info["speed"] = json!(format!("{:?}", st.speed));
        info["effective-speed"] = json!((st.effective_speed * 100.0).round() / 100.0);
        info["ram-bytes"] = json!(st.ram_size);
        info["pmmu"] = json!(st.has_pmmu);
        info["cycles"] = json!(st.cycles);
        info["pc"] = json!(format!("{:08X}", st.regs.pc));
        info["floppy"] = st
            .fdd
            .iter()
            .enumerate()
            .filter(|(_, f)| f.present)
            .map(|(i, f)| {
                json!({
                    "drive": i,
                    "type": format!("{:?}", f.drive_type),
                    "inserted": !f.ejected,
                    "image": (!f.ejected).then(|| f.image_title.clone()),
                    "changed": f.dirty,
                    "writeback": f.writeback_enabled,
                })
            })
            .collect();
        info["scsi"] = st
            .scsi
            .iter()
            .enumerate()
            .filter_map(|(id, t)| {
                let t = t.as_ref()?;
                let mut v = json!({
                    "id": id,
                    "type": format!("{:?}", t.target_type),
                    "image": t.image.as_ref().map(|p| p.display().to_string()),
                    "capacity-bytes": t.capacity,
                });
                if let Some(folder) = &t.cdrom_folder {
                    v["folder"] = json!(folder.display().to_string());
                }
                #[cfg(feature = "ethernet")]
                if let Some(link) = &t.link_type {
                    v["link"] = json!(format!("{:?}", link));
                }
                Some(v)
            })
            .collect();
    });
    Ok(Out::text(
        serde_json::to_string_pretty(&info).unwrap_or_default(),
    ))
}

fn vm_power(hub: &Arc<Hub>, args: &Value) -> R<Out> {
    match string(args, "action")?.unwrap_or("") {
        "pause" => {
            hub.send(EmulatorCommand::Stop)?;
            Ok(Out::text("Paused"))
        }
        "resume" => {
            hub.send(EmulatorCommand::Run)?;
            Ok(Out::text("Running"))
        }
        "reset" => {
            hub.send(EmulatorCommand::Reset)?;
            Ok(Out::text("Reset"))
        }
        "quit" => {
            if hub.send(EmulatorCommand::Quit).is_ok() {
                // The emulator writes back floppies and ends its thread
                let deadline = Instant::now() + Duration::from_secs(15);
                while !hub.has_ended() && Instant::now() < deadline {
                    hub.wait_change(Duration::from_millis(100));
                }
            }
            Ok(Out {
                text: vec!["Snow is quitting".into()],
                exit: true,
                ..Default::default()
            })
        }
        a => Err(format!(
            "Unknown action '{}': pause, resume, reset or quit",
            a
        )),
    }
}

fn vm_speed(hub: &Arc<Hub>, args: &Value) -> R<Out> {
    let speed = match string(args, "speed")?.unwrap_or("") {
        "accurate" => EmulatorSpeed::Accurate,
        "dynamic" => EmulatorSpeed::Dynamic,
        "uncapped" => EmulatorSpeed::Uncapped,
        "video" => EmulatorSpeed::Video,
        "fastforward" => EmulatorSpeed::FastForward(num(args, "factor")?.unwrap_or(4.0).max(1.0)),
        s => {
            return Err(format!(
                "Unknown speed '{}': accurate, dynamic, uncapped, video or fastforward",
                s
            ));
        }
    };
    hub.send(EmulatorCommand::SetSpeed(speed))?;
    Ok(Out::text(format!("Speed: {:?}", speed)))
}

// ---------------------------------------------------------------- screen

/// The newest frame, waiting a little for the first one
fn current_frame(hub: &Hub) -> R<Frame> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(f) = hub.frame() {
            return Ok(f);
        }
        if !hub.is_attached() {
            return Err("No machine is running: load a ROM or a workspace in Snow first".into());
        }
        if Instant::now() >= deadline {
            return Err("The emulator has not drawn a frame yet".into());
        }
        hub.wait_change(Duration::from_millis(50));
    }
}

fn shot(
    hub: &Hub,
    region: Option<Rect>,
    scale: Option<f64>,
    pointer: bool,
) -> R<(Vec<u8>, String)> {
    let f = current_frame(hub)?;
    let r = match region {
        Some(r) => r
            .clip(&f)
            .ok_or_else(|| format!("The region is outside the {}x{} screen", f.width, f.height))?,
        None => Rect::whole(&f),
    };
    let mut img = image::crop(&f, r);
    if pointer && let Some((x, y)) = hub.pointer() {
        image::mark(
            &mut img,
            x as isize - r.x as isize,
            y as isize - r.y as isize,
        );
    }
    let factor = scale.unwrap_or(1.0).clamp(0.1, 8.0);
    let img = image::scale(img, factor);
    let note = format!(
        "Screen {}x{}{}{}",
        f.width,
        f.height,
        if region.is_some() {
            format!(", region x={} y={} w={} h={}", r.x, r.y, r.w, r.h)
        } else {
            String::new()
        },
        if (factor - 1.0).abs() > 1e-6 {
            format!(", shown at {}x", factor)
        } else {
            String::new()
        }
    );
    Ok((image::png(&img), note))
}

fn screenshot(hub: &Arc<Hub>, args: &Value) -> R<Out> {
    let (png, note) = shot(
        hub,
        opt_rect(args, "region")?,
        num(args, "scale")?,
        boolean(args, "pointer")?.unwrap_or(false),
    )?;
    Ok(Out {
        text: vec![note],
        image: Some(png),
        ..Default::default()
    })
}

#[derive(Clone, Copy, PartialEq)]
enum Until {
    Change,
    Stable,
    ChangeThenStable,
}

struct WaitSpec {
    until: Until,
    stable: Duration,
    timeout: Duration,
    region: Option<Rect>,
    ignore: Vec<Rect>,
    min_pixels: usize,
}

impl WaitSpec {
    /// Reads a wait from tool arguments; None for until = none
    fn parse(args: &Value) -> R<Option<Self>> {
        let until = match string(args, "until")?.unwrap_or("change-then-stable") {
            "change" => Until::Change,
            "stable" => Until::Stable,
            "change-then-stable" => Until::ChangeThenStable,
            "none" => return Ok(None),
            u => {
                return Err(format!(
                    "Unknown until '{}': change, stable, change-then-stable or none",
                    u
                ));
            }
        };
        let ms = |k: &str, d: u64| -> R<Duration> {
            Ok(Duration::from_millis(
                int(args, k)?
                    .map_or(d, |v| v.max(0) as u64)
                    .min(MAX_WAIT_MS),
            ))
        };
        let ignore = match args.get("ignore") {
            None | Some(Value::Null) => vec![],
            Some(Value::Array(a)) => a.iter().map(rect).collect::<R<_>>()?,
            Some(_) => return Err("'ignore' must be a list of rectangles".into()),
        };
        Ok(Some(Self {
            until,
            stable: ms("stable-ms", 1000)?,
            timeout: ms("timeout-ms", 30000)?,
            region: opt_rect(args, "region")?,
            ignore,
            min_pixels: int(args, "min-pixels")?.map_or(32, |v| v.max(0) as usize),
        }))
    }

    /// Waits; returns whether the wait was met and what happened
    fn run(&self, hub: &Hub) -> R<(bool, String)> {
        let start = Instant::now();
        let deadline = start + self.timeout;
        let mut reference = current_frame(hub)?;
        let area = match self.region {
            Some(r) => r
                .clip(&reference)
                .ok_or("The region is outside the screen")?,
            None => Rect::whole(&reference),
        };
        let mut changed = false;
        let mut last_change = start;
        loop {
            let now = Instant::now();
            let stable_for = now - last_change;
            let met = match self.until {
                Until::Change => changed,
                Until::Stable => stable_for >= self.stable,
                Until::ChangeThenStable => changed && stable_for >= self.stable,
            };
            if met {
                let what = match self.until {
                    Until::Change => "The screen changed".to_string(),
                    _ => format!(
                        "The screen {}settled for {} ms",
                        if changed { "changed and " } else { "" },
                        self.stable.as_millis()
                    ),
                };
                return Ok((
                    true,
                    format!("{} after {} ms", what, start.elapsed().as_millis()),
                ));
            }
            if now >= deadline {
                let what = match (self.until, changed) {
                    (Until::Change, _) | (Until::ChangeThenStable, false) => {
                        "the screen did not change"
                    }
                    _ => "the screen did not settle",
                };
                return Ok((
                    false,
                    format!("Not met: {} in {} ms", what, self.timeout.as_millis()),
                ));
            }
            hub.wait_change((deadline - now).min(Duration::from_millis(50)));
            let Some(f) = hub.frame() else {
                continue;
            };
            if f.seq == reference.seq {
                continue;
            }
            let d = image::diff(&reference, &f, area, &self.ignore);
            if d > self.min_pixels {
                changed = true;
                last_change = Instant::now();
                reference = f;
            }
        }
    }
}

fn wait_screen_tool(hub: &Arc<Hub>, args: &Value) -> R<Out> {
    let Some(spec) = WaitSpec::parse(args)? else {
        return Ok(Out::text("Nothing to wait for"));
    };
    let (met, text) = spec.run(hub)?;
    let mut out = Out {
        text: vec![text],
        unmet: !met,
        ..Default::default()
    };
    if boolean(args, "screenshot")?.unwrap_or(true) {
        let (png, _) = shot(hub, None, num(args, "scale")?, false)?;
        out.image = Some(png);
    }
    Ok(out)
}

/// Runs an input tool's "then": a wait and a screenshot
fn then(hub: &Hub, args: &Value, mut out: Out) -> R<Out> {
    let Some(t) = args.get("then").filter(|t| !t.is_null()) else {
        return Ok(out);
    };
    if let Some(spec) = WaitSpec::parse(t)? {
        let (met, text) = spec.run(hub)?;
        out.text.push(text);
        out.unmet |= !met;
    }
    if boolean(t, "screenshot")?.unwrap_or(true) {
        let (png, _) = shot(hub, opt_rect(t, "region")?, num(t, "scale")?, false)?;
        out.image = Some(png);
    }
    Ok(out)
}

// ---------------------------------------------------------------- keyboard

/// Gives the guest time to see an input: two frames, or a little while if
/// the machine is paused
fn settle(hub: &Hub, frames: u64) {
    hub.wait_frames(frames, Duration::from_millis(250));
}

fn press(hub: &Hub, combo: &[u8], hold: Duration) -> R<()> {
    for &sc in combo {
        hub.send(EmulatorCommand::KeyEvent(KeyEvent::KeyDown(
            sc,
            Keymap::Universal,
        )))?;
    }
    thread::sleep(hold);
    for &sc in combo.iter().rev() {
        hub.send(EmulatorCommand::KeyEvent(KeyEvent::KeyUp(
            sc,
            Keymap::Universal,
        )))?;
    }
    Ok(())
}

fn key(hub: &Arc<Hub>, args: &Value) -> R<Out> {
    let list: Vec<String> = match args.get("keys") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or("'keys' must hold strings")
            })
            .collect::<Result<_, _>>()?,
        _ => return Err("'keys' is missing".into()),
    };
    // Check them all before pressing any
    let combos = list.iter().map(|k| keys::combo(k)).collect::<R<Vec<_>>>()?;
    let hold =
        Duration::from_millis(int(args, "hold-ms")?.map_or(60, |v| v.clamp(0, 10000) as u64));
    for c in &combos {
        press(hub, c, hold)?;
        settle(hub, 2);
    }
    then(hub, args, Out::text(format!("Pressed {}", list.join(", "))))
}

fn type_text(hub: &Arc<Hub>, args: &Value) -> R<Out> {
    let text = string(args, "text")?.ok_or("'text' is missing")?;
    let delay =
        Duration::from_millis(int(args, "delay-ms")?.map_or(30, |v| v.clamp(0, 5000) as u64));
    let codes = text
        .chars()
        .map(|c| {
            keys::by_char(c).ok_or_else(|| {
                format!(
                    "Can't type {:?}: it is not on the US layout (nothing was typed)",
                    c
                )
            })
        })
        .collect::<R<Vec<_>>>()?;
    for (sc, shift) in codes {
        let combo: &[u8] = if shift { &[keys::SHIFT, sc] } else { &[sc] };
        press(hub, combo, delay)?;
        thread::sleep(delay);
    }
    settle(hub, 2);
    then(
        hub,
        args,
        Out::text(format!("Typed {} characters", text.chars().count())),
    )
}

// ---------------------------------------------------------------- mouse

fn point(hub: &Hub, args: &Value, kx: &str, ky: &str) -> R<Option<(u16, u16)>> {
    let (x, y) = match (int(args, kx)?, int(args, ky)?) {
        (None, None) => return Ok(None),
        (Some(x), Some(y)) => (x, y),
        _ => return Err(format!("Give both '{}' and '{}'", kx, ky)),
    };
    let f = current_frame(hub)?;
    if x < 0 || y < 0 || x as usize >= f.width || y as usize >= f.height {
        return Err(format!(
            "({}, {}) is outside the {}x{} screen",
            x, y, f.width, f.height
        ));
    }
    Ok(Some((x as u16, y as u16)))
}

fn move_to(hub: &Hub, (x, y): (u16, u16)) -> R<()> {
    match hub.mouse_mode() {
        MouseMode::Absolute => (),
        m => {
            return Err(format!(
                "The mouse mode is {}: the mouse tools need Absolute (Options menu in Snow)",
                m
            ));
        }
    }
    hub.send(EmulatorCommand::MouseUpdateAbsolute { x, y })?;
    hub.set_pointer((x, y));
    settle(hub, 2);
    Ok(())
}

fn button(hub: &Hub, down: bool) -> R<()> {
    hub.send(EmulatorCommand::MouseUpdateRelative {
        relx: 0,
        rely: 0,
        btn: Some(down),
    })?;
    settle(hub, 2);
    Ok(())
}

fn mouse_move(hub: &Arc<Hub>, args: &Value) -> R<Out> {
    let p = point(hub, args, "x", "y")?.ok_or("'x' and 'y' are missing")?;
    move_to(hub, p)?;
    then(
        hub,
        args,
        Out::text(format!("Pointer at ({}, {})", p.0, p.1)),
    )
}

fn mouse_click(hub: &Arc<Hub>, args: &Value) -> R<Out> {
    if let Some(p) = point(hub, args, "x", "y")? {
        move_to(hub, p)?;
    }
    let hold =
        Duration::from_millis(int(args, "hold-ms")?.map_or(100, |v| v.clamp(0, 10000) as u64));
    let double = boolean(args, "double")?.unwrap_or(false);
    for _ in 0..if double { 2 } else { 1 } {
        button(hub, true)?;
        thread::sleep(if double {
            hold.min(Duration::from_millis(60))
        } else {
            hold
        });
        button(hub, false)?;
    }
    let at = hub
        .pointer()
        .map_or(String::new(), |(x, y)| format!(" at ({}, {})", x, y));
    then(
        hub,
        args,
        Out::text(format!(
            "{}{}",
            if double { "Double-clicked" } else { "Clicked" },
            at
        )),
    )
}

fn mouse_drag(hub: &Arc<Hub>, args: &Value) -> R<Out> {
    let from = point(hub, args, "from-x", "from-y")?.ok_or("'from-x' and 'from-y' are missing")?;
    let to = point(hub, args, "to-x", "to-y")?.ok_or("'to-x' and 'to-y' are missing")?;
    let steps = int(args, "steps")?.map_or(12, |v| v.clamp(1, 500));
    let hold =
        Duration::from_millis(int(args, "hold-ms")?.map_or(150, |v| v.clamp(0, 10000) as u64));
    move_to(hub, from)?;
    button(hub, true)?;
    thread::sleep(hold);
    for i in 1..=steps {
        let lerp = |a: u16, b: u16| {
            (f64::from(a) + (f64::from(b) - f64::from(a)) * i as f64 / steps as f64).round() as u16
        };
        let p = (lerp(from.0, to.0), lerp(from.1, to.1));
        hub.send(EmulatorCommand::MouseUpdateAbsolute { x: p.0, y: p.1 })?;
        hub.set_pointer(p);
        settle(hub, 1);
    }
    settle(hub, 2);
    button(hub, false)?;
    then(
        hub,
        args,
        Out::text(format!(
            "Dragged from ({}, {}) to ({}, {})",
            from.0, from.1, to.0, to.1
        )),
    )
}

fn mouse_button(hub: &Arc<Hub>, args: &Value) -> R<Out> {
    let down = match string(args, "state")?.unwrap_or("") {
        "down" => true,
        "up" => false,
        s => return Err(format!("Unknown state '{}': down or up", s)),
    };
    if let Some(p) = point(hub, args, "x", "y")? {
        if down {
            move_to(hub, p)?;
        } else {
            // Moving with the button down
            hub.send(EmulatorCommand::MouseUpdateAbsolute { x: p.0, y: p.1 })?;
            hub.set_pointer(p);
            settle(hub, 2);
        }
    }
    button(hub, down)?;
    then(
        hub,
        args,
        Out::text(format!("Button {}", if down { "down" } else { "up" })),
    )
}

// ---------------------------------------------------------------- media

fn host_path(p: &str) -> R<PathBuf> {
    let path = PathBuf::from(p);
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    if !path.exists() {
        return Err(format!("{} does not exist", path.display()));
    }
    Ok(path)
}

fn media(hub: &Arc<Hub>, args: &Value) -> R<Out> {
    let action = string(args, "action")?.ok_or("'action' is missing")?;
    let path = || -> R<PathBuf> { host_path(string(args, "path")?.ok_or("'path' is missing")?) };
    let id = || -> R<usize> {
        let id = need_int(args, "id")?;
        if !(0..=6).contains(&id) {
            return Err("'id' must be a SCSI ID from 0 to 6".into());
        }
        Ok(id as usize)
    };
    let drive = || -> R<usize> {
        let d = int(args, "drive")?.unwrap_or(0);
        if !(0..=2).contains(&d) {
            return Err("'drive' must be 0, 1 or 2".into());
        }
        Ok(d as usize)
    };
    let text = match action {
        "insert-floppy" => {
            let p = path()?;
            let wp = boolean(args, "write-protect")?.unwrap_or(false);
            let d = drive()?;
            hub.send(EmulatorCommand::InsertFloppy(
                d,
                p.to_string_lossy().into_owned(),
                wp,
            ))?;
            format!("Inserted {} in floppy drive {}", p.display(), d)
        }
        "eject-floppy" => {
            let d = drive()?;
            hub.send(EmulatorCommand::EjectFloppy(d))?;
            format!("Ejected floppy drive {}", d)
        }
        "attach-disk" => {
            let (id, p) = (id()?, path()?);
            hub.send(EmulatorCommand::ScsiAttachHdd(id, p.clone()))?;
            format!(
                "Attached {} as SCSI ID {} (the Mac sees it after a restart)",
                p.display(),
                id
            )
        }
        "attach-cdrom" => {
            let id = id()?;
            let p = string(args, "path")?.map(host_path).transpose()?;
            hub.send(EmulatorCommand::ScsiAttachCdrom(id))?;
            if let Some(p) = &p {
                hub.send(EmulatorCommand::ScsiLoadMedia(id, p.clone()))?;
            }
            format!("Attached a CD-ROM drive as SCSI ID {}", id)
        }
        "load-cdrom" => {
            let (id, p) = (id()?, path()?);
            hub.send(EmulatorCommand::ScsiLoadMedia(id, p.clone()))?;
            format!("Loaded {} in SCSI ID {}", p.display(), id)
        }
        #[cfg(feature = "ethernet")]
        "attach-ethernet" => {
            let id = id()?;
            hub.send(EmulatorCommand::ScsiAttachEthernet(id))?;
            format!("Attached the Ethernet adapter as SCSI ID {}", id)
        }
        #[cfg(feature = "ethernet")]
        "ethernet-link" => {
            use snow_core::mac::scsi::ethernet::EthernetLinkType;
            let id = id()?;
            let link = match string(args, "link")?.unwrap_or("") {
                "down" => EthernetLinkType::Down,
                #[cfg(feature = "ethernet_nat")]
                "nat" => EthernetLinkType::NAT,
                #[cfg(feature = "ethernet_nat_https_stripping")]
                "nat-https" => EthernetLinkType::NATHttpsStripping,
                l => return Err(format!("Unknown or unsupported link '{}'", l)),
            };
            hub.send(EmulatorCommand::EthernetSetLink(id, link.clone()))?;
            format!("SCSI ID {} link: {:?}", id, link)
        }
        "detach" => {
            let id = id()?;
            hub.send(EmulatorCommand::DetachScsiTarget(id))?;
            format!("Detached SCSI ID {}", id)
        }
        a => return Err(format!("Unknown or unsupported action '{}'", a)),
    };
    Ok(Out::text(text))
}

// ---------------------------------------------------------------- clipboard and serial

fn clipboard_read(hub: &Arc<Hub>) -> R<Out> {
    hub.clear_scrap();
    hub.send(EmulatorCommand::ReadScrapText)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(text) = hub.take_scrap() {
            return Ok(Out::text(
                text.unwrap_or_else(|| "(the clipboard holds no text)".into()),
            ));
        }
        if Instant::now() >= deadline {
            return Err("The emulator did not answer (is the machine paused?)".into());
        }
        hub.wait_change(Duration::from_millis(50));
    }
}

fn port(args: &Value) -> R<SccCh> {
    match string(args, "port")?.unwrap_or("a") {
        "a" | "A" | "modem" => Ok(SccCh::A),
        "b" | "B" | "printer" => Ok(SccCh::B),
        p => Err(format!("Unknown port '{}': a (modem) or b (printer)", p)),
    }
}

/// Where serial-read last stopped, per port
static SERIAL_READ: Mutex<[u64; 2]> = Mutex::new([0; 2]);

fn serial_write(hub: &Arc<Hub>, args: &Value) -> R<Out> {
    let ch = port(args)?;
    let text = string(args, "text")?.ok_or("'text' is missing")?;
    hub.send(EmulatorCommand::SccReceiveData(
        ch,
        text.as_bytes().to_vec(),
    ))?;
    Ok(Out::text(format!("Sent {} bytes", text.len())))
}

fn serial_read(hub: &Arc<Hub>, args: &Value) -> R<Out> {
    let ch = port(args)?;
    let i = super::hub::serial_index(ch);
    let from = if boolean(args, "all")?.unwrap_or(false) {
        0
    } else {
        SERIAL_READ.lock().unwrap()[i]
    };
    let (data, total) = hub.serial_since(ch, from);
    SERIAL_READ.lock().unwrap()[i] = total;
    Ok(Out::text(String::from_utf8_lossy(&data).into_owned()))
}

fn serial_expect(hub: &Arc<Hub>, args: &Value) -> R<Out> {
    let ch = port(args)?;
    let i = super::hub::serial_index(ch);
    let want = string(args, "text")?.ok_or("'text' is missing")?;
    let timeout = Duration::from_millis(
        int(args, "timeout-ms")?
            .map_or(30000, |v| v.max(0) as u64)
            .min(MAX_WAIT_MS),
    );
    let deadline = Instant::now() + timeout;
    let from = SERIAL_READ.lock().unwrap()[i];
    loop {
        let (data, total) = hub.serial_since(ch, from);
        let text = String::from_utf8_lossy(&data);
        if let Some(pos) = text.find(want) {
            // Next reads start after the match
            let upto = text[..pos + want.len()].len() as u64;
            SERIAL_READ.lock().unwrap()[i] = (total - data.len() as u64) + upto;
            return Ok(Out::text(text[..pos + want.len()].to_string()));
        }
        if Instant::now() >= deadline {
            return Ok(Out {
                text: vec![format!(
                    "Not met: {:?} did not come in {} ms; got: {}",
                    want,
                    timeout.as_millis(),
                    text
                )],
                unmet: true,
                ..Default::default()
            });
        }
        hub.wait_change(Duration::from_millis(50));
        let _ = hub.serial_total(ch);
    }
}

// ---------------------------------------------------------------- steps

fn steps(hub: &Arc<Hub>, args: &Value) -> R<Out> {
    let Some(Value::Array(list)) = args.get("steps") else {
        return Err("'steps' must be a list".into());
    };
    let mut out = Out::default();
    for (n, step) in list.iter().enumerate() {
        let tool = step
            .get("tool")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("Step {} has no 'tool'", n + 1))?;
        if tool == "steps" || tool.starts_with("vm-list") || tool == "vm-wait" {
            return Err(format!("Step {}: '{}' can't run inside steps", n + 1, tool));
        }
        let r = call(hub, tool, step.get("args").unwrap_or(&Value::Null));
        out.text.extend(
            r.text
                .into_iter()
                .map(|t| format!("{}. {}: {}", n + 1, tool, t)),
        );
        if r.image.is_some() {
            out.image = r.image;
        }
        out.exit |= r.exit;
        if r.error || r.unmet {
            out.error = r.error;
            out.unmet = r.unmet;
            out.text
                .push(format!("Stopped at step {} of {}", n + 1, list.len()));
            break;
        }
    }
    Ok(out)
}
