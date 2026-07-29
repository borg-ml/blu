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

    #[must_use]
    pub fn for_dialect(dialect: Dialect) -> Self {
        Self {
            compiler: Compiler::default(),
            vm: Vm::new(dialect),
        }
    }

    pub fn execute(&mut self, source: impl AsRef<[u8]>) -> Result<Vec<Value>, ExecuteError> {
        let source = source.as_ref();
        if let Some(dialect) = source_dialect(source)? {
            let configured = self.vm.dialect();
            if dialect != configured {
                return Err(ExecuteError::DialectMismatch {
                    configured,
                    source: dialect,
                });
            }
        }
        let chunk = self
            .compiler
            .compile(source)
            .map_err(ExecuteError::Compile)?;
        self.vm.execute_owned(chunk).map_err(ExecuteError::Runtime)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExecuteError {
    Compile(CompileError),
    Runtime(RuntimeError),
    UnknownDialect(String),
    DialectMismatch {
        configured: Dialect,
        source: Dialect,
    },
}

impl fmt::Display for ExecuteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Compile(error) => write!(f, "source compilation failed: {error}"),
            Self::Runtime(error) => write!(f, "source execution failed: {error}"),
            Self::UnknownDialect(dialect) => {
                write!(f, "source selects unknown dialect {dialect:?}")
            }
            Self::DialectMismatch { configured, source } => write!(
                f,
                "source dialect {source:?} conflicts with configured dialect {configured:?}"
            ),
        }
    }
}

impl std::error::Error for ExecuteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Compile(error) => Some(error),
            Self::Runtime(error) => Some(error),
            Self::UnknownDialect(_) | Self::DialectMismatch { .. } => None,
        }
    }
}

