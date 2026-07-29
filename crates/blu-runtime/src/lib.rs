#![forbid(unsafe_code)]

//! Blu's Lua-family runtime.
//!
//! The current interpreter executes a deliberately enumerated subset of pinned
//! Luau bytecode. Encountering an instruction without ported semantics is a
//! hard error; Blu never silently treats unsupported behavior as a no-op.

pub use blu_bytecode as bytecode;

mod dialect;
mod heap;
mod memory;
mod value;
mod vm;

pub use dialect::Dialect;
pub use heap::{ClosureId, CollectionStats, Heap, HeapError, TableId, ThreadId};
pub use memory::{
    MemoryAccount, MemoryConfig, MemoryError, MemoryReservation, MemoryUsage, checked_hash_bytes,
    checked_reallocation_peak, checked_vector_bytes,
};
pub use value::{NativeFunctionId, Value};
pub use vm::{RuntimeError, Vm};
