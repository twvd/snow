//! RPC integration for the Snow frontend
//!
//! This module provides the RPC server integration and implements the RpcHandler trait
//! for the emulator state.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use snow_core::emulator::comm::EmulatorSpeed;
use snow_core::keymap::KeyEvent;
use snow_core::mac::scc::SccCh;
use snow_core::mac::serial_bridge::SerialBridgeConfig;
use snow_core::renderer::DisplayBuffer;
use snow_core::rpc::{
    BreakpointInfo, BreakpointType, DisassemblyEntry, FloppyInfo, InstructionHistoryEntry,
    PeripheralInfo, PropertyInfo, RegistersGetResult, RpcConfig, RpcHandler, RpcMessage, RpcServer,
    ScreenInfo, ScsiTargetInfo, SerialBridgeStatusInfo, SerialPortInfo, StatusGetResult,
    SystrapHistoryEntry,
};

use crate::emulator::EmulatorState;

/// Wrapper around EmulatorState that implements RpcHandler
pub struct EmulatorRpcHandler<'a> {
    state: &'a mut EmulatorState,
    frame_buffer: Arc<Mutex<Option<DisplayBuffer>>>,
    shared_dir: Option<PathBuf>,
}

impl<'a> EmulatorRpcHandler<'a> {
    pub fn new(
        state: &'a mut EmulatorState,
        frame_buffer: Arc<Mutex<Option<DisplayBuffer>>>,
        shared_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            state,
            frame_buffer,
            shared_dir,
        }
    }
}

