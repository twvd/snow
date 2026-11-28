//! Host game controllers for the emulated Mac, separate from the Apple IIe
//! game port.
//!
//! [`GamepadHub`] is the only gilrs context. The IIe joystick reads a
//! [`PadSnapshot`] from it. [`map_gamepad`] turns two snapshots into pointer
//! and key edges. [`plan_gamepad_injection`] turns those edges into the mouse
//! and key events the emulator loop sends.

use eframe::egui;
use egui_winit::winit::keyboard::KeyCode;
use gilrs::{Axis, Button, EventType, GamepadId, Gilrs};
use serde::{Deserialize, Serialize};
use snow_core::emulator::MouseMode;

/// Buttons the mapping table can bind, in the order stored by [`GamepadConfig`].
const MAPPED_BUTTONS: [Button; 10] = [
    Button::South,
    Button::East,
    Button::West,
    Button::North,
    Button::LeftTrigger,
    Button::RightTrigger,
    Button::Select,
    Button::Start,
    Button::LeftThumb,
    Button::RightThumb,
];

/// Every named gilrs button except [`Button::Unknown`], which panics `is_pressed`.
const TRACKED_BUTTONS: [Button; 19] = [
    Button::South,
    Button::East,
    Button::North,
    Button::West,
    Button::C,
    Button::Z,
    Button::LeftTrigger,
    Button::LeftTrigger2,
    Button::RightTrigger,
    Button::RightTrigger2,
    Button::Select,
    Button::Start,
    Button::Mode,
    Button::LeftThumb,
    Button::RightThumb,
    Button::DPadUp,
    Button::DPadDown,
    Button::DPadLeft,
    Button::DPadRight,
];

/// A hat axis at least this far from centre counts as that direction.
const HAT_THRESHOLD: f32 = 0.5;

fn default_enabled() -> bool {
    true
}

fn default_speed() -> f32 {
    12.0
}

fn default_deadzone() -> f32 {
    0.10
}

fn default_buttons() -> [(Button, ButtonAction); 10] {
    MAPPED_BUTTONS.map(|button| {
        let action = if button == Button::South {
            ButtonAction::MouseButton
        } else {
            ButtonAction::None
        };
        (button, action)
    })
}

/// One sample of the active host gamepad.
///
/// Stick axes follow gilrs: up is positive, each axis is about -1 to 1.
/// D-pad axes are -1, 0, or 1, up positive.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PadSnapshot {
    pub connected: bool,
    pub name: String,
    pub left_x: f32,
    pub left_y: f32,
    pub dpad_x: i8,
    pub dpad_y: i8,
    /// Gilrs buttons that are down. [`Button::Unknown`] is never reported.
    pub buttons: Vec<Button>,
}

impl PadSnapshot {
    pub fn button_pressed(&self, button: Button) -> bool {
        self.buttons.contains(&button)
    }
}

/// What the left stick and d-pad do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum StickMode {
    /// Move the emulated pointer. Screen Y grows downward.
    #[default]
    Mouse,
    /// Digital arrow keys, on the edge of a direction zone.
    ArrowKeys,
}

/// What a controller button does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ButtonAction {
    #[default]
    None,
    /// The emulated mouse button.
    MouseButton,
    /// A host key, mapped further on by the emulator keymap.
    Key(KeyCode),
}

/// Host gamepad mapping. Unknown saved values must not reject [`crate::settings::AppSettings`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GamepadConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub stick_mode: StickMode,
    /// Pointer pixels per poll at full deflection. Arrow keys ignore this.
    #[serde(default = "default_speed")]
    pub speed: f32,
    /// Stick deflection below this is the centre. The d-pad is not scaled by it.
    #[serde(default = "default_deadzone")]
    pub deadzone: f32,
    /// `(button, action)` for the ten mappable buttons.
    #[serde(default = "default_buttons")]
    pub buttons: [(Button, ButtonAction); 10],
}

impl Default for GamepadConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            stick_mode: StickMode::default(),
            speed: default_speed(),
            deadzone: default_deadzone(),
            buttons: default_buttons(),
        }
    }
}

/// Edges and repeats produced by [`map_gamepad`] for one poll.
///
/// `mouse_dy` is positive down the screen. `mouse_button` and `keys` are
/// edges only: a hold sets nothing. `mouse_dx` and `mouse_dy` repeat while
/// the stick is deflected in [`StickMode::Mouse`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GamepadActions {
    pub mouse_dx: i16,
    pub mouse_dy: i16,
    pub mouse_button: Option<bool>,
    pub keys: Vec<(KeyCode, bool)>,
    pub disconnected: bool,
}

