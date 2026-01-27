//! RPC method handlers and dispatch

use super::types::*;
use crate::emulator::comm::EmulatorSpeed;
use crate::keymap::{KeyEvent, Keymap, Scancode};
use crate::mac::scc::SccCh;
use crate::mac::serial_bridge::SerialBridgeConfig;
use crate::renderer::DisplayBuffer;
use log::*;
use std::collections::HashMap;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};

/// Key name to scancode mapping (Apple Extended Keyboard M0115)
fn build_key_map() -> HashMap<&'static str, Scancode> {
    HashMap::from([
        // Modifier keys
        ("command", 0x37),
        ("cmd", 0x37),
        ("apple", 0x37),
        ("option", 0x3A),
        ("alt", 0x3A),
        ("control", 0x36),
        ("ctrl", 0x36),
        ("shift", 0x38),
        ("capslock", 0x39),
        ("caps", 0x39),
        // Function keys
        ("escape", 0x35),
        ("esc", 0x35),
        ("f1", 0x7A),
        ("f2", 0x78),
        ("f3", 0x63),
        ("f4", 0x76),
        ("f5", 0x60),
        ("f6", 0x61),
        ("f7", 0x62),
        ("f8", 0x64),
        ("f9", 0x65),
        ("f10", 0x6D),
        ("f11", 0x67),
        ("f12", 0x6F),
        ("f13", 0x69),
        ("f14", 0x6B),
        ("f15", 0x71),
        // Special keys
        ("return", 0x24),
        ("enter", 0x24),
        ("tab", 0x30),
        ("space", 0x31),
        ("backspace", 0x33),
        ("delete", 0x75),
        ("del", 0x75),
        // Arrow keys
        ("up", 0x3E),
        ("down", 0x3D),
        ("left", 0x3B),
        ("right", 0x3C),
        // Navigation keys
        ("home", 0x73),
        ("end", 0x77),
        ("pageup", 0x74),
        ("pgup", 0x74),
        ("pagedown", 0x79),
        ("pgdn", 0x79),
        ("insert", 0x72),
        ("ins", 0x72),
        // Letter keys (lowercase)
        ("a", 0x00),
        ("b", 0x0B),
        ("c", 0x08),
        ("d", 0x02),
        ("e", 0x0E),
        ("f", 0x03),
        ("g", 0x05),
        ("h", 0x04),
        ("i", 0x22),
        ("j", 0x26),
        ("k", 0x28),
        ("l", 0x25),
        ("m", 0x2E),
        ("n", 0x2D),
        ("o", 0x1F),
        ("p", 0x23),
        ("q", 0x0C),
        ("r", 0x0F),
        ("s", 0x01),
        ("t", 0x11),
        ("u", 0x20),
        ("v", 0x09),
        ("w", 0x0D),
        ("x", 0x07),
        ("y", 0x10),
        ("z", 0x06),
        // Number keys (top row)
        ("1", 0x12),
        ("2", 0x13),
        ("3", 0x14),
        ("4", 0x15),
        ("5", 0x17),
        ("6", 0x16),
        ("7", 0x1A),
        ("8", 0x1C),
        ("9", 0x19),
        ("0", 0x1D),
        // Punctuation and symbols
        ("-", 0x1B),
        ("minus", 0x1B),
        ("=", 0x18),
        ("equals", 0x18),
        ("[", 0x21),
        ("lbracket", 0x21),
        ("]", 0x1E),
        ("rbracket", 0x1E),
        ("\\", 0x2A),
        ("backslash", 0x2A),
        (";", 0x29),
        ("semicolon", 0x29),
        ("'", 0x27),
        ("quote", 0x27),
        ("`", 0x32),
        ("grave", 0x32),
        ("backtick", 0x32),
        (",", 0x2B),
        ("comma", 0x2B),
        (".", 0x2F),
        ("period", 0x2F),
        ("/", 0x2C),
        ("slash", 0x2C),
    ])
}

static KEY_MAP: LazyLock<HashMap<&'static str, Scancode>> = LazyLock::new(build_key_map);

/// Resolves a key name to a scancode
pub fn resolve_key(key: &str) -> Option<Scancode> {
    KEY_MAP.get(key.to_lowercase().as_str()).copied()
}

