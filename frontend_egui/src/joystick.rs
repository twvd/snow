//! Joystick input for the Apple IIe Card's game port: game controllers on
//! the host, and with controller emulation, keys on the host keyboard.
//!
//! Without controller emulation the IIe has a joystick while a controller
//! is connected to the host. With it, the IIe always has one: the keys
//! move it while switched on, and a real controller still works, taking
//! priority over the keys.

use egui_winit::winit::keyboard::KeyCode;
use gilrs::{Axis, Button, EventType, GamepadId, Gilrs};
use serde::{Deserialize, Serialize};
use snow_core::mac::maclc::pds_iie::GamePort;
use std::fmt;
use strum::{Display, EnumIter, IntoEnumIterator};

/// The Apple IIe joystick inputs, in the order of [`JoyBindings`]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Display, EnumIter)]
pub enum JoyInput {
    #[strum(to_string = "Button 0")]
    Button0,
    #[strum(to_string = "Button 1")]
    Button1,
    Left,
    Right,
    Up,
    Down,
}

/// A host key or game controller button that drives a joystick input
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoyBinding {
    Key(KeyCode),
    Pad(Button),
}

impl fmt::Display for JoyBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Key(key) => write!(f, "{:?}", key),
            Self::Pad(button) => write!(f, "Controller {:?}", button),
        }
    }
}

/// The bindings each joystick input can have; any of them works
pub const JOY_CHOICES: usize = 5;

/// The bindings of each joystick input, indexed by [`JoyInput`]
pub type JoyBindings = [[Option<JoyBinding>; JOY_CHOICES]; 6];

/// Binds `binding` to choice `slot` of `input`. A binding is used once:
/// where it was bound before, it is removed.
pub fn assign(bindings: &mut JoyBindings, input: JoyInput, slot: usize, binding: JoyBinding) {
    for b in bindings.iter_mut().flatten() {
        if *b == Some(binding) {
            *b = None;
        }
    }
    bindings[input as usize][slot] = Some(binding);
}

/// The default bindings. Buttons, by column: Z/X, 1/2, Left Ctrl/Alt,
/// keypad 0/. and the controller's West/East. Directions: I/J/L/M,
/// keypad 8/4/6/2, the arrows, W/A/S/D and the D-pad.
pub fn default_bindings() -> JoyBindings {
    use JoyBinding::{Key, Pad};
    let key = |k| Some(Key(k));
    let pad = |b| Some(Pad(b));
    [
        // The right-hand key or button of each pair is button 0: in Lode
        // Runner it digs right
        [
            key(KeyCode::KeyX),
            key(KeyCode::Digit2),
            key(KeyCode::AltLeft),
            key(KeyCode::NumpadDecimal),
            pad(Button::East),
        ],
        [
            key(KeyCode::KeyZ),
            key(KeyCode::Digit1),
            key(KeyCode::ControlLeft),
            key(KeyCode::Numpad0),
            pad(Button::West),
        ],
        [
            key(KeyCode::KeyJ),
            key(KeyCode::Numpad4),
            key(KeyCode::ArrowLeft),
            key(KeyCode::KeyA),
            pad(Button::DPadLeft),
        ],
        [
            key(KeyCode::KeyL),
            key(KeyCode::Numpad6),
            key(KeyCode::ArrowRight),
            key(KeyCode::KeyD),
            pad(Button::DPadRight),
        ],
        [
            key(KeyCode::KeyI),
            key(KeyCode::Numpad8),
            key(KeyCode::ArrowUp),
            key(KeyCode::KeyW),
            pad(Button::DPadUp),
        ],
        [
            key(KeyCode::KeyM),
            key(KeyCode::Numpad2),
            key(KeyCode::ArrowDown),
            key(KeyCode::KeyS),
            pad(Button::DPadDown),
        ],
    ]
}

/// The key that switches the joystick keys on and off
const JOY_TOGGLE_KEY: KeyCode = KeyCode::ScrollLock;

/// A joystick axis position at rest
const CENTRE: u8 = 128;
/// Stick deflections below this are taken as the centre
const DEADZONE: f32 = 0.1;

/// Converts a controller axis, -1.0 to 1.0, to a paddle position
fn axis_to_paddle(v: f32) -> u8 {
    if v.abs() < DEADZONE {
        return CENTRE;
    }
    ((v.clamp(-1.0, 1.0) + 1.0) * 127.5).round() as u8
}

