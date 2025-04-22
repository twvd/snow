pub mod comm;

use snow_floppy::loaders::{Autodetect, FloppyImageLoader, FloppyImageSaver, Moof};
use snow_floppy::Floppy;
use std::collections::VecDeque;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};
use strum::IntoEnumIterator;

use crate::bus::{Address, Bus, InspectableBus};
use crate::cpu_m68k::cpu::CpuM68k;
use crate::debuggable::Debuggable;
use crate::keymap::Keymap;
use crate::mac::adb::{AdbKeyboard, AdbMouse};
use crate::mac::audio::AudioReceiver;
use crate::mac::bus::{MacBus, RAM_DIRTY_PAGESIZE};
use crate::mac::video::{SCREEN_HEIGHT, SCREEN_WIDTH};
use crate::mac::MacModel;
use crate::renderer::channel::ChannelRenderer;
use crate::renderer::{DisplayBuffer, Renderer};
use crate::tickable::{Tickable, Ticks};
use crate::types::{ClickEventSender, KeyEventSender};

use anyhow::Result;
use log::*;

use crate::cpu_m68k::regs::Register;
use crate::emulator::comm::UserMessageType;
use comm::{
    Breakpoint, EmulatorCommand, EmulatorCommandSender, EmulatorEvent, EmulatorEventReceiver,
    EmulatorStatus, FddStatus, HddStatus, InputRecording,
};

/// Emulator runner
pub struct Emulator {
    cpu: CpuM68k<MacBus<ChannelRenderer>>,
    command_recv: crossbeam_channel::Receiver<EmulatorCommand>,
    command_sender: EmulatorCommandSender,
    event_sender: crossbeam_channel::Sender<EmulatorEvent>,
    event_recv: EmulatorEventReceiver,
    run: bool,
    last_update: Instant,
    adbmouse_sender: Option<ClickEventSender>,
    adbkeyboard_sender: Option<KeyEventSender>,
    model: MacModel,
    record_input: Option<InputRecording>,
    replay_input: VecDeque<(Ticks, EmulatorCommand)>,
    peripheral_debug: bool,
}

impl Emulator {
    pub fn new(
        rom: &[u8],
        model: MacModel,
    ) -> Result<(Self, crossbeam_channel::Receiver<DisplayBuffer>)> {
        // Set up channels
        let (cmds, cmdr) = crossbeam_channel::unbounded();
        let (statuss, statusr) = crossbeam_channel::unbounded();
        let renderer = ChannelRenderer::new(SCREEN_WIDTH, SCREEN_HEIGHT)?;
        let frame_recv = renderer.get_receiver();

        // Initialize bus and CPU
        let bus = MacBus::new(model, rom, renderer);
        let mut cpu = CpuM68k::new(bus);

        // Initialize input devices
        let adbmouse_sender = if model.has_adb() {
            let (mouse, mouse_sender) = AdbMouse::new();
            cpu.bus.via.adb.add_device(mouse);
            Some(mouse_sender)
        } else {
            None
        };
        let adbkeyboard_sender = if model.has_adb() {
            let (keyboard, sender) = AdbKeyboard::new();
            cpu.bus.via.adb.add_device(keyboard);
            Some(sender)
        } else {
            None
        };

        // Initialize RTC
        //cpu.bus
        //    .via
        //    .rtc
        //    .load_pram(&format!("{:?}.pram", model).to_ascii_lowercase());

        cpu.reset()?;
        let mut emu = Self {
            cpu,
            command_recv: cmdr,
            command_sender: cmds,
            event_sender: statuss,
            event_recv: statusr,
            run: false,
            last_update: Instant::now(),
            adbmouse_sender,
            adbkeyboard_sender,
            model,
            record_input: None,
            replay_input: VecDeque::default(),
            peripheral_debug: false,
        };
        emu.status_update()?;

        Ok((emu, frame_recv))
    }

    pub fn create_cmd_sender(&self) -> EmulatorCommandSender {
        self.command_sender.clone()
    }

