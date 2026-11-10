//! Simulation of the Lisa's COPS: the COP421 on the I/O board, and the keyboard
//!
//! Spec: docs/lisa/lisa-spec-cops.md. It only sees the pins of the keyboard VIA, so that
//! a COP421 running the firmware can replace it.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::keymap::{KeyEvent, Keymap};
use crate::tickable::Ticks;

/// One pass through the main loop of the firmware
const LOOP_TICKS: Ticks = 4096;
/// CRDY low, once a loop
const CRDY_LOW_TICKS: Ticks = 61;
/// The firmware reads the port this long after CRDY rises
const SAMPLE_TICKS: Ticks = CRDY_LOW_TICKS + 41;
/// A timer tick of the COP421: 1024 cycles of 4.07 us
const TIMER_TICK_TICKS: Ticks = 21_236;
/// A tenth of a second
const TENTH_TICKS: Ticks = 509_376;
/// From keyboard reset to it being seen as unplugged
const UNPLUGGED_TICKS: Ticks = 10_188;
/// From the end of the keyboard reset to the keyboard answering
const KEYBOARD_ID_TICKS: Ticks = 509_376;

const QUEUE_SIZE: usize = 8;

const RESET_CODE: u8 = 0x80;
const CODE_POWER_SWITCH: u8 = 0xFB;
const CODE_TIMER: u8 = 0xFC;
const CODE_UNPLUGGED: u8 = 0xFD;
/// Keytronics, Final US layout, US legends
const KEYBOARD_ID: u8 = 0xBF;
const KEY_MOUSE_BUTTON: u8 = 0x06;
const KEY_DOWN: u8 = 0x80;

/// Clock digit positions, from the first one written
const DIGITS: usize = 16;
const DIGIT_YEAR: usize = 5;
const DIGIT_DAY: usize = 6;
const DIGIT_HOUR: usize = 9;
const DIGIT_MINUTE: usize = 11;
const DIGIT_SECOND: usize = 13;
const DIGIT_TENTH: usize = 15;

/// Clock mode bits of the mode command
const MODE_SET: u8 = 1 << 3;
const MODE_STAY_ON: u8 = 1 << 2;
const MODE_TIMER: u8 = 1 << 1;
const MODE_CLOCK_MASK: u8 = 0b11;

#[derive(Serialize, Deserialize)]
pub struct CopsSim {
    /// CPU clocks since power on
    now: Ticks,
    next_sample: Ticks,
    next_tenth: Ticks,
    next_timer_tick: Ticks,

    /// Bytes to send to the CPU
    queue: VecDeque<u8>,
    /// A byte was sent, and the CPU hasn't taken it
    waiting_ack: bool,
    /// Commands $00 and $01
    port_on: bool,
    /// A byte to strobe into the VIA
    strobe: Option<u8>,

    power_on: bool,
    /// The VIA holds the keyboard in reset
    keyboard_reset: bool,
    unplug_at: Option<Ticks>,
    id_at: Option<Ticks>,

    nmi_key: u8,
    nmi: bool,

    mouse_enabled: bool,
    /// Timer ticks between mouse reports
    mouse_interval: u8,
    mouse_countdown: u8,
    mouse_x: i32,
    mouse_y: i32,
    mouse_button: bool,
    mouse_abs: Option<(u16, u16)>,

    /// Command $02, until the port is free
    clock_read_pending: bool,
    /// Mode command bits
    clock_mode: u8,
    digit_pointer: usize,
    digits: [u8; DIGITS],
}

impl CopsSim {
    pub fn new() -> Self {
        let mut digits = [0; DIGITS];
        // 1 January 1984
        digits[DIGIT_YEAR] = 4;
        digits[DIGIT_DAY + 2] = 1;
        Self {
            now: 0,
            next_sample: SAMPLE_TICKS,
            next_tenth: TENTH_TICKS,
            next_timer_tick: TIMER_TICK_TICKS,
            queue: VecDeque::with_capacity(QUEUE_SIZE),
            waiting_ack: false,
            port_on: true,
            strobe: None,
            power_on: true,
            keyboard_reset: false,
            unplug_at: None,
            id_at: None,
            nmi_key: 0,
            nmi: false,
            mouse_enabled: false,
            mouse_interval: 0,
            mouse_countdown: 0,
            mouse_x: 0,
            mouse_y: 0,
            mouse_button: false,
            mouse_abs: None,
            clock_read_pending: false,
            clock_mode: 0b0101,
            digit_pointer: 0,
            digits,
        }
    }

