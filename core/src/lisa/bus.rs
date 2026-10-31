//! The Lisa system bus: address decoding behind the MMU
//!
//! The physical I/O space addresses are in the Lisa Hardware Manual, figure 2-6,
//! where programs see them at $FCxxxx through the map the boot ROM sets up.

use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use log::*;
use serde::{Deserialize, Serialize};

use super::cops_sim::CopsSim;
use super::fdc_sim::{FdcSim, LisaDisk, ROM_ID_2_5, ROM_ID_2_10};
use super::mmu::{LisaMmu, Space};
use super::profile_sim::ProfileSim;
use super::speaker::{Speaker, SpeakerInputs};
use super::via::{KeyboardVia, ParallelVia};
use super::video::LisaVideo;
use crate::bus::access::{Bus68000, BusAccess, BusResponse, bus_access_byte_lane, bus_access_word};
use crate::bus::native_clock::{C8M_PER_VIA, DeviceCycle, NativeClock};
use crate::bus::{
    Address, Bus, BusMember, InspectRun, InspectableBus, IrqSource, inspect_read_bytes,
};
use crate::debuggable::{Debuggable, DebuggableProperties};
use crate::emulator::MouseMode;
use crate::emulator::comm::EmulatorSpeed;
use crate::keymap::KeyEvent;
use crate::mac::MacModel;
use crate::mac::compact::audio::AudioState;
use crate::mac::rtc::Rtc;
use crate::mac::scc::Scc;
use crate::mac::scsi::controller::ScsiController;
use crate::mac::swim::Swim;
use crate::mac::via_common::{LineState, ViaLine, ViaReg};
use crate::renderer::{
    AUDIO_BUFFER_SAMPLES, AUDIO_CHANNELS, AudioProvider, Renderer, null_audio_sink,
};
use crate::tickable::{Tickable, Ticks};
use crate::types::Byte;

/// The clock the SCC's baud rate generators can run from
const SCC_RTXC_FREQUENCY: Ticks = 3_686_400;

/// CPU clock: the 20.37504 MHz dot clock divided by 4
const BUS_FREQUENCY: Ticks = 5_093_760;

/// The VIAs of the 2/10 run at DOTCK/16, the CPU at DOTCK/4. Its I/O board makes
/// their clock from CPUCK with two flip-flops (050-4033-C sheet 2).
const CPU_PER_FAST_VIA: Ticks = 4;

const AUDIO_FREQUENCY: Ticks = 22050;

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

/// With the power off, the CPU stays stalled this long at a time
const POWERED_OFF_STALL: Ticks = 1024;