impl RpcHandler for EmulatorRpcHandler<'_> {
    fn get_status(&self) -> Option<StatusGetResult> {
        use snow_core::cpu_m68k::M68000;

        let model = self.state.get_model()?;

        // Get FDD status
        let mut floppy = Vec::new();
        for drive in 0..3 {
            if let Some(status) = self.state.get_fdd_status(drive) {
                floppy.push(FloppyInfo {
                    drive,
                    present: status.present,
                    ejected: status.ejected,
                    motor: status.motor,
                    writing: status.writing,
                    track: status.track,
                    image_title: status.image_title.clone(),
                    dirty: status.dirty,
                });
            }
        }

        // Get SCSI status
        let mut scsi = Vec::new();
        if let Some(targets) = self.state.get_scsi_target_status() {
            for (id, target) in targets.iter().enumerate() {
                scsi.push(ScsiTargetInfo {
                    id,
                    target_type: target.as_ref().map(|t| format!("{:?}", t.target_type)),
                    image: target.as_ref().and_then(|t| t.image.clone()),
                    capacity: target.as_ref().and_then(|t| t.capacity),
                });
            }
        }

        // Get serial port status
        let serial = vec![
            SerialPortInfo {
                channel: "A".to_string(),
                enabled: self.state.is_serial_bridge_enabled(SccCh::A),
                status: self
                    .state
                    .get_serial_bridge_status(SccCh::A)
                    .map(|s| format!("{}", s)),
            },
            SerialPortInfo {
                channel: "B".to_string(),
                enabled: self.state.is_serial_bridge_enabled(SccCh::B),
                status: self
                    .state
                    .get_serial_bridge_status(SccCh::B)
                    .map(|s| format!("{}", s)),
            },
        ];

        // Get screen info based on CPU type
        let screen = if model.cpu_type() == M68000 {
            // Compact Mac - fixed resolution 512x342 B&W
            ScreenInfo {
                width: 512,
                height: 342,
                color: false,
            }
        } else {
            // Mac II series - get from configured monitor
            let monitor = self.state.get_monitor().unwrap_or_default();
            ScreenInfo {
                width: monitor.width() as u32,
                height: monitor.height() as u32,
                color: monitor.has_color(),
            }
        };

        // Get RAM size - use configured size or model default
        let ram_bytes = self
            .state
            .get_ram_size()
            .unwrap_or_else(|| model.ram_size_default());
        let ram_mb = (ram_bytes / 1024 / 1024) as u32;

        // Format CPU type as "M68000", "M68020", "M68030"
        let cpu_type = format!("M{}", model.cpu_type());

        Some(StatusGetResult {
            running: self.state.is_running(),
            model: format!("{}", model),
            cpu_type,
            ram_mb,
            screen,
            has_adb: model.has_adb(),
            has_scsi: model.has_scsi(),
            hd_floppy: model.fdd_hd(),
            speed: if self.state.is_fastforward() {
                "Uncapped".to_string()
            } else {
                "Accurate".to_string()
            },
            effective_speed: 1.0, // TODO: get actual effective speed
            cycles: self.state.get_cycles() as u64,
            scsi,
            floppy,
            serial,
            shared_dir: self.shared_dir.clone(),
        })
    }

    fn get_shared_dir(&self) -> Option<PathBuf> {
        self.shared_dir.clone()
    }

    fn set_shared_dir(&mut self, path: Option<PathBuf>) -> bool {
        self.state.set_shared_dir(path);
        true
    }

    fn get_speed(&self) -> Option<(EmulatorSpeed, f64)> {
        if !self.state.is_initialized() {
            return None;
        }
        let speed = if self.state.is_fastforward() {
            EmulatorSpeed::Uncapped
        } else {
            EmulatorSpeed::Accurate
        };
        Some((speed, 1.0)) // TODO: get actual effective speed
    }

    fn set_speed(&mut self, speed: EmulatorSpeed) -> Option<EmulatorSpeed> {
        if !self.state.is_initialized() {
            return None;
        }
        let prev = if self.state.is_fastforward() {
            EmulatorSpeed::Uncapped
        } else {
            EmulatorSpeed::Accurate
        };

        match speed {
            EmulatorSpeed::Uncapped => {
                if !self.state.is_fastforward() {
                    self.state.toggle_fastforward();
                }
            }
            EmulatorSpeed::Accurate | EmulatorSpeed::Video | EmulatorSpeed::Dynamic => {
                if self.state.is_fastforward() {
                    self.state.toggle_fastforward();
                }
            }
        }

        Some(prev)
    }

    fn emulator_run(&mut self) -> bool {
        if self.state.is_initialized() {
            self.state.run();
            true
        } else {
            false
        }
    }

    fn emulator_stop(&mut self) -> bool {
        if self.state.is_initialized() {
            self.state.stop();
            true
        } else {
            false
        }
    }

    fn emulator_reset(&mut self) -> bool {
        if self.state.is_initialized() {
            self.state.reset();
            true
        } else {
            false
        }
    }

    fn get_mouse_position(&self) -> Option<(u16, u16)> {
        // Mouse position is not directly tracked in the current implementation
        // This would require extending EmulatorState to track the last known position
        None
    }

    fn set_mouse_position(&mut self, x: u16, y: u16) -> bool {
        if !self.state.is_initialized() || !self.state.is_running() {
            return false;
        }

        use eframe::egui::Pos2;
        let pos = Pos2::new(x as f32, y as f32);
        self.state.update_mouse(Some(&pos), &Pos2::ZERO);
        true
    }

    fn move_mouse(&mut self, dx: i16, dy: i16) -> bool {
        if !self.state.is_initialized() || !self.state.is_running() {
            return false;
        }

        use eframe::egui::Pos2;
        let rel = Pos2::new(dx as f32, dy as f32);
        self.state.update_mouse(None, &rel);
        true
    }

    fn set_mouse_button(&mut self, down: bool) -> bool {
        if !self.state.is_initialized() || !self.state.is_running() {
            return false;
        }

        self.state.update_mouse_button(down);
        true
    }

    fn send_key_event(&mut self, event: KeyEvent) {
        if !self.state.is_initialized() || !self.state.is_running() {
            return;
        }

        match event {
            KeyEvent::KeyDown(scancode, _) => {
                self.state.update_key(scancode, true);
            }
            KeyEvent::KeyUp(scancode, _) => {
                self.state.update_key(scancode, false);
            }
        }
    }

    fn release_all_keys(&mut self) {
        if self.state.is_initialized() {
            self.state.release_all_inputs();
        }
    }

    fn get_serial_status(&self, channel: SccCh) -> Option<String> {
        self.state
            .get_serial_bridge_status(channel)
            .map(|s| format!("{}", s))
    }

    fn enable_serial(
        &mut self,
        channel: SccCh,
        config: SerialBridgeConfig,
    ) -> Result<SerialBridgeStatusInfo, String> {
        use snow_core::mac::serial_bridge::SerialBridgeStatus;

        let timeout = std::time::Duration::from_millis(500);
        let status = self
            .state
            .enable_serial_bridge_wait(channel, config, timeout)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Bridge enabled but status unavailable (timeout)".to_string())?;

        Ok(match status {
            SerialBridgeStatus::Pty(path) => SerialBridgeStatusInfo::Pty {
                path: path.display().to_string(),
            },
            SerialBridgeStatus::TcpListening(port) => SerialBridgeStatusInfo::Tcp {
                port,
                connected: None,
            },
            SerialBridgeStatus::TcpConnected(port, addr) => SerialBridgeStatusInfo::Tcp {
                port,
                connected: Some(addr),
            },
            SerialBridgeStatus::LocalTalk(lt_status) => SerialBridgeStatusInfo::LocalTalk {
                status: format!("{}", lt_status),
            },
        })
    }

    fn disable_serial(&mut self, channel: SccCh) -> bool {
        self.state.disable_serial_bridge(channel).is_ok()
    }

    fn is_serial_enabled(&self, channel: SccCh) -> bool {
        self.state.is_serial_bridge_enabled(channel)
    }

    fn get_frame_buffer(&self) -> Option<Arc<Mutex<Option<DisplayBuffer>>>> {
        Some(self.frame_buffer.clone())
    }

    fn floppy_insert(&mut self, drive: usize, path: &std::path::Path, write_protect: bool) -> bool {
        if !self.state.is_initialized() {
            return false;
        }
        self.state.load_floppy(drive, path, write_protect);
        true
    }

    fn floppy_eject(&mut self, drive: usize) -> bool {
        if !self.state.is_initialized() {
            return false;
        }
        self.state.force_eject(drive);
        true
    }

    fn cdrom_insert(&mut self, id: usize, path: &std::path::Path) -> bool {
        if !self.state.is_initialized() {
            return false;
        }
        self.state.scsi_load_cdrom(id, path);
        true
    }

    fn cdrom_eject(&mut self, _id: usize) -> bool {
        // CD-ROM ejection is typically done via the OS, not via external control
        // Return false as this isn't supported
        false
    }

    fn scsi_attach_hdd(&mut self, id: usize, path: &std::path::Path) -> bool {
        if !self.state.is_initialized() {
            return false;
        }
        self.state.scsi_attach_hdd(id, path);
        true
    }

    fn scsi_attach_cdrom(&mut self, id: usize) -> bool {
        if !self.state.is_initialized() {
            return false;
        }
        self.state.scsi_attach_cdrom(id);
        true
    }

    fn scsi_detach(&mut self, id: usize) -> bool {
        if !self.state.is_initialized() {
            return false;
        }
        self.state.scsi_detach_target(id);
        true
    }

    // Debugger stepping implementations

    fn debugger_step(&mut self) -> bool {
        if !self.state.is_initialized() {
            return false;
        }
        self.state.step();
        true
    }

    fn debugger_step_out(&mut self) -> bool {
        if !self.state.is_initialized() {
            return false;
        }
        self.state.step_out();
        true
    }

    fn debugger_step_over(&mut self) -> bool {
        if !self.state.is_initialized() {
            return false;
        }
        self.state.step_over();
        true
    }

    // Breakpoint implementations

    fn breakpoint_set(&mut self, address: u32, bp_type: BreakpointType) -> bool {
        if !self.state.is_initialized() {
            return false;
        }
        use snow_core::emulator::comm::{Breakpoint, BusBreakpoint};
        let bp = match bp_type {
            BreakpointType::Exec => Breakpoint::Execution(address),
            BreakpointType::Read => Breakpoint::Bus(BusBreakpoint::Read, address),
            BreakpointType::Write => Breakpoint::Bus(BusBreakpoint::Write, address),
        };
        self.state.set_breakpoint(bp);
        true
    }

    fn breakpoint_list(&self) -> Vec<BreakpointInfo> {
        if !self.state.is_initialized() {
            return Vec::new();
        }
        use snow_core::emulator::comm::{Breakpoint, BusBreakpoint};
        self.state
            .get_breakpoints()
            .iter()
            .filter_map(|bp| match bp {
                Breakpoint::Execution(addr) => Some(BreakpointInfo {
                    address: *addr,
                    bp_type: "exec".to_string(),
                }),
                Breakpoint::Bus(BusBreakpoint::Read, addr) => Some(BreakpointInfo {
                    address: *addr,
                    bp_type: "read".to_string(),
                }),
                Breakpoint::Bus(BusBreakpoint::Write, addr) => Some(BreakpointInfo {
                    address: *addr,
                    bp_type: "write".to_string(),
                }),
                Breakpoint::Bus(BusBreakpoint::ReadWrite, addr) => Some(BreakpointInfo {
                    address: *addr,
                    bp_type: "readwrite".to_string(),
                }),
                _ => None, // Ignore other breakpoint types (step over, step out, etc.)
            })
            .collect()
    }

    fn breakpoint_remove(&mut self, address: u32, bp_type: BreakpointType) -> bool {
        if !self.state.is_initialized() {
            return false;
        }
        use snow_core::emulator::comm::{Breakpoint, BusBreakpoint};
        let bp = match bp_type {
            BreakpointType::Exec => Breakpoint::Execution(address),
            BreakpointType::Read => Breakpoint::Bus(BusBreakpoint::Read, address),
            BreakpointType::Write => Breakpoint::Bus(BusBreakpoint::Write, address),
        };
        // Check if breakpoint exists
        if self.state.get_breakpoints().contains(&bp) {
            self.state.toggle_breakpoint(bp);
            true
        } else {
            false
        }
    }

    fn breakpoint_toggle(&mut self, address: u32, bp_type: BreakpointType) -> (bool, bool) {
        if !self.state.is_initialized() {
            return (false, false);
        }
        use snow_core::emulator::comm::{Breakpoint, BusBreakpoint};
        let bp = match bp_type {
            BreakpointType::Exec => Breakpoint::Execution(address),
            BreakpointType::Read => Breakpoint::Bus(BusBreakpoint::Read, address),
            BreakpointType::Write => Breakpoint::Bus(BusBreakpoint::Write, address),
        };
        let was_enabled = self.state.get_breakpoints().contains(&bp);
        self.state.toggle_breakpoint(bp);
        (true, !was_enabled)
    }

    // Memory access implementations

    fn memory_read(&self, address: u32, length: usize) -> Option<Vec<u8>> {
        // Memory read is not directly available without stopping emulator
        // This would require adding a method to EmulatorState
        // For now, return None (not implemented)
        let _ = (address, length);
        None
    }

    fn memory_write(&mut self, address: u32, data: &[u8]) -> bool {
        if !self.state.is_initialized() {
            return false;
        }
        for (i, &byte) in data.iter().enumerate() {
            self.state.write_bus(address + i as u32, byte);
        }
        true
    }

    // Register access implementations

    fn registers_get(&self) -> Option<RegistersGetResult> {
        if !self.state.is_initialized() {
            return None;
        }
        let regs = self.state.get_regs();
        Some(RegistersGetResult {
            d0: regs.d[0],
            d1: regs.d[1],
            d2: regs.d[2],
            d3: regs.d[3],
            d4: regs.d[4],
            d5: regs.d[5],
            d6: regs.d[6],
            d7: regs.d[7],
            a0: regs.a[0],
            a1: regs.a[1],
            a2: regs.a[2],
            a3: regs.a[3],
            a4: regs.a[4],
            a5: regs.a[5],
            a6: regs.a[6],
            a7: regs.read_a::<u32>(7),
            pc: regs.pc,
            sr: regs.sr.sr(),
            usp: regs.usp,
            ssp: *regs.ssp(),
        })
    }

    fn register_get(&self, name: &str) -> Option<u32> {
        if !self.state.is_initialized() {
            return None;
        }
        let regs = self.state.get_regs();
        let name_lower = name.to_lowercase();
        match name_lower.as_str() {
            "d0" => Some(regs.d[0]),
            "d1" => Some(regs.d[1]),
            "d2" => Some(regs.d[2]),
            "d3" => Some(regs.d[3]),
            "d4" => Some(regs.d[4]),
            "d5" => Some(regs.d[5]),
            "d6" => Some(regs.d[6]),
            "d7" => Some(regs.d[7]),
            "a0" => Some(regs.a[0]),
            "a1" => Some(regs.a[1]),
            "a2" => Some(regs.a[2]),
            "a3" => Some(regs.a[3]),
            "a4" => Some(regs.a[4]),
            "a5" => Some(regs.a[5]),
            "a6" => Some(regs.a[6]),
            "a7" | "sp" => Some(regs.read_a::<u32>(7)),
            "pc" => Some(regs.pc),
            "sr" => Some(regs.sr.sr() as u32),
            "usp" => Some(regs.usp),
            "ssp" => Some(*regs.ssp()),
            _ => None,
        }
    }

    fn register_set(&mut self, name: &str, value: u32) -> bool {
        use snow_core::cpu_m68k::regs::Register;
        if !self.state.is_initialized() {
            return false;
        }
        let name_lower = name.to_lowercase();
        let reg = match name_lower.as_str() {
            "d0" => Register::Dn(0),
            "d1" => Register::Dn(1),
            "d2" => Register::Dn(2),
            "d3" => Register::Dn(3),
            "d4" => Register::Dn(4),
            "d5" => Register::Dn(5),
            "d6" => Register::Dn(6),
            "d7" => Register::Dn(7),
            "a0" => Register::An(0),
            "a1" => Register::An(1),
            "a2" => Register::An(2),
            "a3" => Register::An(3),
            "a4" => Register::An(4),
            "a5" => Register::An(5),
            "a6" => Register::An(6),
            "a7" | "sp" => Register::An(7),
            "pc" => Register::PC,
            "sr" => Register::SR,
            "usp" => Register::USP,
            "ssp" => Register::SSP,
            _ => return false,
        };
        self.state.write_register(reg, value);
        true
    }

    // Disassembly implementation

    fn disassembly_get(&self, address: Option<u32>, count: usize) -> Vec<DisassemblyEntry> {
        if !self.state.is_initialized() {
            return Vec::new();
        }
        let _ = (address, count);
        // Use the current disassembly from the emulator state
        self.state
            .get_disassembly()
            .iter()
            .map(|entry| {
                // Parse the disassembly string to split mnemonic and operands
                let parts: Vec<&str> = entry.str.splitn(2, ' ').collect();
                let (mnemonic, operands) = if parts.len() > 1 {
                    (parts[0].to_string(), parts[1].to_string())
                } else {
                    (entry.str.clone(), String::new())
                };
                DisassemblyEntry {
                    address: entry.addr,
                    bytes: entry.raw_as_string(),
                    mnemonic,
                    operands,
                }
            })
            .collect()
    }

    // Audio implementations

    fn audio_get_mute(&self) -> bool {
        self.state.audio_is_muted()
    }

    fn audio_set_mute(&mut self, muted: bool) -> bool {
        self.state.audio_mute(muted);
        true
    }

    // Recording implementations

    fn recording_start(&mut self, path: Option<PathBuf>) -> Option<PathBuf> {
        if !self.state.is_initialized() || self.state.is_recording_input() {
            return None;
        }
        let path = path.unwrap_or_else(|| {
            std::env::temp_dir().join(format!("snow_recording_{}.json", std::process::id()))
        });
        self.state.record_input(&path);
        Some(path)
    }

    fn recording_stop(&mut self) -> bool {
        if !self.state.is_initialized() || !self.state.is_recording_input() {
            return false;
        }
        self.state.record_input_end();
        true
    }

    fn recording_is_active(&self) -> bool {
        self.state.is_recording_input()
    }

    fn recording_get_path(&self) -> Option<PathBuf> {
        // The path is stored internally in EmulatorState, not accessible externally
        // Would need to add a getter to EmulatorState
        None
    }

    fn recording_play(&mut self, path: &std::path::Path) -> bool {
        if !self.state.is_initialized() {
            return false;
        }
        self.state.replay_input(path).is_ok()
    }

    // History implementations

    fn history_instruction_enable(&mut self, enabled: bool) -> bool {
        if !self.state.is_initialized() {
            return false;
        }
        self.state.enable_history(enabled).is_ok()
    }

    fn history_instruction_is_enabled(&self) -> bool {
        self.state.is_history_enabled()
    }

    fn history_instruction_get(&self, count: Option<usize>) -> Vec<InstructionHistoryEntry> {
        use snow_core::cpu_m68k::cpu::HistoryEntry as CoreHistoryEntry;
        use snow_core::cpu_m68k::disassembler::Disassembler;

        let history = self.state.get_history();
        let mut entries: Vec<_> = history
            .iter()
            .rev()
            .take(count.unwrap_or(100))
            .map(|entry| match entry {
                CoreHistoryEntry::Instruction(inst) => {
                    // Disassemble the raw bytes
                    let disasm: Vec<_> =
                        Disassembler::from(&mut inst.raw.iter().copied(), inst.pc).collect();
                    let instruction = if let Some(d) = disasm.first() {
                        d.str.clone()
                    } else {
                        "???".to_string()
                    };
                    InstructionHistoryEntry {
                        address: inst.pc,
                        instruction,
                        cycles: inst.cycles as u64,
                    }
                }
                CoreHistoryEntry::Exception { vector, cycles } => InstructionHistoryEntry {
                    address: *vector,
                    instruction: format!("EXCEPTION vector={:#X}", vector),
                    cycles: *cycles as u64,
                },
                CoreHistoryEntry::Pagefault { address, write } => InstructionHistoryEntry {
                    address: *address,
                    instruction: format!(
                        "PAGEFAULT {} @ {:#X}",
                        if *write { "W" } else { "R" },
                        address
                    ),
                    cycles: 0,
                },
            })
            .collect();
        entries.reverse();
        entries
    }

    fn history_systrap_enable(&mut self, enabled: bool) -> bool {
        if !self.state.is_initialized() {
            return false;
        }
        self.state.enable_systrap_history(enabled).is_ok()
    }

    fn history_systrap_is_enabled(&self) -> bool {
        self.state.is_systrap_history_enabled()
    }

    fn history_systrap_get(&self, count: Option<usize>) -> Vec<SystrapHistoryEntry> {
        let history = self.state.get_systrap_history();
        let mut entries: Vec<_> = history
            .iter()
            .rev()
            .take(count.unwrap_or(100))
            .map(|entry| SystrapHistoryEntry {
                address: entry.pc,
                trap_word: entry.trap,
                trap_name: format!("${:04X}", entry.trap), // Could be enhanced with trap name lookup
                cycles: entry.cycles as u64,
            })
            .collect();
        entries.reverse();
        entries
    }

    // Peripheral debug implementations

    fn peripheral_debug_enable(&mut self, enabled: bool) -> bool {
        if !self.state.is_initialized() {
            return false;
        }
        self.state.enable_peripheral_debug(enabled).is_ok()
    }

    fn peripheral_debug_is_enabled(&self) -> bool {
        self.state.is_peripheral_debug_enabled()
    }

    fn peripheral_state_get(&self) -> Vec<PeripheralInfo> {
        use snow_core::debuggable::DebuggablePropertyValue;

        fn format_value(value: &DebuggablePropertyValue) -> String {
            match value {
                DebuggablePropertyValue::Header => String::new(),
                DebuggablePropertyValue::Nested(_) => "[nested]".to_string(),
                DebuggablePropertyValue::Boolean(v) => v.to_string(),
                DebuggablePropertyValue::Byte(v) => format!("${:02X}", v),
                DebuggablePropertyValue::ByteBinary(v) => format!("%{:08b}", v),
                DebuggablePropertyValue::Word(v) => format!("${:04X}", v),
                DebuggablePropertyValue::WordBinary(v) => format!("%{:016b}", v),
                DebuggablePropertyValue::Long(v) => format!("${:08X}", v),
                DebuggablePropertyValue::SignedDecimal(v) => v.to_string(),
                DebuggablePropertyValue::UnsignedDecimal(v) => v.to_string(),
                DebuggablePropertyValue::StaticStr(v) => v.to_string(),
                DebuggablePropertyValue::String(v) => v.clone(),
            }
        }

        let debug_props = self.state.get_peripheral_debug();
        debug_props
            .iter()
            .map(|prop| PeripheralInfo {
                name: prop.name().to_string(),
                properties: match prop.value() {
                    DebuggablePropertyValue::Nested(nested) => nested
                        .iter()
                        .map(|p| PropertyInfo {
                            key: p.name().to_string(),
                            value: format_value(p.value()),
                        })
                        .collect(),
                    _ => vec![PropertyInfo {
                        key: "value".to_string(),
                        value: format_value(prop.value()),
                    }],
                },
            })
            .collect()
    }

    // Misc implementations

    fn emulator_get_cycles(&self) -> Option<u64> {
        if !self.state.is_initialized() {
            return None;
        }
        Some(self.state.get_cycles() as u64)
    }

    fn emulator_programmer_key(&mut self) -> bool {
        if !self.state.is_initialized() {
            return false;
        }
        self.state.progkey();
        true
    }

    fn input_release_all(&mut self) -> bool {
        if !self.state.is_initialized() {
            return false;
        }
        self.state.release_all_inputs();
        true
    }
}

