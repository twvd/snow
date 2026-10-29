//! National Semiconductor DP83932 SONIC Ethernet controller
//!
//! Register map, descriptor formats and command behaviour follow the
//! DP83932C datasheet. The chip is a bus master: the descriptor areas and
//! the buffers are in the memory it reaches through [`SonicDma`], the Mac's
//! RAM or a card's own RAM. The machine models the bus clock (BSCK) and the
//! cost of the DMA cycles. This model only knows the chip's 20 MHz
//! oscillator, which paces the frames on the wire and clocks the
//! general-purpose timer.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;

use crate::bus::Address;
use crate::debuggable::{Debuggable, DebuggableProperties};
use crate::net::EthernetPort;
use crate::tickable::Ticks;

/// Memory as seen by the SONIC's DMA engine
pub trait SonicDma {
    fn read_byte(&mut self, addr: Address) -> u8;
    fn write_byte(&mut self, addr: Address, val: u8);
}

/// Oscillator frequency (X1)
const OSC_FREQUENCY: Ticks = 20_000_000;

/// Oscillator cycles per byte on the wire (10 Mbit/s)
const OSC_PER_BYTE: Ticks = 16;

/// Oscillator cycles per general-purpose timer count (TXC / 2)
const OSC_PER_TIMER: Ticks = 4;

/// Minimum frame length without the FCS
const MIN_FRAME: usize = 60;

/// Maximum frame length without the FCS
const MAX_FRAME: usize = 1514;

// User registers
const CR: usize = 0x00;
const DCR: usize = 0x01;
const RCR: usize = 0x02;
const TCR: usize = 0x03;
const IMR: usize = 0x04;
const ISR: usize = 0x05;
const UTDA: usize = 0x06;
const CTDA: usize = 0x07;
const URDA: usize = 0x0D;
const CRDA: usize = 0x0E;
const EOBC: usize = 0x13;
const URRA: usize = 0x14;
const RSA: usize = 0x15;
const REA: usize = 0x16;
const RRP: usize = 0x17;
const RWP: usize = 0x18;
const CEP: usize = 0x21;
const CAP2: usize = 0x22;
const CAP1: usize = 0x23;
const CAP0: usize = 0x24;
const CE: usize = 0x25;
const CDP: usize = 0x26;
const CDC: usize = 0x27;
const SR: usize = 0x28;
const WT0: usize = 0x29;
const WT1: usize = 0x2A;
const RSC: usize = 0x2B;
const CRCT: usize = 0x2C;
const FAET: usize = 0x2D;
const MPT: usize = 0x2E;
const DCR2: usize = 0x3F;

// Internal registers
const TPS: usize = 0x08;
const TFC: usize = 0x09;
const TSA0: usize = 0x0A;
const TSA1: usize = 0x0B;
const TFS: usize = 0x0C;
const CRBA0: usize = 0x0F;
const CRBA1: usize = 0x10;
const RBWC0: usize = 0x11;
const RBWC1: usize = 0x12;
const TRBA0: usize = 0x19;
const TRBA1: usize = 0x1A;
const LLFA: usize = 0x1F;
const TTDA: usize = 0x20;

const CR_HTX: u16 = 1 << 0;
const CR_TXP: u16 = 1 << 1;
const CR_RXDIS: u16 = 1 << 2;
const CR_RXEN: u16 = 1 << 3;
const CR_STP: u16 = 1 << 4;
const CR_ST: u16 = 1 << 5;
const CR_RST: u16 = 1 << 7;
const CR_RRRA: u16 = 1 << 8;
const CR_LCAM: u16 = 1 << 9;

const DCR_DW: u16 = 1 << 5;

const RCR_PRX: u16 = 1 << 0;
const RCR_LBK: u16 = 1 << 1;
const RCR_LPKT: u16 = 1 << 6;
const RCR_BC: u16 = 1 << 7;
const RCR_MC: u16 = 1 << 8;
const RCR_LB: u16 = 3 << 9;
const RCR_AMC: u16 = 1 << 11;
const RCR_PRO: u16 = 1 << 12;
const RCR_BRD: u16 = 1 << 13;
const RCR_RNT: u16 = 1 << 14;
/// The bits that the driver writes; the others are status bits of the last
/// packet
const RCR_CONFIG: u16 = 0xFE00;

const TCR_PTX: u16 = 1 << 0;
const TCR_BCM: u16 = 1 << 1;
const TCR_CRSL: u16 = 1 << 7;
const TCR_NCRS: u16 = 1 << 8;
const TCR_CRCI: u16 = 1 << 13;
const TCR_PINT: u16 = 1 << 15;
/// Bits loaded from TXpkt.config
const TCR_CONFIG: u16 = 0xF000;

const ISR_RBAE: u16 = 1 << 4;
const ISR_RBE: u16 = 1 << 5;
const ISR_RDE: u16 = 1 << 6;
const ISR_TC: u16 = 1 << 7;
const ISR_TXER: u16 = 1 << 8;
const ISR_TXDN: u16 = 1 << 9;
const ISR_PKTRX: u16 = 1 << 10;
const ISR_PINT: u16 = 1 << 11;
const ISR_LCD: u16 = 1 << 12;
const ISR_MP: u16 = 1 << 1;

/// End of list bit in the link fields
const EOL: u16 = 1;

