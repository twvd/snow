//! The X/Lisa Programmable Frequency Generator (PFG), which MacWorks Plus II requires
//!
//! It sits in the SCC socket, with the SCC on it, and clips onto the floppy controller's
//! clock. It has no address of its own: it watches the SCC's bus.
//!
//! No documentation of it is known: this is worked out from MacWorks Plus II 2.5.0's boot
//! code.
//! - Writing WR9 = $0D opens the PFG, WR9 = $09 closes it. While it is open, a write of
//!   WR7 on channel A is a PFG command: bits 4-6 pick one of eight 3-bit latches, bits 1-3
//!   are its data.
//! - Latch 0: the EEPROM's DI (bit 0), CS (bit 1) and SK (bit 2). Latches 0 and 1 also
//!   make the 6-bit frequency code. Latch 5 bit 0 switches the floppy controller to the
//!   PFG's clock; MacWorks Plus II also pulses it after each EEPROM command.
//! - The PFG answers on channel B's DCD: the EEPROM's DO, inverted, while CS is high,
//!   else a bit of a 16-bit id picked by latch 1 and SK.
//! - The EEPROM is a 93C56-like part of 128 words of 16 bits: the 256 bytes of PRAM of
//!   MacWorks Plus II.

use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::fdc_sim::{FDC_CLOCK, PARAMETER_MEMORY_SIZE};
use crate::emulator::devices::{BusKind, DeviceModel, FunctionKind, ModelInfo};
use crate::mac::MacModel;
use crate::tickable::Ticks;

pub const EEPROM_SIZE: usize = 256;
const EEPROM_WORDS: usize = EEPROM_SIZE / 2;

/// MacWorks Plus II only checks that its low nibble is $A. It derives board options from
/// bits 5-7, whose meaning is unknown.
const ID: u16 = 0x000A;

/// At code 0, MacWorks Plus II expects the 6504 to run 1600/900 slower than on its own
/// clock.
const CLOCK_AT_CODE_0: Ticks = FDC_CLOCK * 900 / 1600;
/// A guess: MacWorks Plus II only checks code 0. With this step the codes reach a little
/// above the controller's own clock.
const CLOCK_STEP: Ticks = 16_000;

#[derive(Serialize, Deserialize, Default)]
enum Microwire {
    /// Waiting for the start bit
    #[default]
    Idle,
    /// Taking the opcode and the address, `bits` of them so far
    Command { bits: u8, value: u16 },
    /// Taking the 16 bits to write
    Data { addr: u8, bits: u8, value: u16 },
    /// Sending a word, MSB first, after a dummy 0
    Read { word: u16, left: u8 },
}

#[derive(Serialize, Deserialize)]
pub struct Pfg {
    /// Channel A's register pointer, followed as the SCC does
    pointer: u8,
    open: bool,
    latch: [u8; 8],
    words: Vec<u16>,
    write_enabled: bool,
    state: Microwire,
    data_out: bool,
    /// The PRAM file, which has the EEPROM after the Lisa's parameter memory. A save
    /// state doesn't bring it back, like the PRAM file of the Macs.
    #[serde(skip)]
    path: Option<PathBuf>,
    #[serde(skip)]
    changed: bool,
}

impl Pfg {
    pub fn new() -> Self {
        Self {
            pointer: 0,
            open: false,
            latch: [0; 8],
            // As it comes from the factory
            words: vec![0xFFFF; EEPROM_WORDS],
            write_enabled: false,
            state: Microwire::Idle,
            data_out: true,
            path: None,
            changed: false,
        }
    }

    /// The machine's reset. The EEPROM keeps its contents.
    pub fn reset(&mut self) {
        self.pointer = 0;
        self.open = false;
        self.latch = [0; 8];
        self.write_enabled = false;
        self.state = Microwire::Idle;
        self.data_out = true;
    }

    /// A read of channel A's control port resets the pointer
    pub fn control_read(&mut self) {
        self.pointer = 0;
    }

    /// A write of channel A's control port
    pub fn control_write(&mut self, val: u8) {
        let reg = self.pointer;
        self.pointer = 0;
        match reg {
            0 => {
                self.pointer = val & 7;
                // Point high
                if (val >> 3) & 7 == 1 {
                    self.pointer |= 8;
                }
            }
            9 => self.open = val == 0x0D,
            7 if self.open => self.command(val),
            _ => (),
        }
    }

    fn command(&mut self, val: u8) {
        let n = usize::from((val >> 4) & 7);
        let data = (val >> 1) & 7;
        let old = self.latch[n];
        self.latch[n] = data;
        match n {
            0 => self.eeprom_lines(old, data),
            // The end of an EEPROM command
            5 if data & 1 != 0 => self.state = Microwire::Idle,
            _ => (),
        }
    }