/// Converts a character to a scancode, optionally returning whether shift is needed
pub fn char_to_scancode(c: char) -> Option<(Scancode, bool)> {
    let (key, shift) = match c {
        'a'..='z' => (c.to_string(), false),
        'A'..='Z' => (c.to_ascii_lowercase().to_string(), true),
        '0'..='9' => (c.to_string(), false),
        ' ' => ("space".to_string(), false),
        '\n' => ("return".to_string(), false),
        '\t' => ("tab".to_string(), false),
        '!' => ("1".to_string(), true),
        '@' => ("2".to_string(), true),
        '#' => ("3".to_string(), true),
        '$' => ("4".to_string(), true),
        '%' => ("5".to_string(), true),
        '^' => ("6".to_string(), true),
        '&' => ("7".to_string(), true),
        '*' => ("8".to_string(), true),
        '(' => ("9".to_string(), true),
        ')' => ("0".to_string(), true),
        '-' => ("-".to_string(), false),
        '_' => ("-".to_string(), true),
        '=' => ("=".to_string(), false),
        '+' => ("=".to_string(), true),
        '[' => ("[".to_string(), false),
        '{' => ("[".to_string(), true),
        ']' => ("]".to_string(), false),
        '}' => ("]".to_string(), true),
        '\\' => ("\\".to_string(), false),
        '|' => ("\\".to_string(), true),
        ';' => (";".to_string(), false),
        ':' => (";".to_string(), true),
        '\'' => ("'".to_string(), false),
        '"' => ("'".to_string(), true),
        '`' => ("`".to_string(), false),
        '~' => ("`".to_string(), true),
        ',' => (",".to_string(), false),
        '<' => (",".to_string(), true),
        '.' => (".".to_string(), false),
        '>' => (".".to_string(), true),
        '/' => ("/".to_string(), false),
        '?' => ("/".to_string(), true),
        _ => return None,
    };

    resolve_key(&key).map(|sc| (sc, shift))
}

/// Trait that must be implemented by the frontend to handle RPC requests
pub trait RpcHandler {
    /// Get current emulator status
    fn get_status(&self) -> Option<StatusGetResult>;

    /// Get shared directory path
    fn get_shared_dir(&self) -> Option<PathBuf>;

    /// Set shared directory path
    fn set_shared_dir(&mut self, path: Option<PathBuf>) -> bool;

    /// Get current speed mode
    fn get_speed(&self) -> Option<(EmulatorSpeed, f64)>;

    /// Set speed mode, returns previous mode
    fn set_speed(&mut self, speed: EmulatorSpeed) -> Option<EmulatorSpeed>;

    /// Run the emulator
    fn emulator_run(&mut self) -> bool;

    /// Stop the emulator
    fn emulator_stop(&mut self) -> bool;

    /// Reset the emulator
    fn emulator_reset(&mut self) -> bool;

    /// Get current mouse position (absolute mode only)
    fn get_mouse_position(&self) -> Option<(u16, u16)>;

    /// Set absolute mouse position
    fn set_mouse_position(&mut self, x: u16, y: u16) -> bool;

    /// Move mouse relative
    fn move_mouse(&mut self, dx: i16, dy: i16) -> bool;

    /// Set mouse button state
    fn set_mouse_button(&mut self, down: bool) -> bool;

    /// Send key event
    fn send_key_event(&mut self, event: KeyEvent);

    /// Release all keys
    fn release_all_keys(&mut self);

    /// Get serial bridge status
    fn get_serial_status(&self, channel: SccCh) -> Option<String>;

    /// Enable serial bridge
    fn enable_serial(
        &mut self,
        channel: SccCh,
        config: SerialBridgeConfig,
    ) -> Result<SerialBridgeStatusInfo, String>;

    /// Disable serial bridge
    fn disable_serial(&mut self, channel: SccCh) -> bool;

    /// Check if serial bridge is enabled
    fn is_serial_enabled(&self, channel: SccCh) -> bool;

    /// Get the current frame buffer for screenshots
    fn get_frame_buffer(&self) -> Option<Arc<Mutex<Option<DisplayBuffer>>>>;

    /// Insert a floppy disk
    fn floppy_insert(&mut self, drive: usize, path: &std::path::Path, write_protect: bool) -> bool;

    /// Eject a floppy disk
    fn floppy_eject(&mut self, drive: usize) -> bool;

    /// Insert a CD-ROM
    fn cdrom_insert(&mut self, id: usize, path: &std::path::Path) -> bool;

    /// Eject a CD-ROM (not implemented in most emulators - usually done via OS)
    fn cdrom_eject(&mut self, id: usize) -> bool;

    /// Attach a SCSI HDD
    fn scsi_attach_hdd(&mut self, id: usize, path: &std::path::Path) -> bool;

    /// Attach a SCSI CD-ROM drive
    fn scsi_attach_cdrom(&mut self, id: usize) -> bool;

    /// Detach a SCSI target
    fn scsi_detach(&mut self, id: usize) -> bool;

