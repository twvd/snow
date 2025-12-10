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
        /// WR7' enable (alternate register access) - should always be 0
        pub wr7_prime_en: bool @ 0,
        pub zero_count: bool @ 1,
        /// SDLC status FIFO enable
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
    /// RX interrupt mode from WR1 bits 3-4:
    /// 0 = disabled, 1 = first char or special, 2 = all chars or special, 3 = special only
    rx_int_mode: u8,
    /// First character flag - set when enabling "int on first char" mode
    first_char: bool,
    ext_ie: bool,
    dcd: bool,
    dcd_ie: bool,

    reg15: u8,

    tx_queue: VecDeque<u8>,
    rx_queue: VecDeque<u8>,

    // LocalTalk/SDLC frame state
    /// Current LocalTalk RX frame (complete LLAP packet)
    lt_rx_frame: Option<Vec<u8>>,
    /// Offset into current frame (including 2 CRC bytes at end)
    lt_rx_offset: usize,
    /// End of frame flag (set after CRC bytes read)
    lt_end_of_frame: bool,
    /// Pre-buffered RX byte
    lt_rx_buff: u8,
    /// RX character available flag
    lt_rx_chr_avail: bool,
    /// Flag set when LocalTalk needs to be polled (RX just re-enabled)
    localtalk_poll_needed: bool,
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

        // LocalTalk/SDLC mode: read from frame buffer
        if self.ch[chi].sdlc {
            return self.read_data_sdlc(chi);
        }

        self.ch[chi].rx_queue.pop_front().unwrap_or(0)
    }

    /// Read data byte in SDLC mode (LocalTalk)
    fn read_data_sdlc(&mut self, chi: usize) -> u8 {
        // Get the pre-buffered value
        let value = self.ch[chi].lt_rx_buff;

        // Clear first_char flag after reading (important for RxIntMode 1)
        self.ch[chi].first_char = false;

        // Advance to next byte
        self.lt_rx_buff_advance(chi);

        value
    }

    /// Advance the RX buffer to the next byte
    fn lt_rx_buff_advance(&mut self, chi: usize) {
        // If no frame, clear chr_avail and set flag byte
        let Some(ref frame) = self.ch[chi].lt_rx_frame else {
            self.ch[chi].lt_rx_buff = 0x7E;
            self.ch[chi].lt_rx_chr_avail = false;
            return;
        };

        let frame_len = frame.len();
        let offset = self.ch[chi].lt_rx_offset;

        if offset < frame_len {
            // Return frame data
            self.ch[chi].lt_rx_buff = frame[offset];
        } else {
            // CRC bytes
            let crc_offset = offset - frame_len;
            // After reading second CRC byte (crc_offset == 1), signal end of frame
            if crc_offset == 1 {
                // Clear the frame and signal EOF
                self.ch[chi].lt_rx_frame = None;
                self.ch[chi].lt_end_of_frame = true;
                self.ch[chi].hunt = true;
                // Check for queued frames
                self.lt_check_queued_frame(chi);
            }
            self.ch[chi].lt_rx_buff = 0; // CRC byte value
        }

        self.ch[chi].lt_rx_offset += 1;
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
            return None;
        }

        // Check channel A TX first (highest priority after master disable)
        if self.ch[0].tx_ip && self.ch[0].tx_ie {
            return Some(SccIrqPending::ATxEmpty);
        }

        // Channel B receive interrupt (based on RxIntMode)
        let b_rx_int = self.check_rx_interrupt(1);
        if b_rx_int {
            return Some(SccIrqPending::BRxAvailable);
        }

        // Channel B special receive (EndOfFrame)
        let b_rx_special = self.ch[1].lt_end_of_frame && self.ch[1].rx_int_mode != 0;
        if b_rx_special {
            return Some(SccIrqPending::BSpecialReceive);
        }

        // Channel B TX
        if self.ch[1].tx_ip && self.ch[1].tx_ie {
            return Some(SccIrqPending::BTxEmpty);
        }

        // Channel B special receive (EndOfFrame)
        if self.has_rx_available(0) && self.ch[0].rx_int_mode != 0 {
            return Some(SccIrqPending::ARxAvailable);
        }

        // External/status interrupts
        if self.ch[0].ext_ip && self.ch[0].ext_ie {
            return Some(SccIrqPending::AExtStatusChange);
        }
        if self.ch[1].ext_ip && self.ch[1].ext_ie {
            return Some(SccIrqPending::BExtStatusChange);
        }

        None
    }

    /// Check if a receive interrupt should fire based on RxIntMode
    fn check_rx_interrupt(&self, chi: usize) -> bool {
        let ch = &self.ch[chi];

        // For SDLC/LocalTalk, use lt_rx_chr_avail
        let rx_chr_avail = if ch.sdlc {
            ch.lt_rx_chr_avail
        } else {
            !ch.rx_queue.is_empty()
        };

        match ch.rx_int_mode {
            0 => false, // disabled
            1 => {
                // Rx INT on 1st char or special condition
                rx_chr_avail && ch.first_char
            }
            2 => {
                // INT on all Rx char or special condition
                rx_chr_avail
            }
            3 => false, // special condition only (handled separately)
            _ => false,
        }
    }

    /// Check if channel has RX data available (considers SDLC mode)
    fn has_rx_available(&self, chi: usize) -> bool {
        if self.ch[chi].sdlc {
            self.ch[chi].lt_rx_chr_avail
        } else {
            !self.ch[chi].rx_queue.is_empty()
        }
    }

    fn read_ctrl(&mut self, ch: SccCh) -> u8 {
        let chi = ch.to_usize().unwrap();

        // Determine RX char available based on mode
        let rx_char_avail = if self.ch[chi].sdlc {
            // SDLC/LocalTalk mode: use lt_rx_chr_avail flag
            self.ch[chi].lt_rx_chr_avail
        } else {
            // Normal mode: char available if rx_queue not empty
            !self.ch[chi].rx_queue.is_empty()
        };

        let result = match (self.reg, ch) {
            (0 | 4, _) => *RdReg0::default()
                .with_rx_char(rx_char_avail)
                .with_tx_empty(true)
                .with_tx_underrun(true)
                .with_sync_hunt(self.ch[chi].hunt)
                .with_dcd(self.ch[chi].dcd),
            (1 | 5, _) => *RdReg1::default()
                .with_all_sent(true)
                .with_end_of_frame(self.ch[chi].lt_end_of_frame),
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

        match reg {
            0 => {
                let r = WrReg0(val);
                // Register pointer
                self.reg = r.reg() as usize;

                match r.cmdcode().unwrap() {
                    SccCommand::Null => (),
                    SccCommand::ResetError => {
                        // Error Reset - clears EndOfFrame status
                        self.ch[chi].lt_end_of_frame = false;
                    }
                    SccCommand::PointHigh => self.reg |= 1 << 3,
                    SccCommand::ResetExtStatusInt => {
                        self.ch[chi].hunt = false;
                        self.ch[chi].ext_ip = false;
                        self.ch[chi].lt_end_of_frame = false;
                    }
                    SccCommand::ResetTxInt => {
                        self.ch[chi].tx_ip = false;
                    }
                    SccCommand::IntNextRx => {
                        // "Enable Int on next Rx char" - sets FirstChar flag
                        self.ch[chi].first_char = true;
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
                self.ch[chi].ext_ie = r.ext_ie();

                // RxIntMode from bits 3-4
                let new_rx_int_mode = r.rx_ie();
                if self.ch[chi].rx_int_mode != new_rx_int_mode {
                    self.ch[chi].rx_int_mode = new_rx_int_mode;
                    // When enabling "int on first char" mode, set first_char flag
                    if new_rx_int_mode == 1 {
                        self.ch[chi].first_char = true;
                    }
                }
            }
            2 => {
                self.intvec = val;
            }
            3 => {
                let r = WrReg3(val);
                let was_rx_enabled = self.ch[chi].rx_enable;

                // Enable SDLC mode FIRST if SDLC address search is enabled
                // This must happen before the poll_needed check below
                if r.sdlc_address_search() && !self.ch[chi].sdlc {
                    log::info!("SCC {:?}: SDLC mode enabled via address search mode", ch);
                    self.ch[chi].sdlc = true;
                }

                // Enter Hunt Mode - but only clear frame if there isn't one already waiting
                // The Mac writes hunt=1 to prepare for receiving, not to discard received data
                if r.hunt() && self.ch[chi].lt_rx_frame.is_none() {
                    self.ch[chi].hunt = true;
                    self.ch[chi].lt_end_of_frame = false;
                }
                // If RX is being disabled, clear the frame and chr_avail
                if !r.rx_enable() && was_rx_enabled {
                    self.ch[chi].hunt = true;
                    self.ch[chi].lt_rx_frame = None;
                    self.ch[chi].lt_end_of_frame = false;
                    self.ch[chi].lt_rx_chr_avail = false;
                }
                self.ch[chi].rx_enable = r.rx_enable();

                // If RX is being enabled (was disabled, now enabled), and we're in SDLC mode,
                // signal that LocalTalk bridge should be polled immediately
                if r.rx_enable() && !was_rx_enabled && self.ch[chi].sdlc {
                    self.ch[chi].localtalk_poll_needed = true;
                }
            }
            5 => {
                let r = WrReg5(val);
                self.ch[chi].tx_enable = r.tx_enable();
                self.ch[chi].tx_ip = false;
            }
            9 => {
                self.mic.0 = val;
            }
            14 => {
                // DPLL/baudrate generator
            }
            15 => {
                // WR15 controls external/status interrupt enables
                // Note: Bit 0 is WR7' enable, NOT SDLC mode enable
                // SDLC mode is controlled only by WR3 address search mode
                let wrval = WrReg15(val);
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

    /// Check and clear the LocalTalk poll needed flag for a channel.
    /// Returns true if the flag was set (meaning we should poll the bridge immediately).
    pub fn take_localtalk_poll_needed(&mut self, ch: SccCh) -> bool {
        let chi = ch.to_usize().unwrap();
        let needed = self.ch[chi].localtalk_poll_needed;
        self.ch[chi].localtalk_poll_needed = false;
        needed
    }

    /// Check if the SCC channel is ready to receive data (RxEnable && !RxChrAvail)
    pub fn is_rx_ready_for_data(&self, ch: SccCh) -> bool {
        let chi = ch.to_usize().unwrap();
        self.ch[chi].rx_enable && !self.ch[chi].lt_rx_chr_avail
    }

    pub fn push_rx(&mut self, ch: SccCh, data: &[u8]) {
        let chi = ch.to_usize().unwrap();
        if !self.ch[chi].rx_enable {
            return;
        }

        // For LocalTalk/SDLC mode, push as a complete frame
        if self.ch[chi].sdlc {
            self.push_rx_frame(ch, data.to_vec());
            return;
        }

        self.ch[chi].rx_queue.extend(data.iter());
        // Set interrupt if enabled (rx_int_mode 1 or 2)
        if self.mic.mie() && self.ch[chi].rx_int_mode != 0 {
            self.ch[chi].rx_ip = true;
        }
    }

    /// Push a complete LocalTalk/SDLC frame for reception
    pub fn push_rx_frame(&mut self, ch: SccCh, frame: Vec<u8>) {
        let chi = ch.to_usize().unwrap();
        if !self.ch[chi].rx_enable {
            return;
        }

        // If there's already a frame being received (chr_avail set), queue this one
        if self.ch[chi].lt_rx_chr_avail {
            // Queue for later - use rx_queue as a frame queue
            // Prefix with length so we can extract it later
            self.ch[chi].rx_queue.push_back((frame.len() >> 8) as u8);
            self.ch[chi].rx_queue.push_back(frame.len() as u8);
            self.ch[chi].rx_queue.extend(frame.iter());
            return;
        }

        // Start receiving this frame
        self.ch[chi].lt_rx_frame = Some(frame);
        self.ch[chi].lt_rx_offset = 0;
        self.ch[chi].lt_end_of_frame = false;
        self.ch[chi].hunt = false; // Exit hunt mode

        // Pre-buffer the first byte
        self.lt_rx_buff_advance(chi);

        // Set chr_avail and first_char to signal data is ready and trigger interrupt
        self.ch[chi].lt_rx_chr_avail = true;
        self.ch[chi].first_char = true; // For interrupt mode 1 ("int on first char")
    }

    /// Check if there's a queued frame and start receiving it
    fn lt_check_queued_frame(&mut self, chi: usize) {
        // Only start a new frame if chr_avail is false (no frame being processed)
        if !self.ch[chi].lt_rx_chr_avail && self.ch[chi].rx_queue.len() >= 2 {
            // Extract queued frame
            let len_hi = self.ch[chi].rx_queue.pop_front().unwrap() as usize;
            let len_lo = self.ch[chi].rx_queue.pop_front().unwrap() as usize;
            let len = (len_hi << 8) | len_lo;

            if self.ch[chi].rx_queue.len() >= len {
                let frame: Vec<u8> = self.ch[chi].rx_queue.drain(..len).collect();
                self.ch[chi].lt_rx_frame = Some(frame);
                self.ch[chi].lt_rx_offset = 0;
                self.ch[chi].lt_end_of_frame = false;
                self.ch[chi].hunt = false;

                // Pre-buffer the first byte
                self.lt_rx_buff_advance(chi);
                self.ch[chi].lt_rx_chr_avail = true;
                self.ch[chi].first_char = true; // For interrupt mode 1
            }
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
