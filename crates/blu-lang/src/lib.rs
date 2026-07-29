//! Public embedding interface for Blu.
//!
//! Most applications should depend only on `blu-lang`. Lower-level bytecode and
//! runtime crates remain public for tooling and specialized embedders.

pub use blu_bytecode as bytecode;
pub use blu_compiler::{CompileError, CompileOptions, Compiler, LUAU_COMPILER_RELEASE};
pub use blu_runtime::{Dialect, RuntimeError, Value, Vm};

use core::fmt;

#[derive(Clone, Debug, Default)]
pub struct Engine {
    compiler: Compiler,
    vm: Vm,
}

impl Engine {
    #[must_use]
    pub const fn new(compiler: Compiler, vm: Vm) -> Self {
        Self { compiler, vm }
    }

    #[must_use]
    pub const fn vm(&self) -> &Vm {
        &self.vm
    }

    pub const fn vm_mut(&mut self) -> &mut Vm {
        &mut self.vm
    }

    pub fn execute(&mut self, source: impl AsRef<[u8]>) -> Result<Vec<Value>, ExecuteError> {
        let chunk = self
            .compiler
            .compile(source)
            .map_err(ExecuteError::Compile)?;
        self.vm.execute(&chunk).map_err(ExecuteError::Runtime)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExecuteError {
    Compile(CompileError),
    Runtime(RuntimeError),
}

impl fmt::Display for ExecuteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Compile(error) => write!(f, "source compilation failed: {error}"),
            Self::Runtime(error) => write!(f, "source execution failed: {error}"),
        }
    }
}

impl std::error::Error for ExecuteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Compile(error) => Some(error),
            Self::Runtime(error) => Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executes_source_through_the_public_facade() {
        assert_eq!(
            Engine::default().execute(b"return string.reverse('blu')"),
            Ok(vec![Value::String(std::sync::Arc::from(&b"ulb"[..]))])
        );
    }
}
