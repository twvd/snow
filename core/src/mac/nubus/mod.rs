pub mod mdc12;
pub mod se30video;
pub mod toby;

use crate::debuggable::Debuggable;
use crate::emulator::EmuContext;
use crate::renderer::Renderer;
use crate::tickable::{Tickable, Ticks};
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

impl<TRenderer> NubusCard<TRenderer>
where
    TRenderer: Renderer,
{
    pub fn get_irq(&mut self) -> bool {
        match self {
            Self::MDC12(inner) => inner.get_irq(),
            Self::SE30Video(inner) => inner.get_irq(),
            Self::Toby(inner) => inner.get_irq(),
        }
    }

    pub fn reset(&mut self) {
        match self {
            Self::MDC12(inner) => inner.reset(),
            Self::SE30Video(inner) => inner.reset(),
            Self::Toby(inner) => inner.reset(),
        }
    }

    /// Reinstalls a renderer (lost during serialization) after a state load.
    pub fn reinstall_renderer(&mut self, renderer: TRenderer) -> anyhow::Result<()> {
        match self {
            Self::MDC12(inner) => inner.renderer = Some(renderer),
            Self::SE30Video(inner) => inner.renderer = Some(renderer),
            Self::Toby(inner) => inner.renderer = Some(renderer),
        }
        match self {
            Self::MDC12(inner) => inner.render(),
            Self::SE30Video(inner) => inner.render(),
            Self::Toby(inner) => inner.render(),
        }
    }
}

impl<TRenderer> Debuggable for NubusCard<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        match self {
            Self::MDC12(inner) => inner.get_debug_properties(),
            Self::SE30Video(inner) => inner.get_debug_properties(),
            Self::Toby(inner) => inner.get_debug_properties(),
        }
    }
}

impl<TRenderer> Display for NubusCard<TRenderer>
where
    TRenderer: Renderer,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?}",
            match self {
                Self::MDC12(inner) => inner.to_string(),
                Self::SE30Video(inner) => inner.to_string(),
                Self::Toby(inner) => inner.to_string(),
            }
        )
    }
}

impl<TRenderer> Tickable<&dyn EmuContext> for NubusCard<TRenderer>
where
    TRenderer: Renderer,
{
    fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext) -> anyhow::Result<Ticks> {
        match self {
            Self::MDC12(inner) => inner.tick(ticks, ctx),
            Self::SE30Video(inner) => inner.tick(ticks, ctx),
            Self::Toby(inner) => inner.tick(ticks, ctx),
        }
    }
}
