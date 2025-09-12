use arrayvec::ArrayVec;
use chrono::{Local, NaiveDate};
use serde::{Deserialize, Serialize};
#[cfg(feature = "mmap")]
use std::fs::OpenOptions;
use std::path::Path;

#[cfg(feature = "mmap")]
use fs2::FileExt;
use log::*;
#[cfg(feature = "mmap")]
use memmap2::MmapMut;

const PRAM_SIZE: usize = 256;

/// Serde adapter for PRAM
#[cfg(feature = "mmap")]
pub mod serde_rtc_pram {
    use super::*;
    use serde::de::Error;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S>(value: &MmapMut, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let pram_vec = Vec::from_iter(value.iter().copied());
        pram_vec.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<MmapMut, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut mmap = MmapMut::map_anon(PRAM_SIZE).unwrap();
        let vec = Vec::deserialize(deserializer)?;

        if vec.len() != PRAM_SIZE {
            return Err(D::Error::invalid_length(vec.len(), &"invalid size"));
        }

        for (i, c) in vec.into_iter().enumerate() {
            mmap[i] = c;
        }

        Ok(mmap)
    }
}

/// Macintosh Real-Time Clock
#[derive(Serialize, Deserialize)]
pub struct Rtc {
    io_enable: bool,
    io_clk: bool,

    cmd: ArrayVec<u8, 3>,
    cmd_len: usize,
    byte_in: u8,
    byte_in_bit: usize,
    data_out: Option<u8>,

    data: RtcData,
}

#[derive(Serialize, Deserialize)]
pub struct RtcData {
    writeprotect: bool,
    seconds: u32,

    #[cfg(feature = "mmap")]
    #[serde(with = "serde_rtc_pram")]
    pram: MmapMut,

    #[cfg(not(feature = "mmap"))]
    pram: Vec<u8>,
}

impl RtcData {
    /// Try to load a PRAM file, given the filename.
    ///
    /// This locks the file on PRAM and memory maps the file for use by
    /// the emulator for fast access and automatic writes back to PRAM,
    /// at the discretion of the operating system.
    #[cfg(feature = "mmap")]
    pub(super) fn load_pram(filename: &Path) -> Option<MmapMut> {
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(filename)
            .inspect_err(|e| error!("Opening PRAM {} failed: {}", filename.display(), e))
            .ok()?;

        f.set_len(PRAM_SIZE as u64)
            .inspect_err(|e| error!("Opening PRAM {} failed: {}", filename.display(), e))
            .ok()?;

        f.try_lock_exclusive()
            .inspect_err(|e| error!("Cannot lock PRAM {}: {}", filename.display(), e))
            .ok()?;

        let mmapped = unsafe {
            MmapMut::map_mut(&f)
                .inspect_err(|e| error!("Cannot mmap PRAM file {}: {}", filename.display(), e))
                .ok()?
        };

        Some(mmapped)
    }

    #[cfg(not(feature = "mmap"))]
    pub(super) fn load_pram(filename: &Path) -> Option<Vec<u8>> {
        use std::fs;

        if !filename.exists() {
            // File not found
            return None;
        }

        let pram = match fs::read(filename) {
            Ok(d) => d,
            Err(e) => {
                error!("Failed to open PRAM file {}: {}", filename.display(), e);
                return None;
            }
        };

        if pram.len() != PRAM_SIZE {
            error!(
                "Cannot load PRAM {}: not {} bytes",
                filename.display(),
                PRAM_SIZE
            );
            return None;
        }

        Some(pram)
    }

    #[cfg(feature = "mmap")]
    pub(super) fn empty_pram() -> MmapMut {
        MmapMut::map_anon(PRAM_SIZE).unwrap()
    }

    #[cfg(not(feature = "mmap"))]
    pub(super) fn empty_pram() -> Vec<u8> {
        vec![0; PRAM_SIZE]
    }
}

impl Default for Rtc {
    fn default() -> Self {
        // Initialize clock from host system
        let seconds = Local::now()
            .naive_local()
            .signed_duration_since(
                NaiveDate::from_ymd_opt(1904, 1, 1)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap(),
            )
            .num_seconds() as u32;

        Self {
            io_enable: false,
            io_clk: false,
            cmd: ArrayVec::new(),
            cmd_len: 0,
            byte_in: 0,
            byte_in_bit: 0,
            data_out: None,
            data: RtcData {
                writeprotect: true,
                seconds,
                pram: RtcData::empty_pram(),
            },
        }
    }
}

impl Rtc {
    /// Loads a data file into PRAM
    pub fn load_pram(&mut self, filename: &Path) {
        let Some(pram) = RtcData::load_pram(filename) else {
            warn!(
                "Cannot load PRAM file {}, PRAM reset and changes will not be saved",
                filename.display()
            );
            return;
        };
        info!("Persisting PRAM in {}", filename.display());

        self.data.pram = pram;
    }

