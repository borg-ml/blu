use crate::{ClosureId, TableId, ThreadId};
use core::cmp::Ordering;
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
    CoroutineFunction(ThreadId),
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
            Self::CoroutineFunction(_) => "function",
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

    pub(crate) fn numeric_equal(&self, other: &Self) -> Option<bool> {
        match (self, other) {
            (Self::Number(left), Self::Number(right)) => Some(left == right),
            (Self::Integer(left), Self::Integer(right)) => Some(left == right),
            (Self::Integer(integer), Self::Number(number))
            | (Self::Number(number), Self::Integer(integer)) => {
                Some(integer_number_order(*integer, *number) == Some(Ordering::Equal))
            }
            _ => None,
        }
    }

    pub(crate) fn numeric_less(&self, other: &Self) -> Option<bool> {
        match (self, other) {
            (Self::Number(left), Self::Number(right)) => Some(left < right),
            (Self::Integer(left), Self::Integer(right)) => Some(left < right),
            (Self::Integer(integer), Self::Number(number)) => {
                Some(integer_number_order(*integer, *number) == Some(Ordering::Less))
            }
            (Self::Number(number), Self::Integer(integer)) => {
                Some(integer_number_order(*integer, *number) == Some(Ordering::Greater))
            }
            _ => None,
        }
    }

    pub(crate) fn numeric_less_equal(&self, other: &Self) -> Option<bool> {
        match (self, other) {
            (Self::Number(left), Self::Number(right)) => Some(left <= right),
            (Self::Integer(left), Self::Integer(right)) => Some(left <= right),
            (Self::Integer(integer), Self::Number(number)) => Some(matches!(
                integer_number_order(*integer, *number),
                Some(Ordering::Less | Ordering::Equal)
            )),
            (Self::Number(number), Self::Integer(integer)) => Some(matches!(
                integer_number_order(*integer, *number),
                Some(Ordering::Greater | Ordering::Equal)
            )),
            _ => None,
        }
    }
}

fn integer_number_order(integer: i64, number: f64) -> Option<Ordering> {
    if number.is_nan() {
        return None;
    }
    if number >= -(i64::MIN as f64) {
        return Some(Ordering::Less);
    }
    if number < i64::MIN as f64 {
        return Some(Ordering::Greater);
    }
    let truncated = number as i64;
    match integer.cmp(&truncated) {
        Ordering::Equal if number > truncated as f64 => Some(Ordering::Less),
        Ordering::Equal if number < truncated as f64 => Some(Ordering::Greater),
        ordering => Some(ordering),
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
            Self::CoroutineFunction(value) => {
                f.debug_tuple("CoroutineFunction").field(value).finish()
            }
            Self::NativeFunction(value) => value.fmt(f),
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Nil, Self::Nil) => true,
            (Self::Boolean(left), Self::Boolean(right)) => left == right,
            (Self::Number(_), Self::Number(_))
            | (Self::Integer(_), Self::Integer(_))
            | (Self::Number(_), Self::Integer(_))
            | (Self::Integer(_), Self::Number(_)) => self.numeric_equal(other).unwrap_or(false),
            (Self::String(left), Self::String(right)) => left == right,
            (Self::Table(left), Self::Table(right)) => left == right,
            (Self::Closure(left), Self::Closure(right)) => left == right,
            (Self::Thread(left), Self::Thread(right)) => left == right,
            (Self::CoroutineFunction(left), Self::CoroutineFunction(right)) => left == right,
            (Self::NativeFunction(left), Self::NativeFunction(right)) => left == right,
            _ => false,
        }
    }
}
