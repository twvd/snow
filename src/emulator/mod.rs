pub mod comm;

use crate::cpu_m68k::cpu::CpuM68k;
use crate::frontend::channel::ChannelRenderer;
use crate::frontend::{DisplayBuffer, Renderer};
use crate::mac::bus::MacBus;
use crate::mac::video::{SCREEN_HEIGHT, SCREEN_WIDTH};
use crate::tickable::{Tickable, Ticks};

use anyhow::Result;
use comm::{EmulatorCommand, EmulatorCommandSender};

/// Specific properties of a specific Macintosh model
pub struct MacModel {
    pub name: &'static str,
    pub ram_size: usize,
}

/// Emulator runner
pub struct Emulator {
    cpu: CpuM68k<MacBus<ChannelRenderer>>,
    command_recv: crossbeam_channel::Receiver<EmulatorCommand>,
    command_sender: EmulatorCommandSender,
}

impl Emulator {
    pub fn new(
        rom: &[u8],
        model: MacModel,
    ) -> Result<(Self, crossbeam_channel::Receiver<DisplayBuffer>)> {
        // Set up channels
        let (cmds, cmdr) = crossbeam_channel::unbounded();
        let renderer = ChannelRenderer::new(SCREEN_WIDTH, SCREEN_HEIGHT)?;
        let frame_recv = renderer.get_receiver();

        // Initialize bus and CPU
        let bus = MacBus::new(&rom, model.ram_size, renderer);
        let mut cpu = CpuM68k::new(bus);

        cpu.reset()?;
        Ok((
            Self {
                cpu,
                command_recv: cmdr,
                command_sender: cmds,
            },
            frame_recv,
        ))
    }

    pub fn create_cmd_sender(&self) -> EmulatorCommandSender {
        self.command_sender.clone()
    }
}

impl Tickable for Emulator {
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        if !self.command_recv.is_empty() {
            while let Ok(cmd) = self.command_recv.try_recv() {
                match cmd {
                    EmulatorCommand::MouseUpdateRelative { relx, rely, btn } => {
                        self.cpu.bus.mouse_update(relx, rely, btn)
                    }
                    EmulatorCommand::Quit => return Ok(0),
                    EmulatorCommand::InsertFloppy(image) => self.cpu.bus.iwm.disk_insert(&image),
                }
            }
        }
        self.cpu.tick(ticks)?;
        Ok(ticks)
    }
}
