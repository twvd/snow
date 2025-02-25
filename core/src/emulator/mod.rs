pub mod comm;

use snow_floppy::loaders::{Autodetect, Bitfile, FloppyImageLoader, FloppyImageSaver};
use snow_floppy::Floppy;
use std::thread;
use std::time::{Duration, Instant};

use crate::bus::{Address, Bus, InspectableBus};
use crate::cpu_m68k::cpu::CpuM68k;
use crate::keymap::Keymap;
use crate::mac::adb::{AdbKeyboard, AdbMouse};
use crate::mac::audio::AudioReceiver;
use crate::mac::bus::MacBus;
use crate::mac::video::{SCREEN_HEIGHT, SCREEN_WIDTH};
use crate::mac::MacModel;
use crate::renderer::channel::ChannelRenderer;
use crate::renderer::{DisplayBuffer, Renderer};
use crate::tickable::{Tickable, Ticks};
use crate::types::{ClickEventSender, KeyEventSender};

use anyhow::Result;
use log::*;

use comm::{
    EmulatorCommand, EmulatorCommandSender, EmulatorEvent, EmulatorEventReceiver, EmulatorStatus,
    FddStatus, HddStatus,
};

/// Emulator runner
pub struct Emulator {
    cpu: CpuM68k<MacBus<ChannelRenderer>>,
    command_recv: crossbeam_channel::Receiver<EmulatorCommand>,
    command_sender: EmulatorCommandSender,
    event_sender: crossbeam_channel::Sender<EmulatorEvent>,
    event_recv: EmulatorEventReceiver,
    run: bool,
    breakpoints: Vec<Address>,
    last_update: Instant,
    adbmouse_sender: Option<ClickEventSender>,
    adbkeyboard_sender: Option<KeyEventSender>,
    model: MacModel,
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
        cpu.bus
            .via
            .rtc
            .load_pram(&format!("{:?}.pram", model).to_ascii_lowercase());

        cpu.reset()?;
        let mut emu = Self {
            cpu,
            command_recv: cmdr,
            command_sender: cmds,
            event_sender: statuss,
            event_recv: statusr,
            run: false,
            breakpoints: vec![],
            last_update: Instant::now(),
            adbmouse_sender,
            adbkeyboard_sender,
            model,
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
        self.event_sender
            .send(EmulatorEvent::Status(Box::new(EmulatorStatus {
                regs: self.cpu.regs.clone(),
                running: self.run,
                breakpoints: self.breakpoints.clone(),
                cycles: self.cpu.cycles,
                fdd: core::array::from_fn(|i| FddStatus {
                    present: self.cpu.bus.swim.drives[i].is_present(),
                    ejected: !self.cpu.bus.swim.drives[i].floppy_inserted,
                    motor: self.cpu.bus.swim.drives[i].motor,
                    writing: self.cpu.bus.swim.drives[i].motor && self.cpu.bus.swim.is_writing(),
                    track: self.cpu.bus.swim.drives[i].track,
                    image_title: self.cpu.bus.swim.drives[i].floppy.get_title().to_owned(),
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

        if self.run
            && (self.breakpoints.contains(&self.cpu.regs.pc)
                || self.cpu.bus.swim.dbg_break.get_clear()
                || self.cpu.bus.dbg_break.get_clear())
        {
            stop_break = true;
        }
        if stop_break {
            info!("Stopped at breakpoint: {:06X}", self.cpu.regs.pc);
            self.run = false;
            self.status_update()?;
        }
        Ok(())
    }

    pub fn get_audio(&self) -> AudioReceiver {
        self.cpu.bus.get_audio_channel()
    }

    pub fn load_hdd_image(&mut self, filename: &str, scsi_id: usize) -> Result<()> {
        self.cpu.bus.scsi.load_disk_at(filename, scsi_id)
    }
}

impl Tickable for Emulator {
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        if !self.command_recv.is_empty() {
            while let Ok(cmd) = self.command_recv.try_recv() {
                match cmd {
                    EmulatorCommand::MouseUpdateRelative { relx, rely, btn } => {
                        if let Some(s) = self.adbmouse_sender.as_ref() {
                            if let Some(b) = btn {
                                s.send(b)?;
                            }
                        }
                        self.cpu.bus.mouse_update_rel(relx, rely, btn);
                    }
                    EmulatorCommand::MouseUpdateAbsolute { x, y } => {
                        self.cpu.bus.mouse_update_abs(x, y);
                    }
                    EmulatorCommand::Quit => {
                        info!("Emulator terminating");
                        return Ok(0);
                    }
                    EmulatorCommand::InsertFloppy(drive, filename) => {
                        let image = Autodetect::load_file(&filename);
                        match image {
                            Ok(img) => {
                                if let Err(e) = self.cpu.bus.swim.disk_insert(drive, img) {
                                    error!("Cannot insert disk: {}", e);
                                }
                            }
                            Err(e) => error!("Cannot load image '{}': {}", filename, e),
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::LoadHddImage(id, filename) => {
                        match self.load_hdd_image(&filename, id) {
                            Ok(_) => info!("SCSI ID #{}: image '{}' loaded", id, filename),
                            Err(e) => {
                                error!("SCSI ID #{}: cannot load image '{}': {}", id, filename, e);
                            }
                        };
                        self.status_update()?;
                    }
                    EmulatorCommand::SaveFloppy(drive, filename) => {
                        Bitfile::save_file(self.cpu.bus.swim.get_active_image(drive), &filename)?;
                        self.status_update()?;
                    }
                    EmulatorCommand::Run => {
                        info!("Running");
                        self.run = true;
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
                        self.status_update()?;
                    }
                    EmulatorCommand::Step => {
                        if !self.run {
                            self.step()?;
                            self.status_update()?;
                        }
                    }
                    EmulatorCommand::ToggleBreakpoint(addr) => {
                        if let Some(idx) = self.breakpoints.iter().position(|&v| v == addr) {
                            self.breakpoints.remove(idx);
                            info!("Breakpoint removed: ${:06X}", addr);
                        } else {
                            self.breakpoints.push(addr);
                            info!("Breakpoint set: ${:06X}", addr);
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
                }
            }
        }

        if self.run {
            if self.last_update.elapsed() > Duration::from_millis(500) {
                self.last_update = Instant::now();
                self.status_update()?;
            }

            // Batch 10000 steps for performance reasons
            for _ in 0..10000 {
                if !self.run {
                    break;
                }
                self.step()?;
            }
        } else {
            thread::sleep(Duration::from_millis(100));
        }

        Ok(ticks)
    }
}
