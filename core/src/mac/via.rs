use std::ops::{Deref, DerefMut};

use crate::bus::{Address, BusMember};
use crate::debuggable::Debuggable;
use crate::mac::pluskbd::PlusKeyboard;
use crate::mac::rtc::Rtc;
use crate::mac::via_common::{LineState, ViaCommon, ViaLine, ViaReg, ViaWiring};
use crate::tickable::{Tickable, Ticks};
use crate::types::Byte;

use anyhow::Result;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

use super::MacModel;
use super::adb::AdbTransceiver;

/// Counter at which to trigger the one second interrupt
/// (counted on the E Clock)
const ONESEC_TICKS: Ticks = 783360;

/// Time it takes the keyboard or the ADB transceiver to clock a byte through
/// the shift register. Ideally, these would time the transfer themselves.
const SHIFT_DELAY: Ticks = ONESEC_TICKS * 3 / 1000;

const ACR_SHIFT_OUT: u8 = 0b111;
const ACR_SHIFT_IN: u8 = 0b011;

bitfield! {
    /// VIA Register A (for classic models)
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterA(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Sound volume (non-ASC models)
        pub sound: u8 @ 0..=2,

        /// Sound buffer
        /// (true = main, false = alternate)
        pub sndpg2: bool @ 3,

        /// ROM overlay map is used when set (512K and Plus)
        pub overlay: bool @ 4,

        /// Floppy drive select
        pub drivesel: bool @ 4,

        /// Disk SEL line
        pub sel: bool @ 5,

        /// Video page to be used by video circuitry
        /// (true = main, false = alternate)
        pub page2: bool @ 6,

        /// Another model sense bit
        pub model6: bool @ 6,

        /// SCC Wait/Request (false)
        pub sccwrreq: bool @ 7,
    }
}

bitfield! {
    /// VIA Register B (for classic models)
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterB(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// RTC data line
        pub rtcdata: bool @ 0,

        /// RTC clock line
        pub rtcclk: bool @ 1,

        /// RTC enabled
        pub rtcenb: bool @ 2,

        /// Mouse switch (false = down) (512K and Plus)
        pub sw: bool @ 3,

        /// ADB interrupt (SE+)
        pub adb_int: bool @ 3,

        /// Mouse X2 (512K and Plus)
        pub mouse_x2: bool @ 4,

        /// ADB state input 0 (ST0) (SE+)
        pub adb_st0: bool @ 4,

        /// Mouse Y2 (512K and Plus)
        pub mouse_y2: bool @ 5,

        /// ADB state input 1 (ST1) (SE+)
        pub adb_st1: bool @ 5,

        /// HBlank (512K and Plus)
        pub h4: bool @ 6,

        /// SCSI interrupt (false = enabled) (SE+)
        pub scsi_int: bool @ 6,

        /// VBlank enable for SE/30
        pub se30_vblank_enable: bool @ 6,

        /// Sound enable
        pub sndenb: bool @ 7,

        pub pb0: bool @ 0,
        pub pb1: bool @ 1,
        pub pb2: bool @ 2,
        pub pb3: bool @ 3,
        pub pb4: bool @ 4,
        pub pb5: bool @ 5,
        pub pb6: bool @ 6,
        pub pb7: bool @ 7,
    }
}

bitfield! {
    /// VIA Interrupt flag/enable registers
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterIRQ(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// One-second interrupt
        /// Cleared on Register A read/write
        pub onesec: bool @ 0,

        /// Vertical blank
        /// Cleared on Register A read/write
        pub vblank: bool @ 1,

        /// Keyboard data ready
        /// Cleared on read/write shift reg
        pub kbdready: bool @ 2,

        /// Keyboard data
        /// Cleared on Register B read/write
        pub kbddata: bool @ 3,

        /// Keyboard clock
        /// Cleared on Register B read/write
        pub kbdclock: bool @ 4,

        /// Timer T2
        /// Cleared on read of T2 counter LSB or write of T2 counter MSB
        pub t2: bool @ 5,

        /// Timer T1
        /// Cleared on read of T1 counter LSB or write of T1 counter MSB
        pub t1: bool @ 6,
    }
}

pub struct Via1Wiring;

impl ViaWiring for Via1Wiring {
    type RegisterA = RegisterA;
    type RegisterB = RegisterB;
    type RegisterIRQ = RegisterIRQ;
}

