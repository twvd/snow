use std::fmt::Display;

use anyhow::Result;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

use super::{NubusSlot, VideoCard, bus_long, lanes, standard_offset};
use crate::bus::Address;
use crate::debuggable::Debuggable;
use crate::emulator::EmuContext;
use crate::mac::MacMonitor;
use crate::renderer::{DisplayBuffer, MONOCHROME_LUT, Renderer, render_packed};
use crate::tickable::{Tickable, Ticks};
use crate::types::{Field32, LatchingEvent, Word};

bitfield! {
    /// Control register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct CtrlReg(pub Word): Debug, FromStorage, IntoStorage, DerefStorage {
        pub low: u8 @ 0..=7,
        pub high: u8 @ 8..=15,

        pub reset: bool @ 15,
        pub pixelclock: u8 @ 12..=14,

        pub sense_out: u8 @ 9..=11,
        pub sense_in2: bool @ 9,
        pub sense_in1: bool @ 10,
        pub sense_in0: bool @ 11,

        pub transfer: bool @ 6,
        pub convolution: bool @ 5,
        pub interlace: bool @ 4,
        pub refresh: bool @ 3,
        pub rgb: bool @ 2,
        pub ram: bool @ 0,
    }
}

bitfield! {
    /// RAMDAC control register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RamdacCtrlReg(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        pub mode: u8 @ 1..=4,
        pub conv: bool @ 0,
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Copy, strum::IntoStaticStr, Serialize, Deserialize)]
pub enum Bpp {
    /// 1bpp (black & white)
    One,

    /// 2bpp paletted (4 colors)
    Two,

    /// 4bpp paletted (16 colors)
    Four,

    /// 8bpp paletted (256 colors)
    Eight,

    /// 24-bit direct color ('Millions' of colors)
    TwentyFour,
}

/// Macintosh Display Card 1.2.341-0868
#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Mdc12<TRenderer: Renderer> {
    #[serde(skip)]
    pub renderer: Option<TRenderer>,

    /// None: nothing on the connector
    monitor: Option<MacMonitor>,
    rom: Vec<u8>,
    ctrl: CtrlReg,
    ramdac_ctrl: RamdacCtrlReg,
    vblank_irq: bool,
    vblank_enable: bool,
    vblank_ticks: Ticks,
    pub vram: Vec<u8>,
    toggle: bool,
    pub render: LatchingEvent,
    base: Field32,
    stride: Field32,
    pub palette: Vec<u32>,
    palette_addr: Field32,
    palette_wr: Field32,
    palette_cnt: usize,
}

impl<TRenderer> Mdc12<TRenderer>
where
    TRenderer: Renderer,
{
    /// What the sense lines read with nothing on the connector: none grounded, and in the
    /// extended (type-7) protocol, only the line the card pulls low reads low (TN HW30)
    const NO_MONITOR_SENSE: [u8; 4] = [7, 3, 5, 6];

    /// A card without a monitor has no renderer
    pub fn new(rom: &[u8], renderer: Option<TRenderer>, monitor: Option<MacMonitor>) -> Self {
        Self {
            renderer,
            monitor,
            rom: rom.to_owned(),
            ctrl: CtrlReg(0),
            ramdac_ctrl: RamdacCtrlReg(0),
            vblank_irq: false,
            vblank_enable: false,
            vram: vec![0; 0x20_0000],
            toggle: false,
            vblank_ticks: 0,
            palette: vec![0; 256],
            palette_addr: Field32(0),
            palette_wr: Field32(0),
            render: LatchingEvent::default(),
            base: Field32(0),
            stride: Field32(0),
            palette_cnt: 0,
        }
    }

    pub fn has_monitor(&self) -> bool {
        self.monitor.is_some()
    }

    fn read_ctrl(&self) -> CtrlReg {
        let msense = self
            .monitor
            .map_or(Self::NO_MONITOR_SENSE, MacMonitor::sense);
        let mut sense = msense[0];
        if self.ctrl.sense_in0() {
            sense &= msense[1];
        }
        if self.ctrl.sense_in1() {
            sense &= msense[2];
        }
        if self.ctrl.sense_in2() {
            sense &= msense[3];
        }

        self.ctrl.with_sense_out(sense)
    }

    pub fn bpp(&self) -> Bpp {
        match self.ramdac_ctrl.mode() {
            0 => Bpp::One,
            0x04 => Bpp::Two,
            0x08 => Bpp::Four,
            0x0C => Bpp::Eight,
            0x0D => Bpp::TwentyFour,
            _ => panic!("Unknown RAMDAC mode {:02X}", self.ramdac_ctrl.mode()),
        }
    }

    pub fn framebuffer(&self) -> &[u8] {
        let base_offset = match self.bpp() {
            // ??? not sure why this is off by 2 scanlines
            Bpp::TwentyFour => {
                ((self.base.0 as usize) * 64 * 4) - (self.monitor.map_or(0, MacMonitor::width) * 8)
            }

            _ => (self.base.0 as usize) * 32,
        };
        &self.vram[base_offset..]
    }

    pub fn stride(&self) -> usize {
        let shift = if self.bpp() == Bpp::TwentyFour { 3 } else { 2 };
        (self.stride.0 as usize) << shift
    }

    /// Renders current dislayed frame to target DisplayBuffer
    pub fn render_to(&self, buf: &mut DisplayBuffer) {
        let Some(monitor) = self.monitor else {
            return;
        };
        let fb = self.framebuffer();
        let pixels = monitor.width() * monitor.height();
        buf.set_size(monitor.width(), monitor.height());
        let out = &mut buf[..pixels * 4];
        match self.bpp() {
            Bpp::One => render_packed(fb, out, &MONOCHROME_LUT),
            Bpp::Two => render_packed(fb, out, &self.palette_lut::<4>()),
            Bpp::Four => render_packed(fb, out, &self.palette_lut::<16>()),
            Bpp::Eight => render_packed(fb, out, &self.palette_lut::<256>()),
            Bpp::TwentyFour => {
                assert!(monitor.has_color());
                for (px, src) in out
                    .chunks_exact_mut(4)
                    .zip(fb[..pixels * 4].chunks_exact(4))
                {
                    px.copy_from_slice(&[src[1], src[2], src[3], 0xFF]);
                }
            }
        }
    }

    /// The RGBA pixel of each of the first N palette entries, in grayscale
    /// (the blue channel) on a monochrome monitor
    fn palette_lut<const N: usize>(&self) -> [[u8; 4]; N] {
        let color = self.monitor.is_some_and(|m| m.has_color());
        std::array::from_fn(|i| {
            let c = self.palette[i];
            if color {
                [c as u8, (c >> 8) as u8, (c >> 16) as u8, 0xFF]
            } else {
                let b = (c >> 16) as u8;
                [b, b, b, 0xFF]
            }
        })
    }
}

