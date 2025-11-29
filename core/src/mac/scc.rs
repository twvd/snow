use std::collections::VecDeque;

use crate::{
    bus::{Address, BusMember},
    types::Byte,
};
use log::*;
use num_derive::{FromPrimitive, ToPrimitive};
use num_traits::{FromPrimitive, ToPrimitive};
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

#[derive(Debug, FromPrimitive, ToPrimitive)]
enum SccCommand {
    Null = 0b000,
    PointHigh = 0b001,
    ResetExtStatusInt = 0b010,
    /// Send Abort (SDLC)
    SendAbort = 0b011,
    /// Enable interrupt on next RX character
    IntNextRx = 0b100,
    /// Reset TX interrupt pending
    ResetTxInt = 0b101,
    ResetError = 0b110,
    ResetHighestIUS = 0b111,
}

#[derive(Debug, FromPrimitive, ToPrimitive)]
enum SccIrqPending {
    BTxEmpty = 0b000,
    BExtStatusChange = 0b001,
    BRxAvailable = 0b010,
    BSpecialReceive = 0b011,
    ATxEmpty = 0b100,
    AExtStatusChange = 0b101,
    ARxAvailable = 0b110,
    ASpecialReceive = 0b111,
}

bitfield! {
    /// SCC read register 0
    /// Transmit and Receive buffer status and external status
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
    pub struct RdReg0(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// RX character available
        pub rx_char: bool @ 0,
        /// Zero Count
        pub zero: bool @ 1,
        /// TX buffer empty
        pub tx_empty: bool @ 2,
        /// DCD
        pub dcd: bool @ 3,
        /// Sync/hunt
        pub sync_hunt: bool @ 4,
        /// CTS
        pub cts: bool @ 5,
        /// TX underrun/EOM
        pub tx_underrun: bool @ 6,
        /// Break/abort
        pub break_abort: bool @ 7,
    }
}

bitfield! {
    /// SCC read register 1
    /// Special Receive Condition status bits and the residue codes for the l-field in SDLC mode
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize,Default)]
    pub struct RdReg1(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        pub all_sent: bool @ 0,
        pub residue: u8 @ 1..=3,
        pub parity_error: bool @ 4,
        pub rx_overrun_err: bool @ 5,
        pub crc_framing_err: bool @ 6,
        pub end_of_frame: bool @ 7,
    }
}

bitfield! {
    /// SCC read register 2
    /// Interrupt vector/status bits
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RdReg2(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        pub intvec: u8 @ 0..=7,
        pub status_low: u8 @ 1..=3,
        pub status_high: u8 @ 4..=6,
    }
}

bitfield! {
    /// SCC read register 3
    /// Interrupt pending
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
    pub struct RdReg3(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        pub b_ext_status_ip: bool @ 0,
        pub b_tx_ip: bool @ 1,
        pub b_rx_ip: bool @ 2,
        pub a_ext_status_ip: bool @ 3,
        pub a_tx_ip: bool @ 4,
        pub a_rx_ip: bool @ 5,
    }
}

bitfield! {
    /// SCC write register 0
    /// CRC initialize, initialization commands for the various modes, register pointers
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct WrReg0(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        pub reg: u8 @ 0..=2,
        pub cmdcode: u8 [try_get_fn SccCommand::from_u8 -> Option<SccCommand>] @ 3..=5,
        pub crcreset: u8 @ 6..=7,
    }
}

bitfield! {
    /// SCC write register 1
    /// Transmit/Receive Interrupt and Data Transfer Mode Definition
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct WrReg1(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        pub ext_ie: bool @ 0,
        pub tx_ie: bool @ 1,
        pub parity_special: bool @ 2,
        pub rx_ie: u8 @ 3..=4,
        // 3 bits WAIT/DMA stuff
    }
}