/// The keyboard VIA, the parallel port VIA below it
const VIA_KEYBOARD_BASE: Address = 0xDC00;

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

    keyboard_via: KeyboardVia,
    parallel_via: ParallelVia,
    cops: CopsSim,
    fdc: FdcSim,
    /// The drive on the parallel port. A save state has it apart, by its slot.
    #[serde(skip)]
    profile: Option<ProfileSim>,
    /// CRES as the drive last saw it
    profile_not_reset: bool,
    /// BSY as the VIA last saw it
    profile_not_busy: bool,

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
    /// The NMI button, or the key programmed in the COPS
    nmi: bool,

    /// CPU clocks since the last tick, of the access in progress
    access_ahead: Ticks,

    #[serde(skip, default = "Instant::now")]
    frame_time: Instant,
    /// CPU clocks towards a tick of the VIAs of the 2/10
    fast_via_clock: Ticks,
    speaker: Speaker,
    audio: AudioState,
    /// Towards the next sample, in units of a CPU clock times the sample rate
    audio_clock: Ticks,
    speaker_via_clocks: Ticks,
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
        double_sided_drive: bool,
    ) -> Self {
        let ram_size = ram_size.unwrap_or_else(|| model.ram_size_default());
        let video = LisaVideo::new(video_prom, renderer);
        // MacWorks on a Screen Kit machine has its screen and boot in low memory
        let (ram_low, ram_high) = if video.is_screen_kit() {
            (0, ram_size as Address)
        } else {
            ram_range(ram_size)
        };
        Self {
            model,
            rom: rom.to_vec(),
            ram: vec![0; RAM_SPACE],
            ram_low,
            ram_high,
            mmu: LisaMmu::new(),
            keyboard_via: KeyboardVia::new(),
            parallel_via: ParallelVia::new(),
            cops: CopsSim::new(),
            fdc: FdcSim::new(
                if model == MacModel::Lisa210 {
                    ROM_ID_2_10
                } else {
                    ROM_ID_2_5
                },
                double_sided_drive,
            ),
            profile: None,
            profile_not_reset: true,
            profile_not_busy: true,
            scc: Scc::new(SCC_RTXC_FREQUENCY),
            swim: Swim::new(&[], false, BUS_FREQUENCY),
            scsi: ScsiController::new(),
            rtc: Rtc::default(),
            speed: EmulatorSpeed::Accurate,
            native_clock: NativeClock::new(BUS_FREQUENCY),
            video,
            soft_error_masked: true,
            hard_error_masked: true,
            force_bad_parity: false,
            bad_parity: None,
            hard_error: false,
            error_address: 0,
            nmi: false,
            access_ahead: 0,
            frame_time: Instant::now(),
            fast_via_clock: 0,
            speaker: Speaker::default(),
            audio: AudioState::default(),
            audio_clock: 0,
            speaker_via_clocks: 0,
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

    /// Holds the frame until it is due. With no sound to pace by, this is what
    /// keeps the speed right in `Accurate` too.
    fn pace_frame(&mut self) {
        let max_speed = match self.speed {
            EmulatorSpeed::Accurate | EmulatorSpeed::Video => 1.0,
            EmulatorSpeed::FastForward(max_speed) => max_speed,
            EmulatorSpeed::Dynamic | EmulatorSpeed::Uncapped => return,
        };
        let period = self.video.frame_period() as f64 / (BUS_FREQUENCY as f64 * max_speed);
        // Counted from when the last frame was due, for the error of the sleep
        // not to add up
        let due = self.frame_time + Duration::from_secs_f64(period);
        let now = Instant::now();
        if now < due {
            thread::sleep(due - now);
            self.frame_time = due;
        } else {
            self.frame_time = now;
        }
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

    pub fn set_audio_provider(&mut self, provider: &mut dyn AudioProvider) -> Result<()> {
        let sink = provider
            .create_stream(
                AUDIO_FREQUENCY as i32,
                AUDIO_CHANNELS as u8,
                AUDIO_BUFFER_SAMPLES as u16,
            )
            .unwrap_or_else(|_| null_audio_sink());
        self.audio.set_sink(sink);
        Ok(())
    }

    /// Gives the speaker what happened since the last sample. Frames pace the
    /// emulation, so a sample that doesn't fit is dropped rather than waited for.
    fn tick_audio(&mut self, cpu_clocks: Ticks, via_clocks: Ticks) -> Result<()> {
        self.speaker_via_clocks += via_clocks;
        self.audio_clock += cpu_clocks * AUDIO_FREQUENCY;
        while self.audio_clock >= BUS_FREQUENCY {
            self.audio_clock -= BUS_FREQUENCY;
            let inputs = SpeakerInputs {
                sr_mode: self.keyboard_via.acr.sr_mode(),
                sr: self.keyboard_via.sr,
                t2_latch_low: self.keyboard_via.t2latch.lsb(),
                volume: self.keyboard_via.pb().volume(),
            };
            let sample = self
                .speaker
                .sample(std::mem::take(&mut self.speaker_via_clocks), inputs);
            if matches!(self.speed, EmulatorSpeed::Accurate | EmulatorSpeed::Dynamic)
                && !self.audio.sink_full()
            {
                self.audio.push(sample)?;
            }
        }
        Ok(())
    }

    pub fn mouse_update_rel(&mut self, relx: i16, rely: i16, button: Option<bool>) {
        self.cops.mouse_rel(relx, rely, button);
    }

    pub fn mouse_update_abs(
        &mut self,
        x: u16,
        y: u16,
        _rawmouse: (u16, u16),
    ) -> Option<crate::mac::AbsoluteMouse> {
        self.cops.mouse_abs(x, y);
        None
    }

    pub fn set_mouse_mode(&mut self, _mode: MouseMode) {}

    pub fn keyboard_event(&mut self, ke: KeyEvent) {
        self.cops.key_event(ke);
    }

    pub fn input_release_all(&mut self) {
        self.cops.release_all();
    }

    /// The NMI button on the back of the machine
    pub fn progkey(&mut self) {
        self.nmi = true;
    }

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
            let serial = if self.video.serial_bit(self.access_ahead) {
                0x8000
            } else {
                0
            };
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

    /// Port A of the keyboard VIA, the commands to the COPS, as it is at the COPS
    /// with undriven lines high
    fn cops_port(&mut self) -> u8 {
        let ddra = self.keyboard_via.read(ViaReg::DDRA);
        self.keyboard_via.pa() | !ddra
    }

    fn keyboard_via_read(&mut self, addr: Address, _len: u32) -> Option<u32> {
        let number = addr >> 1;
        let reg = KeyboardVia::register(number);
        let val = self.keyboard_via.read(reg);
        if reg == ViaReg::A {
            self.cops.acknowledge();
        }
        Some(u32::from(val))
    }

    fn keyboard_via_write(&mut self, addr: Address, _len: u32, val: u32) -> Option<()> {
        let reg = KeyboardVia::register(addr >> 1);
        self.keyboard_via.write(reg, val as Byte);
        if reg == ViaReg::A {
            self.cops.acknowledge();
        }
        self.cops
            .set_keyboard_reset(!self.keyboard_via.pb().not_reset());
        let not_cres = self.keyboard_via.pb().not_cres();
        if !not_cres
            && self.profile_not_reset
            && let Some(profile) = &mut self.profile
        {
            profile.reset();
            self.sync_profile();
        }
        self.profile_not_reset = not_cres;
        Some(())
    }

    fn parallel_via_read(&mut self, addr: Address, _len: u32) -> Option<u32> {
        let reg = ParallelVia::register(addr >> 3);
        let val = self.parallel_via.read(reg);
        // CA2 pulses, which clocks a byte
        if reg == ViaReg::A
            && let Some(profile) = &mut self.profile
        {
            profile.strobe_read();
            self.sync_profile();
        }
        Some(u32::from(val))
    }

    fn parallel_via_write(&mut self, addr: Address, _len: u32, val: u32) -> Option<()> {
        let reg = ParallelVia::register(addr >> 3);
        self.parallel_via.write(reg, val as Byte);
        let ddra = self.parallel_via.read(ViaReg::DDRA);
        let port = self.parallel_via.pa() | !ddra;
        let cmd = !self.parallel_via.pb().not_cmd();
        if let Some(profile) = &mut self.profile {
            if reg == ViaReg::A {
                profile.strobe_write(val as Byte);
            }
            profile.set_cmd(cmd, port);
        }
        self.sync_profile();
        Some(())
    }

    /// The lines from the drive into the parallel port VIA
    fn sync_profile(&mut self) {
        let profile = self.profile.as_ref();
        self.parallel_via.b_in.set_ocd(profile.is_none());
        let not_busy = profile.is_none_or(ProfileSim::not_busy);
        self.parallel_via.b_in.set_not_bsy(not_busy);
        self.parallel_via.a_in = profile.map_or(0xFF, ProfileSim::port);
        if not_busy != self.profile_not_busy {
            self.profile_not_busy = not_busy;
            self.parallel_via
                .set_line(ViaLine::CA1, LineState::from(not_busy));
        }
    }

    /// The drive on the parallel port
    pub fn profile(&self) -> Option<&ProfileSim> {
        self.profile.as_ref()
    }

    pub fn profile_mut(&mut self) -> Option<&mut ProfileSim> {
        self.profile.as_mut()
    }

    /// Plugs a drive in the parallel port, or unplugs it
    pub fn plug_profile(&mut self, profile: Option<ProfileSim>) {
        self.profile = profile;
        self.sync_profile();
    }

    /// The SCC: A/B on A1 and control/data on A2
    fn scc_read(&mut self, addr: Address, _len: u32) -> Option<u32> {
        self.scc.read((addr >> 1) & 3).map(u32::from)
    }

    fn scc_write(&mut self, addr: Address, _len: u32, val: u32) -> Option<()> {
        self.scc.write((addr >> 1) & 3, val as Byte)
    }

    /// An access to a VIA is a 68000 VPA cycle, on the lower byte lane, and lasts a
    /// whole cycle of the E clock.
    fn via_cycle(&mut self, acc: BusAccess<Bus68000>) -> BusResponse {
        let w = match self.native_clock.via_sync(&acc, C8M_PER_VIA) {
            DeviceCycle::Stall(n) => return BusResponse::Stall(n),
            DeviceCycle::Access(w) => w,
        };
        if acc.addr < VIA_KEYBOARD_BASE {
            bus_access_byte_lane::<2, 1, _, _>(
                acc,
                w,
                self,
                Self::parallel_via_read,
                Self::parallel_via_write,
            )
        } else {
            self.keyboard_via.b_in.set_crdy(self.cops.crdy(acc.pending));
            bus_access_byte_lane::<2, 1, _, _>(
                acc,
                w,
                self,
                Self::keyboard_via_read,
                Self::keyboard_via_write,
            )
        }
    }

    /// The shared RAM of the floppy controller is on the lower byte lane
    fn fdc_read(&mut self, addr: Address, _len: u32) -> Option<u32> {
        Some(u32::from(self.fdc.ram_read((addr as usize & 0xFFF) >> 1)))
    }

    fn fdc_write(&mut self, addr: Address, _len: u32, val: u32) -> Option<()> {
        self.fdc
            .ram_write((addr as usize & 0xFFF) >> 1, val as Byte);
        Some(())
    }

    pub fn powered_on(&self) -> bool {
        self.cops.power_on()
    }

    pub fn on_off_button(&mut self) {
        self.cops.power_switch();
    }

    /// The floppy controller, with its drive
    pub fn fdc(&self) -> &FdcSim {
        &self.fdc
    }

    pub fn fdc_mut(&mut self) -> &mut FdcSim {
        &mut self.fdc
    }

    /// The floppy that left the drive
    pub fn disk_take_ejected(&mut self) -> Option<LisaDisk> {
        self.fdc.take_ejected()
    }

    fn io_access(&mut self, acc: BusAccess<Bus68000>, waits: Ticks) -> BusResponse {
        match acc.addr {
            0xC000..=0xCFFF => bus_access_byte_lane::<2, 1, _, _>(
                acc,
                waits,
                self,
                Self::fdc_read,
                Self::fdc_write,
            ),
            0xD000..=0xD3FF => bus_access_byte_lane::<2, 1, _, _>(
                acc,
                waits,
                self,
                Self::scc_read,
                Self::scc_write,
            ),
            0xD800..=0xDFFF => self.via_cycle(acc),
            _ => bus_access_word(acc, waits, self, Self::io_read, Self::io_write),
        }
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
        0xC1 | (if self.hard_error { 0 } else { 1 << 1 })
            | self.video.status(&self.ram, self.access_ahead)
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
        if !self.cops.power_on() {
            return Ok(BusResponse::Stall(POWERED_OFF_STALL));
        }
        let supervisor = acc.fc & FC_SUPERVISOR != 0;
        let Some((space, physical)) = self.mmu.translate(acc.addr, acc.write, supervisor) else {
            return Ok(BusResponse::BusError);
        };
        acc.addr = physical;
        self.access_ahead = acc.pending;

        // Every access waits for its memory slot
        let now = self.native_clock.native_ticks() + acc.pending;
        let waits = (MEMORY_SLOT - now % MEMORY_SLOT) % MEMORY_SLOT;
        self.access_ahead += waits;

        Ok(match space {
            Space::Ram => bus_access_word(acc, waits, self, Self::ram_read, Self::ram_write),
            Space::SpecialIo => {
                bus_access_word(acc, waits, self, Self::special_read, Self::special_write)
            }
            Space::Io => self.io_access(acc, waits),
        })
    }

    fn reset(&mut self, hard: bool) -> Result<bool> {
        if hard {
            self.ram.fill(0);
        }
        self.mmu.reset();
        self.video.reset();
        self.keyboard_via = KeyboardVia::new();
        self.parallel_via = ParallelVia::new();
        self.cops.machine_reset();
        self.fdc.reset();
        if let Some(profile) = &mut self.profile {
            profile.reset();
        }
        self.profile_not_reset = true;
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
            let via_ticks = if self.model == MacModel::Lisa210 {
                self.fast_via_clock += step.cpu_used;
                let via_ticks = self.fast_via_clock / CPU_PER_FAST_VIA;
                self.fast_via_clock %= CPU_PER_FAST_VIA;
                via_ticks
            } else {
                Ticks::from(step.via_tick)
            };
            if via_ticks > 0 {
                self.keyboard_via.tick(via_ticks, ())?;
                self.parallel_via.tick(via_ticks, ())?;
            }
            self.tick_audio(step.cpu_used, via_ticks)?;
        }

        self.fdc.tick(ticks);
        if let Some(profile) = &mut self.profile {
            profile.tick(ticks);
        }
        self.sync_profile();
        self.scc.tick(ticks, BUS_FREQUENCY);
        self.keyboard_via.b_in.set_fdir(self.fdc.fdir());
        self.parallel_via.b_in.set_disk_diag(self.fdc.diag());

        let port = self.cops_port();
        self.cops.tick(ticks, port);
        if let Some(byte) = self.cops.take_strobe() {
            self.keyboard_via.a_in = byte;
            self.keyboard_via.set_line(ViaLine::CA1, LineState::Pulse);
        }
        if self.cops.take_nmi() {
            self.nmi = true;
        }

        if self.video.tick(ticks, &self.ram)? {
            self.pace_frame();
        }

        Ok(ticks)
    }
}

impl<TRenderer> IrqSource for LisaBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_irq(&mut self) -> Option<u8> {
        if self.hard_error || std::mem::take(&mut self.nmi) {
            return Some(7);
        }
        if self.scc.get_irq() {
            return Some(6);
        }
        if self.keyboard_via.irq() {
            return Some(2);
        }
        if self.video.irq() || self.parallel_via.irq() || self.fdc.fdir() {
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
        use crate::dbgprop_nest;
        use crate::debuggable::*;

        vec![
            dbgprop_nest!("Keyboard VIA", self.keyboard_via),
            dbgprop_nest!("Parallel port VIA", self.parallel_via),
        ]
    }
}
