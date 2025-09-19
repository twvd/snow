use std::collections::VecDeque;

use anyhow::Result;
use log::*;
use serde::{Deserialize, Serialize};

use crate::keymap::{KeyEvent, Keymap};

/// Apple M0110 keyboard, for the 512K/Plus
#[derive(Default, Serialize, Deserialize)]
pub struct PlusKeyboard {
    event_queue: VecDeque<KeyEvent>,
}

// Scancodes
pub const SC_BACKSPACE: u8 = 0x67;
pub const SC_TAB: u8 = 0x61;
pub const SC_CAPSLOCK: u8 = 0x73;
pub const SC_RETURN: u8 = 0x49;
pub const SC_SHIFT: u8 = 0x71;
pub const SC_OPTION: u8 = 0x75;
pub const SC_APPLE: u8 = 0x6F;
pub const SC_SPACE: u8 = 0x63;

impl PlusKeyboard {
    const KEYMAP: Keymap = Keymap::AkM0110;

    pub fn pending_events(&self) -> bool {
        !self.event_queue.is_empty()
    }

    pub fn event(&mut self, ev: KeyEvent) {
        self.event_queue.push_back(ev);
    }

    pub fn cmd(&mut self, cmd: u8) -> Result<u8> {
        match cmd {
            // Inquire/Instant
            0x10 | 0x14 => {
                if let Some(ev) = self
                    .event_queue
                    .pop_front()
                    .and_then(|ke| ke.translate_scancode(Self::KEYMAP))
                {
                    let result = match ev {
                        KeyEvent::KeyDown(sc) => sc,
                        KeyEvent::KeyUp(sc) => 0x80 | sc,
                    };
                    Ok(result | 0x01)
                } else {
                    // Null
                    Ok(0x7B)
                }
            }
            // Model
            0x16 => {
                // US layout
                info!("Keyboard reset");
                Ok(3)
            }
            // Test
            0x36 => Ok(0x7D),
            _ => {
                warn!("Unknown keyboard command ${:02X}", cmd);
                Ok(0)
            }
        }
    }
}