/// Merges the controller and the keys into the game port. Without
/// controller emulation the port is empty unless a controller is connected.
/// A controller axis away from the centre wins over the keys; the buttons
/// of both work.
fn game_port_from(emulation: bool, pad: Option<JoyState>, keys: JoyState) -> GamePort {
    if !emulation && pad.is_none() {
        return GamePort::default();
    }
    let (pad_axes, pad_buttons) = pad.unwrap_or(([CENTRE; 2], [false; 2]));
    let (key_axes, key_buttons) = keys;
    let axis = |i: usize| {
        Some(if pad_axes[i] != CENTRE {
            pad_axes[i]
        } else {
            key_axes[i]
        })
    };
    GamePort {
        paddles: [axis(0), axis(1)],
        buttons: [
            pad_buttons[0] || key_buttons[0],
            pad_buttons[1] || key_buttons[1],
        ],
    }
}

/// A paddle position from two opposite keys
fn keys_to_paddle(low: bool, high: bool) -> u8 {
    match (low, high) {
        (true, false) => 0,
        (false, true) => 255,
        _ => CENTRE,
    }
}

pub struct Joystick {
    /// Controller emulation: the IIe always has a joystick, and the keys
    /// can drive it
    emulation: bool,
    /// The controller library (None if it failed to start)
    gilrs: Option<Gilrs>,
    /// The controller that last sent an event
    gamepad: Option<GamepadId>,
    /// The joystick keys are switched on (controller emulation only)
    keys_on: bool,
    /// The keys and controller buttons of each input
    bindings: JoyBindings,
    /// The key bindings held, in the order of `bindings`
    held: [[bool; JOY_CHOICES]; 6],
    /// The game port as last sent to the emulator
    sent: Option<GamePort>,
    /// The controller button last pressed, for the mapping dialog
    pressed_button: Option<Button>,
}

/// Joystick axes (0 to 255) and buttons
type JoyState = ([u8; 2], [bool; 2]);

impl Joystick {
    pub fn new(emulation: bool) -> Self {
        let gilrs = match Gilrs::new() {
            Ok(g) => Some(g),
            Err(e) => {
                log::error!("Cannot open game controllers: {}", e);
                None
            }
        };
        Self::with_gilrs(emulation, gilrs)
    }

    fn with_gilrs(emulation: bool, gilrs: Option<Gilrs>) -> Self {
        Self {
            emulation,
            gilrs,
            gamepad: None,
            keys_on: false,
            bindings: default_bindings(),
            held: [[false; JOY_CHOICES]; 6],
            sent: None,
            pressed_button: None,
        }
    }

    /// Replaces the bindings
    pub fn set_bindings(&mut self, bindings: JoyBindings) {
        self.bindings = bindings;
        self.held = [[false; JOY_CHOICES]; 6];
    }

    /// The controller button pressed since the last call, if any
    pub fn take_pressed_button(&mut self) -> Option<Button> {
        self.pressed_button.take()
    }

    /// What the joystick keys are, for the menu and toolbar
    pub fn help(&self) -> String {
        let mut text = String::from("Apple IIe joystick keys");
        for input in JoyInput::iter() {
            let keys: Vec<String> = self.bindings[input as usize]
                .iter()
                .flatten()
                .filter(|b| matches!(b, JoyBinding::Key(_)))
                .map(|b| b.to_string())
                .collect();
            if !keys.is_empty() {
                text += &format!("\n{}: {}", input, keys.join(", "));
            }
        }
        text + "\nScroll Lock switches them on and off."
    }

    /// Whether controller emulation is enabled
    pub fn emulation(&self) -> bool {
        self.emulation
    }

    /// Whether the joystick keys are switched on
    pub fn keys_on(&self) -> bool {
        self.keys_on
    }

    /// Switches the joystick keys on or off
    pub fn set_keys_on(&mut self, on: bool) {
        self.keys_on = on;
        self.held = [[false; JOY_CHOICES]; 6];
    }