/// Synertek SY6522 Versatile Interface Adapter
#[derive(Serialize, Deserialize)]
pub struct Via {
    core: ViaCommon<Via1Wiring>,

    /// The currently emulated Macintosh model
    model: MacModel,

    /// Counter for the one-second timer
    onesec: Ticks,

    kbdshift_in: u8,
    kbdshift_in_time: Ticks,
    kbdshift_out_time: Ticks,

    pub keyboard: PlusKeyboard,
    pub(crate) rtc: Rtc,

    pub(crate) adb: AdbTransceiver,
}

impl Via {
    pub fn new(model: MacModel) -> Self {
        let mut core = ViaCommon::new();
        core.a_in = model.via1_a_in();

        Self {
            core,
            model,

            onesec: 0,
            kbdshift_in: 0,
            kbdshift_in_time: 0,
            kbdshift_out_time: 0,

            keyboard: PlusKeyboard::default(),
            rtc: Rtc::default(),
            adb: AdbTransceiver::default(),
        }
    }
}

impl Deref for Via {
    type Target = ViaCommon<Via1Wiring>;

    fn deref(&self) -> &Self::Target {
        &self.core
    }
}

impl DerefMut for Via {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.core
    }
}

impl BusMember<Address> for Via {
    fn read(&mut self, addr: Address) -> Option<Byte> {
        let reg = ViaReg::from_addr(addr);

        match reg {
            ViaReg::B => {
                // Lazy update ADB Int to current state
                if self.model.has_adb() {
                    self.core.b_in.set_adb_int(!self.adb.get_int());
                }
            }
            ViaReg::A | ViaReg::ANH => {
                // The SCC write request bit is used for checking if there's data ready
                // from the SCC in critical sections where SCC interrupts are disabled.
                // Reporting 1 signals that there's no data ready.
                self.core.a_in.set_sccwrreq(true);
            }
            _ => (),
        }

        Some(self.core.read(reg))
    }

    fn write(&mut self, addr: Address, val: Byte) -> Option<()> {
        let reg = ViaReg::from_addr(addr);

        if reg == ViaReg::ACR {
            let oldmode = self.core.acr.sr_mode();
            self.core.write(reg, val);
            let newmode = self.core.acr.sr_mode();

            if newmode != oldmode {
                // Reset shifter operation
                self.kbdshift_in_time = 0;
                self.kbdshift_out_time = 0;
            }
            if newmode == ACR_SHIFT_IN {
                self.kbdshift_in_time = SHIFT_DELAY;
            } else if newmode == ACR_SHIFT_OUT {
                self.kbdshift_out_time = SHIFT_DELAY;
            }
            return Some(());
        }

        self.core.write(reg, val);

        match reg {
            // Keyboard shift register
            ViaReg::SR => {
                if self.core.acr.sr_mode() == ACR_SHIFT_OUT {
                    // Start shift-out
                    self.kbdshift_out_time = SHIFT_DELAY;
                }
            }
            ViaReg::B => {
                let rtcin = self.rtc.io(
                    self.core.pb().rtcenb(),
                    self.core.pb().rtcclk(),
                    self.core.pb().rtcdata(),
                );
                self.core.b_in.set_rtcdata(rtcin);

                if self.model.has_adb()
                    && let Some(b) = self
                        .adb
                        .io(self.core.pb().adb_st0(), self.core.pb().adb_st1())
                {
                    self.kbdshift_in = b;
                }
            }
            _ => (),
        }

        Some(())
    }
}

/// Ticked with VIA (E) clock ticks
impl Tickable for Via {
    fn tick(&mut self, ticks: Ticks, _: ()) -> Result<Ticks> {
        self.tick_e(ticks)
    }
}

impl Via {
    /// Runs the VIA for `ticks` ticks of its E clock
    fn tick_e(&mut self, ticks: Ticks) -> Result<Ticks> {
        self.onesec += ticks;

        // One second interrupt
        if self.onesec >= ONESEC_TICKS {
            self.onesec -= ONESEC_TICKS;
            self.core.set_line(ViaLine::CA2, LineState::Pulse);
            self.rtc.second();
        }

        self.core.tick_timers(ticks)?;

        // ADB/Keyboard response
        if self.kbdshift_out_time > 0 {
            self.kbdshift_out_time = self.kbdshift_out_time.saturating_sub(ticks);
            if self.kbdshift_out_time == 0 {
                let data = self.core.shift_sr(0);
                if !self.model.has_adb() {
                    self.kbdshift_in = self.keyboard.cmd(data)?;
                } else {
                    self.adb.data_in(data);
                }
            }
        }
        if self.kbdshift_in_time > 0 {
            self.kbdshift_in_time = self.kbdshift_in_time.saturating_sub(ticks);
            if self.kbdshift_in_time == 0 {
                self.core.shift_sr(self.kbdshift_in);
                self.kbdshift_in_time = SHIFT_DELAY;
            }
        }

        Ok(ticks)
    }
}