/// A transmission on the wire
#[derive(Serialize, Deserialize, Clone, Copy)]
struct TxBusy {
    /// Oscillator cycles until the frame and the interframe gap are over
    remaining: Ticks,
}

/// DP83932 SONIC
#[derive(Serialize, Deserialize)]
pub struct Sonic {
    #[serde(with = "BigArray")]
    regs: [u16; 64],
    cam: [[u8; 6]; 16],

    /// The connection to the host's network
    port: EthernetPort,

    /// The fraction of an oscillator cycle that `tick()` hasn't counted yet, in
    /// units of 1/`frequency` of a cycle
    osc_clock: Ticks,
    /// Oscillator cycles towards the next timer count
    timer_clock: Ticks,
    /// Transmission in progress
    tx_busy: Option<TxBusy>,
    /// Oscillator cycles until the receiver can take the next frame
    rx_busy: Ticks,
    /// All receive buffers are used up, reception stopped (RBE)
    rx_buffers_exhausted: bool,
    /// Frames looped back by the MAC loopback mode
    loopback: VecDeque<Vec<u8>>,
}

impl Sonic {
    /// Creates a SONIC with the given silicon revision (SR register), connected
    /// to the host's network through `port`
    pub fn new(revision: u16, mut port: EthernetPort) -> Self {
        // The SONIC filters the addresses itself, with its CAM: the port passes all
        // frames
        port.set_promiscuous(true);
        let mut s = Self {
            regs: [0; 64],
            cam: [[0; 6]; 16],
            port,
            osc_clock: 0,
            timer_clock: 0,
            tx_busy: None,
            rx_busy: 0,
            rx_buffers_exhausted: false,
            loopback: VecDeque::new(),
        };
        s.regs[SR] = revision;
        s.reset();
        s
    }

    /// Hardware reset
    pub fn reset(&mut self) {
        self.regs[CR] = CR_RST | CR_STP | CR_RXDIS;
        self.regs[DCR] &= !((1 << 15) | (1 << 13));
        self.regs[RCR] &= !(RCR_RNT | RCR_BRD | RCR_LB);
        self.regs[TCR] = TCR_NCRS | TCR_PTX;
        self.regs[IMR] = 0;
        self.regs[ISR] = 0;
        self.regs[CE] = 0;
        self.regs[EOBC] = 0x02F8;
        self.regs[RSC] = 0;
        self.regs[DCR2] &= 0xF000;
        self.tx_busy = None;
        self.rx_busy = 0;
        self.rx_buffers_exhausted = false;
        self.loopback.clear();
        self.update_rx_enabled();
    }

    /// After a state was loaded: tells the port again whether the receiver is
    /// on, and reopens the link to the host's network, which the state doesn't
    /// hold
    pub fn after_deserialize(&mut self) {
        self.update_rx_enabled();
        if let Err(e) = self.port.set_link(self.port.link()) {
            log::warn!("Ethernet link: {:#}", e);
        }
    }

    /// Tells the port whether the receiver is on, so that the port drops the
    /// frames from the network while it is off. The chip counts the valid
    /// packets that arrive while RXDIS is set in MPT, its missed packet tally.
    /// The model doesn't, as it never sees those frames.
    fn update_rx_enabled(&mut self) {
        let enabled = self.regs[CR] & CR_RXEN != 0;
        if self.port.rx_enabled() != enabled {
            self.port.set_rx_enabled(enabled);
        }
    }

    pub fn port(&self) -> &EthernetPort {
        &self.port
    }

    pub fn port_mut(&mut self) -> &mut EthernetPort {
        &mut self.port
    }

    /// Interrupt output (active high here; the board inverts it as needed)
    pub fn irq(&self) -> bool {
        self.regs[ISR] & self.regs[IMR] != 0
    }

    /// Reads a register
    pub fn read(&self, reg: usize) -> u16 {
        let reg = reg & 0x3F;
        match reg {
            CAP0 | CAP1 | CAP2 => {
                if self.regs[CR] & CR_RST == 0 {
                    return 0;
                }
                let e = &self.cam[usize::from(self.regs[CEP] & 0xF)];
                let i = 2 * (CAP0 - reg);
                u16::from_le_bytes([e[i], e[i + 1]])
            }
            _ => self.regs[reg],
        }
    }

    /// Writes a register
    pub fn write(&mut self, reg: usize, val: u16, dma: &mut impl SonicDma) {
        let reg = reg & 0x3F;
        log::trace!("SONIC: write {:02X} = {:04X}", reg, val);
        let in_reset = self.regs[CR] & CR_RST != 0;
        match reg {
            CR => self.command(val, dma),
            DCR | DCR2 | CE if !in_reset => {
                log::debug!("SONIC: write to register {:02X} outside reset ignored", reg);
            }
            RCR => self.regs[RCR] = (self.regs[RCR] & !RCR_CONFIG) | (val & RCR_CONFIG),
            TCR => self.regs[TCR] = (self.regs[TCR] & !TCR_CONFIG) | (val & TCR_CONFIG),
            IMR => self.regs[IMR] = val & 0x7FFF,
            ISR => {
                self.regs[ISR] &= !val;
                // Clearing RBE after adding resources resumes reception
                if val & ISR_RBE != 0
                    && self.rx_buffers_exhausted
                    && self.regs[RRP] != self.regs[RWP]
                {
                    self.read_rra(dma);
                }
            }
            RSA | REA | RRP | RWP | CDP => self.regs[reg] = val & !1,
            CRCT | FAET | MPT => self.regs[reg] = !val,
            CAP0 | CAP1 | CAP2 | SR => (),
            CDC => self.regs[CDC] = val & 0x1F,
            CEP => self.regs[CEP] = val & 0xF,
            _ => self.regs[reg] = val,
        }
    }

