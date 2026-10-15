pub mod mdc12;
pub mod se30video;
pub mod toby;

use crate::bus::{Address, BusMember};
use crate::debuggable::Debuggable;
use crate::emulator::EmuContext;
use crate::renderer::Renderer;
use crate::tickable::{Tickable, Ticks};
use anyhow::Result;
use mdc12::Mdc12;
use se30video::SE30Video;
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use toby::Toby;

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub enum NubusCard<TRenderer: Renderer> {
    MDC12(Mdc12<TRenderer>),
    SE30Video(SE30Video<TRenderer>),
    Toby(Toby<TRenderer>),
}

/// What a NuBus slot sees of a card
pub trait NubusSlot<TRenderer>:
    BusMember<Address> + for<'a> Tickable<&'a dyn EmuContext> + Debuggable + Display
{
    fn get_irq(&self) -> bool;
    fn reset(&mut self);
    fn blank(&mut self) -> Result<()>;
    fn render(&mut self) -> Result<()>;
    /// Reinstalls the renderer, which is not serialized
    fn set_renderer(&mut self, renderer: TRenderer);
}

/// Evaluates `$e` with `$c` bound to the card in a NubusCard
macro_rules! with_card {
    ($card:expr, $c:ident => $e:expr) => {
        match $card {
            NubusCard::MDC12($c) => $e,
            NubusCard::SE30Video($c) => $e,
            NubusCard::Toby($c) => $e,
        }
    };
}

impl<TRenderer> NubusCard<TRenderer>
where
    TRenderer: Renderer,
{
    pub fn slot(&mut self) -> &mut dyn NubusSlot<TRenderer> {
        with_card!(self, c => c)
    }

    pub fn slot_ref(&self) -> &dyn NubusSlot<TRenderer> {
        with_card!(self, c => c)
    }

    // Statically dispatched, for the paths run on every bus tick and access

    pub fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext) -> Result<Ticks> {
        with_card!(self, c => c.tick(ticks, ctx))
    }

    pub fn get_irq(&self) -> bool {
        with_card!(self, c => c.get_irq())
    }

    pub fn read(&mut self, addr: Address) -> Option<u8> {
        with_card!(self, c => c.read(addr))
    }

    pub fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        with_card!(self, c => c.write(addr, val))
    }
}

impl<TRenderer> Debuggable for NubusCard<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        self.slot_ref().get_debug_properties()
    }
}

impl<TRenderer> Display for NubusCard<TRenderer>
where
    TRenderer: Renderer,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.slot_ref().to_string())
    }
}