impl<TRenderer> NubusSlot for Mdc12<TRenderer>
where
    TRenderer: Renderer,
{
    // VRAM, below the registers at $20_0000, is memory and answers every size.
    // A register is one to a long word, and the card decodes only its address:
    // a write takes the low bits of what the CPU drives, whatever its size, and
    // a read gives the register in the low bits of the long. So MAME models the
    // card (nubus_48gc.cpp: "lane select is ignored and firmware relies on
    // smearing").
    fn read_sized(&mut self, addr: Address, size: u32) -> Option<u32> {
        let offset = standard_offset(addr)?;
        if let Some(bytes) = self.vram.get(offset as usize..(offset + size) as usize) {
            return Some(bytes.iter().fold(0, |v, &b| (v << 8) | u32::from(b)));
        }
        let long = match offset & !3 {
            // Control, with the monitor sense lines
            0x20_0000 => u32::from(self.read_ctrl().0),
            0x20_0008 => self.base.0,
            0x20_000C => self.stride.0,
            // CRTC beam position
            0x20_01C0 => {
                self.toggle = !self.toggle;
                if self.toggle { 0 } else { 4 }
            }
            // This has to read 0
            0x20_01C4 | 0x20_01C8 | 0x20_01CC => 0,
            // RAMDAC
            0x20_0200 => self.palette_addr.0,
            0x20_0208 => u32::from(self.ramdac_ctrl.0),
            // ROM, on byte lane 3
            a @ 0xFE_0000..=0xFF_FFFC => self
                .rom
                .get(((a - 0xFE_0000) / 4) as usize)
                .map_or(0, |&b| u32::from(b)),
            a => {
                log::warn!("{}: read from unimplemented register {:06X}", self, a);
                0
            }
        };
        Some(lanes(long, offset, size))
    }

    fn write_sized(&mut self, addr: Address, size: u32, val: u32) -> Option<()> {
        let offset = standard_offset(addr)?;
        if let Some(bytes) = self.vram.get_mut(offset as usize..(offset + size) as usize) {
            bytes.copy_from_slice(&val.to_be_bytes()[4 - size as usize..]);
            return Some(());
        }
        let data = bus_long(val, size, offset);
        match offset & !3 {
            // The control, base and stride registers are 16 bits wide
            0x20_0000 => {
                self.ctrl.0 = data as Word;
                self.ctrl.set_reset(false);
            }
            0x20_0008 => self.base.0 = data & 0xFFFF,
            0x20_000C => self.stride.0 = data & 0xFFFF,
            // CRTC: bit 1 disables the VBL interrupt
            0x20_013C => self.vblank_enable = data & (1 << 1) == 0,
            // Clears the interrupt
            0x20_0148 => self.vblank_irq = false,
            // RAMDAC, 8 bits wide: the address starts a new colour
            0x20_0200 => {
                self.palette_addr.0 = data & 0xFF;
                self.palette_wr.0 = 0;
                self.palette_cnt = 0;
            }
            // Palette data, red, green then blue
            0x20_0204 => {
                self.palette_wr.0 >>= 8;
                self.palette_wr.0 |= (data & 0xFF) << 24;
                self.palette_cnt += 1;
                if self.palette_cnt == 3 {
                    self.palette[(self.palette_addr.0 % 256) as usize] = self.palette_wr.0 >> 8;
                    self.palette_wr.0 = 0;
                    self.palette_addr.0 += 1;
                    self.palette_cnt = 0;
                }
            }
            0x20_0208 => self.ramdac_ctrl.0 = data as u8,
            // Not modelled: the RAMDAC clock preload, the CRTC timings and the
            // clock synthesiser
            0x20_0004 | 0x20_0100..=0x20_01FC | 0x20_0300..=0x20_033C => {}
            a => log::warn!(
                "{}: write to unimplemented register {:06X} {:08X}",
                self,
                a,
                data
            ),
        }
        Some(())
    }

    fn reset(&mut self) {
        self.vblank_irq = false;
        self.vblank_enable = false;
        self.ctrl.0 = 0;
    }

    fn get_irq(&self) -> bool {
        self.vblank_irq
    }
}