    fn command(&mut self, val: u16, dma: &mut impl SonicDma) {
        if val & CR_RST != 0 {
            // Software reset
            self.regs[CR] &= !(CR_LCAM | CR_RRRA | CR_TXP | CR_HTX | CR_RXEN);
            self.regs[CR] |= CR_RST | CR_RXDIS;
            self.tx_busy = None;
            self.update_rx_enabled();
            return;
        }
        if self.regs[CR] & CR_RST != 0 {
            // A write without RST takes the chip out of reset and does nothing else:
            // the other command bits need a write of their own
            self.regs[CR] &= !CR_RST;
            return;
        }

        // HTX is only sampled after a TXpkt.status write, so with nothing
        // to transmit it stays set until a halt or a reset. The Mac OS
        // driver depends on this: it writes HTX while idle and polls CR
        // until it reads HTX back, then resets the chip. Clearing it without
        // a transmission hung 7.5.5 at startup with EtherTalk selected.
        if val & CR_HTX != 0 {
            self.regs[CR] |= CR_HTX;
        }
        if val & CR_RXDIS != 0 {
            self.regs[CR] = (self.regs[CR] & !CR_RXEN) | CR_RXDIS;
        } else if val & CR_RXEN != 0 {
            self.regs[CR] = (self.regs[CR] & !CR_RXDIS) | CR_RXEN;
        }
        self.update_rx_enabled();
        if val & CR_STP != 0 {
            self.regs[CR] = (self.regs[CR] & !CR_ST) | CR_STP;
        } else if val & CR_ST != 0 {
            self.regs[CR] = (self.regs[CR] & !CR_STP) | CR_ST;
        }
        if val & CR_RRRA != 0 {
            self.read_rra(dma);
        }
        if val & CR_LCAM != 0 {
            self.load_cam(dma);
        }
        if val & CR_TXP != 0 && self.tx_busy.is_none() {
            self.regs[CR] |= CR_TXP;
            self.start_tx(dma);
        }
    }

    /// Advances by `ticks` of a clock running at `frequency`
    pub fn tick(&mut self, ticks: Ticks, frequency: Ticks, dma: &mut impl SonicDma) {
        self.osc_clock += ticks * OSC_FREQUENCY;
        let osc = self.osc_clock / frequency;
        self.osc_clock -= osc * frequency;
        if osc == 0 {
            return;
        }

        if self.regs[CR] & CR_ST != 0 {
            self.tick_timer(osc);
        }

        if let Some(busy) = &mut self.tx_busy {
            busy.remaining = busy.remaining.saturating_sub(osc);
            if busy.remaining == 0 {
                self.tx_busy = None;
                self.finish_tx(dma);
            }
        }

        // Frames come from the loopback queue first, then from the port. The port
        // is only read while the receiver is enabled.
        self.rx_busy = self.rx_busy.saturating_sub(osc);
        if self.rx_busy == 0 && (self.regs[CR] & CR_RXEN != 0 || !self.loopback.is_empty()) {
            let frame = match self.loopback.pop_front() {
                Some(frame) => Some((frame, true)),
                None => self.port.recv().map(|f| (f, false)),
            };
            if let Some((frame, looped)) = frame {
                self.rx_busy = Self::wire_time(frame.len());
                self.receive(frame, looped, dma);
            } else {
                // Frames on the wire are at least a minimum frame time apart,
                // so the port is only checked that often. Checking it on every
                // tick cost about 11% of the time of an idle Quadra 700; this
                // costs 3%.
                self.rx_busy = Self::wire_time(0);
            }
        }
    }

    fn tick_timer(&mut self, osc: Ticks) {
        self.timer_clock += osc;
        let counts = self.timer_clock / OSC_PER_TIMER;
        self.timer_clock -= counts * OSC_PER_TIMER;
        let value = (u32::from(self.regs[WT1]) << 16) | u32::from(self.regs[WT0]);
        if Ticks::from(value) < counts {
            self.regs[ISR] |= ISR_TC;
        }
        let value = value.wrapping_sub(counts as u32);
        self.regs[WT1] = (value >> 16) as u16;
        self.regs[WT0] = value as u16;
    }

    /// Oscillator cycles a frame of `len` bytes (without FCS) occupies the
    /// wire, with the preamble, FCS and interframe gap
    fn wire_time(len: usize) -> Ticks {
        (8 + len.max(MIN_FRAME) as Ticks + 4 + 12) * OSC_PER_BYTE
    }

    fn wide(&self) -> bool {
        self.regs[DCR] & DCR_DW != 0
    }

    /// Reads a descriptor field: 16-bit fields, at a long word stride in
    /// 32-bit mode with the data on D15-D0 (big endian bus)
    fn desc_read(&self, dma: &mut impl SonicDma, base: Address, field: Address) -> u16 {
        let addr = if self.wide() {
            base + field * 4 + 2
        } else {
            base + field * 2
        };
        u16::from_be_bytes([dma.read_byte(addr), dma.read_byte(addr + 1)])
    }

