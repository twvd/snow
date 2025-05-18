pub mod comm;

use snow_floppy::loaders::{Autodetect, FloppyImageLoader, FloppyImageSaver, Moof};
use snow_floppy::Floppy;
use std::collections::VecDeque;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};
use strum::IntoEnumIterator;

use crate::bus::{Address, Bus, InspectableBus};
use crate::cpu_m68k::cpu::HistoryEntry;
use crate::cpu_m68k::{CpuM68000, CpuM68020};
use crate::debuggable::{Debuggable, DebuggableProperties};
use crate::keymap::{KeyEvent, Keymap};
use crate::mac::adb::{AdbKeyboard, AdbMouse};
use crate::mac::compact::bus::{CompactMacBus, RAM_DIRTY_PAGESIZE};
use crate::mac::macii::bus::MacIIBus;
use crate::mac::scc::Scc;
use crate::mac::MacModel;
use crate::renderer::channel::ChannelRenderer;
use crate::renderer::AudioReceiver;
use crate::renderer::{DisplayBuffer, Renderer};
use crate::tickable::{Tickable, Ticks};
use crate::types::{Byte, ClickEventSender, KeyEventSender};

use anyhow::Result;
use bit_set::BitSet;
use log::*;

use crate::cpu_m68k::regs::{Register, RegisterFile};
use crate::emulator::comm::{EmulatorSpeed, UserMessageType};
use crate::mac::scsi::ScsiController;
use crate::mac::swim::Swim;
use comm::{
    Breakpoint, EmulatorCommand, EmulatorCommandSender, EmulatorEvent, EmulatorEventReceiver,
    EmulatorStatus, FddStatus, HddStatus, InputRecording,
};

macro_rules! indirection {
    ($name:ident, $name_mut:ident, $($prop:ident).+, $t:ident) => {
        pub fn $name(&self) -> &$t {
            match self {
                Self::Compact(c) => &c.$($prop).+,
                Self::MacII(c) => &c.$($prop).+,
            }
        }

        pub fn $name_mut(&mut self) -> &mut $t {
            match self {
                Self::Compact(c) => &mut c.$($prop).+,
                Self::MacII(c) => &mut c.$($prop).+,
            }
        }
    };

    ($name:ident, $($prop:ident).+, $t:ident) => {
        pub fn $name(&self) -> &$t {
            match self {
                Self::Compact(c) => &c.$($prop).+,
                Self::MacII(c) => &c.$($prop).+,
            }
        }
    };

    ($name:ident, $($prop:ident).+(), $t:ident) => {
        pub fn $name(&self) -> &$t {
            match self {
                Self::Compact(c) => &c.$($prop).+(),
                Self::MacII(c) => &c.$($prop).+(),
            }
        }
    };
}

/// Emulator config. Basically an abstraction on top of the CPU for multiple different model groups
/// that provides access to the inner components by the emulator runner through dynamic dispatch.
enum EmulatorConfig {
    /// Compact series - Mac 128K, 512K, Plus, SE, Classic
    Compact(CpuM68000<CompactMacBus<ChannelRenderer>>),
    /// Compact series - Mac 128K, 512K, Plus, SE, Classic
    MacII(CpuM68020<MacIIBus<ChannelRenderer>>),
}

#[allow(dead_code)]
impl EmulatorConfig {
    indirection!(swim, swim_mut, bus.swim, Swim);
    indirection!(scsi, scsi_mut, bus.scsi, ScsiController);
    indirection!(scc, scc_mut, bus.scc, Scc);
    indirection!(cpu_regs, cpu_regs_mut, regs, RegisterFile);
    indirection!(cpu_cycles, cycles, Ticks);
    indirection!(speed, bus.speed, EmulatorSpeed);
    indirection!(ram_dirty, ram_dirty_mut, bus.ram_dirty, BitSet);

    pub fn cpu_breakpoints(&self) -> &[Breakpoint] {
        match self {
            Self::Compact(c) => c.breakpoints(),
            Self::MacII(c) => c.breakpoints(),
        }
    }

