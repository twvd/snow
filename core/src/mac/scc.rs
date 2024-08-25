use crate::{
    bus::{Address, BusMember},
    types::Byte,
};

/// Zilog Z8530 Serial Communications Controller
pub struct Scc {
    wr_ctrl_a: u8,
    wr_data_a: u8,
    wr_ctrl_b: u8,
    wr_data_b: u8,
    rd_ctrl_a: u8,
    rd_data_a: u8,
    rd_ctrl_b: u8,
    rd_data_b: u8,
}

impl Scc {
    pub fn new() -> Self {
        Self {
            wr_ctrl_a: 0,
            wr_data_a: 0,
            wr_ctrl_b: 0,
            wr_data_b: 0,
            rd_ctrl_a: 0,
            rd_data_a: 0,
            rd_ctrl_b: 0,
            rd_data_b: 0,
        }
    }
}

impl BusMember<Address> for Scc {
    fn read(&mut self, addr: Address) -> Option<Byte> {
        match addr {
            0x9FFFF8 => Some(self.rd_ctrl_b),
            0x9FFFFA => Some(self.rd_ctrl_a),
            0x9FFFFC => Some(self.rd_data_b),
            0x9FFFFE => Some(self.rd_data_a),
            0xBFFFF9 => Some(self.wr_ctrl_b),
            0xBFFFFB => Some(self.wr_ctrl_a),
            0xBFFFFD => Some(self.wr_data_b),
            0xBFFFFF => Some(self.wr_data_a),

            _ => None,
        }
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        match addr {
            0x9FFFF8 => Some(self.rd_ctrl_b = val),
            0x9FFFFA => Some(self.rd_ctrl_a = val),
            0x9FFFFC => Some(self.rd_data_b = val),
            0x9FFFFE => Some(self.rd_data_a = val),
            0xBFFFF9 => Some(self.wr_ctrl_b = val),
            0xBFFFFB => Some(self.wr_ctrl_a = val),
            0xBFFFFD => Some(self.wr_data_b = val),
            0xBFFFFF => Some(self.wr_data_a = val),

            _ => None,
        }
    }
}