    fn eeprom_lines(&mut self, old: u8, new: u8) {
        if new & 2 == 0 {
            self.state = Microwire::Idle;
            return;
        }
        if old & 4 == 0 && new & 4 != 0 {
            self.clock(new & 1 != 0);
        }
    }

    /// A rising edge of SK
    fn clock(&mut self, di: bool) {
        let bit = u16::from(di);
        self.state = match std::mem::take(&mut self.state) {
            Microwire::Idle if di => Microwire::Command { bits: 0, value: 0 },
            Microwire::Idle => Microwire::Idle,
            Microwire::Command { bits, value } => {
                let value = (value << 1) | bit;
                // 2 bits of opcode and 7 of address
                if bits + 1 < 9 {
                    Microwire::Command {
                        bits: bits + 1,
                        value,
                    }
                } else {
                    self.execute(value)
                }
            }
            Microwire::Data { addr, bits, value } => {
                let value = (value << 1) | bit;
                if bits + 1 < 16 {
                    Microwire::Data {
                        addr,
                        bits: bits + 1,
                        value,
                    }
                } else {
                    self.write(addr, value);
                    // Ready at once
                    self.data_out = true;
                    Microwire::Idle
                }
            }
            Microwire::Read { word, left } => {
                self.data_out = word & 0x8000 != 0;
                if left > 1 {
                    Microwire::Read {
                        word: word << 1,
                        left: left - 1,
                    }
                } else {
                    Microwire::Idle
                }
            }
        };
    }

    fn execute(&mut self, value: u16) -> Microwire {
        let addr = (value & 0x7F) as u8;
        match value >> 7 {
            // READ
            0b10 => {
                self.data_out = false;
                Microwire::Read {
                    word: self.words[usize::from(addr)],
                    left: 16,
                }
            }
            // WRITE
            0b01 => Microwire::Data {
                addr,
                bits: 0,
                value: 0,
            },
            // ERASE
            0b11 => {
                self.write(addr, 0xFFFF);
                Microwire::Idle
            }
            _ => {
                match addr >> 5 {
                    0b11 => self.write_enabled = true,
                    0b00 => self.write_enabled = false,
                    // ERAL
                    0b10 => {
                        for addr in 0..EEPROM_WORDS as u8 {
                            self.write(addr, 0xFFFF);
                        }
                    }
                    _ => log::warn!("PFG: unknown EEPROM command {:03X}", value),
                }
                Microwire::Idle
            }
        }
    }

    fn write(&mut self, addr: u8, value: u16) {
        let word = &mut self.words[usize::from(addr)];
        if self.write_enabled && *word != value {
            *word = value;
            self.changed = true;
        }
    }

    /// Channel B's DCD
    pub fn dcd(&self) -> bool {
        let latch0 = self.latch[0];
        if latch0 & 2 != 0 {
            return !self.data_out;
        }
        let sk = usize::from((latch0 >> 2) & 1);
        let k = usize::from(self.latch[1]);
        (ID >> (15 - 2 * k - sk)) & 1 != 0
    }

    /// The floppy controller's clock, if the PFG gives it one
    pub fn fdc_clock(&self) -> Option<Ticks> {
        if self.latch[5] & 1 == 0 {
            return None;
        }
        let code = Ticks::from(self.latch[0] | (self.latch[1] << 3));
        Some(CLOCK_AT_CODE_0 + code * CLOCK_STEP)
    }

