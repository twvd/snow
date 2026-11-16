//! Processor-direct slots
//!
//! A processor-direct slot, or PDS, is an expansion connector wired to the CPU's own bus. A Mac
//! has one at most. See docs/misc/pds.md.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::bus::Address;
use crate::debuggable::Debuggable;
use crate::devices::DeviceOps;
use crate::emulator::EmuContext;
use crate::mac::dma::DmaOps;
use crate::tickable::Ticks;

/// What a processor-direct slot sees of a card
#[typetag::serde(tag = "type")]
pub(crate) trait PdsSlot: Send + DeviceOps + Debuggable {
    /// One cycle of the CPU: `size` bytes (1, 2 or 4) at `addr`, the address as it is on the
    /// CPU's bus. `map32`: the cycle uses the 32-bit memory map (what /FC3 says on an LC).
    /// None: the card doesn't answer there. Called by the machine for a cycle in the address
    /// ranges it gives the slot: the card decodes the address itself.
    fn read(&mut self, addr: Address, size: u32, map32: bool) -> Option<u32>;

    /// One write cycle of the CPU, as `read`
    fn write(&mut self, addr: Address, size: u32, map32: bool, val: u32) -> Option<()>;

    /// Called by the machine to specify the tick rate, once, when the bus attaches the card and
    /// before any other method. `tick_rate` is the number of ticks in one second, for the ticks
    /// that `tick()` counts. Nothing by default.
    fn connected(&mut self, _tick_rate: Ticks) {}

    /// The levels of the card's interrupt lines, one bit per line of the slot: bit 0 for the
    /// one line of an LC. A bit stays set until the guest has dealt with its cause. Called by
    /// the machine at each of its ticks.
    fn irqs(&self) -> u8;

    /// Called by the machine when it is reset. `hard`: the machine was powered on, and not only
    /// reset.
    fn reset(&mut self, hard: bool);

    /// Called by the machine at each of its ticks, with the time that passed. A card that
    /// masters the bus does its DMA here, through `dma`.
    fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext, dma: &mut dyn DmaOps) -> Result<()>;

    /// The average level of the card's SNDOUT line since the last call, from 0 (low) to 1
    /// (high). Called by the machine for each sample of its sound output. 0 by default: the
    /// card doesn't drive the line.
    fn sound_pin_value(&mut self) -> f32 {
        0.0
    }
}

/// The processor-direct slot of a machine, with the card in it if there is one
#[derive(Serialize, Deserialize)]
pub struct Pds {
    card: Option<Box<dyn PdsSlot>>,

    /// The number of ticks in one second, for the ticks that `tick()` counts
    tick_rate: Ticks,
}

impl Pds {
    pub(crate) fn new(tick_rate: Ticks) -> Self {
        Self {
            card: None,
            tick_rate,
        }
    }

    /// Puts `card` in the slot
    pub(crate) fn insert(&mut self, mut card: Box<dyn PdsSlot>) {
        card.connected(self.tick_rate);
        self.card = Some(card);
    }

    pub(crate) fn card(&self) -> Option<&dyn PdsSlot> {
        self.card.as_deref()
    }

    pub(crate) fn card_mut(&mut self) -> Option<&mut dyn PdsSlot> {
        let card = self.card.as_mut()?;
        Some(&mut **card)
    }

    /// One read cycle of the CPU for the card, at the address on the CPU's bus, in the memory
    /// map `map32` says. None if the slot is empty or the card doesn't answer.
    pub(crate) fn read(&mut self, addr: Address, size: u32, map32: bool) -> Option<u32> {
        self.card.as_mut()?.read(addr, size, map32)
    }

    /// One write cycle of the CPU for the card, as `read`
    pub(crate) fn write(&mut self, addr: Address, size: u32, map32: bool, val: u32) -> Option<()> {
        self.card.as_mut()?.write(addr, size, map32, val)
    }

    pub(crate) fn tick(
        &mut self,
        ticks: Ticks,
        ctx: &dyn EmuContext,
        dma: &mut dyn DmaOps,
    ) -> Result<()> {
        match &mut self.card {
            Some(card) => card.tick(ticks, ctx, dma),
            None => Ok(()),
        }
    }

    /// The interrupt requests of the card, one bit per line of the slot
    pub(crate) fn irqs(&self) -> u8 {
        self.card.as_ref().map_or(0, |card| card.irqs())
    }

