//! The Lisa system bus: address decoding behind the MMU
//!
//! The physical I/O space addresses are in the Lisa Hardware Manual, figure 2-6,
//! where programs see them at $FCxxxx through the map the boot ROM sets up.

use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use log::*;
use serde::{Deserialize, Serialize};

use super::mmu::{LisaMmu, Space};
use super::video::LisaVideo;
use crate::bus::access::{Bus68000, BusAccess, BusResponse, bus_access_word};
use crate::bus::native_clock::NativeClock;
use crate::bus::{Address, Bus, InspectRun, InspectableBus, IrqSource, inspect_read_bytes};
use crate::debuggable::{Debuggable, DebuggableProperties};
use crate::emulator::MouseMode;
use crate::emulator::comm::EmulatorSpeed;
use crate::keymap::KeyEvent;
use crate::mac::MacModel;
use crate::mac::rtc::Rtc;
use crate::mac::scc::Scc;
use crate::mac::scsi::controller::ScsiController;
use crate::mac::swim::Swim;
use crate::renderer::{AudioProvider, Renderer};
use crate::tickable::{Tickable, Ticks};
use crate::types::Byte;

/// SCC baud rate generator clock
const SCC_RTXC_FREQUENCY: Ticks = 3_686_400;

/// CPU clock: the 20.37504 MHz dot clock divided by 4
const BUS_FREQUENCY: Ticks = 5_093_760;

/// CPU clocks per slot of the memory cycle, which alternates between the CPU
/// and the video
const MEMORY_SLOT: Ticks = 4;

/// Size of the physical main memory space
const RAM_SPACE: usize = 2 * 1024 * 1024;

/// The memory boards meet at this physical address: the one in slot 1 goes up
/// from here, the one in slot 2 down
const RAM_BOARDS_MEET: Address = 0x10_0000;

/// FC2: supervisor
const FC_SUPERVISOR: u8 = 1 << 2;

