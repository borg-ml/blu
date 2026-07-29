#![forbid(unsafe_code)]

//! Luau-compatible instruction and serialized chunk decoding.
//!
//! Opcode numbers and layouts are compatibility-sensitive. They are ported
//! from Luau's `Common/include/Luau/Bytecode.h` at the revision recorded in
//! the repository's `UPSTREAM.toml`.

mod chunk;
mod disassemble;
mod instruction;
mod opcode;
mod validate;

pub use chunk::{
    Chunk, ChunkError, Constant, DebugInfo, DebugLocal, FeedbackSlot, LineInfo, LoadLimits,
    Prototype, ValidatedChunk, load, load_validated,
};
pub use disassemble::disassemble;
pub use instruction::{DecodeError, Instruction, InstructionIter, decode};
pub use opcode::Opcode;
pub use validate::{ValidationError, validate};

pub const BYTECODE_VERSION_MIN: u8 = 3;
pub const BYTECODE_VERSION_MAX: u8 = 12;
pub const BYTECODE_VERSION_TARGET: u8 = 9;
pub const TYPEINFO_VERSION_MIN: u8 = 1;
pub const TYPEINFO_VERSION_MAX: u8 = 3;
pub const TYPEINFO_VERSION_TARGET: u8 = 3;
pub const MAX_TABLE_INITIAL_CAPACITY: usize = 1 << 20;
