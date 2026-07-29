use crate::{ClosureId, TableId, ThreadId};
use core::fmt;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct NativeFunctionId(pub(crate) u32);

#[derive(Clone)]
#[non_exhaustive]
pub enum Value {
    Nil,
    Boolean(bool),
    Number(f64),
    Integer(i64),
    String(Arc<[u8]>),
    Table(TableId),
    Closure(ClosureId),
    Thread(ThreadId),
    NativeFunction(NativeFunctionId),
}

impl Value {
    #[must_use]
    pub const fn is_truthy(&self) -> bool {
        !matches!(self, Self::Nil | Self::Boolean(false))
    }

    #[must_use]
    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::Nil => "nil",
            Self::Boolean(_) => "boolean",
            Self::Number(_) | Self::Integer(_) => "number",
            Self::String(_) => "string",
            Self::Table(_) => "table",
            Self::Closure(_) => "function",
            Self::Thread(_) => "thread",
            Self::NativeFunction(_) => "function",
        }
    }

    pub(crate) fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(value) => Some(*value),
            Self::Integer(value) => Some(*value as f64),
            _ => None,
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Nil => f.write_str("Nil"),
            Self::Boolean(value) => f.debug_tuple("Boolean").field(value).finish(),
            Self::Number(value) => f.debug_tuple("Number").field(value).finish(),
            Self::Integer(value) => f.debug_tuple("Integer").field(value).finish(),
            Self::String(value) => f
                .debug_tuple("String")
                .field(&String::from_utf8_lossy(value))
                .finish(),
            Self::Table(value) => value.fmt(f),
            Self::Closure(value) => value.fmt(f),
            Self::Thread(value) => value.fmt(f),
            Self::NativeFunction(value) => value.fmt(f),
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Nil, Self::Nil) => true,
            (Self::Boolean(left), Self::Boolean(right)) => left == right,
            (Self::Number(left), Self::Number(right)) => left == right,
            (Self::Integer(left), Self::Integer(right)) => left == right,
            (Self::Number(left), Self::Integer(right))
            | (Self::Integer(right), Self::Number(left)) => *left == *right as f64,
            (Self::String(left), Self::String(right)) => left == right,
            (Self::Table(left), Self::Table(right)) => left == right,
            (Self::Closure(left), Self::Closure(right)) => left == right,
            (Self::Thread(left), Self::Thread(right)) => left == right,
            (Self::NativeFunction(left), Self::NativeFunction(right)) => left == right,
            _ => false,
        }
    }
}