    /// Reset of the machine, which leaves the COPS running
    pub fn machine_reset(&mut self) {
        self.power_on = true;
        self.queue.clear();
        self.waiting_ack = false;
        self.strobe = None;
        self.port_on = true;
        self.keyboard_reset = false;
    }

    pub fn power_on(&self) -> bool {
        self.power_on
    }

    /// CRDY at a time ahead of the last tick: high is not ready
    pub fn crdy(&self, ahead: Ticks) -> bool {
        (self.now + ahead) % LOOP_TICKS >= CRDY_LOW_TICKS
    }

    /// The CPU read or wrote port A with handshake, which pulses CA2
    pub fn acknowledge(&mut self) {
        self.waiting_ack = false;
    }

    /// The keyboard reset line, driven by the VIA
    pub fn set_keyboard_reset(&mut self, reset: bool) {
        if reset == self.keyboard_reset {
            return;
        }
        self.keyboard_reset = reset;
        log::debug!("Keyboard reset {} at {}", reset, self.now);
        if reset {
            self.unplug_at = Some(self.now + UNPLUGGED_TICKS);
            self.id_at = None;
        } else {
            self.unplug_at = None;
            self.id_at = Some(self.now + KEYBOARD_ID_TICKS);
        }
    }

    /// A byte to put on port A and strobe with CA1
    pub fn take_strobe(&mut self) -> Option<u8> {
        self.strobe.take()
    }

    pub fn take_nmi(&mut self) -> bool {
        std::mem::take(&mut self.nmi)
    }

    /// `port` is what the VIA drives on port A, with undriven lines high
    pub fn tick(&mut self, ticks: Ticks, port: u8) {
        self.now += ticks;

        if self.unplug_at.is_some_and(|t| self.now >= t) {
            self.unplug_at = None;
            self.send_reset_code(CODE_UNPLUGGED);
        }
        if self.id_at.is_some_and(|t| self.now >= t) {
            self.id_at = None;
            self.send_reset_code(KEYBOARD_ID);
        }
        while self.now >= self.next_tenth {
            self.next_tenth += TENTH_TICKS;
            self.tick_tenth();
        }
        while self.now >= self.next_timer_tick {
            self.next_timer_tick += TIMER_TICK_TICKS;
            self.tick_mouse();
        }
        if self.now >= self.next_sample {
            self.next_sample += LOOP_TICKS;
            if self.power_on {
                self.command(port);
            }
            // Only when nothing else is going out
            if self.clock_read_pending
                && self.port_on
                && self.queue.is_empty()
                && !self.waiting_ack
                && self.strobe.is_none()
            {
                self.clock_read_pending = false;
                self.send_clock();
            }
            if self.port_on
                && !self.waiting_ack
                && self.strobe.is_none()
                && let Some(byte) = self.queue.pop_front()
            {
                #[cfg(feature = "lisa-cops-debug")]
                log::debug!("COPS sends {:02X} at {}", byte, self.now);
                self.strobe = Some(byte);
                self.waiting_ack = true;
            }
        }
    }

    fn send(&mut self, byte: u8) {
        if self.queue.len() < QUEUE_SIZE {
            self.queue.push_back(byte);
        } else {
            log::warn!("COPS queue full, lost {:02X}", byte);
        }
    }

    fn send_reset_code(&mut self, code: u8) {
        self.send(RESET_CODE);
        self.send(code);
    }

    fn command(&mut self, cmd: u8) {
        if cmd & 0x80 == 0 {
            log::debug!("COPS command {:02X} at {}", cmd, self.now);
        }
        let n = cmd & 0x0F;
        match cmd >> 4 {
            0 => match cmd {
                0 => self.port_on = true,
                1 => self.port_on = false,
                2 => self.clock_read_pending = true,
                _ => (),
            },
            1 => {
                if self.clock_mode & MODE_SET != 0 && self.digit_pointer < DIGITS {
                    self.digits[self.digit_pointer] = n;
                    self.digit_pointer += 1;
                }
            }
            2 => {
                self.clock_mode = n;
                if n & MODE_STAY_ON != 0 {
                    self.digit_pointer = 0;
                } else {
                    log::info!("COPS: power off");
                    self.power_on = false;
                }
            }
            // Keyboard indicators: none on the keyboards
            3 | 4 => (),
            5 => self.nmi_key = (self.nmi_key & 0x0F) | (n << 4),
            6 => self.nmi_key = (self.nmi_key & 0xF0) | n,
            7 => {
                self.mouse_enabled = n & 8 != 0;
                self.mouse_interval = n & 7;
                self.mouse_countdown = self.mouse_interval;
            }
            _ => (),
        }
    }