/// The single gilrs context. `None` if the library failed to start; polling
/// then reports a disconnected pad.
pub struct GamepadHub {
    gilrs: Option<Gilrs>,
    /// Last pad that sent an event, if it is still connected.
    active: Option<GamepadId>,
    snapshot: PadSnapshot,
    pressed_button: Option<Button>,
}

impl GamepadHub {
    pub fn new() -> Self {
        let gilrs = match Gilrs::new() {
            Ok(gilrs) => Some(gilrs),
            Err(e) => {
                log::error!("Cannot open game controllers: {}", e);
                None
            }
        };
        Self {
            gilrs,
            active: None,
            snapshot: PadSnapshot::default(),
            pressed_button: None,
        }
    }

    /// Drains gilrs events and refreshes the cached snapshot.
    pub fn poll(&mut self) -> &PadSnapshot {
        if let Some(gilrs) = self.gilrs.as_mut() {
            while let Some(ev) = gilrs.next_event() {
                self.active = Some(ev.id);
                if let EventType::ButtonPressed(button, _) = ev.event {
                    self.pressed_button = Some(button);
                }
            }
            let id = self
                .active
                .filter(|&id| gilrs.connected_gamepad(id).is_some())
                .or_else(|| gilrs.gamepads().next().map(|(id, _)| id));
            self.active = id;
            self.snapshot = id
                .and_then(|id| gilrs.connected_gamepad(id))
                .map(snapshot_of)
                .unwrap_or_default();
        } else {
            self.snapshot = PadSnapshot::default();
        }
        &self.snapshot
    }

    /// The controller button pressed since the last call, for the IIe mapping dialog.
    pub fn take_pressed_button(&mut self) -> Option<Button> {
        self.pressed_button.take()
    }
}

fn snapshot_of(pad: gilrs::Gamepad<'_>) -> PadSnapshot {
    let mut buttons = Vec::new();
    for button in TRACKED_BUTTONS {
        if pad.is_pressed(button) {
            buttons.push(button);
        }
    }
    PadSnapshot {
        connected: true,
        name: pad.name().to_string(),
        left_x: pad.value(Axis::LeftStickX),
        left_y: pad.value(Axis::LeftStickY),
        dpad_x: dpad_axis(
            buttons.contains(&Button::DPadLeft),
            buttons.contains(&Button::DPadRight),
            pad.value(Axis::DPadX),
        ),
        dpad_y: dpad_axis(
            buttons.contains(&Button::DPadDown),
            buttons.contains(&Button::DPadUp),
            pad.value(Axis::DPadY),
        ),
        buttons,
    }
}

/// Hat buttons win over the axis. Gilrs' d-pad Y is up-positive, like the stick.
fn dpad_axis(negative: bool, positive: bool, axis: f32) -> i8 {
    match (negative, positive) {
        (true, false) => -1,
        (false, true) => 1,
        (true, true) => 0,
        (false, false) => {
            if axis >= HAT_THRESHOLD {
                1
            } else if axis <= -HAT_THRESHOLD {
                -1
            } else {
                0
            }
        }
    }
}

/// Maps `prev` then `now` through `cfg`.
///
/// A d-pad axis that is not zero replaces the left stick on that axis only.
/// In [`StickMode::Mouse`] the delta is emitted on every call while deflected,
/// including when `prev` and `now` match. In [`StickMode::ArrowKeys`] a
/// direction is a key edge, not a repeat.
///
/// Two buttons bound to [`ButtonAction::MouseButton`] share one emulated
/// button: a press in the same poll wins over a release, and a release is
/// emitted only when no such binding remains held.
pub fn map_gamepad(prev: &PadSnapshot, now: &PadSnapshot, cfg: &GamepadConfig) -> GamepadActions {
    if !cfg.enabled || (!prev.connected && !now.connected) {
        return GamepadActions::default();
    }
    if !now.connected {
        return disconnect_actions(prev, cfg);
    }

    let mut actions = GamepadActions::default();
    match cfg.stick_mode {
        StickMode::Mouse => {
            let x = deflected(now.left_x, now.dpad_x, cfg.deadzone);
            let y = deflected(now.left_y, now.dpad_y, cfg.deadzone);
            actions.mouse_dx = scale_to_i16(x * cfg.speed);
            actions.mouse_dy = scale_to_i16(-y * cfg.speed);
        }
        StickMode::ArrowKeys => {
            let (prev_x, prev_y) = zones(prev, cfg);
            let (now_x, now_y) = zones(now, cfg);
            push_signed_edges(
                &mut actions.keys,
                prev_x,
                now_x,
                KeyCode::ArrowLeft,
                KeyCode::ArrowRight,
            );
            push_signed_edges(
                &mut actions.keys,
                prev_y,
                now_y,
                KeyCode::ArrowDown,
                KeyCode::ArrowUp,
            );
        }
    }
    actions.mouse_button = mouse_button_edge(prev, now, cfg);
    push_key_edges(&mut actions.keys, prev, now, cfg);
    actions
}