    pub fn create_event_recv(&self) -> EmulatorEventReceiver {
        self.event_recv.clone()
    }

    fn status_update(&mut self) -> Result<()> {
        for (i, drive) in self.cpu.bus.swim.drives.iter_mut().enumerate() {
            if let Some(img) = drive.take_ejected_image() {
                self.event_sender
                    .send(EmulatorEvent::FloppyEjected(i, img))?;
            }
        }

        self.event_sender
            .send(EmulatorEvent::Status(Box::new(EmulatorStatus {
                regs: self.cpu.regs.clone(),
                running: self.run,
                breakpoints: self.cpu.breakpoints().to_vec(),
                cycles: self.cpu.cycles,
                fdd: core::array::from_fn(|i| FddStatus {
                    present: self.cpu.bus.swim.drives[i].is_present(),
                    ejected: !self.cpu.bus.swim.drives[i].floppy_inserted,
                    motor: self.cpu.bus.swim.drives[i].motor,
                    writing: self.cpu.bus.swim.drives[i].motor && self.cpu.bus.swim.is_writing(),
                    track: self.cpu.bus.swim.drives[i].track,
                    image_title: self.cpu.bus.swim.drives[i].floppy.get_title().to_owned(),
                    dirty: self.cpu.bus.swim.drives[i].floppy.is_dirty(),
                }),
                model: self.model,
                hdd: core::array::from_fn(|i| {
                    self.cpu
                        .bus
                        .scsi
                        .get_disk_capacity(i)
                        .map(|capacity| HddStatus {
                            capacity,
                            image: self.cpu.bus.scsi.get_disk_imagefn(i).unwrap().to_owned(),
                        })
                }),
                speed: self.cpu.bus.speed,
            })))?;

        // Next code stream for disassembly listing
        self.disassemble(self.cpu.regs.pc, 200)?;

        // Memory contents
        for page in &self.cpu.bus.ram_dirty {
            let r = (page * RAM_DIRTY_PAGESIZE)..((page + 1) * RAM_DIRTY_PAGESIZE);
            self.event_sender.send(EmulatorEvent::Memory((
                r.start as Address,
                self.cpu.bus.ram[r].to_vec(),
            )))?;
        }
        self.cpu.bus.ram_dirty.clear();

        // Instruction history
        if let Some(history) = self.cpu.read_history() {
            self.event_sender
                .send(EmulatorEvent::InstructionHistory(history.to_vec()))?;
        }

        // Peripheral debug view
        if self.peripheral_debug {
            self.event_sender.send(EmulatorEvent::PeripheralDebug(
                self.cpu.bus.get_debug_properties(),
            ))?;
        }

        Ok(())
    }

    fn disassemble(&mut self, addr: Address, len: usize) -> Result<()> {
        let ops = (addr..)
            .flat_map(|addr| self.cpu.bus.inspect_read(addr))
            .take(len)
            .collect::<Vec<_>>();

        self.event_sender
            .send(EmulatorEvent::NextCode((addr, ops)))?;

        Ok(())
    }

    /// Steps the emulator by one instruction.
    fn step(&mut self) -> Result<()> {
        let mut stop_break = false;
        self.cpu.bus.swim.dbg_pc = self.cpu.regs.pc;
        self.cpu.bus.scsi.dbg_pc = self.cpu.regs.pc;
        self.cpu.tick(1)?;

        // Mac 512K: 0x402154, Mac Plus: 0x418CCC
        //if self.cpu.regs.pc == 0x418CCC {
        //    debug!(
        //        "Sony_RdAddr = {}, format: {:02X}, track: {}, sector: {}",
        //        self.cpu.regs.d[0] as i32,
        //        self.cpu.regs.d[3] as u8,
        //        self.cpu.regs.d[1] as u16,
        //        self.cpu.regs.d[2] as u16,
        //    );
        //}
        //if self.cpu.regs.pc == 0x418EBC {
        //    debug!("Sony_RdData = {}", self.cpu.regs.d[0] as i32);
        //}

        if self.run && self.cpu.get_clr_breakpoint_hit() {
            stop_break = true;
        }
        if stop_break {
            self.run = false;
            self.status_update()?;
        }
        Ok(())
    }