    // Debugger stepping
    fn debugger_step(&mut self) -> bool;
    fn debugger_step_out(&mut self) -> bool;
    fn debugger_step_over(&mut self) -> bool;

    // Breakpoints
    fn breakpoint_set(&mut self, address: u32, bp_type: BreakpointType) -> bool;
    fn breakpoint_list(&self) -> Vec<BreakpointInfo>;
    fn breakpoint_remove(&mut self, address: u32, bp_type: BreakpointType) -> bool;
    fn breakpoint_toggle(&mut self, address: u32, bp_type: BreakpointType) -> (bool, bool);

    // Memory access
    fn memory_read(&self, address: u32, length: usize) -> Option<Vec<u8>>;
    fn memory_write(&mut self, address: u32, data: &[u8]) -> bool;

    // Register access
    fn registers_get(&self) -> Option<RegistersGetResult>;
    fn register_get(&self, name: &str) -> Option<u32>;
    fn register_set(&mut self, name: &str, value: u32) -> bool;

    // Disassembly
    fn disassembly_get(&self, address: Option<u32>, count: usize) -> Vec<DisassemblyEntry>;

    // Audio
    fn audio_get_mute(&self) -> bool;
    fn audio_set_mute(&mut self, muted: bool) -> bool;

    // Recording
    fn recording_start(&mut self, path: Option<std::path::PathBuf>) -> Option<std::path::PathBuf>;
    fn recording_stop(&mut self) -> bool;
    fn recording_is_active(&self) -> bool;
    fn recording_get_path(&self) -> Option<std::path::PathBuf>;
    fn recording_play(&mut self, path: &std::path::Path) -> bool;

    // History
    fn history_instruction_enable(&mut self, enabled: bool) -> bool;
    fn history_instruction_is_enabled(&self) -> bool;
    fn history_instruction_get(&self, count: Option<usize>) -> Vec<InstructionHistoryEntry>;
    fn history_systrap_enable(&mut self, enabled: bool) -> bool;
    fn history_systrap_is_enabled(&self) -> bool;
    fn history_systrap_get(&self, count: Option<usize>) -> Vec<SystrapHistoryEntry>;

    // Peripheral debug
    fn peripheral_debug_enable(&mut self, enabled: bool) -> bool;
    fn peripheral_debug_is_enabled(&self) -> bool;
    fn peripheral_state_get(&self) -> Vec<PeripheralInfo>;

    // Misc
    fn emulator_get_cycles(&self) -> Option<u64>;
    fn emulator_programmer_key(&mut self) -> bool;
    fn input_release_all(&mut self) -> bool;

