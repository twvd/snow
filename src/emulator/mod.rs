pub mod comm;

use std::thread;
use std::time::{Duration, Instant};

use crate::bus::{Address, Bus};
use crate::cpu_m68k::cpu::CpuM68k;
use crate::frontend::channel::ChannelRenderer;
use crate::frontend::{DisplayBuffer, Renderer};
use crate::mac::bus::MacBus;
use crate::mac::video::{SCREEN_HEIGHT, SCREEN_WIDTH};
use crate::tickable::{Tickable, Ticks};

use anyhow::Result;
use log::*;

use comm::{
    EmulatorCommand, EmulatorCommandSender, EmulatorEvent, EmulatorEventReceiver, EmulatorStatus,
};

/// Specific properties of a specific Macintosh model
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MacModel {
    pub name: &'static str,
    pub ram_size: usize,
}

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
        let bus = MacBus::new(rom, model.ram_size, renderer);
        let mut cpu = CpuM68k::new(bus);

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
            .send(EmulatorEvent::Status(EmulatorStatus {
                regs: self.cpu.regs.clone(),
                running: self.run,
                breakpoints: self.breakpoints.clone(),
                cycles: self.cpu.cycles,
            }))?;

        // Next code stream for disassembly listing
        let mut ops = Vec::with_capacity(200);
        for pc in self.cpu.regs.pc..self.cpu.regs.pc.wrapping_add(200) {
            // TODO deal with read sideeffects
            ops.push(self.cpu.bus.read(pc));
        }
        self.event_sender
            .send(EmulatorEvent::NextCode((self.cpu.regs.pc, ops)))?;

        Ok(())
    }

    /// Steps the emulator by one instruction.
    fn step(&mut self) -> Result<()> {
        self.cpu.tick(1)?;
        if self.run
            && (self.breakpoints.contains(&self.cpu.regs.pc)
                || self.cpu.bus.iwm.dbg_break.get_clear())
            || self.cpu.bus.dbg_break.get_clear()
        {
            info!("Stopped at breakpoint: {:06X}", self.cpu.regs.pc);
            debug!("VIA: {:?}", self.cpu.bus.via);
            self.run = false;
            self.status_update()?;
        }
        Ok(())
    }
}

impl Tickable for Emulator {
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        if !self.command_recv.is_empty() {
            while let Ok(cmd) = self.command_recv.try_recv() {
                trace!("Emulator command {:?}", cmd);
                match cmd {
                    EmulatorCommand::MouseUpdateRelative { relx, rely, btn } => {
                        self.cpu.bus.mouse_update(relx, rely, btn);
                    }
                    EmulatorCommand::Quit => return Ok(0),
                    EmulatorCommand::InsertFloppy(image) => self.cpu.bus.iwm.disk_insert(&image),
                    EmulatorCommand::Run => {
                        info!("Running");
                        self.run = true;
                    }
                    EmulatorCommand::Stop => {
                        info!("Stopped");
                        self.run = false;
                    }
                    EmulatorCommand::Step => {
                        if !self.run {
                            self.step()?;
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
                    }
                }
            }
            self.status_update()?;
        }

        if self.run {
            if self.last_update.elapsed() > Duration::from_millis(500) {
                self.last_update = Instant::now();
                self.status_update()?;
            }
            self.step()?;
        } else {
            thread::sleep(Duration::from_millis(100));
        }

        Ok(ticks)
    }
}