    /// Pokes the RTC that one second has passed
    /// In the emulator, one second interrupt is driven by the VIA for ease.
    pub fn second(&mut self) {
        self.data.seconds = self.data.seconds.wrapping_add(1);
    }

    /// Updates RTC I/O lines from the VIA.
    pub fn io(&mut self, enable: bool, clk: bool, data: bool) -> bool {
        let mut res = true;

        if enable {
            // Disabled
            self.io_enable = true;
            return true;
        }
        if !enable && self.io_enable {
            // Reset
            self.io_enable = false;
            self.data_out = None;
            self.cmd.clear();
            self.byte_in = 0;
            self.byte_in_bit = 0;
        }

        if clk && !self.io_clk {
            // Rising edge, clock bit in/out
            if self.data_out.is_none() {
                // Receiving command, clock bit in
                self.byte_in <<= 1;
                if data {
                    self.byte_in |= 1;
                }
                self.byte_in_bit += 1;
                if self.byte_in_bit >= 8 {
                    if self.cmd.is_empty() {
                        // First byte, determine command length
                        self.cmd_len = 1;
                        if self.byte_in & 0b0111_1000 == 0b0011_1000 {
                            // Extended command
                            self.cmd_len += 1;
                        }
                        if self.byte_in & 0x80 == 0 {
                            // Write command
                            self.cmd_len += 1;
                        }
                    }

                    self.cmd.push(self.byte_in);
                    self.byte_in = 0;
                    self.byte_in_bit = 0;

                    if self.cmd.len() == self.cmd_len {
                        // Complete
                        let write = self.cmd[0] & 0x80 == 0;
                        if self.cmd[0] & 0b0111_1000 == 0b0011_1000 {
                            // Extended command
                            self.cmd_ext();
                        } else if write {
                            // Write command
                            self.cmd_write(self.cmd[0], self.cmd[1]);
                        } else {
                            // Read command
                            self.cmd_read(self.cmd[0]);
                        }
                    }
                }
            } else if let Some(b) = self.data_out.as_mut() {
                // Sending response, clock bit out
                res = *b & 0x80 != 0;
                *b = b.wrapping_shl(1);
            }
        }

        self.io_clk = clk;
        res
    }

    /// Process a command from the CPU that writes to the RTC
    fn cmd_write(&mut self, cmd: u8, val: u8) {
        let scmd = (cmd >> 2) & 0b11111;
        match scmd {
            0x00 | 0x04 => {
                self.data.seconds = (self.data.seconds & 0xFFFFFF00) | (val as u32);
            }
            0x01 | 0x05 => {
                self.data.seconds = (self.data.seconds & 0xFFFF00FF) | ((val as u32) << 8);
            }
            0x02 | 0x06 => {
                self.data.seconds = (self.data.seconds & 0xFF00FFFF) | ((val as u32) << 16);
            }
            0x03 | 0x07 => {
                self.data.seconds = (self.data.seconds & 0x00FFFFFF) | ((val as u32) << 24);
            }
            0x0C => {
                // Test register
            }
            0x0D => self.data.writeprotect = val & 0x80 != 0,
            0x08..=0x0B => {
                let addr = usize::from(scmd);
                if !self.data.writeprotect {
                    self.data.pram[addr] = val;
                }
            }
            0x10..=0x1F => {
                let addr = usize::from(scmd);
                if !self.data.writeprotect {
                    self.data.pram[addr] = val;
                }
            }
            _ => {
                warn!(
                    "Unknown RTC write command: {:02X} {:08b}, data: {:02X}",
                    cmd, cmd, val
                );
            }
        }
    }

    /// Process a command from the CPU that reads from the RTC
    fn cmd_read(&mut self, cmd: u8) {
        let scmd = (cmd >> 2) & 0b11111;
        self.data_out = Some(match scmd {
            // Force AppleTalk to OFF to prevent freezing on the SCC during boot
            // for System 6.x+
            // Since this value is cached in RAM, an extra reboot may be required if
            // PRAM is cleared.
            0x13 => 0x22,

            0x00 | 0x04 => self.data.seconds as u8,
            0x01 | 0x05 => (self.data.seconds >> 8) as u8,
            0x02 | 0x06 => (self.data.seconds >> 16) as u8,
            0x03 | 0x07 => (self.data.seconds >> 24) as u8,
            0x08..=0x0B => self.data.pram[usize::from(scmd)],
            0x10..=0x1F => self.data.pram[usize::from(scmd)],
            _ => {
                warn!("Unknown RTC read command: {:02X} {:08b}", cmd, cmd);
                0
            }
        });
    }

    /// Process an extended command (read or write)
    fn cmd_ext(&mut self) {
        let cmd = &self.cmd;
        let write = cmd[0] & 0x80 == 0;
        let addr = (((cmd[0] & 0x07) << 5) | ((cmd[1] >> 2) & 0x1F)) as usize;

        if write {
            if !self.data.writeprotect {
                self.data.pram[addr] = cmd[2];
            }
        } else {
            self.data_out = Some(self.data.pram[addr]);
        }
    }
}