    /// Keeps the EEPROM in the PRAM file from now on, after the parameter memory, whose
    /// code checks the file's size. Without one there yet, it is as it came from the
    /// factory.
    pub fn persist_eeprom(&mut self, path: &Path) -> Result<()> {
        let saved = match std::fs::read(path) {
            Ok(data) => data,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        match saved.get(PARAMETER_MEMORY_SIZE..) {
            Some(eeprom) if eeprom.len() == EEPROM_SIZE => {
                for (word, bytes) in self.words.iter_mut().zip(eeprom.chunks(2)) {
                    *word = u16::from_be_bytes([bytes[0], bytes[1]]);
                }
            }
            _ => self.changed = true,
        }
        self.path = Some(path.to_path_buf());
        Ok(())
    }

    /// Writes the EEPROM to its file if the guest changed it
    pub fn save_eeprom(&mut self) -> Result<()> {
        if !std::mem::take(&mut self.changed) {
            return Ok(());
        }
        let Some(path) = &self.path else {
            return Ok(());
        };
        let bytes: Vec<u8> = self.words.iter().flat_map(|w| w.to_be_bytes()).collect();
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.seek(SeekFrom::Start(PARAMETER_MEMORY_SIZE as u64))?;
        file.write_all(&bytes)?;
        Ok(())
    }
}

impl Default for Pfg {
    fn default() -> Self {
        Self::new()
    }
}

/// MacWorks Plus II runs on a Lisa 2
fn fits_pfg(mac: MacModel) -> bool {
    matches!(mac, MacModel::Lisa25 | MacModel::Lisa210)
}

pub const INFO: ModelInfo = ModelInfo {
    model: DeviceModel::Pfg,
    name: "X/Lisa PFG",
    bus: BusKind::BuiltIn,
    fits: fits_pfg,
    function: FunctionKind::FrequencyGenerator,
    fixed: true,
    one_per_bus: true,
    rom: None,
    path_keys: &[],
    adds_buses: &[],
    monitors: &[],
    create: None,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn command(pfg: &mut Pfg, latch: u8, data: u8) {
        pfg.control_write(7);
        pfg.control_write((latch << 4) | (data << 1));
    }

    fn open() -> Pfg {
        let mut pfg = Pfg::new();
        pfg.control_write(9);
        pfg.control_write(0x0D);
        pfg
    }

    /// Clocks bits into the EEPROM with CS high
    fn send(pfg: &mut Pfg, bits: &[u8]) {
        for &bit in bits {
            command(pfg, 0, 2 | bit);
            command(pfg, 0, 6 | bit);
        }
    }

    fn receive(pfg: &mut Pfg, count: usize) -> u16 {
        let mut value = 0;
        for _ in 0..count {
            command(pfg, 0, 2);
            command(pfg, 0, 6);
            value = (value << 1) | u16::from(!pfg.dcd());
        }
        value
    }

    fn bits(value: u16, count: usize) -> Vec<u8> {
        (0..count).rev().map(|i| ((value >> i) & 1) as u8).collect()
    }

    fn deselect(pfg: &mut Pfg) {
        command(pfg, 0, 0);
    }

    #[test]
    fn closed_takes_no_commands() {
        let mut pfg = Pfg::new();
        command(&mut pfg, 5, 1);
        assert_eq!(pfg.fdc_clock(), None);
        let mut pfg = open();
        command(&mut pfg, 5, 1);
        assert_eq!(pfg.fdc_clock(), Some(CLOCK_AT_CODE_0));
        pfg.control_write(9);
        pfg.control_write(0x09);
        command(&mut pfg, 5, 0);
        assert!(pfg.fdc_clock().is_some());
    }

    #[test]
    fn id_low_nibble() {
        let mut pfg = open();
        let mut id = 0;
        for k in 0..8 {
            command(&mut pfg, 0, 0);
            command(&mut pfg, 1, k);
            id = (id << 1) | u16::from(pfg.dcd());
            command(&mut pfg, 0, 4);
            id = (id << 1) | u16::from(pfg.dcd());
        }
        assert_eq!(id & 0xF, 0xA);
    }

    #[test]
    fn eeprom_write_and_read() {
        let mut pfg = open();
        // EWEN, then WRITE word 5
        send(&mut pfg, &[1, 0, 0, 1, 1, 0, 0, 0, 0, 0]);
        deselect(&mut pfg);
        let mut write = vec![1, 0, 1];
        write.extend(bits(5, 7));
        write.extend(bits(0x4275, 16));
        send(&mut pfg, &write);
        deselect(&mut pfg);
        // READ word 5: the dummy 0, then the word
        let mut read = vec![1, 1, 0];
        read.extend(bits(5, 7));
        send(&mut pfg, &read);
        assert!(pfg.dcd(), "dummy 0, inverted");
        assert_eq!(receive(&mut pfg, 16), 0x4275);
        assert!(pfg.changed);
    }

    #[test]
    fn writes_need_ewen() {
        let mut pfg = open();
        let mut write = vec![1, 0, 1];
        write.extend(bits(5, 7));
        write.extend(bits(0, 16));
        send(&mut pfg, &write);
        assert_eq!(pfg.words[5], 0xFFFF);
    }

    #[test]
    fn pram_file() {
        let dir = std::env::temp_dir().join(format!("snow-lisa-pfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("lisa.pram");

        // The parameter memory of a Lisa without a PFG stays as it is
        std::fs::write(&path, vec![0x55; PARAMETER_MEMORY_SIZE]).unwrap();
        let mut pfg = Pfg::new();
        pfg.persist_eeprom(&path).unwrap();
        pfg.words[1] = 0x1234;
        pfg.save_eeprom().unwrap();
        let file = std::fs::read(&path).unwrap();
        assert_eq!(file.len(), PARAMETER_MEMORY_SIZE + EEPROM_SIZE);
        assert_eq!(file[..PARAMETER_MEMORY_SIZE], [0x55; PARAMETER_MEMORY_SIZE]);
        assert_eq!(file[PARAMETER_MEMORY_SIZE + 2..][..2], [0x12, 0x34]);

        let mut pfg = Pfg::new();
        pfg.persist_eeprom(&path).unwrap();
        assert_eq!(pfg.words[1], 0x1234);
        assert!(!pfg.changed);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