    /// Handles a host key. Returns true if the joystick takes it, and it
    /// must not reach the emulated keyboard.
    pub fn key(&mut self, key: KeyCode, pressed: bool) -> bool {
        if !self.emulation {
            return false;
        }
        if key == JOY_TOGGLE_KEY {
            if pressed {
                self.set_keys_on(!self.keys_on);
            }
            return true;
        }
        if !self.keys_on {
            return false;
        }
        // A release whose press went to the emulated keyboard, before
        // the joystick keys were switched on, goes there too
        let mut taken = false;
        for (bindings, held) in self.bindings.iter().zip(&mut self.held) {
            for (binding, held) in bindings.iter().zip(held) {
                if *binding == Some(JoyBinding::Key(key)) && (pressed || *held) {
                    *held = pressed;
                    taken = true;
                }
            }
        }
        taken
    }

    /// Whether a key held drives `input`
    fn key_input(&self, input: JoyInput) -> bool {
        self.held[input as usize].contains(&true)
    }

    /// The joystick from the keys (centred when they are switched off)
    fn keys_state(&self) -> JoyState {
        (
            [
                keys_to_paddle(
                    self.key_input(JoyInput::Left),
                    self.key_input(JoyInput::Right),
                ),
                keys_to_paddle(self.key_input(JoyInput::Up), self.key_input(JoyInput::Down)),
            ],
            [
                self.key_input(JoyInput::Button0),
                self.key_input(JoyInput::Button1),
            ],
        )
    }

    /// The joystick from the host's game controller, or None if none is
    /// connected
    fn controller_state(&mut self) -> Option<JoyState> {
        let gilrs = self.gilrs.as_mut()?;
        while let Some(ev) = gilrs.next_event() {
            self.gamepad = Some(ev.id);
            if let EventType::ButtonPressed(button, _) = ev.event {
                self.pressed_button = Some(button);
            }
        }
        // With no controller event yet, take the first connected
        let id = self
            .gamepad
            .filter(|&id| gilrs.connected_gamepad(id).is_some())
            .or_else(|| gilrs.gamepads().next().map(|(id, _)| id))?;
        let pad = gilrs.connected_gamepad(id)?;
        // A controller button bound to `input` is down
        let input = |input: JoyInput| {
            self.bindings[input as usize]
                .iter()
                .any(|b| matches!(b, Some(JoyBinding::Pad(button)) if pad.is_pressed(*button)))
        };
        // The bound buttons (the D-pad by default) override the left stick
        let x = keys_to_paddle(input(JoyInput::Left), input(JoyInput::Right));
        let y = keys_to_paddle(input(JoyInput::Up), input(JoyInput::Down));
        Some((
            [
                if x != CENTRE {
                    x
                } else {
                    axis_to_paddle(pad.value(Axis::LeftStickX))
                },
                // The stick's up is positive, the IIe's is 0
                if y != CENTRE {
                    y
                } else {
                    axis_to_paddle(-pad.value(Axis::LeftStickY))
                },
            ],
            [input(JoyInput::Button0), input(JoyInput::Button1)],
        ))
    }

    /// The game port: the host's controller and the keys, merged
    fn game_port(&mut self) -> GamePort {
        let pad = self.controller_state();
        game_port_from(self.emulation, pad, self.keys_state())
    }

