use serde::{Deserialize, Serialize};

use crate::bus::{Address, Bus};
use crate::tickable::{Tickable, Ticks};

use super::regs::RegisterFile;

/// Motorola 680x0
#[derive(Serialize, Deserialize)]
pub struct CpuM68k<TBus: Bus<Address>> {
    pub bus: TBus,
    pub regs: RegisterFile,
    pub cycles: Ticks,
}

impl<TBus> CpuM68k<TBus>
where
    TBus: Bus<Address>,
{
    pub fn new(bus: TBus) -> Self {
        Self {
            bus,
            regs: RegisterFile::new(),
            cycles: 0,
        }
    }

    pub fn step(&mut self) {
        todo!();
    }
}
