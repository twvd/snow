use std::borrow::Cow;

use crate::types::{Byte, Long, Word};

#[macro_export]
macro_rules! dbgprop_header {
    ($name:expr) => {
        DebuggableProperty::new($name, DebuggablePropertyValue::Header)
    };
}

#[macro_export]
macro_rules! dbgprop_bool {
    ($name:expr, $val:expr) => {
        DebuggableProperty::new($name, DebuggablePropertyValue::Boolean($val))
    };
}

#[macro_export]
macro_rules! dbgprop_byte {
    ($name:expr, $val:expr) => {
        DebuggableProperty::new($name, DebuggablePropertyValue::Byte($val))
    };
}

#[macro_export]
macro_rules! dbgprop_byte_bin {
    ($name:expr, $val:expr) => {
        DebuggableProperty::new($name, DebuggablePropertyValue::ByteBinary($val))
    };
}

#[macro_export]
macro_rules! dbgprop_word {
    ($name:expr, $val:expr) => {
        DebuggableProperty::new($name, DebuggablePropertyValue::Word($val))
    };
}

#[macro_export]
macro_rules! dbgprop_word_bin {
    ($name:expr, $val:expr) => {
        DebuggableProperty::new($name, DebuggablePropertyValue::WordBinary($val))
    };
}

#[macro_export]
macro_rules! dbgprop_long {
    ($name:expr, $val:expr) => {
        DebuggableProperty::new($name, DebuggablePropertyValue::Long($val))
    };
}

#[macro_export]
macro_rules! dbgprop_udec {
    ($name:expr, $val:expr) => {
        DebuggableProperty::new($name, DebuggablePropertyValue::UnsignedDecimal(($val).try_into().unwrap()))
    };
}

#[macro_export]
macro_rules! dbgprop_sdec {
    ($name:expr, $val:expr) => {
        DebuggableProperty::new($name, DebuggablePropertyValue::SignedDecimal($val))
    };
}

#[macro_export]
macro_rules! dbgprop_enum {
    ($name:expr, $val:expr) => {
        DebuggableProperty::new(
            $name,
            DebuggablePropertyValue::StaticStr($val.clone().into()),
        )
    };
}

#[macro_export]
macro_rules! dbgprop_string {
    ($name:expr, $val:expr) => {
        DebuggableProperty::new($name, DebuggablePropertyValue::String($val))
    };
}

#[macro_export]
macro_rules! dbgprop_str {
    ($name:expr, $val:expr) => {
        DebuggableProperty::new($name, DebuggablePropertyValue::StaticStr($val))
    };
}

#[macro_export]
macro_rules! dbgprop_nest {
    ($name:expr, $val:expr) => {
        DebuggableProperty::new(
            $name,
            DebuggablePropertyValue::Nested($val.get_debug_properties()),
        )
    };
}

#[macro_export]
macro_rules! dbgprop_group {
    ($name:expr, $val:expr) => {
        DebuggableProperty::new($name, DebuggablePropertyValue::Nested($val))
    };
}

pub type DebuggableProperties = Vec<DebuggableProperty>;

pub struct DebuggableProperty {
    name: Cow<'static, str>,
    value: DebuggablePropertyValue,
}

impl DebuggableProperty {
    pub fn new(name: impl Into<Cow<'static, str>>, value: DebuggablePropertyValue) -> Self {
        Self {
            name: name.into(),
            value,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn value(&self) -> &DebuggablePropertyValue {
        &self.value
    }
}

pub enum DebuggablePropertyValue {
    Header,
    Nested(DebuggableProperties),
    Boolean(bool),
    Byte(Byte),
    ByteBinary(Byte),
    Word(Word),
    WordBinary(Word),
    Long(Long),
    SignedDecimal(i64),
    UnsignedDecimal(u64),
    StaticStr(&'static str),
    String(String),
}

pub trait Debuggable {
    fn get_debug_properties(&self) -> DebuggableProperties;
}