    fn digit_pair(&self, first: usize) -> u8 {
        (self.digits[first] << 4) | self.digits[first + 1]
    }

    fn send_clock(&mut self) {
        self.send(RESET_CODE);
        self.send(0xE0 | self.digits[DIGIT_YEAR]);
        for first in (DIGIT_DAY..DIGIT_TENTH).step_by(2) {
            self.send(self.digit_pair(first));
        }
    }

    /// Adds one to a BCD number in the digits, wrapping from `limit`
    fn step_bcd(&mut self, first: usize, count: usize, limit: u32, wrap_to: u32) -> bool {
        let value = self.digits[first..first + count]
            .iter()
            .fold(0, |v, &d| v * 10 + u32::from(d))
            + 1;
        let wrapped = value > limit;
        let mut value = if wrapped { wrap_to } else { value };
        for digit in self.digits[first..first + count].iter_mut().rev() {
            *digit = (value % 10) as u8;
            value /= 10;
        }
        wrapped
    }

    fn tick_tenth(&mut self) {
        if self.clock_mode & MODE_CLOCK_MASK == 0 {
            return;
        }
        if self.step_bcd(DIGIT_TENTH, 1, 9, 0) {
            self.tick_second();
            if self.step_bcd(DIGIT_SECOND, 2, 59, 0)
                && self.step_bcd(DIGIT_MINUTE, 2, 59, 0)
                && self.step_bcd(DIGIT_HOUR, 2, 23, 0)
            {
                let days = if self.digits[DIGIT_YEAR].is_multiple_of(4) {
                    366
                } else {
                    365
                };
                if self.step_bcd(DIGIT_DAY, 3, days, 1) {
                    self.digits[DIGIT_YEAR] = (self.digits[DIGIT_YEAR] + 1) & 0x0F;
                }
            }
        }
    }

    /// The timer is the alarm digits, binary
    fn tick_second(&mut self) {
        if self.clock_mode & MODE_TIMER == 0 {
            return;
        }
        let timer = self.digits[..5]
            .iter()
            .fold(0_u32, |v, &d| (v << 4) | u32::from(d));
        if timer == 0 {
            if self.clock_mode & MODE_CLOCK_MASK == MODE_CLOCK_MASK {
                self.power_on = true;
            }
            self.clock_mode &= !MODE_TIMER;
            self.send_reset_code(CODE_TIMER);
            return;
        }
        let timer = timer - 1;
        for (i, digit) in self.digits[..5].iter_mut().enumerate() {
            *digit = ((timer >> (4 * (4 - i))) & 0xF) as u8;
        }
    }

    fn tick_mouse(&mut self) {
        if !self.mouse_enabled {
            return;
        }
        if self.mouse_countdown > 0 {
            self.mouse_countdown -= 1;
            return;
        }
        self.mouse_countdown = self.mouse_interval;
        if self.mouse_x == 0 && self.mouse_y == 0 {
            return;
        }
        // All at once, or not: the packet can't be split
        if self.queue.len() + 3 > QUEUE_SIZE || self.waiting_ack {
            return;
        }
        let dx = self.mouse_x.clamp(-127, 127);
        let dy = self.mouse_y.clamp(-127, 127);
        self.mouse_x -= dx;
        self.mouse_y -= dy;
        self.send(0);
        self.send(dx as u8);
        self.send(dy as u8);
    }

    /// The power switch while on
    pub fn power_switch(&mut self) {
        self.send_reset_code(CODE_POWER_SWITCH);
    }

    pub fn mouse_rel(&mut self, relx: i16, rely: i16, button: Option<bool>) {
        self.mouse_x += i32::from(relx);
        self.mouse_y += i32::from(rely);
        if let Some(button) = button
            && button != self.mouse_button
        {
            self.mouse_button = button;
            self.send(KEY_MOUSE_BUTTON | if button { KEY_DOWN } else { 0 });
        }
    }