    fn desc_write(&self, dma: &mut impl SonicDma, base: Address, field: Address, val: u16) {
        let addr = if self.wide() {
            base + field * 4 + 2
        } else {
            base + field * 2
        };
        let [hi, lo] = val.to_be_bytes();
        dma.write_byte(addr, hi);
        dma.write_byte(addr + 1, lo);
    }

    fn desc_size(&self) -> u16 {
        if self.wide() { 4 } else { 2 }
    }

    fn addr(upper: u16, lower: u16) -> Address {
        (Address::from(upper) << 16) | Address::from(lower & !1)
    }

    fn reg32(&self, lo: usize, hi: usize) -> u32 {
        (u32::from(self.regs[hi]) << 16) | u32::from(self.regs[lo])
    }

    fn set_reg32(&mut self, lo: usize, hi: usize, val: u32) {
        self.regs[lo] = val as u16;
        self.regs[hi] = (val >> 16) as u16;
    }

    /// Load CAM command
    fn load_cam(&mut self, dma: &mut impl SonicDma) {
        while self.regs[CDC] & 0x1F != 0 {
            let base = Self::addr(self.regs[URRA], self.regs[CDP]);
            let entry = usize::from(self.desc_read(dma, base, 0) & 0xF);
            for i in 0..3 {
                let word = self.desc_read(dma, base, 1 + i as Address);
                self.cam[entry][2 * i..2 * i + 2].copy_from_slice(&word.to_le_bytes());
            }
            self.regs[CDC] -= 1;
            self.regs[CDP] = self.regs[CDP].wrapping_add(4 * self.desc_size());
        }
        let base = Self::addr(self.regs[URRA], self.regs[CDP]);
        self.regs[CE] = self.desc_read(dma, base, 0);
        self.regs[CDP] = self.regs[CDP].wrapping_add(self.desc_size());
        self.regs[ISR] |= ISR_LCD;
    }

    /// Reads the next resource descriptor from the RRA
    fn read_rra(&mut self, dma: &mut impl SonicDma) {
        let base = Self::addr(self.regs[URRA], self.regs[RRP]);
        self.regs[CRBA0] = self.desc_read(dma, base, 0);
        self.regs[CRBA1] = self.desc_read(dma, base, 1);
        self.regs[RBWC0] = self.desc_read(dma, base, 2);
        self.regs[RBWC1] = self.desc_read(dma, base, 3);
        self.rx_buffers_exhausted = false;

        self.regs[RRP] = self.regs[RRP].wrapping_add(4 * self.desc_size());
        if self.regs[RRP] == self.regs[REA] {
            self.regs[RRP] = self.regs[RSA];
        }
        if self.regs[RRP] == self.regs[RWP] {
            // RRP has reached RWP: the resource just read is the last one.
            // RBE warns the driver before reception has to stop.
            self.regs[ISR] |= ISR_RBE;
        }
    }

    /// Starts transmitting the packet at CTDA
    fn start_tx(&mut self, dma: &mut impl SonicDma) {
        self.regs[TTDA] = self.regs[CTDA];
        let base = Self::addr(self.regs[UTDA], self.regs[TTDA]);
        self.regs[TCR] =
            (self.regs[TCR] & !TCR_CONFIG) | (self.desc_read(dma, base, 1) & TCR_CONFIG);
        self.regs[TPS] = self.desc_read(dma, base, 2);
        self.regs[TFC] = self.desc_read(dma, base, 3);
        if self.regs[TCR] & TCR_PINT != 0 {
            self.regs[ISR] |= ISR_PINT;
        }

        // Clear the status bits that the previous packet left in TCR. Set
        // NCRS ("no carrier sense"): send_packet clears it again once the
        // frame has gone out on the wire, where the SONIC sees the carrier.
        self.regs[TCR] &= TCR_CONFIG;
        self.regs[TCR] |= TCR_NCRS;
        self.tx_busy = Some(TxBusy {
            remaining: Self::wire_time(usize::from(self.regs[TPS])),
        });
    }

    /// Reads the packet in progress from memory and sends it.
    ///
    /// The SONIC doesn't read a packet from memory all at once when TXP is
    /// written. It reads it a few words at a time into its 32-byte transmit
    /// FIFO, while the serializer sends the bytes already in the FIFO
    /// (DP83932 datasheet 1.4.2 "Transmit FIFO"). So each byte is read from
    /// memory at most 32 byte times (26 us at 10 Mbit/s) before it is sent.
    ///
    /// Dayna's E/si30 driver depends on this: for a large packet, it writes
    /// TXP before it has finished copying the packet into its transmit
    /// buffer, and copies the rest while the SONIC sends it.
    ///
    /// The FIFO isn't modelled: the whole packet is read from memory here,
    /// once its time on the wire is over (`tx_busy`). A driver can't tell the
    /// difference, as it can't reuse the buffer before the packet is sent.
    fn send_packet(&mut self, dma: &mut impl SonicDma) {
        let base = Self::addr(self.regs[UTDA], self.regs[TTDA]);
        let mut frame = vec![];
        for i in 0..Address::from(self.regs[TFC]) {
            self.regs[TSA0] = self.desc_read(dma, base, 4 + 3 * i);
            self.regs[TSA1] = self.desc_read(dma, base, 5 + 3 * i);
            self.regs[TFS] = self.desc_read(dma, base, 6 + 3 * i);
            let start = self.reg32(TSA0, TSA1);
            for a in 0..Address::from(self.regs[TFS]) {
                frame.push(dma.read_byte(start.wrapping_add(a)));
            }
        }

        if frame.len() != usize::from(self.regs[TPS]) {
            self.regs[TCR] |= TCR_BCM;
        } else {
            if self.regs[TCR] & TCR_CRCI != 0 {
                // With CRCI, the packet in memory ends with its FCS: leave it
                // out, as the host's network takes frames without one
                frame.truncate(frame.len().saturating_sub(4));
            }
            if self.regs[RCR] & RCR_LB != 0 {
                self.regs[TCR] |= TCR_CRSL;
                self.loopback.push_back(frame.clone());
            } else {
                self.port.send(&frame);
                // The SONIC saw the carrier during the transmission
                self.regs[TCR] &= !TCR_NCRS;
            }
            self.regs[TCR] |= TCR_PTX;
        }
    }

