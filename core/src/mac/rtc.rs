use chrono::{Local, NaiveDate};
use log::*;

/// Macintosh Real-Time Clock
pub struct Rtc {
    io_enable: bool,
    io_clk: bool,

    write_cmd: Option<u8>,
    byte_in: u8,
    byte_in_bit: usize,
    data_out: u8,
    sending: bool,

    data: RtcData,
}

#[derive(Default)]
pub struct RtcData {
    writeprotect: bool,
    seconds: u32,
    pram: [u8; 0x14],
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
            write_cmd: None,
            byte_in: 0,
            byte_in_bit: 0,
            data_out: 0,
            sending: false,
            data: RtcData {
                writeprotect: true,
                seconds,
                pram: [0; 0x14],
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
            self.data_out = 0;
            self.write_cmd = None;
            self.byte_in = 0;
            self.byte_in_bit = 0;
            self.sending = false;
        }

        if !self.sending && clk && !self.io_clk {
            // Receiving command
            self.byte_in <<= 1;
            if data {
                self.byte_in |= 1;
            }
            self.byte_in_bit += 1;
            if self.byte_in_bit >= 8 {
                if let Some(cmd) = self.write_cmd {
                    // Second byte of write
                    self.cmd_write(cmd, self.byte_in);
                    self.write_cmd = None;
                } else if self.byte_in & 0x80 == 0 {
                    // Write - read another byte
                    self.write_cmd = Some(self.byte_in);
                } else {
                    self.data_out = self.cmd_read(self.byte_in);
                    self.sending = true;
                }
                self.byte_in = 0;
                self.byte_in_bit = 0;
            }
        } else if self.sending && clk && !self.io_clk {
            // Sending response
            res = self.data_out & 0x80 != 0;
            self.data_out = self.data_out.wrapping_shl(1);
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
    fn cmd_read(&self, cmd: u8) -> u8 {
        let scmd = (cmd >> 2) & 0b11111;
        match scmd {
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
        }
    }
}