fn disconnect_actions(prev: &PadSnapshot, cfg: &GamepadConfig) -> GamepadActions {
    let released = PadSnapshot::default();
    let mut actions = GamepadActions {
        disconnected: true,
        mouse_button: mouse_button_edge(prev, &released, cfg),
        ..GamepadActions::default()
    };
    if cfg.stick_mode == StickMode::ArrowKeys {
        let (x, y) = zones(prev, cfg);
        push_signed_edges(
            &mut actions.keys,
            x,
            0,
            KeyCode::ArrowLeft,
            KeyCode::ArrowRight,
        );
        push_signed_edges(
            &mut actions.keys,
            y,
            0,
            KeyCode::ArrowDown,
            KeyCode::ArrowUp,
        );
    }
    push_key_edges(&mut actions.keys, prev, &released, cfg);
    actions
}

/// D-pad overrides the stick on this axis. A stick inside the deadzone is 0.
fn deflected(analog: f32, dpad: i8, deadzone: f32) -> f32 {
    if dpad != 0 {
        f32::from(dpad)
    } else if analog.abs() < deadzone {
        0.0
    } else {
        analog
    }
}

fn zones(pad: &PadSnapshot, cfg: &GamepadConfig) -> (i8, i8) {
    if !pad.connected {
        return (0, 0);
    }
    (
        sign(deflected(pad.left_x, pad.dpad_x, cfg.deadzone)),
        sign(deflected(pad.left_y, pad.dpad_y, cfg.deadzone)),
    )
}

fn sign(value: f32) -> i8 {
    if value > 0.0 {
        1
    } else if value < 0.0 {
        -1
    } else {
        0
    }
}

fn scale_to_i16(value: f32) -> i16 {
    let rounded = if value.is_finite() {
        value.round()
    } else {
        value
    };
    rounded.clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
}

fn mouse_button_edge(prev: &PadSnapshot, now: &PadSnapshot, cfg: &GamepadConfig) -> Option<bool> {
    let mut press = false;
    let mut release = false;
    let mut held = false;
    for &(button, action) in &cfg.buttons {
        if action != ButtonAction::MouseButton {
            continue;
        }
        let was = prev.button_pressed(button);
        let is = now.button_pressed(button);
        if is {
            held = true;
        }
        if is && !was {
            press = true;
        } else if was && !is {
            release = true;
        }
    }
    if press {
        Some(true)
    } else if release && !held {
        Some(false)
    } else {
        None
    }
}

fn push_key_edges(
    keys: &mut Vec<(KeyCode, bool)>,
    prev: &PadSnapshot,
    now: &PadSnapshot,
    cfg: &GamepadConfig,
) {
    for &(button, action) in &cfg.buttons {
        let ButtonAction::Key(keycode) = action else {
            continue;
        };
        let was = prev.button_pressed(button);
        let is = now.button_pressed(button);
        if is && !was {
            keys.push((keycode, true));
        } else if was && !is {
            keys.push((keycode, false));
        }
    }
}

fn push_signed_edges(
    keys: &mut Vec<(KeyCode, bool)>,
    prev: i8,
    now: i8,
    negative: KeyCode,
    positive: KeyCode,
) {
    if prev == -1 && now != -1 {
        keys.push((negative, false));
    } else if prev == 1 && now != 1 {
        keys.push((positive, false));
    }
    if now == -1 && prev != -1 {
        keys.push((negative, true));
    } else if now == 1 && prev != 1 {
        keys.push((positive, true));
    }
}

/// Guest pointer and the keys or mouse button this pad has pressed.
///
/// Only inputs this pad injected are recorded. Releasing them does not clear
/// the real keyboard.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GamepadPointer {
    /// Absolute cursor in guest pixels. `None` until the first absolute move.
    pub cursor: Option<egui::Pos2>,
    /// Host keys this pad has pressed and not yet released.
    pub held_keys: Vec<KeyCode>,
    /// This pad is holding the emulated mouse button.
    pub mouse_down: bool,
}