/// The range of the populated physical RAM
fn ram_range(size: usize) -> (Address, Address) {
    let size = size as Address;
    if size <= 512 * 1024 {
        (RAM_BOARDS_MEET, RAM_BOARDS_MEET + size)
    } else {
        (RAM_BOARDS_MEET - size / 2, RAM_BOARDS_MEET + size / 2)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct LisaBus<TRenderer: Renderer> {
    model: MacModel,

    rom: Vec<u8>,
    /// The physical main memory space, populated or not
    pub(crate) ram: Vec<u8>,
    ram_low: Address,
    ram_high: Address,

    mmu: LisaMmu,

    pub(crate) scc: Scc,
    /// Not on the machine: the emulator expects one on every machine
    pub(crate) swim: Swim,
    /// Not on the machine: the emulator expects one on every machine
    pub(crate) scsi: ScsiController,
    /// Not on the machine: the emulator expects one on every machine
    rtc: Rtc,

    /// Emulation speed setting
    pub(crate) speed: EmulatorSpeed,
    native_clock: NativeClock<false>,

    pub(crate) video: LisaVideo<TRenderer>,

    /// Soft and hard memory errors are masked
    soft_error_masked: bool,
    hard_error_masked: bool,
    /// DIAG2: writes make bad parity
    force_bad_parity: bool,
    /// The address (word) last written with bad parity
    bad_parity: Option<Address>,
    /// A read found bad parity: NMI
    hard_error: bool,
    /// A15-A20, A6-A14 and access type of the last hard error
    error_address: u16,

    #[serde(skip, default = "Instant::now")]
    frame_time: Instant,
    frame_clock: Ticks,
}

impl<TRenderer> LisaBus<TRenderer>
where
    TRenderer: Renderer,
{
    pub fn new(
        model: MacModel,
        rom: &[u8],
        video_prom: &[u8],
        renderer: TRenderer,
        _mouse_mode: MouseMode,
        ram_size: Option<usize>,
    ) -> Self {
        let ram_size = ram_size.unwrap_or_else(|| model.ram_size_default());
        let (ram_low, ram_high) = ram_range(ram_size);
        Self {
            model,
            rom: rom.to_vec(),
            ram: vec![0; RAM_SPACE],
            ram_low,
            ram_high,
            mmu: LisaMmu::new(),
            scc: Scc::new(SCC_RTXC_FREQUENCY),
            swim: Swim::new(&[], false, BUS_FREQUENCY),
            scsi: ScsiController::new(),
            rtc: Rtc::default(),
            speed: EmulatorSpeed::Accurate,
            native_clock: NativeClock::new(BUS_FREQUENCY),
            video: LisaVideo::new(video_prom, renderer),
            soft_error_masked: true,
            hard_error_masked: true,
            force_bad_parity: false,
            bad_parity: None,
            hard_error: false,
            error_address: 0,
            frame_time: Instant::now(),
            frame_clock: 0,
        }
    }

    /// The display outputs of this machine (the single built-in display)
    pub fn display_names(&self) -> Vec<String> {
        vec!["Built-in display".to_string()]
    }

    pub fn after_deserialize(&mut self, renderers: Vec<TRenderer>) {
        self.video
            .after_deserialize(renderers.into_iter().next().expect("no renderer"));
    }

    pub fn model(&self) -> MacModel {
        self.model
    }

    pub fn get_effective_speed(&self) -> f64 {
        1.0
    }

    pub fn set_speed(&mut self, speed: EmulatorSpeed) {
        info!("Emulation speed: {:?}", speed);
        self.speed = speed;
    }

    pub fn set_overclock(&mut self, bus_frequency: Option<u64>) {
        self.native_clock
            .set_cpu_frequency(bus_frequency.unwrap_or(BUS_FREQUENCY));
    }

    pub fn set_audio_provider(&mut self, _provider: &mut dyn AudioProvider) -> Result<()> {
        Ok(())
    }

    pub fn mouse_update_rel(&mut self, _relx: i16, _rely: i16, _button: Option<bool>) {}

    pub fn mouse_update_abs(
        &mut self,
        _x: u16,
        _y: u16,
        _rawmouse: (u16, u16),
    ) -> Option<crate::mac::AbsoluteMouse> {
        None
    }

    pub fn set_mouse_mode(&mut self, _mode: MouseMode) {}

    pub fn keyboard_event(&mut self, _ke: KeyEvent) {}

    pub fn input_release_all(&mut self) {}

    pub fn progkey(&mut self) {}

    pub fn video_blank(&mut self) -> Result<()> {
        self.video.blank()
    }

    pub fn rtc_mut(&mut self) -> &mut Rtc {
        &mut self.rtc
    }

    fn ram_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        if !(self.ram_low..self.ram_high).contains(&addr) {
            // The boot ROM sizes memory by what doesn't read back
            return Some(if len == 2 { 0xFFFF } else { 0xFF });
        }
        if self.bad_parity == Some(addr & !1) && !self.hard_error_masked {
            self.hard_error = true;
            self.error_address = (addr >> 5) as u16 & !1;
        }
        let idx = addr as usize;
        Some(match self.ram.get(idx..idx + len as usize) {
            Some(&[hi, lo]) => u32::from(u16::from_be_bytes([hi, lo])),
            Some(&[b]) => u32::from(b),
            _ => 0,
        })
    }

    fn ram_write(&mut self, addr: Address, len: u32, val: u32) -> Option<()> {
        if !(self.ram_low..self.ram_high).contains(&addr) {
            return Some(());
        }
        if self.force_bad_parity {
            self.bad_parity = Some(addr & !1);
        } else if self.bad_parity == Some(addr & !1) {
            self.bad_parity = None;
        }
        let idx = addr as usize;
        let bytes = &(val as u16).to_be_bytes()[2 - len as usize..];
        if let Some(ram) = self.ram.get_mut(idx..idx + bytes.len()) {
            ram.copy_from_slice(bytes);
        }
        Some(())
    }

    /// The boot ROM, or the MMU registers with A15 set
    fn special_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        if addr & (1 << 15) != 0 {
            let serial = if self.video.serial_bit() { 0x8000 } else { 0 };
            return Some(u32::from(self.mmu.read_register(addr) | serial));
        }
        let idx = addr as usize & (self.rom.len() - 1);
        Some(match self.rom.get(idx..idx + len as usize) {
            Some(&[hi, lo]) => u32::from(u16::from_be_bytes([hi, lo])),
            Some(&[b]) => u32::from(b),
            _ => 0xFF,
        })
    }

    fn special_write(&mut self, addr: Address, _len: u32, val: u32) -> Option<()> {
        if addr & (1 << 15) != 0 {
            self.mmu.write_register(addr, val as u16);
        }
        Some(())
    }

    /// A strobe: any access sets or clears a latch, the address tells which
    fn latch_strobe(&mut self, addr: Address) {
        match addr & 0x1E {
            0x04 => self.force_bad_parity = false,
            0x06 => self.force_bad_parity = true,
            0x08 => self.mmu.set_seg1(false),
            0x0A => self.mmu.set_seg1(true),
            0x0C => self.mmu.set_seg2(false),
            0x0E => self.mmu.set_seg2(true),
            // Manual errata: on at the lower address
            0x10 => self.mmu.set_setup(true),
            0x12 => self.mmu.set_setup(false),
            0x14 => self.soft_error_masked = true,
            0x16 => self.soft_error_masked = false,
            0x18 => self.video.set_disabled(true),
            0x1A => self.video.set_disabled(false),
            0x1C => {
                self.hard_error_masked = true;
                self.hard_error = false;
            }
            0x1E => {
                self.hard_error_masked = false;
                self.hard_error = false;
            }
            _ => (),
        }
    }

    /// System status: errors, and bits from the video. The error bits are active
    /// low, per the boot ROM. Bit 6 is low with the inverse video option.
    fn status(&self) -> u16 {
        0xC1 | (if self.hard_error { 0 } else { 1 << 1 }) | self.video.status(&self.ram)
    }

    /// A 16-bit register: a byte access gets the half on its byte lane
    fn register_lane(addr: Address, len: u32, value: u16) -> u32 {
        match (len, addr & 1) {
            (2, _) => u32::from(value),
            (_, 0) => u32::from(value >> 8),
            _ => u32::from(value & 0xFF),
        }
    }

    fn io_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        match addr {
            0xE000..=0xE7FF => {
                self.latch_strobe(addr);
                Some(0)
            }
            0xE800..=0xEFFF => Some(Self::register_lane(addr, len, u16::from(self.video.latch))),
            0xF000..=0xF7FF => Some(Self::register_lane(addr, len, self.error_address)),
            0xF800..=0xFFFF => Some(Self::register_lane(addr, len, self.status())),
            0xC000..=0xFFFF => {
                debug!("I/O read {:04X}", addr);
                Some(0)
            }
            // Empty expansion slots
            _ => None,
        }
    }

    fn io_write(&mut self, addr: Address, _len: u32, val: u32) -> Option<()> {
        match addr {
            0xE000..=0xE7FF => {
                self.latch_strobe(addr);
                Some(())
            }
            0xE800..=0xEFFF => {
                // On the lower data lines, which a byte write drives too
                self.video.latch = val as u8;
                Some(())
            }
            0xC000..=0xFFFF => {
                debug!("I/O write {:04X} {:04X}", addr, val);
                Some(())
            }
            _ => None,
        }
    }
}

