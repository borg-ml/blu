//! In-process Luau source compilation for Blu.
//!
//! The foreign compiler boundary is isolated in this crate. Every returned
//! buffer is copied into Rust-owned memory, released with the matching C
//! allocator, and decoded under [`blu_bytecode::LoadLimits`] before use.

use blu_bytecode::{Chunk, ChunkError, LoadLimits, load};
use core::fmt;
use std::ffi::{c_char, c_int, c_void};

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
    pub chunk: Chunk,
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
        self.compile_bytecode(source).map(|compiled| compiled.chunk)
    }

    pub fn compile_bytecode(
        &self,
        source: impl AsRef<[u8]>,
    ) -> Result<CompiledBytecode, CompileError> {
        validate_level("optimization", self.options.optimization_level, 2)?;
        validate_level("debug", self.options.debug_level, 2)?;
        validate_level("type information", self.options.type_info_level, 1)?;
        validate_level("coverage", self.options.coverage_level, 2)?;

        let source = source.as_ref();
        let mut size = 0;
        let mut options = NativeCompileOptions {
            optimization_level: c_int::from(self.options.optimization_level),
            debug_level: c_int::from(self.options.debug_level),
            type_info_level: c_int::from(self.options.type_info_level),
            coverage_level: c_int::from(self.options.coverage_level),
            vector_lib: core::ptr::null(),
            vector_ctor: core::ptr::null(),
            vector_type: core::ptr::null(),
            mutable_globals: core::ptr::null(),
            userdata_types: core::ptr::null(),
            libraries_with_known_members: core::ptr::null(),
            library_member_type_callback: None,
            library_member_constant_callback: None,
            disabled_builtins: core::ptr::null(),
        };
        // SAFETY: `source` is valid for `source.len()` bytes, `options` and
        // `size` remain live for the call, and the returned allocation is
        // copied before being released with the compiler's C allocator.
        let output = unsafe {
            luau_compile(
                source.as_ptr().cast(),
                source.len(),
                &raw mut options,
                &raw mut size,
            )
        };
        if output.is_null() {
            return Err(CompileError::Allocation);
        }
        // SAFETY: Luau reports the exact initialized allocation size.
        let bytes = unsafe { core::slice::from_raw_parts(output.cast::<u8>(), size) }.to_vec();
        // SAFETY: `luau_compile` documents that its result must be released
        // with `free`, and this pointer has not previously been freed.
        unsafe { free(output.cast()) };
        let chunk = load(&bytes, self.load_limits).map_err(CompileError::Chunk)?;
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
    Chunk(ChunkError),
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

type LibraryMemberTypeCallback = unsafe extern "C" fn(*const c_char, *const c_char) -> c_int;
type LibraryMemberConstantCallback =
    unsafe extern "C" fn(*const c_char, *const c_char, *mut *mut c_void);

#[repr(C)]
struct NativeCompileOptions {
    optimization_level: c_int,
    debug_level: c_int,
    type_info_level: c_int,
    coverage_level: c_int,
    vector_lib: *const c_char,
    vector_ctor: *const c_char,
    vector_type: *const c_char,
    mutable_globals: *const *const c_char,
    userdata_types: *const *const c_char,
    libraries_with_known_members: *const *const c_char,
    library_member_type_callback: Option<LibraryMemberTypeCallback>,
    library_member_constant_callback: Option<LibraryMemberConstantCallback>,
    disabled_builtins: *const *const c_char,
}

unsafe extern "C" {
    fn luau_compile(
        source: *const c_char,
        size: usize,
        options: *mut NativeCompileOptions,
        output_size: *mut usize,
    ) -> *mut c_char;
    fn free(pointer: *mut c_void);
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
            load(&compiled.bytes, LoadLimits::default()).unwrap(),
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
}
