#![forbid(unsafe_code)]

//! Public embedding interface for Blu.
//!
//! Most applications should depend only on `blu-lang`. Lower-level bytecode and
//! runtime crates remain public for tooling and specialized embedders.

pub use blu_bytecode as bytecode;
pub use blu_compiler::{
    CompileError, CompileOptions, CompiledBytecode, Compiler, LUAU_COMPILER_RELEASE,
};
pub use blu_core::SemanticProfile;
pub use blu_package as package;
pub use blu_runtime::{
    CalendarDate, CalendarDateInput, Dialect, InterruptHandle, IoBufferMode, IoFile, IoReadRequest,
    IoSeekWhence, IoStreamKind, LoadReaderCompletion, NativeLibraryFailure,
    NativeLibraryLoadResult, OsExecuteResult, OsExitRequest, RuntimeError, TableId, Value, Vm,
};

use blu_core::{
    CompilerId, CompilerIdentity, Diagnostic, IdentityError, IdentityLimits, SourceError,
    SourceFile, SourceId, SourceLimits,
};

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

use blu_package::{AuthorityProfile, CapabilityRequirement, Package, PackageDialect};
use core::fmt;
use std::sync::Arc;

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

    /// Returns a thread-safe handle for cooperatively interrupting execution.
    #[must_use]
    pub fn interrupt_handle(&self) -> InterruptHandle {
        self.vm.interrupt_handle()
    }

    /// Replaces or clears the absolute wall-clock execution deadline.
    pub fn set_deadline(&mut self, deadline: Option<std::time::Instant>) {
        self.vm.set_deadline(deadline);
    }

    /// Returns the currently configured absolute execution deadline.
    #[must_use]
    pub const fn deadline(&self) -> Option<std::time::Instant> {
        self.vm.deadline()
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
        self.vm.execute_blu_v1_with_luau_errors(
            compilation.into_validated_artifact(),
            execution_limits,
        )
    }

    /// Compiles and executes source through the explicitly selected Blu-owned
    /// frontend. This is the public source entry point for the experimental
    /// profile-aware frontend slice, including Lua 5.1–5.5.
    ///
    /// The established [`Self::execute`] path remains on the legacy Luau
    /// compiler for compatibility. Callers using this method opt into the
    /// owned frontend and receive structured source, compilation, or runtime
    /// errors rather than an implicit compiler fallback.
    pub fn execute_owned_source(
        &mut self,
        source: impl AsRef<[u8]>,
        profile: SemanticProfile,
    ) -> Result<Vec<Value>, OwnedExecuteError> {
        self.execute_owned_source_named(source, "source.blu", profile)
    }

    /// Compiles and executes owned source with an explicit chunk name.
    ///
    /// The name is retained in debug metadata and runtime error locations,
    /// which lets embedders preserve the identity of a source file while
    /// still using the bounded owned frontend.
    pub fn execute_owned_source_named(
        &mut self,
        source: impl AsRef<[u8]>,
        chunk_name: impl Into<String>,
        profile: SemanticProfile,
    ) -> Result<Vec<Value>, OwnedExecuteError> {
        self.execute_owned_source_named_with_limits(
            source,
            chunk_name,
            profile,
            frontend::OwnedCompileLimits::default(),
            bytecode::blu::BluLimits::default(),
        )
    }

    /// Variant of [`Self::execute_owned_source`] with explicit compiler and
    /// artifact execution limits.
    pub fn execute_owned_source_with_limits(
        &mut self,
        source: impl AsRef<[u8]>,
        profile: SemanticProfile,
        compile_limits: frontend::OwnedCompileLimits,
        execution_limits: bytecode::blu::BluLimits,
    ) -> Result<Vec<Value>, OwnedExecuteError> {
        self.execute_owned_source_named_with_limits(
            source,
            "source.blu",
            profile,
            compile_limits,
            execution_limits,
        )
    }

    /// Variant of [`Self::execute_owned_source_named`] with explicit
    /// compiler and artifact execution limits.
    pub fn execute_owned_source_named_with_limits(
        &mut self,
        source: impl AsRef<[u8]>,
        chunk_name: impl Into<String>,
        profile: SemanticProfile,
        compile_limits: frontend::OwnedCompileLimits,
        execution_limits: bytecode::blu::BluLimits,
    ) -> Result<Vec<Value>, OwnedExecuteError> {
        self.install_owned_load(compile_limits, execution_limits)
            .map_err(OwnedExecuteError::Runtime)?;
        let compilation =
            compile_owned_source(source.as_ref(), chunk_name, profile, compile_limits)?;
        let result = self.execute_owned_compilation(compilation, execution_limits);
        result.map_err(OwnedExecuteError::Runtime)
    }

    /// Compiles source into a callable owned chunk using the supplied Lua
    /// 5.2+ environment. The returned closure preserves that environment for
    /// every invocation, matching the observable state behavior of `load`.
    pub fn load_owned_source(
        &mut self,
        source: impl AsRef<[u8]>,
        profile: SemanticProfile,
        environment: TableId,
    ) -> Result<Value, OwnedExecuteError> {
        self.load_owned_source_with_limits(
            source,
            "=(load)",
            profile,
            environment,
            frontend::OwnedCompileLimits::default(),
            bytecode::blu::BluLimits::default(),
        )
    }

    /// Variant of [`Self::load_owned_source`] with explicit chunk name and
    /// compiler/runtime limits.
    pub fn load_owned_source_with_limits(
        &mut self,
        source: impl AsRef<[u8]>,
        chunk_name: impl Into<String>,
        profile: SemanticProfile,
        environment: TableId,
        compile_limits: frontend::OwnedCompileLimits,
        execution_limits: bytecode::blu::BluLimits,
    ) -> Result<Value, OwnedExecuteError> {
        let compilation =
            compile_owned_source(source.as_ref(), chunk_name, profile, compile_limits)?;
        self.vm
            .create_blu_v1_closure_in_environment(
                compilation.into_validated_artifact(),
                execution_limits,
                environment,
            )
            .map_err(OwnedExecuteError::Runtime)
    }

    fn install_owned_load(
        &mut self,
        compile_limits: frontend::OwnedCompileLimits,
        execution_limits: bytecode::blu::BluLimits,
    ) -> Result<(), RuntimeError> {
        let source_module_compile_limits = compile_limits;
        let source_module_execution_limits = execution_limits;
        let load = self.vm.try_register_function(move |vm, arguments| {
            let profile = vm.active_semantic_profile()?;
            let source_value = arguments.first().ok_or(RuntimeError::Argument {
                function: "load",
                index: 1,
            })?;
            let loadstring_call = matches!(arguments.get(4), Some(Value::Boolean(true)));
            let implicit_loadstring_name =
                loadstring_call && matches!(arguments.get(1), None | Some(Value::Nil));
            if profile == SemanticProfile::Lua51
                && !loadstring_call
                && matches!(source_value, Value::String(_))
            {
                return Err(RuntimeError::Type {
                    operation: "load",
                    expected: "function",
                    actual: "string",
                });
            }
            let (source, reader) = match source_value {
                Value::String(source) => (Some(source.to_vec()), None),
                Value::Closure(_) | Value::NativeFunction(_) => (None, Some(source_value.clone())),
                value => {
                    return Err(RuntimeError::Type {
                        operation: "load",
                        expected: "string or function",
                        actual: value.type_name(),
                    });
                }
            };
            let mode = if profile == SemanticProfile::Lua51 {
                b"bt".to_vec()
            } else {
                match arguments.get(2) {
                    None | Some(Value::Nil) => b"bt".to_vec(),
                    Some(Value::String(mode)) => mode.to_vec(),
                    Some(value) => {
                        return Err(RuntimeError::Type {
                            operation: "load",
                            expected: "string",
                            actual: value.type_name(),
                        });
                    }
                }
            };
            let chunk_name = match arguments.get(1) {
                None | Some(Value::Nil) if is_lua_profile(profile) => {
                    source.as_deref().map_or_else(
                        || "=(load)".to_owned(),
                        |source| {
                            // The source text is Lua's implicit chunk name,
                            // but Blu's artifact identity deliberately
                            // rejects embedded NULs. Keep the load itself
                            // usable for valid source strings containing a
                            // NUL (for example, a quoted `"a\\0b"` literal)
                            // and fall back to the conventional load name
                            // when that name cannot be represented.
                            if source.contains(&0) {
                                "=(load)".to_owned()
                            } else {
                                String::from_utf8_lossy(source).into_owned()
                            }
                        },
                    )
                }
                None | Some(Value::Nil) if implicit_loadstring_name => {
                    source.as_deref().map_or_else(
                        || "=(load)".to_owned(),
                        |source| String::from_utf8_lossy(source).into_owned(),
                    )
                }
                None | Some(Value::Nil) => "=(load)".to_owned(),
                Some(Value::String(name)) if name.is_empty() && !is_lua_profile(profile) => {
                    "=(load)".to_owned()
                }
                Some(Value::String(name)) if name.is_empty() => String::new(),
                Some(Value::String(name)) => String::from_utf8_lossy(name).into_owned(),
                Some(value) => {
                    return Err(RuntimeError::Type {
                        operation: "load",
                        expected: "string",
                        actual: value.type_name(),
                    });
                }
            };
            let environment = match arguments.get(3) {
                None | Some(Value::Nil)
                    if matches!(
                        profile,
                        SemanticProfile::Blu | SemanticProfile::Luau | SemanticProfile::Lua51
                    ) =>
                {
                    if matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
                        vm.current_blu_environment()?
                    } else {
                        vm.current_lua51_environment()?
                    }
                }
                None | Some(Value::Nil) => vm.default_environment()?,
                Some(Value::Table(environment)) => *environment,
                Some(value) => {
                    return Err(RuntimeError::Type {
                        operation: "load",
                        expected: "table",
                        actual: value.type_name(),
                    });
                }
            };
            let completion = LoadReaderCompletion::new(move |vm, source| {
                if source.starts_with(&bytecode::blu::MAGIC) {
                    if !mode.contains(&b'b') {
                        let mode = String::from_utf8_lossy(&mode);
                        return Ok(vec![
                            Value::Nil,
                            Value::String(Arc::from(
                                format!("attempt to load a binary chunk (mode is '{mode}')")
                                    .as_bytes(),
                            )),
                        ]);
                    }
                    let artifact =
                        match bytecode::blu::decode_validated(source, compile_limits.artifact) {
                            Ok(artifact) => artifact,
                            Err(error) => {
                                return Ok(vec![
                                    Value::Nil,
                                    Value::String(Arc::from(error.to_string().as_bytes())),
                                ]);
                            }
                        };
                    let closure = match profile {
                        SemanticProfile::Blu | SemanticProfile::Luau => vm
                            .create_blu_v1_closure_in_environment(
                                artifact,
                                execution_limits,
                                environment,
                            )?,
                        SemanticProfile::Lua51
                        | SemanticProfile::Lua52
                        | SemanticProfile::Lua53
                        | SemanticProfile::Lua54
                        | SemanticProfile::Lua55 => vm.create_blu_v1_closure_in_environment(
                            artifact,
                            execution_limits,
                            environment,
                        )?,
                        _ => vm.create_blu_v1_closure_in_environment(
                            artifact,
                            execution_limits,
                            environment,
                        )?,
                    };
                    return Ok(vec![closure]);
                }
                if !mode.contains(&b't') {
                    let mode = String::from_utf8_lossy(&mode);
                    return Ok(vec![
                        Value::Nil,
                        Value::String(Arc::from(
                            format!("attempt to load a text chunk (mode is '{mode}')").as_bytes(),
                        )),
                    ]);
                }
                let compiler_chunk_name = if chunk_name.is_empty() && !is_lua_profile(profile) {
                    "=(load)".to_owned()
                } else {
                    chunk_name.clone()
                };
                let compiler_chunk_name_len = compiler_chunk_name.len();
                let compilation = match compile_owned_source(
                    source,
                    compiler_chunk_name,
                    profile,
                    compile_limits,
                ) {
                    Ok(compilation) => compilation,
                    Err(error) => {
                        let message = if is_lua_profile(profile) {
                            lua_load_compile_error(source, &chunk_name, &error)
                        } else if profile == SemanticProfile::Luau {
                            luau_load_compile_error(&chunk_name, implicit_loadstring_name, &error)
                        } else {
                            format!(
                                "[string \"{chunk_name}\"]:1: {error}",
                                chunk_name = chunk_name
                            )
                        };
                        return Ok(vec![
                            Value::Nil,
                            Value::String(Arc::from(message.into_bytes())),
                        ]);
                    }
                };
                let mut chunk_execution_limits = execution_limits;
                chunk_execution_limits.identity.max_source_name_bytes = chunk_execution_limits
                    .identity
                    .max_source_name_bytes
                    .max(compiler_chunk_name_len);
                let closure = match profile {
                    SemanticProfile::Blu | SemanticProfile::Luau => vm
                        .create_blu_v1_closure_in_environment(
                            compilation.into_validated_artifact(),
                            chunk_execution_limits,
                            environment,
                        )?,
                    SemanticProfile::Lua51
                    | SemanticProfile::Lua52
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55 => vm.create_blu_v1_closure_in_environment(
                        compilation.into_validated_artifact(),
                        chunk_execution_limits,
                        environment,
                    )?,
                    _ => vm.create_blu_v1_closure_in_environment(
                        compilation.into_validated_artifact(),
                        chunk_execution_limits,
                        environment,
                    )?,
                };
                Ok(vec![closure])
            });
            if let Some(reader) = reader {
                return vm.invoke_load_reader(
                    reader,
                    SourceLimits::default().max_bytes,
                    completion,
                    &[Value::Table(environment)],
                );
            }
            completion.complete(vm, &source.expect("string source is present"))
        })?;
        self.vm
            .try_set_global(&b"load"[..], Value::NativeFunction(load))?;
        let loadstring = self.vm.try_register_function(move |vm, arguments| {
            let source = arguments.first().ok_or(RuntimeError::Argument {
                function: "loadstring",
                index: 1,
            })?;
            if !matches!(source, Value::String(_)) {
                return Err(RuntimeError::Type {
                    operation: "loadstring",
                    expected: "string",
                    actual: source.type_name(),
                });
            }
            let mut forwarded = Vec::new();
            forwarded
                .try_reserve(arguments.len().saturating_add(5))
                .map_err(|_| RuntimeError::Allocation {
                    what: "loadstring arguments",
                })?;
            forwarded.extend_from_slice(arguments);
            while forwarded.len() < 4 {
                forwarded.push(Value::Nil);
            }
            forwarded.push(Value::Boolean(true));
            vm.invoke_native_callback_without_yield(
                "loadstring",
                "nested loadstring invocation",
                Value::NativeFunction(load),
                &forwarded,
            )
        })?;
        self.vm
            .try_set_global(&b"loadstring"[..], Value::NativeFunction(loadstring))?;
        let loadfile = self.vm.try_register_function(move |vm, arguments| {
            let profile = vm.active_semantic_profile()?;
            if profile == SemanticProfile::Luau {
                return Err(RuntimeError::UnsupportedSemanticProfile {
                    operation: "loadfile",
                    profile,
                });
            }
            let path = arguments.first().ok_or(RuntimeError::Argument {
                function: "loadfile",
                index: 1,
            })?;
            let path = match path {
                Value::String(path) => Arc::clone(path),
                value => {
                    return Err(RuntimeError::Type {
                        operation: "loadfile",
                        expected: "string",
                        actual: value.type_name(),
                    });
                }
            };
            let source = match vm.read_file_source(path.as_ref()) {
                Ok(source) => source,
                Err(error) => {
                    return Ok(vec![
                        Value::Nil,
                        Value::String(Arc::from(error.to_string().into_bytes())),
                    ]);
                }
            };
            let source = normalize_lua_file_source(source);
            if source.len() > SourceLimits::default().max_bytes {
                return Ok(vec![
                    Value::Nil,
                    Value::String(Arc::from(
                        format!(
                            "file source exceeds limit {}",
                            SourceLimits::default().max_bytes
                        )
                        .into_bytes(),
                    )),
                ]);
            }
            let mut forwarded = Vec::with_capacity(5);
            forwarded.push(Value::String(Arc::from(source)));
            let chunk_name = if is_lua_profile(profile) {
                let mut chunk_name = Vec::with_capacity(path.len().saturating_add(1));
                chunk_name.push(b'@');
                chunk_name.extend_from_slice(&path);
                Arc::from(chunk_name)
            } else {
                path
            };
            forwarded.push(Value::String(chunk_name));
            if profile == SemanticProfile::Lua51 {
                forwarded.extend([Value::Nil, Value::Nil, Value::Boolean(true)]);
            } else {
                let mode = match arguments.get(1) {
                    None | Some(Value::Nil) => Value::Nil,
                    Some(Value::String(mode)) => Value::String(Arc::clone(mode)),
                    Some(value) => {
                        return Err(RuntimeError::Type {
                            operation: "loadfile",
                            expected: "string",
                            actual: value.type_name(),
                        });
                    }
                };
                let environment = match arguments.get(2) {
                    None | Some(Value::Nil) => Value::Nil,
                    Some(Value::Table(environment)) => Value::Table(*environment),
                    Some(value) => {
                        return Err(RuntimeError::Type {
                            operation: "loadfile",
                            expected: "table",
                            actual: value.type_name(),
                        });
                    }
                };
                forwarded.extend([mode, environment]);
            }
            vm.invoke_native_callback_without_yield(
                "loadfile",
                "nested loadfile invocation",
                Value::NativeFunction(load),
                &forwarded,
            )
        })?;
        self.vm
            .try_set_global(&b"loadfile"[..], Value::NativeFunction(loadfile))?;
        let dofile = self.vm.try_register_function(move |vm, arguments| {
            let loaded = vm.invoke_native_callback_without_yield(
                "dofile",
                "nested dofile loadfile invocation",
                Value::NativeFunction(loadfile),
                arguments,
            )?;
            let function = loaded.first().cloned().unwrap_or(Value::Nil);
            if matches!(function, Value::Nil) {
                return Err(RuntimeError::Raised(loaded.get(1).cloned().unwrap_or_else(
                    || Value::String(Arc::from(&b"loadfile failed"[..])),
                )));
            }
            vm.invoke_native_callback_without_yield(
                "dofile",
                "yielding loaded files",
                function,
                &[],
            )
        })?;
        self.vm
            .try_set_global(&b"dofile"[..], Value::NativeFunction(dofile))?;
        if !self.vm.has_module_loader() && self.vm.has_file_loader() && self.vm.has_file_probe() {
            self.vm.set_module_loader(move |vm, name| {
                let profile = vm.active_semantic_profile()?;
                if !matches!(
                    profile,
                    SemanticProfile::Lua51
                        | SemanticProfile::Lua52
                        | SemanticProfile::Lua53
                        | SemanticProfile::Lua54
                        | SemanticProfile::Lua55
                ) {
                    return Ok(Value::Nil);
                }
                let (source_path, source_error) = vm.find_package_source_path_with_error(name)?;
                let Some(path) = source_path else {
                    let cpath_error = vm
                        .package_cpath()?
                        .map(|path| vm.find_package_path_with_error(name, &path))
                        .transpose()?
                        .map_or_else(Vec::new, |(_, error)| error);
                    let mut message = format!(
                        "module '{}' not found:\n\tno field package.preload['{}']",
                        String::from_utf8_lossy(name),
                        String::from_utf8_lossy(name)
                    )
                    .into_bytes();
                    for error in [&source_error, &cpath_error] {
                        if error.is_empty() {
                            continue;
                        }
                        if !error.starts_with(b"\n\t") {
                            message.extend_from_slice(b"\n\t");
                        }
                        message.extend_from_slice(error);
                    }
                    return Err(RuntimeError::Raised(Value::String(Arc::from(message))));
                };
                let source = normalize_lua_file_source(vm.read_file_source(&path)?);
                if source.len() > frontend::SourceLimits::default().max_bytes {
                    return Err(RuntimeError::Raised(Value::String(Arc::from(
                        format!(
                            "module source exceeds limit {}",
                            frontend::SourceLimits::default().max_bytes
                        )
                        .into_bytes(),
                    ))));
                }
                let chunk_name = if is_lua_profile(profile) {
                    format!("@{}", String::from_utf8_lossy(&path))
                } else {
                    String::from_utf8_lossy(&path).into_owned()
                };
                let compilation = compile_owned_source(
                    &source,
                    chunk_name,
                    profile,
                    source_module_compile_limits,
                )
                .map_err(|error| {
                    RuntimeError::Raised(Value::String(Arc::from(error.to_string().into_bytes())))
                })?;
                let environment = if profile == SemanticProfile::Lua51 {
                    vm.current_lua51_environment()?
                } else {
                    vm.default_environment()?
                };
                let closure = vm.create_blu_v1_closure_in_environment(
                    compilation.into_validated_artifact(),
                    source_module_execution_limits,
                    environment,
                )?;
                let values = vm.invoke_native_callback_without_yield(
                    "require",
                    "yielding source module loaders",
                    closure,
                    &[Value::String(Arc::from(name))],
                )?;
                Ok(values.into_iter().next().unwrap_or(Value::Boolean(true)))
            });
        }
        Ok(())
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
        for requirement in &package.manifest().authority.capabilities {
            if !policy
                .capabilities
                .iter()
                .any(|grant| grant.name == requirement.name && grant.scope == requirement.scope)
            {
                return Err(ExecutePackageError::CapabilityNotGranted(
                    requirement.clone(),
                ));
            }
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

fn is_lua_profile(profile: SemanticProfile) -> bool {
    matches!(
        profile,
        SemanticProfile::Lua51
            | SemanticProfile::Lua52
            | SemanticProfile::Lua53
            | SemanticProfile::Lua54
            | SemanticProfile::Lua55
    )
}

fn normalize_lua_file_source(source: Vec<u8>) -> Vec<u8> {
    if source.first() != Some(&b'#') {
        return source;
    }
    let mut normalized = Vec::with_capacity(source.len().saturating_add(1));
    normalized.extend_from_slice(b"--");
    normalized.extend_from_slice(&source[1..]);
    normalized
}

fn lua_load_compile_error(source: &[u8], chunk_name: &str, error: &OwnedExecuteError) -> String {
    let Some(error) = (match error {
        OwnedExecuteError::Compile(error) => Some(error.as_ref()),
        _ => None,
    }) else {
        return format!("[string \"{chunk_name}\"]:1: {error}");
    };
    let Some(diagnostic) = error
        .syntax()
        .and_then(|rejected| {
            rejected
                .diagnostics()
                .iter()
                .find(|diagnostic| {
                    matches!(
                        diagnostic.code().as_str(),
                        "BLU-LEX-0007" | "BLU-LEX-0017" | "BLU-LEX-0018"
                    )
                })
                .or_else(|| rejected.diagnostics().first())
        })
        .or_else(|| error.diagnostic())
    else {
        return format!("[string \"{chunk_name}\"]:1: {error}");
    };
    let line = source_line(
        source,
        usize::try_from(diagnostic.primary().span().start().get()).unwrap_or(usize::MAX),
    );
    let detail = lua_diagnostic_detail(source, diagnostic);
    format!("[string \"{chunk_name}\"]:{line}: {detail}")
}

fn luau_load_compile_error(
    chunk_name: &str,
    implicit_loadstring_name: bool,
    error: &OwnedExecuteError,
) -> String {
    let detail = match error {
        OwnedExecuteError::Compile(error) => error
            .syntax()
            .and_then(|rejected| rejected.diagnostics().first())
            .map_or_else(
                || error.to_string(),
                |diagnostic| {
                    if diagnostic.code().as_str() == "BLU-PARSE-0006" {
                        "Incomplete statement: expected assignment or a function call".to_owned()
                    } else {
                        error.to_string()
                    }
                },
            ),
        _ => error.to_string(),
    };
    if chunk_name == "=(load)" || implicit_loadstring_name {
        if implicit_loadstring_name {
            return format!(
                "[string \"{}\"]:1: {detail}",
                escape_luau_chunk_name(chunk_name)
            );
        }
        return format!("[string \"{chunk_name}\"]:1: {detail}");
    }
    let display_name = if let Some(name) = chunk_name.strip_prefix('@') {
        if name.len() > 255 {
            format!("...{}", &name[name.len() - 252..])
        } else {
            name.to_owned()
        }
    } else if let Some(name) = chunk_name.strip_prefix('=') {
        name.chars().take(255).collect()
    } else {
        chunk_name.to_owned()
    };
    format!("{display_name}:1: {detail}")
}

fn escape_luau_chunk_name(name: &str) -> String {
    let mut escaped = String::with_capacity(name.len());
    for byte in name.bytes() {
        match byte {
            b'\\' => escaped.push_str("\\\\"),
            b'"' => escaped.push_str("\\\""),
            b'\n' => escaped.push_str("\\n"),
            b'\r' => escaped.push_str("\\r"),
            byte if byte.is_ascii_graphic() || byte == b' ' => escaped.push(byte as char),
            byte => escaped.push_str(&format!("\\x{byte:02x}")),
        }
    }
    escaped
}

fn lua_diagnostic_detail(source: &[u8], diagnostic: &Diagnostic) -> String {
    let code = diagnostic.code().as_str();
    let near = |kind| {
        lua_near_fragment(source, diagnostic, kind)
            .map(|fragment| String::from_utf8_lossy(&fragment).into_owned())
            .unwrap_or_else(|| {
                String::from_utf8_lossy(diagnostic.found().unwrap_or_default()).into_owned()
            })
    };
    match code {
        "BLU-LEX-0007" => format!("invalid escape sequence near '{}'", near("invalid")),
        "BLU-LEX-0017" => {
            let escape =
                usize::try_from(diagnostic.primary().span().start().get()).unwrap_or(usize::MAX);
            let label = if source.get(escape + 1) == Some(&b'x') {
                "hexadecimal digit expected"
            } else {
                "decimal escape too large"
            };
            format!("{label} near '{}'", near("byte"))
        }
        "BLU-LEX-0018" => {
            let escape =
                usize::try_from(diagnostic.primary().span().start().get()).unwrap_or(usize::MAX);
            let after = escape.saturating_add(2);
            let label = if source.get(after) != Some(&b'{') {
                "missing '{'"
            } else if source
                .get(after + 1..)
                .and_then(|bytes| bytes.iter().position(|byte| *byte == b'}'))
                .is_some_and(|end| end > 8)
            {
                "UTF-8 value too large"
            } else if source.get(after + 1) == Some(&b'}') {
                "hexadecimal digit expected"
            } else {
                "missing '}'"
            };
            format!("{label} near '{}'", near("unicode"))
        }
        "BLU-LEX-0008" => "unfinished string near <eof>".to_owned(),
        "BLU-LEX-0019" => {
            let start =
                usize::try_from(diagnostic.primary().span().start().get()).unwrap_or(usize::MAX);
            let starting_line = source_line(source, start);
            format!("unfinished long string (starting at line {starting_line}) near <eof>")
        }
        "BLU-LEX-0015" => "malformed number".to_owned(),
        "BLU-PARSE-0004" => "unexpected symbol near <eof>".to_owned(),
        "BLU-PARSE-0005" => "malformed number near '1p'".to_owned(),
        _ => diagnostic.primary().message().to_owned(),
    }
}

fn lua_near_fragment(source: &[u8], diagnostic: &Diagnostic, kind: &str) -> Option<Vec<u8>> {
    let offset = usize::try_from(diagnostic.primary().span().start().get()).unwrap_or(usize::MAX);
    let (start, quote) = quoted_literal_start(source, offset)?;
    let span_end = usize::try_from(diagnostic.primary().span().end().get()).unwrap_or(usize::MAX);
    let end = match kind {
        "invalid" => span_end.saturating_add(1).min(source.len()),
        "byte" => {
            if source.get(offset + 1) == Some(&b'x') {
                let mut end = offset.saturating_add(2);
                while end < source.len()
                    && end < offset.saturating_add(4)
                    && source[end].is_ascii_hexdigit()
                {
                    end += 1;
                }
                if end < source.len() && !matches!(source[end], b'\r' | b'\n') {
                    end += 1;
                }
                end
            } else {
                let mut end = span_end;
                while end < source.len() && !matches!(source[end], b'\r' | b'\n' | b'\'' | b'"') {
                    end += 1;
                }
                if source.get(end) == Some(&quote) {
                    end += 1;
                }
                end
            }
        }
        "unicode" => {
            let escape = offset;
            let after = escape.saturating_add(2);
            if source.get(after) == Some(&b'{') {
                let mut end = after + 1;
                let mut all_hex = true;
                while end < source.len()
                    && !matches!(source[end], b'\r' | b'\n' | b'}' | b'"' | b'\'')
                {
                    all_hex &= source[end].is_ascii_hexdigit();
                    end += 1;
                }
                if all_hex && end > after + 1 && source.get(end) == Some(&quote) {
                    end += 1;
                }
                end
            } else {
                after.saturating_add(usize::from(
                    source.get(after) == Some(&quote)
                        || source.get(after).is_some_and(u8::is_ascii_hexdigit),
                ))
            }
        }
        _ => span_end,
    };
    source.get(start..end).map(ToOwned::to_owned)
}

fn quoted_literal_start(source: &[u8], offset: usize) -> Option<(usize, u8)> {
    let mut open = None;
    let mut cursor = 0;
    while cursor <= offset && cursor < source.len() {
        if let Some((_, quote)) = open {
            if source[cursor] == b'\\' {
                cursor = cursor.saturating_add(2);
                continue;
            }
            if source[cursor] == quote {
                open = None;
            }
        } else if matches!(source[cursor], b'\'' | b'"') {
            open = Some((cursor, source[cursor]));
        }
        cursor += 1;
    }
    open
}

fn source_line(source: &[u8], offset: usize) -> usize {
    let mut line = 1;
    let mut cursor = 0;
    while cursor < offset.min(source.len()) {
        match source[cursor] {
            b'\r' => {
                line += 1;
                cursor += usize::from(source.get(cursor + 1) == Some(&b'\n'));
            }
            b'\n' => line += 1,
            _ => {}
        }
        cursor += 1;
    }
    line
}

fn compile_owned_source(
    source: &[u8],
    chunk_name: impl Into<String>,
    profile: SemanticProfile,
    mut compile_limits: frontend::OwnedCompileLimits,
) -> Result<frontend::OwnedCompilation, OwnedExecuteError> {
    let chunk_name = chunk_name.into();
    let mut source_limits = SourceLimits::default();
    source_limits.max_name_bytes = source_limits.max_name_bytes.max(chunk_name.len());
    compile_limits.artifact.identity.max_source_name_bytes = compile_limits
        .artifact
        .identity
        .max_source_name_bytes
        .max(chunk_name.len());
    let source = SourceFile::new(SourceId::new(1), chunk_name, source.to_vec(), source_limits)
        .map_err(OwnedExecuteError::Source)?;
    let compiler_identity = CompilerIdentity::new(
        CompilerId::new(*b"blu-owned-v1\0\0\0\0"),
        "blu-owned",
        env!("CARGO_PKG_VERSION"),
        None,
        IdentityLimits::default(),
    )
    .map_err(OwnedExecuteError::Identity)?;
    frontend::OwnedCompiler::new(compile_limits)
        .compile(&source, profile, compiler_identity)
        .map_err(|error| OwnedExecuteError::Compile(Box::new(error)))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostPolicy {
    pub authority: AuthorityProfile,
    /// Exact capability grants available to packages executed under this policy.
    ///
    /// A grant never widens a package declaration: its name and scope must both
    /// exactly match a manifest requirement. Capability handles, delegation,
    /// revocation, and service linking remain outside this first policy gate.
    pub capabilities: Vec<CapabilityGrant>,
}

impl Default for HostPolicy {
    fn default() -> Self {
        Self {
            authority: AuthorityProfile::Pure,
            capabilities: Vec::new(),
        }
    }
}

impl HostPolicy {
    #[must_use]
    pub fn new(authority: AuthorityProfile) -> Self {
        Self {
            authority,
            capabilities: Vec::new(),
        }
    }

    /// Adds exact capability grants to this host policy.
    #[must_use]
    pub fn with_capabilities(
        mut self,
        capabilities: impl IntoIterator<Item = CapabilityGrant>,
    ) -> Self {
        self.capabilities.extend(capabilities);
        self
    }
}

/// A host-side capability grant.
///
/// Capability scopes are opaque to Blu. Matching is deliberately exact until
/// each capability family defines its own safe attenuation and containment
/// rules.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityGrant {
    pub name: blu_package::Name,
    pub scope: Vec<u8>,
}

impl CapabilityGrant {
    #[must_use]
    pub fn new(name: blu_package::Name, scope: impl Into<Vec<u8>>) -> Self {
        Self {
            name,
            scope: scope.into(),
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
    CapabilityNotGranted(CapabilityRequirement),
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
            Self::CapabilityNotGranted(requirement) => write!(
                f,
                "package requires ungranted capability {} with scope {:?}",
                requirement.name, requirement.scope
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

#[derive(Debug)]
pub enum OwnedExecuteError {
    Source(SourceError),
    Identity(IdentityError),
    Compile(Box<frontend::OwnedCompileError>),
    Runtime(RuntimeError),
}

impl fmt::Display for OwnedExecuteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(error) => write!(f, "owned source preparation failed: {error}"),
            Self::Identity(error) => write!(f, "owned compiler identity failed: {error}"),
            Self::Compile(error) => write!(f, "owned source compilation failed: {error}"),
            Self::Runtime(error) => write!(f, "owned source execution failed: {error}"),
        }
    }
}

impl std::error::Error for OwnedExecuteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Source(error) => Some(error),
            Self::Identity(error) => Some(error),
            Self::Compile(error) => Some(error.as_ref()),
            Self::Runtime(error) => Some(error),
        }
    }
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
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn owned_numeric_for_with_negative_start_terminates() {
        let mut engine = Engine::new(
            Compiler::default(),
            Vm::default().with_instruction_limit(10_000),
        );
        let result = engine
            .execute_owned_source(
                br#"
                    local count = 0
                    for index = -40, 40 do
                        count = count + 1
                    end
                    return count
                "#,
                SemanticProfile::Luau,
            )
            .expect("negative-start numeric loop should terminate");
        assert_eq!(result, [Value::Number(81.0)]);
    }

    #[test]
    fn owned_numeric_for_coerces_numeric_string_controls() {
        for profile in [
            SemanticProfile::Blu,
            SemanticProfile::Luau,
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let expected = if profile == SemanticProfile::Lua53 {
                [
                    Value::String(Arc::from(&b"9.0"[..])),
                    Value::String(Arc::from(&b"10"[..])),
                ]
            } else {
                [
                    Value::String(Arc::from(&b"9"[..])),
                    Value::String(Arc::from(&b"10"[..])),
                ]
            };
            let result = Engine::default()
                .execute_owned_source(
                    br#"
                        local count = 0
                        for index = "1", "5", "2" do
                            count = count + index
                        end
                        local precision = 0
                        for index = 0, 0.999999999, 0.1 do
                            precision = precision + 1
                        end
                        return tostring(count), tostring(precision)
                    "#,
                    profile,
                )
                .unwrap_or_else(|error| panic!("{profile:?}: {error}"));
            assert_eq!(result, expected, "{profile:?}");
        }
    }

    #[test]
    fn lua55_global_wildcard_preserves_nested_environment_captures() {
        let result = Engine::default()
            .execute_owned_source(
                br#"
                    global *
                    local function newobj(name)
                        _ENV[name] = true
                        return setmetatable({}, {
                            __close = function()
                                _ENV[name] = nil
                            end,
                        })
                    end
                    local observed
                    do
                        local resource <close> = newobj("X")
                        observed = X == true
                    end
                    return observed and X == nil
                "#,
                SemanticProfile::Lua55,
            )
            .expect("global wildcard should not shadow lexical _ENV captures");
        assert_eq!(result, [Value::Boolean(true)]);
    }

    #[test]
    fn lua54_load_reports_all_portable_goto_scope_diagnostics() {
        let cases = [
            ("goto l1; do ::l1:: end", "label 'l1'"),
            ("do ::l1:: end goto l1;", "label 'l1'"),
            ("::l1:: ::l1::", "label 'l1'"),
            ("::l1:: do ::l1:: end", "label 'l1'"),
            ("goto l1; local aa ::l1:: ::l2:: print(3)", "local 'aa'"),
            (
                "do local bb, cc; goto l1; end local aa ::l1:: print(3)",
                "local 'aa'",
            ),
            ("do ::l1:: end goto l1", "label 'l1'"),
            ("goto l1 do ::l1:: end", "label 'l1'"),
            (
                "repeat if x then goto cont end local xuxu = 10 ::cont:: until xuxu < x",
                "local 'xuxu'",
            ),
        ];
        for (source, expected) in cases {
            let probe = format!("local _, message = load({source:?}); return message");
            let result = Engine::default()
                .execute_owned_source(probe.as_bytes(), SemanticProfile::Lua54)
                .unwrap_or_else(|error| panic!("{source:?}: {error}"));
            let Some(Value::String(message)) = result.first() else {
                panic!("{source:?}: load unexpectedly succeeded with {result:?}");
            };
            assert!(
                String::from_utf8_lossy(message).contains(expected),
                "{source:?}: expected {expected:?} in {message:?}"
            );
        }
    }

    #[test]
    fn owned_luau_interpolated_strings_match_reference() {
        let source = br#"
            local name = "Blu"
            local nested = `Hello {`from {name}`}!`
            return `Welcome to {name}!`, nested, `Escaped \{brace\} \` \u{0041}\t`
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            let result = Engine::default()
                .execute_owned_source(source, profile)
                .unwrap_or_else(|error| panic!("{profile}: {error}"));
            assert_eq!(
                result,
                [
                    Value::String(Arc::from(&b"Welcome to Blu!"[..])),
                    Value::String(Arc::from(&b"Hello from Blu!"[..])),
                    Value::String(Arc::from(&b"Escaped {brace} ` A\t"[..])),
                ],
                "{profile}"
            );
        }
    }

    #[test]
    fn owned_generic_iteration_handles_callable_values_and_holes() {
        let result = Engine::default()
            .execute_owned_source(
                br#"
                    do
                        local a
                        for a, b in pairs{} do error("not here") end
                        for i = 1, 0 do error("not here") end
                        for i = 0, 1, -1 do error("not here") end
                        a = nil
                        for i = 1, 1 do a = 1 end
                        a = nil
                        for i = 1, 1, -1 do a = 1 end
                        a = 0
                        for i = 0, 1, 0.1 do a = a + 1 end
                        a = 0
                        for i = 0, 0.999999999, 0.1 do a = a + 1 end
                        a = 0
                        for i = 1, 1, 1 do a = a + 1 end
                        a = 0
                        for i = 1e10, 1e10, -1 do a = a + 1 end
                        a = 0
                        for i = 1, 0.99999, 1 do a = a + 1 end
                        a = 0
                        for i = 99999, 1e5, -1 do a = a + 1 end
                        a = 0
                        for i = 1, 0.99999, -1 do a = a + 1 end
                    end
                    local f = {}
                    setmetatable(f, {
                        __call = function(_, _, n)
                            if n > 0 then return n - 1 end
                        end,
                    })
                    local call_total = 0
                    for n in f, nil, 5 do call_total += n end
                    local userdata_iterator = newproxy(true)
                    getmetatable(userdata_iterator).__call = getmetatable(f).__call
                    local userdata_total = 0
                    for n in userdata_iterator, nil, 5 do userdata_total += n end
                    local pairs_total = 0
                    for k, value in pairs({a = 1, b = 2, c = 3}) do
                        pairs_total += value
                    end
                    local hole_total = 0
                    for k, value in pairs({1, 2, 3, nil, 5}) do
                        hole_total += value
                    end
                    local total = 0
                    for _, value in ipairs({1, 2, 3, nil, 5}) do
                        total += value
                    end
                    return call_total, userdata_total, pairs_total, hole_total, total
                "#,
                SemanticProfile::Luau,
            )
            .unwrap();
        assert_eq!(
            result,
            [
                Value::Number(10.0),
                Value::Number(10.0),
                Value::Number(6.0),
                Value::Number(11.0),
                Value::Number(6.0),
            ]
        );
    }

    #[test]
    fn owned_generic_for_non_iterable_is_caught_by_pcall() {
        let result = Engine::default()
            .execute_owned_source(
                br#"
                    local numeric_total = 0
                    for index = 0, 1, 0.1 do numeric_total += 1 end
                    for index = 0, 0.999999999, 0.1 do numeric_total += 1 end
                    local f = {}
                    setmetatable(f, {
                        __call = function(_, _, n)
                            if n > 0 then return n - 1 end
                        end,
                    })
                    local call_total = 0
                    for n in f, nil, 5 do call_total += n end
                    local userdata_iterator = newproxy(true)
                    getmetatable(userdata_iterator).__call = getmetatable(f).__call
                    local userdata_total = 0
                    for n in userdata_iterator, nil, 5 do userdata_total += n end
                    local ok, message = pcall(function()
                        for value in 42 do end
                    end)
                    return numeric_total, call_total, userdata_total, ok, message
                "#,
                SemanticProfile::Luau,
            )
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            result,
            [
                Value::Number(21.0),
                Value::Number(10.0),
                Value::Number(10.0),
                Value::Boolean(false),
                Value::String(Arc::from(&b"attempt to iterate over a number value"[..])),
            ]
        );
    }

    #[test]
    fn owned_generic_for_nil_iterators_report_call_errors() {
        let result = Engine::default()
            .execute_owned_source(
                br#"
                    local object = setmetatable({}, { __iter = function() end })
                    local ok, message = pcall(function()
                        for value in object do end
                    end)
                    return ok, message
                "#,
                SemanticProfile::Luau,
            )
            .unwrap();
        assert_eq!(
            result,
            [
                Value::Boolean(false),
                Value::String(Arc::from(&b"attempt to call a nil value"[..])),
            ]
        );
    }

    #[test]
    fn legacy_generic_for_nil_iterators_report_call_errors() {
        let mut engine = Engine::for_dialect(Dialect::Luau);
        assert_eq!(
            engine.execute(
                br#"
                    local object = setmetatable({}, { __iter = function() end })
                    local ok, message = pcall(function()
                        for value in object do end
                    end)
                    return ok, message
                "#,
            ),
            Ok(vec![
                Value::Boolean(false),
                Value::String(Arc::from(&b"attempt to call a nil value"[..])),
            ])
        );
    }

    #[test]
    fn string_gsub_function_replacements_use_the_first_callback_result() {
        let source = br#"
            local function replace(value, replacement)
                return string.gsub(value, ".", replacement)
            end
            local result = string.gsub("prefix |test|b|", "|([^|]*)|([^|]*)|", replace)
            return result
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile}: {error}")),
                vec![Value::String(Arc::from(&b"prefix bbbb"[..]))],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn owned_nested_pairs_and_negative_numeric_for_terminate() {
        let mut engine = Engine::new(
            Compiler::default(),
            Vm::default().with_instruction_limit(100_000),
        );
        let result = engine
            .execute_owned_source(
                br#"
                    local values = {0, 1, 2, 3, 10, 0x80000000, 0xaaaaaaaa, 0x55555555, 0xffffffff, 0x7fffffff}
                    local count = 0
                    for _, value in pairs(values) do
                        for index = -40, 40 do
                            count = count + 1
                            bit32.lshift(value, index)
                        end
                    end
                    return count
                "#,
                SemanticProfile::Luau,
            )
            .expect("nested official bitwise loops should terminate");
        assert_eq!(result, [Value::Number(810.0)]);
    }

    #[test]
    fn owned_official_bitwise_special_cases_terminate() {
        let mut engine = Engine::new(
            Compiler::default(),
            Vm::default().with_instruction_limit(100_000),
        );
        let result = engine
            .execute_owned_source(
                br#"
                    local c = {0, 1, 2, 3, 10, 0x80000000, 0xaaaaaaaa, 0x55555555,
                               0xffffffff, 0x7fffffff}
                    for _, b in pairs(c) do
                        assert(bit32.band(b) == b)
                        assert(bit32.band(b, b) == b)
                        assert(bit32.btest(b, b) == (b ~= 0))
                        assert(bit32.band(b, b, b) == b)
                        assert(bit32.btest(b, b, b) == (b ~= 0))
                        assert(bit32.band(b, bit32.bnot(b)) == 0)
                        assert(bit32.bor(b, bit32.bnot(b)) == bit32.bnot(0))
                        assert(bit32.bor(b) == b)
                        assert(bit32.bor(b, b) == b)
                        assert(bit32.bor(b, b, b) == b)
                        assert(bit32.bxor(b) == b)
                        assert(bit32.bxor(b, b) == 0)
                        assert(bit32.bxor(b, 0) == b)
                        assert(bit32.bxor(b, b, b) == b)
                        assert(bit32.bxor(b, b, b, b) == 0)
                        assert(bit32.bnot(b) ~= b)
                        assert(bit32.bnot(bit32.bnot(b)) == b)
                        assert(bit32.bnot(b) == 2^32 - 1 - b)
                        assert(bit32.lrotate(b, 32) == b)
                        assert(bit32.rrotate(b, 32) == b)
                        assert(bit32.lshift(bit32.lshift(b, -4), 4) == bit32.band(b, bit32.bnot(0xf)))
                        assert(bit32.rshift(bit32.rshift(b, 4), -4) == bit32.band(b, bit32.bnot(0xf)))
                        for i = -40, 40 do
                            assert(bit32.lshift(b, i) == math.floor((b * 2^i) % 2^32))
                        end
                    end
                    assert(not pcall(bit32.band, {}))
                    assert(not pcall(bit32.bnot, "a"))
                    assert(not pcall(bit32.lshift, 45))
                    assert(not pcall(bit32.lshift, 45, print))
                    assert(not pcall(bit32.rshift, 45, print))
                    return "OK"
                "#,
                SemanticProfile::Luau,
            )
            .expect("official bitwise special cases should terminate");
        assert_eq!(result, [Value::String(Arc::from(&b"OK"[..]))]);
    }

    #[test]
    fn luau_large_integer_global_table_mutation_terminates() {
        let mut engine = Engine::new(
            Compiler::default(),
            Vm::default().with_instruction_limit(500_000),
        );
        let result = engine
            .execute_owned_source(
                br#"
                    for i = 1, 10_000 do
                        _G[i] = i
                    end
                    return _G[10_000]
                "#,
                SemanticProfile::Luau,
            )
            .expect("large Luau global-table mutation should terminate");
        assert_eq!(result, [Value::Integer(10_000)]);
    }

    #[test]
    fn luau_official_tables_control_global_is_nil_by_default() {
        let mut engine = Engine::default();
        engine
            .vm_mut()
            .set_global(&b"limitedstack"[..], Value::Boolean(true));
        let result = engine
            .execute_owned_source(br#"return T == nil"#, SemanticProfile::Luau)
            .expect("global T probe should execute");
        assert_eq!(result, [Value::Boolean(true)]);
    }

    #[test]
    fn luau_official_tables_preamble_terminates() {
        let mut engine = Engine::default();
        engine
            .vm_mut()
            .set_global(&b"limitedstack"[..], Value::Boolean(true));
        let result = engine
            .execute_owned_source(
                br#"
                    local a = {}
                    for i = 1, 100 do a[i .. "+"] = true end
                    for i = 1, 100 do a[i .. "+"] = nil end
                    for i = 1, 100 do
                        a[i] = true
                        assert(#a == i)
                    end
                    return true
                "#,
                SemanticProfile::Luau,
            )
            .expect("official tables preamble should terminate");
        assert_eq!(result, [Value::Boolean(true)]);
    }

    #[test]
    fn luau_large_global_table_iteration_and_cleanup_terminate() {
        let mut engine = Engine::new(
            Compiler::default(),
            Vm::default().with_instruction_limit(2_000_000),
        );
        let result = engine
            .execute_owned_source(
                br#"
                    local missing = {}
                    local function find(name)
                        local key, value
                        while true do
                            key, value = next(_G, key)
                            if not key then return missing end
                            if key == name then return value end
                        end
                    end
                    for i = 1, 10_000 do
                        _G[i] = i
                    end
                    assert(find("return") == missing)
                    local values = {}
                    for i = 0, 10_000 do
                        if i % 10 ~= 0 then values["x" .. i] = i end
                    end
                    local count = 0
                    for key, value in pairs(values) do
                        count = count + 1
                        assert(values[key] == value)
                    end
                    assert(count == 9_000)
                    for i = 1, 10_000 do
                        _G[i] = nil
                    end
                    return true
                "#,
                SemanticProfile::Luau,
            )
            .expect("large Luau table iteration and cleanup should terminate");
        assert_eq!(result, [Value::Boolean(true)]);
    }

    #[test]
    fn luau_scalar_global_gc_stress_terminates() {
        let mut engine = Engine::new(
            Compiler::default(),
            Vm::default().with_instruction_limit(5_000_000),
        );
        let result = engine
            .execute_owned_source(
                br#"
                    for i = 1, 10_000 do
                        _G[i] = i
                    end
                    local copy = {}
                    for key, value in pairs(_G) do
                        copy[key] = value
                    end
                    for key in pairs(copy) do
                        if type(key) == "number" then
                            _G[key] = nil
                            collectgarbage()
                        end
                    end
                    return true
                "#,
                SemanticProfile::Luau,
            )
            .expect("scalar global GC stress should terminate");
        assert_eq!(result, [Value::Boolean(true)]);
    }

    #[test]
    fn luau_official_sparse_array_stress_terminates() {
        let mut engine = Engine::new(
            Compiler::default(),
            Vm::default().with_instruction_limit(40_000_000),
        );
        let result = engine
            .execute_owned_source(
                br#"
                    for i = 1, 10_000 do
                        _G[i] = i
                    end
                    local function obscuredalloc()
                        return {}
                    end
                    local bits = 16
                    for i = 1, 2 ^ bits - 1 do
                        local t = obscuredalloc()
                        for k = 1, bits do
                            t[k] = if bit32.extract(i, k - 1) == 1 then true else nil
                        end
                    end
                    return true
                "#,
                SemanticProfile::Luau,
            )
            .expect("official sparse array stress should terminate");
        assert_eq!(result, [Value::Boolean(true)]);
    }

    #[test]
    fn luau_official_tables_global_find_and_foreach_terminate() {
        let mut engine = Engine::default();
        engine
            .vm_mut()
            .set_global(&b"limitedstack"[..], Value::Boolean(true));
        let result = engine
            .execute_owned_source(
                br#"
                    local nofind = {}
                    a, b, c = 1, 2, 3
                    a, b, c = nil
                    local function find(name)
                        local n, v
                        while true do
                            n, v = next(_G, n)
                            if not n then return nofind end
                            assert(v ~= nil)
                            if n == name then return v end
                        end
                    end
                    local function find1(name)
                        for n, v in pairs(_G) do
                            if n == name then return v end
                        end
                        return nil
                    end
                    for i = 1, 10_000 do _G[i] = i end
                    a = {x = 90, y = 8, z = 23}
                    assert(table.foreach(a, function(i, v) if i == "x" then return v end end) == 90)
                    assert(table.foreach(a, function(i, v) if i == "a" then return v end end) == nil)
                    table.foreach({}, error)
                    table.foreachi({x = 10, y = 20}, error)
                    local a = {n = 1}
                    table.foreachi({n = 3}, function(i, v)
                        assert(a.n == i and not v)
                        a.n = a.n + 1
                    end)
                    a = {10, 20, 30, nil, 50}
                    table.foreachi(a, function(i, v) assert(a[i] == v) end)
                    assert(table.foreachi({"a", "b", "c"}, function(i, v)
                        if i == 2 then return v end
                    end) == "b")
                    assert(nofind == find("return"))
                    assert(not find1("return"))
                    _G["ret" .. "urn"] = nil
                    assert(nofind == find("return"))
                    _G["xxx"] = 1
                    return true
                "#,
                SemanticProfile::Luau,
            )
            .expect("official tables global find and foreach should terminate");
        assert_eq!(result, [Value::Boolean(true)]);
    }

    #[test]
    fn luau_official_tables_hash_iteration_cleanup_terminates() {
        let mut engine = Engine::default();
        engine
            .vm_mut()
            .set_global(&b"limitedstack"[..], Value::Boolean(true));
        let result = engine
            .execute_owned_source(
                br#"
                    local a = {}
                    for i = 0, 10_000 do
                        if i % 10 ~= 0 then a["x" .. i] = i end
                    end
                    local n = {n = 0}
                    for i, v in pairs(a) do
                        n.n = n.n + 1
                        assert(i and v and a[i] == v)
                    end
                    assert(n.n == 9_000)
                    a = nil
                    for i = 1, 10_000 do _G[i] = nil end
                    local a = {}
                    local preserve = {io = 1, string = 1, debug = 1, os = 1,
                        coroutine = 1, table = 1, math = 1}
                    for n, v in pairs(_G) do a[n] = v end
                    for n, v in pairs(a) do
                        if not preserve[n] and type(v) ~= "function"
                                and not string.find(n, "^[%u_]") then
                            _G[n] = nil
                        end
                        collectgarbage()
                    end
                    return true
                "#,
                SemanticProfile::Luau,
            )
            .expect("official tables hash iteration and cleanup should terminate");
        assert_eq!(result, [Value::Boolean(true)]);
    }

    #[test]
    fn luau_official_tables_length_and_high_key_cluster_terminates() {
        let mut engine = Engine::default();
        engine
            .vm_mut()
            .set_global(&b"limitedstack"[..], Value::Boolean(true));
        let result = engine
            .execute_owned_source(
                br#"
                    local function checknext(value)
                        local copy = {}
                        for key, item in pairs(value) do copy[key] = item end
                        for key, item in pairs(copy) do assert(value[key] == item) end
                        for key, item in pairs(value) do assert(copy[key] == item) end
                        local key, item = next(value)
                        while key do
                            copy[key] = item
                            key, item = next(value, key)
                        end
                        for key, item in pairs(copy) do assert(value[key] == item) end
                        for key, item in pairs(value) do assert(copy[key] == item) end
                    end
                    checknext({1, x = 1, y = 2, z = 3})
                    checknext({1, 2, x = 1, y = 2, z = 3})
                    checknext({1, 2, 3, x = 1, y = 2, z = 3})
                    checknext({1, 2, 3, 4, x = 1, y = 2, z = 3})
                    checknext({1, 2, 3, 4, 5, x = 1, y = 2, z = 3})
                    assert(table.getn({}) == 0)
                    assert(table.getn({[-1] = 2}) == 0)
                    assert(table.getn({1, 2, 3, nil, nil}) == 3)
                    for i = 0, 40 do
                        local value = {}
                        for j = 1, i do value[j] = j end
                        assert(table.getn(value) == i)
                    end
                    assert(table.maxn({}) == 0)
                    assert(table.maxn({[-100] = 1}) == 0)
                    assert(table.maxn({["1000"] = true}) == 0)
                    assert(table.maxn({["1000"] = true, [24.5] = 3}) == 24.5)
                    assert(table.maxn({[1000] = true}) == 1000)
                    local value = {[10] = 1, [20] = 2}
                    value[20] = nil
                    assert(table.maxn(value) == 10)
                    value = {}
                    for i = 0, 50 do value[math.pow(2, i)] = true end
                    assert(value[table.getn(value)])
                    return true
                "#,
                SemanticProfile::Luau,
            )
            .expect("official tables length and high-key cluster should terminate");
        assert_eq!(result, [Value::Boolean(true)]);
    }

    #[test]
    fn luau_official_tables_mutation_and_insert_cluster_terminates() {
        let mut engine = Engine::default();
        engine
            .vm_mut()
            .set_global(&b"limitedstack"[..], Value::Boolean(true));
        let result = engine
            .execute_owned_source(
                br#"
                    local value = {[{1}] = 1, [{2}] = 2, [string.rep("x ", 4)] = 3,
                        [100.3] = 4, [4] = 5}
                    local count = 0
                    for key, item in pairs(value) do
                        count = count + 1
                        assert(value[key] == item)
                        value[key] = nil
                        collectgarbage()
                        assert(value[key] == nil)
                    end
                    assert(count == 5)
                    local function exercise(items)
                        table.insert(items, 10)
                        table.insert(items, 2, 20)
                        table.insert(items, 1, -1)
                        table.insert(items, 40)
                        table.insert(items, table.getn(items) + 1, 50)
                        table.insert(items, 2, -2)
                        assert(table.remove(items, 1) == -1)
                        assert(table.remove(items, 1) == -2)
                        assert(table.remove(items, 1) == 10)
                        assert(table.remove(items, 1) == 20)
                        assert(table.remove(items, 1) == 40)
                        assert(table.remove(items, 1) == 50)
                        assert(table.remove(items, 1) == nil)
                    end
                    value = {n = 0, [-7] = "ban"}
                    exercise(value)
                    assert(value.n == 0 and value[-7] == "ban")
                    value = {[-7] = "ban"}
                    exercise(value)
                    assert(value.n == nil and table.getn(value) == 0 and value[-7] == "ban")
                    value = {"c", "d"}
                    table.insert(value, 3, "a")
                    table.insert(value, "b")
                    assert(table.remove(value, 1) == "c")
                    assert(table.remove(value, 1) == "d")
                    assert(table.remove(value, 1) == "a")
                    assert(table.remove(value, 1) == "b")
                    assert(table.getn(value) == 0 and value.n == nil)
                    return true
                "#,
                SemanticProfile::Luau,
            )
            .expect("official tables mutation and insert cluster should terminate");
        assert_eq!(result, [Value::Boolean(true)]);
    }

    #[test]
    fn luau_bit32_extract_and_replace_match_reference() {
        let source = br#"
            return bit32.extract(0x12345678, 0, 4),
                bit32.extract(0x12345678, 4, 4),
                bit32.replace(0x12345678, 5, 28, 4),
                bit32.replace(0x12345678, 0x87654321, 0, 32)
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Luau)
                .unwrap_or_else(|error| panic!("{error}")),
            vec![
                Value::Number(8.0),
                Value::Number(7.0),
                Value::Number(0x52345678_u32 as f64),
                Value::Number(0x87654321_u32 as f64),
            ]
        );
    }

    #[test]
    fn luau_bit32_extract_accepts_values_returned_through_pcall() {
        let source = br#"
            local function noinline(value, ...)
                local ok, result = pcall(function(argument) return argument end, value)
                return result
            end
            return bit32.extract(noinline(0x12345678), 0, 4),
                bit32.extract(0x12345678, noinline(0), 4),
                bit32.extract(0x12345678, 0, noinline(4))
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Luau)
                .unwrap_or_else(|error| panic!("{error}")),
            vec![Value::Number(8.0), Value::Number(8.0), Value::Number(8.0)]
        );
    }

    #[test]
    fn string_packsize_rejects_oversized_formats_without_allocating() {
        let mut engine = Engine::new(
            Compiler::default(),
            Vm::default().with_instruction_limit(100_000),
        );
        let result = engine
            .execute_owned_source(
                br#"
                    local function rejects(f, ...)
                        return not pcall(f, ...)
                    end
                    local repeated = string.rep("c268435456", 8)
                    local near_limit = string.rep("c268435456", 7) .. "c268435453"
                    return rejects(string.packsize, repeated),
                        rejects(string.packsize, near_limit),
                        string.packsize("c1073741824") == 1073741824,
                        rejects(string.unpack, "i987654321", ""),
                        rejects(string.unpack, "c9876543210", ""),
                        rejects(string.packsize, "c1" .. string.rep("0", 40))
                "#,
                SemanticProfile::Luau,
            )
            .expect("oversized pack formats should be bounded");
        assert_eq!(
            result,
            [
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
            ]
        );
    }

    #[test]
    fn string_pack_variable_integer_formats_terminate() {
        let mut engine = Engine::new(
            Compiler::default(),
            Vm::default().with_instruction_limit(100_000),
        );
        let result = engine
            .execute_owned_source(
                br#"
                    local pack = string.pack
                    local unpack = string.unpack
                    for i = 1, 16 do
                        local signed = string.rep("\xff", i)
                        assert(pack("i" .. i, -1) == signed)
                        assert(unpack("i" .. i, signed) == -1)
                        local unsigned = "\xaa" .. string.rep("\0", i - 1)
                        assert(pack("<I" .. i, 0xaa) == unsigned)
                        assert(unpack("<I" .. i, unsigned) == 0xaa)
                    end
                    return true
                "#,
                SemanticProfile::Luau,
            )
            .expect("variable integer pack formats should terminate");
        assert_eq!(result, [Value::Boolean(true)]);
    }

    #[test]
    fn owned_frontend_compiles_and_executes_every_baseline_profile() {
        use frontend::{
            CompilerId, CompilerIdentity, IdentityLimits, OwnedCompiler, SemanticProfile,
            SourceFile, SourceId, SourceLimits,
        };

        let source = SourceFile::new(
            SourceId::new(1),
            "answer.blu",
            b"local empty = nil\nlocal yes = true\nreturn empty, yes, false, 'blu', not empty, not yes, not 0, not 'blu', (40 + 2), 40 - 2 - 3, 2 + 5 * 8, 21 / 2, 20 / 5, -7, -(2 + 3), - -1, -7 % 3, 7 % -3, -2^2, 2^-2, 2^3^2, #'blu', #\"a\\nb\", #'', 1.5, .25, 1., 1.e2, 2e3, 4.5E-2, 0x10, 0Xff".to_vec(),
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
            b"local answer = 40;answer = answer + 2;return answer;".to_vec(),
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
        let escaped_string = SourceFile::new(
            SourceId::new(9),
            "escaped-string.blu",
            br#"return "\\\'\"\a\b\f\n\r\t\v""#.to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let binary_integers = SourceFile::new(
            SourceId::new(10),
            "binary-integers.blu",
            b"return 0b101010, 0B1111_0000".to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let hex_exponents = SourceFile::new(
            SourceId::new(11),
            "hex-exponents.blu",
            b"return 0x1p2, 0x1p-2".to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let fractional_hex = SourceFile::new(
            SourceId::new(12),
            "fractional-hex.blu",
            b"return 0x1.8p1, 0x.8p1, 0x1.8".to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let decimal_byte_escapes = SourceFile::new(
            SourceId::new(13),
            "decimal-byte-escapes.blu",
            br#"return "\0\7\65\255""#.to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let hex_byte_escapes = SourceFile::new(
            SourceId::new(14),
            "hex-byte-escapes.blu",
            br#"return "\x00\x41\xff""#.to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let whitespace_escape = SourceFile::new(
            SourceId::new(15),
            "whitespace-escape.blu",
            b"return \"left\\z \n\t\r\n right\"".to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let line_continuations = SourceFile::new(
            SourceId::new(16),
            "line-continuations.blu",
            b"return \"a\\\nb\", \"c\\\r\nd\", \"e\\\rf\"".to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let unicode_escapes = SourceFile::new(
            SourceId::new(17),
            "unicode-escapes.blu",
            br#"return "\u{41}\u{D800}\u{1F41B}""#.to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let extended_unicode_escapes = SourceFile::new(
            SourceId::new(18),
            "extended-unicode-escapes.blu",
            br#"return "\u{110000}\u{7fffffff}""#.to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let long_string = SourceFile::new(
            SourceId::new(19),
            "long-string.blu",
            b"return [==[\ra\rb\r\nc\\n\0\xff]==]".to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let concatenation = SourceFile::new(
            SourceId::new(20),
            "concatenation.blu",
            br#"return "a" .. 1 .. 2.5, 1 + 2 .. "x""#.to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let comparisons = SourceFile::new(
            SourceId::new(21),
            "comparisons.blu",
            br#"return 2 == 2, 2 ~= 3, 1 < 2, 2 <= 2, 3 > 2, 3 >= 3, "a" < "b", 1 == "1", 1 + 2 < 4, "a" .. "b" == "ab""#
                .to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let logical_operators = SourceFile::new(
            SourceId::new(22),
            "logical-operators.blu",
            br#"return "left" and "right", nil or "fallback", false and (1 + "2"), true or (1 + "2"), false or nil, nil and "unused""#
                .to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let conditionals = SourceFile::new(
            SourceId::new(23),
            "conditionals.blu",
            br#"local value = "none"
if false then
    value = "bad"
elseif 1 < 2 then
    local selected = "selected"
    value = selected
else
    value = "else"
end
if true then
    value = value .. "!"
else
    value = "bad"
end
return value"#
                .to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let while_loop = SourceFile::new(
            SourceId::new(24),
            "while-loop.blu",
            b"local index = 0\nlocal total = 0\nwhile index < 5 do\nlocal next = index + 1\nindex = next\ntotal = total + index\nend\nreturn total, index"
                .to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let nested_break = SourceFile::new(
            SourceId::new(25),
            "nested-break.blu",
            b"local outer = 0\nlocal hits = 0\nwhile outer < 3 do\nouter = outer + 1\nlocal inner = 0\nwhile true do\ninner = inner + 1\nhits = hits + 1\nif inner == 2 then break end\nend\nif outer == 2 then break end\nend\nreturn outer, hits"
                .to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let continue_loop = SourceFile::new(
            SourceId::new(26),
            "continue-loop.blu",
            b"local index = 0\nlocal total = 0\nwhile index < 5 do\nindex = index + 1\nif index % 2 == 0 then continue end\ntotal = total + index\nend\nreturn total"
                .to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let repeat_loop = SourceFile::new(
            SourceId::new(27),
            "repeat-loop.blu",
            b"local count = 0\nrepeat\ncount = count + 1\nlocal current = count\nuntil current == 3\nreturn count"
                .to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let do_block = SourceFile::new(
            SourceId::new(28),
            "do-block.blu",
            b"local value = 1\ndo\nlocal value = 5\nvalue = value + 1\nend\nreturn value".to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let numeric_for = SourceFile::new(
            SourceId::new(29),
            "numeric-for.blu",
            b"local total = 0\nfor index = 1, 4 do total = total + index end\nreturn total"
                .to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let descending_for = SourceFile::new(
            SourceId::new(30),
            "descending-for.blu",
            b"local total = 0\nfor index = 5, 1, -2 do total = total + index end\nreturn total"
                .to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let globals = SourceFile::new(
            SourceId::new(31),
            "globals.blu",
            b"answer = 40\nanswer = answer + 2\nreturn answer, missing".to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let tables = SourceFile::new(
            SourceId::new(32),
            "tables.blu",
            br#"local values = {40, answer = 41}; values.answer = 42; return values[1], values.answer, values["missing"]"#
                .to_vec(),
            SourceLimits::default(),
        )
        .unwrap();
        let fixed_calls = SourceFile::new(
            SourceId::new(33),
            "fixed-calls.blu",
            br#"local object = {kind = type}; print("owned"); return string.sub("blue", 2), type({}), object:kind()"#
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
                .compile(&fixed_calls, profile, compiler.clone())
                .unwrap();
            let mut engine = Engine::default();
            assert_eq!(
                engine.execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                Ok(vec![
                    Value::String(b"lue".as_slice().into()),
                    Value::String(b"table".as_slice().into()),
                    Value::String(b"table".as_slice().into()),
                ]),
                "{profile}"
            );
            assert_eq!(engine.vm_mut().take_output(), b"owned\n", "{profile}");
            let compiled = OwnedCompiler::default()
                .compile(&tables, profile, compiler.clone())
                .unwrap();
            let expected = if matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ) {
                Value::Integer(42)
            } else {
                Value::Number(42.0)
            };
            assert_eq!(
                Engine::default()
                    .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                Ok(vec![
                    if matches!(
                        profile,
                        SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
                    ) {
                        Value::Integer(40)
                    } else {
                        Value::Number(40.0)
                    },
                    expected,
                    Value::Nil,
                ]),
                "{profile}"
            );
            let compiled = OwnedCompiler::default()
                .compile(&globals, profile, compiler.clone())
                .unwrap();
            let expected = if matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ) {
                Value::Integer(42)
            } else {
                Value::Number(42.0)
            };
            assert_eq!(
                Engine::default()
                    .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                Ok(vec![expected, Value::Nil]),
                "{profile}"
            );
            let compiled = OwnedCompiler::default()
                .compile(&descending_for, profile, compiler.clone())
                .unwrap();
            let expected = if matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ) {
                Value::Integer(9)
            } else {
                Value::Number(9.0)
            };
            assert_eq!(
                Engine::default()
                    .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                Ok(vec![expected]),
                "{profile}"
            );
            let compiled = OwnedCompiler::default()
                .compile(&numeric_for, profile, compiler.clone())
                .unwrap();
            let expected = if matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ) {
                Value::Integer(10)
            } else {
                Value::Number(10.0)
            };
            assert_eq!(
                Engine::default()
                    .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                Ok(vec![expected]),
                "{profile}"
            );
            let compiled = OwnedCompiler::default()
                .compile(&do_block, profile, compiler.clone())
                .unwrap();
            let expected = if matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ) {
                Value::Integer(1)
            } else {
                Value::Number(1.0)
            };
            assert_eq!(
                Engine::default()
                    .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                Ok(vec![expected]),
                "{profile}"
            );
            let compiled = OwnedCompiler::default()
                .compile(&repeat_loop, profile, compiler.clone())
                .unwrap();
            let expected = if matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ) {
                Value::Integer(3)
            } else {
                Value::Number(3.0)
            };
            assert_eq!(
                Engine::default()
                    .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                Ok(vec![expected]),
                "{profile}"
            );
            let compiled =
                OwnedCompiler::default().compile(&continue_loop, profile, compiler.clone());
            if matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
                assert_eq!(
                    Engine::default().execute_owned_compilation(
                        compiled.unwrap(),
                        bytecode::blu::BluLimits::default()
                    ),
                    Ok(vec![Value::Number(9.0)]),
                    "{profile}"
                );
            } else {
                assert!(compiled.is_err(), "{profile}");
            }
            let compiled = OwnedCompiler::default()
                .compile(&nested_break, profile, compiler.clone())
                .unwrap();
            let expected = if matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ) {
                vec![Value::Integer(2), Value::Integer(4)]
            } else {
                vec![Value::Number(2.0), Value::Number(4.0)]
            };
            assert_eq!(
                Engine::default()
                    .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                Ok(expected),
                "{profile}"
            );
            let compiled = OwnedCompiler::default()
                .compile(&while_loop, profile, compiler.clone())
                .unwrap();
            let expected = if matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ) {
                vec![Value::Integer(15), Value::Integer(5)]
            } else {
                vec![Value::Number(15.0), Value::Number(5.0)]
            };
            assert_eq!(
                Engine::default()
                    .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                Ok(expected),
                "{profile}"
            );
            let compiled = OwnedCompiler::default()
                .compile(&conditionals, profile, compiler.clone())
                .unwrap();
            assert_eq!(
                Engine::default()
                    .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                Ok(vec![Value::String(Arc::from(&b"selected!"[..]))]),
                "{profile}"
            );
            let compiled = OwnedCompiler::default()
                .compile(&logical_operators, profile, compiler.clone())
                .unwrap();
            assert_eq!(
                Engine::default()
                    .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                Ok(vec![
                    Value::String(Arc::from(&b"right"[..])),
                    Value::String(Arc::from(&b"fallback"[..])),
                    Value::Boolean(false),
                    Value::Boolean(true),
                    Value::Nil,
                    Value::Nil,
                ]),
                "{profile}"
            );
            let compiled = OwnedCompiler::default()
                .compile(&comparisons, profile, compiler.clone())
                .unwrap();
            assert_eq!(
                Engine::default()
                    .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                Ok(vec![
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(false),
                    Value::Boolean(true),
                    Value::Boolean(true),
                ]),
                "{profile}"
            );
            let compiled = OwnedCompiler::default()
                .compile(&concatenation, profile, compiler.clone())
                .unwrap();
            assert_eq!(
                Engine::default()
                    .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                Ok(vec![
                    Value::String(Arc::from(&b"a12.5"[..])),
                    Value::String(Arc::from(&b"3x"[..])),
                ]),
                "{profile}"
            );
            let compiled = OwnedCompiler::default()
                .compile(&long_string, profile, compiler.clone())
                .unwrap();
            let expected = if profile == SemanticProfile::Luau {
                b"\ra\rb\nc\\n\0\xff".as_slice()
            } else {
                b"a\nb\nc\\n\0\xff".as_slice()
            };
            assert_eq!(
                Engine::default()
                    .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                Ok(vec![Value::String(Arc::from(expected))]),
                "{profile}"
            );
            if !matches!(profile, SemanticProfile::Lua51 | SemanticProfile::Lua52) {
                let compiled = OwnedCompiler::default()
                    .compile(&unicode_escapes, profile, compiler.clone())
                    .unwrap();
                assert_eq!(
                    Engine::default()
                        .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                    Ok(vec![Value::String(Arc::from(
                        &[0x41, 0xed, 0xa0, 0x80, 0xf0, 0x9f, 0x90, 0x9b][..]
                    ))]),
                    "{profile}"
                );
            }
            if matches!(
                profile,
                SemanticProfile::Blu | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ) {
                let compiled = OwnedCompiler::default()
                    .compile(&extended_unicode_escapes, profile, compiler.clone())
                    .unwrap();
                assert_eq!(
                    Engine::default()
                        .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                    Ok(vec![Value::String(Arc::from(
                        &[0xf4, 0x90, 0x80, 0x80, 0xfd, 0xbf, 0xbf, 0xbf, 0xbf, 0xbf,][..]
                    ))]),
                    "{profile}"
                );
            }
            let compiled = OwnedCompiler::default()
                .compile(&line_continuations, profile, compiler.clone())
                .unwrap();
            assert_eq!(
                Engine::default()
                    .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                Ok(vec![
                    Value::String(Arc::from(&b"a\nb"[..])),
                    Value::String(Arc::from(&b"c\nd"[..])),
                    Value::String(Arc::from(&b"e\nf"[..])),
                ]),
                "{profile}"
            );
            let compiled = OwnedCompiler::default()
                .compile(&decimal_byte_escapes, profile, compiler.clone())
                .unwrap();
            assert_eq!(
                Engine::default()
                    .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                Ok(vec![Value::String(Arc::from(&[0, 7, 65, 255][..]))]),
                "{profile}"
            );
            if profile != SemanticProfile::Lua51 {
                let compiled = OwnedCompiler::default()
                    .compile(&whitespace_escape, profile, compiler.clone())
                    .unwrap();
                assert_eq!(
                    Engine::default()
                        .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                    Ok(vec![Value::String(Arc::from(&b"leftright"[..]))]),
                    "{profile}"
                );
                let compiled = OwnedCompiler::default()
                    .compile(&hex_byte_escapes, profile, compiler.clone())
                    .unwrap();
                assert_eq!(
                    Engine::default()
                        .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                    Ok(vec![Value::String(Arc::from(&[0, 65, 255][..]))]),
                    "{profile}"
                );
            }
            if profile != SemanticProfile::Luau {
                let compiled = OwnedCompiler::default()
                    .compile(&hex_exponents, profile, compiler.clone())
                    .unwrap();
                assert_eq!(
                    Engine::default()
                        .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                    Ok(vec![Value::Number(4.0), Value::Number(0.25)]),
                    "{profile}"
                );
            }
            if matches!(
                profile,
                SemanticProfile::Blu
                    | SemanticProfile::Lua52
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                let compiled = OwnedCompiler::default()
                    .compile(&fractional_hex, profile, compiler.clone())
                    .unwrap();
                assert_eq!(
                    Engine::default()
                        .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                    Ok(vec![
                        Value::Number(3.0),
                        Value::Number(1.0),
                        Value::Number(1.5)
                    ]),
                    "{profile}"
                );
            }
            if matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
                let compiled = OwnedCompiler::default()
                    .compile(&binary_integers, profile, compiler.clone())
                    .unwrap();
                assert_eq!(
                    Engine::default()
                        .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                    Ok(vec![Value::Number(42.0), Value::Number(240.0)]),
                    "{profile}"
                );
            }
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
                    Value::Number(10.5),
                    Value::Number(4.0),
                    Value::Number(-7.0),
                    Value::Number(-5.0),
                    Value::Number(1.0),
                    Value::Number(2.0),
                    Value::Number(-2.0),
                    Value::Number(-4.0),
                    Value::Number(0.25),
                    Value::Number(512.0),
                    Value::Number(3.0),
                    Value::Number(3.0),
                    Value::Number(0.0),
                    Value::Number(1.5),
                    Value::Number(0.25),
                    Value::Number(1.0),
                    Value::Number(100.0),
                    Value::Number(2_000.0),
                    Value::Number(0.045),
                    if matches!(
                        profile,
                        SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
                    ) {
                        Value::Integer(16)
                    } else {
                        Value::Number(16.0)
                    },
                    if matches!(
                        profile,
                        SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
                    ) {
                        Value::Integer(255)
                    } else {
                        Value::Number(255.0)
                    },
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
            let compiled = OwnedCompiler::default()
                .compile(&escaped_string, profile, compiler.clone())
                .unwrap();
            assert_eq!(
                Engine::default()
                    .execute_owned_compilation(compiled, bytecode::blu::BluLimits::default()),
                Ok(vec![Value::String(Arc::from(
                    &b"\\'\"\x07\x08\x0c\n\r\t\x0b"[..]
                ))]),
                "string escapes under {profile}"
            );
        }
    }

    #[test]
    fn official_luau_constructs_priority_cluster_matches_blu() {
        let source = r#"
            assert(2^3^2 == 2^(3^2))
            assert(2^3*4 == (2^3)*4)
            assert(2^-2 == 1/4 and -2^- -2 == - - -4)
            assert(not nil and 2 and not(2>3 or 3<2))
            assert(-3-1-5 == 0+0-9)
            assert(-2^2 == -4 and (-2)^2 == 4 and 2*2-3-1 == 0)
            assert(2*1+3/3 == 3 and 1+2 .. 3*1 == "33")
            assert(not(2+1 > 3*1) and "a".."b" > "a")
            assert(not ((true or false) and nil))
            assert(true or false and nil)
            local a,b = 1,nil
            assert(-(1 or 2) == -1 and (1 and 2)+(-1.25 or -4) == 0.75)
            x = ((b or a)+1 == 2 and (10 or a)+1 == 11)
            assert(x)
            x = (((2<3) or 1) == true and (2<3 and 4) == 4)
            assert(x)
            x,y=1,2
            assert((x>y) and x or y == 2)
            x,y=2,1
            assert((x>y) and x or y == 2)
            return true
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn official_luau_constructs_repeat_break_cluster_matches_blu() {
        let source = br#"
            function f(b)
                local x = 1
                repeat
                    local a
                    if b == 1 then
                        local b = 1
                        x = 10
                        break
                    elseif b == 2 then
                        x = 20
                        break
                    elseif b == 3 then
                        x = 30
                    else
                        local a,b,c,d = math.sin(1)
                        x = x + 1
                    end
                until x >= 12
                return x
            end
            return f(1), f(2), f(3), f(4)
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap(),
            vec![
                Value::Integer(10),
                Value::Integer(20),
                Value::Integer(30),
                Value::Integer(12),
            ]
        );
    }

    #[test]
    fn official_luau_table_move_type_errors_match_reference_shape() {
        let source = br#"
            local ok, err = pcall(table.move, 1, 2, 3, 4)
            local too_many_ok, too_many = pcall(table.move, {}, 0, 2147483647, 1)
            local wrap_ok, wrap = pcall(table.move, {}, 1, 2147483647, 2)
            return not ok and string.find(err, "table expected") ~= nil
                and not too_many_ok and string.find(too_many, "too many elements to move") ~= nil
                and not wrap_ok and string.find(wrap, "too many elements to move") ~= nil
        "#;
        let result = Engine::default()
            .execute_owned_source(source, SemanticProfile::Blu)
            .unwrap();
        assert_eq!(result, vec![Value::Boolean(true)]);
    }

    #[test]
    fn luau_table_move_preserves_32_bit_destination_bounds() {
        let source = br#"
            local ok, err = pcall(table.move, {}, 1, 2147483647, 2)
            return not ok and string.find(err, "destination wrap around") ~= nil
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Luau)
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn blu_table_move_preserves_64_bit_destination_positions() {
        let source = br#"
            local max = 2147483647
            local min = -2147483648
            local high = table.move({45}, 1, 1, max)
            local low = table.move({46}, 1, 1, min)
            return high[max] == 45 and low[min] == 46
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn luau_table_move_reports_readonly_destinations() {
        let source = br#"
            local ok, message = pcall(table.move, table.freeze({1}), 1, 1, 1)
            return not ok and string.find(message, "readonly") ~= nil
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Luau)
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn official_luau_table_move_edge_diagnostics() {
        let source = br#"
            local function eqT(a, b)
                for k, v in pairs(a) do
                    if b[k] ~= v then return false, "a", k, v, b[k] end
                end
                for k, v in pairs(b) do
                    if a[k] ~= v then return false, "b", k, a[k], v end
                end
                return true, nil, nil, nil, nil
            end
            local a = { [ -1000 ] = 1, [1000] = 2, [1] = 3 }
            table.move({10}, -1000, 1000, -1000, a)
            return eqT(a, {10})
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            let values = Engine::default()
                .execute_owned_source(source, profile)
                .unwrap_or_else(|error| panic!("{profile:?}: {error:?}"));
            assert_eq!(
                values.first(),
                Some(&Value::Boolean(true)),
                "{profile:?}: {values:?}"
            );
        }
    }

    #[test]
    fn official_luau_table_move_extreme_keys_match() {
        let source = br#"
            local function eqT(a, b)
                for k, v in pairs(a) do
                    if b[k] ~= v then return false end
                end
                for k, v in pairs(b) do
                    if a[k] ~= v then return false end
                end
                return true
            end
            local maxI = 2147483647
            local minI = -2147483648
            local a = table.move({[maxI - 2] = 1, [maxI - 1] = 2, [maxI] = 3},
                maxI - 2, maxI, -10, {})
            local b = table.move({[minI] = 1, [minI + 1] = 2, [minI + 2] = 3},
                minI, minI + 2, -10, {})
            local c = table.move({45}, 1, 1, maxI)
            local d = table.move({[maxI] = 100}, maxI, maxI, minI)
            local e = table.move({[minI] = 100}, minI, minI, maxI)
            return eqT(a, {[-10] = 1, [-9] = 2, [-8] = 3}),
                eqT(b, {[-10] = 1, [-9] = 2, [-8] = 3}),
                eqT(c, {45, [maxI] = 45}),
                eqT(d, {[minI] = 100, [maxI] = 100}),
                eqT(e, {[minI] = 100, [maxI] = 100})
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            let values = Engine::default()
                .execute_owned_source(source, profile)
                .unwrap_or_else(|error| panic!("{profile:?}: {error:?}"));
            assert_eq!(
                values,
                vec![
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                ],
                "{profile:?}: {values:?}"
            );
        }
    }

    #[test]
    fn luau_bit32_extension_cluster_matches_reference() {
        let source = br#"
            return bit32.btest(), bit32.btest(0),
                bit32.countlz(0), bit32.countlz(0x80000000),
                bit32.countrz(0), bit32.countrz(0x80000000),
                bit32.byteswap(0x10203040), bit32.byteswap(-1)
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            let values = Engine::default()
                .execute_owned_source(source, profile)
                .unwrap_or_else(|error| panic!("{profile}: {error}"));
            let integral = |value| {
                if profile == SemanticProfile::Blu {
                    Value::Integer(value)
                } else {
                    Value::Number(value as f64)
                }
            };
            assert_eq!(
                values,
                vec![
                    Value::Boolean(true),
                    Value::Boolean(false),
                    integral(32),
                    integral(0),
                    integral(32),
                    integral(31),
                    integral(0x40302010),
                    integral(0xffff_ffff),
                ],
                "{profile}"
            );
        }
        assert_eq!(
            Engine::default()
                .execute_owned_source(br#"return type(bit32.countlz)"#, SemanticProfile::Lua52)
                .unwrap(),
            vec![Value::String(Arc::from(&b"nil"[..]))]
        );
    }

    #[test]
    fn luau_bit32_rotate_loop_matches_reference_and_terminates() {
        let source = br#"
            local function noinline(x, ...)
                local ok, result = pcall(function(value) return value end, x)
                return result
            end
            assert(bit32.band() == bit32.bnot(0))
            assert(bit32.btest() == true)
            assert(bit32.bor() == 0)
            assert(bit32.bxor() == 0)
            assert(bit32.band(-1) == 0xffffffff)
            assert(bit32.band(2^33 - 1) == 0xffffffff)
            assert(bit32.band(-2^33 - 1) == 0xffffffff)
            assert(bit32.band(2^33 + 1) == 1)
            assert(bit32.band(-2^33 + 1) == 1)
            assert(bit32.band(-2^40) == 0)
            assert(bit32.band(2^40) == 0)
            assert(bit32.band(-2^40 - 2) == 0xfffffffe)
            assert(bit32.band(2^40 - 4) == 0xfffffffc)
            assert(bit32.band(noinline(-1)) == 0xffffffff)
            assert(bit32.band(noinline(2^33 - 1)) == 0xffffffff)
            assert(bit32.band(noinline(-2^33 - 1)) == 0xffffffff)
            assert(bit32.band(noinline(2^33 + 1)) == 1)
            assert(bit32.band(noinline(-2^33 + 1)) == 1)
            assert(bit32.band(noinline(-2^40)) == 0)
            assert(bit32.band(noinline(2^40)) == 0)
            assert(bit32.band(noinline(-2^40 - 2)) == 0xfffffffe)
            assert(bit32.band(noinline(2^40 - 4)) == 0xfffffffc)
            assert(bit32.lrotate(0, -1) == 0)
            assert(bit32.lrotate(0, 7) == 0)
            assert(bit32.lrotate(0x12345678, 4) == 0x23456781)
            assert(bit32.rrotate(0x12345678, -4) == 0x23456781)
            assert(bit32.lrotate(0x12345678, -8) == 0x78123456)
            assert(bit32.rrotate(0x12345678, 8) == 0x78123456)
            assert(bit32.lrotate(0xaaaaaaaa, 2) == 0xaaaaaaaa)
            assert(bit32.lrotate(0xaaaaaaaa, -2) == 0xaaaaaaaa)
            assert(bit32.lrotate(noinline(0), -1) == 0)
            assert(bit32.lrotate(noinline(0), 7) == 0)
            assert(bit32.lrotate(noinline(0x12345678), 4) == 0x23456781)
            assert(bit32.rrotate(noinline(0x12345678), -4) == 0x23456781)
            assert(bit32.lrotate(noinline(0x12345678), -8) == 0x78123456)
            assert(bit32.rrotate(noinline(0x12345678), 8) == 0x78123456)
            assert(bit32.lrotate(noinline(0xaaaaaaaa), 2) == 0xaaaaaaaa)
            assert(bit32.lrotate(noinline(0xaaaaaaaa), -2) == 0xaaaaaaaa)
            for i = -50, 50 do
                assert(bit32.lrotate(0x89abcdef, i)
                    == bit32.lrotate(0x89abcdef, i % 32))
            end
            assert(bit32.lshift(0x12345678, 4) == 0x23456780)
            assert(bit32.lshift(0x12345678, 8) == 0x34567800)
            assert(bit32.lshift(0x12345678, -4) == 0x01234567)
            assert(bit32.lshift(0x12345678, -8) == 0x00123456)
            assert(bit32.lshift(0x12345678, 32) == 0)
            assert(bit32.lshift(0x12345678, -32) == 0)
            assert(bit32.rshift(0x12345678, 4) == 0x01234567)
            assert(bit32.rshift(0x12345678, 8) == 0x00123456)
            assert(bit32.rshift(0x12345678, 32) == 0)
            assert(bit32.rshift(0x12345678, -32) == 0)
            assert(bit32.arshift(0x12345678, 0) == 0x12345678)
            assert(bit32.arshift(0x12345678, 1) == 0x12345678 / 2)
            assert(bit32.arshift(0x12345678, -1) == 0x12345678 * 2)
            assert(bit32.arshift(-1, 1) == 0xffffffff)
            assert(bit32.arshift(-1, 24) == 0xffffffff)
            assert(bit32.arshift(-1, 32) == 0xffffffff)
            assert(bit32.arshift(-1, -1) == (-1 * 2) % 2^32)
            return true
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            let mut engine = Engine::default();
            *engine.vm_mut() = Vm::default().with_instruction_limit(50_000);
            assert_eq!(
                engine
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile}: {error}")),
                vec![Value::Boolean(true)],
                "{profile}"
            );
        }
    }

    #[test]
    fn luau_math_extensions_coerce_numeric_strings() {
        let source = br#"
            return math.clamp("0", 2, 3),
                math.clamp("4", 2, 3),
                math.sign("-2"),
                math.round("1.8"),
                math.lerp("1", "5", 0.5),
                math.isnan("123.45"),
                math.isinf("123.45"),
                math.isfinite("123.45")
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Luau)
                .unwrap_or_else(|error| panic!("Luau: {error}")),
            vec![
                Value::Number(2.0),
                Value::Number(3.0),
                Value::Number(-1.0),
                Value::Number(2.0),
                Value::Number(3.0),
                Value::Boolean(false),
                Value::Boolean(false),
                Value::Boolean(true),
            ]
        );
    }

    #[test]
    fn luau_table_clear_argument_diagnostics_match_reference() {
        let source = br#"
            local missing_ok, missing_error = pcall(table.clear)
            local value_ok, value_error = pcall(table.clear, 1)
            return missing_ok == false
                and missing_error == "missing argument #1 to 'clear' (table expected)"
                and value_ok == false
                and value_error == "invalid argument #1 to 'clear' (table expected, got number)"
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap(),
                vec![Value::Boolean(true)],
                "{profile}"
            );
        }
    }

    #[test]
    fn table_sort_validates_empty_table_comparators() {
        let source = br#"
            return not pcall(table.sort, {}, 42)
                and not pcall(table.sort, {}, {})
                and pcall(table.sort, {}, nil)
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn official_luau_sort_basic_cluster_executes() {
        let source = br#"
            local function checksort(t, f, ...)
                assert(#t == select('#', ...))
                local copy = table.clone(t)
                table.sort(copy, f)
                for i = 1, #t do
                    assert(copy[i] == select(i, ...))
                end
            end
            checksort({}, nil)
            checksort({1}, nil, 1)
            checksort({1, 2}, nil, 1, 2)
            checksort({2, 1}, nil, 1, 2)
            checksort({3, 1, 2}, nil, 1, 2, 3)
            return pcall(table.sort, table.freeze({2, 1})) == false
                and pcall(table.sort) == false
                and pcall(table.sort, "abc") == false
                and pcall(table.sort, {}, 42) == false
                and pcall(table.sort, {}, {}) == false
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn luau_table_sort_rejects_callback_mutation() {
        let source = br#"
            local values = { 3, 1, 2 }
            local ok = pcall(table.sort, values, function(left, right)
                values.extra = true
                return left < right
            end)
            return ok == false
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Luau)
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn official_luau_sort_legacy_global_table_cluster_executes() {
        let source = br#"
            function check(a, f)
                f = f or function(x, y) return x < y end
                for n = table.getn(a), 2, -1 do
                    assert(not f(a[n], a[n - 1]))
                end
            end
            a = {"Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"}
            table.sort(a)
            check(a)
            return type(a) == "table" and #a == 12
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn table_sort_recovers_after_invalid_comparator_calls() {
        let source = br#"
            assert(pcall(table.sort) == false)
            assert(pcall(table.sort, "abc") == false)
            assert(pcall(table.sort, {}, 42) == false)
            assert(pcall(table.sort, {}, {}) == false)
            local values = {"Jan", "Feb", "Mar", "Apr"}
            table.sort(values)
            return values[1] == "Apr" and values[4] == "Mar"
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn blu_global_environment_is_self_referential() {
        let source = br#"
            a = 42
            return _G == _G and rawget(_G, "a") == 42
                and rawget(_G, "_G") == _G
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn official_luau_sort_full_prelude_preserves_legacy_table() {
        let source = br#"
            function checksort(t, f, ...)
                assert(#t == select('#', ...))
                local copy = table.clone(t)
                table.sort(copy, f)
                for i = 1, #t do assert(copy[i] == select(i, ...)) end
            end
            checksort({}, nil)
            checksort({1}, nil, 1)
            checksort({1, 2}, nil, 1, 2)
            checksort({2, 1}, nil, 1, 2)
            checksort({1, 2, 3}, nil, 1, 2, 3)
            checksort({2, 1, 3}, nil, 1, 2, 3)
            checksort({1, 3, 2}, nil, 1, 2, 3)
            checksort({3, 2, 1}, nil, 1, 2, 3)
            checksort({3, 1, 2}, nil, 1, 2, 3)
            checksort({3, 8, 1, 7, 10, 2, 5, 4, 9, 6}, nil, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10)
            checksort({"Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"}, nil, "Apr", "Aug", "Dec", "Feb", "Jan", "Jul", "Jun", "Mar", "May", "Nov", "Oct", "Sep")
            checksort({3, 1, 1, 7, 1, 3, 5, 1, 9, 3}, nil, 1, 1, 1, 1, 3, 3, 3, 5, 7, 9)
            checksort({3, 8, 1, 7, 10, 2, 5, 4, 9, 6}, function(a, b) return a > b end, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1)
            assert(pcall(table.sort, table.freeze({2, 1})) == false)
            assert(pcall(table.sort) == false)
            assert(pcall(table.sort, "abc") == false)
            assert(pcall(table.sort, {}, 42) == false)
            assert(pcall(table.sort, {}, {}) == false)
            function check(a, f)
                f = f or function(x, y) return x < y end
                for n = table.getn(a), 2, -1 do assert(not f(a[n], a[n - 1])) end
            end
            a = {"Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"}
            table.sort(a)
            check(a)
            return type(a) == "table" and #a == 12
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn blu_luau_direct_table_iteration_handles_table_create() {
        let source = br#"
            local values = table.create(100, 0)
            local count = 0
            for key in values do
                count = count + 1
                values[key] = key
            end
            return type(values), count, values[1], values[100]
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap(),
            vec![
                Value::String(Arc::from(&b"table"[..])),
                Value::Integer(100),
                Value::Integer(1),
                Value::Integer(100),
            ]
        );
    }

    #[test]
    fn blu_luau_table_create_keeps_preallocated_length_boundary() {
        let source = br#"
            local values = table.create(5)
            local empty = #values
            values[5] = 5
            return empty, #values
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile:?}: {error}")),
                vec![Value::Integer(0), Value::Integer(5)],
                "{profile:?}"
            );
        }

        let sparse_source = br#"
            local values = table.create(10, 1)
            local filled = #values
            values[5] = nil
            local interior_hole = #values
            values[10] = nil
            local tail_hole = #values
            values[9] = nil
            values[8] = nil
            return filled, interior_hole, tail_hole, #values
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(sparse_source, profile)
                    .unwrap_or_else(|error| panic!("{profile:?}: {error}")),
                vec![
                    Value::Number(10.0),
                    Value::Number(10.0),
                    Value::Number(9.0),
                    Value::Number(7.0),
                ],
                "{profile:?}"
            );
        }
    }

    #[test]
    fn blu_and_luau_sparse_tail_filling_keeps_length_empty_until_index_one() {
        let source = br#"
            for index = 1, 10_000 do
                _G[index] = index
            end
            do
                local arr = table.create(5, 42)
                arr[1] = nil
                arr.a = "a"
                assert(#arr == 5)
            end
            do
                local arr = {}
                arr.a = "a"
                arr.a = nil
                arr[1] = 1
                assert(#arr == 1)
            end
            local values = {}
            local before_first = {}
            for index = 5, 2, -1 do
                values[index] = index
                before_first[#before_first + 1] = #values
            end
            values[1] = 1
            return before_first[1], before_first[2], before_first[3], before_first[4], #values
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile:?}: {error}")),
                vec![
                    Value::Integer(0),
                    Value::Integer(0),
                    Value::Integer(0),
                    Value::Integer(0),
                    Value::Integer(5),
                ],
                "{profile:?}"
            );
        }
    }

    #[test]
    fn blu_and_luau_sparse_array_bit32_writes_accept_empty_tables() {
        let source = br#"
            local function obscuredalloc()
                return {}
            end
            local bits = 16
            for i = 1, 1 do
                local values = obscuredalloc()
                for k = 1, bits do
                    values[k] = if bit32.extract(i, k - 1) == 1 then true else nil
                end
            end
            return true
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile:?}: {error}")),
                vec![Value::Boolean(true)],
                "{profile:?}"
            );
        }
    }

    #[test]
    fn blu_luau_table_clear_matches_preallocated_length_boundary() {
        let source = br#"
            local cleared = {}
            for index = 1, 16 do
                cleared[index] = index
            end
            table.clear(cleared)
            local created = table.create(16)
            for index = 1, 16 do
                cleared[index] = true
                created[index] = true
                assert(#cleared == #created)
            end
            return true
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile:?}: {error}")),
                vec![Value::Boolean(true)],
                "{profile:?}"
            );
        }
    }

    #[test]
    fn blu_luau_table_clone_preserves_preallocated_length_boundary() {
        let source = br#"
            local original = table.create(10, 1)
            original[5] = nil
            local clone = table.clone(original)
            return #original, #clone, clone[10], clone[5]
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile:?}: {error}")),
                vec![
                    Value::Number(10.0),
                    Value::Number(10.0),
                    Value::Integer(1),
                    Value::Nil,
                ],
                "{profile:?}"
            );
        }
    }

    #[test]
    fn blu_and_luau_table_clone_preserve_small_hash_iteration_order() {
        let source = br#"
            local function order(value)
                local result = ""
                for _, item in pairs(value) do
                    result = result .. tostring(item)
                end
                return result
            end
            local original = { a = 1, b = 2, c = 3, d = 4, e = 5, f = 6 }
            local clone = table.clone(original)
            return order(original) == order(clone)
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile:?}: {error}")),
                vec![Value::Boolean(true)],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn blu_luau_constant_table_hash_iteration_matches_luau_order() {
        let source = br#"
            local value = { foo = 1, bar = "string", thing = true }
            local keys = {}
            for key in value do
                keys[#keys + 1] = key
            end
            return table.concat(keys, ",")
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile:?}: {error}")),
                vec![Value::String(Arc::from(&b"thing,bar,foo"[..]))],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn blu_luau_table_find_scans_hash_numeric_start() {
        let source = br#"
            return table.find({ [(2)] = true }, true, 2)
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile:?}: {error}")),
                vec![Value::Integer(2)],
                "{profile:?}"
            );
        }
    }

    #[test]
    fn blu_luau_table_find_uses_equal_metamethods() {
        let source = br#"
            local meta = {
                __eq = function(left, right)
                    return left.value == right.value
                end,
            }
            local values = {
                setmetatable({ value = 1 }, meta),
                setmetatable({ value = 2 }, meta),
            }
            return table.find(values, setmetatable({ value = 2 }, meta))
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile:?}: {error}")),
                vec![Value::Integer(2)],
                "{profile:?}"
            );
        }
    }

    #[test]
    fn blu_luau_direct_table_iteration_yields_key_and_value() {
        let source = br#"
            local values = { 10, 20 }
            local keys = 0
            local total = 0
            for key, value in values do
                keys = keys + key
                total = total + value
            end
            return keys, total
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap(),
            vec![Value::Integer(3), Value::Integer(30)]
        );
    }

    #[test]
    fn official_luau_string_format_char_cluster_executes() {
        let source = br#"
            return string.format("%c", 34)
                    .. string.format("%c", 48)
                    .. string.format("%c", 90)
                    .. string.format("%c", 100)
                == string.format("%c%c%c%c", 34, 48, 90, 100)
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn standard_string_metatable_exposes_string_library_for_all_profiles() {
        let source = br#"
            local metatable = getmetatable("")
            return type(metatable), ("blu"):sub(2), metatable.__index.sub("blu", 2)
        "#;
        for profile in [
            SemanticProfile::Blu,
            SemanticProfile::Luau,
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile:?}: {error}")),
                vec![
                    Value::String(Arc::from(&b"table"[..])),
                    Value::String(Arc::from(&b"lu"[..])),
                    Value::String(Arc::from(&b"lu"[..])),
                ],
                "{profile:?}"
            );
        }
    }

    #[test]
    fn string_char_range_errors_use_lua_wording() {
        let result = Engine::default()
            .execute_owned_source(
                br#"local ok, message = pcall(string.char, 256); return ok, message"#,
                SemanticProfile::Lua54,
            )
            .expect("string.char range probe should execute");
        assert_eq!(result[0], Value::Boolean(false));
        let Value::String(message) = &result[1] else {
            panic!("expected string error, got {result:?}");
        };
        assert!(String::from_utf8_lossy(message).contains("out of range"));
    }

    #[test]
    fn lua54_strings_checkerror_messages_match_the_portable_fixture() {
        let result = Engine::default()
            .execute_owned_source(
                br#"
                    local function check(message, function_value, ...)
                        local ok, error = pcall(function_value, ...)
                        return not ok and string.find(error, message) ~= nil
                    end
                    local maxi, mini = math.maxinteger, math.mininteger
                    local aux = string.rep("0", 600)
                    return
                        check("out of range", string.char, 256),
                        check("out of range", string.char, -1),
                        check("out of range", string.char, maxi),
                        check("out of range", string.char, mini),
                        check("too large", string.rep, "aa", 1 << 30),
                        check("too large", string.rep, "a", 1 << 30, ","),
                        check("no literal", string.format, "%q", {}),
                        check("contains zeros", string.format, "%10s", "\0"),
                        check("'__tostring' must return a string", tostring,
                            setmetatable({}, { __tostring = function() return {} end })),
                        check("invalid conversion", string.format, "%100.3d", 10),
                        check("too long", string.format, "%1" .. string.rep("0", 600) .. ".3d", 10),
                        check("invalid conversion", string.format, "%1.100d", 10),
                        check("too long", string.format, "%10.1" .. aux .. "004d", 10),
                        check("invalid conversion", string.format, "%t", 10),
                        check("too long", string.format, "%" .. aux .. "d", 10),
                        check("no value", string.format, "%d %d", 10)
                        , check("invalid conversion", string.format, "%010c", 10)
                        , check("invalid conversion", string.format, "%.10c", 10)
                        , check("invalid conversion", string.format, "%0.34s", 10)
                        , check("invalid conversion", string.format, "%#i", 10)
                        , check("invalid conversion", string.format, "%3.1p", 10)
                        , check("invalid conversion", string.format, "%0.s", 10)
                        , check("cannot have modifiers", string.format, "%10q", 10)
                        , check("invalid conversion", string.format, "%F", 10)
                        , check("table expected", table.concat, 3)
                        , check("at index " .. maxi, table.concat, {}, " ", maxi, maxi)
                        , check("at index %" .. mini, table.concat, {}, " ", mini, mini)
                "#,
                SemanticProfile::Lua54,
            )
            .expect("portable string error probes should execute");
        assert_eq!(
            result,
            vec![Value::Boolean(true); 27],
            "portable strings.lua error wording drifted: {result:?}"
        );
    }

    #[test]
    fn lua54_quoted_strings_round_trip_through_load() {
        let result = Engine::default()
            .execute_owned_source(
                br#"
                    local value = "\0\1\0023\5\0009"
                    local encoded = string.format("%q", value)
                    local loaded = assert(load("return " .. encoded))()
                    return encoded, loaded == value
                "#,
                SemanticProfile::Lua54,
            )
            .expect("quoted string round-trip should execute");
        assert_eq!(
            result.get(1),
            Some(&Value::Boolean(true)),
            "quoted string did not round-trip: {result:?}"
        );
    }

    #[test]
    fn lua54_format_q_preserves_portable_literal_types() {
        let result = Engine::default()
            .execute_owned_source(
                br#"
                    local function check(value)
                        local encoded = string.format("%q", value)
                        local decoded = assert(load("return " .. encoded))()
                        return value == decoded and math.type(value) == math.type(decoded), encoded
                    end
                    local first, first_text = check("\0\0\1\255\u{234}")
                    local second, second_text = check(math.maxinteger)
                    local third, third_text = check(math.mininteger)
                    local fourth, fourth_text = check(math.pi)
                    local fifth, fifth_text = check(0.1)
                    local sixth, sixth_text = check(true)
                    local seventh, seventh_text = check(nil)
                    local eighth, eighth_text = check(false)
                    local ninth, ninth_text = check(math.huge)
                    local tenth, tenth_text = check(-math.huge)
                    return first, second, third, fourth, fifth,
                        sixth, seventh, eighth, ninth, tenth,
                        first_text, second_text, third_text, fourth_text, fifth_text,
                        sixth_text, seventh_text, eighth_text, ninth_text, tenth_text
                "#,
                SemanticProfile::Lua54,
            )
            .expect("format-q type probe should execute");
        assert_eq!(
            &result[0..10],
            &vec![Value::Boolean(true); 10],
            "format-q type drift: {result:?}"
        );
    }

    #[test]
    fn lua54_math_huge_is_available_to_quoted_literals() {
        let result = Engine::default()
            .execute_owned_source(
                br#"return math.huge, -math.huge, type(math.huge), math.pi"#,
                SemanticProfile::Lua54,
            )
            .expect("Lua 5.4 math constants should execute");
        assert_eq!(
            result,
            vec![
                Value::Number(f64::INFINITY),
                Value::Number(f64::NEG_INFINITY),
                Value::String(Arc::from(&b"number"[..])),
                Value::Number(core::f64::consts::PI),
            ]
        );
    }

    #[test]
    fn lua54_math_integer_conversion_diagnostics_match_puc() {
        let result = Engine::default()
            .execute_owned_source(
                br#"
                    local function run(value)
                        local ok, message = pcall(function() return value | value end)
                        return ok, message
                    end
                    local r1, m3 = run(math.huge)
                    local r2, m4 = run(0 / 0)
                    local function check(message, function_value, ...)
                        local ok, error = pcall(function_value, ...)
                        return not ok and string.find(error, message) ~= nil
                    end
                    local function checkcompiled(message, source)
                        local function_value = assert(load(source))
                        return check(message, function_value)
                    end
                    local function f2i(value) return value | value end
                    local field_function = assert(load("return math.huge << 1"))
                    local field_ok, field_message = pcall(field_function)
                    return checkcompiled("number.* has no integer representation", "return 2.3 >> 0"),
                        checkcompiled("number.* has no integer representation", "return 2.0^63 & 1"),
                        not field_ok and string.find(field_message, "field 'huge'") ~= nil,
                        checkcompiled("number.* has no integer representation", "return 1 | 2.0^63"),
                        checkcompiled("number.* has no integer representation", "return 2.3 ~ 0.0"),
                        not r1
                        and string.find(m3, "number.* has no integer representation") ~= nil,
                        not r2
                        and string.find(m4, "number.* has no integer representation") ~= nil,
                        check("number.* has no integer representation", f2i, -math.huge),
                        check("number.* has no integer representation", f2i, math.maxinteger + 0.0),
                        check("number expected", math.floor, {}),
                        check("number expected", math.ceil, print),
                        check("zero", math.fmod, 3, 0),
                        check("value expected", math.max),
                        check("value expected", math.min)
                "#,
                SemanticProfile::Lua54,
            )
            .expect("math conversion diagnostic probe should execute");
        assert!(
            result.iter().all(|value| value == &Value::Boolean(true)),
            "{result:?}"
        );
    }

    #[test]
    fn lua54_random_seed_matches_puc_xoshiro_sequence() {
        let result = Engine::default()
            .execute_owned_source(
                br#"
                    math.randomseed(1007)
                    local integer = math.random(0)
                    math.randomseed(1007, 0)
                    local fraction = math.random()
                    return integer, fraction
                "#,
                SemanticProfile::Lua54,
            )
            .expect("random sequence probe should execute");
        assert_eq!(
            result,
            vec![
                Value::Integer(8822622750169614806),
                Value::Number(0.4782753376376966),
            ]
        );
    }

    #[test]
    fn lua55_global_none_keeps_declared_locals_visible_in_nested_functions() {
        let result = Engine::default()
            .execute_owned_source(
                br#"
                    local math = require "math"
                    local string = require "string"
                    global none
                    global<const> print, assert, pcall, type, pairs, load
                    global<const> tonumber, tostring, select
                    local function checkerror(message, function_value, ...)
                        local ok, error = pcall(function_value, ...)
                        return not ok and string.find(error, message) ~= nil
                    end
                    return checkerror("number expected", math.floor, {})
                "#,
                SemanticProfile::Lua55,
            )
            .expect("Lua 5.5 global declaration probe should execute");
        assert_eq!(result, vec![Value::Boolean(true)]);
    }

    #[test]
    fn lua54_hexadecimal_format_uses_puc_zero_exponents() {
        let result = Engine::default()
            .execute_owned_source(
                br#"return string.format("%a", 0.0), string.format("%A", 0.0)"#,
                SemanticProfile::Lua54,
            )
            .expect("Lua hexadecimal format probe should execute");
        assert_eq!(
            result,
            vec![
                Value::String(Arc::from(&b"0x0p+0"[..])),
                Value::String(Arc::from(&b"0X0P+0"[..])),
            ],
            "hexadecimal format drift: {result:?}"
        );
    }

    #[test]
    fn lua54_format_flags_match_puc_width_and_zero_rules() {
        let result = Engine::default()
            .execute_owned_source(
                br#"
                    return string.format("%#12o", 10),
                        string.format("%#10x", 100),
                        string.format("%#-17X", 100),
                        string.format("%013i", -100),
                        string.format("%2.5d", -100),
                        string.format("%.u", 0),
                        string.format("%+#014.0f", 100),
                        string.format("%-16c", 97),
                        string.format("%+.3G", 1.5),
                        string.format("%.0s", "alo"),
                        string.format("%.s", "alo")
                "#,
                SemanticProfile::Lua54,
            )
            .expect("Lua format flag probe should execute");
        assert_eq!(
            result,
            vec![
                Value::String(Arc::from(&b"         012"[..])),
                Value::String(Arc::from(&b"      0x64"[..])),
                Value::String(Arc::from(&b"0X64             "[..])),
                Value::String(Arc::from(&b"-000000000100"[..])),
                Value::String(Arc::from(&b"-00100"[..])),
                Value::String(Arc::from(&b""[..])),
                Value::String(Arc::from(&b"+000000000100."[..])),
                Value::String(Arc::from(&b"a               "[..])),
                Value::String(Arc::from(&b"+1.5"[..])),
                Value::String(Arc::from(&b""[..])),
                Value::String(Arc::from(&b""[..])),
            ],
            "format flag drift: {result:?}"
        );
    }

    #[test]
    fn luau_string_format_negative_integer_conversions_match_reference() {
        let source = br#"
            return string.format("%o %u %x %X", -1, -1, -1, -1)
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Luau)
                .unwrap(),
            vec![Value::String(Arc::from(
                &b"1777777777777777777777 18446744073709551615 ffffffffffffffff FFFFFFFFFFFFFFFF"[..],
            ))]
        );
    }

    #[test]
    fn luau_string_format_rejects_duplicate_modifiers() {
        assert_eq!(
            Engine::default()
                .execute_owned_source(
                    br#"return pcall(string.format, "%##################d", 1) == false"#,
                    SemanticProfile::Luau,
                )
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn luau_newproxy_supports_string_format_userdata_hooks() {
        let source = br#"
            local value = newproxy(true)
            getmetatable(value).__tostring = function() return "good" end
            return type(value), string.format("%*", value)
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Luau)
                .unwrap(),
            vec![
                Value::String(Arc::from(&b"userdata"[..])),
                Value::String(Arc::from(&b"good"[..])),
            ]
        );
    }

    #[test]
    fn luau_newproxy_numeric_index_hooks_receive_non_string_keys() {
        let source = br#"
            local value = newproxy(true)
            local metatable = getmetatable(value)
            metatable.__index = function(_, key) return rawget(metatable, key) or key * 2 end
            metatable.__newindex = function(_, key, assigned)
                rawset(metatable, key, assigned)
            end
            value[4] = 7
            return value[3], value[4]
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Luau)
                .unwrap(),
            vec![Value::Integer(6), Value::Integer(7)]
        );
    }

    #[test]
    fn owned_newindex_handlers_receive_nil_and_nan_keys_before_raw_key_validation() {
        let source = br#"
            local calls = 0
            local value = setmetatable({}, {
                __newindex = function(_, _, _) calls = calls + 1 end,
            })
            local nil_ok = pcall(function() value[nil] = 1 end)
            local nan_ok = pcall(function() value[0 / 0] = 2 end)
            return nil_ok, nan_ok, calls
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap(),
                vec![
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Integer(2)
                ],
                "{profile:?}"
            );
        }
    }

    #[test]
    fn luau_shared_equality_metamethods_override_identity_for_tables_and_userdata() {
        let source = br#"
            local table_value = setmetatable({}, { __eq = function() return false end })
            local userdata_value = newproxy(true)
            getmetatable(userdata_value).__eq = function() return false end
            return table_value == table_value, table_value ~= table_value,
                userdata_value == userdata_value, userdata_value ~= userdata_value
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Luau)
                .unwrap(),
            vec![
                Value::Boolean(false),
                Value::Boolean(true),
                Value::Boolean(false),
                Value::Boolean(true),
            ]
        );
    }

    #[test]
    fn length_metamethod_result_rules_follow_the_profile() {
        let source = br#"
            local value = setmetatable({}, { __len = function() return "length" end })
            local ok, message = pcall(function() return #value end)
            return ok, message
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Luau)
                .unwrap(),
            vec![
                Value::Boolean(false),
                Value::String(Arc::from(
                    &b"source.blu:2: '__len' must return a number"[..]
                )),
            ]
        );
        for profile in [
            SemanticProfile::Blu,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap(),
                vec![
                    Value::Boolean(true),
                    Value::String(Arc::from(&b"length"[..])),
                ],
                "{profile}"
            );
        }
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Lua51)
                .unwrap(),
            vec![Value::Boolean(true), Value::Number(0.0)]
        );
    }

    #[test]
    fn luau_tostring_requires_string_metamethod_results_with_profile_wording() {
        let source = br#"
            local value = setmetatable({}, { __tostring = function() return 1 end })
            local ok, message = pcall(tostring, value)
            return ok, message
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Luau)
                .unwrap(),
            vec![
                Value::Boolean(false),
                Value::String(Arc::from(&b"'__tostring' must return a string"[..])),
            ]
        );
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap(),
            vec![
                Value::Boolean(false),
                Value::String(Arc::from(
                    &b"tostring expected string from __tostring, received number"[..],
                )),
            ]
        );
    }

    #[test]
    fn official_luau_string_format_dynamic_string_cluster_executes() {
        let source = br#"
            local a = "1234567890"
            a = string.format("%*%*%*%*%*", a, a, a, a, a)
            a = string.format("%*%*%*%*%*", a, a, a, a, a)
            a = string.format("%*%*%*%*%*", a, a, a, a, a)
            return a == string.rep("1234567890", 125)
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn luau_string_format_quoted_carriage_return_matches_its_profile() {
        let source = br#"return string.format("%q", "\r")"#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Luau)
                .unwrap(),
            vec![Value::String(Arc::from(&b"\"\\r\""[..]))]
        );
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap(),
            vec![Value::String(Arc::from(&b"\"\\r\""[..]))]
        );
    }

    #[test]
    fn official_luau_utf8_ascii_index_cluster_matches_blu() {
        let source = br#"
            local s = "hello World"
            local l = utf8.len(s, 1, -1)
            local pi = utf8.offset(s, 1)
            local pi1 = utf8.offset(s, 2, pi)
            return l == 11 and pi == 1 and pi1 == 2
                and utf8.len(s, pi, -1) == l
                and utf8.len(s, pi1, -1) == l - 1
                and utf8.len(s, 1, pi) == 1
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn official_luau_math_decimal_literal_cluster_matches_blu() {
        let source = br#"
            local a,b,c = "2", " 3e0 ", " 10  "
            assert(a+b == 5 and -b == -3 and b+"2" == 5 and "10"-c == 0)
            assert(a == "2" and b == " 3e0 " and c == " 10  " and -c == -"  10 ")
            assert(c%a == 0 and a^b == 8)
            assert(1.1 == 1.+.1)
            return 100.0 == 1E2 and .01 == 1e-2
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn luau_large_decimal_literals_follow_the_number_only_model() {
        let source = b"return 10000000000000001 == 10000000000000000";
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Luau)
                .unwrap(),
            vec![Value::Boolean(true)]
        );
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap(),
            vec![Value::Boolean(false)]
        );
    }

    #[test]
    fn blu_luau_type_assertions_are_erased_runtime_noops() {
        let source = br#"
            local function mutate(value)
                value.answer = 42
            end
            local table = {}
            mutate(table :: any);
            (if true then table else table)["question"] = 7
            return table.answer, table.question
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile:?}: {error}")),
                vec![Value::Integer(42), Value::Integer(7)],
                "{profile:?}"
            );
        }
    }

    #[test]
    fn generic_for_custom_iterator_can_forward_next_state() {
        let source = br#"
            local function incnext(table, key)
                local next_key, value = next(table, key)
                if next_key ~= nil then table[next_key] = value + 1 end
                return next_key, value
            end
            local table = { answer = 1 }
            for _, _ in incnext, table do end
            return table.answer
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile:?}: {error}")),
                vec![Value::Integer(2)],
                "{profile:?}"
            );
        }
    }

    #[test]
    fn blu_setfenv_updates_function_global_lookup_for_next() {
        let source = br#"
            local function foo()
                local getfenv, setfenv, assert, next = getfenv, setfenv, assert, next
                local environment = { gl1 = 3 }
                setfenv(foo, environment)
                assert(getfenv(foo) == getfenv(1))
                assert(getfenv(foo) == environment)
                assert(print == nil and gl1 == 3)
                gl1 = nil
                gl = 1
                assert(environment.gl == 1 and next(environment, "gl") == nil)
                local total = 0
                for index = 1, 3 do
                    total = total + index
                end
                assert(total == 6)
            end
            foo()
            return true
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap_or_else(|error| panic!("Blu: {error}")),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn next_accepts_each_key_returned_by_the_same_table() {
        let source = br#"
            local __blu_native_assert = assert
            local __blu_assert_count = 0
            assert = function(value, ...)
                __blu_assert_count = __blu_assert_count + 1
                return __blu_native_assert(value, ...)
            end
            local function check(subject)
                local copy = {}
                _G.table.foreach(subject, function(key, value) copy[key] = value end)
                local key, value = next(subject)
                while key do
                    copy[key] = value
                    key, value = next(subject, key)
                end
                local count = 0
                for k, v in pairs(subject) do
                    assert(copy[k] == v)
                    count = count + 1
                end
                return count
            end
            return check{ 1, x = 1, y = 2, z = 3 }
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile:?}: {error}")),
                vec![Value::Integer(4)],
                "{profile:?}"
            );
        }
    }

    #[test]
    fn luau_tables_sparse_power_of_two_length_preserves_iteration_keys() {
        let source = br#"
            local function checknext(subject)
                local copy = {}
                table.foreach(subject, function(key, value) copy[key] = value end)
                for key, value in pairs(copy) do assert(subject[key] == value) end
                for key, value in pairs(subject) do assert(copy[key] == value) end
                copy = {}
                do
                    local key, value = next(subject)
                    while key do
                        copy[key] = value
                        key, value = next(subject, key)
                    end
                end
                for key, value in pairs(copy) do assert(subject[key] == value) end
                for key, value in pairs(subject) do assert(copy[key] == value) end
            end
            checknext{1, x = 1, y = 2, z = 3}
            checknext{1, 2, x = 1, y = 2, z = 3}
            checknext{1, 2, 3, x = 1, y = 2, z = 3}
            checknext{1, 2, 3, 4, x = 1, y = 2, z = 3}
            checknext{1, 2, 3, 4, 5, x = 1, y = 2, z = 3}
            assert(table.getn{} == 0)
            assert(table.getn{[-1] = 2} == 0)
            assert(table.getn{1, 2, 3, nil, nil} == 3)
            for i = 0, 40 do
                local values = {}
                for j = 1, i do values[j] = j end
                assert(table.getn(values) == i)
            end
            assert(table.maxn{} == 0)
            assert(table.maxn{[-100] = 1} == 0)
            assert(table.maxn{["1000"] = true} == 0)
            assert(table.maxn{["1000"] = true, [24.5] = 3} == 24.5)
            assert(table.maxn{[1000] = true} == 1000)
            local values = {[10] = 1, [20] = 2}
            values[20] = nil
            assert(table.maxn(values) == 10)
            values = {}
            for i = 0, 50 do values[math.pow(2, i)] = true end
            assert(values[table.getn(values)])
            return true
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile:?}: {error}")),
                vec![Value::Boolean(true)],
                "{profile:?}"
            );
        }
    }

    #[test]
    fn tables_can_delete_the_current_iteration_key_and_continue() {
        let source = br#"
            local subject = {
                [{ 1 }] = 1,
                [{ 2 }] = 2,
                [string.rep("x ", 4)] = 3,
                [100.3] = 4,
                [4] = 5,
            }
            local count = 0
            for key, value in pairs(subject) do
                count = count + 1
                assert(subject[key] == value)
                subject[key] = nil
                collectgarbage()
                assert(subject[key] == nil)
            end
            local remaining = next(subject)
            assert(remaining == nil)
            assert(not pcall(next, { [2] = true }, 1))
            return count, remaining
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile:?}: {error}")),
                vec![Value::Integer(5), Value::Nil],
                "{profile:?}"
            );
        }
    }

    #[test]
    fn blu_luau_table_insert_keeps_out_of_range_positions_as_raw_keys() {
        let source = br#"
            local values = { 1, 2, 3 }
            table.insert(values, 0, 0)
            table.insert(values, 10, 10)
            table.insert(values, -1000000000, 42)
            local nonfinite = { 1, 2, 3 }
            local nan_ok = pcall(table.insert, nonfinite, 0 / 0, 99)
            local infinity_ok = pcall(table.insert, nonfinite, math.huge, 99)
            local negative_infinity_ok = pcall(table.insert, nonfinite, -math.huge, 99)
            local too_many_ok = pcall(table.insert, { 1 }, 1, 2, 3)
            local expected = true
            if _VERSION == "Blu" or _VERSION == "Luau" then
                expected = nan_ok and infinity_ok and negative_infinity_ok
                    and not too_many_ok
                    and nonfinite[0] == nil
                    and nonfinite[1] == 1 and nonfinite[2] == 2 and nonfinite[3] == 3
            end
            return values[0] == 0
                and table.concat(values) == "123"
                and table.maxn(values) == 10
                and values[10] == 10
                and values[-1000000000] == 42
                and expected
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile:?}: {error}")),
                vec![Value::Boolean(true)],
                "{profile:?}"
            );
        }
    }

    #[test]
    fn blu_luau_local_type_annotations_are_runtime_transparent() {
        let source = br#"
            local answer: number = 40
            local label: string = "ok"
            local function choose(...: string?) return ... end
            local first = choose(label)
            return answer + 2, first
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            let mut engine = Engine::default();
            let values = engine
                .execute_owned_source(source, profile)
                .unwrap_or_else(|error| panic!("{profile}: {error}"));
            assert_eq!(values.len(), 2, "{profile}");
            assert_eq!(values[0], Value::Integer(42), "{profile}");
            assert_eq!(values[1], Value::String(Arc::from(&b"ok"[..])), "{profile}");
        }
    }

    #[test]
    fn blu_luau_balanced_type_annotations_are_runtime_transparent() {
        let source = br#"
            local record: { field: number, nested: { string } } = { field = 40 }
            local function read(value: Array<number>): (number)
                return value.field + 2
            end
            local callback: (number) -> string = nil
            return read(record), callback == nil
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            let values = Engine::default()
                .execute_owned_source(source, profile)
                .unwrap_or_else(|error| panic!("{profile}: {error}"));
            assert_eq!(
                values,
                vec![Value::Integer(42), Value::Boolean(true)],
                "{profile}"
            );
        }
    }

    #[test]
    fn luau_math_constants_match_reference_surface() {
        let source = br#"
            return math.nan ~= math.nan,
                math.tau == math.pi * 2,
                math.sqrt2 == math.sqrt(2),
                math.e == 2.718281828459045,
                math.phi == (1 + math.sqrt(5)) / 2,
                math.isnan(math.nan),
                math.isinf(math.huge) == true,
                math.isfinite(math.huge) == false
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap(),
                vec![
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                ],
                "{profile}"
            );
        }
    }

    #[test]
    fn luau_tostring_nan_uses_lowercase_spelling() {
        assert_eq!(
            Engine::default()
                .execute_owned_source(
                    br#"return tostring(math.pow(-2, 0.5))"#,
                    SemanticProfile::Luau,
                )
                .unwrap(),
            vec![Value::String(Arc::from(&b"nan"[..]))]
        );
    }

    #[test]
    fn tostring_special_numbers_match_lua_family_spelling() {
        let source = br#"return tostring(0/0), tostring(1/0), tostring(-1/0), tostring(-0.0)"#;
        for profile in [
            SemanticProfile::Blu,
            SemanticProfile::Luau,
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let negative_zero = if matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ) {
                "-0.0"
            } else {
                "-0"
            };
            let expected = vec![
                Value::String(Arc::from(&b"nan"[..])),
                Value::String(Arc::from(&b"inf"[..])),
                Value::String(Arc::from(&b"-inf"[..])),
                Value::String(Arc::from(negative_zero.as_bytes())),
            ];
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile}: {error}")),
                expected,
                "{profile}"
            );
        }
    }

    #[test]
    fn lua55_tostring_preserves_float_round_trips() {
        let result = Engine::default()
            .execute_owned_source(
                br#"
                    local values = {
                        1.0,
                        -0.0,
                        2 ^ 52 + 1,
                        2 ^ 53 - 1,
                        1.2345678901234567,
                    }
                    local output = {}
                    for index, value in ipairs(values) do
                        local spelling = tostring(value)
                        output[index] = tonumber(spelling) == value
                            and (index > 2 or string.find(spelling, "%.0$") ~= nil)
                            or spelling
                    end
                    return table.unpack(output)
                "#,
                SemanticProfile::Lua55,
            )
            .expect("Lua 5.5 float tostring probe should execute");
        assert_eq!(
            result,
            vec![
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
            ]
        );
    }

    #[test]
    fn loop_local_closures_get_distinct_capture_cells() {
        let source = br#"
            local functions = {}
            for index = 1, 3 do
                local value = 0
                functions[index] = function()
                    value = value + 1
                    return value
                end
            end
            return functions[1]() == 1
                and functions[1]() == 2
                and functions[2]() == 1
                and functions[3]() == 1
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile}: {error}")),
                vec![Value::Boolean(true)],
                "{profile}"
            );
        }
    }

    #[test]
    fn weak_table_values_can_be_collected_after_their_initializer_expires() {
        let source = br#"
            local weak = {[1] = {}}
            setmetatable(weak, { __mode = "v" })
            collectgarbage("collect")
            return weak[1] == nil
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile}: {error}")),
                vec![Value::Boolean(true)],
                "{profile}"
            );
        }
    }

    #[test]
    fn weak_table_keys_can_be_collected_after_their_initializer_expires() {
        let source = br#"
            local weak = {}
            setmetatable(weak, { __mode = "k" })
            local key = {}
            weak[key] = true
            key = nil
            collectgarbage("collect")
            return next(weak) == nil
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile}: {error}")),
                vec![Value::Boolean(true)],
                "{profile}"
            );
        }
    }

    #[test]
    fn weak_key_ephemerons_retain_reachable_chains_then_clear() {
        let source = br#"
            local weak = setmetatable({}, { __mode = "k" })
            local previous
            for index = 1, 100 do
                local key = {}
                weak[key] = { previous }
                previous = key
            end
            collectgarbage("collect")
            local count = 0
            local key = previous
            while key do
                local value = weak[key]
                if value == nil then return false end
                key = value[1]
                count = count + 1
            end
            previous = nil
            collectgarbage("collect")
            return count == 100 and next(weak) == nil
        "#;
        for profile in [
            SemanticProfile::Blu,
            SemanticProfile::Luau,
            SemanticProfile::Lua54,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile}: {error}")),
                vec![Value::Boolean(true)],
                "{profile}"
            );
        }
    }

    #[test]
    fn weak_table_mode_added_after_attachment_keeps_string_gc_cadence() {
        let source = br#"
            local weak = {}
            local metatable = {}
            setmetatable(weak, metatable)
            metatable.__mode = "v"
            local value = {}
            weak[1] = value
            value = nil
            for index = 1, 1000 do
                local garbage = index .. index
                if weak[1] == nil then
                    return true
                end
            end
            return false
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile}: {error}")),
                vec![Value::Boolean(true)],
                "{profile}"
            );
        }
    }

    #[test]
    fn numeric_for_closures_capture_iteration_local_when_accessed_through_table() {
        let source = br#"
            local a = {}
            for i = 1, 10 do
                a[i] = {
                    set = function(x) i = x end,
                    get = function() return i end,
                }
                if i == 3 then break end
            end
            a[1].set(10)
            return a[2].get() == 2,
                a[3].get() == 3,
                a[2].set("a"),
                a[2].get() == "a"
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile}: {error}")),
                vec![
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Nil,
                    Value::Boolean(true),
                ],
                "{profile}"
            );
        }
    }

    #[test]
    fn official_closure_prefix_preserves_numeric_for_capture_cells() {
        let source = br#"
            local unpack = table.unpack
            local A, B = 0, {g = 10}
            function f(x)
                local a = {}
                for i = 1, 1000 do
                    local y = 0
                    do
                        a[i] = function()
                            B.g = B.g + 1
                            y = y + x
                            return y + A
                        end
                    end
                end
                local dummy = function() return a[A] end
                collectgarbage()
                A = 1
                assert(dummy() == a[1])
                A = 0
                assert(a[1]() == x)
                assert(a[3]() == x)
                collectgarbage()
                assert(B.g == 12)
                return a
            end
            a = f(10)
            local x = {[1] = {}}
            setmetatable(x, {__mode = "kv"})
            while x[1] do
                local garbage = A .. A .. A .. A
                A = A + 1
            end
            assert(a[1]() == 20 + A)
            assert(a[1]() == 30 + A)
            assert(a[2]() == 10 + A)
            collectgarbage()
            assert(a[2]() == 20 + A)
            assert(a[2]() == 30 + A)
            assert(a[3]() == 20 + A)
            assert(a[8]() == 10 + A)
            assert(getmetatable(x).__mode == "kv")
            assert(B.g == 19)
            a = {}
            for i = 1, 10 do
                a[i] = {
                    set = function(value) i = value end,
                    get = function() return i end,
                }
                if i == 3 then break end
            end
            a[1].set(10)
            return a[2].get() == 2, a[3].get() == 3
        "#;
        let mut engine = Engine::default();
        let result = engine.execute_owned_source(source, SemanticProfile::Blu);
        assert_eq!(
            result.unwrap_or_else(|error| {
                panic!("Blu: {error}; output {:?}", engine.vm_mut().take_output())
            }),
            vec![Value::Boolean(true), Value::Boolean(true)]
        );
    }

    #[test]
    fn official_closure_coroutine_resume_preserves_arguments_after_environment_rebinding() {
        let source = br#"
            local __native_assert = assert
            local __assert_count = 0
            assert = function(value, ...)
                __assert_count = __assert_count + 1
                if not value then error("assert #" .. tostring(__assert_count), 0) end
                return __native_assert(value, ...)
            end
            local f
            local _G = getfenv()
            local function foo(a)
                setfenv(0, a)
                coroutine.yield(getfenv())
                assert(getfenv(0) == a)
                assert(getfenv(1) == _G)
                assert(getfenv(loadstring"") == a)
                return getfenv()
            end
            f = coroutine.wrap(foo)
            local a = {}
            assert(f(a) == _G)
            local a, b = pcall(f)
            assert(a and b == _G)
            _G.x = nil
            function foo(a, ...)
                assert(coroutine.running() == f)
                assert(coroutine.status(f) == "running")
                local arg = {...}
                for i = 1, table.getn(arg) do
                    _G.x = {coroutine.yield(unpack(arg[i]))}
                end
                return unpack(a)
            end
            f = coroutine.create(foo)
            local s, a, b, c, d
            s, a, b, c, d = coroutine.resume(f, {1, 2, 3}, {}, {1}, {"a", "b", "c"})
            assert(s and a == nil and coroutine.status(f) == "suspended")
            s, a, b, c, d = coroutine.resume(f)
            local x1, x2, x3 = _G.x[1], a, b
            s, a, b, c, d = coroutine.resume(f, 1, 2, 3)
            return x1 == nil, x2 == 1, x3 == nil, s, a, b, c, d
        "#;
        let mut engine = Engine::default();
        let result = engine.execute_owned_source(source, SemanticProfile::Blu);
        assert_eq!(
            result.unwrap_or_else(|error| {
                panic!("Blu: {error}; output {:?}", engine.vm_mut().take_output())
            }),
            vec![
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::String(Arc::from(&b"a"[..])),
                Value::String(Arc::from(&b"b"[..])),
                Value::String(Arc::from(&b"c"[..])),
                Value::Nil,
            ]
        );
    }

    #[test]
    fn official_closure_tail_vararg_call_closes_captured_local() {
        let source = br#"
            local function t()
                local function c(a, b)
                    assert(a == "test" and b == "OK")
                end
                local function v(f, ...)
                    c("test", f() ~= 1 and "FAILED" or "OK")
                end
                local x = 1
                return v(function() return x end)
            end
            t()
            return true
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile}: {error}")),
                vec![Value::Boolean(true)],
                "{profile}"
            );
        }
    }

    #[test]
    fn official_closure_coroutine_errors_preserve_wrapped_and_threaded_state() {
        let source = br#"
            function foo()
                coroutine.yield(3)
                error("foo")
            end
            function goo() foo() end
            local wrapped = coroutine.wrap(goo)
            local first = wrapped()
            local ok, wrapped_error = pcall(wrapped)
            local thread = coroutine.create(goo)
            local resumed, value = coroutine.resume(thread)
            local failed, thread_error = coroutine.resume(thread)
            local dead, dead_error = coroutine.resume(thread)
            return first == 3,
                not ok and type(wrapped_error) == "string",
                resumed and value == 3,
                not failed and type(thread_error) == "string",
                not dead and string.find(dead_error, "dead") ~= nil,
                coroutine.status(thread) == "dead"
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile}: {error}")),
                vec![Value::Boolean(true); 6],
                "{profile}"
            );
        }
    }

    #[test]
    fn luau_wrapped_coroutine_errors_keep_only_the_origin_prefix() {
        let result = Engine::default()
            .execute_owned_source_named(
                br#"
                    local function foo()
                        coroutine.yield()
                        error("foo")
                    end
                    local wrapped = coroutine.wrap(foo)
                    wrapped()
                    local ok, message = pcall(wrapped)
                    return ok, message
                "#,
                "closure.luau",
                SemanticProfile::Luau,
            )
            .unwrap_or_else(|error| panic!("{error}"));
        let Value::String(message) = &result[1] else {
            panic!("expected wrapped coroutine error string, got {result:?}");
        };
        assert!(
            !message
                .windows(b": closure.luau:".len())
                .any(|window| window == b": closure.luau:")
        );
        assert!(message.ends_with(b": foo"));
        assert_eq!(result[0], Value::Boolean(false));
    }

    #[test]
    fn luau_wrapped_dead_coroutine_resume_keeps_the_callsite_prefix() {
        let result = Engine::default()
            .execute_owned_source_named(
                br#"
                    local function weird()
                        coroutine.yield(weird)
                        weird()
                    end
                    local ok, message = pcall(function()
                        for _ in coroutine.wrap(pcall), weird do end
                    end)
                    return ok, message
                "#,
                "pcall.luau",
                SemanticProfile::Luau,
            )
            .unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(
            result,
            vec![
                Value::Boolean(false),
                Value::String(Arc::from(
                    &b"pcall.luau:7: cannot resume dead coroutine"[..]
                )),
            ]
        );
    }

    #[test]
    fn owned_execution_preserves_explicit_chunk_names_in_errors() {
        let error = Engine::default()
            .execute_owned_source_named("error(\"boom\")", "fixture.luau", SemanticProfile::Blu)
            .expect_err("the fixture must fail");
        assert!(error.to_string().contains("fixture.luau"));
    }

    #[test]
    fn blu_pcall_error_prefix_matches_luau_named_chunks() {
        let source = br#"local ok, value = pcall(function() error("oops") end); return value"#;
        let result = Engine::default()
            .execute_owned_source_named(source, "basic.luau", SemanticProfile::Blu)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            result,
            vec![Value::String(Arc::from(&b"basic.luau:1: oops"[..]))]
        );
    }

    #[test]
    fn luau_pcall_error_prefix_matches_lua_source_calls() {
        let source = br#"local ok, value = pcall(function() error("oops") end); return value"#;
        let result = Engine::default()
            .execute_owned_source_named(source, "basic.luau", SemanticProfile::Luau)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            result,
            vec![Value::String(Arc::from(&b"basic.luau:1: oops"[..]))]
        );
    }

    #[test]
    fn owned_assert_diagnostics_match_luau_fixture() {
        let source = br#"
            local function ecall(fn, ...)
                local ok, err = pcall(fn, ...)
                assert(not ok)
                return err:sub(err:find(": ") + 2, #err)
            end
            return ecall(function() assert() end),
                ecall(function() assert(nil) end),
                ecall(function() assert(false) end),
                ecall(function() assert(nil, "epic fail") end)
        "#;
        let expected = vec![
            Value::String(Arc::from(&b"missing argument #1"[..])),
            Value::String(Arc::from(&b"assertion failed!"[..])),
            Value::String(Arc::from(&b"assertion failed!"[..])),
            Value::String(Arc::from(&b"epic fail"[..])),
        ];
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source_named(source, "assert.luau", profile)
                    .unwrap_or_else(|error| panic!("{profile}: {error}")),
                expected,
                "{profile} profile"
            );
        }
    }

    #[test]
    fn luau_concatenation_diagnostics_match_reference() {
        let source = br#"
            local ok, message = pcall(function()
                return "1" .. nil .. "2"
            end)
            return ok, message:sub(message:find(": ") + 2)
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source_named(source, "basic.luau", SemanticProfile::Luau)
                .unwrap_or_else(|error| panic!("{error}")),
            vec![
                Value::Boolean(false),
                Value::String(Arc::from(&b"attempt to concatenate nil with string"[..])),
            ]
        );
    }

    #[test]
    fn balanced_quote_patterns_match_lua() {
        let source = br#"
            local first, start, finish = string.find("alo 'oi' alo", "%b''")
            local result, count = string.gsub("alo 'oi' alo", "%b''", '"')
            return first, start, finish, result, count
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            let result = Engine::default()
                .execute_owned_source_named(source, "pm.luau", profile)
                .unwrap_or_else(|error| panic!("{profile}: {error}"));
            let numeric = |value| {
                if profile == SemanticProfile::Blu {
                    Value::Integer(value)
                } else {
                    Value::Number(value as f64)
                }
            };
            assert_eq!(
                result,
                vec![
                    numeric(5),
                    numeric(8),
                    Value::Nil,
                    Value::String(Arc::from(&b"alo \" alo"[..])),
                    numeric(1),
                ],
                "{profile} profile"
            );
        }
    }

    #[test]
    fn owned_getfenv_iterator_diagnostics_match_luau() {
        let source = br#"
            function testgetfenv()
                local env = getfenv(1)
                env.pairs = function() return "nope" end
                env.ipairs = function() return "nope" end
                env.next = "next"
                local ok1, err1 = pcall(function() for k, v in pairs({}) do end end)
                local ok2, err2 = pcall(function() for k, v in ipairs({}) do end end)
                local ok3, err3 = pcall(function() for k, v in next, {} do end end)
                return ok1, err1, ok2, err2, ok3, err3
            end
            return testgetfenv()
        "#;
        let result = Engine::default()
            .execute_owned_source_named(source, "iter_fenv.luau", SemanticProfile::Luau)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            result,
            vec![
                Value::Boolean(false),
                Value::String(Arc::from(&b"attempt to iterate over a string value"[..])),
                Value::Boolean(false),
                Value::String(Arc::from(&b"attempt to iterate over a string value"[..])),
                Value::Boolean(false),
                Value::String(Arc::from(&b"attempt to iterate over a string value"[..])),
            ]
        );
    }

    #[test]
    fn owned_tmerror_diagnostics_match_luau() {
        let source = br#"
            local testtable = {}
            setmetatable(testtable, {
                __index = function()
                    error("Error")
                end
            })
            local status1, result1 = pcall(function()
                testtable.missingmethod()
            end)

            local testtable2 = {}
            setmetatable(testtable2, {
                __index = function()
                    local status, result = pcall(function()
                        error("Error")
                    end)
                    return nil
                end
            })
            local m2 = testtable2.missingmethod
            local status2, result2 = pcall(function()
                testtable2.missingmethod()
            end)
            return status1, result1, status2, result2
        "#;
        let result = Engine::default()
            .execute_owned_source_named(source, "tmerror.luau", SemanticProfile::Luau)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            result,
            vec![
                Value::Boolean(false),
                Value::String(Arc::from(&b"tmerror.luau:5: Error"[..])),
                Value::Boolean(false),
                Value::String(Arc::from(
                    &b"tmerror.luau:23: attempt to call a nil value"[..]
                )),
            ]
        );
    }

    #[test]
    fn luau_core_type_errors_match_guest_wording() {
        let source = br#"
            local function ecall(fn)
                local ok, message = pcall(fn)
                assert(not ok)
                return message:sub((message:find(": ") or -1) + 2)
            end
            return ecall(function() return nil + 5 end),
                ecall(function() return 1 > nil end),
                ecall(function() for i = 1, "a" do end end),
                ecall(function() ({}):foo() end),
                ecall(function() (42):foo() end)
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Luau)
                .unwrap(),
            vec![
                Value::String(Arc::from(
                    &b"attempt to perform arithmetic (add) on nil and number"[..]
                )),
                Value::String(Arc::from(&b"attempt to compare nil < number"[..])),
                Value::String(Arc::from(
                    &b"invalid 'for' limit (number expected, got string)"[..]
                )),
                Value::String(Arc::from(
                    &b"attempt to call missing method 'foo' of table"[..]
                )),
                Value::String(Arc::from(&b"attempt to index number with 'foo'"[..])),
            ]
        );
    }

    #[test]
    fn luau_native_protected_error_keeps_the_raw_string_message() {
        let source =
            br#"local ok, nested_ok, value = pcall(pcall, error, "oops"); return nested_ok, value"#;
        let result = Engine::default()
            .execute_owned_source_named(source, "basic.luau", SemanticProfile::Luau)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            result,
            vec![
                Value::Boolean(false),
                Value::String(Arc::from(&b"oops"[..])),
            ]
        );
    }

    #[test]
    fn owned_number_rendering_matches_profile_precision() {
        let source = br#"return tostring(1 / 3)"#;
        let legacy = Value::String(Arc::from(&b"0.33333333333333"[..]));
        let exact = Value::String(Arc::from(&b"0.3333333333333333"[..]));
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile}: {error}")),
                vec![legacy.clone()],
                "{profile}"
            );
        }
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile}: {error}")),
                vec![exact.clone()],
                "{profile}"
            );
        }
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Lua55)
                .unwrap(),
            vec![Value::String(Arc::from(&b"0.33333333333333331"[..]))],
            "lua55"
        );
    }

    #[test]
    fn weak_table_values_are_collected_across_a_loop_back_edge() {
        let source = br#"
            local weak = {[1] = {}}
            setmetatable(weak, { __mode = "v" })
            local attempts = 0
            while weak[1] do
                attempts = attempts + 1
                if attempts > 4 then
                    return false
                end
                local garbage = {}
                collectgarbage("collect")
            end
            return true
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile}: {error}")),
                vec![Value::Boolean(true)],
                "{profile}"
            );
        }
    }

    #[test]
    fn weak_table_values_are_cleared_during_automatic_collection() {
        let source = br#"
            local weak = {[1] = {}}
            setmetatable(weak, { __mode = "v" })
            for index = 1, 1000 do
                local garbage = index .. index
                if weak[1] == nil then
                    return true
                end
            end
            return false
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile}: {error}")),
                vec![Value::Boolean(true)],
                "{profile}"
            );
        }
    }

    #[test]
    fn coroutine_multiple_yield_arguments_survive_loop_capture_closure_changes() {
        let source = br#"
            _G.x = nil
            function foo(a, ...)
                local arg = {...}
                for i = 1, table.getn(arg) do
                    _G.x = { coroutine.yield(unpack(arg[i])) }
                end
                return unpack(a)
            end
            f = coroutine.create(foo)
            local s1, a1, b1 = coroutine.resume(f, {1, 2, 3}, {}, {1}, {"a", "b", "c"})
            local s2, a2, b2 = coroutine.resume(f)
            return s1, a1, b1, s2, a2, b2, coroutine.status(f)
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile}: {error}")),
                vec![
                    Value::Boolean(true),
                    Value::Nil,
                    Value::Nil,
                    Value::Boolean(true),
                    Value::Integer(1),
                    Value::Nil,
                    Value::String(Arc::from(&b"suspended"[..])),
                ],
                "{profile}"
            );
        }
    }

    #[test]
    fn wrapped_coroutines_suspend_and_resume_across_environment_changes() {
        let source = br#"
            local _G = getfenv()
            local function foo(a)
                setfenv(0, a)
                coroutine.yield(getfenv())
                assert(getfenv(0) == a)
                assert(getfenv(1) == _G)
                assert(getfenv(loadstring "") == a)
                return getfenv()
            end
            f = coroutine.wrap(foo)
            local a = {}
            local first = f(a)
            local ok, second = pcall(f)
            return first == _G and ok and second == _G
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile}: {error}")),
                vec![Value::Boolean(true)],
                "{profile}"
            );
        }
    }

    #[test]
    fn gsub_table_index_yield_is_rejected_at_the_native_boundary() {
        let source = br#"
            local thread = coroutine.create(function()
                local replacements = setmetatable({}, {
                    __index = function()
                        coroutine.yield("gsub index pause")
                        return "replacement"
                    end,
                })
                return string.gsub("a", "a", replacements)
            end)
            local ok = coroutine.resume(thread)
            return not ok
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile}: {error}")),
                vec![Value::Boolean(true)],
                "{profile}"
            );
        }
    }

    #[test]
    fn wrapped_tail_yield_can_fail_after_resume() {
        let source = br#"
            local function foo()
                coroutine.yield(3)
                error("foo")
            end
            local function goo()
                foo()
            end
            local wrapped = coroutine.wrap(goo)
            local first = wrapped()
            local ok = pcall(wrapped)
            return first == 3 and not ok
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile}: {error}")),
                vec![Value::Boolean(true)],
                "{profile}"
            );
        }
    }

    #[test]
    fn luau_math_fmod_preserves_negative_divisor_signs() {
        let source = br#"
            local function noinline(value, ...)
                local ok, result = pcall(function(item) return item end, value)
                return result
            end
            return math.fmod(3, 2) == 1,
                math.fmod(-3, 2) == -1,
                math.fmod(3, -2) == 1,
                math.fmod(-3, -2) == -1,
                math.fmod(noinline(3), 2) == 1,
                math.fmod(noinline(-3), 2) == -1,
                math.fmod(noinline(3), -2) == 1,
                math.fmod(noinline(-3), -2) == -1
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Luau)
                .unwrap(),
            vec![Value::Boolean(true); 8]
        );
    }

    #[test]
    fn luau_nan_table_reads_are_nil_but_writes_are_rejected() {
        let source = br#"
            local nan = 0 / 0
            local values = {}
            local first_write_ok = pcall(function() values[nan] = 1 end)
            local first_read = values[nan]
            values[1] = 1
            local second_write_ok = pcall(function() values[nan] = 1 end)
            local second_read = values[nan]
            return not first_write_ok and first_read == nil
                and not second_write_ok and second_read == nil
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Luau)
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn luau_math_min_max_integer_arguments_preserve_ordering() {
        let source = br#"
            return math.min(1, 2), math.max(1, 2),
                math.min(1, 2) == 1, math.max(1, 2) == 2
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Luau)
                .unwrap(),
            vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Boolean(true),
                Value::Boolean(true),
            ]
        );
    }

    #[test]
    fn luau_math_abs_rejects_missing_and_empty_call_arguments() {
        let source = br#"
            local function nothing() end
            local missing = pcall(math.abs)
            local empty = pcall(function() return math.abs(nothing()) end)
            return missing, empty
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Luau)
                .unwrap(),
            vec![Value::Boolean(false), Value::Boolean(false)]
        );
    }

    #[test]
    fn luau_math_random_argument_count_uses_reference_error_wording() {
        let source = br#"
            local ok, message = pcall(math.random, 1, 2, 3)
            return not ok and string.find(message, "wrong number of arguments") ~= nil
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Luau)
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn official_luau_tpack_platform_sizes_are_explicitly_pinned() {
        let source = br#"
            return string.packsize("h"), string.packsize("i"), string.packsize("l"),
                string.packsize("T"), string.packsize("j"), string.packsize("f"),
                string.packsize("d"), string.packsize("n"), string.packsize("!xXi16")
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap(),
            vec![
                Value::Integer(2),
                Value::Integer(4),
                Value::Integer(8),
                Value::Integer(8),
                Value::Integer(8),
                Value::Integer(4),
                Value::Integer(8),
                Value::Integer(8),
                Value::Integer(8),
            ]
        );
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
    fn owned_source_entry_point_executes_the_baseline_for_every_profile() {
        for profile in SemanticProfile::ALL {
            let result = Engine::default()
                .execute_owned_source(b"return 40 + 2", profile)
                .unwrap();
            let expected = if matches!(
                profile,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                Value::Integer(42)
            } else {
                Value::Number(42.0)
            };
            assert_eq!(result, vec![expected], "{profile}");
        }

        assert_eq!(
            Engine::default()
                .execute_owned_source(b"--!dialect lua54\nreturn 40 + 2", SemanticProfile::Lua54,)
                .unwrap(),
            vec![Value::Integer(42)]
        );
    }

    #[test]
    fn owned_load_returns_a_stateful_closure_in_the_requested_environment() {
        let source = br#"
            answer = 39
            local default_loaded = load("answer = answer + 1; return answer")
            local default_result = default_loaded()
            local environment = { answer = 40 }
            local loaded = load("answer = answer + 1; return answer", "chunk", "t", environment)
            local first = loaded()
            local second = loaded()
            return default_result, answer, first, second, environment.answer
        "#;
        for profile in [
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let result = Engine::default()
                .execute_owned_source(source, profile)
                .unwrap();
            let default = if matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ) {
                Value::Integer(40)
            } else {
                Value::Number(40.0)
            };
            let expected = if matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ) {
                Value::Integer(41)
            } else {
                Value::Number(41.0)
            };
            let second = if matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ) {
                Value::Integer(42)
            } else {
                Value::Number(42.0)
            };
            assert_eq!(
                result,
                vec![default.clone(), default, expected, second.clone(), second],
                "{profile}"
            );
        }
    }

    #[test]
    fn owned_load_accepts_chunked_reader_functions_for_every_lua_profile() {
        let source = br#"
            local chunks = { "return 40", " + 2" }
            local index = 0
            local loaded, message = load(function()
                index = index + 1
                return chunks[index]
            end)
            local empty_chunks = { "return 7", "", " + 2" }
            local empty_index = 0
            local empty_loaded = load(function()
                empty_index = empty_index + 1
                return empty_chunks[empty_index]
            end)
            return loaded ~= nil
                and message == nil
                and loaded() == 42
                and index == 3
                and empty_loaded() == 7
                and empty_index == 2
        "#;
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let values = Engine::default()
                .execute_owned_source(source, profile)
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(values, vec![Value::Boolean(true)], "profile {profile:?}");
        }
    }

    #[test]
    fn owned_load_reader_can_yield_and_resume_for_every_lua_profile() {
        let source = br#"
            local thread = coroutine.create(function()
                local reads = 0
                local loaded, message = load(function()
                    reads = reads + 1
                    if reads == 1 then
                        coroutine.yield("reader pause")
                        return "return 42"
                    end
                    return ""
                end)
                return loaded()
            end)
            local first, signal = coroutine.resume(thread)
            local second, result = coroutine.resume(thread)
            return first and signal == "reader pause"
                and second and result
                and coroutine.status(thread) == "dead"
        "#;
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let values = Engine::default()
                .execute_owned_source(source, profile)
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(values, vec![Value::Boolean(true)], "profile {profile:?}");
        }
    }

    #[test]
    fn owned_load_modes_and_lua51_loadstring_follow_profile_signatures() {
        let modern = Engine::default()
            .execute_owned_source(
                r#"
                    local binary, message = load("return 1", "chunk", "b")
                    local text = load("return 42", "chunk", "t")
                    return binary == nil and type(message) == "string" and text() == 42
                "#,
                SemanticProfile::Lua54,
            )
            .unwrap();
        assert_eq!(modern, vec![Value::Boolean(true)]);

        let mode_errors = Engine::default()
            .execute_owned_source(
                r#"
                    local empty, empty_message = load("return 1", "chunk", "")
                    local unknown, unknown_message = load("return 1", "chunk", "x")
                    return empty == nil
                        and empty_message == "attempt to load a text chunk (mode is '')"
                        and unknown == nil
                        and unknown_message == "attempt to load a text chunk (mode is 'x')"
                "#,
                SemanticProfile::Lua55,
            )
            .unwrap();
        assert_eq!(mode_errors, vec![Value::Boolean(true)]);

        let lua51 = Engine::default()
            .execute_owned_source(
                r#"
                    local ok = pcall(load, "return 1")
                    local loaded = loadstring("return 42")
                    return not ok and loaded() == 42
                "#,
                SemanticProfile::Lua51,
            )
            .unwrap();
        assert_eq!(lua51, vec![Value::Boolean(true)]);
    }

    #[test]
    fn owned_blu_and_luau_source_entry_points_expose_loadstring() {
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            let values = Engine::default()
                .execute_owned_source(
                    r#"
                        local loaded = loadstring("return 42")
                        return type(loadstring), loaded(), type(table.pack),
                            type(table.unpack), type(unpack), type(table.getn), type(exit)
                    "#,
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(
                values,
                vec![
                    Value::String(Arc::from(&b"function"[..])),
                    Value::Integer(42),
                    Value::String(Arc::from(&b"function"[..])),
                    Value::String(Arc::from(&b"function"[..])),
                    Value::String(Arc::from(&b"function"[..])),
                    Value::String(Arc::from(&b"function"[..])),
                    Value::String(Arc::from(&b"nil"[..])),
                ],
                "{profile:?}"
            );
        }
    }

    #[test]
    fn owned_loadstring_supports_recursive_generated_chunks() {
        let values = Engine::default()
            .execute_owned_source(
                r#"
                    function fat(x)
                        if x <= 1 then return 1
                        else return x * loadstring("return fat(" .. x - 1 .. ")")()
                        end
                    end
                    return fat(6)
                "#,
                SemanticProfile::Blu,
            )
            .unwrap();
        assert_eq!(values, vec![Value::Integer(720)]);
    }

    #[test]
    fn owned_loadstring_cooperates_with_gsub_numeric_capture_callbacks() {
        let source = br#"
            local function f1(s, p)
                p = string.gsub(p, "%%([0-9])", function(s) return "%" .. (s + 1) end)
                p = string.gsub(p, "^(^?)", "%1()")
                p = string.gsub(p, "($?)$", "()%1")
                local t = {string.match(s, p)}
                return string.sub(s, t[1], t[#t] - 1)
            end
            local first = f1("alo alx 123 b\0o b\0o", "(..*) %1")
            local function dostring(s) return loadstring(s)() or "" end
            local second, count = string.gsub("alo $a=1$ novamente $return a$", "$([^$]*)%$", dostring)
            return first, second, count
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            let values = Engine::default()
                .execute_owned_source(source, profile)
                .unwrap_or_else(|error| panic!("{profile:?}: {error}"));
            assert_eq!(
                values,
                vec![
                    Value::String(Arc::from(&b"b\0o b\0o"[..])),
                    Value::String(Arc::from(&b"alo  novamente 1"[..])),
                    Value::Integer(2),
                ],
                "{profile:?}"
            );
        }
    }

    #[test]
    fn owned_gsub_callbacks_receive_numeric_captures_as_strings() {
        let source = br#"
            local seen
            local result = string.gsub("%1", "%%([0-9])", function(s)
                seen = s
                return "%" .. s
            end)
            return seen, result
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap(),
            vec![
                Value::String(Arc::from(&b"1"[..])),
                Value::String(Arc::from(&b"%1"[..])),
            ]
        );
    }

    #[test]
    fn owned_gsub_numeric_capture_arithmetic_uses_the_active_profile() {
        let source = br#"
            return string.gsub("%1", "%%([0-9])", function(s)
                return "%" .. (s + 1)
            end)
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap(),
            vec![Value::String(Arc::from(&b"%2"[..])), Value::Integer(1)]
        );
    }

    #[test]
    fn owned_gmatch_preserves_final_zero_width_captures() {
        let source = br#"
            local positions = {}
            for position in string.gmatch("abcde", "()") do
                positions[#positions + 1] = position
            end
            local text = {}
            for value in string.gmatch("ba", "a*") do
                text[#text + 1] = value
            end
            return #positions, positions[1], positions[6],
                #text, text[1], text[2], text[3]
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            let values = Engine::default()
                .execute_owned_source(source, profile)
                .unwrap_or_else(|error| panic!("{profile:?}: {error}"));
            let number = |value| {
                if profile == SemanticProfile::Blu {
                    Value::Integer(value)
                } else {
                    Value::Number(value as f64)
                }
            };
            assert_eq!(
                values,
                vec![
                    Value::Number(6.0),
                    number(1),
                    number(6),
                    Value::Number(if profile == SemanticProfile::Luau {
                        3.0
                    } else {
                        2.0
                    }),
                    Value::String(Arc::from(&b""[..])),
                    Value::String(Arc::from(&b"a"[..])),
                    if profile == SemanticProfile::Luau {
                        Value::String(Arc::from(&b""[..]))
                    } else {
                        Value::Nil
                    },
                ],
                "{profile:?}"
            );
        }
    }

    #[test]
    fn owned_malformed_pattern_messages_match_lua_substrings() {
        let source = br#"
            local function malformed(pattern, expected)
                local ok, message = pcall(string.find, "a", pattern)
                return not ok and string.find(message, expected) ~= nil
            end
            return malformed("(.", "unfinished capture"),
                malformed(").", "invalid pattern capture"),
                malformed("[a", "malformed"),
                malformed("[]", "malformed"),
                malformed("[^]", "malformed"),
                malformed("[a%]", "malformed"),
                malformed("[a%", "malformed"),
                malformed("%b", "malformed"),
                malformed("%ba", "malformed"),
                malformed("%", "malformed"),
                malformed("%f", "missing")
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile:?}: {error}")),
                vec![Value::Boolean(true); 11],
                "{profile:?}"
            );
        }
    }

    #[test]
    fn owned_pattern_capture_renumbering_supports_position_captures() {
        let source = br#"
            local function f1(s, p)
                p = string.gsub(p, "%%([0-9])", function(s) return "%" .. (s + 1) end)
                p = string.gsub(p, "^(^?)", "%1()")
                p = string.gsub(p, "($?)$", "()%1")
                local t = {string.match(s, p)}
                return string.sub(s, t[1], t[#t] - 1)
            end
            return f1("alo alx 123 b\0o b\0o", "(..*) %1")
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap(),
            vec![Value::String(Arc::from(&b"b\0o b\0o"[..]))]
        );
    }

    #[test]
    fn blu_loadstring_main_chunk_varargs_are_callable() {
        let values = Engine::default()
            .execute_owned_source(
                r#"
                    local loaded, load_error = loadstring [[ return {...} ]]
                    local values = loaded(2, 3)
                    return type(loaded), load_error, values[1], values[2], values[3]
                "#,
                SemanticProfile::Blu,
            )
            .unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(
            values,
            vec![
                Value::String(Arc::from(&b"function"[..])),
                Value::Nil,
                Value::Integer(2),
                Value::Integer(3),
                Value::Nil,
            ]
        );
    }

    #[test]
    fn owned_load_accepts_blu_binary_chunks_under_binary_mode() {
        let profile = SemanticProfile::Lua54;
        let compilation = compile_owned_source(
            b"return 42",
            "binary-chunk",
            profile,
            frontend::OwnedCompileLimits::default(),
        )
        .unwrap();
        let artifact = compilation.into_validated_artifact();
        let binary = Arc::<[u8]>::from(
            bytecode::blu::encode(&artifact, bytecode::blu::BluLimits::default()).unwrap(),
        );
        let mut engine = Engine::default();
        let binary_source = Arc::clone(&binary);
        let binary_function = engine
            .vm_mut()
            .try_register_function(move |_, _| Ok(vec![Value::String(Arc::clone(&binary_source))]))
            .unwrap();
        engine
            .vm_mut()
            .try_set_global(
                &b"binary_source"[..],
                Value::NativeFunction(binary_function),
            )
            .unwrap();
        let values = engine
            .execute_owned_source(
                br#"
                    local loaded = load(binary_source(), "chunk", "b")
                    local text, message = load(binary_source(), "chunk", "t")
                    return loaded() == 42
                        and text == nil
                        and message == "attempt to load a binary chunk (mode is 't')"
                "#,
                profile,
            )
            .unwrap();
        assert_eq!(values, vec![Value::Boolean(true)]);
    }

    #[test]
    fn owned_load_uses_global_environment_for_blu_and_luau_profiles() {
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            let values = Engine::default()
                .execute_owned_source(
                    br#"
                        local loaded = load("return 42")
                        return loaded()
                    "#,
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(values, vec![Value::Number(42.0)], "profile {profile:?}");
        }
    }

    #[test]
    fn utf8_library_is_profile_gated_and_handles_valid_and_invalid_bytes() {
        let source = br#"
            local text = utf8.char(65, 233, 0x1F600)
            local first, second, third = utf8.codepoint(text, 1, #text)
            local invalid, position = utf8.len("\255")
            local surrogate = utf8.char(0xD800)
            local surrogate_codepoint = 0
            if _VERSION == "Lua 5.3" or _VERSION == "Blu" then
                surrogate_codepoint = utf8.codepoint(surrogate)
            end
            local surrogate_length, surrogate_position = utf8.len(surrogate)
            local valid_surrogate = pcall(utf8.char, 0xD800)
            return utf8.len(text) == 3
                and first == 65
                and second == 233
                and third == 0x1F600
                and type(utf8.charpattern) == "string"
                and invalid == nil
                and position == 1
                and #surrogate == 3
                and ((_VERSION == "Lua 5.3" or _VERSION == "Blu")
                    and surrogate_codepoint == 0xD800
                    and surrogate_length == 1
                    or (_VERSION == "Lua 5.4" or _VERSION == "Lua 5.5")
                    and surrogate_length == nil
                    and surrogate_position == 1)
                and valid_surrogate
        "#;
        for profile in [
            SemanticProfile::Blu,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let values = Engine::default()
                .execute_owned_source(source, profile)
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(values, vec![Value::Boolean(true)], "profile {profile:?}");
        }
        for profile in [SemanticProfile::Lua51, SemanticProfile::Lua52] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source("return type(utf8)", profile)
                    .unwrap(),
                vec![Value::String(Arc::from(&b"nil"[..]))],
                "profile {profile:?}"
            );
        }
        assert!(
            Engine::default()
                .execute_owned_source("return utf8.codepoint(\"\\255\")", SemanticProfile::Blu)
                .is_err()
        );
        assert!(
            Engine::default()
                .execute_owned_source("return utf8.char(0x110000)", SemanticProfile::Blu)
                .is_err()
        );
        assert_eq!(
            Engine::default()
                .execute_owned_source("return utf8.char(0xD800)", SemanticProfile::Blu,)
                .unwrap(),
            vec![Value::String(Arc::from(&[0xED, 0xA0, 0x80][..]))]
        );
    }

    #[test]
    fn luau_utf8_decoding_rejects_surrogate_codepoints_but_char_preserves_bytes() {
        assert_eq!(
            Engine::default()
                .execute_owned_source("return pcall(utf8.char, 0xD800)", SemanticProfile::Luau,)
                .unwrap(),
            vec![
                Value::Boolean(true),
                Value::String(Arc::from(&[0xED, 0xA0, 0x80][..]))
            ]
        );
        assert_eq!(
            Engine::default()
                .execute_owned_source(
                    br#"
                        local surrogate = utf8.char(0xD800)
                        local length = utf8.len(surrogate)
                        local code_ok = pcall(utf8.codepoint, surrogate)
                        local iterator, state, control = utf8.codes(surrogate)
                        local iter_ok = pcall(iterator, state, control)
                        return length == nil and not code_ok and not iter_ok
                    "#,
                    SemanticProfile::Luau,
                )
                .unwrap(),
            vec![Value::Boolean(true)]
        );
        assert_eq!(
            Engine::default()
                .execute_owned_source(
                    br#"
                        local surrogate = utf8.char(0xD800)
                        local codepoint = utf8.codepoint(surrogate)
                        local iterator, state, control = utf8.codes(surrogate)
                        local position, value = iterator(state, control)
                        return utf8.len(surrogate) == 1
                            and codepoint == 0xD800
                            and position == 1 and value == 0xD800
                    "#,
                    SemanticProfile::Blu,
                )
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn utf8_offset_tracks_character_boundaries_and_lua55_end_positions() {
        let source = br#"
            local text = "A" .. utf8.char(233) .. utf8.char(0x1F600) .. "Z"
            local first = utf8.offset(text, 1)
            local second = utf8.offset(text, 2)
            local inside = utf8.offset(text, 0, 3)
            local previous, previous_end = utf8.offset(text, -1)
            return first == 1
                and second == 2
                and inside == 2
                and previous == 8
                and ((_VERSION == "Lua 5.5" and previous_end == 8)
                    or (_VERSION ~= "Lua 5.5" and previous_end == nil))
        "#;
        for profile in [
            SemanticProfile::Blu,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap(),
                vec![Value::Boolean(true)],
                "profile {profile:?}"
            );
        }
        assert!(
            Engine::default()
                .execute_owned_source(
                    "return utf8.offset(\"\\195\\169\", 1, 2)",
                    SemanticProfile::Blu,
                )
                .is_err()
        );
    }

    #[test]
    fn utf8_codes_returns_a_bounded_stateful_iterator() {
        let source = br#"
            local text = "A" .. utf8.char(233) .. utf8.char(0x1F600)
            local iterator, state, control = utf8.codes(text)
            local first_position, first_codepoint = iterator(state, control)
            local second_position, second_codepoint = iterator(state, first_position)
            local third_position, third_codepoint = iterator(state, second_position)
            local finished = iterator(state, third_position)
            return type(iterator) == "function"
                and state == text
                and control == 0
                and first_position == 1
                and first_codepoint == 65
                and second_position == 2
                and second_codepoint == 233
                and third_position == 4
                and third_codepoint == 0x1F600
                and finished == nil
        "#;
        for profile in [
            SemanticProfile::Blu,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap(),
                vec![Value::Boolean(true)],
                "profile {profile:?}"
            );
        }

        let mut engine = Engine::default();
        engine
            .vm_mut()
            .set_native_library_loader(|vm, library, symbol| {
                assert_eq!(library, b"trusted.so");
                assert_eq!(symbol, b"luaopen_trusted");
                let function = vm.register_function(|_, _| Ok(vec![Value::Integer(42)]));
                Ok(Value::NativeFunction(function))
            });
        assert_eq!(
            engine
                .execute_owned_source(
                    r#"
                        local loaded = package.loadlib("trusted.so", "luaopen_trusted")
                        return loaded()
                    "#,
                    SemanticProfile::Lua54,
                )
                .unwrap(),
            vec![Value::Integer(42)]
        );
    }

    #[test]
    fn warn_is_profile_gated_and_uses_a_separate_warning_channel() {
        for profile in [
            SemanticProfile::Blu,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let mut engine = Engine::default();
            assert_eq!(
                engine
                    .execute_owned_source(
                        r#"
                            warn("@on")
                            warn("alpha", "beta")
                            warn("@off")
                            warn("ignored")
                            return type(warn)
                        "#,
                        profile,
                    )
                    .unwrap(),
                vec![Value::String(Arc::from(&b"function"[..]))],
                "profile {profile:?}"
            );
            assert_eq!(engine.vm_mut().take_warnings(), b"Lua warning: alphabeta\n");
        }
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
        ] {
            let mut engine = Engine::default();
            assert_eq!(
                engine
                    .execute_owned_source("return type(warn)", profile)
                    .unwrap(),
                vec![Value::String(Arc::from(&b"nil"[..]))],
                "profile {profile:?}"
            );
            assert!(engine.vm_mut().take_warnings().is_empty());
        }
    }

    #[test]
    fn public_owned_load_api_returns_a_callable_environment_bound_chunk() {
        let mut engine = Engine::default();
        let environment = engine.vm_mut().default_environment().unwrap();
        let loaded = engine
            .load_owned_source(
                "answer = 40 + 2; return answer",
                SemanticProfile::Lua52,
                environment,
            )
            .unwrap();
        engine.vm_mut().set_global(&b"loaded"[..], loaded);
        assert_eq!(
            engine
                .execute_owned_source("return loaded()", SemanticProfile::Lua52)
                .unwrap(),
            vec![Value::Number(42.0)]
        );
    }

    #[test]
    fn lua51_loadstring_and_function_environments_are_profile_compatible() {
        let source = br#"
            local environment = { answer = 40 }
            local function read()
                return answer
            end
            setfenv(read, environment)
            local loaded = loadstring("answer = answer + 1; return answer")
            setfenv(loaded, environment)
            local first = loaded()
            local second = loaded()
            return first == 41 and second == 42 and environment.answer == 42
                and getfenv(read) == environment and getfenv(loaded) == environment
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Lua51)
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn lua51_current_stack_environment_can_be_rebound() {
        let source = br#"
            __blu_native_assert = assert
            __blu_assert_count = 0
            assert = function(value, ...)
                __blu_assert_count = __blu_assert_count + 1
                if not value then
                    error("assert #" .. tostring(__blu_assert_count), 0)
                end
                return __blu_native_assert(value, ...)
            end
            local get_environment = getfenv
            local set_environment = setfenv
            local load_source = loadstring
            local environment = { answer = 40, getfenv = get_environment }
            set_environment(0, environment)
            local loaded = load_source("answer = answer + 1; return answer")
            local first = loaded()
            local second = loaded()
            return first == 41 and second == 42
                and environment.answer == 42
                and get_environment() ~= environment
                and get_environment(0) == environment
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Lua51)
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn lua51_stack_environment_targets_live_closure_frames() {
        let source = br#"
            local get_environment = getfenv
            local set_environment = setfenv
            local base = get_environment(0)
            local function read()
                local before = get_environment(1)
                local caller = get_environment(2)
                local environment = { answer = 41 }
                set_environment(1, environment)
                local after = get_environment(1)
                return before == base
                    and caller == base
                    and after == environment
                    and answer == 41
            end
            return read()
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Lua51)
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn lua51_noncurrent_stack_environment_boundary_is_explicit() {
        let source = br#"
            local get_environment = getfenv
            local set_environment = setfenv
            local type_result = type
            local base = get_environment(0)
            local function outer()
                local function middle()
                    local caller = get_environment(3)
                    local set_ok, set_result = pcall(set_environment, 3, {})
                    return caller == base, set_ok, type_result(set_result) == "string"
                end
                return middle()
            end
            local ok, caller_ok, set_ok, set_error = pcall(outer)
            local result
            if ok then
                result = "outer-ok:" .. (caller_ok and "caller-ok" or "caller-error") .. ":"
                    .. (set_ok and "set-ok" or "set-error") .. ":"
                    .. (set_error and "error-string" or "set-value")
            else
                result = "outer-error:"
                    .. (type_result(caller_ok) == "string" and "error-string" or "other")
            end
            return result
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Lua51)
                .unwrap(),
            vec![Value::String(Arc::from(&b"outer-error:error-string"[..]))]
        );
    }

    #[test]
    fn legacy_environment_names_are_absent_from_modern_owned_profiles() {
        assert_eq!(
            Engine::default()
                .execute_owned_source(
                    "return type(getfenv), type(setfenv), type(loadstring)",
                    SemanticProfile::Lua52,
                )
                .unwrap(),
            vec![
                Value::String(Arc::from(&b"nil"[..])),
                Value::String(Arc::from(&b"nil"[..])),
                Value::String(Arc::from(&b"nil"[..])),
            ]
        );
    }

    #[test]
    fn blu_getfenv_nil_uses_the_current_environment() {
        let source = br#"
            local current = getfenv()
            local ok, explicit_nil = pcall(getfenv, nil)
            return ok and explicit_nil == current
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Lua51] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap(),
                vec![Value::Boolean(true)],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn blu_loadstring_tracks_thread_environment_rebinding() {
        let source = br#"
            f = nil
            local f
            x = 1
            a = nil
            loadstring('local a = {}')()
            assert(type(a) ~= 'table')
            local get_environment = getfenv
            local set_environment = setfenv
            local load_source = loadstring
            local environment = { loadstring = load_source }
            set_environment(0, environment)
            local loaded = load_source("local a = -3; a = a - 7; return a")
            local first_ok, first = pcall(loaded)
            local restored = get_environment()
            set_environment(0, restored)
            local second = load_source("local a = -3; a = a - 7; return a")
            local second_ok, second_value = pcall(second)
            local a
            local p = 4
            local loop_ok, loop_value = pcall(function()
                for i = 2, 31 do
                    for j = -3, 3 do
                        assert(load_source(string.format([[local a=%s;a=a+
                                            %s;
                                      assert(a
                                      ==2^%s)]], j, p - j, i)))()
                        assert(load_source(string.format([[local a=%s;
                                      a=a-%s;
                                      assert(a==-2^%s)]], -j, p - j, i)))()
                        assert(load_source(string.format([[local a,b=0,%s;
                                      a=b-%s;
                                      assert(a==-2^%s)]], -j, p - j, i)))()
                    end
                    p = 2 * p
                end
            end)
            return first_ok, first, second_ok, second_value,
                type(restored.string), get_environment() == restored, loop_ok, loop_value
        "#;
        let values = Engine::default()
            .execute_owned_source(source, SemanticProfile::Blu)
            .unwrap();
        assert_eq!(
            values,
            vec![
                Value::Boolean(true),
                Value::Integer(-10),
                Value::Boolean(true),
                Value::Integer(-10),
                Value::String(Arc::from(&b"table"[..])),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Nil,
            ]
        );
    }

    #[test]
    fn lua51_main_chunk_setfenv_rebinds_global_writes() {
        let source = br#"
            local f = function(t, i)
                error("cannot redefine global variable `" .. i .. "'", 2)
            end
            local g = {}
            local global_environment = getfenv()
            setmetatable(g, { __index = global_environment, __newindex = f })
            setfenv(1, g)
            rawset(g, "x", 3)
            x = 2
            y = 1
        "#;
        let error = Engine::default()
            .execute_owned_source(source, SemanticProfile::Lua51)
            .expect_err("Lua 5.1 main-chunk setfenv must route writes through __newindex");
        assert!(
            error
                .to_string()
                .contains("cannot redefine global variable `y'")
        );
    }

    #[test]
    fn lua51_rebound_global_newindex_can_yield_and_resume() {
        let values = Engine::default()
            .execute_owned_source(
                br#"
                    local function body()
                        local environment = {}
                        local received
                        setmetatable(environment, {
                            __index = getfenv(),
                            __newindex = function(_, key, value)
                                received = key .. ":" .. value
                                coroutine.yield("pause")
                            end,
                        })
                        setfenv(1, environment)
                        answer = 42
                        return received
                    end
                    local thread = coroutine.create(body)
                    local first, pause = coroutine.resume(thread)
                    local second, result = coroutine.resume(thread)
                    return first, pause, second, result
                "#,
                SemanticProfile::Lua51,
            )
            .expect("Lua 5.1 rebound global __newindex yield");
        assert_eq!(
            values,
            vec![
                Value::Boolean(true),
                Value::String(Arc::from(&b"pause"[..])),
                Value::Boolean(true),
                Value::String(Arc::from(&b"answer:42"[..])),
            ]
        );
    }

    #[test]
    fn luau_legacy_environment_compatibility_rebinds_closures_and_loadstring() {
        let source = br#"
            local get_environment = getfenv
            local set_environment = setfenv
            local load_source = loadstring
            local global_environment = get_environment()
            local functions = {}
            for i = 1, 3 do
                functions[i] = function()
                    A = A + 1
                    return A, global_environment.getfenv(1)
                end
            end
            A = 10
            local first = functions[1]() == 11
            set_environment(functions[2], { A = 20 })
            local second, second_environment = functions[2]()
            local rebound = second == 21 and second_environment.A == 21
            local environment = { loadstring = load_source, answer = 42 }
            set_environment(0, environment)
            local loaded = load_source("return answer")
            local loaded_environment = get_environment(loaded)
            local result = loaded() == 42 and loaded_environment == environment
            set_environment(0, global_environment)
            return first and rebound and result
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Luau)
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn blu_setfenv_returns_each_rebound_closure() {
        let values = Engine::default()
            .execute_owned_source(
                r#"
                    local functions = {}
                    local _G = getfenv()
                    for i = 1, 10 do
                        functions[i] = function(x)
                            A = A + 1
                            return A, _G.getfenv(x)
                        end
                    end
                    A = 10
                    return setfenv(functions[1], {}) == functions[1],
                        setfenv(functions[2], {}) == functions[2],
                        setfenv(functions[3], {}) == functions[3],
                        setfenv(functions[4], {}) == functions[4],
                        setfenv(functions[5], {}) == functions[5],
                        setfenv(functions[6], {}) == functions[6],
                        setfenv(functions[7], {}) == functions[7],
                        setfenv(functions[8], {}) == functions[8],
                        setfenv(functions[9], {}) == functions[9],
                        setfenv(functions[10], {}) == functions[10]
                "#,
                SemanticProfile::Blu,
            )
            .unwrap();
        assert_eq!(values, vec![Value::Boolean(true); 10]);
    }

    #[test]
    fn blu_setfenv_rebinds_the_live_caller_frame() {
        let source = br#"
            f = nil
            local f
            x = 1
            a = nil
            loadstring('local a = {}')()
            assert(type(a) ~= 'table')
            function f(a)
                local _1, _2, _3, _4, _5
                local _6, _7, _8, _9, _10
                local x = 3
                local b = a
                local c, d = a, b
                if d == b then
                    local x = "q"
                    x = b
                    assert(x == 2)
                else
                    assert(nil)
                end
                assert(x == 3)
                local f = 10
            end
            local b = 10
            local a
            repeat
                local b
                a, b = 1, 2
                assert(a + 1 == b)
            until a + b == 3
            assert(x == 1)
            f(2)
            assert(type(f) == "function")
            local f = {}
            local global_environment = getfenv()
            for i = 1, 10 do
                f[i] = function(x)
                    A = A + 1
                    return A, global_environment.getfenv(x)
                end
            end
            A = 10
            assert(f[1]() == 11)
            for i = 1, 10 do
                assert(setfenv(f[i], { A = i }) == f[i])
            end
            assert(f[3]() == 4 and A == 11)
            local a, b = f[8](1)
            assert(b.A == 9)
            a, b = f[8](0)
            assert(b.A == 11)
            local g
            local function f()
                assert(setfenv(2, { a = "10" }) == g)
            end
            g = function()
                f()
                global_environment.assert(global_environment.getfenv(1).a == "10")
            end
            g()
            assert(getfenv(g).a == "10")
            local function foo(s)
                return loadstring(s)
            end
            assert(getfenv(foo("")) == getfenv())
            local a = { loadstring = loadstring }
            setfenv(foo, a)
            assert(getfenv(foo("")) == getfenv())
            setfenv(0, a)
            assert(getfenv(foo("")) == a)
            setfenv(0, getfenv())
            local a
            local p = 4
            for i = 2, 31 do
                for j = -3, 3 do
                    assert(loadstring(string.format([[local a=%s;a=a+
                                        %s;
                                  assert(a
                                  ==2^%s)]], j, p - j, i)))()
                    assert(loadstring(string.format([[local a=%s;
                                  a=a-%s;
                                  assert(a==-2^%s)]], -j, p - j, i)))()
                    assert(loadstring(string.format([[local a,b=0,%s;
                                  a=b-%s;
                                  assert(a==-2^%s)]], -j, p - j, i)))()
                end
                p = 2 * p
            end
            return true
        "#;
        let values = Engine::default()
            .execute_owned_source(source, SemanticProfile::Blu)
            .unwrap();
        assert_eq!(values, vec![Value::Boolean(true)]);
    }

    #[test]
    fn owned_load_reports_source_errors_as_lua_style_second_results() {
        let values = Engine::default()
            .execute_owned_source(
                "local loaded, message = load(\"local =\"); return loaded == nil, type(message)",
                SemanticProfile::Lua52,
            )
            .unwrap();
        let luau_values = Engine::default()
            .execute_owned_source(
                "local loaded, message = loadstring('hello world'); return loaded, message",
                SemanticProfile::Luau,
            )
            .unwrap();
        assert_eq!(
            luau_values,
            vec![
                Value::Nil,
                Value::String(Arc::from(
                    &b"[string \"hello world\"]:1: Incomplete statement: expected assignment or a function call"[..]
                )),
            ]
        );
        assert_eq!(
            values,
            vec![
                Value::Boolean(true),
                Value::String(Arc::from(&b"string"[..])),
            ]
        );
    }

    #[test]
    fn owned_luau_method_errors_strip_the_internal_namecall_marker() {
        let values = Engine::default()
            .execute_owned_source(
                r#"
                    local function ecall(fn, ...)
                        local ok, err = pcall(fn, ...)
                        return err:sub((err:find(": ") or -1) + 2)
                    end
                    return ecall(function() ({ }):foo() end),
                        ecall(function() (""):foo() end),
                        ecall(function() (42):foo() end)
                "#,
                SemanticProfile::Luau,
            )
            .unwrap();
        assert_eq!(
            values,
            vec![
                Value::String(Arc::from(
                    &b"attempt to call missing method 'foo' of table"[..]
                )),
                Value::String(Arc::from(
                    &b"attempt to call missing method 'foo' of string"[..]
                )),
                Value::String(Arc::from(&b"attempt to index number with 'foo'"[..])),
            ]
        );
    }

    #[test]
    fn owned_luau_constant_hash_tables_follow_guest_iteration_order() {
        let values = Engine::default()
            .execute_owned_source(
                "local ordering = { foo = 1, bar = 'string', thing = true }; local result = ''; for key in ordering do result = result .. key end; return result",
                SemanticProfile::Luau,
            )
            .unwrap();
        assert_eq!(values, vec![Value::String(Arc::from(&b"thingbarfoo"[..]))]);
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
    fn package_capabilities_require_exact_host_grants() {
        let compiled = Compiler::default()
            .compile_bytecode(b"return 40 + 2")
            .unwrap();
        let manifest = Manifest {
            package: PackageIdentity {
                name: Name::new("example.capability").unwrap(),
                version: Version::new(1, 0, 0),
            },
            dialect: PackageDialect::Blu,
            bytecode: BytecodeDescriptor {
                format: BytecodeFormat::Luau,
                version: compiled.chunk.version,
                typeinfo_version: compiled.chunk.typeinfo_version,
            },
            authority: AuthorityRequirement {
                profile: AuthorityProfile::Confined,
                capabilities: vec![CapabilityRequirement {
                    name: Name::new("fs.read").unwrap(),
                    scope: b"workspace".to_vec(),
                }],
            },
            imports: Vec::new(),
            exports: Vec::new(),
        };
        let package = Package::new(manifest, compiled.bytes, PackageLimits::default()).unwrap();
        let encoded = package.encode();

        let missing = Package::decode(&encoded, PackageLimits::default()).unwrap();
        assert_eq!(
            Engine::default()
                .execute_package(missing, &HostPolicy::new(AuthorityProfile::Confined)),
            Err(ExecutePackageError::CapabilityNotGranted(
                CapabilityRequirement {
                    name: Name::new("fs.read").unwrap(),
                    scope: b"workspace".to_vec(),
                }
            ))
        );

        let wrong_scope = Package::decode(&encoded, PackageLimits::default()).unwrap();
        assert!(matches!(
            Engine::default().execute_package(
                wrong_scope,
                &HostPolicy::new(AuthorityProfile::Confined).with_capabilities([
                    CapabilityGrant::new(Name::new("fs.read").unwrap(), b"other".to_vec()),
                ])
            ),
            Err(ExecutePackageError::CapabilityNotGranted(_))
        ));

        let granted = Package::decode(&encoded, PackageLimits::default()).unwrap();
        assert_eq!(
            Engine::default().execute_package(
                granted,
                &HostPolicy::new(AuthorityProfile::Confined).with_capabilities([
                    CapabilityGrant::new(Name::new("fs.read").unwrap(), b"workspace".to_vec()),
                ])
            ),
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
    fn require_exposes_the_host_cache_through_package_loaded() {
        let mut engine = Engine::default();
        engine
            .vm_mut()
            .set_module_loader(|_, name| Ok(Value::String(Arc::from(name))));
        assert_eq!(
            engine
                .execute(
                    b"local value = require('answer'); return value == package.loaded.answer, type(package.loaded), type(package.preload)"
                )
                .unwrap(),
            vec![
                Value::Boolean(true),
                Value::String(Arc::from(&b"table"[..])),
                Value::String(Arc::from(&b"table"[..])),
            ]
        );
    }

    #[test]
    fn package_config_is_available_only_in_lua_profiles() {
        let source = b"return package == nil, package ~= nil and package.config";
        for profile in SemanticProfile::ALL {
            let result = Engine::default()
                .execute_owned_source(source, profile)
                .unwrap();
            let expected = match profile {
                SemanticProfile::Blu | SemanticProfile::Luau => {
                    vec![Value::Boolean(true), Value::Boolean(false)]
                }
                SemanticProfile::Lua51 => vec![
                    Value::Boolean(false),
                    Value::String(Arc::from(&b"/\n;\n?\n!\n-"[..])),
                ],
                SemanticProfile::Lua52
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55 => vec![
                    Value::Boolean(false),
                    Value::String(Arc::from(&b"/\n;\n?\n!\n-\n"[..])),
                ],
                _ => vec![Value::Boolean(true), Value::Boolean(false)],
            };
            assert_eq!(result, expected, "profile {profile:?}");
        }
    }

    #[test]
    fn coroutine_close_has_the_profile_correct_surface() {
        for profile in SemanticProfile::ALL {
            let result = Engine::default()
                .execute_owned_source(
                    "return type(coroutine.close), type(coroutine.isyieldable)",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            let close_expected = if matches!(
                profile,
                SemanticProfile::Blu
                    | SemanticProfile::Luau
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                b"function".as_slice()
            } else {
                b"nil".as_slice()
            };
            let isyieldable_expected = if matches!(
                profile,
                SemanticProfile::Blu
                    | SemanticProfile::Luau
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                b"function".as_slice()
            } else {
                b"nil".as_slice()
            };
            assert_eq!(
                result,
                vec![
                    Value::String(Arc::from(close_expected)),
                    Value::String(Arc::from(isyieldable_expected)),
                ],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn package_defaults_match_puc_unix_profiles_and_can_be_overridden() {
        if cfg!(windows) {
            return;
        }
        let expected = [
            (
                SemanticProfile::Lua51,
                b"./?.lua;/usr/local/share/lua/5.1/?.lua;/usr/local/share/lua/5.1/?/init.lua;/usr/local/lib/lua/5.1/?.lua;/usr/local/lib/lua/5.1/?/init.lua".as_slice(),
                b"./?.so;/usr/local/lib/lua/5.1/?.so;/usr/local/lib/lua/5.1/loadall.so".as_slice(),
            ),
            (
                SemanticProfile::Lua52,
                b"/usr/local/share/lua/5.2/?.lua;/usr/local/share/lua/5.2/?/init.lua;/usr/local/lib/lua/5.2/?.lua;/usr/local/lib/lua/5.2/?/init.lua;./?.lua".as_slice(),
                b"/usr/local/lib/lua/5.2/?.so;/usr/local/lib/lua/5.2/loadall.so;./?.so".as_slice(),
            ),
            (
                SemanticProfile::Lua53,
                b"/usr/local/share/lua/5.3/?.lua;/usr/local/share/lua/5.3/?/init.lua;/usr/local/lib/lua/5.3/?.lua;/usr/local/lib/lua/5.3/?/init.lua;./?.lua;./?/init.lua".as_slice(),
                b"/usr/local/lib/lua/5.3/?.so;/usr/local/lib/lua/5.3/loadall.so;./?.so".as_slice(),
            ),
            (
                SemanticProfile::Lua54,
                b"/usr/local/share/lua/5.4/?.lua;/usr/local/share/lua/5.4/?/init.lua;/usr/local/lib/lua/5.4/?.lua;/usr/local/lib/lua/5.4/?/init.lua;./?.lua;./?/init.lua".as_slice(),
                b"/usr/local/lib/lua/5.4/?.so;/usr/local/lib/lua/5.4/loadall.so;./?.so".as_slice(),
            ),
            (
                SemanticProfile::Lua55,
                b"/usr/local/share/lua/5.5/?.lua;/usr/local/share/lua/5.5/?/init.lua;/usr/local/lib/lua/5.5/?.lua;/usr/local/lib/lua/5.5/?/init.lua;./?.lua;./?/init.lua".as_slice(),
                b"/usr/local/lib/lua/5.5/?.so;/usr/local/lib/lua/5.5/loadall.so;./?.so".as_slice(),
            ),
        ];
        for (profile, path, cpath) in expected {
            let values = Engine::default()
                .execute_owned_source("return package.path, package.cpath", profile)
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(
                values,
                vec![
                    Value::String(Arc::from(path)),
                    Value::String(Arc::from(cpath)),
                ],
                "profile {profile:?}"
            );
            assert_eq!(
                Engine::default()
                    .execute_owned_source(
                        r#"
                            return rawget(package, "path") == package.path
                                and rawget(package, "cpath") == package.cpath
                                and rawget(package, "config") == package.config
                                and rawget(package, "loadlib") == package.loadlib
                                and ((_VERSION == "Lua 5.1" and rawget(package, "searchpath") == nil)
                                    or (_VERSION ~= "Lua 5.1" and rawget(package, "searchpath") == package.searchpath))
                        "#,
                        profile,
                    )
                    .unwrap(),
                vec![Value::Boolean(true)],
                "profile {profile:?} raw package fields"
            );
        }

        let mut engine = Engine::default();
        engine.vm_mut().set_package_path("./custom/?.lua").unwrap();
        engine.vm_mut().set_package_cpath("./custom/?.so").unwrap();
        assert_eq!(
            engine
                .execute_owned_source("return package.path, package.cpath", SemanticProfile::Lua54)
                .unwrap(),
            vec![
                Value::String(Arc::from(&b"./custom/?.lua"[..])),
                Value::String(Arc::from(&b"./custom/?.so"[..])),
            ]
        );
    }

    #[test]
    fn package_loadlib_is_present_but_requires_an_explicit_native_bridge() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let values = Engine::default()
                .execute_owned_source(
                    r#"
                        local loaded, message, where = package.loadlib("missing", "luaopen_missing")
                        return type(package.loadlib), loaded == nil, type(message), where
                    "#,
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(
                values,
                vec![
                    Value::String(Arc::from(&b"function"[..])),
                    Value::Boolean(true),
                    Value::String(Arc::from(&b"string"[..])),
                    Value::String(Arc::from(&b"absent"[..])),
                ],
                "profile {profile:?}"
            );
            assert_eq!(
                Engine::default()
                    .execute_owned_source(
                        r#"
                            local ok, message = pcall(io.read, "*a")
                            local lines_ok, lines_error = pcall(io.lines)
                            return ok, type(message), lines_ok, type(lines_error), type(io.lines)
                        "#,
                        profile,
                    )
                    .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}")),
                vec![
                    Value::Boolean(false),
                    Value::String(Arc::from(&b"string"[..])),
                    Value::Boolean(false),
                    Value::String(Arc::from(&b"string"[..])),
                    Value::String(Arc::from(&b"function"[..])),
                ],
                "profile {profile:?}"
            );
        }
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source("return type(package)", profile)
                    .unwrap(),
                vec![Value::String(Arc::from(&b"nil"[..]))],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn native_bridge_can_preserve_standard_loadlib_unavailable_results() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            for (failure, expected_where) in [
                (NativeLibraryFailure::Open, "open"),
                (NativeLibraryFailure::Absent, "absent"),
                (NativeLibraryFailure::Init, "init"),
            ] {
                let mut engine = Engine::default();
                engine
                    .vm_mut()
                    .set_native_library_loader_result(move |_, library, symbol| {
                        assert_eq!(library, b"trusted.so");
                        assert_eq!(symbol, b"luaopen_trusted");
                        Ok(NativeLibraryLoadResult::Unavailable {
                            message: b"trusted bridge unavailable".to_vec(),
                            where_: failure,
                        })
                    });
                let values = engine
                    .execute_owned_source(
                        r#"
                            local loaded, message, where = package.loadlib(
                                "trusted.so", "luaopen_trusted")
                            return loaded == nil, message, where
                        "#,
                        profile,
                    )
                    .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
                assert_eq!(
                    values,
                    vec![
                        Value::Boolean(true),
                        Value::String(Arc::from(&b"trusted bridge unavailable"[..])),
                        Value::String(Arc::from(expected_where.as_bytes())),
                    ],
                    "profile {profile:?}, failure {failure:?}"
                );
            }
        }
    }

    #[test]
    fn native_bridge_yielding_loader_is_rejected_at_the_native_boundary() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let mut engine = Engine::default();
            engine.vm_mut().set_native_library_loader_result(|_, _, _| {
                Err(RuntimeError::CoroutineYield(Vec::new()))
            });
            let error = engine
                .execute_owned_source(
                    r#"
                        local ok, message = pcall(package.loadlib,
                            "trusted.so", "luaopen_trusted")
                        return ok, type(message)
                    "#,
                    profile,
                )
                .expect_err("a native loader cannot yield through package.loadlib");
            assert!(
                matches!(
                    error,
                    OwnedExecuteError::Runtime(RuntimeError::CoroutineYieldOutside)
                ),
                "profile {profile:?}: {error:?}"
            );
        }
    }

    #[test]
    fn lua51_newproxy_allocates_guest_userdata_and_preserves_profile_presence() {
        let values = Engine::default()
            .execute_owned_source(
                r#"
                    local plain = newproxy()
                    local false_proxy = newproxy(false)
                    local shared = newproxy(true)
                    local shared_metatable = getmetatable(shared)
                    local copy = newproxy(shared)
                    local finalized = 0
                    shared_metatable.__gc = function() finalized = finalized + 1 end
                    local invalid_ok = pcall(newproxy, 1)
                    shared = nil
                    collectgarbage("collect")
                    return type(plain), getmetatable(plain) == nil,
                        type(false_proxy), getmetatable(false_proxy) == nil,
                        type(copy), getmetatable(copy) == shared_metatable,
                        finalized, invalid_ok
                "#,
                SemanticProfile::Lua51,
            )
            .unwrap();
        assert_eq!(
            values,
            vec![
                Value::String(Arc::from(&b"userdata"[..])),
                Value::Boolean(true),
                Value::String(Arc::from(&b"userdata"[..])),
                Value::Boolean(true),
                Value::String(Arc::from(&b"userdata"[..])),
                Value::Boolean(true),
                Value::Integer(1),
                Value::Boolean(false),
            ]
        );

        for profile in [
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let values = Engine::default()
                .execute_owned_source(
                    r#"
                        local ok, message = pcall(function() return newproxy() end)
                        return type(newproxy), type(_G.newproxy), ok, type(message)
                    "#,
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(
                values,
                vec![
                    Value::String(Arc::from(&b"nil"[..])),
                    Value::String(Arc::from(&b"nil"[..])),
                    Value::Boolean(false),
                    Value::String(Arc::from(&b"string"[..])),
                ],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn native_bridge_can_create_opaque_userdata_with_guest_finalization() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let mut engine = Engine::default();
            engine
                .vm_mut()
                .set_native_library_loader(|vm, library, symbol| {
                    assert_eq!(library, b"trusted.so");
                    assert_eq!(symbol, b"luaopen_trusted");
                    vm.create_userdata(b"trusted opaque userdata")
                });
            let result = engine
                .execute_owned_source(
                    r#"
                        local value = package.loadlib("trusted.so", "luaopen_trusted")
                        local finalized = 0
                        debug.setmetatable(value, {
                            __gc = function(userdata)
                                finalized = finalized + 1
                            end,
                        })
                        local kind = type(value)
                        value = nil
                        collectgarbage("collect")
                        return kind, finalized
                    "#,
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            let expected_count = if profile == SemanticProfile::Lua52 {
                Value::Number(1.0)
            } else {
                Value::Integer(1)
            };
            assert_eq!(
                result,
                vec![Value::String(Arc::from(&b"userdata"[..])), expected_count],
                "profile {profile:?}"
            );
        }
    }

    #[allow(clippy::single_element_loop)]
    #[test]
    fn os_library_is_profile_gated_and_environment_authorized() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(
                        "return type(os), os.difftime(9, 4) == 5, os.getenv(\"BLU_MISSING_ENV\") == nil",
                        profile,
                    )
                    .unwrap(),
                vec![
                    Value::String(Arc::from(&b"table"[..])),
                    Value::Boolean(true),
                    Value::Boolean(true),
                ],
                "profile {profile:?}"
            );
        }
        for profile in [SemanticProfile::Blu] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source("return type(os)", profile)
                    .unwrap(),
                vec![Value::String(Arc::from(&b"nil"[..]))],
                "profile {profile:?}"
            );
        }
        assert_eq!(
            Engine::default()
                .execute_owned_source(
                    "return type(os), type(os.clock), type(os.date), type(os.time), type(os.difftime), type(os.getenv), os.difftime(9, 4) == 5",
                    SemanticProfile::Luau,
                )
                .unwrap(),
            vec![
                Value::String(Arc::from(&b"table"[..])),
                Value::String(Arc::from(&b"function"[..])),
                Value::String(Arc::from(&b"function"[..])),
                Value::String(Arc::from(&b"function"[..])),
                Value::String(Arc::from(&b"function"[..])),
                Value::String(Arc::from(&b"nil"[..])),
                Value::Boolean(true),
            ]
        );

        let mut engine = Engine::default();
        engine.vm_mut().set_environment_getter(|name| {
            Ok((name == b"BLU_TEST_ENV").then(|| b"configured".to_vec()))
        });
        assert_eq!(
            engine
                .execute_owned_source(
                    "return os.getenv(\"BLU_TEST_ENV\"), os.getenv(\"BLU_MISSING_ENV\")",
                    SemanticProfile::Lua54,
                )
                .unwrap(),
            vec![Value::String(Arc::from(&b"configured"[..])), Value::Nil,]
        );
    }

    #[test]
    fn os_clock_is_profile_gated_and_host_authorized() {
        for profile in [
            SemanticProfile::Luau,
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(
                        "local ok, message = pcall(os.clock)\nreturn type(os.clock), ok, type(message)",
                        profile,
                    )
                    .unwrap(),
                vec![
                    Value::String(Arc::from(&b"function"[..])),
                    Value::Boolean(false),
                    Value::String(Arc::from(&b"string"[..])),
                ],
                "profile {profile:?}"
            );
        }

        let mut engine = Engine::default();
        engine.vm_mut().set_clock_getter(|| Ok(1.25));
        assert_eq!(
            engine
                .execute_owned_source(
                    "return type(os.clock), os.clock() == 1.25",
                    SemanticProfile::Lua54,
                )
                .unwrap(),
            vec![
                Value::String(Arc::from(&b"function"[..])),
                Value::Boolean(true),
            ]
        );

        let mut engine = Engine::default();
        engine.vm_mut().set_clock_getter(|| Ok(1.25));
        assert_eq!(
            engine
                .execute_owned_source(
                    "return type(os.clock), os.clock() == 1.25",
                    SemanticProfile::Luau,
                )
                .unwrap(),
            vec![
                Value::String(Arc::from(&b"function"[..])),
                Value::Boolean(true),
            ]
        );

        let mut engine = Engine::default();
        engine.vm_mut().set_clock_getter(|| Ok(-1.0));
        assert_eq!(
            engine
                .execute_owned_source(
                    "local ok, message = pcall(os.clock)\nreturn ok, type(message)",
                    SemanticProfile::Lua54,
                )
                .unwrap(),
            vec![
                Value::Boolean(false),
                Value::String(Arc::from(&b"string"[..]))
            ]
        );
    }

    #[allow(clippy::single_element_loop)]
    #[test]
    fn os_time_is_profile_gated_and_host_authorized() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(
                        "local ok, message = pcall(os.time)\nreturn type(os.time), ok, type(message)",
                        profile,
                    )
                    .unwrap(),
                vec![
                    Value::String(Arc::from(&b"function"[..])),
                    Value::Boolean(false),
                    Value::String(Arc::from(&b"string"[..])),
                ],
                "profile {profile:?}"
            );

            let mut engine = Engine::default();
            engine.vm_mut().set_time_getter(|| Ok(1_700_000_000));
            let result = engine
                .execute_owned_source("return type(os.time()), os.time() == 1700000000", profile)
                .unwrap();
            assert_eq!(
                result,
                vec![
                    Value::String(Arc::from(&b"number"[..])),
                    Value::Boolean(true),
                ],
                "profile {profile:?}"
            );
            assert_eq!(
                Engine::default()
                    .execute_owned_source(
                        "local ok, message = pcall(os.time, {})\nreturn ok, type(message)",
                        profile,
                    )
                    .unwrap(),
                vec![
                    Value::Boolean(false),
                    Value::String(Arc::from(&b"string"[..]))
                ],
                "profile {profile:?} calendar table"
            );
            engine.vm_mut().set_calendar_time_getter(|input| {
                if input
                    == (CalendarDateInput {
                        year: 2023,
                        month: 11,
                        day: 14,
                        hour: 22,
                        minute: 13,
                        second: 20,
                        is_dst: Some(false),
                    })
                {
                    Ok(1_700_000_000)
                } else if input
                    == (CalendarDateInput {
                        year: 2023,
                        month: 11,
                        day: 14,
                        hour: 12,
                        minute: 0,
                        second: 0,
                        is_dst: None,
                    })
                {
                    Ok(1_699_963_200)
                } else {
                    Err(RuntimeError::InvalidRange {
                        operation: "test os.time calendar request",
                    })
                }
            });
            assert_eq!(
                engine
                    .execute_owned_source(
                        "return os.time{year=2023, month=11, day=14, hour=22, min=13, sec=20, isdst=false} == 1700000000",
                        profile,
                    )
                    .unwrap(),
                vec![Value::Boolean(true)],
                "profile {profile:?} reverse calendar result"
            );
            assert_eq!(
                engine
                    .execute_owned_source(
                        "return os.time{year=2023, month=11, day=14} == 1699963200",
                        profile,
                    )
                    .unwrap(),
                vec![Value::Boolean(true)],
                "profile {profile:?} reverse calendar defaults"
            );
            engine.vm_mut().set_calendar_getter(|timestamp, utc| {
                if timestamp == Some(1_700_000_000) && utc {
                    Ok(CalendarDate {
                        year: 2023,
                        month: 11,
                        day: 14,
                        hour: 22,
                        minute: 13,
                        second: 20,
                        weekday: 3,
                        yearday: 318,
                        is_dst: false,
                    })
                } else {
                    Err(RuntimeError::InvalidRange {
                        operation: "test os.date calendar request",
                    })
                }
            });
            assert_eq!(
                engine
                    .execute_owned_source(
                        "local date = os.date('!*t', 1700000000)\nreturn date.year, date.month, date.day, date.hour, date.min, date.sec, date.wday, date.yday, date.isdst",
                        profile,
                    )
                    .unwrap(),
                vec![
                    Value::Integer(2023),
                    Value::Integer(11),
                    Value::Integer(14),
                    Value::Integer(22),
                    Value::Integer(13),
                    Value::Integer(20),
                    Value::Integer(3),
                    Value::Integer(318),
                    Value::Boolean(false),
                ],
                "profile {profile:?} calendar result"
            );
        }
        for profile in [SemanticProfile::Blu] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source("return type(os)", profile)
                    .unwrap(),
                vec![Value::String(Arc::from(&b"nil"[..]))],
                "profile {profile:?}"
            );
        }
    }

    #[allow(clippy::single_element_loop)]
    #[test]
    fn os_date_is_profile_gated_and_host_formatted() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(
                        "local ok, message = pcall(os.date)\nreturn type(os.date), ok, type(message)",
                        profile,
                    )
                    .unwrap(),
                vec![
                    Value::String(Arc::from(&b"function"[..])),
                    Value::Boolean(false),
                    Value::String(Arc::from(&b"string"[..])),
                ],
                "profile {profile:?}"
            );

            let mut engine = Engine::default();
            engine.vm_mut().set_date_getter(|format, timestamp| {
                if format == b"!%Y-%m-%d" && timestamp == Some(1_700_000_000) {
                    Ok(b"2023-11-14".to_vec())
                } else {
                    Err(RuntimeError::InvalidRange {
                        operation: "test os.date request",
                    })
                }
            });
            assert_eq!(
                engine
                    .execute_owned_source(
                        "return type(os.date), os.date('!%Y-%m-%d', 1700000000) == '2023-11-14'",
                        profile,
                    )
                    .unwrap(),
                vec![
                    Value::String(Arc::from(&b"function"[..])),
                    Value::Boolean(true),
                ],
                "profile {profile:?}"
            );
            assert_eq!(
                engine
                    .execute_owned_source(
                        "local ok, message = pcall(os.date, '*t')\nreturn ok, type(message)",
                        profile,
                    )
                    .unwrap(),
                vec![
                    Value::Boolean(false),
                    Value::String(Arc::from(&b"string"[..]))
                ],
                "profile {profile:?} calendar table"
            );
        }
        for profile in [SemanticProfile::Blu] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source("return type(os)", profile)
                    .unwrap(),
                vec![Value::String(Arc::from(&b"nil"[..]))],
                "profile {profile:?}"
            );
        }
    }

    #[allow(clippy::single_element_loop)]
    #[test]
    fn os_filesystem_mutation_is_profile_gated_and_host_authorized() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(
                        "local a, b = pcall(os.remove, 'answer.txt'), pcall(os.rename, 'a', 'b')\nreturn type(os.remove), type(os.rename), a, b",
                        profile,
                    )
                    .unwrap(),
                vec![
                    Value::String(Arc::from(&b"function"[..])),
                    Value::String(Arc::from(&b"function"[..])),
                    Value::Boolean(false),
                    Value::Boolean(false),
                ],
                "profile {profile:?} unavailable mutation"
            );

            let mut engine = Engine::default();
            engine.vm_mut().set_os_remove_getter(|path| {
                if path == b"answer.txt" {
                    Ok(())
                } else {
                    Err(RuntimeError::InvalidRange {
                        operation: "test os.remove path",
                    })
                }
            });
            engine.vm_mut().set_os_rename_getter(|from, to| {
                if from == b"old.txt" && to == b"new.txt" {
                    Ok(())
                } else {
                    Err(RuntimeError::InvalidRange {
                        operation: "test os.rename paths",
                    })
                }
            });
            assert_eq!(
                engine
                    .execute_owned_source(
                        "return os.remove('answer.txt'), os.rename('old.txt', 'new.txt')",
                        profile,
                    )
                    .unwrap(),
                vec![Value::Boolean(true), Value::Boolean(true)],
                "profile {profile:?} authorized mutation"
            );
        }
        for profile in [SemanticProfile::Blu] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source("return type(os)", profile)
                    .unwrap(),
                vec![Value::String(Arc::from(&b"nil"[..]))],
                "profile {profile:?}"
            );
        }
    }

    #[allow(clippy::single_element_loop)]
    #[test]
    fn os_execute_is_profile_gated_and_host_authorized() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(
                        "local ok, message = pcall(os.execute)\nreturn type(os.execute), ok, type(message)",
                        profile,
                    )
                    .unwrap(),
                vec![
                    Value::String(Arc::from(&b"function"[..])),
                    Value::Boolean(false),
                    Value::String(Arc::from(&b"string"[..])),
                ],
                "profile {profile:?} unavailable execute"
            );

            let mut engine = Engine::default();
            engine
                .vm_mut()
                .set_os_execute_getter(|command| match command {
                    None => Ok(OsExecuteResult::Availability(true)),
                    Some(b"true") => Ok(OsExecuteResult::Command {
                        success: true,
                        kind: b"exit".to_vec(),
                        code: 0,
                    }),
                    Some(_) => Err(RuntimeError::InvalidRange {
                        operation: "test os.execute command",
                    }),
                });
            assert_eq!(
                engine
                    .execute_owned_source(
                        "local available = os.execute()\nlocal status, kind, code = os.execute('true')\nif _VERSION == 'Lua 5.1' then return available == 1 and status == 0 and kind == nil and code == nil end\nreturn available == true and status == true and kind == 'exit' and code == 0",
                        profile,
                    )
                    .unwrap(),
                vec![Value::Boolean(true)],
                "profile {profile:?} authorized execute"
            );
        }
        for profile in [SemanticProfile::Blu] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source("return type(os)", profile)
                    .unwrap(),
                vec![Value::String(Arc::from(&b"nil"[..]))],
                "profile {profile:?}"
            );
        }
    }

    #[allow(clippy::single_element_loop)]
    #[test]
    fn os_exit_is_profile_gated_and_host_authorized() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(
                        "local ok, message = pcall(os.exit)\nreturn type(os.exit), ok, type(message)",
                        profile,
                    )
                    .unwrap(),
                vec![
                    Value::String(Arc::from(&b"function"[..])),
                    Value::Boolean(false),
                    Value::String(Arc::from(&b"string"[..])),
                ],
                "profile {profile:?} unavailable exit"
            );

            let mut engine = Engine::default();
            engine.vm_mut().set_os_exit_getter(move |request| {
                let expected = if profile == SemanticProfile::Lua51 {
                    OsExitRequest {
                        status: 7,
                        close: false,
                    }
                } else {
                    OsExitRequest {
                        status: 0,
                        close: true,
                    }
                };
                if request == expected {
                    Ok(())
                } else {
                    Err(RuntimeError::InvalidRange {
                        operation: "test os.exit request",
                    })
                }
            });
            assert_eq!(
                engine
                    .execute_owned_source(
                        "if _VERSION == 'Lua 5.1' then os.exit(7) else os.exit(true, true) end\nreturn true",
                        profile,
                    )
                    .unwrap(),
                vec![Value::Boolean(true)],
                "profile {profile:?} authorized exit"
            );
        }
        for profile in [SemanticProfile::Blu] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source("return type(os)", profile)
                    .unwrap(),
                vec![Value::String(Arc::from(&b"nil"[..]))],
                "profile {profile:?}"
            );
        }
    }

    #[allow(clippy::single_element_loop)]
    #[test]
    fn os_locale_and_tmpname_are_profile_gated_and_host_authorized() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(
                        "local a, b = pcall(os.setlocale), pcall(os.tmpname)\nreturn type(os.setlocale), type(os.tmpname), a, b",
                        profile,
                    )
                    .unwrap(),
                vec![
                    Value::String(Arc::from(&b"function"[..])),
                    Value::String(Arc::from(&b"function"[..])),
                    Value::Boolean(false),
                    Value::Boolean(false),
                ],
                "profile {profile:?} unavailable locale"
            );

            let mut engine = Engine::default();
            engine.vm_mut().set_os_setlocale_getter(|locale, category| {
                if (locale.is_none() && category == b"all")
                    || (locale == Some(b"C") && category == b"numeric")
                {
                    Ok(Some(b"C".to_vec()))
                } else {
                    Err(RuntimeError::InvalidRange {
                        operation: "test os.setlocale request",
                    })
                }
            });
            engine
                .vm_mut()
                .set_os_tmpname_getter(|| Ok(b"/tmp/blu-owned.tmp".to_vec()));
            assert_eq!(
                engine
                    .execute_owned_source(
                        "local current = os.setlocale(nil)\nlocal numeric = os.setlocale('C', 'numeric')\nreturn current == 'C' and numeric == 'C' and os.tmpname() == '/tmp/blu-owned.tmp'",
                        profile,
                    )
                    .unwrap(),
                vec![Value::Boolean(true)],
                "profile {profile:?} authorized locale"
            );
        }
        for profile in [SemanticProfile::Blu] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source("return type(os)", profile)
                    .unwrap(),
                vec![Value::String(Arc::from(&b"nil"[..]))],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn debug_metatable_slice_matches_lua_profiles_and_is_hidden_elsewhere() {
        let source = r#"
            local value = {}
            local metatable = { answer = 42 }
            setmetatable(value, { __metatable = "locked" })
            local raw_before = debug.getmetatable(value)
            local returned = debug.setmetatable(value, metatable)
            return raw_before.__metatable == "locked"
                and getmetatable(value) == metatable
                and debug.getmetatable(value) == metatable
                and type(debug.getregistry()) == "table"
                and debug.getregistry() == debug.getregistry()
                and ((_VERSION == "Lua 5.1" and returned == true)
                    or (_VERSION ~= "Lua 5.1" and returned == value))
        "#;
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}")),
                vec![Value::Boolean(true)],
                "profile {profile:?}"
            );
        }
        assert_eq!(
            Engine::default()
                .execute_owned_source("return type(debug)", SemanticProfile::Luau)
                .unwrap(),
            vec![Value::String(Arc::from(&b"table"[..]))]
        );
        assert_eq!(
            Engine::default()
                .execute_owned_source(
                    "return type(debug.getmetatable), type(debug.setmetatable), type(debug.getregistry)",
                    SemanticProfile::Luau,
                )
                .unwrap(),
            vec![
                Value::String(Arc::from(&b"nil"[..])),
                Value::String(Arc::from(&b"nil"[..])),
                Value::String(Arc::from(&b"nil"[..])),
            ]
        );
        assert_eq!(
            Engine::default()
                .execute_owned_source("return type(debug.traceback)", SemanticProfile::Luau)
                .unwrap(),
            vec![Value::String(Arc::from(&b"function"[..]))]
        );
        assert_eq!(
            Engine::default()
                .execute_owned_source("return type(debug)", SemanticProfile::Blu)
                .unwrap(),
            vec![Value::String(Arc::from(&b"nil"[..]))]
        );
    }

    #[test]
    fn debug_metatables_apply_to_shared_scalar_types() {
        let source = r#"
            local metatable = {
                __index = function(value, key)
                    if type(value) == "number" then return value + key end
                    return value or key
                end,
                __add = function(left, right) return (left or 1) + (right or 2) end,
            }
            debug.setmetatable(10, metatable)
            debug.setmetatable(true, metatable)
            debug.setmetatable(nil, metatable)
            return (10)[3] == 13,
                (true)[false] == true,
                (false)[false] == false,
                10 + nil == 12,
                nil + 23 == 24,
                nil + nil == 3,
                getmetatable(-2) == metatable,
                getmetatable(false) == metatable,
                getmetatable(nil) == metatable
        "#;
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}")),
                vec![Value::Boolean(true); 9],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn host_userdata_metatables_support_indexing_and_gc_roots() {
        let source = r#"
            local file = io.open("answer.txt")
            local values = {}
            local metatable = {
                __index = { answer = 42 },
                __newindex = function(_, key, value) values[key] = value end,
            }
            local global_ok = pcall(setmetatable, file, metatable)
            local returned = debug.setmetatable(file, metatable)
            local answer = file.answer
            file.answer = 43
            return not global_ok
                and ((_VERSION == "Lua 5.1" and returned == true)
                or (_VERSION ~= "Lua 5.1" and returned == file))
                and getmetatable(file) == metatable
                and debug.getmetatable(file) == metatable
                and answer == 42
                and values.answer == 43
        "#;
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let mut engine = Engine::default();
            engine.vm_mut().set_io_file_opener(|_, _| {
                Ok(Arc::new(TestIoFile(Arc::new(AtomicUsize::new(0)))) as Arc<dyn IoFile>)
            });
            assert_eq!(
                engine
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}")),
                vec![Value::Boolean(true)],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn heap_traced_io_iterators_report_c_function_debug_metadata() {
        let source = r#"
            local file = io.open("answer.txt")
            local iterator = file:lines()
            local info = debug.getinfo(iterator, "Snu")
            return info.what == "C" and info.source == "=[C]"
                and ((_VERSION == "Lua 5.1" and info.nups == 2)
                    or (_VERSION ~= "Lua 5.1" and info.nups == 3))
        "#;
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let mut engine = Engine::default();
            engine.vm_mut().set_io_file_opener(|_, _| {
                Ok(Arc::new(TestIoFile(Arc::new(AtomicUsize::new(0)))) as Arc<dyn IoFile>)
            });
            assert_eq!(
                engine
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}")),
                vec![Value::Boolean(true)],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn debug_getinfo_reports_function_shape_without_fabricating_lines() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let actual = Engine::default()
                .execute_owned_source(
                    "local function answer(value, ...)\n    local live = debug.getinfo(1, 'Snuf')\n    return live.what == 'Lua' and live.linedefined == 1 and live.lastlinedefined == 4 and live.func == answer and live.nups >= 0 and ((_VERSION == 'Lua 5.1' and live.nparams == nil and live.isvararg == nil) or (_VERSION ~= 'Lua 5.1' and live.nparams == 1 and live.isvararg)), live.nups\nend\nlocal info = debug.getinfo(answer, 'Snu')\nlocal live_ok, live_nups = answer(1)\nreturn info.what == 'Lua' and type(info.source) == 'string' and info.linedefined == 1 and info.lastlinedefined == 4 and info.nups == live_nups and ((_VERSION == 'Lua 5.1' and info.nparams == nil and info.isvararg == nil) or (_VERSION ~= 'Lua 5.1' and info.nparams == 1 and info.isvararg)) and live_ok",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(actual, vec![Value::Boolean(true)], "profile {profile:?}");
        }
        assert_eq!(
            Engine::default()
                .execute_owned_source(
                    "local ok, message = pcall(debug.getinfo, 1, 'X')\nreturn ok, type(message)",
                    SemanticProfile::Lua54,
                )
                .unwrap(),
            vec![
                Value::Boolean(false),
                Value::String(Arc::from(&b"string"[..]))
            ]
        );
    }

    #[test]
    fn debug_hooks_reject_call_return_masks_and_run_count_hooks() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let actual = Engine::default()
                .execute_owned_source(
                    "local seen = 0\nlocal function hook(event, line) if event == 'count' and line == nil then seen = seen + 1 end end\nlocal mask_ok = pcall(debug.sethook, hook, 'x')\nlocal count_ok = pcall(debug.sethook, hook, '', 3)\nlocal value = 0\nfor index = 1, 5 do value = value + index end\ndebug.sethook()\nreturn not mask_ok and count_ok and seen > 0",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(actual, vec![Value::Boolean(true)], "profile {profile:?}");
        }
    }

    #[test]
    fn debug_call_and_return_hooks_report_owned_function_events() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let actual = Engine::default()
                .execute_owned_source(
                    "local saw_call = false\nlocal saw_return = false\nlocal function hook(event, line) if event == 'call' then saw_call = true end if event == 'return' then saw_return = true end end\ndebug.sethook(hook, 'cr')\nlocal function answer()\n    return 42\nend\nlocal value = answer()\ndebug.sethook()\nreturn value == 42 and saw_call and saw_return",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(actual, vec![Value::Boolean(true)], "profile {profile:?}");
        }
    }

    #[test]
    fn debug_tail_hooks_report_profile_specific_owned_tail_events() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let actual = Engine::default()
                .execute_owned_source(
                    "local saw_tail = false\nlocal function hook(event, line) if event == 'tail call' or (_VERSION == 'Lua 5.1' and event == 'tail return') then saw_tail = true end end\ndebug.sethook(hook, 'cr')\nlocal function leaf(value) return value end\nlocal function tail(value) return leaf(value) end\nlocal value = tail(42)\ndebug.sethook()\nreturn value == 42 and saw_tail",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(actual, vec![Value::Boolean(true)], "profile {profile:?}");
        }
    }

    #[test]
    fn debug_hooks_report_owned_frame_native_callback_activity() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let actual = Engine::default()
                .execute_owned_source(
                    "local calls = 0\nlocal returns = 0\nlocal function hook(event, line) if event == 'call' then calls = calls + 1 end if event == 'return' then returns = returns + 1 end end\ndebug.sethook(hook, 'cr')\nlocal function wrapper() return math.abs(-2) end\nlocal value = wrapper()\ndebug.sethook()\nreturn value == 2 and calls >= 3 and returns >= 2",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(actual, vec![Value::Boolean(true)], "profile {profile:?}");
        }
    }

    #[test]
    fn debug_native_hook_frames_report_stable_c_metadata() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let actual = Engine::default()
                .execute_owned_source(
                    "local saw_c = false\nlocal function hook(event, line) if event == 'call' then local info = debug.getinfo(2, 'Snu') if info.what == 'C' and info.source == '=[C]' and info.short_src == '[C]' and info.nups == 0 and ((_VERSION == 'Lua 5.1' and info.nparams == nil and info.isvararg == nil) or (_VERSION ~= 'Lua 5.1' and info.nparams == 0 and info.isvararg == true)) then saw_c = true end end end\ndebug.sethook(hook, 'c')\nlocal value = math.abs(-2)\ndebug.sethook()\nreturn value == 2 and saw_c",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(actual, vec![Value::Boolean(true)], "profile {profile:?}");
        }
    }

    #[test]
    fn debug_native_hook_frames_report_direct_call_names() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let actual = Engine::default()
                .execute_owned_source(
                    "local saw_name = false\nlocal function hook(event, line) if event == 'call' then local info = debug.getinfo(2, 'Sn') if info.what == 'C' and info.namewhat == 'field' and info.name == 'abs' then saw_name = true end end end\ndebug.sethook(hook, 'c')\nlocal value = math.abs(-2)\ndebug.sethook()\nreturn value == 2 and saw_name",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(actual, vec![Value::Boolean(true)], "profile {profile:?}");
        }
    }

    #[test]
    fn debug_hook_yield_is_rejected_at_the_hook_boundary() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let actual = Engine::default()
                .execute_owned_source(
                    "local co = coroutine.create(function()\n    local first = true\n    debug.sethook(function()\n        if first then first = false; coroutine.yield('hook') end\n    end, 'l')\n    local value = 1\n    value = value + 1\n    debug.sethook()\n    return value\nend)\nlocal ok, value = coroutine.resume(co)\nreturn not ok",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(actual, vec![Value::Boolean(true)], "profile {profile:?}");
        }
    }

    #[test]
    fn debug_thread_targeted_hooks_follow_the_selected_coroutine() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let actual = Engine::default()
                .execute_owned_source(
                    "local seen = 0\nlocal hook = function(event) if event == 'call' then seen = seen + 1 end end\nlocal co = coroutine.create(function() return math.abs(-2) end)\ndebug.sethook(co, hook, 'c')\nlocal main_hook = debug.gethook()\nlocal target_hook, mask = debug.gethook(co)\nlocal ok, value = coroutine.resume(co)\ndebug.sethook(co)\nreturn ok and value == 2 and seen > 0 and main_hook == nil and target_hook == hook and mask == 'c'",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(actual, vec![Value::Boolean(true)], "profile {profile:?}");
        }
    }

    #[test]
    fn debug_getlocal_reports_owned_names_and_live_register_values() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let actual = Engine::default()
                .execute_owned_source(
                    "local function inspect(argument)\n    local answer = 42\n    local first_name, first_value = debug.getlocal(1, 1)\n    local second_name, second_value = debug.getlocal(1, 2)\n    return first_name == 'argument' and first_value == argument and second_name == 'answer' and second_value == 42\nend\nreturn inspect(7)",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(actual, vec![Value::Boolean(true)], "profile {profile:?}");
        }
    }

    #[test]
    fn debug_setlocal_updates_active_and_suspended_owned_frames() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let actual = Engine::default()
                .execute_owned_source(
                    "local function active(argument)\n    local answer = 42\n    local first = debug.setlocal(1, 1, 7)\n    local second = debug.setlocal(1, 2, 8)\n    local first_name, first_value = debug.getlocal(1, 1)\n    local second_name, second_value = debug.getlocal(1, 2)\n    return first == 'argument' and second == 'answer' and first_name == 'argument' and first_value == 7 and second_name == 'answer' and second_value == 8\nend\nlocal function suspended(argument)\n    local answer = 42\n    coroutine.yield()\n    return argument + answer\nend\nlocal thread = coroutine.create(suspended)\nlocal started = coroutine.resume(thread, 3)\nlocal first = debug.setlocal(thread, 1, 1, 7)\nlocal second = debug.setlocal(thread, 1, 2, 8)\nlocal first_name, first_value = debug.getlocal(thread, 1, 1)\nlocal second_name, second_value = debug.getlocal(thread, 1, 2)\nlocal finished, result = coroutine.resume(thread)\nreturn active(1) and started and finished and result == 15 and first == 'argument' and second == 'answer' and first_name == 'argument' and first_value == 7 and second_name == 'answer' and second_value == 8",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(actual, vec![Value::Boolean(true)], "profile {profile:?}");
        }
    }

    #[test]
    fn debug_getinfo_reports_currentline_for_active_owned_frames() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let actual = Engine::default()
                .execute_owned_source(
                    "local function inspect()\n    local info = debug.getinfo(1, 'l')\n    return info.currentline == 2\nend\nreturn inspect()",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(actual, vec![Value::Boolean(true)], "profile {profile:?}");
        }
    }

    #[test]
    fn debug_getinfo_reports_owned_active_lines() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let actual = Engine::default()
                .execute_owned_source(
                    "local function inspect()\n    local info = debug.getinfo(1, 'L')\n    return type(info.activelines) == 'table' and info.activelines[2] == true\nend\nreturn inspect()",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(actual, vec![Value::Boolean(true)], "profile {profile:?}");
        }
    }

    #[test]
    fn debug_stack_queries_report_retained_owned_callers() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let actual = Engine::default()
                .execute_owned_source(
                    "local function inner()\n    local info = debug.getinfo(2, 'Su')\n    local name, value = debug.getlocal(2, 1)\n    return info ~= nil and info.what == 'Lua' and name == 'argument' and value == 7\nend\nlocal function outer(argument)\n    local result = inner()\n    return result\nend\nlocal result = outer(7)\nreturn result",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(actual, vec![Value::Boolean(true)], "profile {profile:?}");
        }
    }

    #[test]
    fn debug_setlocal_updates_retained_owned_callers() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let actual = Engine::default()
                .execute_owned_source(
                    "local function inner()\n    local name = debug.setlocal(2, 1, 9)\n    local caller_name, caller_value = debug.getlocal(2, 1)\n    return name == 'argument' and caller_name == 'argument' and caller_value == 9\nend\nlocal function outer(argument)\n    local result = inner()\n    return result and argument == 9\nend\nreturn outer(7)",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(actual, vec![Value::Boolean(true)], "profile {profile:?}");
        }
    }

    #[test]
    fn debug_upvaluejoin_shares_owned_closure_cells_on_modern_lua_profiles() {
        for profile in [
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let actual = Engine::default()
                .execute_owned_source(
                    "local first_value = 1\nlocal function first() return first_value end\nlocal second_value = 2\nlocal function second() return second_value end\nlocal first_id = debug.upvalueid(first, 1)\nlocal second_id = debug.upvalueid(second, 1)\ndebug.upvaluejoin(first, 1, second, 1)\nlocal joined_id = debug.upvalueid(first, 1)\ndebug.setupvalue(second, 1, 3)\nreturn type(first_id) == 'userdata' and first_id ~= second_id and second_id == joined_id and first() == 3 and second() == 3",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(actual, vec![Value::Boolean(true)], "profile {profile:?}");
        }
        let lua51 = Engine::default()
            .execute_owned_source(
                "return debug and debug.upvaluejoin == nil",
                SemanticProfile::Lua51,
            )
            .expect("Lua 5.1 debug.upvaluejoin surface");
        assert_eq!(lua51, vec![Value::Boolean(true)]);
    }

    #[test]
    fn debug_thread_targets_report_active_coroutine_frames_and_locals() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let actual = Engine::default()
                .execute_owned_source(
                    "local function inspect(argument)\n    local thread = coroutine.running()\n    local info = debug.getinfo(thread, 1, 'Su')\n    local name, value = debug.getlocal(thread, 1, 1)\n    return info.what == 'Lua' and name == 'argument' and value == argument\nend\nlocal thread = coroutine.create(inspect)\nlocal ok, result = coroutine.resume(thread, 7)\nreturn ok and result",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(actual, vec![Value::Boolean(true)], "profile {profile:?}");
        }
    }

    #[test]
    fn debug_thread_targets_report_suspended_owned_frames_and_locals() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let actual = Engine::default()
                .execute_owned_source(
                    "local function inspect(argument)\n    local answer = 42\n    coroutine.yield()\n    return argument + answer\nend\nlocal thread = coroutine.create(inspect)\nlocal started = coroutine.resume(thread, 7)\nlocal info = debug.getinfo(thread, 1, 'Su')\nlocal first_name, first_value = debug.getlocal(thread, 1, 1)\nlocal second_name, second_value = debug.getlocal(thread, 1, 2)\nlocal missing = debug.getinfo(thread, 2)\nlocal finished, result = coroutine.resume(thread)\nreturn started and finished and result == 49 and info.what == 'Lua' and first_name == 'argument' and first_value == 7 and second_name == 'answer' and second_value == 42 and missing == nil",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(actual, vec![Value::Boolean(true)], "profile {profile:?}");
        }
    }

    #[test]
    fn debug_upvalue_access_reports_names_and_updates_shared_cells() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let actual = Engine::default()
                .execute_owned_source(
                    "local captured = 41\nlocal function inner() return captured end\nlocal name, value = debug.getupvalue(inner, 1)\nlocal changed = debug.setupvalue(inner, 1, 42)\nlocal _, updated = debug.getupvalue(inner, 1)\nreturn name == 'captured' and value == 41 and changed == 'captured' and updated == 42 and inner() == 42",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(actual, vec![Value::Boolean(true)], "profile {profile:?}");
        }
    }

    #[test]
    fn debug_uservalue_matches_profile_and_userdata_slot_shapes() {
        for profile in [SemanticProfile::Lua52, SemanticProfile::Lua53] {
            let mut engine = Engine::default();
            engine.vm_mut().set_io_file_opener(|_, _| {
                Ok(Arc::new(TestIoFile(Arc::new(AtomicUsize::new(0)))) as Arc<dyn IoFile>)
            });
            assert_eq!(
                engine
                    .execute_owned_source(
                        "local file = io.open('answer.txt')\nlocal before = debug.getuservalue(file)\nlocal marker = {}\nlocal returned = debug.setuservalue(file, marker)\nlocal after = debug.getuservalue(file)\nreturn before == nil and returned == marker and after == marker",
                        profile,
                    )
                    .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}")),
                vec![Value::Boolean(true)],
                "profile {profile:?}"
            );
        }

        for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
            let mut engine = Engine::default();
            engine.vm_mut().set_io_file_opener(|_, _| {
                Ok(Arc::new(TestIoFile(Arc::new(AtomicUsize::new(0)))) as Arc<dyn IoFile>)
            });
            assert_eq!(
                engine
                    .execute_owned_source(
                        "local file = io.open('answer.txt')\nlocal before, before_ok = debug.getuservalue(file)\nlocal returned = debug.setuservalue(file, 'value')\nlocal after, after_ok = debug.getuservalue(file)\nreturn before == nil and before_ok == nil and returned == false and after == nil and after_ok == nil",
                        profile,
                    )
                    .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}")),
                vec![Value::Boolean(true)],
                "profile {profile:?}"
            );
        }

        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Blu,
            SemanticProfile::Luau,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(
                        "return type(debug), type(debug and debug.getuservalue)",
                        profile,
                    )
                    .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}")),
                vec![
                    Value::String(Arc::from(
                        if matches!(profile, SemanticProfile::Lua51 | SemanticProfile::Luau) {
                            &b"table"[..]
                        } else {
                            &b"nil"[..]
                        }
                    )),
                    Value::String(Arc::from(&b"nil"[..])),
                ],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn debug_traceback_reports_bounded_owned_frames() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(
                        "local function answer()\n    local trace = debug.traceback('marker', 1)\n    return type(trace) == 'string' and string.find(trace, 'marker', 1, true) ~= nil and string.find(trace, 'stack traceback', 1, true) ~= nil and string.find(trace, ':2: in function', 1, true) ~= nil\nend\nreturn answer()",
                        profile,
                    )
                    .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}")),
                vec![Value::Boolean(true)],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn debug_line_hooks_report_real_owned_lines_and_clear() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let actual = Engine::default()
                .execute_owned_source(
                    "local seen = 0\nlocal last = 0\nlocal function hook(event, line)\n    if event == 'line' then seen = seen + 1; last = line end\nend\ndebug.sethook(hook, 'l')\nlocal value = 1\nvalue = value + 1\ndebug.sethook()\nlocal f, mask, count = debug.gethook()\nreturn seen > 0 and last > 0 and f == nil and ((_VERSION == 'Lua 5.4' or _VERSION == 'Lua 5.5') and mask == nil and count == nil or (_VERSION ~= 'Lua 5.4' and _VERSION ~= 'Lua 5.5' and mask == '' and count == 0))",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(actual, vec![Value::Boolean(true)], "profile {profile:?}");
        }
        assert_eq!(
            Engine::default()
                .execute_owned_source(
                    "return type(debug.sethook), type(debug.gethook)",
                    SemanticProfile::Luau,
                )
                .unwrap(),
            vec![
                Value::String(Arc::from(&b"nil"[..])),
                Value::String(Arc::from(&b"nil"[..])),
            ]
        );
    }

    #[test]
    fn string_literal_call_sugar_executes_across_owned_profiles() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
            SemanticProfile::Blu,
            SemanticProfile::Luau,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(
                        "local function echo(value) return value end\nreturn echo\"answer\"",
                        profile,
                    )
                    .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}")),
                vec![Value::String(Arc::from(&b"answer"[..]))],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn io_is_profile_gated_and_requires_an_explicit_opener() {
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source("return type(io)", profile)
                    .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}")),
                vec![Value::String(Arc::from(&b"nil"[..]))],
                "profile {profile:?}"
            );
        }
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(
                        "local file, message = io.open(\"answer.txt\")\nreturn type(io), file == nil, type(message)",
                        profile,
                    )
                    .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}")),
                vec![
                    Value::String(Arc::from(&b"table"[..])),
                    Value::Boolean(true),
                    Value::String(Arc::from(&b"string"[..])),
                ],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn io_tmpfile_requires_an_explicit_opener_and_returns_opaque_userdata() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let unavailable = Engine::default()
                .execute_owned_source(
                    "local ok, value = pcall(io.tmpfile)\nreturn ok, type(value)",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(
                unavailable,
                vec![
                    Value::Boolean(false),
                    Value::String(Arc::from(&b"string"[..])),
                ],
                "profile {profile:?}"
            );

            let mut engine = Engine::default();
            engine.vm_mut().set_io_tempfile_opener(|| {
                Ok(Arc::new(TestIoFile(Arc::new(AtomicUsize::new(0)))) as Arc<dyn IoFile>)
            });
            let available = engine
                .execute_owned_source(
                    "local file = io.tmpfile()\nreturn type(file) == 'userdata' and io.type(file) == 'file'",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(available, vec![Value::Boolean(true)], "profile {profile:?}");
        }
    }

    #[test]
    fn file_setvbuf_forwards_bounded_buffering_requests_to_the_host() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let mut engine = Engine::default();
            engine.vm_mut().set_io_file_opener(|_, _| {
                Ok(Arc::new(TestIoFile(Arc::new(AtomicUsize::new(0)))) as Arc<dyn IoFile>)
            });
            let result = engine
                .execute_owned_source(
                    "local file = io.open('answer.txt', 'r')\nlocal ok = file:setvbuf('full', 64)\nlocal bad, error = pcall(file.setvbuf, file, 'bad')\nreturn ok, bad, type(error)",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(
                result,
                vec![
                    Value::Boolean(true),
                    Value::Boolean(false),
                    Value::String(Arc::from(&b"string"[..])),
                ],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn io_popen_requires_an_explicit_process_capability_and_returns_a_file_handle() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let unavailable = Engine::default()
                .execute_owned_source(
                    "local ok, value = pcall(io.popen, 'echo blu', 'r')\nreturn ok, type(value)",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(
                unavailable,
                vec![
                    Value::Boolean(false),
                    Value::String(Arc::from(&b"string"[..])),
                ],
                "profile {profile:?}"
            );

            let mut engine = Engine::default();
            engine.vm_mut().set_io_popen_opener(|command, mode| {
                assert_eq!(command, b"echo blu");
                assert_eq!(mode, b"r");
                Ok(Arc::new(TestIoFile(Arc::new(AtomicUsize::new(0)))) as Arc<dyn IoFile>)
            });
            let available = engine
                .execute_owned_source(
                    "local file = io.popen('echo blu')\nlocal kind = type(file) == 'userdata' and io.type(file) == 'file'\nlocal closed = file:close()\nreturn kind and closed",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(available, vec![Value::Boolean(true)], "profile {profile:?}");
        }
    }

    struct TestIoFile(Arc<AtomicUsize>);

    impl IoFile for TestIoFile {
        fn close(&self) -> Result<(), RuntimeError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn set_buffering(
            &self,
            _mode: IoBufferMode,
            _size: Option<usize>,
        ) -> Result<(), RuntimeError> {
            Ok(())
        }
    }

    struct MemoryIoFile {
        bytes: Mutex<Vec<u8>>,
        position: Mutex<usize>,
        flushes: Arc<AtomicUsize>,
        closes: Arc<AtomicUsize>,
    }

    impl IoFile for MemoryIoFile {
        fn close(&self) -> Result<(), RuntimeError> {
            self.closes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn read(&self, request: IoReadRequest) -> Result<Option<Vec<u8>>, RuntimeError> {
            let bytes = self.bytes.lock().expect("memory file bytes lock");
            let mut position = self.position.lock().expect("memory file position lock");
            if *position >= bytes.len() {
                return Ok(match request {
                    IoReadRequest::Bytes(0) => Some(Vec::new()),
                    _ => None,
                });
            }
            let end = match request {
                IoReadRequest::All => bytes.len(),
                IoReadRequest::Bytes(count) => position.saturating_add(count).min(bytes.len()),
                IoReadRequest::Line { .. } => bytes[*position..]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(bytes.len(), |offset| *position + offset + 1),
            };
            let mut result = bytes[*position..end].to_vec();
            if let IoReadRequest::Line { keep_end: false } = request
                && result.last() == Some(&b'\n')
            {
                result.pop();
                if result.last() == Some(&b'\r') {
                    result.pop();
                }
            }
            *position = end;
            Ok(Some(result))
        }

        fn read_number(&self) -> Result<Option<Vec<u8>>, RuntimeError> {
            let mut token = Vec::new();
            loop {
                let Some(bytes) = self.read(IoReadRequest::Bytes(1))? else {
                    break;
                };
                let byte = bytes[0];
                if token.is_empty() && byte.is_ascii_whitespace() {
                    continue;
                }
                if byte.is_ascii_whitespace() {
                    break;
                }
                token.push(byte);
            }
            Ok((!token.is_empty()).then_some(token))
        }

        fn write(&self, bytes: &[u8]) -> Result<(), RuntimeError> {
            let mut target = self.bytes.lock().expect("memory file bytes lock");
            let mut position = self.position.lock().expect("memory file position lock");
            let end = position
                .checked_add(bytes.len())
                .ok_or(RuntimeError::StringLimit {
                    required: usize::MAX,
                    limit: 64 * 1024 * 1024,
                })?;
            if end > target.len() {
                target.resize(end, 0);
            }
            target[*position..end].copy_from_slice(bytes);
            *position = end;
            Ok(())
        }

        fn seek(&self, whence: IoSeekWhence, offset: i64) -> Result<u64, RuntimeError> {
            let bytes = self.bytes.lock().expect("memory file bytes lock");
            let mut position = self.position.lock().expect("memory file position lock");
            let base = match whence {
                IoSeekWhence::Set => 0_i64,
                IoSeekWhence::Current => i64::try_from(*position).unwrap_or(i64::MAX),
                IoSeekWhence::End => i64::try_from(bytes.len()).unwrap_or(i64::MAX),
            };
            let next = base.checked_add(offset).ok_or(RuntimeError::InvalidRange {
                operation: "memory io.seek",
            })?;
            if next < 0 {
                return Err(RuntimeError::InvalidRange {
                    operation: "memory io.seek",
                });
            }
            *position = usize::try_from(next).map_err(|_| RuntimeError::InvalidRange {
                operation: "memory io.seek",
            })?;
            Ok(next as u64)
        }

        fn flush(&self) -> Result<(), RuntimeError> {
            self.flushes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn io_file_methods_support_bounded_read_write_seek_and_flush() {
        let flushes = Arc::new(AtomicUsize::new(0));
        let closes = Arc::new(AtomicUsize::new(0));
        let callback_flushes = Arc::clone(&flushes);
        let callback_closes = Arc::clone(&closes);
        let mut engine = Engine::default();
        engine.vm_mut().set_io_file_opener(move |_, mode| {
            let initial = if mode == b"rb" {
                b"alpha\nbeta\n".to_vec()
            } else {
                Vec::new()
            };
            Ok(Arc::new(MemoryIoFile {
                bytes: Mutex::new(initial),
                position: Mutex::new(0),
                flushes: Arc::clone(&callback_flushes),
                closes: Arc::clone(&callback_closes),
            }) as Arc<dyn IoFile>)
        });
        let result = engine
            .execute_owned_source(
                r#"
                        local input = io.open("answer.txt", "rb")
                        local first = input:read(6)
                        local line = input:read("*l")
                        local position = input:seek("set", 0)
                        local all = input:read("*a")
                        local flushed = input:flush()
                        local iterator_type = type(input.lines)
                        local reset = input:seek("set", 0)
                        local iterator = input:lines()
                        local first_line = iterator()
                        local second_line = iterator()
                        local done = iterator()
                        local output = io.open("output.txt", "w")
                        local returned = output:write("blu", 5)
                        output:close()
                        input:close()
                        return first == "alpha\n" and line == "beta" and position == 0
                            and all == "alpha\nbeta\n" and flushed == true
                            and iterator_type == "function" and reset == 0
                            and first_line == "alpha" and second_line == "beta"
                            and done == nil and returned == output
                    "#,
                SemanticProfile::Lua54,
            )
            .unwrap();
        assert_eq!(result, vec![Value::Boolean(true)]);
        assert_eq!(flushes.load(Ordering::SeqCst), 1);
        assert_eq!(closes.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn io_read_supports_two_line_formats_in_one_call() {
        let mut engine = Engine::default();
        engine.vm_mut().set_io_file_opener(|_, mode| {
            assert_eq!(mode, b"rb");
            Ok(Arc::new(MemoryIoFile {
                bytes: Mutex::new(b"alpha\nbeta\n".to_vec()),
                position: Mutex::new(0),
                flushes: Arc::new(AtomicUsize::new(0)),
                closes: Arc::new(AtomicUsize::new(0)),
            }) as Arc<dyn IoFile>)
        });
        assert_eq!(
            engine
                .execute_owned_source(
                    r#"
                        local file = io.open("answer.txt", "rb")
                        local iterator = file:lines("*l", "*l")
                        local first, second = iterator()
                        local done = iterator()
                        return first == "alpha" and second == "beta" and done == nil
                    "#,
                    SemanticProfile::Lua54,
                )
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn io_read_supports_host_authorized_numeric_formats() {
        let mut engine = Engine::default();
        engine.vm_mut().set_io_file_opener(|_, mode| {
            assert_eq!(mode, b"rb");
            Ok(Arc::new(MemoryIoFile {
                bytes: Mutex::new(b"42 3.5".to_vec()),
                position: Mutex::new(0),
                flushes: Arc::new(AtomicUsize::new(0)),
                closes: Arc::new(AtomicUsize::new(0)),
            }) as Arc<dyn IoFile>)
        });
        assert_eq!(
            engine
                .execute_owned_source(
                    r#"
                        local file = io.open("answer.txt", "rb")
                        local iterator = file:lines("*n")
                        local integer = iterator()
                        local fraction = iterator()
                        local done = iterator()
                        return integer == 42 and fraction == 3.5 and done == nil
                    "#,
                    SemanticProfile::Lua54,
                )
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn io_lines_roots_the_file_until_eof_then_allows_gc_close() {
        let closes = Arc::new(AtomicUsize::new(0));
        let callback_closes = Arc::clone(&closes);
        let mut engine = Engine::default();
        engine.vm_mut().set_io_file_opener(move |_, _| {
            Ok(Arc::new(MemoryIoFile {
                bytes: Mutex::new(b"one\ntwo\n".to_vec()),
                position: Mutex::new(0),
                flushes: Arc::new(AtomicUsize::new(0)),
                closes: Arc::clone(&callback_closes),
            }) as Arc<dyn IoFile>)
        });
        assert_eq!(
            engine
                .execute_owned_source(
                    r#"
                        local file = io.open("answer.txt")
                        local iterator = file:lines()
                        file = nil
                        local first = iterator()
                        local second = iterator()
                        local done = iterator()
                        return first, second, done
                    "#,
                    SemanticProfile::Lua54,
                )
                .unwrap(),
            vec![
                Value::String(Arc::from(&b"one"[..])),
                Value::String(Arc::from(&b"two"[..])),
                Value::Nil,
            ]
        );
        assert_eq!(closes.load(Ordering::SeqCst), 0);
        engine.vm_mut().collect(std::iter::empty()).unwrap();
        assert_eq!(closes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn io_default_streams_are_host_authorized_and_profile_gated() {
        let stdin = Arc::new(MemoryIoFile {
            bytes: Mutex::new(b"abc\nfirst\n".to_vec()),
            position: Mutex::new(0),
            flushes: Arc::new(AtomicUsize::new(0)),
            closes: Arc::new(AtomicUsize::new(0)),
        });
        let stdout = Arc::new(MemoryIoFile {
            bytes: Mutex::new(Vec::new()),
            position: Mutex::new(0),
            flushes: Arc::new(AtomicUsize::new(0)),
            closes: Arc::new(AtomicUsize::new(0)),
        });
        let stderr = Arc::new(MemoryIoFile {
            bytes: Mutex::new(Vec::new()),
            position: Mutex::new(0),
            flushes: Arc::new(AtomicUsize::new(0)),
            closes: Arc::new(AtomicUsize::new(0)),
        });
        let mut engine = Engine::default();
        let callback_stdin = Arc::clone(&stdin);
        let callback_stdout = Arc::clone(&stdout);
        let callback_stderr = Arc::clone(&stderr);
        engine.vm_mut().set_io_stream_opener(move |kind| {
            Ok(match kind {
                IoStreamKind::Stdin => Arc::clone(&callback_stdin) as Arc<dyn IoFile>,
                IoStreamKind::Stdout => Arc::clone(&callback_stdout) as Arc<dyn IoFile>,
                IoStreamKind::Stderr => Arc::clone(&callback_stderr) as Arc<dyn IoFile>,
            })
        });
        let result = engine
            .execute_owned_source(
                r#"
                        local input = io.input()
                        local stdin = io.stdin
                        local first = io.read(3)
                        local newline = io.read("*l")
                        local reset = input:seek("set", 0)
                        local iterator = io.lines()
                        local first_line = iterator()
                        local second_line = iterator()
                        local done = iterator()
                        local output = io.output()
                        local stdout = io.stdout
                        local returned = io.write("blu", 5)
                        local flushed = io.flush()
                        local stderr = io.stderr
                        io.stdin = nil
                        local removed = io.stdin
                        return input == stdin and first == "abc" and newline == ""
                            and reset == 0 and first_line == "abc" and second_line == "first"
                            and done == nil and output == stdout and returned == output
                            and flushed == true and type(stderr) == "userdata"
                            and removed == nil
                    "#,
                SemanticProfile::Lua54,
            )
            .unwrap();
        assert_eq!(result, vec![Value::Boolean(true)]);
        assert_eq!(&*stdout.bytes.lock().unwrap(), b"blu5");
        assert_eq!(stdout.flushes.load(Ordering::SeqCst), 1);
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                engine
                    .execute_owned_source("return type(io)", profile)
                    .unwrap(),
                vec![Value::String(Arc::from(&b"nil"[..])),],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn io_file_handles_are_opaque_profile_gated_and_closeable() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let closes = Arc::new(AtomicUsize::new(0));
            let callback_closes = Arc::clone(&closes);
            let mut engine = Engine::default();
            engine.vm_mut().set_io_file_opener(move |path, mode| {
                assert_eq!(path, b"answer.txt");
                assert_eq!(mode, b"rb");
                Ok(Arc::new(TestIoFile(Arc::clone(&callback_closes))) as Arc<dyn IoFile>)
            });
            assert_eq!(
                engine
                    .execute_owned_source(
                        r#"
                            local file, open_error = io.open("answer.txt", "rb")
                            local before = io.type(file)
                            local closed = io.close(file)
                            local after = io.type(file)
                            local second_ok, close_error = pcall(io.close, file)
                            return type(io), open_error == nil, before == "file"
                                and closed == true and after == "closed file"
                                and second_ok == false and type(close_error) == "string"
                        "#,
                        profile,
                    )
                    .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}")),
                vec![
                    Value::String(Arc::from(&b"table"[..])),
                    Value::Boolean(true),
                    Value::Boolean(true),
                ],
                "profile {profile:?}"
            );
            assert_eq!(closes.load(Ordering::SeqCst), 1, "profile {profile:?}");
        }
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            let mut engine = Engine::default();
            engine.vm_mut().set_io_file_opener(|_, _| {
                Ok(Arc::new(TestIoFile(Arc::new(AtomicUsize::new(0)))) as Arc<dyn IoFile>)
            });
            assert_eq!(
                engine
                    .execute_owned_source("return type(io)", profile)
                    .unwrap(),
                vec![Value::String(Arc::from(&b"nil"[..]))],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn host_userdata_finalizers_follow_profile_order_and_rearming() {
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let closes = Arc::new(AtomicUsize::new(0));
            let callback_closes = Arc::clone(&closes);
            let mut engine = Engine::default();
            engine.vm_mut().set_io_file_opener(move |_, _| {
                Ok(Arc::new(TestIoFile(Arc::clone(&callback_closes))) as Arc<dyn IoFile>)
            });
            let result = engine
                .execute_owned_source(
                    r#"
                        local finalized = 0
                        local resurrected
                        local metatable = { __gc = function(value)
                            finalized = finalized + 1
                            resurrected = value
                        end }
                        local file = io.open("answer.txt")
                        debug.setmetatable(file, metatable)
                        file = nil
                        collectgarbage("collect")
                        if resurrected ~= nil then
                            debug.setmetatable(resurrected, metatable)
                            resurrected = nil
                            collectgarbage("collect")
                        end
                        return finalized
                    "#,
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            let expected = if profile == SemanticProfile::Lua52 {
                Value::Number(1.0)
            } else if matches!(
                profile,
                SemanticProfile::Lua51
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                if matches!(
                    profile,
                    SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
                ) {
                    Value::Integer(2)
                } else {
                    Value::Number(2.0)
                }
            } else {
                unreachable!("host userdata finalizer test only uses Lua profiles")
            };
            assert_eq!(result, vec![expected], "profile {profile:?}");
            engine.vm_mut().collect(std::iter::empty()).unwrap();
            assert_eq!(closes.load(Ordering::SeqCst), 1, "profile {profile:?}");
        }
    }

    #[test]
    fn unreachable_io_file_handles_close_during_gc() {
        let closes = Arc::new(AtomicUsize::new(0));
        let callback_closes = Arc::clone(&closes);
        let mut engine = Engine::default();
        engine.vm_mut().set_io_file_opener(move |_, _| {
            Ok(Arc::new(TestIoFile(Arc::clone(&callback_closes))) as Arc<dyn IoFile>)
        });
        assert_eq!(
            engine
                .execute_owned_source(
                    "local file = io.open(\"answer.txt\")\nreturn true",
                    SemanticProfile::Lua54,
                )
                .unwrap(),
            vec![Value::Boolean(true)]
        );
        assert_eq!(closes.load(Ordering::SeqCst), 0);
        engine.vm_mut().collect(std::iter::empty()).unwrap();
        assert_eq!(closes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn package_searchpath_uses_an_explicit_host_probe_and_profile_gates() {
        let source = br#"
            local found, found_error = package.searchpath("answer.mod", "./?.lua")
            local missing, missing_error = package.searchpath("missing", "./?.lua")
            return found, found_error == nil, missing == nil, missing_error
        "#;
        for profile in [
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let mut engine = Engine::default();
            engine
                .vm_mut()
                .set_file_probe(|path| Ok(path == b"./answer/mod.lua"));
            let result = engine
                .execute_owned_source(source, profile)
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error}"));
            let missing_error =
                if matches!(profile, SemanticProfile::Lua54 | SemanticProfile::Lua55) {
                    "no file './missing.lua'"
                } else {
                    "\n\tno file './missing.lua'"
                };
            assert_eq!(
                result,
                vec![
                    Value::String(Arc::from(&b"./answer/mod.lua"[..])),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::String(Arc::from(missing_error.as_bytes())),
                ],
                "profile {profile:?}"
            );
        }

        let unavailable = Engine::default()
            .execute_owned_source(
                b"local ok, error = pcall(package.searchpath, \"answer\", \"./?.lua\"); return ok, type(error)",
                SemanticProfile::Lua53,
            )
            .unwrap();
        assert_eq!(
            unavailable,
            vec![
                Value::Boolean(false),
                Value::String(Arc::from(&b"string"[..])),
            ]
        );

        for profile in [
            SemanticProfile::Blu,
            SemanticProfile::Luau,
            SemanticProfile::Lua51,
        ] {
            let result = Engine::default()
                .execute_owned_source(
                    b"return type(package), package == nil and \"nil\" or type(package.searchpath)",
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error}"));
            let expected_package = if profile == SemanticProfile::Lua51 {
                "table"
            } else {
                "nil"
            };
            assert_eq!(
                result,
                vec![
                    Value::String(Arc::from(expected_package.as_bytes())),
                    Value::String(Arc::from(&b"nil"[..])),
                ],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn package_searchpath_reports_empty_templates_like_puc_lua() {
        let source = br#"
            local found, error = package.searchpath("x", "?.lua;;")
            return found, error
        "#;
        for profile in [
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let mut engine = Engine::default();
            engine.vm_mut().set_file_probe(|_| Ok(false));
            let result = engine
                .execute_owned_source(source, profile)
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error}"));
            let prefix = if matches!(profile, SemanticProfile::Lua54 | SemanticProfile::Lua55) {
                ""
            } else {
                "\n\t"
            };
            let expected = format!("{prefix}no file 'x.lua'\n\tno file ''\n\tno file ''");
            assert_eq!(
                result,
                vec![Value::Nil, Value::String(Arc::from(expected.into_bytes()))],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn require_loads_profile_aware_source_modules_through_file_capabilities() {
        let source = br#"
            local value = require("answer")
            local empty = require("empty")
            return value == 41 and package.loaded.answer == 41
                and empty == true and package.loaded.empty == true
        "#;
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let mut engine = Engine::default();
            engine
                .vm_mut()
                .set_file_probe(|path| Ok(path == b"./answer.lua" || path == b"./empty.lua"));
            engine.vm_mut().set_file_loader(|path| {
                if path == b"./answer.lua" {
                    Ok(b"return (...) == \"answer\" and 41 or nil".to_vec())
                } else if path == b"./empty.lua" {
                    Ok(b"return".to_vec())
                } else {
                    Err(RuntimeError::Raised(Value::String(Arc::from(
                        &b"unknown module path"[..],
                    ))))
                }
            });
            let result = engine
                .execute_owned_source(source, profile)
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error}"));
            assert_eq!(result, vec![Value::Boolean(true)], "profile {profile:?}");
        }

        let mut host_precedence = Engine::default();
        host_precedence
            .vm_mut()
            .set_file_probe(|path| Ok(path == b"./answer.lua"));
        host_precedence
            .vm_mut()
            .set_file_loader(|_| Ok(b"return 41".to_vec()));
        host_precedence
            .vm_mut()
            .set_module_loader(|_, _| Ok(Value::Integer(7)));
        assert_eq!(
            host_precedence
                .execute_owned_source(
                    b"package.path = \"./?.lua\"; return require(\"answer\")",
                    SemanticProfile::Lua53,
                )
                .unwrap(),
            vec![Value::Integer(7)]
        );
    }

    #[test]
    fn loadfile_and_dofile_use_an_explicit_host_source_loader() {
        let source = br#"
            local loaded, load_error = loadfile("answer.lua")
            local first = loaded()
            local second = dofile("answer.lua")
            return load_error == nil and first == 41 and second == 41
        "#;
        for profile in [
            SemanticProfile::Blu,
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let mut engine = Engine::default();
            engine.vm_mut().set_file_loader(|path| {
                if path == b"answer.lua" {
                    Ok(b"return 41".to_vec())
                } else {
                    Err(RuntimeError::Raised(Value::String(Arc::from(
                        &b"unknown file"[..],
                    ))))
                }
            });
            assert_eq!(
                engine
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("profile {profile:?}: {error}")),
                vec![Value::Boolean(true)],
                "profile {profile:?}"
            );
        }

        let luau = Engine::default()
            .execute_owned_source(
                b"return type(loadfile), type(dofile)",
                SemanticProfile::Luau,
            )
            .unwrap();
        assert_eq!(
            luau,
            vec![
                Value::String(Arc::from(&b"nil"[..])),
                Value::String(Arc::from(&b"nil"[..]))
            ]
        );
    }

    #[test]
    fn lua_file_loaders_strip_hash_first_lines_but_load_keeps_hash_operator() {
        let source = br##"
            local load_text = loadstring or load
            local loaded, load_error = load_text("#=1", "")
            local file, file_error = loadfile("hash.lua")
            return loaded == nil and type(load_error) == "string"
                and file_error == nil and file() == 41
        "##;
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let mut engine = Engine::default();
            engine.vm_mut().set_file_loader(|path| {
                if path == b"hash.lua" {
                    Ok(b"# testing special comment\nreturn 41".to_vec())
                } else {
                    Err(RuntimeError::Raised(Value::String(Arc::from(
                        &b"unexpected file read"[..],
                    ))))
                }
            });
            assert_eq!(
                engine
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile}: {error:?}")),
                vec![Value::Boolean(true)],
                "{profile}"
            );
        }
    }

    #[test]
    fn require_reports_lua54_and_lua55_not_found_diagnostics() {
        let source = br#"
            package.path = "?.lua;?/?"
            package.cpath = "?.so;?/init"
            local ok, message = pcall(require, "XXX")
            return not ok and message == [[module 'XXX' not found:
	no field package.preload['XXX']
	no file 'XXX.lua'
	no file 'XXX/XXX'
	no file 'XXX.so'
	no file 'XXX/init']]
        "#;
        for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
            let mut engine = Engine::default();
            engine.vm_mut().set_file_probe(|_| Ok(false));
            engine.vm_mut().set_file_loader(|_| {
                Err(RuntimeError::Raised(Value::String(Arc::from(
                    &b"unexpected file read"[..],
                ))))
            });
            assert_eq!(
                engine
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("profile {profile:?}: {error}")),
                vec![Value::Boolean(true)],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn require_executes_and_caches_profile_package_preload_loaders() {
        let source = br#"
            local calls = 0
            package.preload.answer = function(name)
                calls = calls + 1
                return { name = name, value = 42 }
            end
            package.preload.empty = function() end
            local first = require("answer")
            local second = require("answer")
            return first.name == "answer"
                and first.value == 42
                and first == second
                and first == package.loaded.answer
                and calls == 1
                and require("empty") == true
                and package.loaded.empty == true
        "#;
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap(),
                vec![Value::Boolean(true)],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn require_loads_the_installed_debug_library_for_exposed_profiles() {
        let source = br#"
            local loaded = require("debug")
            return loaded == debug and loaded == _G.debug
        "#;
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
            SemanticProfile::Luau,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap(),
                vec![Value::Boolean(true)],
                "profile {profile:?}"
            );
        }
        let hidden = Engine::default().execute_owned_source(source, SemanticProfile::Blu);
        assert!(
            hidden.is_err(),
            "Blu must keep debug hidden through require"
        );
    }

    #[test]
    fn require_loads_puc_standard_libraries_from_package_loaded() {
        let source = br#"
            return require("_G") == _G
                and require("package") == package
                and require("coroutine") == coroutine
                and require("table") == table
                and require("io") == io
                and require("os") == os
                and require("string") == string
                and require("math") == math
                and require("debug") == debug
        "#;
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap(),
                vec![Value::Boolean(true)],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn load_accepts_an_empty_source_name_like_puc_lua() {
        let source = br#"
            local loaded = assert(load("return 41", ""))
            return loaded()
        "#;
        for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap(),
                vec![Value::Integer(41)],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn lua_loaded_chunks_preserve_source_and_short_source_identity() {
        let source = br#"
            local loaded = assert(load("function f () end"))
            local info = debug.getinfo(loaded, "S")
            local empty = assert(load("return 41", ""))
            local empty_info = debug.getinfo(empty, "S")
            return info.source == "function f () end"
                and info.short_src == "[string \"function f () end\"]"
                and empty_info.source == ""
                and empty_info.short_src == "[string \"\"]"
                and empty() == 41
        "#;
        for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap(),
                vec![Value::Boolean(true)],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn loaded_chunks_accept_lua_line_endings_for_debug_lines() {
        let source = br#"
            local function dostring(x)
                local loaded, message = load(x)
                if not loaded then return "LOADERROR:" .. tostring(message) end
                local result = loaded()
                return result or "NORESULT"
            end
            local prog = [[
local a = 1        -- a comment
local b = 2


x = [=[
hi
]=]
y = "\
hello\r\n\
"
return require"debug".getinfo(1).currentline
]]
            local results = {}
            for _, n in pairs{"\n", "\r", "\n\r", "\r\n"} do
                local changed, nn = string.gsub(prog, "\n", n)
                local line, message = dostring(changed)
                assert(line, message)
                results[#results + 1] = line
                results[#results + 1] = nn
            end
            return table.unpack(results)
        "#;
        let expected = vec![
            Value::Integer(11),
            Value::Integer(11),
            Value::Integer(11),
            Value::Integer(11),
            Value::Integer(11),
            Value::Integer(11),
            Value::Integer(11),
            Value::Integer(11),
        ];
        for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap(),
                expected,
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn recursive_coroutine_closures_survive_gc() {
        let source = br#"
            local x = {"=", "[", "]", "\n"}
            local function gen(c, n)
                if n == 0 then coroutine.yield(c)
                else
                    for _, a in pairs(x) do
                        gen(c .. a, n - 1)
                    end
                end
            end
            local count = 0
            for s in coroutine.wrap(function() gen("", 3) end) do
                count = count + 1
                assert(s ~= nil)
            end
            return count
        "#;
        for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap(),
                vec![Value::Integer(64)],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn debug_getinfo_default_includes_currentline() {
        let source = br#"
            local function inspect()
                return debug.getinfo(1).currentline
            end
            return inspect()
        "#;
        for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap(),
                vec![Value::Integer(3)],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn require_dispatches_through_profile_package_searcher_tables() {
        let source = br#"
            local calls = 0
            local key = _VERSION == "Lua 5.1" and "loaders" or "searchers"
            local searchers = {}
            searchers[1] = function(name)
                calls = calls + 1
                if name == "guest" then
                    return function(module_name)
                        return { name = module_name, answer = 42, extra = "payload" }
                    end, "payload"
                end
            end
            package[key] = searchers
            local value = require("guest")
            return value.name == "guest"
                and value.answer == 42
                and value.extra == "payload"
                and value == package.loaded.guest
                and calls == 1
        "#;
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap(),
                vec![Value::Boolean(true)],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn require_searchers_can_yield_and_resume_for_every_lua_profile() {
        let source = br#"
            local searchers = {
                function(name)
                    coroutine.yield("searcher paused")
                    return function()
                        return 42
                    end
                end
            }
            package.searchers = searchers
            package.loaders = searchers
            local thread = coroutine.create(function()
                return require("yielded")
            end)
            local first, pause = coroutine.resume(thread)
            local second, value = coroutine.resume(thread)
            return first
                and pause == "searcher paused"
                and second
                and value == 42
                and package.loaded.yielded == 42
        "#;
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap(),
                vec![Value::Boolean(true)],
                "profile {profile:?}"
            );
        }
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
                        math.fmod(-7, 3),
                        math.fmod(7, -3),
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
                Value::Number(-1.0),
                Value::Number(1.0),
                Value::Number(4.0),
                Value::Nil,
                Value::Number(6.0),
            ])
        );
        assert_eq!(
            Engine::for_dialect(Dialect::Luau).execute(b"return string.rep('ab', 3, '-')"),
            Ok(vec![Value::String(Arc::from(&b"ababab"[..]))])
        );
        assert!(matches!(
            Engine::default().execute(b"return math.fmod('x', 2)"),
            Err(ExecuteError::Runtime(RuntimeError::Type {
                operation: "math.fmod",
                ..
            }))
        ));
    }

    #[test]
    fn table_unpack_treats_explicit_nil_bounds_as_defaults() {
        let source = br#"
            local values = {10, 20, 30}
            local a, b, c = table.unpack(values, 1, nil)
            local d, e = unpack(values, nil, 2)
            local f, g, h = table.unpack(values, nil, nil)
            return a, b, c, d, e, f, g, h
        "#;
        assert_eq!(
            Engine::default().execute(source),
            Ok(vec![
                Value::Integer(10),
                Value::Integer(20),
                Value::Integer(30),
                Value::Integer(10),
                Value::Integer(20),
                Value::Integer(10),
                Value::Integer(20),
                Value::Integer(30),
            ])
        );
    }

    #[test]
    fn blu_and_luau_table_unpack_match_the_7999_result_boundary() {
        let source = br##"
            local small = table.create(7999, 0)
            local large = table.create(8000, 0)
            local small_ok, small_count = pcall(function()
                return select("#", table.unpack(small))
            end)
            local large_ok = pcall(function()
                return select("#", table.unpack(large))
            end)
            return small_ok and small_count == 7999 and not large_ok
        "##;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            let result = Engine::default()
                .execute_owned_source(source, profile)
                .expect("profile table.unpack boundary should execute");
            assert_eq!(result, vec![Value::Boolean(true)]);
        }
    }

    #[test]
    fn nested_multi_result_calls_expand_in_table_constructors() {
        let values = Engine::default()
            .execute_owned_source(
                "local values = {select(3, unpack{10,20,30,40})}; return #values, values[1], values[2], values[3]",
                SemanticProfile::Luau,
            )
            .unwrap();
        assert_eq!(
            values,
            vec![
                Value::Number(2.0),
                Value::Number(30.0),
                Value::Number(40.0),
                Value::Nil,
            ]
        );
    }

    #[test]
    fn blu_select_accepts_luau_numeric_string_selectors() {
        let source = br##"
            local ok, value = pcall(select, "3", 10, 20, 30)
            return ok and value == 30 and select("#", 10, 20, 30) == 3
        "##;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap(),
                vec![Value::Boolean(true)],
                "profile {profile:?}"
            );
        }
        for profile in [
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap(),
                vec![Value::Boolean(false)],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn function_declaration_rebinds_existing_local() {
        let source = br#"
            local f = function() return "old" end
            function f(value) return value end
            local function outer()
                local g = function() return "old" end
                function g(value) return value + 1 end
                return g(4)
            end
            return f(7), outer()
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap(),
            vec![Value::Integer(7), Value::Integer(5)]
        );
    }

    #[test]
    fn utf8_invalid_code_errors_match_lua_convention() {
        let source = br#"
            local function error_text(f, ...)
                local ok, err = pcall(f, ...)
                return not ok and string.find(err, "invalid UTF%-8 code") ~= nil
            end
            local codepoint_error = error_text(utf8.codepoint, "ab\255", 3, 3)
            local iterator = utf8.codes("ab\255")
            local function next_code()
                iterator("", 0)
                iterator("", 0)
                return iterator("", 0)
            end
            local codes_error = error_text(next_code)
            return codepoint_error, codes_error
        "#;
        let result = Engine::default()
            .execute_owned_source(source, SemanticProfile::Blu)
            .unwrap();
        assert_eq!(result, vec![Value::Boolean(true), Value::Boolean(true)]);
    }

    #[test]
    fn utf8_index_bounds_match_lua_conventions() {
        let source = br#"
            local function errors(f, needle, ...)
                local ok, err = pcall(f, ...)
                return not ok and type(err) == "string"
                    and string.find(err, needle) ~= nil
            end
            return errors(utf8.len, "initial position out of string", "abc", 0, 2),
                errors(utf8.len, "final position out of string", "abc", 1, 4),
                errors(utf8.codepoint, "out of range", "abc", 4),
                errors(utf8.codepoint, "out of range", "abc", -4, 1),
                errors(utf8.codepoint, "out of range", "abc", 1, 4),
                ((_VERSION == "Lua 5.4" or _VERSION == "Lua 5.5")
                    or errors(utf8.char, "value out of range", 0x110000))
        "#;
        for profile in [
            SemanticProfile::Blu,
            SemanticProfile::Luau,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap(),
                vec![
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                ],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn string_find_handles_long_literal_patterns_without_exhausting_budget() {
        let source = br#"
            local subject = string.rep("x", 4000)
            local needle = string.rep("x", 4000)
            local first, last = string.find(subject, needle)
            return first == 1 and last == 4000
        "#;
        for profile in [
            SemanticProfile::Blu,
            SemanticProfile::Luau,
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap(),
                vec![Value::Boolean(true)],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn utf8_charpattern_matches_single_codepoints() {
        let source = r#"
            local pattern = "^" .. utf8.charpattern .. "$"
            return string.find("h", pattern) ~= nil,
                string.find("é", pattern) ~= nil,
                string.find("hello", pattern) == nil
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source.as_bytes(), SemanticProfile::Blu)
                .unwrap(),
            vec![
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
            ]
        );
    }

    #[test]
    fn utf8_offset_returns_final_position_after_last_codepoint() {
        let source = br#"
            return utf8.offset("abc", 2, 3) == 4,
                utf8.offset("abc", 3, 3) == nil,
                utf8.offset("abc", 0, 4) == 4
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap(),
            vec![
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
            ]
        );
    }

    #[test]
    fn unary_minus_coerces_numeric_strings_for_luau_and_lua_profiles() {
        let source = br#"
            local a, b, c = "2", " 3e0 ", "  10  "
            return a + b == 5,
                -b == -3,
                b + "2" == 5,
                "10" - c == 0,
                a ^ b == 8,
                c % a == 0,
                -c == -"  10 "
        "#;
        for profile in [
            SemanticProfile::Blu,
            SemanticProfile::Luau,
            SemanticProfile::Lua51,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap(),
                vec![
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                ],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn string_rep_empty_values_avoid_unbounded_iteration() {
        let source = br#"
            local ok1 = pcall(function() return string.upper("ab\0c") == "AB\0C" end)
            local ok2 = pcall(function() return string.lower("\0ABCc%$") == "\0abcc%$" end)
            local ok3 = pcall(function() return string.rep('teste', 0) == '' end)
            local ok4 = pcall(function() return string.rep('t\195\169s\00t\195\170', 2) == 't\195\169s\0t\195\170t\195\169s\000t\195\170' end)
            local ok5 = pcall(function() return string.rep('', 10) == '' end)
            local ok6 = pcall(function() return string.rep('', 1e9) == '' end)
            local ok7 = pcall(function() return string.rep('x', 2e9) == '' end)
            local ok8 = pcall(function() return string.reverse('') == '' end)
            local ok9 = pcall(function() return string.reverse('\0\1\2\3') == '\3\2\1\0' end)
            local ok10 = pcall(function() return string.reverse('\0001234') == '4321\0' end)
            return ok1, ok2, ok3, ok4, ok5, ok6, ok7, ok8, ok9, ok10
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Luau)
                .unwrap(),
            vec![
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(false),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
            ]
        );
    }

    #[test]
    fn owned_string_method_calls_support_string_receivers() {
        let source = br#"
            return ("\000123456789"):sub(8) == "789",
                ('alo(.)alo'):find('(.)', 1, 1) == 4
        "#;
        for profile in SemanticProfile::ALL {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap(),
                vec![Value::Boolean(true), Value::Boolean(true)],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn owned_zero_result_call_argument_is_elided() {
        let result = Engine::default()
            .execute_owned_source(
                br#"
                    local function nothing() end
                    return pcall(function() return tostring(nothing()) end)
                "#,
                SemanticProfile::Luau,
            )
            .unwrap();
        assert_eq!(result.first(), Some(&Value::Boolean(false)));
    }

    #[test]
    fn string_byte_clamps_an_explicit_end_range_start() {
        let source = br#"
            local first, second, third = string.byte("hi", -3, 100)
            local missing = string.byte("hi", -3)
            return first == string.byte("h") and second == string.byte("i")
                and third == nil and missing == nil
        "#;
        for profile in SemanticProfile::ALL {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap(),
                vec![Value::Boolean(true)],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn owned_luau_string_tostring_edges_match_reference_shape() {
        let source = br#"
            local function check(f)
                local ok, value = pcall(f)
                return ok and value
            end
            local function nothing() end
            local format_ok, format_value = pcall(string.format, "%*", "hi")
            return check(function()
                    for i = 0, 30 do
                        if string.len(string.rep('a', i)) ~= i then return false end
                    end
                    return true
                end),
                check(function() return type(tostring(nil)) == 'string' end),
                check(function() return type(tostring(12)) == 'string' end),
                check(function() return string.find(tostring{}, 'table:') ~= nil end),
                check(function() return string.find(tostring(print), 'function:') ~= nil end),
                check(function() return tostring(1234567890123) == '1234567890123' end),
                check(function() return #tostring('\0') == 1 end),
                check(function() return tostring(true) == 'true' end),
                check(function() return tostring(false) == 'false' end),
                check(function() return not pcall(tostring) end),
                check(function() return not pcall(function() return tostring(nothing()) end) end),
                format_ok,
                format_value,
                check(function()
                    local a = "1234567890"
                    a = string.format("%*%*%*%*%*", a, a, a, a, a)
                    a = string.format("%*%*%*%*%*", a, a, a, a, a)
                    a = string.format("%*%*%*%*%*", a, a, a, a, a)
                    return a == string.rep("1234567890", 125)
                end)
        "#;
        let expected = vec![
            Value::Boolean(true),
            Value::Boolean(true),
            Value::Boolean(true),
            Value::Boolean(true),
            Value::Boolean(true),
            Value::Boolean(true),
            Value::Boolean(true),
            Value::Boolean(true),
            Value::Boolean(true),
            Value::Boolean(true),
            Value::Boolean(true),
            Value::Boolean(true),
            Value::String(Arc::from(&b"hi"[..])),
            Value::Boolean(true),
        ];
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap(),
                expected,
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn string_pack_unpack_and_packsize_follow_modern_profiles() {
        let source = br#"
            local data = string.pack("<I2 i2 f d c3 z s", 4660, -2, 1.5, 2.5, "ab", "z", "hello")
            local a, b, c, d, e, f, g, position = string.unpack(
                "<I2 i2 f d c3 z s", data)
            return string.packsize("<I2 i2 f d c3") == 19
                and a == 4660 and b == -2 and c == 1.5 and d == 2.5
                and e == "ab\0" and f == "z" and g == "hello"
                and position == #data + 1
        "#;
        for profile in [
            SemanticProfile::Blu,
            SemanticProfile::Luau,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("profile {profile:?}: {error}")),
                vec![Value::Boolean(true)],
                "profile {profile:?}"
            );
        }
        for profile in [
            SemanticProfile::Blu,
            SemanticProfile::Luau,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let aligned = Engine::default()
                .execute_owned_source(
                    b"local data=string.pack('>!4 b Xh i4',-12,100) local a,b,p=string.unpack('>!4 b Xh i4',data) return #data==8 and a==-12 and b==100 and p==9",
                    profile,
                )
                .unwrap_or_else(|error| panic!("alignment profile {profile:?}: {error}"));
            assert_eq!(aligned, vec![Value::Boolean(true)], "profile {profile:?}");
        }
        for profile in [SemanticProfile::Lua51, SemanticProfile::Lua52] {
            let result =
                Engine::default().execute_owned_source(b"return string.pack('b', 1)", profile);
            assert!(
                matches!(
                    result,
                    Err(crate::OwnedExecuteError::Runtime(RuntimeError::Type {
                        operation: "call",
                        ..
                    }))
                ),
                "profile {profile:?}: {result:?}"
            );
        }
    }

    #[test]
    fn string_unpack_wide_unsigned_values_preserve_profile_number_models() {
        let source = br#"
            local lnum = 0x060504030201
            local packed = string.pack("<l", -lnum)
            local actual = string.unpack("<I12", packed .. string.rep("\0", 4))
            local expected = 2^64 - lnum
            return actual == expected, type(actual), type(expected)
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Blu)
                .unwrap(),
            vec![
                Value::Boolean(true),
                Value::String(Arc::from(&b"number"[..])),
                Value::String(Arc::from(&b"number"[..])),
            ]
        );
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Luau)
                .unwrap(),
            vec![
                Value::Boolean(true),
                Value::String(Arc::from(&b"number"[..])),
                Value::String(Arc::from(&b"number"[..])),
            ]
        );
    }

    #[test]
    fn string_pack_alignment_and_boundary_errors_match_lua_family() {
        let source = br#"
            local x = string.pack(">!4 c3 c4 c2 z i4 c5 c2 Xi4",
                "abc", "abcd", "xz", "hello", 5, "world", "xy")
            local a, b, c, d, e, f, g, pos = string.unpack(
                ">!4 c3 c4 c2 z i4 c5 c2 Xh Xi4", x)
            local packed = string.pack("i4i4i4i4", 1, 2, 3, 4)
            local first, first_position = string.unpack("!4 i4", packed, 0)
            local invalid_x, invalid_x_error = pcall(string.pack, "X")
            local too_long, too_long_error = pcall(string.pack, "c3", "1234")
            local too_short, too_short_error = pcall(string.unpack, "c5", "abcd")
            local too_large, too_large_error = pcall(string.packsize, "c2147483648")
            return x == "abcabcdxzhello\0\0\0\0\0\5worldxy\0",
                a == "abc", b == "abcd", c == "xz", d == "hello",
                e == 5, f == "world", g == "xy", pos == 29,
                first == 1, first_position == 5,
                not invalid_x and string.find(invalid_x_error, "invalid next option") ~= nil,
                not too_long and string.find(too_long_error, "longer than") ~= nil,
                not too_short and string.find(too_short_error, "too short") ~= nil,
                not too_large and string.find(too_large_error, "too large") ~= nil,
                string.packsize("c1073741824") == 1073741824
        "#;
        for profile in [
            SemanticProfile::Blu,
            SemanticProfile::Luau,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}")),
                vec![Value::Boolean(true); 16],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn lua51_string_gfind_is_the_gmatch_alias() {
        assert_eq!(
            Engine::default()
                .execute_owned_source(
                    r#"
                        local iterator = string.gfind("a 42 b", "%a+")
                        local first = iterator()
                        local second = iterator()
                        return string.gfind == string.gmatch
                            and first == "a" and second == "b"
                    "#,
                    SemanticProfile::Lua51,
                )
                .unwrap(),
            vec![Value::Boolean(true)]
        );
        for profile in [
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
            SemanticProfile::Blu,
            SemanticProfile::Luau,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(
                        r#"return type(string.gfind) == "nil" and rawget(string, "gfind") == nil"#,
                        profile,
                    )
                    .unwrap(),
                vec![Value::Boolean(true)],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn lua51_and_lua52_module_compatibility_matches_puc_surface() {
        let lua51 = Engine::default()
            .execute_owned_source(
                r#"
                    local set_environment, get_environment = setfenv, getfenv
                    local package_table, global, module_function, to_string = package, _G, module, tostring
                    local old_environment = get_environment(0)
                    local result = module_function("foo.bar", package_table.seeall)
                    local active = package_table.loaded["foo.bar"]
                    local compatible = result == nil
                        and _M == active
                        and active._M == active
                        and active._NAME == "foo.bar"
                        and active._PACKAGE == "foo."
                        and active.print == print
                        and package_table.loaded["foo.bar"] == active
                        and global.foo.bar == active
                    set_environment(0, old_environment)
                    return compatible
                "#,
                SemanticProfile::Lua51,
            )
            .unwrap();
        assert_eq!(lua51, vec![Value::Boolean(true)]);

        let lua52 = Engine::default()
            .execute_owned_source(
                r#"
                    local kind, raw, package_table, global, module_function =
                        type, rawget, package, _G, module
                    local result = module_function("foo.bar", package_table.seeall)
                    local active = package_table.loaded["foo.bar"]
                    return kind(result) == "table"
                        and result == active
                        and active._M == active
                        and active._NAME == "foo.bar"
                        and active._PACKAGE == "foo."
                        and active.print == print
                        and global.foo.bar == active
                        and raw(global, "foo") ~= nil
                "#,
                SemanticProfile::Lua52,
            )
            .unwrap();
        assert_eq!(lua52, vec![Value::Boolean(true)]);

        for profile in [
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
            SemanticProfile::Blu,
            SemanticProfile::Luau,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(
                        r#"return type(module) == "nil"
                            and (type(package) == "nil" or rawget(package, "seeall") == nil)"#,
                        profile,
                    )
                    .unwrap(),
                vec![Value::Boolean(true)],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn string_format_general_conversions_match_profile_references() {
        let source = br#"
            return string.format(
                "%.3g|%.3G|%10.3g|%-10.3g|%.5g|%.3g|%.3g|%.3g|%.3G|%.0g",
                12.34, 1234.5, 12.34, 12.34, 0.000012345, 999.5, 0.0001, 0.00001, 1234.5, 123.5)
        "#;
        let expected = Value::String(Arc::from(
            &b"12.3|1.23E+03|      12.3|12.3      |1.2345e-05|1e+03|0.0001|1e-05|1.23E+03|1e+02"[..],
        ));
        for profile in [
            SemanticProfile::Blu,
            SemanticProfile::Luau,
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("profile {profile:?}: {error}")),
                vec![expected.clone()],
                "profile {profile:?}"
            );
        }
    }

    #[test]
    fn blu_tonumber_boundary_conversions_are_profile_typed() {
        let values = Engine::default()
            .execute_owned_source(
                br#"
                    local function shape(value)
                        return type(value) .. ":" .. tostring(value == nil) .. ":" .. tostring(value == math.huge)
                    end
                    local function integer_shape(value)
                        local subtype = math.type(value)
                        return type(value) .. ":" .. tostring(value == -1) .. ":" .. tostring(subtype)
                    end
                    return shape(tonumber("inf")),
                        shape(tonumber("-inf")),
                        shape(tonumber("nan")),
                        integer_shape(tonumber("0xFFFFFFFFFFFFFFFF")),
                        math.type(tonumber("9223372036854775807")),
                        math.type(tonumber("9223372036854775808"))
                "#,
                SemanticProfile::Blu,
            )
            .unwrap();
        assert_eq!(
            values,
            vec![
                Value::String(Arc::from(&b"nil:true:false"[..])),
                Value::String(Arc::from(&b"nil:true:false"[..])),
                Value::String(Arc::from(&b"nil:true:false"[..])),
                Value::String(Arc::from(&b"number:true:integer"[..])),
                Value::String(Arc::from(&b"integer"[..])),
                Value::String(Arc::from(&b"float"[..])),
            ]
        );
    }

    #[test]
    fn string_dump_round_trips_owned_functions_and_strips_debug_metadata() {
        let source = br#"
            local function answer()
                return 42
            end
            local dumped = string.dump(answer)
            local loader = loadstring or load
            local restored = assert(loader(dumped))
            local stripped = string.dump(answer, true)
            local restored_stripped = assert(loader(stripped))
            return type(dumped) == "string"
                and #dumped > 0
                and restored() == 42
                and #stripped > 0
                and restored_stripped() == 42
        "#;
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let values = Engine::default()
                .execute_owned_source(source, profile)
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(values, vec![Value::Boolean(true)], "profile {profile:?}");
        }
    }

    #[test]
    fn string_dump_round_trips_captured_upvalues_with_a_fresh_environment() {
        let source = br#"
            local captured = 41
            local function answer()
                return captured
            end
            local dumped = string.dump(answer)
            local loader = loadstring or load
            local restored = assert(loader(dumped))
            local name, value = debug.getupvalue(restored, 1)
            local ok, result = pcall(restored)
            return name == "captured"
                and ((_VERSION == "Lua 5.1" and value == nil)
                    or (_VERSION ~= "Lua 5.1" and type(value) == "table"))
                and ok
                and result == value
        "#;
        for profile in [
            SemanticProfile::Lua51,
            SemanticProfile::Lua52,
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            let values = Engine::default()
                .execute_owned_source(source, profile)
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(values, vec![Value::Boolean(true)], "profile {profile:?}");
        }
    }

    #[test]
    fn lua55_named_vararg_table_survives_main_closure_execution() {
        let source = br#"
            local function collect(... args)
                return args.n, args[1], args[2]
            end
            local count, first, second = collect(3, 4)
            return count == 2 and first == 3 and second == 4
        "#;
        let values = Engine::default()
            .execute_owned_source(source, SemanticProfile::Lua55)
            .unwrap();
        assert_eq!(values, vec![Value::Boolean(true)]);
    }

    #[test]
    fn lua55_named_vararg_reads_do_not_change_gc_count() {
        let source = br#"
            local function notab(keys, values, ...args)
                for _, key in pairs(keys) do
                    assert(values[key] == args[key])
                end
                assert(values.n == args.n)
            end
            local values = table.pack(10, 20, 30)
            local keys = {-1, 0, 1, values.n, values.n + 1, 1.0, 1.1,
                "n", print, "k", "1"}
            notab(keys, values, 10, 20, 30)
            local before = collectgarbage("count")
            notab(keys, values, 10, 20, 30)
            return before == collectgarbage("count")
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Lua55)
                .unwrap(),
            vec![Value::Boolean(true)]
        );
    }

    #[test]
    fn lua55_named_varargs_drive_mutable_expansion_and_validate_n() {
        let source = br#"
            local function collect(a, values, ...args)
                for key, value in pairs(values) do args[key] = value end
                return ...
            end
            local collected = table.pack(collect(10, {[1] = 11, [5] = 24}, 1, 2, 3, nil, 4))

            local function expand(a, b, n, ...args)
                args.n = n
                return b, ...
            end
            local expanded = table.pack(expand(10, 1, 10000))

            local function invalid(...args)
                args.n = math.maxinteger
                return ...
            end
            local ok, message = pcall(invalid)
            return collected.n == 5
                and collected[1] == 11
                and collected[2] == 2
                and collected[3] == 3
                and collected[4] == nil
                and collected[5] == 24
                and expanded.n == 10001
                and expanded[1] == 1
                and not ok
                and string.find(message, "no proper 'n'") ~= nil
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Lua55)
                .unwrap(),
            vec![Value::Boolean(true)]
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
    fn protected_calls_from_coroutine_roots_reserve_service_depth() {
        let mut engine = Engine::default();
        *engine.vm_mut() = Vm::default().with_call_limit(64);
        let results = engine
            .execute_owned_source(
                br#"
                    local function recurse(depth)
                        return depth <= 1 and 1 or recurse(depth - 1) + 1
                    end
                    local pcall_thread = coroutine.create(function()
                        coroutine.yield("pcall pause")
                        return pcall(recurse, 62)
                    end)
                    local xpcall_thread = coroutine.create(function()
                        coroutine.yield("xpcall pause")
                        return xpcall(recurse, function()
                            return "handled"
                        end, 62)
                    end)
                    local pcall_started, pcall_pause = coroutine.resume(pcall_thread)
                    local pcall_resumed, pcall_ok, pcall_message =
                        coroutine.resume(pcall_thread)
                    local xpcall_started, xpcall_pause = coroutine.resume(xpcall_thread)
                    local xpcall_resumed, xpcall_ok, xpcall_value =
                        coroutine.resume(xpcall_thread)
                    return pcall_started, pcall_pause, pcall_resumed,
                        pcall_ok, pcall_message, xpcall_started, xpcall_pause,
                        xpcall_resumed, xpcall_ok, xpcall_value,
                        coroutine.status(pcall_thread), coroutine.status(xpcall_thread)
                "#,
                SemanticProfile::Blu,
            )
            .unwrap();
        assert_eq!(
            results,
            vec![
                Value::Boolean(true),
                Value::String(Arc::from(&b"pcall pause"[..])),
                Value::Boolean(true),
                Value::Boolean(false),
                Value::String(Arc::from(&b"call depth limit 64 exceeded"[..])),
                Value::Boolean(true),
                Value::String(Arc::from(&b"xpcall pause"[..])),
                Value::Boolean(true),
                Value::Boolean(false),
                Value::String(Arc::from(&b"handled"[..])),
                Value::String(Arc::from(&b"dead"[..])),
                Value::String(Arc::from(&b"dead"[..])),
            ]
        );
    }

    #[test]
    fn recursive_protected_calls_use_iterative_owned_activations() {
        let results = Engine::default()
            .execute_owned_source(
                br#"
                    local function recurse(depth)
                        if depth == 0 then
                            return "done"
                        end
                        return pcall(recurse, depth - 1)
                    end
                    local result = { pcall(recurse, 64) }
                    local function xrecurse()
                        return xpcall(xrecurse, function(error_value)
                            return error_value
                        end)
                    end
                    local xresult = { xpcall(xrecurse, function(error_value)
                        return error_value
                    end) }
                    return #result, result[#result - 1], result[#result],
                        #xresult, xresult[#xresult - 1], xresult[#xresult]
                "#,
                SemanticProfile::Blu,
            )
            .unwrap();
        assert_eq!(
            results,
            vec![
                Value::Number(66.0),
                Value::Boolean(true),
                Value::String(Arc::from(&b"done"[..])),
                Value::Number(500.0),
                Value::Boolean(false),
                Value::String(Arc::from(&b"call depth limit 1000 exceeded"[..])),
            ]
        );
    }

    #[test]
    fn iterative_protected_calls_still_honor_the_vm_call_limit() {
        let mut engine = Engine::default();
        *engine.vm_mut() = Vm::default().with_call_limit(16);
        let results = engine
            .execute_owned_source(
                br#"
                    local function recurse(depth)
                        if depth == 0 then
                            return "done"
                        end
                        return pcall(recurse, depth - 1)
                    end
                    local result = { pcall(recurse, 64) }
                    return #result, result[#result - 1], result[#result]
                "#,
                SemanticProfile::Blu,
            )
            .unwrap();
        assert_eq!(
            results,
            vec![
                Value::Number(8.0),
                Value::Boolean(false),
                Value::String(Arc::from(&b"call depth limit 16 exceeded"[..])),
            ]
        );
    }

    #[test]
    fn luau_protected_call_reports_stack_overflow_like_luau() {
        let result = Engine::default()
            .execute_owned_source(
                br#"
                    local function recurse()
                        recurse()
                    end
                    local ok, message = pcall(recurse)
                    return ok, string.match(message, "^.-:%d+: stack overflow") ~= nil
                "#,
                SemanticProfile::Luau,
            )
            .unwrap();
        assert_eq!(result, vec![Value::Boolean(false), Value::Boolean(true)]);
    }

    #[test]
    fn owned_xpcall_validates_handler_arguments_before_target_execution() {
        let results = Engine::default()
            .execute_owned_source(
                br#"
                    local missing_ok, missing_message = pcall(xpcall, function()
                        return 42
                    end)
                    local invalid_ok, invalid_message = pcall(xpcall, function()
                        return 42
                    end, true)
                    return missing_ok, missing_message, invalid_ok, invalid_message
                "#,
                SemanticProfile::Luau,
            )
            .unwrap();
        assert_eq!(
            results,
            vec![
                Value::Boolean(false),
                Value::String(Arc::from(
                    &b"missing argument #2 to 'xpcall' (function expected)"[..],
                )),
                Value::Boolean(false),
                Value::String(Arc::from(
                    &b"invalid argument #2 to 'xpcall' (function expected, got boolean)"[..],
                )),
            ]
        );
    }

    #[test]
    fn iterative_protected_call_frames_root_results_across_gc() {
        let results = Engine::default()
            .execute_owned_source(
                br#"
                    local function recurse(depth)
                        local value = { depth }
                        if depth == 0 then
                            collectgarbage("collect")
                            return value
                        end
                        local result = { pcall(recurse, depth - 1) }
                        collectgarbage("collect")
                        return table.unpack(result)
                    end
                    local result = { pcall(recurse, 128) }
                    return type(result[#result]) == "table"
                        and result[#result][1] == 0
                "#,
                SemanticProfile::Blu,
            )
            .unwrap();
        assert_eq!(results, vec![Value::Boolean(true)]);
    }

    #[test]
    fn xpcall_handler_nested_protection_is_an_explicit_boundary() {
        let error = Engine::default()
            .execute_owned_source(
                br#"
                    local ok, message = xpcall(function()
                        error("outer")
                    end, function()
                        return pcall(function()
                            return "inner"
                        end)
                    end)
                    return ok, message
                "#,
                SemanticProfile::Blu,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            OwnedExecuteError::Runtime(RuntimeError::UnsupportedLibraryFeature {
                function: "xpcall",
                feature: "protected activation from an error handler",
            })
        ));
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
                Value::String(Arc::from(&b"error in error handling"[..])),
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
    fn coroutine_wrap_trampolines_deep_generator_chain() {
        let source = br#"
            local function gen(n)
                return coroutine.wrap(function()
                    for i = 2, n do coroutine.yield(i) end
                end)
            end
            local function filter(p, g)
                return coroutine.wrap(function()
                    while true do
                        local n = g()
                        if n == nil then return end
                        if n % p ~= 0 then coroutine.yield(n) end
                    end
                end)
            end
            local g = gen(1000)
            local count = 0
            while true do
                local n = g()
                if n == nil then break end
                count = count + 1
                g = filter(n, g)
            end
            return count
        "#;
        assert_eq!(
            Engine::default()
                .execute_owned_source(source, SemanticProfile::Lua51)
                .unwrap(),
            vec![Value::Number(168.0)]
        );
    }

    #[test]
    fn owned_generic_for_coroutine_iterator_survives_collection() {
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            let result = Engine::default()
                .execute_owned_source(
                    br#"
                    local x = {"=", "[", "]", "\n"}
                    local len = 4
                    local function gen(c, n)
                        if n == 0 then coroutine.yield(c)
                        else
                            for _, a in pairs(x) do
                                gen(c .. a, n - 1)
                            end
                        end
                    end
                    local count = 0
                    for s in coroutine.wrap(function() gen("", len) end) do
                        count = count + 1
                        local loaded = assert(loadstring("return [====[\n" .. s .. "]====]"))
                        collectgarbage("collect")
                        assert(s == loaded())
                    end
                    return count
                "#,
                    profile,
                )
                .unwrap_or_else(|error| panic!("profile {profile:?}: {error:?}"));
            assert_eq!(result, vec![Value::Integer(256)], "profile {profile:?}");
        }
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
                Value::String(Arc::from(&b"nil"[..])),
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

    #[test]
    fn owned_luau_native_callbacks_are_not_yieldable() {
        let result = Engine::default()
            .execute_owned_source(
                br#"
                    local callback_yieldable
                    local result = string.gsub("a", ".", function()
                        callback_yieldable = coroutine.isyieldable()
                        return "b"
                    end)
                    return result, callback_yieldable, coroutine.isyieldable()
                "#,
                SemanticProfile::Luau,
            )
            .expect("owned Luau native callback probe should execute");
        assert_eq!(
            result,
            [
                Value::String(Arc::from(&b"b"[..])),
                Value::Boolean(false),
                Value::Boolean(true),
            ]
        );
    }

    #[test]
    fn owned_luau_coroutine_close_rejects_normal_coroutines() {
        let result = Engine::default()
            .execute_owned_source(
                br#"
                    local outer = coroutine.create(function()
                        local current = coroutine.running()
                        local inner = coroutine.wrap(function()
                            return pcall(coroutine.close, current)
                        end)
                        return inner()
                    end)
                    local resumed, ok, message = coroutine.resume(outer)
                    return resumed, ok, message
                "#,
                SemanticProfile::Luau,
            )
            .expect("owned Luau normal-coroutine close probe should execute");
        assert_eq!(
            result,
            [
                Value::Boolean(true),
                Value::Boolean(false),
                Value::String(Arc::from(&b"cannot close normal coroutine"[..])),
            ]
        );
    }

    #[test]
    fn owned_luau_wrapped_normal_coroutine_close_matches_official_fixture() {
        let result = Engine::default()
            .execute_owned_source(
                br#"
                    local ok, message
                    local outer = coroutine.wrap(function()
                        local current = coroutine.running()
                        coroutine.wrap(function()
                            ok, message = pcall(coroutine.close, current)
                        end)()
                    end)
                    outer()
                    return ok, message
                "#,
                SemanticProfile::Luau,
            )
            .expect("owned Luau wrapped normal-coroutine close probe should execute");
        assert_eq!(
            result,
            [
                Value::Boolean(false),
                Value::String(Arc::from(&b"cannot close normal coroutine"[..])),
            ]
        );
    }

    #[test]
    fn owned_luau_coroutine_close_consumes_dead_errors_once() {
        let result = Engine::default()
            .execute_owned_source(
                br#"
                    local co = coroutine.create(error)
                    local object = { 42 }
                    local resumed, first = coroutine.resume(co, object)
                    local closed, second = coroutine.close(co)
                    local closed_again, third = coroutine.close(co)
                    return not resumed, first == object,
                        not closed, second == object,
                        closed_again, third == nil
                "#,
                SemanticProfile::Luau,
            )
            .expect("owned Luau dead-error close probe should execute");
        assert_eq!(result, vec![Value::Boolean(true); 6]);
    }

    #[test]
    fn owned_luau_xpcall_handlers_cannot_yield_across_the_c_call() {
        let result = Engine::default()
            .execute_owned_source(
                br#"
                    local co = coroutine.wrap(xpcall)
                    co(0, coroutine.yield, 0)
                    local status, message = pcall(co, 0, 0, 0)
                    return status, message
                "#,
                SemanticProfile::Luau,
            )
            .expect("owned Luau xpcall non-yieldable probe should execute");
        assert_eq!(
            result,
            [
                Value::Boolean(false),
                Value::String(Arc::from(&b"cannot resume dead coroutine"[..])),
            ]
        );
    }

    #[test]
    fn owned_luau_collected_coroutine_wrappers_leave_weak_values() {
        let result = Engine::default()
            .execute_owned_source(
                br#"
                    local C = {}
                    setmetatable(C, { __mode = "kv" })
                    local x = coroutine.wrap(function()
                        local a = 10
                        local function f()
                            a = a + 10
                            return a
                        end
                        while true do
                            a = a + 1
                            coroutine.yield(f)
                        end
                    end)
                    C[1] = x
                    local f = x()
                    assert(f() == 21 and x()() == 32 and x() == f)
                    x = nil
                    collectgarbage()
                    return C[1] == nil, f() == 43, f() == 53
                "#,
                SemanticProfile::Luau,
            )
            .expect("owned Luau coroutine weak-value probe should execute");
        assert_eq!(
            result,
            [
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
            ]
        );
    }

    #[test]
    fn coroutine_close_unwinds_suspended_to_be_closed_values() {
        let source = br#"
            local events = ""
            local paused = coroutine.create(function()
                local resource <close> = setmetatable({}, {
                    __close = function()
                        events = events .. "closed"
                    end,
                })
                coroutine.yield("pause")
            end)
            local resumed, signal = coroutine.resume(paused)
            local closed, close_error = coroutine.close(paused)

            local failing = coroutine.create(function()
                local resource <close> = setmetatable({}, {
                    __close = function()
                        events = events .. "error"
                        error("close failure")
                    end,
                })
                coroutine.yield("pause")
            end)
            coroutine.resume(failing)
            local failed, failure = coroutine.close(failing)
            local closed_again = coroutine.close(failing)
            return resumed and signal == "pause"
                and closed and close_error == nil and events == "closederror"
                and not failed and type(failure) == "string" and closed_again
                and coroutine.status(paused) == "dead"
                and coroutine.status(failing) == "dead"
        "#;
        for profile in [SemanticProfile::Lua54, SemanticProfile::Lua55] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile}: {error:?}")),
                vec![Value::Boolean(true)],
                "{profile}"
            );
        }
    }

    #[test]
    fn luau_typed_function_annotations_are_runtime_transparent() {
        let source = br#"
            local function regularadd(depth: number, a: number, b: number): number
                if depth == 0 then return 1 end
                return a + regularadd(depth - 1, b, a + b)
            end
            local function protectedadd(depth: number, a: number, b: number): number
                if depth == 0 then return 1 end
                local ok, result = pcall(protectedadd, depth - 1, b, a + b)
                assert(ok)
                return a + result
            end
            return regularadd(4, 0, 1) == protectedadd(4, 0, 1)
        "#;
        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Engine::default()
                    .execute_owned_source(source, profile)
                    .unwrap_or_else(|error| panic!("{profile}: {error:?}")),
                vec![Value::Boolean(true)],
                "{profile}"
            );
        }
    }

    #[test]
    fn luau_debug_traceback_includes_owned_function_names() {
        let result = Engine::default()
            .execute_owned_source_named(
                br#"
                    local function foo()
                        return debug.traceback()
                    end
                    local trace = foo()
                    return trace
                "#,
                "debug.luau",
                SemanticProfile::Luau,
            )
            .unwrap_or_else(|error| panic!("{error}"));
        let Value::String(trace) = &result[0] else {
            panic!("expected traceback string, got {:?}", result[0]);
        };
        assert!(
            trace
                .windows(b"function foo".len())
                .any(|window| window == b"function foo"),
            "trace: {:?}",
            String::from_utf8_lossy(trace)
        );
    }

    #[test]
    fn luau_debug_traceback_reports_suspended_thread_frames() {
        let result = Engine::default()
            .execute_owned_source_named(
                br#"
                    local function bar()
                        coroutine.yield()
                    end
                    local co = coroutine.create(bar)
                    coroutine.resume(co)
                    local trace = debug.traceback(co)
                    local with_message = debug.traceback(co, "hello")
                    local skipped = debug.traceback(co, "hello", 2)
                    return string.find(trace, "function bar", 1, true) ~= nil
                        and string.find(with_message, "hello", 1, true) ~= nil
                        and string.find(with_message, "function bar", 1, true) ~= nil
                        and string.find(skipped, "hello", 1, true) ~= nil
                        and string.find(skipped, "function bar", 1, true) == nil
                "#,
                "debug.luau",
                SemanticProfile::Luau,
            )
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(result, vec![Value::Boolean(true)]);
    }
}