bitfield! {
    /// SCC write register 3
    /// Receive Parameters and Control
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct WrReg3(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        pub rx_enable: bool @ 0,
        pub sync_load_inhibit: bool @ 1,
        pub sdlc_address_search: bool @ 2,
        pub rx_crc_enable: bool @ 3,
        /// Enable hunt mode
        pub hunt: bool @ 4,
        /// Automatic control of DCD/CTS
        pub auto_enables: bool @ 5,
        pub data_bits: u8 @ 6..=7,
    }
}

bitfield! {
    /// SCC write register 5
    /// Transmit parameters and controls
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct WrReg5(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        pub tx_crc: bool @ 0,
        pub rts: bool @ 1,
        pub sdlc: bool @ 2,
        pub tx_enable: bool @ 3,
        pub send_break: bool @ 4,
        pub databits: u8 @ 5..=6,
        pub dtr: bool @ 7,
    }
}

bitfield! {
    /// SCC write register 9
    /// Master Interrupt Control and Reset
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
    pub struct WrReg9(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Vector Includes Status
        pub vis: bool @ 0,
        /// No Vector
        pub nv: bool @ 1,
        pub dlc: bool @ 2,
        /// Master Interrupt Enable
        pub mie: bool @ 3,
        /// Status high/status low
        pub st_high_low: bool @ 4,
        pub intack: bool @ 5,
    }
}

bitfield! {
    /// SCC write register 15
    /// External/status interrupt control
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
    pub struct WrReg15(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        pub sdlc_en: bool @ 0,
        pub zero_count: bool @ 1,
        pub sdlc_fifo: bool @ 2,
        pub dcd: bool @ 3,
        pub sync_hunt: bool @ 4,
        pub cts: bool @ 5,
        /// TX underrun / EOM
        pub tx_underrun: bool @ 6,
        pub break_abort: bool @ 7,
    }
}

bitfield! {
    /// SCC read register 15
    /// External/status interrupt control
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
    pub struct RdReg15(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        pub zero_count: bool @ 1,
        pub dcd: bool @ 3,
        pub sync_hunt: bool @ 4,
        pub cts: bool @ 5,
        /// TX underrun / EOM
        pub tx_underrun: bool @ 6,
        pub break_abort: bool @ 7,
    }
}

#[derive(
    Debug, ToPrimitive, Eq, PartialEq, Copy, Clone, Serialize, Deserialize, strum::EnumIter,
)]
pub enum SccCh {
    A = 0,
    B = 1,
}

#[derive(Default, Serialize, Deserialize)]
struct SccChannel {
    sdlc: bool,
    hunt: bool,
    tx_enable: bool,
    rx_enable: bool,
    ext_ip: bool,
    tx_ip: bool,
    tx_ie: bool,
    rx_ip: bool,
    rx_ie: bool,
    ext_ie: bool,
    dcd: bool,
    dcd_ie: bool,

    reg12: u8,
    reg13: u8,
    reg15: u8,

    tx_queue: VecDeque<u8>,
    rx_queue: VecDeque<u8>,
}

/// Zilog Z8530 Serial Communications Controller
#[derive(Default, Serialize, Deserialize)]
pub struct Scc {
    /// Channels
    /// 0 = Channel A, 1 = Channel B
    ch: [SccChannel; 2],

    /// Selected register
    reg: usize,

    /// Interrupt vector
    intvec: u8,

    /// Master interrupt control (register 9)
    mic: WrReg9,
}

impl Scc {
    pub fn new() -> Self {
        Self::default()
    }

    fn read_data(&mut self, ch: SccCh) -> u8 {
        let chi = ch.to_usize().unwrap();
        self.ch[chi].rx_queue.pop_front().unwrap_or(0)
    }

    fn write_data(&mut self, ch: SccCh, val: u8) {
        let chi = ch.to_usize().unwrap();
        if self.ch[chi].tx_enable && self.ch[chi].tx_ie && self.mic.mie() {
            self.ch[chi].tx_ip = true;
        }
        self.ch[chi].tx_queue.push_back(val);
    }

