#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

//! In-process Luau source compilation for Blu.
//!
//! The foreign compiler boundary is isolated in this crate. Every returned
//! buffer is copied into Rust-owned memory, released with the matching C
//! allocator, and decoded under [`blu_bytecode::LoadLimits`] before use.

use blu_bytecode::{Chunk, ChunkError, LoadLimits, ValidatedChunk, load_validated};
use core::fmt;

#[allow(unsafe_code)]
mod ffi;

/// The bundled compiler's Luau release number.
pub const LUAU_COMPILER_RELEASE: &str = env!("LUAU_VERSION");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompileOptions {
    pub optimization_level: u8,
    pub debug_level: u8,
    pub type_info_level: u8,
    pub coverage_level: u8,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            optimization_level: 1,
            debug_level: 1,
            type_info_level: 0,
            coverage_level: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Compiler {
    options: CompileOptions,
    load_limits: LoadLimits,
}

#[derive(Clone, Debug)]
pub struct CompiledBytecode {
    pub bytes: Vec<u8>,
    pub chunk: ValidatedChunk,
}

impl Compiler {
    #[must_use]
    pub const fn new(options: CompileOptions, load_limits: LoadLimits) -> Self {
        Self {
            options,
            load_limits,
        }
    }

    pub fn compile(&self, source: impl AsRef<[u8]>) -> Result<Chunk, CompileError> {
        self.compile_bytecode(source)
            .map(|compiled| compiled.chunk.into_chunk())
    }

    pub fn compile_bytecode(
        &self,
        source: impl AsRef<[u8]>,
    ) -> Result<CompiledBytecode, CompileError> {
        validate_level("optimization", self.options.optimization_level, 2)?;
        validate_level("debug", self.options.debug_level, 2)?;
        validate_level("type information", self.options.type_info_level, 1)?;
        validate_level("coverage", self.options.coverage_level, 2)?;

        let bytes = ffi::compile(
            source.as_ref(),
            self.options.optimization_level,
            self.options.debug_level,
            self.options.type_info_level,
            self.options.coverage_level,
            self.load_limits.max_bytes,
        )
        .map_err(CompileError::from_ffi)?;
        let chunk = load_validated(&bytes, self.load_limits).map_err(CompileError::Chunk)?;
        Ok(CompiledBytecode { bytes, chunk })
    }
}

fn validate_level(name: &'static str, value: u8, maximum: u8) -> Result<(), CompileError> {
    if value <= maximum {
        Ok(())
    } else {
        Err(CompileError::Option {
            name,
            value,
            maximum,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompileError {
    Option {
        name: &'static str,
        value: u8,
        maximum: u8,
    },
    Allocation,
    NativeException,
    NativeContract {
        status: i32,
        output_is_null: bool,
        output_size: usize,
    },
    Chunk(ChunkError),
}

impl CompileError {
    fn from_ffi(error: ffi::Error) -> Self {
        match error {
            ffi::Error::Allocation => Self::Allocation,
            ffi::Error::Exception => Self::NativeException,
            ffi::Error::Contract {
                status,
                output_is_null,
                output_size,
            } => Self::NativeContract {
                status,
                output_is_null,
                output_size,
            },
            ffi::Error::TooLarge { actual, limit } => Self::Chunk(ChunkError::TooLarge {
                what: "bytecode bytes",
                actual,
                limit,
            }),
        }
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option {
                name,
                value,
                maximum,
            } => write!(f, "{name} level {value} exceeds maximum {maximum}"),
            Self::Allocation => f.write_str("Luau compiler failed to allocate its output"),
            Self::NativeException => {
                f.write_str("Luau compiler raised an unexpected native exception")
            }
            Self::NativeContract {
                status,
                output_is_null,
                output_size,
            } => write!(
                f,
                "Luau compiler violated its native output contract \
                 (status {status}, null output {output_is_null}, size {output_size})"
            ),
            Self::Chunk(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for CompileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Chunk(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blu_runtime::{Value, Vm};

    #[test]
    fn compiles_source_for_the_blu_runtime() {
        let compiled = Compiler::default()
            .compile_bytecode(b"return 20 + 22")
            .expect("valid source");
        assert_eq!(
            load_validated(&compiled.bytes, LoadLimits::default()).unwrap(),
            compiled.chunk
        );
        assert_eq!(
            Vm::default().execute(&compiled.chunk),
            Ok(vec![Value::Number(42.0)])
        );
    }

    #[test]
    fn returns_structured_syntax_and_option_errors() {
        assert!(matches!(
            Compiler::default().compile(b"local ="),
            Err(CompileError::Chunk(ChunkError::CompileError(_)))
        ));
        assert!(matches!(
            Compiler::new(
                CompileOptions {
                    optimization_level: 3,
                    ..CompileOptions::default()
                },
                LoadLimits::default(),
            )
            .compile(b"return 1"),
            Err(CompileError::Option {
                name: "optimization",
                value: 3,
                maximum: 2,
            })
        ));
    }

    #[test]
    fn accepts_empty_and_binary_source_boundaries() {
        Compiler::default().compile_bytecode([]).unwrap();

        for source in [b"\0".as_slice(), b"return '\0'".as_slice(), &[0xff, 0xfe]] {
            assert!(matches!(
                Compiler::default().compile_bytecode(source),
                Ok(_) | Err(CompileError::Chunk(ChunkError::CompileError(_)))
            ));
        }
    }

    #[test]
    fn enforces_output_limit_before_copying() {
        let limits = LoadLimits {
            max_bytes: 1,
            ..LoadLimits::default()
        };
        assert!(matches!(
            Compiler::new(CompileOptions::default(), limits).compile_bytecode(b"return 1"),
            Err(CompileError::Chunk(ChunkError::TooLarge {
                what: "bytecode bytes",
                actual,
                limit: 1,
            })) if actual > 1
        ));
    }

    #[test]
    fn translates_native_exceptions_to_statuses() {
        assert_eq!(ffi::test_exception(1), Err(ffi::Error::Allocation));
        assert_eq!(ffi::test_exception(2), Err(ffi::Error::Exception));
        assert_eq!(ffi::test_exception(0), Ok(()));
    }
}
