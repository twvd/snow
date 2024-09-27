use arrayvec::ArrayVec;
use chrono::{Local, NaiveDate};
use log::*;

/// Macintosh Real-Time Clock
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

pub struct RtcData {
    writeprotect: bool,
    seconds: u32,
    pram: [u8; 256],
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
                pram: [0; 256],
            },
        }
    }
}

impl Rtc {
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
                self.data.pram[usize::from(scmd - 0x08)] = val;
            }
            0x10..=0x1F => {
                self.data.pram[usize::from(scmd - 0x10 + 4)] = val;
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
            0x00 | 0x04 => self.data.seconds as u8,
            0x01 | 0x05 => (self.data.seconds >> 8) as u8,
            0x02 | 0x06 => (self.data.seconds >> 16) as u8,
            0x03 | 0x07 => (self.data.seconds >> 24) as u8,
            0x08..=0x0B => self.data.pram[usize::from(scmd - 0x08)],
            0x10..=0x1F => self.data.pram[usize::from(scmd - 0x10 + 4)],
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
            self.data.pram[addr] = cmd[2];
        } else {
            self.data_out = Some(self.data.pram[addr]);
        }
    }
}