    pub fn get_audio(&self) -> AudioReceiver {
        self.cpu.bus.get_audio_channel()
    }

    pub fn load_hdd_image(&mut self, filename: &Path, scsi_id: usize) -> Result<()> {
        self.cpu.bus.scsi.load_disk_at(filename, scsi_id)
    }

    fn user_error(&self, msg: &str) {
        self.event_sender
            .send(EmulatorEvent::UserMessage(
                UserMessageType::Error,
                msg.to_owned(),
            ))
            .unwrap();
        error!("{}", msg);
    }

    #[allow(dead_code)]
    fn user_warning(&self, msg: &str) {
        self.event_sender
            .send(EmulatorEvent::UserMessage(
                UserMessageType::Warning,
                msg.to_owned(),
            ))
            .unwrap();
        warn!("{}", msg);
    }

    #[allow(dead_code)]
    fn user_notice(&self, msg: &str) {
        self.event_sender
            .send(EmulatorEvent::UserMessage(
                UserMessageType::Notice,
                msg.to_owned(),
            ))
            .unwrap();
        info!("{}", msg);
    }

    fn user_success(&self, msg: &str) {
        self.event_sender
            .send(EmulatorEvent::UserMessage(
                UserMessageType::Success,
                msg.to_owned(),
            ))
            .unwrap();
        info!("{}", msg);
    }

    #[inline(always)]
    fn try_step(&mut self) {
        if let Err(e) = self.step() {
            self.run = false;
            self.user_error(&format!(
                "Emulator halted: Uncaught CPU stepping error at PC {:06X}: {}",
                self.cpu.regs.pc, e
            ));
            let _ = self.status_update();
        }
    }

    pub fn get_cycles(&self) -> Ticks {
        self.cpu.cycles
    }
}