    /// Ends the packet in progress: sends it, writes its status into its
    /// descriptor, then starts the next packet if there is one
    fn finish_tx(&mut self, dma: &mut impl SonicDma) {
        self.send_packet(dma);
        let base = Self::addr(self.regs[UTDA], self.regs[TTDA]);
        self.desc_write(dma, base, 0, self.regs[TCR] & 0x0FFF);

        let error = self.regs[TCR] & TCR_BCM != 0;
        if error {
            self.regs[ISR] |= ISR_TXER;
        }
        if self.regs[CR] & CR_HTX != 0 || error {
            self.end_tx();
            return;
        }
        let link = self.desc_read(dma, base, 4 + 3 * Address::from(self.regs[TFC]));
        self.regs[CTDA] = link;
        if link & EOL != 0 {
            self.end_tx();
        } else {
            self.start_tx(dma);
        }
    }

    fn end_tx(&mut self) {
        self.regs[CR] &= !(CR_TXP | CR_HTX);
        self.regs[ISR] |= ISR_TXDN;
    }

    /// Address filter: the RCR status bits of the frame (broadcast or
    /// multicast) if it is accepted, None if it isn't
    fn accept(&self, frame: &[u8]) -> Option<u16> {
        let dest: [u8; 6] = frame[0..6].try_into().unwrap();
        let rcr = self.regs[RCR];
        let multicast = dest[0] & 1 != 0;
        let broadcast = dest == [0xFF; 6];
        let cast = if broadcast {
            RCR_BC
        } else if multicast {
            RCR_MC
        } else {
            0
        };

        if rcr & RCR_PRO != 0 && !multicast {
            return Some(0);
        }
        if rcr & RCR_AMC != 0 && multicast {
            return Some(cast);
        }
        if rcr & RCR_BRD != 0 && broadcast {
            return Some(cast);
        }
        let in_cam = (0..16).any(|i| self.regs[CE] & (1 << i) != 0 && self.cam[i] == dest);
        in_cam.then_some(cast)
    }

    /// Buffers a received frame (without FCS)
    fn receive(&mut self, mut frame: Vec<u8>, looped: bool, dma: &mut impl SonicDma) {
        if frame.len() < 14 || frame.len() > MAX_FRAME {
            return;
        }
        // Frames from the host's network may be shorter than the minimum
        // Ethernet frame: pad them
        if frame.len() < MIN_FRAME {
            frame.resize(MIN_FRAME, 0);
        }
        if self.regs[CR] & CR_RXEN == 0 {
            self.missed_packet();
            return;
        }
        let Some(cast) = self.accept(&frame) else {
            return;
        };
        if self.rx_buffers_exhausted {
            self.missed_packet();
            return;
        }

        // CRDA is on the last descriptor in the list: read its link field
        // again, to see whether the driver has added more descriptors
        if self.regs[LLFA] & EOL != 0 {
            let desc = Self::addr(self.regs[URDA], self.regs[CRDA]);
            self.regs[LLFA] = self.desc_read(dma, desc, 5);
            if self.regs[LLFA] & EOL != 0 {
                self.missed_packet();
                return;
            }
            self.desc_write(dma, desc, 6, 0);
            self.regs[CRDA] = self.regs[LLFA];
        }

        self.regs[RCR] &= RCR_CONFIG;
        let fcs = crc::Crc::<u32>::new(&crc::CRC_32_ISO_HDLC).checksum(&frame);
        frame.extend_from_slice(&fcs.to_le_bytes());
        let rx_len = frame.len();
        let align = if self.wide() { 4 } else { 2 };
        let padded = rx_len.div_ceil(align) * align;

        let words = self.reg32(RBWC0, RBWC1);
        if padded / 2 > words as usize {
            // The packet doesn't fit in what is left of the buffer: drop it,
            // and move to the next buffer
            self.regs[ISR] |= ISR_RBAE;
            self.next_buffer(dma);
            return;
        }

        self.regs[TRBA0] = self.regs[CRBA0];
        self.regs[TRBA1] = self.regs[CRBA1];
        let start = self.reg32(CRBA0, CRBA1);
        for (i, b) in frame.iter().enumerate() {
            dma.write_byte(start.wrapping_add(i as Address), *b);
        }
        self.set_reg32(CRBA0, CRBA1, start.wrapping_add(padded as Address));
        let words = words - (padded / 2) as u32;
        self.set_reg32(RBWC0, RBWC1, words);

        let last_in_buffer = words < u32::from(self.regs[EOBC]);
        if last_in_buffer {
            self.regs[RCR] |= RCR_LPKT;
        }
        self.regs[RCR] |= cast | RCR_PRX;
        if looped {
            self.regs[RCR] |= RCR_LBK;
        }

        let desc = Self::addr(self.regs[URDA], self.regs[CRDA]);
        self.desc_write(dma, desc, 0, self.regs[RCR]);
        self.desc_write(dma, desc, 1, rx_len as u16);
        self.desc_write(dma, desc, 2, self.regs[TRBA0]);
        self.desc_write(dma, desc, 3, self.regs[TRBA1]);
        self.desc_write(dma, desc, 4, self.regs[RSC]);
        self.regs[LLFA] = self.desc_read(dma, desc, 5);
        if self.regs[LLFA] & EOL != 0 {
            // CRDA stays on this descriptor until the driver links more
            self.regs[ISR] |= ISR_RDE;
        } else {
            self.desc_write(dma, desc, 6, 0);
            self.regs[CRDA] = self.regs[LLFA];
        }
        self.regs[ISR] |= ISR_PKTRX;
        self.regs[RSC] = (self.regs[RSC] & 0xFF00) | (self.regs[RSC].wrapping_add(1) & 0xFF);

        if last_in_buffer {
            self.next_buffer(dma);
        }
    }