impl Debuggable for Via {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;
        use crate::{dbgprop_byte_bin, dbgprop_group, dbgprop_udec};

        let mut props = self.core.debug_properties_ports();
        props.push(dbgprop_group!(
            "Keyboard shifter",
            vec![
                dbgprop_byte_bin!("Shift register", self.core.sr),
                dbgprop_byte_bin!("Input", self.kbdshift_in),
                dbgprop_udec!("Input timer", self.kbdshift_in_time),
                dbgprop_udec!("Output timer", self.kbdshift_out_time),
            ]
        ));
        props.push(dbgprop_udec!("One second timer", self.onesec));
        props.extend(self.core.debug_properties_control());
        props
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mac::via_common::{IRQ_CA1, IRQ_CA2, IRQ_CB1, IRQ_CB2};

    /// Register A with handshake
    const ADDR_A_HS: Address = 0x01 << 9;
    /// Register A without handshake (vBufA)
    const ADDR_A: Address = 0x0F << 9;
    const ADDR_PCR: Address = 0x0C << 9;
    const ADDR_B: Address = 0x00 << 9;

    #[test]
    fn reg_a_no_handshake_keeps_irq_flags() {
        let mut via = Via::new(MacModel::Plus);

        via.set_irq(IRQ_CA1);
        via.set_irq(IRQ_CA2);
        via.read(ADDR_A);
        assert!(via.ifr().vblank() && via.ifr().onesec());
        via.write(ADDR_A, 0);
        assert!(via.ifr().vblank() && via.ifr().onesec());
    }

    #[test]
    fn reg_a_handshake_clears_irq_flags() {
        let mut via = Via::new(MacModel::Plus);

        via.set_irq(IRQ_CA1);
        via.set_irq(IRQ_CA2);
        via.read(ADDR_A_HS);
        assert!(!via.ifr().vblank() && !via.ifr().onesec());

        via.set_irq(IRQ_CA1);
        via.set_irq(IRQ_CA2);
        via.write(ADDR_A_HS, 0);
        assert!(!via.ifr().vblank() && !via.ifr().onesec());
    }

    #[test]
    fn independent_ca2_keeps_irq_flag() {
        let mut via = Via::new(MacModel::Plus);

        // CA2 independent interrupt input, negative edge
        via.write(ADDR_PCR, 0x02);
        via.set_irq(IRQ_CA1);
        via.set_irq(IRQ_CA2);
        via.read(ADDR_A_HS);
        assert!(!via.ifr().vblank() && via.ifr().onesec());

        // Positive edge
        via.write(ADDR_PCR, 0x06);
        via.write(ADDR_A_HS, 0);
        assert!(via.ifr().onesec());

        // Handshake output is not independent
        via.write(ADDR_PCR, 0x0A);
        via.read(ADDR_A_HS);
        assert!(!via.ifr().onesec());
    }

    #[test]
    fn independent_cb2_keeps_irq_flag() {
        let mut via = Via::new(MacModel::Plus);

        via.set_irq(IRQ_CB2);
        via.set_irq(IRQ_CB1);
        via.read(ADDR_B);
        assert!(!via.ifr().kbddata() && !via.ifr().kbdclock());

        // CB2 independent interrupt input, negative edge
        via.write(ADDR_PCR, 0x20);
        via.set_irq(IRQ_CB2);
        via.set_irq(IRQ_CB1);
        via.read(ADDR_B);
        assert!(via.ifr().kbddata() && !via.ifr().kbdclock());

        // Positive edge
        via.write(ADDR_PCR, 0x60);
        via.write(ADDR_B, 0);
        assert!(via.ifr().kbddata());

        // Handshake output is not independent
        via.write(ADDR_PCR, 0xA0);
        via.read(ADDR_B);
        assert!(!via.ifr().kbddata());
    }
}