/// What [`plan_gamepad_injection`] may send this poll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GamepadInjectOpts {
    pub mouse_mode: MouseMode,
    /// Emulated display size in guest pixels, when it is known.
    pub display: Option<(u16, u16)>,
    /// Stick motion may move the pointer. Off while a modal has the UI.
    pub move_mouse: bool,
    /// Mapping capture, or egui owns the keyboard: do not press keys.
    pub suppress_keys: bool,
    /// Mapping capture owns the click: do not press the mouse button.
    pub suppress_mouse_button: bool,
    /// `GamepadConfig::enabled`. When false, release what the pad is holding.
    pub enabled: bool,
}

/// Events to forward to the emulator. `None` means send nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GamepadInjection {
    /// Absolute guest point. Relative mode leaves this `None`.
    pub mouse_abs: Option<egui::Pos2>,
    /// Relative motion. Absolute mode leaves this `None`.
    pub mouse_rel: Option<egui::Pos2>,
    pub mouse_button: Option<bool>,
    pub keys: Vec<(KeyCode, bool)>,
}

/// Decides which emulator events follow one [`map_gamepad`] result.
///
/// `MouseMode::Disabled` sends no mouse motion and no mouse button. Keys still
/// go through. A disconnect or a disabled pad releases only the keys and the
/// mouse button recorded in `pointer`.
#[must_use]
pub fn plan_gamepad_injection(
    pointer: &mut GamepadPointer,
    actions: &GamepadActions,
    opts: &GamepadInjectOpts,
) -> GamepadInjection {
    let mut injection = GamepadInjection::default();
    if !opts.enabled || actions.disconnected {
        release_pointer(pointer, opts.mouse_mode, &mut injection);
        return injection;
    }

    if opts.move_mouse && opts.mouse_mode != MouseMode::Disabled {
        apply_pointer_motion(pointer, actions, opts, &mut injection);
    }
    apply_mouse_button(pointer, actions, opts, &mut injection);
    if opts.suppress_keys {
        injection.keys.extend(release_keys(&mut pointer.held_keys));
    } else {
        apply_key_edges(&mut pointer.held_keys, &actions.keys, &mut injection.keys);
    }
    injection
}

fn release_pointer(
    pointer: &mut GamepadPointer,
    mode: MouseMode,
    injection: &mut GamepadInjection,
) {
    injection.keys.extend(release_keys(&mut pointer.held_keys));
    injection.mouse_button = take_mouse_release(pointer, mode);
}

fn apply_pointer_motion(
    pointer: &mut GamepadPointer,
    actions: &GamepadActions,
    opts: &GamepadInjectOpts,
    injection: &mut GamepadInjection,
) {
    if actions.mouse_dx == 0 && actions.mouse_dy == 0 {
        return;
    }
    match opts.mouse_mode {
        MouseMode::RelativeHw => {
            injection.mouse_rel = Some(egui::pos2(
                f32::from(actions.mouse_dx),
                f32::from(actions.mouse_dy),
            ));
        }
        MouseMode::Absolute => {
            injection.mouse_abs = Some(step_absolute_cursor(
                pointer,
                actions.mouse_dx,
                actions.mouse_dy,
                opts.display,
            ));
        }
        MouseMode::Disabled => {}
    }
}

fn apply_mouse_button(
    pointer: &mut GamepadPointer,
    actions: &GamepadActions,
    opts: &GamepadInjectOpts,
    injection: &mut GamepadInjection,
) {
    if opts.mouse_mode == MouseMode::Disabled {
        return;
    }
    if opts.suppress_mouse_button {
        injection.mouse_button = take_mouse_release(pointer, opts.mouse_mode);
        return;
    }
    if let Some(down) = actions.mouse_button {
        pointer.mouse_down = down;
        injection.mouse_button = Some(down);
    }
}

fn step_absolute_cursor(
    pointer: &mut GamepadPointer,
    dx: i16,
    dy: i16,
    display: Option<(u16, u16)>,
) -> egui::Pos2 {
    let mut pos = pointer.cursor.unwrap_or_else(|| initial_cursor(display));
    pos.x += f32::from(dx);
    pos.y += f32::from(dy);
    pos = clamp_cursor(pos, display);
    pointer.cursor = Some(pos);
    pos
}