impl Tickable for Emulator {
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        if !self.command_recv.is_empty() {
            while let Ok(cmd) = self.command_recv.try_recv() {
                let cycles = self.get_cycles();

                match cmd {
                    EmulatorCommand::MouseUpdateRelative { relx, rely, btn } => {
                        if let Some(r) = self.record_input.as_mut() {
                            r.push((cycles, cmd));
                        }

                        if let Some(s) = self.adbmouse_sender.as_ref() {
                            if let Some(b) = btn {
                                s.send(b)?;
                            }
                        }
                        self.cpu.bus.mouse_update_rel(relx, rely, btn);
                    }
                    EmulatorCommand::MouseUpdateAbsolute { x, y } => {
                        if let Some(r) = self.record_input.as_mut() {
                            r.push((cycles, cmd));
                        }

                        self.cpu.bus.mouse_update_abs(x, y);
                    }
                    EmulatorCommand::Quit => {
                        info!("Emulator terminating");
                        self.cpu.bus.video.blank()?;
                        return Ok(0);
                    }
                    EmulatorCommand::InsertFloppy(drive, filename) => {
                        let image = Autodetect::load_file(&filename);
                        match image {
                            Ok(img) => {
                                if let Err(e) = self.cpu.bus.swim.disk_insert(drive, img) {
                                    self.user_error(&format!("Cannot insert disk: {}", e));
                                }
                            }
                            Err(e) => {
                                self.user_error(&format!(
                                    "Cannot load image '{}': {}",
                                    filename, e
                                ));
                            }
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::InsertFloppyWriteProtected(drive, filename) => {
                        let image = Autodetect::load_file(&filename);
                        match image {
                            Ok(mut img) => {
                                img.set_force_wp();
                                if let Err(e) = self.cpu.bus.swim.disk_insert(drive, img) {
                                    self.user_error(&format!("Cannot insert disk: {}", e));
                                }
                            }
                            Err(e) => {
                                self.user_error(&format!(
                                    "Cannot load image '{}': {}",
                                    filename, e
                                ));
                            }
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::InsertFloppyImage(drive, img) => {
                        if let Err(e) = self.cpu.bus.swim.disk_insert(drive, *img) {
                            self.user_error(&format!("Cannot insert disk: {}", e));
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::EjectFloppy(drive) => {
                        self.cpu.bus.swim.drives[drive].eject();
                    }
                    EmulatorCommand::LoadHddImage(id, filename) => {
                        match self.load_hdd_image(&filename, id) {
                            Ok(_) => info!(
                                "SCSI ID #{}: image '{}' loaded",
                                id,
                                filename.to_string_lossy()
                            ),
                            Err(e) => {
                                self.user_error(&format!(
                                    "SCSI ID #{}: cannot load image '{}': {}",
                                    id,
                                    filename.to_string_lossy(),
                                    e
                                ));
                            }
                        };
                        self.status_update()?;
                    }
                    EmulatorCommand::DetachHddImage(id) => {
                        self.cpu.bus.scsi.detach_disk_at(id);
                        info!("SCSI ID #{}: disk detached", id);
                        self.status_update()?;
                    }
                    EmulatorCommand::SaveFloppy(drive, filename) => {
                        if let Err(e) = Moof::save_file(
                            self.cpu.bus.swim.get_active_image(drive),
                            &filename.to_string_lossy(),
                        ) {
                            self.user_error(&format!(
                                "Cannot save file '{}': {}",
                                filename.file_name().unwrap_or_default().to_string_lossy(),
                                e
                            ));
                        } else {
                            self.user_success(&format!(
                                "Saved floppy image as '{}'",
                                filename.file_name().unwrap_or_default().to_string_lossy()
                            ));
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::Run => {
                        info!("Running");
                        self.run = true;
                        self.cpu.get_clr_breakpoint_hit();
                        self.cpu.breakpoints_mut().retain(|bp| {
                            !matches!(bp, Breakpoint::StepOver(_) | Breakpoint::StepOut(_))
                        });
                        self.status_update()?;
                    }
                    EmulatorCommand::Reset => {
                        // Reset bus first so VIA comes back into overlay mode before resetting the CPU
                        // otherwise the wrong reset vector is loaded.
                        self.cpu.bus.reset()?;
                        self.cpu.reset()?;

                        info!("Emulator reset");
                        self.status_update()?;
                    }
                    EmulatorCommand::Stop => {
                        info!("Stopped");
                        self.run = false;
                        self.cpu.breakpoints_mut().retain(|bp| {
                            !matches!(bp, Breakpoint::StepOver(_) | Breakpoint::StepOut(_))
                        });
                        self.status_update()?;
                    }
                    EmulatorCommand::Step => {
                        if !self.run {
                            self.try_step();
                            self.status_update()?;
                        }
                    }
                    EmulatorCommand::StepOut => {
                        if !self.run {
                            self.cpu
                                .set_breakpoint(Breakpoint::StepOut(self.cpu.regs.read_a(7)));
                            self.run = true;
                            self.status_update()?;
                        }
                    }
                    EmulatorCommand::StepOver => {
                        if !self.run {
                            self.try_step();
                            if let Some(addr) = self.cpu.get_step_over() {
                                self.cpu.set_breakpoint(Breakpoint::StepOver(addr));
                                self.run = true;
                            }
                            self.status_update()?;
                        }
                    }
                    EmulatorCommand::ToggleBreakpoint(bp) => {
                        let exists = self.cpu.breakpoints().contains(&bp);
                        if exists {
                            self.cpu.clear_breakpoint(bp);
                            info!("Breakpoint removed: {:X?}", bp);
                        } else {
                            self.cpu.set_breakpoint(bp);
                            info!("Breakpoint set: {:X?}", bp);
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::BusWrite(start, data) => {
                        for (i, d) in data.into_iter().enumerate() {
                            self.cpu.bus.write(start + (i as Address), d);
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::Disassemble(addr, len) => {
                        self.disassemble(addr, len)?;
                        // Skip status update which would reset the disassembly view
                        return Ok(ticks);
                    }
                    EmulatorCommand::KeyEvent(e) => {
                        if let Some(r) = self.record_input.as_mut() {
                            r.push((cycles, cmd));
                        }

                        if !self.run {
                            info!("Ignoring keyboard input while stopped");
                        } else if let Some(sender) = self.adbkeyboard_sender.as_ref() {
                            if let Some(e) = e.translate_scancode(Keymap::AekM0115) {
                                sender.send(e)?;
                            }
                        } else if let Some(e) = e.translate_scancode(Keymap::AkM0110) {
                            self.cpu.bus.via.keyboard.event(e)?;
                        }
                    }
                    EmulatorCommand::ToggleBusTrace => self.cpu.bus.trace = !self.cpu.bus.trace,
                    EmulatorCommand::CpuSetPC(val) => self.cpu.set_pc(val)?,
                    EmulatorCommand::SetSpeed(s) => self.cpu.bus.set_speed(s),
                    EmulatorCommand::ProgKey => self.cpu.bus.progkey(),
                    EmulatorCommand::WriteRegister(reg, val) => {
                        match reg {
                            Register::PC => {
                                if val & 1 != 0 {
                                    self.user_error("Program Counter must be aligned");
                                } else {
                                    self.cpu.set_pc(val)?;
                                    self.cpu.prefetch_refill()?;
                                }
                            }
                            _ => self.cpu.regs.write(reg, val),
                        };
                        self.status_update()?;
                    }
                    EmulatorCommand::StartRecordingInput => {
                        self.record_input = Some(InputRecording::default());
                    }
                    EmulatorCommand::EndRecordingInput => {
                        self.event_sender.send(EmulatorEvent::RecordedInput(
                            self.record_input.take().expect("Recording was not active"),
                        ))?;
                    }
                    EmulatorCommand::ReplayInputRecording(rec, immediately) => {
                        let cycles = self.get_cycles();
                        if rec.is_empty() {
                            break;
                        }

                        // On 'immediately', we skip the delay before the first step and
                        // then continue with the relative cycle delays.
                        //
                        // This is useful if you want to replay a recording once the
                        // system has already been running.
                        let recording_offset = if immediately { rec[0].0 } else { 0 };

                        self.replay_input = VecDeque::from_iter(
                            rec.into_iter()
                                // Offset by current cycles so we can just compare to absolute
                                // cycles later.
                                .map(|(t, c)| (t - recording_offset + cycles, c)),
                        );
                    }
                    EmulatorCommand::SetInstructionHistory(v) => self.cpu.enable_history(v),
                    EmulatorCommand::SetPeripheralDebug(v) => {
                        self.peripheral_debug = v;
                        self.status_update()?;
                    }
                    EmulatorCommand::SccReceiveData(ch, data) => {
                        self.cpu.bus.scc.push_rx(ch, &data);
                    }
                }
            }
        }

        if self.run {
            if self.last_update.elapsed() > Duration::from_millis(500) {
                self.last_update = Instant::now();
                self.status_update()?;

                for ch in crate::mac::scc::SccCh::iter() {
                    if self.cpu.bus.scc.has_tx_data(ch) {
                        self.event_sender.send(EmulatorEvent::SccTransmitData(
                            ch,
                            self.cpu.bus.scc.take_tx(ch),
                        ))?;
                    }
                }
            }

            // Replay next step in recording if currently replaying
            if let Some((t, c)) = self.replay_input.front() {
                if *t <= self.get_cycles() {
                    self.command_sender.send(c.clone()).unwrap();
                    self.replay_input.pop_front().unwrap();
                }
            }

            // Batch 10000 steps for performance reasons
            for _ in 0..10000 {
                if !self.run {
                    break;
                }
                self.try_step();
            }
        } else {
            thread::sleep(Duration::from_millis(100));
        }

        Ok(ticks)
    }
}
