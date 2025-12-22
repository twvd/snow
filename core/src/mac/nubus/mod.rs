pub mod mdc12;
pub mod se30video;

use crate::debuggable::Debuggable;
use crate::renderer::Renderer;
use crate::tickable::Tickable;
use mdc12::Mdc12;
use se30video::SE30Video;
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub enum NubusCard<TRenderer: Renderer> {
    MDC12(Mdc12<TRenderer>),
    SE30Video(SE30Video<TRenderer>),
}

impl<TRenderer> NubusCard<TRenderer>
where
    TRenderer: Renderer,
{
    pub fn get_irq(&mut self) -> bool {
        match self {
            Self::MDC12(inner) => inner.get_irq(),
            Self::SE30Video(inner) => inner.get_irq(),
        }
    }

    pub fn reset(&mut self) {
        match self {
            Self::MDC12(inner) => inner.reset(),
            Self::SE30Video(inner) => inner.reset(),
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
            }
        )
    }
}

impl<TRenderer> Tickable for NubusCard<TRenderer>
where
    TRenderer: Renderer,
{
    fn tick(&mut self, ticks: crate::tickable::Ticks) -> anyhow::Result<crate::tickable::Ticks> {
        match self {
            Self::MDC12(inner) => inner.tick(ticks),
            Self::SE30Video(inner) => inner.tick(ticks),
        }
    }
}