/// Fullscreen request from RPC
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullscreenRequest {
    Enter,
    Exit,
    Toggle,
}

/// RPC state that's stored in the App
pub struct RpcState {
    pub server: Option<RpcServer>,
    pub frame_buffer: Arc<Mutex<Option<DisplayBuffer>>>,
    pub config: RpcConfig,
    fullscreen_request: Option<FullscreenRequest>,
    fullscreen_state: bool,
}

impl Default for RpcState {
    fn default() -> Self {
        Self {
            server: None,
            frame_buffer: Arc::new(Mutex::new(None)),
            config: RpcConfig::default(),
            fullscreen_request: None,
            fullscreen_state: false,
        }
    }
}

impl RpcState {
    pub fn new(config: RpcConfig) -> Self {
        Self {
            server: None,
            frame_buffer: Arc::new(Mutex::new(None)),
            config,
            fullscreen_request: None,
            fullscreen_state: false,
        }
    }

    pub fn start(&mut self) -> anyhow::Result<()> {
        let mut server = RpcServer::new(self.config.clone());
        server.start()?;
        self.server = Some(server);
        Ok(())
    }

    #[allow(dead_code)]
    pub fn stop(&mut self) {
        if let Some(mut server) = self.server.take() {
            server.stop();
        }
    }