    /// Absolute position from the host: the Lisa only has relative motion
    pub fn mouse_abs(&mut self, x: u16, y: u16) {
        if let Some((last_x, last_y)) = self.mouse_abs {
            self.mouse_x += i32::from(x) - i32::from(last_x);
            self.mouse_y += i32::from(y) - i32::from(last_y);
        }
        self.mouse_abs = Some((x, y));
    }

    pub fn key_event(&mut self, event: KeyEvent) {
        let (scancode, keymap, down) = match event {
            KeyEvent::KeyDown(sc, km) => (sc, km, true),
            KeyEvent::KeyUp(sc, km) => (sc, km, false),
        };
        let scancode = if keymap == Keymap::AekM0115 {
            scancode
        } else {
            match Keymap::AekM0115.translate(scancode) {
                Some(sc) => sc,
                None => return,
            }
        };
        let Some(code) = lisa_key_code(scancode) else {
            return;
        };
        if down && code == self.nmi_key && self.nmi_key != 0 {
            self.nmi = true;
            return;
        }
        self.send(code | if down { KEY_DOWN } else { 0 });
    }

    pub fn release_all(&mut self) {
        if self.mouse_button {
            self.mouse_rel(0, 0, Some(false));
        }
    }
}

/// Lisa key code of a scancode of the Apple Extended Keyboard, which is the Snow
/// universal keymap
fn lisa_key_code(scancode: u8) -> Option<u8> {
    Some(match scancode {
        0x00 => 0x70, // A
        0x01 => 0x76, // S
        0x02 => 0x7B, // D
        0x03 => 0x69, // F
        0x04 => 0x6B, // H
        0x05 => 0x6A, // G
        0x06 => 0x79, // Z
        0x07 => 0x7A, // X
        0x08 => 0x6D, // C
        0x09 => 0x6C, // V
        0x0B => 0x6E, // B
        0x0C => 0x75, // Q
        0x0D => 0x77, // W
        0x0E => 0x60, // E
        0x0F => 0x65, // R
        0x10 => 0x67, // Y
        0x11 => 0x66, // T
        0x12 => 0x74, // 1
        0x13 => 0x71, // 2
        0x14 => 0x72, // 3
        0x15 => 0x73, // 4
        0x16 => 0x61, // 6
        0x17 => 0x64, // 5
        0x18 => 0x41, // =
        0x19 => 0x50, // 9
        0x1A => 0x62, // 7
        0x1B => 0x40, // -
        0x1C => 0x63, // 8
        0x1D => 0x51, // 0
        0x1E => 0x57, // ]
        0x1F => 0x5F, // O
        0x20 => 0x52, // U
        0x21 => 0x56, // [
        0x22 => 0x53, // I
        0x23 => 0x44, // P
        0x24 => 0x48, // Return
        0x25 => 0x59, // L
        0x26 => 0x54, // J
        0x27 => 0x5B, // '
        0x28 => 0x55, // K
        0x29 => 0x5A, // ;
        0x2A => 0x42, // Backslash
        0x2B => 0x5D, // ,
        0x2C => 0x4C, // /
        0x2D => 0x6F, // N
        0x2E => 0x58, // M
        0x2F => 0x5E, // .
        0x30 => 0x78, // Tab
        0x31 => 0x5C, // Space
        0x32 => 0x7C, // `
        0x33 => 0x45, // Backspace
        0x37 => 0x7F, // Command
        0x38 => 0x7E, // Shift
        0x39 => 0x7D, // Alpha Lock
        0x3A => 0x68, // Option
        0x41 => 0x2C, // Pad .
        0x47 => 0x20, // Pad Clear
        0x4C => 0x2F, // Pad Enter
        0x4E => 0x21, // Pad -
        0x52 => 0x49, // Pad 0
        0x53 => 0x4D, // Pad 1
        0x54 => 0x2D, // Pad 2
        0x55 => 0x2E, // Pad 3
        0x56 => 0x28, // Pad 4
        0x57 => 0x29, // Pad 5
        0x58 => 0x2A, // Pad 6
        0x59 => 0x24, // Pad 7
        0x5B => 0x25, // Pad 8
        0x5C => 0x26, // Pad 9
        0x7B => 0x22, // Left
        0x7C => 0x23, // Right
        0x7D => 0x2B, // Down
        0x7E => 0x27, // Up
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE: u8 = 0xFF;

    fn run(cops: &mut CopsSim, ticks: Ticks, port: u8) -> Vec<u8> {
        let mut bytes = vec![];
        for _ in 0..ticks / 16 {
            cops.tick(16, port);
            if let Some(byte) = cops.take_strobe() {
                bytes.push(byte);
                cops.acknowledge();
            }
        }
        bytes
    }

    #[test]
    fn crdy_runs_free() {
        let mut cops = CopsSim::new();
        cops.tick(LOOP_TICKS - 1, IDLE);
        assert!(cops.crdy(0));
        assert!(!cops.crdy(1));
        assert!(cops.crdy(1 + CRDY_LOW_TICKS));
    }

    #[test]
    fn command_on_the_sample_only_when_driven() {
        let mut cops = CopsSim::new();
        cops.tick(SAMPLE_TICKS - 1, 0x71);
        assert!(!cops.mouse_enabled);
        run(&mut cops, LOOP_TICKS, 0x7C);
        assert!(cops.mouse_enabled);
        // Undriven lines read as a no-op
        run(&mut cops, LOOP_TICKS * 2, IDLE);
        assert!(cops.mouse_enabled);
    }

    #[test]
    fn keyboard_reset_codes() {
        let mut cops = CopsSim::new();
        cops.set_keyboard_reset(true);
        let bytes = run(&mut cops, UNPLUGGED_TICKS + LOOP_TICKS * 3, IDLE);
        assert_eq!(bytes, [RESET_CODE, CODE_UNPLUGGED]);
        cops.set_keyboard_reset(false);
        let bytes = run(&mut cops, KEYBOARD_ID_TICKS + LOOP_TICKS * 3, IDLE);
        assert_eq!(bytes, [RESET_CODE, KEYBOARD_ID]);
    }

    #[test]
    fn one_byte_until_ack() {
        let mut cops = CopsSim::new();
        cops.send(1);
        cops.send(2);
        let mut seen: Vec<u8> = vec![];
        for _ in 0..LOOP_TICKS * 3 / 16 {
            cops.tick(16, IDLE);
            seen.extend(cops.take_strobe());
        }
        assert_eq!(seen, [1]);
        cops.acknowledge();
        assert_eq!(run(&mut cops, LOOP_TICKS * 2, IDLE), [2]);
    }

    #[test]
    fn clock_set_and_read() {
        let mut cops = CopsSim::new();
        cops.command(0x2C);
        for n in 0..16 {
            cops.command(0x10 | [0, 0, 0, 0, 0, 7, 1, 2, 3, 1, 2, 3, 4, 5, 6, 7][n]);
        }
        cops.command(0x25);
        cops.command(0x02);
        let bytes = run(&mut cops, LOOP_TICKS * 10, IDLE);
        assert_eq!(bytes, [0x80, 0xE7, 0x12, 0x31, 0x23, 0x45, 0x67]);
    }

    #[test]
    fn clock_carries() {
        let mut cops = CopsSim::new();
        cops.digits[DIGIT_HOUR..].copy_from_slice(&[2, 3, 5, 9, 5, 9, 9]);
        cops.tick_tenth();
        assert_eq!(&cops.digits[DIGIT_DAY..], &[0, 0, 2, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn power_off() {
        let mut cops = CopsSim::new();
        cops.command(0x21);
        assert!(!cops.power_on());
        // The VIA stays as it was: no second power off
        let now = cops.now;
        cops.tick(SAMPLE_TICKS, 0x20);
        assert!(cops.now > now && !cops.power_on());
        cops.machine_reset();
        assert!(cops.power_on());
    }

    #[test]
    fn mouse_only_when_moved() {
        let mut cops = CopsSim::new();
        cops.command(0x78);
        assert!(run(&mut cops, TIMER_TICK_TICKS * 2, IDLE).is_empty());
        cops.mouse_rel(3, -2, None);
        let bytes = run(&mut cops, TIMER_TICK_TICKS * 2, IDLE);
        assert_eq!(bytes, [0, 3, 0xFE]);
    }
}