/// Centre of the emulated display, or the origin when the size is unknown.
fn initial_cursor(display: Option<(u16, u16)>) -> egui::Pos2 {
    match display {
        Some((width, height)) if width > 0 && height > 0 => {
            egui::pos2(f32::from(width) / 2.0, f32::from(height) / 2.0)
        }
        _ => egui::Pos2::ZERO,
    }
}

fn clamp_cursor(mut pos: egui::Pos2, display: Option<(u16, u16)>) -> egui::Pos2 {
    let (max_x, max_y) = match display {
        Some((width, height)) if width > 0 && height > 0 => (
            f32::from(width.saturating_sub(1)),
            f32::from(height.saturating_sub(1)),
        ),
        _ => (f32::MAX, f32::MAX),
    };
    pos.x = pos.x.clamp(0.0, max_x);
    pos.y = pos.y.clamp(0.0, max_y);
    pos
}

fn take_mouse_release(pointer: &mut GamepadPointer, mode: MouseMode) -> Option<bool> {
    let down = std::mem::take(&mut pointer.mouse_down);
    if down && mode != MouseMode::Disabled {
        Some(false)
    } else {
        None
    }
}

fn release_keys(held: &mut Vec<KeyCode>) -> Vec<(KeyCode, bool)> {
    held.drain(..).map(|key| (key, false)).collect()
}