    fn get_irq_pending(&self) -> Option<SccIrqPending> {
        if !self.mic.mie() {
            None
        } else if self.ch[0].tx_ip && self.ch[0].tx_ie {
            Some(SccIrqPending::ATxEmpty)
        } else if self.ch[1].tx_ip && self.ch[1].tx_ie {
            Some(SccIrqPending::BTxEmpty)
        } else if !self.ch[0].rx_queue.is_empty() && self.ch[0].rx_ie {
            Some(SccIrqPending::ARxAvailable)
        } else if !self.ch[1].rx_queue.is_empty() && self.ch[1].rx_ie {
            Some(SccIrqPending::BRxAvailable)
        } else if self.ch[0].ext_ip && self.ch[0].ext_ie {
            Some(SccIrqPending::AExtStatusChange)
        } else if self.ch[1].ext_ip && self.ch[1].ext_ie {
            Some(SccIrqPending::BExtStatusChange)
        } else {
            None
        }
    }

    fn read_ctrl(&mut self, ch: SccCh) -> u8 {
        let chi = ch.to_usize().unwrap();

        let result = match (self.reg, ch) {
            (0 | 4, _) => *RdReg0::default()
                .with_rx_char(!self.ch[chi].rx_queue.is_empty())
                .with_tx_empty(true)
                .with_tx_underrun(true)
                .with_sync_hunt(self.ch[chi].hunt)
                .with_dcd(self.ch[chi].dcd),
            (1 | 5, _) => *RdReg1::default().with_all_sent(true),
            (2 | 6, SccCh::B) => {
                // Modified interrupt vector
                let v = self
                    .get_irq_pending()
                    .map(|i| i.to_u8().unwrap())
                    .unwrap_or(0b011);
                if self.mic.st_high_low() {
                    let inv = ((v & 0b100) >> 2) | (v & 0b010) | ((v & 0b001) << 2);
                    *RdReg2(self.intvec).with_status_high(inv)
                } else {
                    *RdReg2(self.intvec).with_status_low(v)
                }
            }
            (2 | 6, SccCh::A) => self.intvec,
            (3, SccCh::B) => 0,
            (3, SccCh::A) => *RdReg3::default()
                .with_b_ext_status_ip(self.ch[1].ext_ip)
                .with_b_tx_ip(self.ch[1].tx_ip)
                .with_b_rx_ip(self.ch[1].rx_ip)
                .with_a_ext_status_ip(self.ch[0].ext_ip)
                .with_a_tx_ip(self.ch[0].tx_ip)
                .with_a_rx_ip(self.ch[0].rx_ip),
            (10, _) => {
                // Misc. status bits
                0
            }
            (12, _) => self.ch[chi].reg12,
            (13, _) => self.ch[chi].reg13,
            (15, _) => self.ch[chi].reg15,
            _ => {
                warn!("Ch {:?} unimplemented ctrl read {}", ch, self.reg);
                0
            }
        };
        //debug!("Ch {:?} read ctrl {} = {:02X}", ch, self.reg, result);

        self.reg = 0;
        result
    }

