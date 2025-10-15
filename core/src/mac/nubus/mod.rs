pub mod mdc12;
pub mod se30video;

use mdc12::Mdc12;
use se30video::SE30Video;
use serde::{Deserialize, Serialize};

use crate::debuggable::Debuggable;
use crate::renderer::Renderer;
use crate::tickable::Tickable;

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
            NubusCard::MDC12(inner) => inner.get_irq(),
            NubusCard::SE30Video(inner) => inner.get_irq(),
        }
    }

    pub fn reset(&mut self) {
        match self {
            NubusCard::MDC12(inner) => inner.reset(),
            NubusCard::SE30Video(inner) => inner.reset(),
        }
    }
}

impl<TRenderer> Debuggable for NubusCard<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        match self {
            NubusCard::MDC12(inner) => inner.get_debug_properties(),
            NubusCard::SE30Video(inner) => inner.get_debug_properties(),
        }
    }
}

impl<TRenderer> ToString for NubusCard<TRenderer>
where
    TRenderer: Renderer,
{
    fn to_string(&self) -> String {
        match self {
            NubusCard::MDC12(inner) => inner.to_string(),
            NubusCard::SE30Video(inner) => inner.to_string(),
        }
    }
}

impl<TRenderer> Tickable for NubusCard<TRenderer>
where
    TRenderer: Renderer,
{
    fn tick(&mut self, ticks: crate::tickable::Ticks) -> anyhow::Result<crate::tickable::Ticks> {
        match self {
            NubusCard::MDC12(inner) => inner.tick(ticks),
            NubusCard::SE30Video(inner) => inner.tick(ticks),
        }
    }
}
