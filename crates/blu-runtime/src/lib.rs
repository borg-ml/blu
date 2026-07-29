//! Blu's Lua-family runtime.
//!
//! The current interpreter executes a deliberately enumerated subset of pinned
//! Luau bytecode. Encountering an instruction without ported semantics is a
//! hard error; Blu never silently treats unsupported behavior as a no-op.

pub use blu_bytecode as bytecode;

mod dialect;
mod value;
mod vm;

pub use dialect::Dialect;
pub use value::Value;
pub use vm::{RuntimeError, Vm};