    /// Update the frame buffer with the latest frame
    pub fn update_frame_buffer(&self, buffer: &DisplayBuffer) {
        if let Ok(mut guard) = self.frame_buffer.lock() {
            // Create a copy of the buffer
            let mut new_buffer = DisplayBuffer::new(buffer.width(), buffer.height());
            new_buffer.copy_from_slice(buffer.as_ref());
            *guard = Some(new_buffer);
        }
    }

    /// Update the fullscreen state (call this from app when fullscreen changes)
    pub fn set_fullscreen_state(&mut self, fullscreen: bool) {
        self.fullscreen_state = fullscreen;
    }

    /// Take pending fullscreen request (returns None if no request pending)
    pub fn take_fullscreen_request(&mut self) -> Option<FullscreenRequest> {
        self.fullscreen_request.take()
    }

    /// Process pending RPC requests
    #[allow(clippy::needless_pass_by_value)]
    pub fn process_requests(&mut self, state: &mut EmulatorState, shared_dir: Option<PathBuf>) {
        let Some(ref server) = self.server else {
            return;
        };

        let request_rx = server.request_receiver();

        // Process all pending requests
        while let Ok(msg) = request_rx.try_recv() {
            match msg {
                RpcMessage::Request {
                    request,
                    response_tx,
                } => {
                    // Handle window.* methods directly since they need access to RpcState
                    let response = match request.method.as_str() {
                        "window.get_fullscreen" => snow_core::rpc::RpcResponse::success(
                            request.id.clone(),
                            serde_json::json!({ "fullscreen": self.fullscreen_state }),
                        ),
                        "window.set_fullscreen" => {
                            let fullscreen = request
                                .params
                                .get("fullscreen")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            self.fullscreen_request = Some(if fullscreen {
                                FullscreenRequest::Enter
                            } else {
                                FullscreenRequest::Exit
                            });
                            snow_core::rpc::RpcResponse::success(
                                request.id.clone(),
                                serde_json::json!({
                                    "success": true,
                                    "fullscreen": fullscreen
                                }),
                            )
                        }
                        "window.toggle_fullscreen" => {
                            self.fullscreen_request = Some(FullscreenRequest::Toggle);
                            snow_core::rpc::RpcResponse::success(
                                request.id.clone(),
                                serde_json::json!({
                                    "success": true,
                                    "fullscreen": !self.fullscreen_state
                                }),
                            )
                        }
                        _ => {
                            // Handle via the normal RpcHandler
                            let mut handler = EmulatorRpcHandler::new(
                                state,
                                self.frame_buffer.clone(),
                                shared_dir.clone(),
                            );
                            handler.handle_request(&request)
                        }
                    };
                    let _ = response_tx.send(response);
                }
                RpcMessage::Shutdown => break,
            }
        }
    }

    /// Get the socket path if the server is running
    #[allow(dead_code)]
    pub fn socket_path(&self) -> Option<PathBuf> {
        self.server.as_ref().and_then(|s| s.socket_path())
    }
}