fn source_dialect(source: &[u8]) -> Result<Option<Dialect>, ExecuteError> {
    let first_line = source
        .split(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    let Some(name) = first_line.strip_prefix(b"--!dialect ") else {
        return Ok(None);
    };
    let name = std::str::from_utf8(name)
        .map_err(|_| ExecuteError::UnknownDialect(String::from_utf8_lossy(name).into_owned()))?
        .trim();
    let dialect = match name {
        "blu" => Dialect::Blu,
        "luau" => Dialect::Luau,
        "lua51" => Dialect::Lua51,
        "lua52" => Dialect::Lua52,
        "lua53" => Dialect::Lua53,
        "lua54" => Dialect::Lua54,
        "lua55" => Dialect::Lua55,
        _ => return Err(ExecuteError::UnknownDialect(name.to_owned())),
    };
    Ok(Some(dialect))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn executes_source_through_the_public_facade() {
        assert_eq!(Engine::default().vm().dialect(), Dialect::Blu);
        assert_eq!(
            Engine::default().execute(b"return string.reverse('blu')"),
            Ok(vec![Value::String(std::sync::Arc::from(&b"ulb"[..]))])
        );
    }

    #[test]
    fn source_dialects_are_explicit_and_conflicts_are_rejected() {
        assert_eq!(
            Engine::for_dialect(Dialect::Luau).execute(b"--!dialect luau\nreturn 1"),
            Ok(vec![Value::Number(1.0)])
        );
        assert_eq!(
            Engine::default().execute(b"--!dialect lua54\nreturn 1"),
            Err(ExecuteError::DialectMismatch {
                configured: Dialect::Blu,
                source: Dialect::Lua54,
            })
        );
        assert_eq!(
            Engine::default().execute(b"--!dialect mystery\nreturn 1"),
            Err(ExecuteError::UnknownDialect("mystery".into()))
        );
        assert_eq!(
            Engine::for_dialect(Dialect::Lua54).execute(b"--!dialect lua54\nreturn 1"),
            Err(ExecuteError::Runtime(RuntimeError::DialectNotImplemented(
                Dialect::Lua54
            )))
        );
    }

    #[test]
    fn escaped_closures_keep_their_owning_chunk_across_executions() {
        let mut engine = Engine::default();
        assert_eq!(
            engine.execute(b"saved = function() return 41 end"),
            Ok(Vec::new())
        );
        assert_eq!(
            engine.execute(b"return saved() + 1"),
            Ok(vec![Value::Number(42.0)])
        );
    }

    #[test]
    fn require_uses_the_host_loader_once_and_caches_escaped_functions() {
        let loads = Arc::new(AtomicUsize::new(0));
        let observed = loads.clone();
        let mut engine = Engine::default();
        engine.vm_mut().set_module_loader(move |vm, name| {
            assert_eq!(name, b"answer");
            observed.fetch_add(1, Ordering::SeqCst);
            let chunk = Compiler::default()
                .compile(b"return function(value) return 40 + value end")
                .expect("valid module source");
            Ok(vm
                .execute_owned(chunk)?
                .into_iter()
                .next()
                .unwrap_or(Value::Nil))
        });
        assert_eq!(
            engine.execute(b"return require('answer')(2)"),
            Ok(vec![Value::Number(42.0)])
        );
        engine.vm_mut().collect(std::iter::empty());
        assert_eq!(
            engine.execute(b"return require('answer')(3)"),
            Ok(vec![Value::Number(43.0)])
        );
        assert_eq!(loads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn require_reports_missing_loaders_and_circular_modules_structurally() {
        assert_eq!(
            Engine::default().execute(b"return require('missing')"),
            Err(ExecuteError::Runtime(RuntimeError::ModuleLoaderMissing))
        );

        let mut engine = Engine::default();
        engine.vm_mut().set_module_loader(|vm, name| {
            let source = format!("return require({:?})", String::from_utf8_lossy(name));
            let chunk = Compiler::default()
                .compile(source)
                .expect("valid recursive module source");
            Ok(vm
                .execute_owned(chunk)?
                .into_iter()
                .next()
                .unwrap_or(Value::Nil))
        });
        assert_eq!(
            engine.execute(b"return require('cycle')"),
            Err(ExecuteError::Runtime(RuntimeError::CircularModule(
                Arc::from(&b"cycle"[..])
            )))
        );
    }

    #[test]
    fn expanded_standard_libraries_preserve_bytes_multret_and_math() {
        assert_eq!(
            Engine::default().execute(
                br#"
                    local packed = table.pack(4, nil, 6)
                    return string.char(65, 0, 255),
                        string.rep("ab", 3, "-"),
                        string.lower("A\255Z"),
                        string.upper("a\255z"),
                        packed.n,
                        math.deg(math.pi),
                        math.log(8, 2),
                        table.unpack(packed, 1, 3)
                "#
            ),
            Ok(vec![
                Value::String(Arc::from(&b"A\0\xff"[..])),
                Value::String(Arc::from(&b"ab-ab-ab"[..])),
                Value::String(Arc::from(&b"a\xffz"[..])),
                Value::String(Arc::from(&b"A\xffZ"[..])),
                Value::Integer(3),
                Value::Number(180.0),
                Value::Number(3.0),
                Value::Number(4.0),
                Value::Nil,
                Value::Number(6.0),
            ])
        );
        assert_eq!(
            Engine::for_dialect(Dialect::Luau).execute(b"return string.rep('ab', 3, '-')"),
            Ok(vec![Value::String(Arc::from(&b"ababab"[..]))])
        );
    }

    #[test]
    fn ordinary_calls_use_the_bounded_explicit_frame_stack() {
        let mut engine = Engine::default();
        *engine.vm_mut() = Vm::default().with_call_limit(10_000);
        let source = br#"
                    local function descend(remaining)
                        if remaining == 0 then
                            return 0
                        end
                        return descend(remaining - 1) + 1
                    end
                    return descend(5_000)
                "#;
        assert_eq!(engine.execute(source), Ok(vec![Value::Number(5_000.0)]));
    }

    #[test]
    fn suspended_explicit_frames_remain_gc_roots() {
        let mut engine = Engine::default();
        let collect = engine.vm_mut().register_function(|vm, _| {
            vm.collect(std::iter::empty());
            Ok(Vec::new())
        });
        engine
            .vm_mut()
            .set_global(&b"collect"[..], Value::NativeFunction(collect));
        assert_eq!(
            engine.execute(
                br#"
                    local retained = { answer = 42 }
                    local function child()
                        collect()
                    end
                    child()
                    return retained.answer
                "#
            ),
            Ok(vec![Value::Number(42.0)])
        );
    }
}