    /// Handle the RPC request
    fn handle_request(&mut self, request: &RpcRequest) -> RpcResponse {
        let method = request.method.as_str();
        let id = request.id.clone();

        match method {
            "status.get" => self.handle_status_get(id),
            "config.set_shared_dir" => self.handle_config_set_shared_dir(id, &request.params),
            "config.serial.get" => self.handle_config_serial_get(id, &request.params),
            "config.serial.enable" => self.handle_config_serial_enable(id, &request.params),
            "config.serial.disable" => self.handle_config_serial_disable(id, &request.params),
            "speed.get" => self.handle_speed_get(id),
            "speed.set" => self.handle_speed_set(id, &request.params),
            "mouse.get_position" => self.handle_mouse_get_position(id),
            "mouse.set_position" => self.handle_mouse_set_position(id, &request.params),
            "mouse.move" => self.handle_mouse_move(id, &request.params),
            "mouse.click" => self.handle_mouse_click(id, &request.params),
            "mouse.button" => self.handle_mouse_button(id, &request.params),
            "keyboard.type" => self.handle_keyboard_type(id, &request.params),
            "keyboard.combo" => self.handle_keyboard_combo(id, &request.params),
            "keyboard.key" => self.handle_keyboard_key(id, &request.params),
            "keyboard.release_all" => self.handle_keyboard_release_all(id),
            "emulator.run" => self.handle_emulator_run(id),
            "emulator.stop" => self.handle_emulator_stop(id),
            "emulator.reset" => self.handle_emulator_reset(id),
            "screenshot.get" => self.handle_screenshot_get(id, &request.params),
            "screenshot.save" => self.handle_screenshot_save(id, &request.params),
            "floppy.insert" => self.handle_floppy_insert(id, &request.params),
            "floppy.eject" => self.handle_floppy_eject(id, &request.params),
            "cdrom.insert" => self.handle_cdrom_insert(id, &request.params),
            "cdrom.eject" => self.handle_cdrom_eject(id, &request.params),
            "scsi.attach_hdd" => self.handle_scsi_attach_hdd(id, &request.params),
            "scsi.attach_cdrom" => self.handle_scsi_attach_cdrom(id, &request.params),
            "scsi.detach" => self.handle_scsi_detach(id, &request.params),
            // Debugger stepping
            "debugger.step" => self.handle_debugger_step(id),
            "debugger.step_out" => self.handle_debugger_step_out(id),
            "debugger.step_over" => self.handle_debugger_step_over(id),
            // Breakpoints
            "debugger.breakpoint.set" => self.handle_breakpoint_set(id, &request.params),
            "debugger.breakpoint.list" => self.handle_breakpoint_list(id),
            "debugger.breakpoint.remove" => self.handle_breakpoint_remove(id, &request.params),
            "debugger.breakpoint.toggle" => self.handle_breakpoint_toggle(id, &request.params),
            // Memory access
            "memory.read" => self.handle_memory_read(id, &request.params),
            "memory.write" => self.handle_memory_write(id, &request.params),
            // Register access
            "registers.get" => self.handle_registers_get(id, &request.params),
            "registers.set" => self.handle_registers_set(id, &request.params),
            // Disassembly
            "disassembly.get" => self.handle_disassembly_get(id, &request.params),
            // Audio
            "audio.get_mute" => self.handle_audio_get_mute(id),
            "audio.set_mute" => self.handle_audio_set_mute(id, &request.params),
            // Recording
            "recording.start" => self.handle_recording_start(id, &request.params),
            "recording.stop" => self.handle_recording_stop(id),
            "recording.status" => self.handle_recording_status(id),
            "recording.play" => self.handle_recording_play(id, &request.params),
            // History
            "history.instruction.enable" => {
                self.handle_history_instruction_enable(id, &request.params)
            }
            "history.instruction.get" => self.handle_history_instruction_get(id, &request.params),
            "history.systrap.enable" => self.handle_history_systrap_enable(id, &request.params),
            "history.systrap.get" => self.handle_history_systrap_get(id, &request.params),
            // Peripheral debug
            "peripheral.enable_debug" => self.handle_peripheral_debug_enable(id, &request.params),
            "peripheral.get_state" => self.handle_peripheral_state_get(id),
            // Misc
            "emulator.get_cycles" => self.handle_emulator_get_cycles(id),
            "emulator.programmer_key" => self.handle_emulator_programmer_key(id),
            "input.release_all" => self.handle_input_release_all(id),
            _ => RpcResponse::method_not_found(id),
        }
    }

    fn handle_status_get(&self, id: Option<serde_json::Value>) -> RpcResponse {
        match self.get_status() {
            Some(status) => RpcResponse::success(id, status),
            None => RpcResponse::internal_error(id, "Emulator not initialized"),
        }
    }

    fn handle_config_set_shared_dir(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: ConfigSetSharedDirParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let success = self.set_shared_dir(params.path);
        RpcResponse::success(id, ConfigSetSharedDirResult { success })
    }

    fn handle_config_serial_get(
        &self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: ConfigSerialGetParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let channel = match params.channel {
            SerialChannel::A => SccCh::A,
            SerialChannel::B => SccCh::B,
        };

        let enabled = self.is_serial_enabled(channel);
        let status = self.get_serial_status(channel);

        RpcResponse::success(id, ConfigSerialGetResult { enabled, status })
    }

    fn handle_config_serial_enable(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: ConfigSerialEnableParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let channel = match params.channel {
            SerialChannel::A => SccCh::A,
            SerialChannel::B => SccCh::B,
        };

        let config = match params.mode {
            SerialMode::Pty => SerialBridgeConfig::Pty,
            SerialMode::Tcp => {
                let port = params.port.unwrap_or(0);
                if port == 0 {
                    return RpcResponse::invalid_params(id, "TCP mode requires port parameter");
                }
                SerialBridgeConfig::Tcp(port)
            }
            SerialMode::Localtalk => SerialBridgeConfig::LocalTalk,
        };

        match self.enable_serial(channel, config) {
            Ok(status) => RpcResponse::success(
                id,
                ConfigSerialEnableResult {
                    success: true,
                    status: Some(status),
                    error: None,
                },
            ),
            Err(e) => RpcResponse::success(
                id,
                ConfigSerialEnableResult {
                    success: false,
                    status: None,
                    error: Some(e),
                },
            ),
        }
    }

    fn handle_config_serial_disable(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: ConfigSerialDisableParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let channel = match params.channel {
            SerialChannel::A => SccCh::A,
            SerialChannel::B => SccCh::B,
        };

        let success = self.disable_serial(channel);
        RpcResponse::success(id, ConfigSerialDisableResult { success })
    }