    /// The average level of the SNDOUT line of the slot since the last call, from 0 to 1
    pub(crate) fn sound_pin_value(&mut self) -> f32 {
        self.card
            .as_mut()
            .map_or(0.0, |card| card.sound_pin_value())
    }

    /// `hard`: the machine was powered on, and not only reset
    pub(crate) fn reset(&mut self, hard: bool) {
        if let Some(card) = &mut self.card {
            card.reset(hard);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::debuggable::DebuggableProperties;
    use crate::devices::{FunctionConfig, FunctionMut, ModelInfo};
    use crate::mac::maclc::pds_iie;

    pub const PATTERN: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];

    /// A card that masters the bus at each tick: it copies `PATTERN.len()` bytes of the
    /// machine's RAM from `from` to `to`, and tries a block at `not_ram`, where the machine has
    /// no RAM
    #[derive(Serialize, Deserialize)]
    pub struct DmaCard {
        pub from: Address,
        pub to: Address,
        pub not_ram: Address,
    }

    impl DeviceOps for DmaCard {
        fn info(&self) -> &'static ModelInfo {
            &pds_iie::INFO
        }

        fn config(&self) -> FunctionConfig {
            FunctionConfig::AppleIIeCard(Default::default())
        }

        fn function(&mut self) -> FunctionMut<'_> {
            unreachable!()
        }
    }

    #[typetag::serde]
    impl PdsSlot for DmaCard {
        fn read(&mut self, _addr: Address, _size: u32, _map32: bool) -> Option<u32> {
            None
        }

        fn write(&mut self, _addr: Address, _size: u32, _map32: bool, _val: u32) -> Option<()> {
            None
        }

        fn irqs(&self) -> u8 {
            0
        }

        fn reset(&mut self, _hard: bool) {}

        fn tick(&mut self, _ticks: Ticks, _: &dyn EmuContext, dma: &mut dyn DmaOps) -> Result<()> {
            let mut block = [0; PATTERN.len()];
            assert!(dma.read(self.from, &mut block).ok);
            assert!(dma.write(self.to, &block).ok);
            assert!(!dma.read(self.not_ram, &mut block).ok);
            assert!(!dma.write(self.not_ram, &block).ok);
            Ok(())
        }
    }

    impl Debuggable for DmaCard {
        fn get_debug_properties(&self) -> DebuggableProperties {
            vec![]
        }
    }

    /// A card that holds its interrupt lines at `irqs`, and answers no cycle
    #[derive(Serialize, Deserialize)]
    pub struct IrqCard {
        pub irqs: u8,
    }

    impl DeviceOps for IrqCard {
        fn info(&self) -> &'static ModelInfo {
            &pds_iie::INFO
        }

        fn config(&self) -> FunctionConfig {
            FunctionConfig::AppleIIeCard(Default::default())
        }

        fn function(&mut self) -> FunctionMut<'_> {
            unreachable!()
        }
    }

    #[typetag::serde]
    impl PdsSlot for IrqCard {
        fn read(&mut self, _addr: Address, _size: u32, _map32: bool) -> Option<u32> {
            None
        }

        fn write(&mut self, _addr: Address, _size: u32, _map32: bool, _val: u32) -> Option<()> {
            None
        }

        fn irqs(&self) -> u8 {
            self.irqs
        }

        fn reset(&mut self, _hard: bool) {}

        fn tick(&mut self, _: Ticks, _: &dyn EmuContext, _: &mut dyn DmaOps) -> Result<()> {
            Ok(())
        }
    }

    impl Debuggable for IrqCard {
        fn get_debug_properties(&self) -> DebuggableProperties {
            vec![]
        }
    }

    #[test]
    fn an_empty_slot_answers_nothing() {
        let mut pds = Pds::new(1);
        assert_eq!(pds.read(0xFE00_0000, 1, true), None);
        assert_eq!(pds.write(0xFE00_0000, 1, true, 0), None);
        assert_eq!(pds.irqs(), 0);
        assert_eq!(pds.sound_pin_value(), 0.0);
    }

    #[test]
    fn the_card_is_saved_and_loaded() {
        let mut pds = Pds::new(1);
        pds.insert(Box::new(pds_iie::PdsIIeCard::new()));

        let saved = serde_json::to_string(&pds).unwrap();
        let pds: Pds = serde_json::from_str(&saved).unwrap();
        let card = pds.card().unwrap();
        assert_eq!(card.info().model, pds_iie::INFO.model);
    }
}