fn apply_key_edges(
    held: &mut Vec<KeyCode>,
    edges: &[(KeyCode, bool)],
    out: &mut Vec<(KeyCode, bool)>,
) {
    for &(key, down) in edges {
        if down && !held.contains(&key) {
            held.push(key);
            out.push((key, true));
        } else if !down && let Some(index) = held.iter().position(|held_key| *held_key == key) {
            held.remove(index);
            out.push((key, false));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn centred() -> PadSnapshot {
        PadSnapshot {
            connected: true,
            ..PadSnapshot::default()
        }
    }

    fn with_button(button: Button) -> PadSnapshot {
        let mut pad = centred();
        pad.buttons.push(button);
        pad
    }

    #[test]
    fn deadzone_suppresses_a_small_stick() {
        let mut now = centred();
        now.left_x = 0.09;
        now.left_y = -0.09;
        let actions = map_gamepad(&centred(), &now, &GamepadConfig::default());
        assert_eq!(actions.mouse_dx, 0);
        assert_eq!(actions.mouse_dy, 0);

        now.left_x = 0.10;
        let actions = map_gamepad(&centred(), &now, &GamepadConfig::default());
        assert_eq!(actions.mouse_dx, 1, "deadzone is exclusive");
    }

    #[test]
    fn mouse_full_right() {
        let mut cfg = GamepadConfig::default();
        cfg.speed = 12.0;
        cfg.stick_mode = StickMode::Mouse;
        let mut now = centred();
        now.left_x = 1.0;
        let actions = map_gamepad(&centred(), &now, &cfg);
        assert_eq!(actions.mouse_dx, 12);
        assert_eq!(actions.mouse_dy, 0);
        assert!(actions.keys.is_empty());
        // The cursor keeps moving while the stick stays deflected.
        let again = map_gamepad(&now, &now, &cfg);
        assert_eq!(again.mouse_dx, 12);
        assert_eq!(again.mouse_dy, 0);
    }

    #[test]
    fn mouse_full_up_is_negative() {
        let mut now = centred();
        now.left_y = 1.0;
        let actions = map_gamepad(&centred(), &now, &GamepadConfig::default());
        assert_eq!(actions.mouse_dx, 0);
        assert_eq!(actions.mouse_dy, -12);
    }

    #[test]
    fn dpad_overrides_stick_on_that_axis() {
        let mut now = centred();
        now.left_x = 1.0;
        now.left_y = 1.0;
        now.dpad_x = -1;
        let actions = map_gamepad(&centred(), &now, &GamepadConfig::default());
        assert_eq!(actions.mouse_dx, -12, "d-pad left beats the stick right");
        assert_eq!(
            actions.mouse_dy, -12,
            "the other axis still follows the stick"
        );
    }

    #[test]
    fn south_press_hold_release() {
        let cfg = GamepadConfig::default();
        let up = centred();
        let down = with_button(Button::South);

        let press = map_gamepad(&up, &down, &cfg);
        assert_eq!(press.mouse_button, Some(true));
        assert!(press.keys.is_empty());

        let hold = map_gamepad(&down, &down, &cfg);
        assert_eq!(hold.mouse_button, None);

        let release = map_gamepad(&down, &up, &cfg);
        assert_eq!(release.mouse_button, Some(false));
    }

    #[test]
    fn releasing_one_mouse_button_while_another_stays_down_does_not_release() {
        // Two bindings to MouseButton share one emulated button. Releasing
        // one while the other stays down emits nothing. A press and a release
        // in the same poll emit a press, so the button stays down while any
        // MouseButton mapping remains held.
        let mut cfg = GamepadConfig::default();
        cfg.buttons[1].1 = ButtonAction::MouseButton;

        let mut both = centred();
        both.buttons.extend([Button::South, Button::East]);
        let east = with_button(Button::East);
        let release_one = map_gamepad(&both, &east, &cfg);
        assert_eq!(release_one.mouse_button, None);

        let south = with_button(Button::South);
        let swapped = map_gamepad(&south, &east, &cfg);
        assert_eq!(swapped.mouse_button, Some(true));

        let cleared = centred();
        let release_last = map_gamepad(&east, &cleared, &cfg);
        assert_eq!(release_last.mouse_button, Some(false));
    }

    #[test]
    fn arrow_keys_emit_edges_not_mouse() {
        let cfg = GamepadConfig {
            stick_mode: StickMode::ArrowKeys,
            ..GamepadConfig::default()
        };
        let centre = centred();
        let mut right = centred();
        right.left_x = 1.0;

        let press = map_gamepad(&centre, &right, &cfg);
        assert_eq!(press.mouse_dx, 0);
        assert_eq!(press.mouse_dy, 0);
        assert_eq!(press.keys, vec![(KeyCode::ArrowRight, true)]);

        let hold = map_gamepad(&right, &right, &cfg);
        assert!(hold.keys.is_empty());
        assert_eq!(hold.mouse_dx, 0);

        let release = map_gamepad(&right, &centre, &cfg);
        assert_eq!(release.keys, vec![(KeyCode::ArrowRight, false)]);
        assert_eq!(release.mouse_dx, 0);

        let mut up = centred();
        up.left_y = 1.0;
        let up_edge = map_gamepad(&centre, &up, &cfg);
        assert_eq!(up_edge.keys, vec![(KeyCode::ArrowUp, true)]);
    }

    #[test]
    fn disabled_yields_empty() {
        let mut cfg = GamepadConfig::default();
        cfg.enabled = false;
        let mut now = with_button(Button::South);
        now.left_x = 1.0;
        assert_eq!(
            map_gamepad(&centred(), &now, &cfg),
            GamepadActions::default()
        );
    }

    #[test]
    fn disconnect_releases_held_input() {
        let mut prev = with_button(Button::South);
        prev.left_x = 1.0;
        let gone = PadSnapshot::default();

        let mouse = map_gamepad(&prev, &gone, &GamepadConfig::default());
        assert!(mouse.disconnected);
        assert_eq!(mouse.mouse_button, Some(false));
        assert_eq!(mouse.mouse_dx, 0);
        assert_eq!(mouse.mouse_dy, 0);
        assert!(mouse.keys.is_empty(), "mouse mode does not release arrows");

        let arrows = GamepadConfig {
            stick_mode: StickMode::ArrowKeys,
            ..GamepadConfig::default()
        };
        let keys = map_gamepad(&prev, &gone, &arrows);
        assert!(keys.disconnected);
        assert_eq!(keys.mouse_button, Some(false));
        assert_eq!(keys.mouse_dx, 0);
        assert_eq!(keys.keys, vec![(KeyCode::ArrowRight, false)]);

        assert_eq!(
            map_gamepad(&gone, &gone, &GamepadConfig::default()),
            GamepadActions::default(),
            "staying disconnected is empty"
        );
    }

    #[test]
    fn key_action_is_an_edge() {
        let mut cfg = GamepadConfig::default();
        cfg.buttons[1].1 = ButtonAction::Key(KeyCode::KeyZ);
        let up = centred();
        let down = with_button(Button::East);
        assert_eq!(
            map_gamepad(&up, &down, &cfg).keys,
            vec![(KeyCode::KeyZ, true)]
        );
        assert!(map_gamepad(&down, &down, &cfg).keys.is_empty());
        assert_eq!(
            map_gamepad(&down, &up, &cfg).keys,
            vec![(KeyCode::KeyZ, false)]
        );

        let ignored = with_button(Button::North);
        assert_eq!(
            map_gamepad(&up, &ignored, &GamepadConfig::default()),
            GamepadActions::default()
        );
    }

    #[test]
    fn gamepad_config_serde_round_trip() {
        let mut cfg = GamepadConfig::default();
        assert_eq!(cfg.buttons[0], (Button::South, ButtonAction::MouseButton));
        assert!(
            cfg.buttons[1..]
                .iter()
                .all(|(_, action)| *action == ButtonAction::None)
        );

        cfg.speed = 7.5;
        cfg.stick_mode = StickMode::ArrowKeys;
        cfg.deadzone = 0.25;
        cfg.buttons[1].1 = ButtonAction::Key(KeyCode::KeyQ);
        let json = serde_json::to_string(&cfg).unwrap();
        let back: GamepadConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);

        let partial: GamepadConfig = serde_json::from_str(r#"{"speed":3.0}"#).unwrap();
        assert!(partial.enabled);
        assert_eq!(partial.speed, 3.0);
        assert_eq!(partial.deadzone, default_deadzone());
        assert_eq!(partial.stick_mode, StickMode::Mouse);
        assert_eq!(partial.buttons, default_buttons());
    }

    #[test]
    fn settings_missing_gamepad_uses_default() {
        let settings: crate::settings::AppSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.gamepad, GamepadConfig::default());

        let broken: crate::settings::AppSettings =
            serde_json::from_str(r#"{"gamepad":{"stick_mode":"nope"}}"#).unwrap();
        assert_eq!(broken.gamepad, GamepadConfig::default());
    }

    fn live_opts(mode: MouseMode, display: Option<(u16, u16)>) -> GamepadInjectOpts {
        GamepadInjectOpts {
            mouse_mode: mode,
            display,
            move_mouse: true,
            suppress_keys: false,
            suppress_mouse_button: false,
            enabled: true,
        }
    }

    fn plan(
        pointer: &mut GamepadPointer,
        actions: &GamepadActions,
        opts: &GamepadInjectOpts,
    ) -> GamepadInjection {
        plan_gamepad_injection(pointer, actions, opts)
    }

    #[test]
    fn absolute_mouse_starts_at_centre_then_clamps() {
        let mut now = centred();
        now.left_x = 1.0;
        let actions = map_gamepad(&centred(), &now, &GamepadConfig::default());
        let mut pointer = GamepadPointer::default();
        let opts = live_opts(MouseMode::Absolute, Some((512, 342)));
        let first = plan(&mut pointer, &actions, &opts);
        assert_eq!(first.mouse_abs, Some(egui::pos2(256.0 + 12.0, 171.0)));
        assert!(first.mouse_rel.is_none());
        assert!(first.keys.is_empty());

        let second = plan(&mut pointer, &actions, &opts);
        assert_eq!(second.mouse_abs, Some(egui::pos2(256.0 + 24.0, 171.0)));

        pointer.cursor = Some(egui::pos2(510.0, 340.0));
        let mut pushed = actions.clone();
        pushed.mouse_dx = 12;
        pushed.mouse_dy = 12;
        let clamped = plan(&mut pointer, &pushed, &opts);
        assert_eq!(clamped.mouse_abs, Some(egui::pos2(511.0, 341.0)));
    }

    #[test]
    fn absolute_mouse_without_a_display_starts_at_the_origin() {
        let mut actions = GamepadActions::default();
        actions.mouse_dx = 4;
        actions.mouse_dy = -3;
        let mut pointer = GamepadPointer::default();
        let injection = plan(
            &mut pointer,
            &actions,
            &live_opts(MouseMode::Absolute, None),
        );
        assert_eq!(injection.mouse_abs, Some(egui::pos2(4.0, 0.0)));
    }

    #[test]
    fn relative_mouse_uses_the_delta_and_not_the_cursor() {
        let mut now = centred();
        now.left_y = 1.0;
        let actions = map_gamepad(&centred(), &now, &GamepadConfig::default());
        let mut pointer = GamepadPointer::default();
        let injection = plan(
            &mut pointer,
            &actions,
            &live_opts(MouseMode::RelativeHw, Some((512, 342))),
        );
        assert_eq!(injection.mouse_rel, Some(egui::pos2(0.0, -12.0)));
        assert!(injection.mouse_abs.is_none());
        assert!(pointer.cursor.is_none());
    }

    #[test]
    fn disabled_mouse_mode_injects_keys_only() {
        let mut actions = GamepadActions::default();
        actions.mouse_dx = 12;
        actions.mouse_dy = 4;
        actions.mouse_button = Some(true);
        actions.keys.push((KeyCode::KeyA, true));
        let mut pointer = GamepadPointer::default();
        let injection = plan(
            &mut pointer,
            &actions,
            &live_opts(MouseMode::Disabled, Some((512, 342))),
        );
        assert!(injection.mouse_abs.is_none());
        assert!(injection.mouse_rel.is_none());
        assert!(injection.mouse_button.is_none());
        assert_eq!(injection.keys, vec![(KeyCode::KeyA, true)]);
        assert!(!pointer.mouse_down);
        assert_eq!(pointer.held_keys, vec![KeyCode::KeyA]);
    }

    #[test]
    fn arrow_keys_do_not_move_the_mouse() {
        let cfg = GamepadConfig {
            stick_mode: StickMode::ArrowKeys,
            ..GamepadConfig::default()
        };
        let mut now = centred();
        now.left_x = 1.0;
        let actions = map_gamepad(&centred(), &now, &cfg);
        let mut pointer = GamepadPointer::default();
        let injection = plan(
            &mut pointer,
            &actions,
            &live_opts(MouseMode::Absolute, Some((512, 342))),
        );
        assert!(injection.mouse_abs.is_none());
        assert!(injection.mouse_rel.is_none());
        assert_eq!(injection.keys, vec![(KeyCode::ArrowRight, true)]);
        assert_eq!(pointer.held_keys, vec![KeyCode::ArrowRight]);
    }

    #[test]
    fn disconnect_releases_only_keys_the_pad_pressed() {
        let mut pointer = GamepadPointer::default();
        pointer.held_keys = vec![KeyCode::ArrowRight];
        pointer.mouse_down = true;
        let mut actions = GamepadActions::default();
        actions.disconnected = true;
        // A key this pad never pressed must not be released. That would drop
        // a real keyboard key that happens to share the scancode.
        actions.keys.push((KeyCode::KeyZ, false));
        actions.mouse_dx = 20;
        let injection = plan(
            &mut pointer,
            &actions,
            &live_opts(MouseMode::Absolute, Some((512, 342))),
        );
        assert_eq!(injection.keys, vec![(KeyCode::ArrowRight, false)]);
        assert_eq!(injection.mouse_button, Some(false));
        assert!(injection.mouse_abs.is_none());
        assert!(pointer.held_keys.is_empty());
        assert!(!pointer.mouse_down);
    }

    #[test]
    fn turning_the_pad_off_releases_held_input_without_moving() {
        let mut pointer = GamepadPointer::default();
        pointer.held_keys = vec![KeyCode::KeyQ];
        pointer.mouse_down = true;
        pointer.cursor = Some(egui::pos2(10.0, 10.0));
        let mut actions = GamepadActions::default();
        actions.mouse_dx = 8;
        actions.keys.push((KeyCode::KeyQ, true));
        let mut opts = live_opts(MouseMode::RelativeHw, Some((512, 342)));
        opts.enabled = false;
        let injection = plan(&mut pointer, &actions, &opts);
        assert_eq!(injection.keys, vec![(KeyCode::KeyQ, false)]);
        assert_eq!(injection.mouse_button, Some(false));
        assert!(injection.mouse_rel.is_none());
        assert_eq!(pointer.cursor, Some(egui::pos2(10.0, 10.0)));
    }

    #[test]
    fn capture_suppresses_new_keys_and_the_mouse_button() {
        let mut pointer = GamepadPointer::default();
        pointer.held_keys = vec![KeyCode::KeyA];
        pointer.mouse_down = true;
        let mut actions = GamepadActions::default();
        actions.keys.push((KeyCode::KeyB, true));
        actions.mouse_button = Some(true);
        actions.mouse_dx = 5;
        let mut opts = live_opts(MouseMode::Absolute, Some((100, 80)));
        opts.suppress_keys = true;
        opts.suppress_mouse_button = true;
        let injection = plan(&mut pointer, &actions, &opts);
        assert_eq!(injection.keys, vec![(KeyCode::KeyA, false)]);
        assert_eq!(injection.mouse_button, Some(false));
        assert_eq!(injection.mouse_abs, Some(egui::pos2(50.0 + 5.0, 40.0)));
        assert!(pointer.held_keys.is_empty());
        assert!(!pointer.mouse_down);
    }

    #[test]
    fn a_frozen_ui_does_not_move_the_pointer() {
        let mut actions = GamepadActions::default();
        actions.mouse_dx = 9;
        let mut pointer = GamepadPointer::default();
        let mut opts = live_opts(MouseMode::Absolute, Some((512, 342)));
        opts.move_mouse = false;
        let injection = plan(&mut pointer, &actions, &opts);
        assert!(injection.mouse_abs.is_none());
        assert!(pointer.cursor.is_none());
    }
}