impl<TRenderer> Bus<Address, Byte> for LisaBus<TRenderer>
where
    TRenderer: Renderer,
{
    type Cpu = Bus68000;

    fn get_mask(&self) -> Address {
        0x00FF_FFFF
    }

    #[inline(always)]
    fn access(&mut self, mut acc: BusAccess<Bus68000>) -> Result<BusResponse> {
        let supervisor = acc.fc & FC_SUPERVISOR != 0;
        let Some((space, physical)) = self.mmu.translate(acc.addr, acc.write, supervisor) else {
            return Ok(BusResponse::BusError);
        };
        acc.addr = physical;

        // Every access waits for its memory slot
        let now = self.native_clock.native_ticks() + acc.pending;
        let waits = (MEMORY_SLOT - now % MEMORY_SLOT) % MEMORY_SLOT;

        Ok(match space {
            Space::Ram => bus_access_word(acc, waits, self, Self::ram_read, Self::ram_write),
            Space::SpecialIo => {
                bus_access_word(acc, waits, self, Self::special_read, Self::special_write)
            }
            Space::Io => bus_access_word(acc, waits, self, Self::io_read, Self::io_write),
        })
    }

    fn reset(&mut self, hard: bool) -> Result<bool> {
        if hard {
            self.ram.fill(0);
        }
        self.mmu.reset();
        self.video.reset();
        self.soft_error_masked = true;
        self.hard_error_masked = true;
        self.force_bad_parity = false;
        self.hard_error = false;
        self.scc = Scc::new(SCC_RTXC_FREQUENCY);
        Ok(false)
    }
}

impl<TRenderer> Tickable for LisaBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn tick(&mut self, ticks: Ticks, _: ()) -> Result<Ticks> {
        let mut cpu = ticks;
        while cpu > 0 {
            let step = self.native_clock.advance(cpu, None);
            if step.native == 0 {
                break;
            }
            cpu -= step.cpu_used;
        }

        self.video.tick(ticks, &self.ram)?;

        self.frame_clock += ticks;
        while self.frame_clock >= BUS_FREQUENCY / 60 {
            self.frame_clock -= BUS_FREQUENCY / 60;

            // No sound to pace the emulation by
            if matches!(self.speed, EmulatorSpeed::Accurate | EmulatorSpeed::Video) {
                let frametime = self.frame_time.elapsed().as_micros() as u64;
                const DESIRED_FRAMETIME: u64 = 1_000_000 / 60;
                if frametime < DESIRED_FRAMETIME {
                    thread::sleep(Duration::from_micros(DESIRED_FRAMETIME - frametime));
                }
                self.frame_time = Instant::now();
            }
        }

        Ok(ticks)
    }
}

impl<TRenderer> IrqSource for LisaBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_irq(&mut self) -> Option<u8> {
        if self.hard_error {
            return Some(7);
        }
        if self.video.irq() {
            return Some(1);
        }
        None
    }
}

impl<TRenderer> InspectableBus<Address, Byte> for LisaBus<TRenderer>
where
    TRenderer: Renderer,
{
    /// Physical main memory
    fn inspect_read(&mut self, addr: Address) -> Option<Byte> {
        self.ram.get(addr as usize).copied()
    }

    fn inspect_write(&mut self, addr: Address, val: Byte) -> Option<()> {
        *self.ram.get_mut(addr as usize)? = val;
        Some(())
    }

    fn inspect_read_bulk(&mut self, addr: Address, out: &mut [Byte]) -> InspectRun {
        // RAM ends on a 512-byte MMU page
        inspect_read_bytes(self, addr, out, 0x200)
    }

    fn inspect_generation(&self) -> u32 {
        0
    }
}

impl<TRenderer> Debuggable for LisaBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_debug_properties(&self) -> DebuggableProperties {
        vec![]
    }
}
