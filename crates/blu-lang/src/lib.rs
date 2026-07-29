#![forbid(unsafe_code)]

//! Public embedding interface for Blu.
//!
//! Most applications should depend only on `blu-lang`. Lower-level bytecode and
//! runtime crates remain public for tooling and specialized embedders.

pub use blu_bytecode as bytecode;
pub use blu_compiler::{
    CompileError, CompileOptions, CompiledBytecode, Compiler, LUAU_COMPILER_RELEASE,
};
pub use blu_package as package;
pub use blu_runtime::{Dialect, RuntimeError, Value, Vm};

/// Explicit, Blu-owned source frontend APIs.
///
/// This module exposes the safe-Rust BluV1 compiler slice through the public
/// facade. Compilation requires caller-selected source and compiler identities
/// plus a semantic profile. It never selects the legacy Luau compiler as a
/// fallback.
pub mod frontend {
    pub use blu_compiler::owned::{
        OwnedCompilation, OwnedCompileError, OwnedCompileLimit, OwnedCompileLimits, OwnedCompiler,
    };
    pub use blu_core::{
        CompilerId, CompilerIdentity, IdentityLimits, SemanticProfile, SourceError, SourceFile,
        SourceId, SourceLimits,
    };
}

use blu_package::{AuthorityProfile, Package, PackageDialect};
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
            .compile_bytecode(source)
            .map_err(ExecuteError::Compile)?;
        self.vm
            .execute_validated_owned(chunk.chunk)
            .map_err(ExecuteError::Runtime)
    }

    /// Executes a Blu-owned compilation directly as BluV1.
    ///
    /// The runtime consumes and revalidates the artifact under the caller's
    /// execution limits. This path never invokes or falls back to the legacy
    /// Luau compiler.
    pub fn execute_owned_compilation(
        &mut self,
        compilation: frontend::OwnedCompilation,
        execution_limits: bytecode::blu::BluLimits,
    ) -> Result<Vec<Value>, RuntimeError> {
        self.vm
            .execute_blu_v1(compilation.into_validated_artifact(), execution_limits)
    }

    pub fn execute_package(
        &mut self,
        package: Package,
        policy: &HostPolicy,
    ) -> Result<Vec<Value>, ExecutePackageError> {
        let package_dialect = match package.manifest().dialect {
            PackageDialect::Blu => Dialect::Blu,
            PackageDialect::Luau => Dialect::Luau,
            PackageDialect::Lua51
            | PackageDialect::Lua52
            | PackageDialect::Lua53
            | PackageDialect::Lua54
            | PackageDialect::Lua55 => {
                return Err(ExecutePackageError::UnsupportedDialect(
                    package.manifest().dialect,
                ));
            }
            profile => return Err(ExecutePackageError::UnsupportedDialect(profile)),
        };
        let configured = self.vm.dialect();
        if package_dialect != configured {
            return Err(ExecutePackageError::DialectMismatch {
                configured,
                package: package_dialect,
            });
        }
        let required = package.manifest().authority.profile;
        if required > policy.authority {
            return Err(ExecutePackageError::Authority {
                required,
                granted: policy.authority,
            });
        }
        if !package.manifest().authority.capabilities.is_empty() {
            return Err(ExecutePackageError::CapabilitiesUnsupported(
                package.manifest().authority.capabilities.len(),
            ));
        }
        if !package.manifest().imports.is_empty() {
            return Err(ExecutePackageError::ImportsUnsupported(
                package.manifest().imports.len(),
            ));
        }
        self.vm
            .execute_validated_owned(package.into_validated_chunk())
            .map_err(ExecutePackageError::Runtime)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostPolicy {
    pub authority: AuthorityProfile,
}

impl Default for HostPolicy {
    fn default() -> Self {
        Self {
            authority: AuthorityProfile::Pure,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExecutePackageError {
    UnsupportedDialect(PackageDialect),
    DialectMismatch {
        configured: Dialect,
        package: Dialect,
    },
    Authority {
        required: AuthorityProfile,
        granted: AuthorityProfile,
    },
    CapabilitiesUnsupported(usize),
    ImportsUnsupported(usize),
    Runtime(RuntimeError),
}

impl fmt::Display for ExecutePackageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedDialect(dialect) => {
                write!(
                    f,
                    "package dialect {dialect:?} has no executable V1 payload"
                )
            }
            Self::DialectMismatch {
                configured,
                package,
            } => write!(
                f,
                "package dialect {package:?} conflicts with configured dialect {configured:?}"
            ),
            Self::Authority { required, granted } => write!(
                f,
                "package requires {required:?} authority but host grants {granted:?}"
            ),
            Self::CapabilitiesUnsupported(count) => write!(
                f,
                "package declares {count} capability requirements but capability matching is not implemented"
            ),
            Self::ImportsUnsupported(count) => write!(
                f,
                "package declares {count} imports but package linking is not implemented"
            ),
            Self::Runtime(error) => write!(f, "package execution failed: {error}"),
        }
    }
}

impl std::error::Error for ExecutePackageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Runtime(error) => Some(error),
            _ => None,
        }
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
    use blu_package::{
        AuthorityRequirement, BytecodeDescriptor, BytecodeFormat, Manifest, Name, PackageIdentity,
        PackageLimits, Version,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn owned_frontend_compiles_and_executes_every_baseline_profile() {
        use frontend::{
            CompilerId, CompilerIdentity, IdentityLimits, OwnedCompiler, SemanticProfile,
            SourceFile, SourceId, SourceLimits,
        };

        let source = SourceFile::new(
            SourceId::new(1),
            "answer.blu",
            b"local empty = nil\nlocal yes = true\nreturn empty, yes, false, 'blu', not empty, not yes, not 0, not 'blu', (40 + 2), 40 - 2 - 3, 2 + 5 * 8".to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let empty_return = SourceFile::new(
            SourceId::new(2),
            "empty.blu",
            b"return".to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let implicit_return = SourceFile::new(
            SourceId::new(3),
            "implicit.blu",
            b"local answer = 42".to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let nil_local = SourceFile::new(
            SourceId::new(4),
            "nil-local.blu",
            b"local missing\nreturn missing".to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let assignment = SourceFile::new(
            SourceId::new(5),
            "assignment.blu",
            b"local answer = 40\nanswer = answer + 2\nreturn answer".to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let semicolons = SourceFile::new(
            SourceId::new(6),
            "semicolons.blu",
            b";local answer = 40;answer = answer + 2;;return answer;".to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let local_list = SourceFile::new(
            SourceId::new(7),
            "local-list.blu",
            b"local value = 40\nlocal value, next, missing = value, value + 2\nreturn value, next, missing".to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let assignment_list = SourceFile::new(
            SourceId::new(8),
            "assignment-list.blu",
            b"local first, second = 1, 2\nfirst, second = second, first\nreturn first, second"
                .to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let compiler = CompilerIdentity::new(
            CompilerId::new(*b"blu-owned-v1\0\0\0\0"),
            "blu-owned",
            env!("CARGO_PKG_VERSION"),
            None,
            IdentityLimits::default(),
        )
        .unwrap();
        for profile in SemanticProfile::ALL {
            let compiled = OwnedCompiler::default()
                .compile(&source, profile, compiler.clone())
                .unwrap();
            let artifact = compiled.artifact().artifact();
            assert_eq!(artifact.prototypes[artifact.main as usize].profile, profile);
            assert!(!compiled.bytes().is_empty());
            assert_eq!(
                Engine::default()
                    .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                Ok(vec![
                    Value::Nil,
                    Value::Boolean(true),
                    Value::Boolean(false),
                    Value::String(Arc::from(&b"blu"[..])),
                    Value::Boolean(true),
                    Value::Boolean(false),
                    Value::Boolean(false),
                    Value::Boolean(false),
                    Value::Number(42.0),
                    Value::Number(35.0),
                    Value::Number(42.0),
                ]),
                "{profile}"
            );
            let compiled = OwnedCompiler::default()
                .compile(&empty_return, profile, compiler.clone())
                .unwrap();
            assert_eq!(
                Engine::default()
                    .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                Ok(Vec::new()),
                "empty return under {profile}"
            );
            let compiled = OwnedCompiler::default()
                .compile(&implicit_return, profile, compiler.clone())
                .unwrap();
            assert_eq!(
                Engine::default()
                    .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                Ok(Vec::new()),
                "implicit return under {profile}"
            );
            let compiled = OwnedCompiler::default()
                .compile(&nil_local, profile, compiler.clone())
                .unwrap();
            assert_eq!(
                Engine::default()
                    .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                Ok(vec![Value::Nil]),
                "uninitialized local under {profile}"
            );
            let compiled = OwnedCompiler::default()
                .compile(&assignment, profile, compiler.clone())
                .unwrap();
            assert_eq!(
                Engine::default()
                    .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                Ok(vec![if matches!(
                    profile,
                    SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
                ) {
                    Value::Integer(42)
                } else {
                    Value::Number(42.0)
                }]),
                "local assignment under {profile}"
            );
            let compiled = OwnedCompiler::default()
                .compile(&semicolons, profile, compiler.clone())
                .unwrap();
            assert_eq!(
                Engine::default()
                    .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                Ok(vec![if matches!(
                    profile,
                    SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
                ) {
                    Value::Integer(42)
                } else {
                    Value::Number(42.0)
                }]),
                "semicolon-separated statements under {profile}"
            );
            let compiled = OwnedCompiler::default()
                .compile(&local_list, profile, compiler.clone())
                .unwrap();
            let numeric = matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            );
            assert_eq!(
                Engine::default()
                    .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                Ok(vec![
                    if numeric {
                        Value::Integer(40)
                    } else {
                        Value::Number(40.0)
                    },
                    if numeric {
                        Value::Integer(42)
                    } else {
                        Value::Number(42.0)
                    },
                    Value::Nil,
                ]),
                "local list adjustment under {profile}"
            );
            let compiled = OwnedCompiler::default()
                .compile(&assignment_list, profile, compiler.clone())
                .unwrap();
            assert_eq!(
                Engine::default()
                    .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                Ok(vec![
                    if numeric {
                        Value::Integer(2)
                    } else {
                        Value::Number(2.0)
                    },
                    if numeric {
                        Value::Integer(1)
                    } else {
                        Value::Number(1.0)
                    },
                ]),
                "assignment list swap under {profile}"
            );
        }
    }

    #[test]
    fn executes_source_through_the_public_facade() {
        assert_eq!(Engine::default().vm().dialect(), Dialect::Blu);
        assert_eq!(
            Engine::default().execute(b"return string.reverse('blu')"),
            Ok(vec![Value::String(std::sync::Arc::from(&b"ulb"[..]))])
        );
    }

    #[test]
    fn compiled_portable_packages_round_trip_and_execute_under_host_policy() {
        let compiled = Compiler::default()
            .compile_bytecode(b"return 40 + 2")
            .unwrap();
        let manifest = Manifest {
            package: PackageIdentity {
                name: Name::new("example.answer").unwrap(),
                version: Version::new(1, 0, 0),
            },
            dialect: PackageDialect::Blu,
            bytecode: BytecodeDescriptor {
                format: BytecodeFormat::Luau,
                version: compiled.chunk.version,
                typeinfo_version: compiled.chunk.typeinfo_version,
            },
            authority: AuthorityRequirement {
                profile: AuthorityProfile::Pure,
                capabilities: Vec::new(),
            },
            imports: Vec::new(),
            exports: Vec::new(),
        };
        let package = Package::new(manifest, compiled.bytes, PackageLimits::default()).unwrap();
        let encoded = package.encode();
        let decoded = Package::decode(&encoded, PackageLimits::default()).unwrap();
        assert_eq!(
            Engine::default().execute_package(decoded, &HostPolicy::default()),
            Ok(vec![Value::Number(42.0)])
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
        engine.vm_mut().collect(std::iter::empty()).unwrap();
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
    fn automatic_gc_reclaims_unreachable_objects_before_enforcing_the_heap_limit() {
        let baseline = Vm::default().heap().live_objects();
        let mut collecting = Engine::new(
            Compiler::default(),
            Vm::default().with_heap_object_limit(baseline + 2),
        );
        assert_eq!(
            collecting.execute(
                br#"
                    local value = {}
                    for _ = 1, 1_000 do
                        value = {}
                    end
                    return type(value)
                "#
            ),
            Ok(vec![Value::String(Arc::from(&b"table"[..]))])
        );
        assert!(collecting.vm().heap().live_objects() <= baseline + 2);

        let mut retained = Engine::new(
            Compiler::default(),
            Vm::default().with_heap_object_limit(baseline + 3),
        );
        assert!(matches!(
            retained.execute(
                br#"
                    local keep = {}
                    keep[1] = {}
                    keep[2] = {}
                    keep[3] = {}
                "#
            ),
            Err(ExecuteError::Runtime(RuntimeError::HeapObjectLimit {
                limit,
                ..
            })) if limit == baseline + 3
        ));
    }

    #[test]
    fn automatic_byte_gc_preserves_guest_globals_frames_and_suspended_threads() {
        let vm = Vm::try_new_with_memory(
            Dialect::Blu,
            blu_runtime::MemoryConfig {
                hard_limit_bytes: None,
                gc_start_bytes: 0,
                gc_growth_percent: 0,
                max_single_allocation_bytes: usize::MAX,
            },
        )
        .unwrap();
        let mut engine = Engine::new(Compiler::default(), vm);

        assert_eq!(
            engine.execute(
                br#"
                    byte_gc_global = { value = 11 }
                    local frame_value = { value = 22 }
                    local thread = coroutine.create(function()
                        local suspended_value = { value = 33 }
                        coroutine.yield()
                        return suspended_value.value
                    end)
                    local started = coroutine.resume(thread)
                    local garbage = { 1, 2, 3, 4 }
                    garbage = {}
                    local resumed, thread_value = coroutine.resume(thread)
                    return byte_gc_global.value, frame_value.value,
                        started, resumed, thread_value, type(garbage)
                "#
            ),
            Ok(vec![
                Value::Number(11.0),
                Value::Number(22.0),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Number(33.0),
                Value::String(Arc::from(&b"table"[..])),
            ])
        );
        assert!(engine.vm().memory_usage().collections > 0);
    }

    #[test]
    fn automatic_byte_gc_preserves_unattached_open_upvalue_cells_for_reuse() {
        let vm = Vm::try_new_with_memory(
            Dialect::Blu,
            blu_runtime::MemoryConfig {
                hard_limit_bytes: None,
                gc_start_bytes: 0,
                gc_growth_percent: 0,
                max_single_allocation_bytes: usize::MAX,
            },
        )
        .unwrap();
        let mut engine = Engine::new(Compiler::default(), vm);

        assert_eq!(
            engine.execute(
                br#"
                    local captured = { answer = 41 }
                    local discarded = function()
                        return captured
                    end
                    discarded = nil
                    local reuse_collected_cell = {}
                    local reused = function()
                        captured.answer = captured.answer + 1
                        return captured.answer
                    end
                    return reused(), type(reuse_collected_cell)
                "#
            ),
            Ok(vec![
                Value::Number(42.0),
                Value::String(Arc::from(&b"table"[..])),
            ])
        );
        assert!(engine.vm().memory_usage().collections > 0);
    }

    #[test]
    fn returned_heap_values_remain_rooted_until_explicit_release() {
        let vm = Vm::try_new_with_memory(
            Dialect::Blu,
            blu_runtime::MemoryConfig {
                hard_limit_bytes: None,
                gc_start_bytes: 0,
                gc_growth_percent: 0,
                max_single_allocation_bytes: usize::MAX,
            },
        )
        .unwrap();
        let mut engine = Engine::new(Compiler::default(), vm);
        let values = engine
            .execute(
                br#"
                    local retained = { answer = 42 }
                    local closure = function(addend)
                        return retained.answer + addend
                    end
                    local thread = coroutine.create(function()
                        return retained.answer
                    end)
                    return retained, closure, thread
                "#,
            )
            .unwrap();
        let table = match values[0] {
            Value::Table(table) => table,
            ref value => panic!("expected table, got {value:?}"),
        };
        assert_eq!(engine.vm().retained_value_count(), 3);

        assert_eq!(
            engine.execute(
                br#"
                    for index = 1, 100 do
                        local garbage = { index, index + 1, index + 2 }
                    end
                    return "allocated"
                "#
            ),
            Ok(vec![Value::String(Arc::from(&b"allocated"[..]))])
        );
        engine
            .vm_mut()
            .set_global(&b"held_table"[..], values[0].clone());
        engine
            .vm_mut()
            .set_global(&b"held_closure"[..], values[1].clone());
        engine
            .vm_mut()
            .set_global(&b"held_thread"[..], values[2].clone());
        assert_eq!(
            engine.execute(
                br#"
                    local ok, thread_value = coroutine.resume(held_thread)
                    return held_table.answer, held_closure(1), ok, thread_value
                "#
            ),
            Ok(vec![
                Value::Number(42.0),
                Value::Number(43.0),
                Value::Boolean(true),
                Value::Number(42.0),
            ])
        );

        assert_eq!(engine.vm_mut().release_values(&values), 3);
        assert_eq!(engine.vm().retained_value_count(), 0);
        engine.vm_mut().set_global(&b"held_table"[..], Value::Nil);
        engine.vm_mut().set_global(&b"held_closure"[..], Value::Nil);
        engine.vm_mut().set_global(&b"held_thread"[..], Value::Nil);
        engine.vm_mut().collect(std::iter::empty()).unwrap();
        assert!(matches!(
            engine
                .vm()
                .heap()
                .table_get(table, &Value::Integer(1)),
            Err(blu_runtime::HeapError::StaleTable(stale)) if stale == table
        ));
    }

    #[test]
    fn returned_heap_value_retention_limit_is_atomic() {
        let mut engine = Engine::new(Compiler::default(), Vm::default().with_host_value_limit(2));
        let retained = engine.execute(b"return {}").unwrap();
        let retained_table = match retained[0] {
            Value::Table(table) => table,
            ref value => panic!("expected table, got {value:?}"),
        };
        assert_eq!(engine.vm().retained_value_count(), 1);
        assert_eq!(engine.vm().host_value_limit(), 2);

        assert!(matches!(
            engine.execute(
                br#"
                    return {}, function() return 1 end
                "#
            ),
            Err(ExecuteError::Runtime(RuntimeError::HostValueLimit {
                required: 3,
                limit: 2,
            }))
        ));
        assert_eq!(
            engine.vm().retained_value_count(),
            1,
            "a rejected result must not alter existing retained occurrences"
        );
        engine.vm_mut().collect(std::iter::empty()).unwrap();
        assert_eq!(engine.vm().heap().table_length(retained_table), Ok(0));
        assert_eq!(engine.vm_mut().release_values(&retained), 1);
    }

    #[test]
    fn duplicate_returned_handles_release_one_occurrence_at_a_time() {
        let mut engine = Engine::default();
        let values = engine
            .execute(b"local value = {}; return value, value")
            .unwrap();
        let table = match values[0] {
            Value::Table(table) => table,
            ref value => panic!("expected table, got {value:?}"),
        };
        assert_eq!(engine.vm().retained_value_count(), 2);

        assert!(engine.vm_mut().release_value(&values[0]));
        engine.vm_mut().collect(std::iter::empty()).unwrap();
        assert_eq!(
            engine.vm().heap().table_length(table),
            Ok(0),
            "the second returned occurrence must remain rooted"
        );

        assert!(engine.vm_mut().release_value(&values[1]));
        engine.vm_mut().collect(std::iter::empty()).unwrap();
        assert!(matches!(
            engine.vm().heap().table_length(table),
            Err(blu_runtime::HeapError::StaleTable(stale)) if stale == table
        ));
    }

    #[test]
    fn heap_accessor_values_can_be_explicitly_retained() {
        let vm = Vm::try_new_with_memory(
            Dialect::Blu,
            blu_runtime::MemoryConfig {
                hard_limit_bytes: None,
                gc_start_bytes: 0,
                gc_growth_percent: 0,
                max_single_allocation_bytes: usize::MAX,
            },
        )
        .unwrap();
        let mut engine = Engine::new(Compiler::default(), vm);
        engine
            .execute(b"accessor_holder = { child = { answer = 42 } }")
            .unwrap();
        let holder = match engine.vm().global(b"accessor_holder") {
            Some(Value::Table(table)) => *table,
            value => panic!("expected table global, got {value:?}"),
        };
        let child = engine
            .vm()
            .heap()
            .table_get(holder, &Value::String(Arc::from(&b"child"[..])))
            .unwrap();
        let child_table = match child {
            Value::Table(table) => table,
            ref value => panic!("expected child table, got {value:?}"),
        };
        assert_eq!(engine.vm_mut().retain_value(&child), Ok(true));
        engine
            .vm_mut()
            .set_global(&b"accessor_holder"[..], Value::Nil);

        assert_eq!(
            engine.execute(b"local garbage = {}; return 1"),
            Ok(vec![Value::Number(1.0)])
        );
        assert_eq!(
            engine
                .vm()
                .heap()
                .table_get(child_table, &Value::String(Arc::from(&b"answer"[..])),),
            Ok(Value::Number(42.0))
        );

        assert!(engine.vm_mut().release_value(&child));
        engine.vm_mut().collect(std::iter::empty()).unwrap();
        assert!(matches!(
            engine.vm().heap().table_length(child_table),
            Err(blu_runtime::HeapError::StaleTable(stale)) if stale == child_table
        ));
    }

    #[test]
    fn raised_heap_values_remain_rooted_until_explicit_release() {
        let vm = Vm::try_new_with_memory(
            Dialect::Blu,
            blu_runtime::MemoryConfig {
                hard_limit_bytes: None,
                gc_start_bytes: 0,
                gc_growth_percent: 0,
                max_single_allocation_bytes: usize::MAX,
            },
        )
        .unwrap();
        let mut engine = Engine::new(Compiler::default(), vm);
        let raised = match engine.execute(b"error({ answer = 42 })") {
            Err(ExecuteError::Runtime(RuntimeError::Raised(value))) => value,
            result => panic!("expected raised table, got {result:?}"),
        };
        let Value::Table(table) = raised else {
            panic!("expected raised table, got {raised:?}");
        };
        assert_eq!(engine.vm().retained_value_count(), 1);

        assert_eq!(
            engine.execute(
                br#"
                    for index = 1, 100 do
                        local garbage = { index, index + 1, index + 2 }
                    end
                    return "collected"
                "#
            ),
            Ok(vec![Value::String(Arc::from(&b"collected"[..]))])
        );
        assert_eq!(
            engine
                .vm()
                .heap()
                .table_get(table, &Value::String(Arc::from(&b"answer"[..]))),
            Ok(Value::Number(42.0))
        );

        assert!(engine.vm_mut().release_value(&Value::Table(table)));
        engine.vm_mut().collect(std::iter::empty()).unwrap();
        assert_eq!(
            engine.vm().heap().table_get(table, &Value::Integer(1)),
            Err(blu_runtime::HeapError::StaleTable(table))
        );
    }

    #[test]
    fn suspended_explicit_frames_remain_gc_roots() {
        let mut engine = Engine::default();
        let collect = engine.vm_mut().register_function(|vm, _| {
            vm.collect(std::iter::empty())?;
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

    #[test]
    fn coroutine_threads_complete_and_report_lifecycle() {
        assert_eq!(
            Engine::default().execute(
                br#"
                    local thread = coroutine.create(function(left, right)
                        return left + right, left * right
                    end)
                    local before = coroutine.status(thread)
                    local ok, sum, product = coroutine.resume(thread, 6, 7)
                    return before, ok, sum, product,
                        coroutine.status(thread), type(thread)
                "#
            ),
            Ok(vec![
                Value::String(Arc::from(&b"suspended"[..])),
                Value::Boolean(true),
                Value::Number(13.0),
                Value::Number(42.0),
                Value::String(Arc::from(&b"dead"[..])),
                Value::String(Arc::from(&b"thread"[..])),
            ])
        );
    }

    #[test]
    fn thread_roots_survive_gc_and_nested_yields_resume() {
        let mut engine = Engine::default();
        assert_eq!(
            engine.execute(
                br#"
                    local retained = { answer = 42 }
                    saved_thread = coroutine.create(function()
                        return retained.answer
                    end)
                "#
            ),
            Ok(Vec::new())
        );
        engine.vm_mut().collect(std::iter::empty()).unwrap();
        assert_eq!(
            engine.execute(b"return coroutine.resume(saved_thread)"),
            Ok(vec![Value::Boolean(true), Value::Number(42.0)])
        );
        assert_eq!(
            engine.execute(
                br#"
                    local thread = coroutine.create(function()
                        local function nested()
                            return coroutine.yield("pause")
                        end
                        return nested() + 1
                    end)
                    local first_ok, paused = coroutine.resume(thread)
                    local suspended = coroutine.status(thread)
                    local second_ok, result = coroutine.resume(thread, 41)
                    return first_ok, paused, suspended,
                        second_ok, result, coroutine.status(thread)
                "#
            ),
            Ok(vec![
                Value::Boolean(true),
                Value::String(Arc::from(&b"pause"[..])),
                Value::String(Arc::from(&b"suspended"[..])),
                Value::Boolean(true),
                Value::Number(42.0),
                Value::String(Arc::from(&b"dead"[..])),
            ])
        );
    }

    #[test]
    fn generic_for_steps_resume_without_restarting_and_trace_pending_values() {
        let mut engine = Engine::default();
        assert_eq!(
            engine.execute(
                br#"
                    local calls = 0
                    local state = { retained = 40 }
                    local function iterator(current_state, control)
                        calls += 1
                        if control >= 2 then
                            return nil
                        end
                        local resumed = coroutine.yield("step" .. calls)
                        return control + 1, current_state.retained + resumed
                    end
                    saved_thread = coroutine.create(function()
                        local total = 0
                        for key, value in iterator, state, 0 do
                            total += key + value
                        end
                        return calls, total
                    end)
                    return coroutine.resume(saved_thread)
                "#
            ),
            Ok(vec![
                Value::Boolean(true),
                Value::String(Arc::from(&b"step1"[..])),
            ])
        );
        engine.vm_mut().collect(std::iter::empty()).unwrap();
        assert_eq!(
            engine.execute(
                br#"
                    local second_ok, second = coroutine.resume(saved_thread, 1)
                    local third_ok, count, total = coroutine.resume(saved_thread, 2)
                    return second_ok, second, third_ok, count, total,
                        coroutine.status(saved_thread)
                "#
            ),
            Ok(vec![
                Value::Boolean(true),
                Value::String(Arc::from(&b"step2"[..])),
                Value::Boolean(true),
                Value::Number(3.0),
                Value::Number(86.0),
                Value::String(Arc::from(&b"dead"[..])),
            ]),
        );
    }

    #[test]
    fn generic_for_errors_after_resume_reach_the_protected_boundary() {
        assert_eq!(
            Engine::default().execute(
                br#"
                    local calls = 0
                    local function iterator()
                        calls += 1
                        coroutine.yield("iteration pause")
                        error("iteration boom")
                    end
                    local thread = coroutine.create(function()
                        local ok, message = pcall(function()
                            for _ in iterator do
                            end
                        end)
                        return ok, message, calls
                    end)
                    local first_ok, pause = coroutine.resume(thread)
                    local second_ok, protected_ok, message, count =
                        coroutine.resume(thread)
                    return first_ok, pause, second_ok, protected_ok,
                        message, count, coroutine.status(thread)
                "#
            ),
            Ok(vec![
                Value::Boolean(true),
                Value::String(Arc::from(&b"iteration pause"[..])),
                Value::Boolean(true),
                Value::Boolean(false),
                Value::String(Arc::from(&b"iteration boom"[..])),
                Value::Number(1.0),
                Value::String(Arc::from(&b"dead"[..])),
            ])
        );
    }

    #[test]
    fn protected_calls_can_yield_and_resume_successfully() {
        assert_eq!(
            Engine::default().execute(
                br#"
                    local thread = coroutine.create(function()
                        local ok, value = pcall(function()
                            local function nested()
                                return coroutine.yield("pause")
                            end
                            return nested() + 1
                        end)
                        return ok, value
                    end)
                    local first_ok, paused = coroutine.resume(thread)
                    local second_ok, protected_ok, value =
                        coroutine.resume(thread, 41)
                    return first_ok, paused, second_ok, protected_ok, value
                "#
            ),
            Ok(vec![
                Value::Boolean(true),
                Value::String(Arc::from(&b"pause"[..])),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Number(42.0),
            ])
        );
    }

    #[test]
    fn resumed_coroutine_errors_unwind_to_suspended_protected_calls() {
        assert_eq!(
            Engine::default().execute(
                br#"
                    local pcall_thread = coroutine.create(function()
                        local ok, message = pcall(function()
                            coroutine.yield("pcall pause")
                            error("pcall boom")
                        end)
                        return ok, message, coroutine.status(coroutine.running())
                    end)
                    local p1, ppaused = coroutine.resume(pcall_thread)
                    local p2, protected_ok, pmessage, inside_status =
                        coroutine.resume(pcall_thread)

                    local xpcall_thread = coroutine.create(function()
                        local ok, message = xpcall(function()
                            coroutine.yield("xpcall pause")
                            error("xpcall boom")
                        end, function(error_value)
                            return "handled: " .. error_value
                        end)
                        return ok, message
                    end)
                    local x1, xpaused = coroutine.resume(xpcall_thread)
                    local x2, handled_ok, xmessage = coroutine.resume(xpcall_thread)

                    return p1, ppaused, p2, protected_ok, pmessage, inside_status,
                        coroutine.status(pcall_thread),
                        x1, xpaused, x2, handled_ok, xmessage,
                        coroutine.status(xpcall_thread)
                "#
            ),
            Ok(vec![
                Value::Boolean(true),
                Value::String(Arc::from(&b"pcall pause"[..])),
                Value::Boolean(true),
                Value::Boolean(false),
                Value::String(Arc::from(&b"pcall boom"[..])),
                Value::String(Arc::from(&b"running"[..])),
                Value::String(Arc::from(&b"dead"[..])),
                Value::Boolean(true),
                Value::String(Arc::from(&b"xpcall pause"[..])),
                Value::Boolean(true),
                Value::Boolean(false),
                Value::String(Arc::from(&b"handled: xpcall boom"[..])),
                Value::String(Arc::from(&b"dead"[..])),
            ])
        );
    }

    #[test]
    fn resumed_xpcall_handlers_can_yield_and_nested_protection_stays_intact() {
        assert_eq!(
            Engine::default().execute(
                br#"
                    local yielding_handler = coroutine.create(function()
                        return xpcall(function()
                            coroutine.yield("target pause")
                            error("target boom")
                        end, function(error_value)
                            local resumed = coroutine.yield("handling " .. error_value)
                            return "handled " .. resumed
                        end)
                    end)
                    local first_ok, first = coroutine.resume(yielding_handler)
                    local second_ok, second = coroutine.resume(yielding_handler)
                    local third_ok, protected_ok, handled =
                        coroutine.resume(yielding_handler, "done")

                    local nested = coroutine.create(function()
                        return pcall(function()
                            local ok, message = xpcall(function()
                                coroutine.yield("nested pause")
                                error("nested target")
                            end, function()
                                error("handler boom")
                            end)
                            return ok, message
                        end)
                    end)
                    local nested_first_ok, nested_pause = coroutine.resume(nested)
                    local nested_second_ok, outer_ok, inner_ok, nested_message =
                        coroutine.resume(nested)

                    return first_ok, first, second_ok, second,
                        third_ok, protected_ok, handled,
                        coroutine.status(yielding_handler),
                        nested_first_ok, nested_pause, nested_second_ok,
                        outer_ok, inner_ok, nested_message, coroutine.status(nested)
                "#
            ),
            Ok(vec![
                Value::Boolean(true),
                Value::String(Arc::from(&b"target pause"[..])),
                Value::Boolean(true),
                Value::String(Arc::from(&b"handling target boom"[..])),
                Value::Boolean(true),
                Value::Boolean(false),
                Value::String(Arc::from(&b"handled done"[..])),
                Value::String(Arc::from(&b"dead"[..])),
                Value::Boolean(true),
                Value::String(Arc::from(&b"nested pause"[..])),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(false),
                Value::String(Arc::from(&b"handler boom"[..])),
                Value::String(Arc::from(&b"dead"[..])),
            ])
        );
    }

    #[test]
    fn yielding_error_handlers_preserve_outer_saved_callers() {
        assert_eq!(
            Engine::default().execute(
                br#"
                    local thread = coroutine.create(function()
                        local function protected()
                            return xpcall(function()
                                coroutine.yield("target pause")
                                error("target boom")
                            end, function(message)
                                local suffix = coroutine.yield("handler pause: " .. message)
                                return "handled " .. suffix
                            end)
                        end
                        local function outer()
                            return "outer", protected()
                        end
                        return outer()
                    end)
                    local first_ok, first = coroutine.resume(thread)
                    local second_ok, second = coroutine.resume(thread)
                    local third_ok, outer, protected_ok, handled =
                        coroutine.resume(thread, "done")
                    return first_ok, first, second_ok, second,
                        third_ok, outer, protected_ok, handled
                "#
            ),
            Ok(vec![
                Value::Boolean(true),
                Value::String(Arc::from(&b"target pause"[..])),
                Value::Boolean(true),
                Value::String(Arc::from(&b"handler pause: target boom"[..])),
                Value::Boolean(true),
                Value::String(Arc::from(&b"outer"[..])),
                Value::Boolean(false),
                Value::String(Arc::from(&b"handled done"[..])),
            ])
        );
    }

    #[test]
    fn resumed_protected_errors_unwind_the_explicit_caller_stack() {
        let mut engine = Engine::default();
        *engine.vm_mut() = Vm::default().with_call_limit(10_000);
        assert_eq!(
            engine.execute(
                br#"
                    local thread = coroutine.create(function()
                        return pcall(function()
                            local function descend(remaining)
                                if remaining == 0 then
                                    coroutine.yield("deep pause")
                                    error("deep boom")
                                end
                                return descend(remaining - 1)
                            end
                            return descend(3_000)
                        end)
                    end)
                    local first_ok, paused = coroutine.resume(thread)
                    local second_ok, protected_ok, message = coroutine.resume(thread)
                    return first_ok, paused, second_ok, protected_ok, message,
                        coroutine.status(thread)
                "#
            ),
            Ok(vec![
                Value::Boolean(true),
                Value::String(Arc::from(&b"deep pause"[..])),
                Value::Boolean(true),
                Value::Boolean(false),
                Value::String(Arc::from(&b"deep boom"[..])),
                Value::String(Arc::from(&b"dead"[..])),
            ])
        );
    }

    #[test]
    fn coroutine_wrap_is_callable_gc_traced_and_propagates_errors() {
        let mut engine = Engine::default();
        assert_eq!(
            engine.execute(
                br#"
                    wrapped = coroutine.wrap(function(first)
                        local resumed = coroutine.yield(first + 1)
                        return resumed + 1
                    end)
                    return wrapped(4)
                "#
            ),
            Ok(vec![Value::Number(5.0)])
        );
        engine.vm_mut().collect(std::iter::empty()).unwrap();
        assert_eq!(
            engine.execute(b"return wrapped(9)"),
            Ok(vec![Value::Number(10.0)])
        );
        assert_eq!(
            engine.execute(
                br#"
                    local failing = coroutine.wrap(function()
                        error("boom")
                    end)
                    local ok, message = pcall(failing)
                    return ok, type(message)
                "#
            ),
            Ok(vec![
                Value::Boolean(false),
                Value::String(Arc::from(&b"string"[..])),
            ])
        );
    }

    #[test]
    fn coroutine_main_thread_and_close_follow_explicit_profiles() {
        assert_eq!(
            Engine::default().execute(
                b"local thread, is_main = coroutine.running()\n\
                  return type(thread), is_main, coroutine.isyieldable()"
            ),
            Ok(vec![
                Value::String(Arc::from(&b"thread"[..])),
                Value::Boolean(true),
                Value::Boolean(false),
            ])
        );
        assert_eq!(
            Engine::for_dialect(Dialect::Luau).execute(
                b"return select('#', coroutine.running()), \
                  type(coroutine.running()), coroutine.isyieldable()"
            ),
            Ok(vec![
                Value::Number(1.0),
                Value::String(Arc::from(&b"thread"[..])),
                Value::Boolean(true),
            ])
        );
        assert_eq!(
            Engine::default().execute(
                br#"
                    local fresh = coroutine.create(function() end)
                    local fresh_closed = coroutine.close(fresh)

                    local paused = coroutine.create(function()
                        coroutine.yield()
                    end)
                    coroutine.resume(paused)
                    local paused_closed = coroutine.close(paused)

                    local failed = coroutine.create(function()
                        error("boom")
                    end)
                    coroutine.resume(failed)
                    local failed_closed, failure = coroutine.close(failed)

                    return fresh_closed, coroutine.status(fresh),
                        paused_closed, coroutine.status(paused),
                        failed_closed, type(failure)
                "#
            ),
            Ok(vec![
                Value::Boolean(true),
                Value::String(Arc::from(&b"dead"[..])),
                Value::Boolean(true),
                Value::String(Arc::from(&b"dead"[..])),
                Value::Boolean(false),
                Value::String(Arc::from(&b"string"[..])),
            ])
        );
    }
}