    /// Moves to the next receive buffer, or stops reception if there is none
    fn next_buffer(&mut self, dma: &mut impl SonicDma) {
        if self.regs[RRP] == self.regs[RWP] {
            self.rx_buffers_exhausted = true;
            return;
        }
        self.read_rra(dma);
        self.regs[RSC] = (self.regs[RSC] & 0xFF00).wrapping_add(0x100);
    }

    fn missed_packet(&mut self) {
        // The tally counters are halted in reset
        if self.regs[CR] & CR_RST != 0 {
            return;
        }
        self.regs[MPT] = self.regs[MPT].wrapping_add(1);
        if self.regs[MPT] == 0 {
            self.regs[ISR] |= ISR_MP;
        }
    }
}

impl Debuggable for Sonic {
    fn get_debug_properties(&self) -> DebuggableProperties {
        use crate::debuggable::*;
        use crate::{dbgprop_group, dbgprop_string, dbgprop_word};

        let mac = self.port.mac();
        let mut result = vec![
            dbgprop_string!(
                "MAC address",
                format!(
                    "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                    mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
                )
            ),
            dbgprop_word!("CR", self.regs[CR]),
            dbgprop_word!("DCR", self.regs[DCR]),
            dbgprop_word!("RCR", self.regs[RCR]),
            dbgprop_word!("TCR", self.regs[TCR]),
            dbgprop_word!("IMR", self.regs[IMR]),
            dbgprop_word!("ISR", self.regs[ISR]),
            dbgprop_word!("CE", self.regs[CE]),
            dbgprop_word!("MPT", self.regs[MPT]),
        ];
        result.push(dbgprop_group!(
            "CAM",
            (0..16)
                .filter(|&i| self.regs[CE] & (1 << i) != 0)
                .map(|i| {
                    let e = self.cam[i];
                    dbgprop_string!(
                        format!("Entry {}", i),
                        format!(
                            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                            e[0], e[1], e[2], e[3], e[4], e[5],
                        )
                    )
                })
                .collect()
        ));
        result.extend(self.port.debug_properties());
        result
    }
}

const fn fits_quadra700(mac: crate::mac::MacModel) -> bool {
    matches!(mac, crate::mac::MacModel::Quadra700)
}