    fn handle_speed_get(&self, id: Option<serde_json::Value>) -> RpcResponse {
        match self.get_speed() {
            Some((speed, effective)) => RpcResponse::success(
                id,
                SpeedGetResult {
                    mode: format!("{:?}", speed),
                    effective_speed: effective,
                },
            ),
            None => RpcResponse::internal_error(id, "Emulator not initialized"),
        }
    }

    fn handle_speed_set(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: SpeedSetParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let speed = match params.mode {
            SpeedMode::Accurate => EmulatorSpeed::Accurate,
            SpeedMode::Uncapped => EmulatorSpeed::Uncapped,
            SpeedMode::Video => EmulatorSpeed::Video,
        };

        match self.set_speed(speed) {
            Some(prev) => RpcResponse::success(
                id,
                SpeedSetResult {
                    success: true,
                    previous: format!("{:?}", prev),
                },
            ),
            None => RpcResponse::internal_error(id, "Emulator not initialized"),
        }
    }

    fn handle_mouse_get_position(&self, id: Option<serde_json::Value>) -> RpcResponse {
        match self.get_mouse_position() {
            Some((x, y)) => RpcResponse::success(id, MouseGetPositionResult { x, y }),
            None => RpcResponse::internal_error(id, "Mouse position not available"),
        }
    }

    fn handle_mouse_set_position(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: MouseSetPositionParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let success = self.set_mouse_position(params.x, params.y);
        RpcResponse::success(id, MouseSetPositionResult { success })
    }

    fn handle_mouse_move(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: MouseMoveParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let success = self.move_mouse(params.dx, params.dy);
        RpcResponse::success(id, MouseMoveResult { success })
    }

    fn handle_mouse_click(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: MouseClickParams = serde_json::from_value(params.clone()).unwrap_or_default();

        // Move to position if specified
        if let (Some(x), Some(y)) = (params.x, params.y) {
            self.set_mouse_position(x, y);
        }

        // Click: press and release
        self.set_mouse_button(true);
        std::thread::sleep(std::time::Duration::from_millis(50));
        self.set_mouse_button(false);

        RpcResponse::success(id, MouseClickResult { success: true })
    }

    fn handle_mouse_button(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: MouseButtonParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let down = params.state == ButtonState::Down;
        let success = self.set_mouse_button(down);
        RpcResponse::success(id, MouseButtonResult { success })
    }

    fn handle_keyboard_type(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: KeyboardTypeParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let delay = std::time::Duration::from_millis(params.delay_ms);

        for c in params.text.chars() {
            if let Some((scancode, shift)) = char_to_scancode(c) {
                if shift {
                    self.send_key_event(KeyEvent::KeyDown(0x38, Keymap::Universal));
                    // Shift down
                }
                self.send_key_event(KeyEvent::KeyDown(scancode, Keymap::Universal));
                if params.delay_ms > 0 {
                    std::thread::sleep(delay);
                }
                self.send_key_event(KeyEvent::KeyUp(scancode, Keymap::Universal));
                if shift {
                    self.send_key_event(KeyEvent::KeyUp(0x38, Keymap::Universal));
                    // Shift up
                }
                if params.delay_ms > 0 {
                    std::thread::sleep(delay);
                }
            } else {
                debug!("Unknown character in keyboard.type: {:?}", c);
            }
        }

        RpcResponse::success(id, KeyboardTypeResult { success: true })
    }

    fn handle_keyboard_combo(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: KeyboardComboParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let delay = std::time::Duration::from_millis(params.delay_ms);

        // Resolve all keys first
        let scancodes: Vec<Scancode> = params.keys.iter().filter_map(|k| resolve_key(k)).collect();

        if scancodes.len() != params.keys.len() {
            return RpcResponse::invalid_params(id, "Unknown key name in combo");
        }

        // Press all keys
        for &scancode in &scancodes {
            self.send_key_event(KeyEvent::KeyDown(scancode, Keymap::Universal));
            if params.delay_ms > 0 {
                std::thread::sleep(delay);
            }
        }

        // Release all keys in reverse order
        for &scancode in scancodes.iter().rev() {
            self.send_key_event(KeyEvent::KeyUp(scancode, Keymap::Universal));
            if params.delay_ms > 0 {
                std::thread::sleep(delay);
            }
        }

        RpcResponse::success(id, KeyboardComboResult { success: true })
    }