    /// Polls the source. Returns the game port if it changed since it was
    /// last sent.
    pub fn poll(&mut self) -> Option<GamePort> {
        let port = self.game_port();
        if self.sent == Some(port) {
            return None;
        }
        self.sent = Some(port);
        Some(port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_range() {
        assert_eq!(axis_to_paddle(-1.0), 0);
        assert_eq!(axis_to_paddle(1.0), 255);
        assert_eq!(axis_to_paddle(0.05), CENTRE);
        assert_eq!(axis_to_paddle(0.5), 191);
    }

    #[test]
    fn keyboard_joystick() {
        let mut joy = Joystick::with_gilrs(true, None);
        assert!(!joy.key(KeyCode::ArrowLeft, true), "keys off");
        joy.set_keys_on(true);
        assert!(joy.key(KeyCode::ArrowLeft, true));
        assert!(!joy.key(KeyCode::KeyQ, true));
        assert_eq!(
            joy.poll(),
            Some(GamePort {
                paddles: [Some(0), Some(CENTRE)],
                buttons: [false; 2],
            })
        );
        assert_eq!(joy.poll(), None, "unchanged");
        joy.key(KeyCode::ArrowLeft, false);
        joy.key(KeyCode::ArrowDown, true);
        assert_eq!(joy.poll().unwrap().paddles, [Some(CENTRE), Some(255)]);
    }

    #[test]
    fn keypad_and_buttons() {
        let mut joy = Joystick::with_gilrs(true, None);
        assert!(joy.key(KeyCode::ScrollLock, true));
        assert!(joy.key(KeyCode::ScrollLock, false));
        assert!(joy.keys_on());
        joy.key(KeyCode::Numpad6, true);
        joy.key(KeyCode::AltLeft, true);
        joy.key(KeyCode::Numpad0, true);
        assert_eq!(
            joy.poll(),
            Some(GamePort {
                paddles: [Some(255), Some(CENTRE)],
                buttons: [true, true],
            })
        );
        // Two bindings of a direction held: releasing one keeps it
        joy.key(KeyCode::ArrowRight, true);
        joy.key(KeyCode::Numpad6, false);
        assert_eq!(joy.poll(), None, "still right");
        joy.key(KeyCode::ScrollLock, true);
        assert!(!joy.keys_on());
        assert!(!joy.key(KeyCode::ControlLeft, true), "keys off");
    }

    #[test]
    fn default_bindings_have_no_conflicts() {
        let all: Vec<_> = default_bindings().into_iter().flatten().flatten().collect();
        for (i, b) in all.iter().enumerate() {
            assert!(!all[i + 1..].contains(b), "{:?} bound twice", b);
        }
    }

    #[test]
    fn assign_removes_the_older_binding() {
        let mut b = default_bindings();
        let south = JoyBinding::Pad(Button::South);
        assign(&mut b, JoyInput::Button0, 2, south);
        assert_eq!(b[JoyInput::Button0 as usize][2], Some(south));
        // Moved to button 1: gone from button 0
        assign(&mut b, JoyInput::Button1, 0, south);
        assert_eq!(b[JoyInput::Button0 as usize][2], None);
        assert_eq!(b[JoyInput::Button1 as usize][0], Some(south));
        // A key already on Left, bound to Right
        let left = JoyBinding::Key(KeyCode::ArrowLeft);
        assign(&mut b, JoyInput::Right, 1, left);
        assert_eq!(b[JoyInput::Left as usize][2], None);
        assert_eq!(b[JoyInput::Right as usize][1], Some(left));
    }

    #[test]
    fn bindings_round_trip() {
        let json = serde_json::to_string(&default_bindings()).unwrap();
        assert!(
            json.contains("ControlLeft") && json.contains("West"),
            "{}",
            json
        );
        let back: JoyBindings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, default_bindings());
    }

    #[test]
    fn rebound_key() {
        let mut joy = Joystick::with_gilrs(true, None);
        let mut b = default_bindings();
        assign(&mut b, JoyInput::Button0, 0, JoyBinding::Key(KeyCode::KeyQ));
        joy.set_bindings(b);
        joy.set_keys_on(true);
        assert!(!joy.key(KeyCode::KeyX, true), "replaced");
        assert!(joy.key(KeyCode::KeyQ, true));
        assert_eq!(joy.poll().unwrap().buttons, [true, false]);
    }

    #[test]
    fn toggle_key_only_with_emulation() {
        let mut joy = Joystick::with_gilrs(false, None);
        assert!(!joy.key(KeyCode::ScrollLock, true));
        assert!(!joy.keys_on());
    }

    #[test]
    fn port_follows_the_controller_without_emulation() {
        let keys = ([CENTRE; 2], [false; 2]);
        assert_eq!(game_port_from(false, None, keys), GamePort::default());
        assert_eq!(
            game_port_from(false, Some(([10, CENTRE], [false, true])), keys).paddles,
            [Some(10), Some(CENTRE)]
        );
        // With emulation the IIe always has a joystick
        assert_eq!(
            game_port_from(true, None, keys).paddles,
            [Some(CENTRE), Some(CENTRE)]
        );
    }

    #[test]
    fn controller_wins_over_keys() {
        let keys = ([0, 255], [true, false]);
        let port = game_port_from(true, Some(([200, CENTRE], [false, true])), keys);
        assert_eq!(port.paddles, [Some(200), Some(255)]);
        assert_eq!(port.buttons, [true, true]);
    }
}