    pub fn cpu_breakpoints_mut(&mut self) -> &mut Vec<Breakpoint> {
        match self {
            Self::Compact(c) => c.breakpoints_mut(),
            Self::MacII(c) => c.breakpoints_mut(),
        }
    }

    pub fn cpu_set_breakpoint(&mut self, bp: Breakpoint) {
        match self {
            Self::Compact(c) => c.set_breakpoint(bp),
            Self::MacII(c) => c.set_breakpoint(bp),
        }
    }

    pub fn cpu_clear_breakpoint(&mut self, bp: Breakpoint) {
        match self {
            Self::Compact(c) => c.clear_breakpoint(bp),
            Self::MacII(c) => c.clear_breakpoint(bp),
        }
    }

    pub fn cpu_read_history(&mut self) -> Option<&[HistoryEntry]> {
        match self {
            Self::Compact(c) => c.read_history(),
            Self::MacII(c) => c.read_history(),
        }
    }

    pub fn debug_properties(&self) -> DebuggableProperties {
        match self {
            Self::Compact(c) => c.bus.get_debug_properties(),
            Self::MacII(c) => c.bus.get_debug_properties(),
        }
    }

    pub fn ram(&self) -> &[u8] {
        match self {
            Self::Compact(c) => &c.bus.ram,
            Self::MacII(c) => &c.bus.ram,
        }
    }

    pub fn ram_mut(&mut self) -> &mut [u8] {
        match self {
            Self::Compact(c) => &mut c.bus.ram,
            Self::MacII(c) => &mut c.bus.ram,
        }
    }

    pub fn bus_inspect_read(&mut self, addr: Address) -> Option<Byte> {
        match self {
            Self::Compact(c) => c.bus.inspect_read(addr),
            Self::MacII(c) => c.bus.inspect_read(addr),
        }
    }

    pub fn cpu_tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        match self {
            Self::Compact(c) => c.tick(ticks),
            Self::MacII(c) => c.tick(ticks),
        }
    }

    pub fn cpu_get_clr_breakpoint_hit(&mut self) -> bool {
        match self {
            Self::Compact(c) => c.get_clr_breakpoint_hit(),
            Self::MacII(c) => c.get_clr_breakpoint_hit(),
        }
    }

    pub fn cpu_enable_history(&mut self, v: bool) {
        match self {
            Self::Compact(c) => c.enable_history(v),
            Self::MacII(c) => c.enable_history(v),
        }
    }

    pub fn cpu_set_pc(&mut self, pc: Address) -> Result<()> {
        match self {
            Self::Compact(c) => c.set_pc(pc),
            Self::MacII(c) => c.set_pc(pc),
        }
    }

    pub fn cpu_prefetch_refill(&mut self) -> Result<()> {
        match self {
            Self::Compact(c) => c.prefetch_refill(),
            Self::MacII(c) => c.prefetch_refill(),
        }
    }

    pub fn bus_write(&mut self, addr: Address, val: Byte) -> crate::bus::BusResult<Byte> {
        match self {
            Self::Compact(c) => c.bus.write(addr, val),
            Self::MacII(c) => c.bus.write(addr, val),
        }
    }

    pub fn get_audio_channel(&self) -> AudioReceiver {
        match self {
            Self::Compact(c) => c.bus.get_audio_channel(),
            Self::MacII(c) => c.bus.get_audio_channel(),
        }
    }

    pub fn mouse_update_rel(&mut self, relx: i16, rely: i16, button: Option<bool>) {
        match self {
            Self::Compact(c) => c.bus.mouse_update_rel(relx, rely, button),
            Self::MacII(c) => c.bus.mouse_update_rel(relx, rely, button),
        }
    }

    pub fn mouse_update_abs(&mut self, x: u16, y: u16) {
        match self {
            Self::Compact(c) => c.bus.mouse_update_abs(x, y),
            Self::MacII(c) => c.bus.mouse_update_abs(x, y),
        }
    }

    pub fn video_blank(&mut self) -> Result<()> {
        match self {
            Self::Compact(c) => c.bus.video.blank(),
            Self::MacII(_) => Ok(()), // TODO
        }
    }

    pub fn cpu_reset(&mut self) -> Result<()> {
        match self {
            Self::Compact(c) => c.reset(),
            Self::MacII(c) => c.reset(),
        }
    }

    pub fn bus_reset(&mut self) -> Result<()> {
        match self {
            Self::Compact(c) => c.bus.reset(),
            Self::MacII(c) => c.bus.reset(),
        }
    }

    pub fn progkey(&mut self) {
        match self {
            Self::Compact(c) => c.bus.progkey(),
            Self::MacII(c) => c.bus.progkey(),
        }
    }

    pub fn set_speed(&mut self, speed: EmulatorSpeed) {
        match self {
            Self::Compact(c) => c.bus.set_speed(speed),
            Self::MacII(c) => c.bus.set_speed(speed),
        }
    }

    pub fn keyboard_event(&mut self, ev: KeyEvent) -> Result<()> {
        match self {
            Self::Compact(c) => c.bus.via.keyboard.event(ev),
            Self::MacII(_) => unreachable!(),
        }
    }

    pub fn cpu_get_step_over(&self) -> Option<Address> {
        match self {
            Self::Compact(c) => c.get_step_over(),
            Self::MacII(c) => c.get_step_over(),
        }
    }
}