    fn handle_keyboard_key(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: KeyboardKeyParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let scancode = match params.key {
            KeySpec::Name(ref name) => match resolve_key(name) {
                Some(sc) => sc,
                None => return RpcResponse::invalid_params(id, "Unknown key name"),
            },
            KeySpec::Scancode(sc) => sc,
        };

        let event = match params.state {
            ButtonState::Down => KeyEvent::KeyDown(scancode, Keymap::Universal),
            ButtonState::Up => KeyEvent::KeyUp(scancode, Keymap::Universal),
        };

        self.send_key_event(event);
        RpcResponse::success(id, KeyboardKeyResult { success: true })
    }

    fn handle_keyboard_release_all(&mut self, id: Option<serde_json::Value>) -> RpcResponse {
        self.release_all_keys();
        RpcResponse::success(id, KeyboardReleaseAllResult { success: true })
    }

    fn handle_emulator_run(&mut self, id: Option<serde_json::Value>) -> RpcResponse {
        let success = self.emulator_run();
        RpcResponse::success(id, EmulatorControlResult { success })
    }

    fn handle_emulator_stop(&mut self, id: Option<serde_json::Value>) -> RpcResponse {
        let success = self.emulator_stop();
        RpcResponse::success(id, EmulatorControlResult { success })
    }

    fn handle_emulator_reset(&mut self, id: Option<serde_json::Value>) -> RpcResponse {
        let success = self.emulator_reset();
        RpcResponse::success(id, EmulatorControlResult { success })
    }

    #[allow(clippy::significant_drop_tightening)]
    fn handle_screenshot_get(
        &self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: ScreenshotGetParams =
            serde_json::from_value(params.clone()).unwrap_or_default();

        let frame_buffer = match self.get_frame_buffer() {
            Some(fb) => fb,
            None => return RpcResponse::internal_error(id, "Frame buffer not available"),
        };

        let guard = match frame_buffer.lock() {
            Ok(g) => g,
            Err(_) => return RpcResponse::internal_error(id, "Failed to lock frame buffer"),
        };

        let buffer = match guard.as_ref() {
            Some(b) => b,
            None => return RpcResponse::internal_error(id, "No frame captured yet"),
        };

        let width = buffer.width();
        let height = buffer.height();

        let (data, format_str) = match params.format {
            ScreenshotFormat::RawRgba => {
                let data = base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    buffer.as_ref(),
                );
                (data, "raw_rgba")
            }
            ScreenshotFormat::Png => match encode_png(buffer) {
                Ok(png_data) => {
                    let data = base64::Engine::encode(
                        &base64::engine::general_purpose::STANDARD,
                        &png_data,
                    );
                    (data, "png")
                }
                Err(e) => {
                    return RpcResponse::internal_error(id, &format!("PNG encoding failed: {}", e))
                }
            },
        };