    fn write_ctrl(&mut self, ch: SccCh, val: u8) {
        let chi = ch.to_usize().unwrap();
        let reg = self.reg;
        self.reg = 0;

        //debug!("Ch {:?} write ctrl {} = {:02X}", ch, reg, val);

        match reg {
            0 => {
                let r = WrReg0(val);
                // Register pointer
                self.reg = r.reg() as usize;

                match r.cmdcode().unwrap() {
                    SccCommand::Null => (),
                    SccCommand::ResetError => (),
                    SccCommand::PointHigh => self.reg |= 1 << 3,
                    SccCommand::ResetExtStatusInt => {
                        self.ch[chi].hunt = false;
                        self.ch[chi].ext_ip = false;
                    }
                    SccCommand::ResetTxInt => {
                        self.ch[chi].tx_ip = false;
                    }
                    SccCommand::IntNextRx => {
                        self.ch[chi].rx_ip = false;
                    }
                    _ => {
                        warn!("unimplemented command {:?}", r.cmdcode().unwrap());
                    }
                }
            }
            1 => {
                let r = WrReg1(val);
                self.ch[chi].tx_ie = r.tx_ie();
                self.ch[chi].rx_ie = r.rx_ie() != 0;
                self.ch[chi].ext_ie = r.ext_ie();
            }
            2 => {
                self.intvec = val;
            }
            3 => {
                let r = WrReg3(val);
                if r.hunt() {
                    self.ch[chi].hunt = true;
                }
                if !r.rx_enable() && self.ch[chi].rx_enable {
                    self.ch[chi].hunt = true;
                }
                self.ch[chi].rx_enable = r.rx_enable();
            }
            5 => {
                let r = WrReg5(val);
                self.ch[chi].tx_enable = r.tx_enable();
                self.ch[chi].tx_ip = false;
            }
            9 => {
                self.mic.0 = val;
            }
            12 => {
                self.ch[chi].reg12 = val;
            }
            13 => {
                self.ch[chi].reg13 = val;
            }
            14 => {
                // DPLL/baudrate generator
            }
            15 => {
                let wrval = WrReg15(val);
                self.ch[chi].sdlc = wrval.sdlc_en();
                self.ch[chi].dcd_ie = wrval.dcd();
                self.ch[chi].reg15 = val & !0b101;
            }
            _ => {
                warn!("{:?} unimplemented wr reg {} {:02X}", ch, reg, val);
            }
        }
    }

    pub fn get_irq(&mut self) -> bool {
        self.get_irq_pending().is_some()
    }

    pub fn push_rx(&mut self, ch: SccCh, data: &[u8]) {
        let chi = ch.to_usize().unwrap();
        if !self.ch[chi].rx_enable {
            return;
        }

        self.ch[chi].rx_queue.extend(data.iter());
        if self.mic.mie() && self.ch[chi].rx_ie {
            self.ch[chi].rx_ip = true;
        }
    }

    pub fn take_tx(&mut self, ch: SccCh) -> Vec<u8> {
        self.ch[ch.to_usize().unwrap()].tx_queue.drain(..).collect()
    }

    pub fn has_tx_data(&self, ch: SccCh) -> bool {
        !self.ch[ch.to_usize().unwrap()].tx_queue.is_empty()
    }

    pub fn decode_addr(addr: Address) -> (SccCh, bool) {
        let ch = if addr & (1 << 0) == 0 {
            SccCh::B
        } else {
            SccCh::A
        };
        let ctrl = addr & (1 << 1) == 0;
        (ch, ctrl)
    }

    pub fn set_dcd(&mut self, ch: SccCh, val: bool) {
        let chi = ch.to_usize().unwrap();

        if self.ch[chi].dcd == val {
            // No change in state
            return;
        }

        if self.ch[chi].dcd_ie {
            // Trigger interrupt
            self.ch[chi].ext_ip = true;
        }
        self.ch[chi].dcd = val;
    }

    pub fn get_dcd(&self, ch: SccCh) -> bool {
        let chi = ch.to_usize().unwrap();
        self.ch[chi].dcd
    }
}

impl BusMember<Address> for Scc {
    fn read(&mut self, addr: Address) -> Option<Byte> {
        let (ch, ctrl) = Self::decode_addr(addr);
        if ctrl {
            Some(self.read_ctrl(ch))
        } else {
            Some(self.read_data(ch))
        }
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        let (ch, ctrl) = Self::decode_addr(addr);
        if ctrl {
            Some(self.write_ctrl(ch, val))
        } else {
            Some(self.write_data(ch, val))
        }
    }
}