/// The SONIC on the logic board of a Quadra 700
pub const INFO: crate::emulator::devices::ModelInfo = crate::emulator::devices::ModelInfo {
    model: crate::emulator::devices::DeviceModel::BuiltInSonic,
    name: "Built-in Ethernet",
    bus: crate::emulator::devices::BusKind::BuiltIn,
    fits: fits_quadra700,
    function: crate::emulator::devices::FunctionKind::Ethernet,
    fixed: true,
    one_per_bus: false,
    rom: None,
    path_keys: &[],
    adds_buses: &[],
    monitors: &[],
    create: None,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::EthernetLinkType;

    const MAC: [u8; 6] = [0x08, 0x00, 0x07, 0x12, 0x34, 0x56];
    const RRA: u16 = 0x1000;
    const RDA: u16 = 0x2000;
    const TDA: u16 = 0x3000;
    const CDA: u16 = 0x4000;
    const RBA: u32 = 0x1_0000;
    const RBA_WORDS: u32 = 0x4000;
    const FREQ: Ticks = 20_000_000;

    struct Ram(Vec<u8>);

    impl SonicDma for Ram {
        fn read_byte(&mut self, addr: Address) -> u8 {
            self.0[addr as usize]
        }

        fn write_byte(&mut self, addr: Address, val: u8) {
            self.0[addr as usize] = val;
        }
    }

    impl Ram {
        /// Writes a 32-bit mode descriptor field
        fn field(&mut self, base: u16, field: u32, val: u16) {
            let addr = u32::from(base) + field * 4;
            self.0[addr as usize..addr as usize + 4].copy_from_slice(&u32::from(val).to_be_bytes());
        }

        fn get(&self, base: u16, field: u32) -> u16 {
            let addr = (u32::from(base) + field * 4 + 2) as usize;
            u16::from_be_bytes([self.0[addr], self.0[addr + 1]])
        }
    }

    /// A SONIC in 32-bit mode with the CAM holding MAC, one receive buffer
    /// and two receive descriptors, receiver enabled
    fn setup() -> (Sonic, Ram) {
        let mut port = EthernetPort::new(MAC);
        port.set_link(EthernetLinkType::Down).unwrap();
        let mut s = Sonic::new(4, port);
        let mut ram = Ram(vec![0; 0x2_0000]);

        // CAM descriptor: entry 0, then the enable mask
        ram.field(CDA, 0, 0);
        ram.field(CDA, 1, u16::from_le_bytes([MAC[0], MAC[1]]));
        ram.field(CDA, 2, u16::from_le_bytes([MAC[2], MAC[3]]));
        ram.field(CDA, 3, u16::from_le_bytes([MAC[4], MAC[5]]));
        ram.field(CDA, 4, 1);

        // One resource, with RWP just past it
        ram.field(RRA, 0, RBA as u16);
        ram.field(RRA, 1, (RBA >> 16) as u16);
        ram.field(RRA, 2, RBA_WORDS as u16);
        ram.field(RRA, 3, (RBA_WORDS >> 16) as u16);

        // Two receive descriptors, the second one last
        ram.field(RDA, 5, RDA + 7 * 4);
        ram.field(RDA, 6, 1);
        ram.field(RDA + 7 * 4, 5, EOL);
        ram.field(RDA + 7 * 4, 6, 1);

        s.write(DCR, DCR_DW, &mut ram);
        s.write(CDP, CDA, &mut ram);
        s.write(CDC, 1, &mut ram);
        s.write(CR, 0, &mut ram);
        s.write(CR, CR_LCAM, &mut ram);
        s.write(RSA, RRA, &mut ram);
        s.write(REA, RRA + 4 * 4 * 4, &mut ram);
        s.write(RRP, RRA, &mut ram);
        s.write(RWP, RRA + 4 * 4, &mut ram);
        s.write(CRDA, RDA, &mut ram);
        s.write(RCR, RCR_BRD, &mut ram);
        s.write(CR, CR_RRRA, &mut ram);
        s.write(ISR, 0xFFFF, &mut ram);
        s.write(CR, CR_RXEN, &mut ram);
        (s, ram)
    }

    fn frame(dest: [u8; 6], len: usize) -> Vec<u8> {
        let mut f = dest.to_vec();
        f.extend_from_slice(&[0x00, 0x05, 0x02, 0xAA, 0xBB, 0xCC, 0x08, 0x00]);
        f.extend((0..len - 14).map(|i| i as u8));
        f
    }

    #[test]
    fn load_cam() {
        let mut port = EthernetPort::new(MAC);
        port.set_link(EthernetLinkType::Down).unwrap();
        let mut s = Sonic::new(4, port);
        let mut ram = Ram(vec![0; 0x1_0000]);
        ram.field(CDA, 0, 3);
        ram.field(CDA, 1, 0x0008);
        ram.field(CDA, 2, 0x1207);
        ram.field(CDA, 3, 0x5634);
        ram.field(CDA, 4, 1 << 3);

        s.write(DCR, DCR_DW, &mut ram);
        s.write(CDP, CDA, &mut ram);
        s.write(CDC, 1, &mut ram);
        s.write(CR, 0, &mut ram);
        s.write(CR, CR_LCAM, &mut ram);
        assert_ne!(s.read(ISR) & ISR_LCD, 0);
        assert_eq!(s.read(CE), 1 << 3);
        assert_eq!(s.read(CDC), 0);
        assert_eq!(s.read(CDP), CDA + 5 * 4);
        assert_eq!(s.cam[3], MAC);

        // The CAM reads back through the address ports in reset only
        s.write(CR, CR_RST, &mut ram);
        s.write(CEP, 3, &mut ram);
        assert_eq!(s.read(CAP0), 0x0008);
        assert_eq!(s.read(CAP1), 0x1207);
        assert_eq!(s.read(CAP2), 0x5634);
    }

    #[test]
    fn registers_writable_in_reset_only() {
        let mut port = EthernetPort::new(MAC);
        port.set_link(EthernetLinkType::Down).unwrap();
        let mut s = Sonic::new(4, port);
        let mut ram = Ram(vec![0; 16]);
        s.write(DCR, DCR_DW, &mut ram);
        s.write(CR, 0, &mut ram);
        s.write(DCR, 0, &mut ram);
        assert_eq!(s.read(DCR), DCR_DW);
        assert_eq!(s.read(SR), 4);
        s.write(MPT, 0xFFFF, &mut ram);
        assert_eq!(s.read(MPT), 0);
    }

    #[test]
    fn receive_into_buffer() {
        let (mut s, mut ram) = setup();
        let tx = s.port.connect_test_rx();
        tx.send(frame(MAC, 100)).unwrap();
        s.tick(1, FREQ, &mut ram);

        assert_ne!(s.read(ISR) & ISR_PKTRX, 0);
        assert_eq!(ram.get(RDA, 0) & RCR_PRX, RCR_PRX);
        assert_eq!(ram.get(RDA, 1), 104);
        assert_eq!(ram.get(RDA, 2), RBA as u16);
        assert_eq!(ram.get(RDA, 3), (RBA >> 16) as u16);
        // The descriptor is handed back to the driver, and CRDA moves to
        // the next one
        assert_eq!(ram.get(RDA, 6), 0);
        assert_eq!(s.read(CRDA), RDA + 7 * 4);
        assert_eq!(&ram.0[RBA as usize..RBA as usize + 6], &MAC);
        assert_eq!(s.read(CRBA0), (RBA + 104) as u16);
    }

    #[test]
    fn receive_filter() {
        let (mut s, mut ram) = setup();
        let tx = s.port.connect_test_rx();
        tx.send(frame([0x00, 0x05, 0x02, 0x01, 0x02, 0x03], 64))
            .unwrap();
        s.tick(1, FREQ, &mut ram);
        assert_eq!(s.read(ISR) & ISR_PKTRX, 0);

        // Broadcasts with BRD
        tx.send(frame([0xFF; 6], 64)).unwrap();
        s.tick(Sonic::wire_time(64), FREQ, &mut ram);
        assert_ne!(s.read(ISR) & ISR_PKTRX, 0);
        assert_eq!(ram.get(RDA, 0) & RCR_BC, RCR_BC);
    }

    #[test]
    fn descriptors_exhausted() {
        let (mut s, mut ram) = setup();
        let tx = s.port.connect_test_rx();
        for _ in 0..3 {
            tx.send(frame(MAC, 64)).unwrap();
        }
        s.tick(1, FREQ, &mut ram);
        s.tick(Sonic::wire_time(64), FREQ, &mut ram);
        // The second (last) descriptor stays with the SONIC
        assert_ne!(s.read(ISR) & ISR_RDE, 0);
        assert_eq!(ram.get(RDA + 7 * 4, 6), 1);
        s.tick(Sonic::wire_time(64), FREQ, &mut ram);
        assert_eq!(s.read(MPT), 1);
    }

    #[test]
    fn transmit_in_loopback() {
        let (mut s, mut ram) = setup();
        s.write(RCR, RCR_BRD | (1 << 9), &mut ram);

        // Two fragments: header and payload
        let f = frame(MAC, 80);
        ram.0[0x5000..0x5000 + 14].copy_from_slice(&f[..14]);
        ram.field(TDA, 1, 0);
        ram.field(TDA, 2, 80);
        ram.field(TDA, 3, 2);
        ram.field(TDA, 4, 0x5000);
        ram.field(TDA, 5, 0);
        ram.field(TDA, 6, 14);
        ram.field(TDA, 7, 0x6001);
        ram.field(TDA, 8, 0);
        ram.field(TDA, 9, 66);
        ram.field(TDA, 10, (TDA + 0x40) | EOL);
        s.write(CTDA, TDA, &mut ram);
        s.write(CR, CR_TXP, &mut ram);
        assert_ne!(s.read(CR) & CR_TXP, 0);
        // The payload might still be in the process of being written after
        // the packet has started: see the comment on Sonic::send_packet
        ram.0[0x6001..0x6001 + 66].copy_from_slice(&f[14..]);

        s.tick(Sonic::wire_time(80), FREQ, &mut ram);
        assert_eq!(s.read(CR) & CR_TXP, 0);
        assert_ne!(s.read(ISR) & ISR_TXDN, 0);
        assert_eq!(ram.get(TDA, 0) & TCR_PTX, TCR_PTX);
        assert_eq!(s.read(CTDA), (TDA + 0x40) | EOL);

        // The frame came back to the receiver
        s.tick(1, FREQ, &mut ram);
        assert_ne!(s.read(ISR) & ISR_PKTRX, 0);
        assert_eq!(ram.get(RDA, 0) & RCR_LBK, RCR_LBK);
        assert_eq!(&ram.0[RBA as usize..RBA as usize + 80], &f[..]);
    }

    #[test]
    fn dropped_while_disabled() {
        let (mut s, mut ram) = setup();
        assert!(s.port.rx_enabled());
        let tx = s.port.connect_test_rx();
        tx.send(frame(MAC, 100)).unwrap();

        // Disabling the receiver drops the frames that the port has queued
        s.write(CR, CR_RXDIS, &mut ram);
        assert!(!s.port.rx_enabled());
        s.write(CR, CR_RXEN, &mut ram);
        assert!(s.port.rx_enabled());
        s.tick(Sonic::wire_time(100), FREQ, &mut ram);
        assert_eq!(s.read(ISR) & ISR_PKTRX, 0);

        s.write(CR, CR_RST, &mut ram);
        assert!(!s.port.rx_enabled());
    }

    #[test]
    fn halt_while_idle() {
        let (mut s, mut ram) = setup();
        s.write(CR, CR_HTX, &mut ram);
        assert_eq!(s.read(CR) & CR_HTX, CR_HTX);
        s.write(CR, CR_RST, &mut ram);
        assert_eq!(s.read(CR) & CR_HTX, 0);
    }

    #[test]
    fn timer() {
        let (mut s, mut ram) = setup();
        s.write(WT0, 10, &mut ram);
        s.write(WT1, 0, &mut ram);
        s.write(IMR, ISR_TC, &mut ram);
        s.write(CR, CR_ST, &mut ram);
        s.tick(10 * OSC_PER_TIMER, FREQ, &mut ram);
        assert!(!s.irq());
        s.tick(OSC_PER_TIMER, FREQ, &mut ram);
        assert!(s.irq());
        assert_eq!(s.read(WT1), 0xFFFF);
    }
}
