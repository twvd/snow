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
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use toby::Toby;

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub enum NubusCard<TRenderer: Renderer> {
    MDC12(Mdc12<TRenderer>),
    Toby(Toby<TRenderer>),
}

/// A card's memory window: the bus reaches the memory behind it directly, like
/// RAM. An offset in the slot is in the window when `offset & mask == value`.
#[derive(Clone, Copy)]
pub struct Window {
    pub mask: Address,
    pub value: Address,
}

impl Window {
    /// Picks the byte of the memory an offset in the window reaches. The bits
    /// above the lowest one of the mask aren't decoded, so they mirror.
    pub const fn index_mask(self) -> Address {
        (1 << self.mask.trailing_zeros()) - 1
    }
}

/// What a NuBus slot sees of a card
pub trait NubusSlot:
    BusMember<Address> + for<'a> Tickable<&'a dyn EmuContext> + Debuggable + Display
{
    /// The card's memory window, and the memory behind it. What the memory
    /// doesn't cover goes to `read` and `write`, like the registers.
    fn window(&mut self) -> Option<(Window, &mut [u8])> {
        None
    }
    fn get_irq(&self) -> bool;
    fn reset(&mut self);
}

/// What the machine sees of a video card, with the renderer it owns
pub trait VideoCard<TRenderer> {
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
            NubusCard::Toby($c) => $e,
        }
    };
}

impl<TRenderer> NubusCard<TRenderer>
where
    TRenderer: Renderer,
{
    pub fn slot(&mut self) -> &mut dyn NubusSlot {
        with_card!(self, c => c)
    }

    pub fn slot_ref(&self) -> &dyn NubusSlot {
        with_card!(self, c => c)
    }

    /// The card as a video card, if it is one
    pub fn video(&mut self) -> Option<&mut dyn VideoCard<TRenderer>> {
        match self {
            Self::MDC12(c) => Some(c),
            Self::Toby(c) => Some(c),
        }
    }
}

// Statically dispatched, for the paths run on every bus tick and access

impl<TRenderer> NubusSlot for NubusCard<TRenderer>
where
    TRenderer: Renderer,
{
    fn window(&mut self) -> Option<(Window, &mut [u8])> {
        with_card!(self, c => c.window())
    }

    fn get_irq(&self) -> bool {
        with_card!(self, c => c.get_irq())
    }

    fn reset(&mut self) {
        with_card!(self, c => c.reset());
    }
}

impl<TRenderer> BusMember<Address> for NubusCard<TRenderer>
where
    TRenderer: Renderer,
{
    fn read(&mut self, addr: Address) -> Option<u8> {
        with_card!(self, c => c.read(addr))
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        with_card!(self, c => c.write(addr, val))
    }
}

impl<TRenderer> Tickable<&dyn EmuContext> for NubusCard<TRenderer>
where
    TRenderer: Renderer,
{
    fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext) -> Result<Ticks> {
        with_card!(self, c => c.tick(ticks, ctx))
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