/// Emulator runner
pub struct Emulator {
    config: EmulatorConfig,
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
    // TODO fix large stack frame?
    #[allow(clippy::large_stack_frames)]
    pub fn new(
        rom: &[u8],
        model: MacModel,
    ) -> Result<(Self, crossbeam_channel::Receiver<DisplayBuffer>)> {
        // Set up channels
        let (cmds, cmdr) = crossbeam_channel::unbounded();
        let (statuss, statusr) = crossbeam_channel::unbounded();
        let renderer = ChannelRenderer::new(model.display_width(), model.display_height())?;
        let frame_recv = renderer.get_receiver();

        let (config, adbkeyboard_sender, adbmouse_sender) = match model {
            MacModel::Early128K
            | MacModel::Early512K
            | MacModel::Plus
            | MacModel::SE
            | MacModel::SeFdhd
            | MacModel::Classic => {
                // Initialize bus and CPU
                let bus = CompactMacBus::new(model, rom, renderer);
                let mut cpu = CpuM68000::new(bus);
                assert_eq!(cpu.get_type(), model.cpu_type());

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
                cpu.reset()?;
                (
                    EmulatorConfig::Compact(cpu),
                    adbkeyboard_sender,
                    adbmouse_sender,
                )
            }
            MacModel::MacII => {
                // Initialize bus and CPU
                let bus = MacIIBus::new(model, rom, renderer);
                let mut cpu = CpuM68020::new(bus);
                assert_eq!(cpu.get_type(), model.cpu_type());

                // Initialize input devices
                let adbmouse_sender = if model.has_adb() {
                    let (mouse, mouse_sender) = AdbMouse::new();
                    cpu.bus.via1.adb.add_device(mouse);
                    Some(mouse_sender)
                } else {
                    None
                };
                let adbkeyboard_sender = if model.has_adb() {
                    let (keyboard, sender) = AdbKeyboard::new();
                    cpu.bus.via1.adb.add_device(keyboard);
                    Some(sender)
                } else {
                    None
                };
                cpu.reset()?;
                (
                    EmulatorConfig::MacII(cpu),
                    adbkeyboard_sender,
                    adbmouse_sender,
                )
            }
        };
        // Initialize RTC
        //cpu.bus
        //    .via
        //    .rtc
        //    .load_pram(&format!("{:?}.pram", model).to_ascii_lowercase());

        let mut emu = Self {
            config,
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
        for (i, drive) in self.config.swim_mut().drives.iter_mut().enumerate() {
            if let Some(img) = drive.take_ejected_image() {
                self.event_sender
                    .send(EmulatorEvent::FloppyEjected(i, img))?;
            }
        }

        self.event_sender
            .send(EmulatorEvent::Status(Box::new(EmulatorStatus {
                regs: self.config.cpu_regs().clone(),
                running: self.run,
                breakpoints: self.config.cpu_breakpoints().to_vec(),
                cycles: *self.config.cpu_cycles(),
                fdd: core::array::from_fn(|i| FddStatus {
                    present: self.config.swim().drives[i].is_present(),
                    ejected: !self.config.swim().drives[i].floppy_inserted,
                    motor: self.config.swim().drives[i].motor,
                    writing: self.config.swim().drives[i].motor && self.config.swim().is_writing(),
                    track: self.config.swim().drives[i].track,
                    image_title: self.config.swim().drives[i].floppy.get_title().to_owned(),
                    dirty: self.config.swim().drives[i].floppy.is_dirty(),
                }),
                model: self.model,
                hdd: core::array::from_fn(|i| {
                    self.config
                        .scsi()
                        .get_disk_capacity(i)
                        .map(|capacity| HddStatus {
                            capacity,
                            image: self.config.scsi().get_disk_imagefn(i).unwrap().to_owned(),
                        })
                }),
                speed: *self.config.speed(),
            })))?;

        // Next code stream for disassembly listing
        self.disassemble(self.config.cpu_regs().pc, 200)?;

        // Memory contents
        for page in self.config.ram_dirty() {
            let r = (page * RAM_DIRTY_PAGESIZE)..((page + 1) * RAM_DIRTY_PAGESIZE);
            self.event_sender.send(EmulatorEvent::Memory((
                r.start as Address,
                self.config.ram()[r].to_vec(),
            )))?;
        }
        self.config.ram_dirty_mut().clear();

        // Instruction history
        if let Some(history) = self.config.cpu_read_history() {
            self.event_sender
                .send(EmulatorEvent::InstructionHistory(history.to_vec()))?;
        }

        // Peripheral debug view
        if self.peripheral_debug {
            self.event_sender.send(EmulatorEvent::PeripheralDebug(
                self.config.debug_properties(),
            ))?;
        }

        Ok(())
    }

    fn disassemble(&mut self, addr: Address, len: usize) -> Result<()> {
        let ops = (addr..)
            .take(len)
            .flat_map(|addr| self.config.bus_inspect_read(addr))
            .collect::<Vec<_>>();

        self.event_sender
            .send(EmulatorEvent::NextCode((addr, ops)))?;

        Ok(())
    }

    /// Steps the emulator by one instruction.
    fn step(&mut self) -> Result<()> {
        let mut stop_break = false;
        self.config.cpu_tick(1)?;

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

        if self.run && self.config.cpu_get_clr_breakpoint_hit() {
            stop_break = true;
        }
        if stop_break {
            self.run = false;
            self.status_update()?;
        }
        Ok(())
    }

    pub fn get_audio(&self) -> AudioReceiver {
        self.config.get_audio_channel()
    }

    pub fn load_hdd_image(&mut self, filename: &Path, scsi_id: usize) -> Result<()> {
        self.config.scsi_mut().load_disk_at(filename, scsi_id)
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
                self.config.cpu_regs().pc,
                e
            ));
            let _ = self.status_update();
        }
    }

    pub fn get_cycles(&self) -> Ticks {
        *self.config.cpu_cycles()
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
                        self.config.mouse_update_rel(relx, rely, btn);
                    }
                    EmulatorCommand::MouseUpdateAbsolute { x, y } => {
                        if let Some(r) = self.record_input.as_mut() {
                            r.push((cycles, cmd));
                        }

                        self.config.mouse_update_abs(x, y);
                    }
                    EmulatorCommand::Quit => {
                        info!("Emulator terminating");
                        self.config.video_blank()?;
                        return Ok(0);
                    }
                    EmulatorCommand::InsertFloppy(drive, filename) => {
                        let image = Autodetect::load_file(&filename);
                        match image {
                            Ok(img) => {
                                if let Err(e) = self.config.swim_mut().disk_insert(drive, img) {
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
                                if let Err(e) = self.config.swim_mut().disk_insert(drive, img) {
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
                        if let Err(e) = self.config.swim_mut().disk_insert(drive, *img) {
                            self.user_error(&format!("Cannot insert disk: {}", e));
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::EjectFloppy(drive) => {
                        self.config.swim_mut().drives[drive].eject();
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
                        self.config.scsi_mut().detach_disk_at(id);
                        info!("SCSI ID #{}: disk detached", id);
                        self.status_update()?;
                    }
                    EmulatorCommand::SaveFloppy(drive, filename) => {
                        if let Err(e) = Moof::save_file(
                            self.config.swim().get_active_image(drive),
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
                        self.config.cpu_get_clr_breakpoint_hit();
                        self.config.cpu_breakpoints_mut().retain(|bp| {
                            !matches!(bp, Breakpoint::StepOver(_) | Breakpoint::StepOut(_))
                        });
                        self.status_update()?;
                    }
                    EmulatorCommand::Reset => {
                        // Reset bus first so VIA comes back into overlay mode before resetting the CPU
                        // otherwise the wrong reset vector is loaded.
                        self.config.bus_reset()?;
                        self.config.cpu_reset()?;

                        info!("Emulator reset");
                        self.status_update()?;
                    }
                    EmulatorCommand::Stop => {
                        info!("Stopped");
                        self.run = false;
                        self.config.cpu_breakpoints_mut().retain(|bp| {
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
                            self.config.cpu_set_breakpoint(Breakpoint::StepOut(
                                self.config.cpu_regs().read_a(7),
                            ));
                            self.run = true;
                            self.status_update()?;
                        }
                    }
                    EmulatorCommand::StepOver => {
                        if !self.run {
                            self.try_step();
                            if let Some(addr) = self.config.cpu_get_step_over() {
                                self.config.cpu_set_breakpoint(Breakpoint::StepOver(addr));
                                self.run = true;
                            }
                            self.status_update()?;
                        }
                    }
                    EmulatorCommand::ToggleBreakpoint(bp) => {
                        let exists = self.config.cpu_breakpoints().contains(&bp);
                        if exists {
                            self.config.cpu_clear_breakpoint(bp);
                            info!("Breakpoint removed: {:X?}", bp);
                        } else {
                            self.config.cpu_set_breakpoint(bp);
                            info!("Breakpoint set: {:X?}", bp);
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::BusWrite(start, data) => {
                        for (i, d) in data.into_iter().enumerate() {
                            self.config.bus_write(start + (i as Address), d);
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
                            self.config.keyboard_event(e)?;
                        }
                    }
                    EmulatorCommand::CpuSetPC(val) => self.config.cpu_set_pc(val)?,
                    EmulatorCommand::SetSpeed(s) => self.config.set_speed(s),
                    EmulatorCommand::ProgKey => self.config.progkey(),
                    EmulatorCommand::WriteRegister(reg, val) => {
                        match reg {
                            Register::PC => {
                                if val & 1 != 0 {
                                    self.user_error("Program Counter must be aligned");
                                } else {
                                    self.config.cpu_set_pc(val)?;
                                    self.config.cpu_prefetch_refill()?;
                                }
                            }
                            _ => self.config.cpu_regs_mut().write(reg, val),
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
                    EmulatorCommand::SetInstructionHistory(v) => self.config.cpu_enable_history(v),
                    EmulatorCommand::SetPeripheralDebug(v) => {
                        self.peripheral_debug = v;
                        self.status_update()?;
                    }
                    EmulatorCommand::SccReceiveData(ch, data) => {
                        self.config.scc_mut().push_rx(ch, &data);
                    }
                }
            }
        }

        if self.run {
            if self.last_update.elapsed() > Duration::from_millis(500) {
                self.last_update = Instant::now();
                self.status_update()?;

                for ch in crate::mac::scc::SccCh::iter() {
                    if self.config.scc().has_tx_data(ch) {
                        self.event_sender.send(EmulatorEvent::SccTransmitData(
                            ch,
                            self.config.scc_mut().take_tx(ch),
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