        RpcResponse::success(
            id,
            ScreenshotGetResult {
                width,
                height,
                data,
                format: format_str.to_string(),
            },
        )
    }

    #[allow(clippy::significant_drop_tightening)]
    fn handle_screenshot_save(
        &self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: ScreenshotSaveParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let frame_buffer = match self.get_frame_buffer() {
            Some(fb) => fb,
            None => return RpcResponse::internal_error(id, "Frame buffer not available"),
        };

        let guard = match frame_buffer.lock() {
            Ok(g) => g,
            Err(_) => return RpcResponse::internal_error(id, "Failed to lock frame buffer"),
        };

        let buffer = match guard.as_ref() {
            Some(b) => b,
            None => return RpcResponse::internal_error(id, "No frame captured yet"),
        };

        match save_png(buffer, &params.path) {
            Ok(_) => RpcResponse::success(
                id,
                ScreenshotSaveResult {
                    success: true,
                    path: params.path,
                },
            ),
            Err(e) => RpcResponse::internal_error(id, &format!("Failed to save PNG: {}", e)),
        }
    }

    fn handle_floppy_insert(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: FloppyInsertParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let success = self.floppy_insert(params.drive, &params.path, params.write_protect);
        RpcResponse::success(id, FloppyInsertResult { success })
    }

    fn handle_floppy_eject(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: FloppyEjectParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let success = self.floppy_eject(params.drive);
        RpcResponse::success(id, FloppyEjectResult { success })
    }

    fn handle_cdrom_insert(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: CdromInsertParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let success = self.cdrom_insert(params.id, &params.path);
        RpcResponse::success(id, CdromInsertResult { success })
    }

    fn handle_cdrom_eject(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: CdromEjectParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let success = self.cdrom_eject(params.id);
        RpcResponse::success(id, CdromEjectResult { success })
    }

    fn handle_scsi_attach_hdd(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: ScsiAttachHddParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let success = self.scsi_attach_hdd(params.id, &params.path);
        RpcResponse::success(id, ScsiAttachHddResult { success })
    }

    fn handle_scsi_attach_cdrom(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: ScsiAttachCdromParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let success = self.scsi_attach_cdrom(params.id);
        RpcResponse::success(id, ScsiAttachCdromResult { success })
    }

    fn handle_scsi_detach(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: ScsiDetachParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let success = self.scsi_detach(params.id);
        RpcResponse::success(id, ScsiDetachResult { success })
    }

    // Debugger stepping handlers

    fn handle_debugger_step(&mut self, id: Option<serde_json::Value>) -> RpcResponse {
        let success = self.debugger_step();
        RpcResponse::success(id, DebuggerStepResult { success })
    }

    fn handle_debugger_step_out(&mut self, id: Option<serde_json::Value>) -> RpcResponse {
        let success = self.debugger_step_out();
        RpcResponse::success(id, DebuggerStepResult { success })
    }

    fn handle_debugger_step_over(&mut self, id: Option<serde_json::Value>) -> RpcResponse {
        let success = self.debugger_step_over();
        RpcResponse::success(id, DebuggerStepResult { success })
    }

    // Breakpoint handlers

    fn handle_breakpoint_set(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: BreakpointSetParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let success = self.breakpoint_set(params.address, params.bp_type);
        RpcResponse::success(id, BreakpointSetResult { success })
    }

    fn handle_breakpoint_list(&self, id: Option<serde_json::Value>) -> RpcResponse {
        let breakpoints = self.breakpoint_list();
        RpcResponse::success(id, BreakpointListResult { breakpoints })
    }

    fn handle_breakpoint_remove(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: BreakpointRemoveParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let success = self.breakpoint_remove(params.address, params.bp_type);
        RpcResponse::success(id, BreakpointRemoveResult { success })
    }

    fn handle_breakpoint_toggle(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: BreakpointToggleParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let (success, enabled) = self.breakpoint_toggle(params.address, params.bp_type);
        RpcResponse::success(id, BreakpointToggleResult { success, enabled })
    }

    // Memory access handlers

    fn handle_memory_read(
        &self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: MemoryReadParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        match self.memory_read(params.address, params.length) {
            Some(data) => {
                let encoded =
                    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data);
                RpcResponse::success(
                    id,
                    MemoryReadResult {
                        address: params.address,
                        data: encoded,
                        length: data.len(),
                    },
                )
            }
            None => RpcResponse::internal_error(id, "Memory read failed"),
        }
    }

    fn handle_memory_write(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: MemoryWriteParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let bytes_written = params.data.len();
        let success = self.memory_write(params.address, &params.data);
        RpcResponse::success(
            id,
            MemoryWriteResult {
                success,
                bytes_written,
            },
        )
    }

    // Register access handlers

    fn handle_registers_get(
        &self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: RegistersGetParams = serde_json::from_value(params.clone()).unwrap_or_default();

        if let Some(ref reg_name) = params.register {
            // Get a single register
            match self.register_get(reg_name) {
                Some(value) => RpcResponse::success(
                    id,
                    RegisterGetSingleResult {
                        register: reg_name.clone(),
                        value,
                    },
                ),
                None => RpcResponse::invalid_params(id, &format!("Unknown register: {}", reg_name)),
            }
        } else {
            // Get all registers
            match self.registers_get() {
                Some(regs) => RpcResponse::success(id, regs),
                None => RpcResponse::internal_error(id, "Emulator not initialized"),
            }
        }
    }

    fn handle_registers_set(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: RegistersSetParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let success = self.register_set(&params.register, params.value);
        if success {
            RpcResponse::success(id, RegistersSetResult { success })
        } else {
            RpcResponse::invalid_params(id, &format!("Unknown register: {}", params.register))
        }
    }

    // Disassembly handler

    fn handle_disassembly_get(
        &self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: DisassemblyGetParams =
            serde_json::from_value(params.clone()).unwrap_or(DisassemblyGetParams {
                address: None,
                count: 20,
            });

        let entries = self.disassembly_get(params.address, params.count);
        RpcResponse::success(id, DisassemblyGetResult { entries })
    }

    // Audio handlers

    fn handle_audio_get_mute(&self, id: Option<serde_json::Value>) -> RpcResponse {
        let muted = self.audio_get_mute();
        RpcResponse::success(id, AudioGetMuteResult { muted })
    }

    fn handle_audio_set_mute(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: AudioSetMuteParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let success = self.audio_set_mute(params.muted);
        let muted = self.audio_get_mute();
        RpcResponse::success(id, AudioSetMuteResult { success, muted })
    }

    // Recording handlers

    fn handle_recording_start(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: RecordingStartParams =
            serde_json::from_value(params.clone()).unwrap_or(RecordingStartParams { path: None });

        match self.recording_start(params.path) {
            Some(path) => RpcResponse::success(
                id,
                RecordingStartResult {
                    success: true,
                    path,
                },
            ),
            None => RpcResponse::internal_error(id, "Failed to start recording"),
        }
    }

    fn handle_recording_stop(&mut self, id: Option<serde_json::Value>) -> RpcResponse {
        let success = self.recording_stop();
        RpcResponse::success(id, RecordingStopResult { success })
    }

    fn handle_recording_status(&self, id: Option<serde_json::Value>) -> RpcResponse {
        let recording = self.recording_is_active();
        let path = self.recording_get_path();
        RpcResponse::success(id, RecordingStatusResult { recording, path })
    }

    fn handle_recording_play(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: RecordingPlayParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let success = self.recording_play(&params.path);
        RpcResponse::success(id, RecordingPlayResult { success })
    }

    // History handlers

    fn handle_history_instruction_enable(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: HistoryEnableParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let success = self.history_instruction_enable(params.enabled);
        let enabled = self.history_instruction_is_enabled();
        RpcResponse::success(id, HistoryEnableResult { success, enabled })
    }

    fn handle_history_instruction_get(
        &self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: HistoryGetParams = serde_json::from_value(params.clone()).unwrap_or_default();

        let entries = self.history_instruction_get(params.count);
        let enabled = self.history_instruction_is_enabled();
        RpcResponse::success(id, InstructionHistoryGetResult { entries, enabled })
    }

    fn handle_history_systrap_enable(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: HistoryEnableParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let success = self.history_systrap_enable(params.enabled);
        let enabled = self.history_systrap_is_enabled();
        RpcResponse::success(id, HistoryEnableResult { success, enabled })
    }

    fn handle_history_systrap_get(
        &self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: HistoryGetParams = serde_json::from_value(params.clone()).unwrap_or_default();

        let entries = self.history_systrap_get(params.count);
        let enabled = self.history_systrap_is_enabled();
        RpcResponse::success(id, SystrapHistoryGetResult { entries, enabled })
    }

    // Peripheral debug handlers

    fn handle_peripheral_debug_enable(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> RpcResponse {
        let params: PeripheralDebugEnableParams = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(e) => return RpcResponse::invalid_params(id, &e.to_string()),
        };

        let success = self.peripheral_debug_enable(params.enabled);
        let enabled = self.peripheral_debug_is_enabled();
        RpcResponse::success(id, PeripheralDebugEnableResult { success, enabled })
    }

    fn handle_peripheral_state_get(&self, id: Option<serde_json::Value>) -> RpcResponse {
        let enabled = self.peripheral_debug_is_enabled();
        let peripherals = self.peripheral_state_get();
        RpcResponse::success(
            id,
            PeripheralStateResult {
                enabled,
                peripherals,
            },
        )
    }

    // Misc handlers

    fn handle_emulator_get_cycles(&self, id: Option<serde_json::Value>) -> RpcResponse {
        match self.emulator_get_cycles() {
            Some(cycles) => RpcResponse::success(id, EmulatorCyclesResult { cycles }),
            None => RpcResponse::internal_error(id, "Emulator not initialized"),
        }
    }

    fn handle_emulator_programmer_key(&mut self, id: Option<serde_json::Value>) -> RpcResponse {
        let success = self.emulator_programmer_key();
        RpcResponse::success(id, EmulatorProgrammerKeyResult { success })
    }

    fn handle_input_release_all(&mut self, id: Option<serde_json::Value>) -> RpcResponse {
        let success = self.input_release_all();
        RpcResponse::success(id, SuccessResult { success })
    }
}

/// Encode a DisplayBuffer as PNG
fn encode_png(buffer: &DisplayBuffer) -> Result<Vec<u8>, png::EncodingError> {
    let mut output = Vec::new();
    {
        let mut encoder = png::Encoder::new(
            Cursor::new(&mut output),
            buffer.width() as u32,
            buffer.height() as u32,
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(buffer.as_ref())?;
    }
    Ok(output)
}

/// Save a DisplayBuffer as a PNG file
fn save_png(
    buffer: &DisplayBuffer,
    path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let file = std::fs::File::create(path)?;
    let buf_writer = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(buf_writer, buffer.width() as u32, buffer.height() as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(buffer.as_ref())?;
    Ok(())
}