impl<TRenderer> VideoCard<TRenderer> for Mdc12<TRenderer>
where
    TRenderer: Renderer,
{
    fn blank(&mut self) -> Result<()> {
        self.vram.fill(0xFF);
        self.ctrl.0 = 0;
        self.render()?;
        Ok(())
    }

    fn render(&mut self) -> Result<()> {
        // We have to move the renderer so we don't upset the borrow checker.
        let Some(mut renderer) = self.renderer.take() else {
            return Ok(());
        };
        self.render_to(renderer.buffer_mut());
        self.renderer = Some(renderer);
        self.renderer.as_mut().unwrap().update()?;
        Ok(())
    }

    fn set_renderer(&mut self, renderer: TRenderer) {
        self.renderer = Some(renderer);
    }
}

impl<TRenderer> Tickable<&dyn EmuContext> for Mdc12<TRenderer>
where
    TRenderer: Renderer,
{
    fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext) -> Result<Ticks> {
        self.vblank_ticks += ticks;
        if self.vblank_ticks >= ctx.bus_frequency() / 60 {
            if self.renderer.as_ref().is_some_and(|r| r.frame_wanted()) {
                self.render()?;
            }

            self.vblank_ticks -= ctx.bus_frequency() / 60;
            if self.vblank_enable {
                self.vblank_irq = true;
            }
        }
        Ok(ticks)
    }
}

impl<TRenderer> Debuggable for Mdc12<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;
        use crate::{dbgprop_bool, dbgprop_group};
        use crate::{dbgprop_byte_bin, dbgprop_enum, dbgprop_long, dbgprop_str, dbgprop_word_bin};

        vec![
            dbgprop_group!(
                "Registers",
                vec![
                    dbgprop_word_bin!("Control", self.ctrl.0),
                    dbgprop_long!("Screen base", self.base.0),
                    dbgprop_long!("Screen stride", self.stride.0),
                    dbgprop_byte_bin!("RAMDAC control", self.ramdac_ctrl.0),
                    dbgprop_long!("Palette write index", self.palette_addr.0)
                ]
            ),
            dbgprop_str!("Monitor", self.monitor.map_or("None", |m| m.into())),
            dbgprop_enum!("BPP", self.bpp()),
            dbgprop_bool!("VBlank enable", self.vblank_enable),
            dbgprop_bool!("VBlank IRQ", self.vblank_irq),
        ]
    }
}

impl<TRenderer> Display for Mdc12<TRenderer>
where
    TRenderer: Renderer,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Macintosh Display Card 8-24")
    }
}

/// The Macs whose NuBus slots take this card
const fn fits_nubus(mac: crate::mac::MacModel) -> bool {
    use crate::mac::MacModel;
    matches!(
        mac,
        MacModel::MacII
            | MacModel::MacIIFDHD
            | MacModel::MacIIx
            | MacModel::MacIIcx
            | MacModel::Quadra700
            | MacModel::Quadra800
    )
}

pub const INFO: crate::emulator::devices::ModelInfo = crate::emulator::devices::ModelInfo {
    model: crate::emulator::devices::DeviceModel::DisplayCard824,
    name: "Display Card 8-24",
    bus: crate::emulator::devices::BusKind::Nubus,
    fits: fits_nubus,
    function: crate::emulator::devices::FunctionKind::Video,
    fixed: true,
    one_per_bus: false,
    rom: Some(crate::emulator::devices::RomInfo {
        label: "Display Card 8-24 ROM",
        digests: &["e2e763a6b432c9196f619a9f90107726ab1a84a1d54242fe5f5182bf3c97b238"],
    }),
    path_keys: &["rom"],
    adds_buses: &[],
    monitors: &[
        None,
        Some(MacMonitor::RGB12),
        Some(MacMonitor::HiRes14),
        Some(MacMonitor::RGB16),
        Some(MacMonitor::RGB21),
        Some(MacMonitor::PortraitBW),
        Some(MacMonitor::TwoPageBW),
    ],
    create: None,
};
