use crate::heap::UpvalueId;
use crate::{
    ClosureId, Dialect, Heap, HeapError, MemoryConfig, MemoryError, MemoryUsage, NativeFunctionId,
    TableId, ThreadId, Value, checked_vector_bytes,
};
use blu_bytecode::{
    Chunk, Constant, Instruction, MAX_TABLE_INITIAL_CAPACITY, Opcode, Prototype, ValidatedChunk,
    ValidationError,
    blu::{
        Artifact as BluArtifact, BluLimits, Constant as BluConstant, Instruction as BluInstruction,
        TranslatedChunk, ValidatedArtifact as ValidatedBluArtifact,
        ValidationError as BluValidationError,
    },
};
use blu_core::SemanticProfile;
use core::fmt;
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    fmt::Write as _,
    sync::Arc,
};

const MAX_DYNAMIC_REGISTERS: usize = 1_000_000;
const MAX_STRING_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_HOST_VALUE_LIMIT: usize = 4096;
const DEFAULT_NATIVE_RESULT_LIMIT: usize = MAX_DYNAMIC_REGISTERS;
const DEFAULT_NATIVE_FUNCTION_LIMIT: usize = 1_000_000;
const DEFAULT_GLOBAL_LIMIT: usize = 1_000_000;
const BUILTIN_NATIVE_CAPACITY: usize = 128;
const BUILTIN_GLOBAL_CAPACITY: usize = 64;

type NativeFunction =
    Arc<dyn Fn(&mut Vm, &[Value]) -> Result<Vec<Value>, RuntimeError> + Send + Sync>;
type ModuleLoader = Arc<dyn Fn(&mut Vm, &[u8]) -> Result<Value, RuntimeError> + Send + Sync>;

#[derive(Clone, Debug)]
enum ThreadState {
    New(Value),
    Suspended(Continuation),
    Running,
    Dead(Option<Value>),
}

enum Resumable {
    New(Value),
    Continuation(Continuation),
}

#[derive(Clone, Copy, Debug)]
struct FrameTarget {
    prototype_index: usize,
    profile: SemanticProfile,
}

#[derive(Clone, Debug, Default)]
struct GcRoots {
    values: Vec<Value>,
    upvalues: Vec<UpvalueId>,
}

#[derive(Debug)]
struct BluCaller {
    artifact: Arc<BluArtifact>,
    prototype: usize,
    constants: Vec<Value>,
    registers: Vec<Value>,
    varargs: Vec<Value>,
    open_upvalues: Vec<Option<UpvalueId>>,
    closure: Option<ClosureId>,
    pc: usize,
    result: BluCallResult,
}

#[derive(Debug)]
enum BluCallResult {
    Fixed { destination: u16, count: u16 },
    Truthy { destination: u16, negate: bool },
    ReturnPrefix { first: u16, count: u16 },
    TableList { table: TableId, start: u32 },
    Dynamic,
    ReadyDynamic(Vec<Value>),
}

enum BluReturnDisposition {
    Resume(BluCaller),
    Propagate(Vec<Value>),
}

enum BluIndexResolution {
    Value(Value),
    Call { function: Value, receiver: Value },
}

enum BluNewIndexResolution {
    Raw(TableId),
    Call { function: Value, receiver: Value },
}

impl GcRoots {
    fn from_values(values: &[Value]) -> Result<Self, RuntimeError> {
        Ok(Self {
            values: try_clone_values(values, "GC roots")?,
            upvalues: Vec::new(),
        })
    }

    fn push_value(&mut self, value: Value) -> Result<(), RuntimeError> {
        try_reserve_exact(&mut self.values, 1, "GC roots")?;
        self.values.push(value);
        Ok(())
    }

    fn try_clone(&self) -> Result<Self, RuntimeError> {
        Ok(Self {
            values: try_clone_slice(&self.values, "GC roots")?,
            upvalues: try_clone_slice(&self.upvalues, "GC upvalue roots")?,
        })
    }

    fn extend(&mut self, other: Self) -> Result<(), RuntimeError> {
        try_reserve_exact(&mut self.values, other.values.len(), "GC roots")?;
        try_reserve_exact(&mut self.upvalues, other.upvalues.len(), "GC upvalue roots")?;
        self.values.extend(other.values);
        self.upvalues.extend(other.upvalues);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum HostRoot {
    Table(TableId),
    Closure(ClosureId),
    Thread(ThreadId),
}

impl HostRoot {
    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Table(value) => Some(Self::Table(*value)),
            Value::Closure(value) => Some(Self::Closure(*value)),
            Value::Thread(value) | Value::CoroutineFunction(value) => Some(Self::Thread(*value)),
            _ => None,
        }
    }

    fn to_value(self) -> Value {
        match self {
            Self::Table(value) => Value::Table(value),
            Self::Closure(value) => Value::Closure(value),
            Self::Thread(value) => Value::Thread(value),
        }
    }
}

#[derive(Clone)]
pub struct Vm {
    dialect: Dialect,
    active_profile: Option<SemanticProfile>,
    instruction_limit: u64,
    call_limit: usize,
    heap_object_limit: usize,
    heap: Heap,
    globals: HashMap<Arc<[u8]>, Value>,
    native_functions: Vec<NativeFunction>,
    protected_call: Option<NativeFunctionId>,
    error_handler_call: Option<NativeFunctionId>,
    coroutine_resume: Option<NativeFunctionId>,
    coroutine_yield: Option<NativeFunctionId>,
    module_loader: Option<ModuleLoader>,
    module_cache: HashMap<Arc<[u8]>, Value>,
    loading_modules: HashSet<Arc<[u8]>>,
    threads: HashMap<ThreadId, ThreadState>,
    main_thread: ThreadId,
    running_thread: Option<ThreadId>,
    output: Vec<u8>,
    output_limit: usize,
    active_roots: Vec<GcRoots>,
    host_roots: HashMap<HostRoot, usize>,
    host_root_count: usize,
    host_value_limit: usize,
    native_result_limit: usize,
    native_function_limit: usize,
    global_limit: usize,
    random_state: u64,
}

impl Default for Vm {
    fn default() -> Self {
        Self::new(Dialect::Blu)
    }
}

impl Vm {
    /// Creates a VM, returning structured heap-allocation failures from
    /// built-in initialization.
    pub fn try_new(dialect: Dialect) -> Result<Self, RuntimeError> {
        Self::try_new_with_memory(dialect, MemoryConfig::default())
    }

    /// Creates a VM with deterministic accounting for heap-owned storage.
    ///
    /// Direct BluV1 constants, registers, string payload copies, and return
    /// buffers are transiently charged. Legacy Luau chunks, their frames,
    /// native-owned values, and GC work queues are not all included in this
    /// stage's reported usage. Logical charged capacities are conservative but
    /// are not an exact process RSS measurement.
    pub fn try_new_with_memory(
        dialect: Dialect,
        memory: MemoryConfig,
    ) -> Result<Self, RuntimeError> {
        let mut heap = Heap::try_new(memory)?;
        let main_thread = heap.allocate_thread(&[])?;
        let mut threads = HashMap::new();
        threads.insert(main_thread, ThreadState::Running);
        let mut vm = Self {
            dialect,
            active_profile: None,
            instruction_limit: 10_000_000,
            call_limit: 1_000,
            heap_object_limit: 1_000_000,
            heap,
            globals: HashMap::new(),
            native_functions: Vec::new(),
            protected_call: None,
            error_handler_call: None,
            coroutine_resume: None,
            coroutine_yield: None,
            module_loader: None,
            module_cache: HashMap::new(),
            loading_modules: HashSet::new(),
            threads,
            main_thread,
            running_thread: None,
            output: Vec::new(),
            output_limit: MAX_STRING_BYTES,
            active_roots: Vec::new(),
            host_roots: HashMap::new(),
            host_root_count: 0,
            host_value_limit: DEFAULT_HOST_VALUE_LIMIT,
            native_result_limit: DEFAULT_NATIVE_RESULT_LIMIT,
            native_function_limit: DEFAULT_NATIVE_FUNCTION_LIMIT,
            global_limit: DEFAULT_GLOBAL_LIMIT,
            random_state: 0x4d59_5df4_d0f3_3173,
        };
        vm.native_functions
            .try_reserve(BUILTIN_NATIVE_CAPACITY)
            .map_err(|_| RuntimeError::Allocation {
                what: "built-in native function registry",
            })?;
        vm.globals
            .try_reserve(BUILTIN_GLOBAL_CAPACITY)
            .map_err(|_| RuntimeError::Allocation {
                what: "built-in global registry",
            })?;
        vm.install_base_library()?;
        Ok(vm)
    }

    /// Creates a VM using the compatibility infallible constructor.
    ///
    /// This panics if the fixed built-in heap cannot be initialized. Embedders
    /// that need structured initialization failures should use [`Self::try_new`].
    #[must_use]
    pub fn new(dialect: Dialect) -> Self {
        Self::try_new(dialect).expect("fixed VM built-ins must fit in memory")
    }

    #[must_use]
    pub fn with_instruction_limit(mut self, limit: u64) -> Self {
        self.instruction_limit = limit;
        self
    }

    #[must_use]
    pub fn with_call_limit(mut self, limit: usize) -> Self {
        self.call_limit = limit;
        self
    }

    #[must_use]
    pub fn with_output_limit(mut self, limit: usize) -> Self {
        self.output_limit = limit;
        self
    }

    #[must_use]
    pub fn with_heap_object_limit(mut self, limit: usize) -> Self {
        self.heap_object_limit = limit;
        self
    }

    /// Sets the maximum number of registered native functions.
    #[must_use]
    pub fn with_native_function_limit(mut self, limit: usize) -> Self {
        self.native_function_limit = limit;
        self
    }

    /// Sets the maximum number of distinct global names.
    ///
    /// Replacing an existing global remains legal at the limit.
    #[must_use]
    pub fn with_global_limit(mut self, limit: usize) -> Self {
        self.global_limit = limit;
        self
    }

    /// Sets the maximum number of heap-handle occurrences retained for the
    /// host, whether automatically from `execute*` or through
    /// [`Self::retain_value`].
    ///
    /// The default is 4096. An operation that would exceed this limit returns
    /// [`RuntimeError::HostValueLimit`] without retaining only part of its
    /// result.
    #[must_use]
    pub fn with_host_value_limit(mut self, limit: usize) -> Self {
        self.host_value_limit = limit;
        self
    }

    /// Sets the maximum number of values accepted from one native callback.
    ///
    /// The default matches the dynamic-register limit. Oversized results are
    /// rejected before any caller frame is modified.
    #[must_use]
    pub fn with_native_result_limit(mut self, limit: usize) -> Self {
        self.native_result_limit = limit;
        self
    }

    #[must_use]
    /// Returns the low-level heap for inspection.
    ///
    /// Heap handles cloned from values read through this reference are not
    /// retained automatically. Call [`Self::retain_value`] before removing
    /// their VM root or allowing a later VM operation to collect.
    pub const fn heap(&self) -> &Heap {
        &self.heap
    }

    #[must_use]
    pub const fn memory_usage(&self) -> MemoryUsage {
        self.heap.memory_usage()
    }

    #[must_use]
    pub const fn dialect(&self) -> Dialect {
        self.dialect
    }

    fn configured_profile(&self) -> Result<SemanticProfile, RuntimeError> {
        match self.dialect {
            Dialect::Blu => Ok(SemanticProfile::Blu),
            Dialect::Luau => Ok(SemanticProfile::Luau),
            dialect => Err(RuntimeError::DialectNotImplemented(dialect)),
        }
    }

    fn active_profile(&self) -> Result<SemanticProfile, RuntimeError> {
        self.active_profile
            .map_or_else(|| self.configured_profile(), Ok)
    }

    /// Compatibility helper for registering a native callback.
    ///
    /// This panics when registry growth is rejected. Strict embedders should
    /// use [`Self::try_register_function`] and handle its structured error.
    pub fn register_function(
        &mut self,
        function: impl Fn(&mut Vm, &[Value]) -> Result<Vec<Value>, RuntimeError> + Send + Sync + 'static,
    ) -> NativeFunctionId {
        self.try_register_function(function)
            .expect("native function registry limit exceeded")
    }

    /// Registers a native callback after checking the configured count limit
    /// and reserving registry growth fallibly.
    pub fn try_register_function(
        &mut self,
        function: impl Fn(&mut Vm, &[Value]) -> Result<Vec<Value>, RuntimeError> + Send + Sync + 'static,
    ) -> Result<NativeFunctionId, RuntimeError> {
        let index = self.native_functions.len();
        let required = index
            .checked_add(1)
            .ok_or(RuntimeError::NativeFunctionLimit {
                required: usize::MAX,
                limit: self.native_function_limit,
            })?;
        let representable_limit = usize::try_from(u32::MAX)
            .unwrap_or(usize::MAX)
            .saturating_add(1);
        let limit = self.native_function_limit.min(representable_limit);
        if required > limit {
            return Err(RuntimeError::NativeFunctionLimit { required, limit });
        }
        self.native_functions
            .try_reserve(1)
            .map_err(|_| RuntimeError::Allocation {
                what: "native function registry",
            })?;
        let id = NativeFunctionId(index as u32);
        self.native_functions.push(Arc::new(function));
        Ok(id)
    }

    /// Compatibility helper for inserting or replacing a global.
    ///
    /// This panics when distinct-name growth is rejected. Strict embedders
    /// should use [`Self::try_set_global`] and handle its structured error.
    pub fn set_global(&mut self, name: impl Into<Arc<[u8]>>, value: Value) {
        self.try_set_global(name, value)
            .expect("global registry limit exceeded");
    }

    /// Inserts or replaces a global after checking the configured distinct-name
    /// limit and reserving map growth fallibly.
    pub fn try_set_global(
        &mut self,
        name: impl Into<Arc<[u8]>>,
        value: Value,
    ) -> Result<Option<Value>, RuntimeError> {
        let name = name.into();
        if !self.globals.contains_key(name.as_ref()) {
            let required = self
                .globals
                .len()
                .checked_add(1)
                .ok_or(RuntimeError::GlobalLimit {
                    required: usize::MAX,
                    limit: self.global_limit,
                })?;
            if required > self.global_limit {
                return Err(RuntimeError::GlobalLimit {
                    required,
                    limit: self.global_limit,
                });
            }
            self.globals
                .try_reserve(1)
                .map_err(|_| RuntimeError::Allocation {
                    what: "global registry",
                })?;
        }
        Ok(self.globals.insert(name, value))
    }

    pub fn set_module_loader(
        &mut self,
        loader: impl Fn(&mut Vm, &[u8]) -> Result<Value, RuntimeError> + Send + Sync + 'static,
    ) {
        self.module_loader = Some(Arc::new(loader));
    }

    pub fn clear_module_cache(&mut self) {
        self.module_cache.clear();
    }

    #[must_use]
    /// Borrows a global value.
    ///
    /// The value remains rooted while it is stored in globals. If the host
    /// clones a heap handle and may later replace that global, it must call
    /// [`Self::retain_value`] first.
    pub fn global(&self, name: &[u8]) -> Option<&Value> {
        self.globals.get(name)
    }

    pub fn take_output(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.output)
    }

    /// Retains one occurrence of a heap handle obtained through a public VM or
    /// heap accessor.
    ///
    /// Returns `false` for scalar, string, and native-function values, which do
    /// not need tracing. A successful `true` result must be paired with exactly
    /// one [`Self::release_value`] call after all associated host clones are no
    /// longer used.
    pub fn retain_value(&mut self, value: &Value) -> Result<bool, RuntimeError> {
        let retained = usize::from(HostRoot::from_value(value).is_some());
        self.retain_host_occurrences(std::slice::from_ref(value))?;
        Ok(retained != 0)
    }

    /// Atomically retains one occurrence of every heap handle in `values`.
    ///
    /// Returns the number retained. No occurrence is retained if the bounded
    /// host-value limit or the bookkeeping allocation fails.
    pub fn retain_values(&mut self, values: &[Value]) -> Result<usize, RuntimeError> {
        let retained = values
            .iter()
            .filter(|value| HostRoot::from_value(value).is_some())
            .count();
        self.retain_host_occurrences(values)?;
        Ok(retained)
    }

    /// Releases one retained occurrence of a heap handle returned by an
    /// `execute*` method or passed to [`Self::retain_value`].
    ///
    /// If the same handle occurs in multiple results, release each occurrence
    /// only after the host no longer needs the corresponding returned value.
    pub fn release_value(&mut self, value: &Value) -> bool {
        let Some(root) = HostRoot::from_value(value) else {
            return false;
        };
        let Some(count) = self.host_roots.get_mut(&root) else {
            return false;
        };
        *count -= 1;
        self.host_root_count -= 1;
        if *count == 0 {
            self.host_roots.remove(&root);
        }
        true
    }

    /// Releases one retained occurrence for each matching value.
    pub fn release_values(&mut self, values: &[Value]) -> usize {
        values
            .iter()
            .filter(|value| self.release_value(value))
            .count()
    }

    /// Releases every automatically or explicitly retained host value.
    pub fn release_all_values(&mut self) {
        self.host_roots.clear();
        self.host_root_count = 0;
    }

    #[must_use]
    pub const fn retained_value_count(&self) -> usize {
        self.host_root_count
    }

    /// Returns the configured maximum retained heap-handle occurrence count.
    #[must_use]
    pub const fn host_value_limit(&self) -> usize {
        self.host_value_limit
    }

    /// Returns the configured maximum result count for one native callback.
    #[must_use]
    pub const fn native_result_limit(&self) -> usize {
        self.native_result_limit
    }

    pub fn collect<'a>(
        &mut self,
        roots: impl IntoIterator<Item = &'a Value>,
    ) -> Result<crate::CollectionStats, RuntimeError> {
        self.collect_internal(roots, std::iter::empty())
    }

    fn collect_internal<'a>(
        &mut self,
        roots: impl IntoIterator<Item = &'a Value>,
        upvalues: impl IntoIterator<Item = UpvalueId>,
    ) -> Result<crate::CollectionStats, RuntimeError> {
        let active_value_count = self.active_roots.iter().try_fold(0usize, |count, roots| {
            count
                .checked_add(roots.values.len())
                .ok_or(RuntimeError::Allocation {
                    what: "collection roots",
                })
        })?;
        let root_capacity = self
            .globals
            .len()
            .checked_add(1)
            .and_then(|count| count.checked_add(self.module_cache.len()))
            .and_then(|count| count.checked_add(active_value_count))
            .and_then(|count| count.checked_add(self.host_roots.len()))
            .ok_or(RuntimeError::Allocation {
                what: "collection roots",
            })?;
        let mut all_roots = try_vec_with_capacity(root_capacity, "collection roots")?;
        all_roots.extend(self.globals.values().cloned());
        all_roots.push(Value::Thread(self.main_thread));
        all_roots.extend(self.module_cache.values().cloned());
        all_roots.extend(
            self.active_roots
                .iter()
                .flat_map(|roots| roots.values.iter().cloned()),
        );
        all_roots.extend(self.host_roots.keys().copied().map(HostRoot::to_value));
        for root in roots {
            try_reserve_exact(&mut all_roots, 1, "collection roots")?;
            all_roots.push(root.clone());
        }
        let active_upvalues = self
            .active_roots
            .iter()
            .flat_map(|roots| roots.upvalues.iter().copied());
        let stats = self
            .heap
            .collect_with_upvalues(&all_roots, active_upvalues.chain(upvalues))?;
        self.threads
            .retain(|thread, _| self.heap.contains_thread(*thread));
        Ok(stats)
    }

    fn ensure_heap_objects<'a>(
        &mut self,
        additional: usize,
        roots: &'a GcRoots,
        values: impl IntoIterator<Item = &'a Value>,
        upvalues: impl IntoIterator<Item = UpvalueId>,
    ) -> Result<(), RuntimeError> {
        let required = self.heap.live_objects().saturating_add(additional);
        if required <= self.heap_object_limit {
            return Ok(());
        }
        self.collect_internal(
            roots.values.iter().chain(values),
            roots.upvalues.iter().copied().chain(upvalues),
        )?;
        let required = self.heap.live_objects().saturating_add(additional);
        if required <= self.heap_object_limit {
            Ok(())
        } else {
            Err(RuntimeError::HeapObjectLimit {
                required,
                limit: self.heap_object_limit,
            })
        }
    }

    fn collect_if_needed<'a>(
        &mut self,
        requested: usize,
        roots: &'a GcRoots,
        values: impl IntoIterator<Item = &'a Value>,
        upvalues: impl IntoIterator<Item = UpvalueId>,
    ) -> Result<(), RuntimeError> {
        if requested != 0 && self.heap.should_collect(requested) {
            self.collect_internal(
                roots.values.iter().chain(values),
                roots.upvalues.iter().copied().chain(upvalues),
            )?;
        }
        Ok(())
    }

    fn allocate_table(
        &mut self,
        array_capacity: usize,
        hash_capacity: usize,
        roots: &GcRoots,
    ) -> Result<TableId, RuntimeError> {
        self.ensure_heap_objects(1, roots, std::iter::empty(), std::iter::empty())?;
        let requested = self
            .heap
            .table_allocation_bytes(array_capacity, hash_capacity)?;
        self.collect_if_needed(requested, roots, std::iter::empty(), std::iter::empty())?;
        Ok(self.heap.allocate_table(array_capacity, hash_capacity)?)
    }

    fn allocate_upvalue(
        &mut self,
        value: Value,
        roots: &GcRoots,
    ) -> Result<UpvalueId, RuntimeError> {
        self.ensure_heap_objects(1, roots, std::iter::once(&value), std::iter::empty())?;
        let requested = self.heap.upvalue_allocation_bytes()?;
        self.collect_if_needed(
            requested,
            roots,
            std::iter::once(&value),
            std::iter::empty(),
        )?;
        Ok(self.heap.allocate_upvalue(value)?)
    }

    fn allocate_closure(
        &mut self,
        chunk: Arc<Chunk>,
        prototype: usize,
        profile: SemanticProfile,
        upvalue_capacity: usize,
        roots: &GcRoots,
    ) -> Result<ClosureId, RuntimeError> {
        self.ensure_heap_objects(1, roots, std::iter::empty(), std::iter::empty())?;
        let requested = self.heap.closure_allocation_bytes(upvalue_capacity)?;
        self.collect_if_needed(requested, roots, std::iter::empty(), std::iter::empty())?;
        Ok(self
            .heap
            .allocate_closure(chunk, prototype, profile, upvalue_capacity)?)
    }

    fn allocate_blu_closure(
        &mut self,
        artifact: Arc<BluArtifact>,
        prototype: usize,
        profile: SemanticProfile,
        upvalue_capacity: usize,
        roots: &GcRoots,
    ) -> Result<ClosureId, RuntimeError> {
        self.ensure_heap_objects(1, roots, std::iter::empty(), std::iter::empty())?;
        let requested = self.heap.closure_allocation_bytes(upvalue_capacity)?;
        self.collect_if_needed(requested, roots, std::iter::empty(), std::iter::empty())?;
        Ok(self
            .heap
            .allocate_blu_closure(artifact, prototype, profile, upvalue_capacity)?)
    }

    fn allocate_thread(
        &mut self,
        thread_roots: &[Value],
        roots: &GcRoots,
    ) -> Result<ThreadId, RuntimeError> {
        self.ensure_heap_objects(1, roots, thread_roots.iter(), std::iter::empty())?;
        let requested = self.heap.thread_allocation_bytes(thread_roots.len())?;
        self.collect_if_needed(requested, roots, thread_roots, std::iter::empty())?;
        Ok(self.heap.allocate_thread(thread_roots)?)
    }

    fn closure_push_upvalue(
        &mut self,
        closure: ClosureId,
        upvalue: UpvalueId,
        roots: &GcRoots,
    ) -> Result<(), RuntimeError> {
        let requested = self.heap.closure_push_upvalue_bytes(closure)?;
        let closure_root = Value::Closure(closure);
        self.collect_if_needed(
            requested,
            roots,
            std::iter::once(&closure_root),
            std::iter::once(upvalue),
        )?;
        Ok(self.heap.closure_push_upvalue(closure, upvalue)?)
    }

    fn table_set(
        &mut self,
        table: TableId,
        key: Value,
        value: Value,
        roots: &GcRoots,
    ) -> Result<(), RuntimeError> {
        let requested = self.heap.table_set_bytes(table, &key, &value)?;
        let table_root = Value::Table(table);
        self.collect_if_needed(
            requested,
            roots,
            [&table_root, &key, &value],
            std::iter::empty(),
        )?;
        Ok(self.heap.table_set(table, key, value)?)
    }

    fn thread_set_roots(
        &mut self,
        thread: ThreadId,
        thread_roots: &GcRoots,
        roots: &GcRoots,
    ) -> Result<(), RuntimeError> {
        let requested = self.heap.thread_set_gc_roots_bytes(
            thread,
            thread_roots.values.len(),
            thread_roots.upvalues.len(),
        )?;
        let thread_root = Value::Thread(thread);
        self.collect_if_needed(
            requested,
            roots,
            std::iter::once(&thread_root).chain(thread_roots.values.iter()),
            thread_roots.upvalues.iter().copied(),
        )?;
        Ok(self
            .heap
            .thread_set_gc_roots(thread, &thread_roots.values, &thread_roots.upvalues)?)
    }

    pub fn execute(&mut self, chunk: &Chunk) -> Result<Vec<Value>, RuntimeError> {
        self.execute_owned(chunk.clone())
    }

    pub fn execute_owned(&mut self, chunk: Chunk) -> Result<Vec<Value>, RuntimeError> {
        let chunk = ValidatedChunk::new(chunk).map_err(RuntimeError::Validation)?;
        self.execute_validated_owned(chunk)
    }

    pub fn execute_validated(
        &mut self,
        chunk: &ValidatedChunk,
    ) -> Result<Vec<Value>, RuntimeError> {
        self.execute_validated_owned(chunk.clone())
    }

    /// Executes a BluV1 baseline translation without discarding its profile.
    ///
    /// The translated chunk is consumed, so this path does not deep-clone the
    /// artifact. Its authorized profile becomes the executing frame profile;
    /// the VM dialect is only the fallback for ordinary unprofiled chunks.
    pub fn execute_translated(
        &mut self,
        translated: TranslatedChunk,
    ) -> Result<Vec<Value>, RuntimeError> {
        self.execute_validated_owned(translated.into_validated_chunk())
    }

    /// Executes the currently supported validated BluV1 instruction slice
    /// directly, without translating it to Luau bytecode.
    ///
    /// The artifact is consumed and revalidated under `execution_limits`.
    /// Unsupported structure and profile-sensitive constants fail explicitly.
    pub fn execute_blu_v1(
        &mut self,
        artifact: ValidatedBluArtifact,
        execution_limits: BluLimits,
    ) -> Result<Vec<Value>, RuntimeError> {
        let artifact = ValidatedBluArtifact::new(artifact.into_artifact(), execution_limits)
            .map_err(RuntimeError::BluValidation)?
            .into_artifact();
        let main = usize::try_from(artifact.main)
            .map_err(|_| RuntimeError::InvalidMainPrototype(usize::MAX))?;
        let prototype = artifact
            .prototypes
            .get(main)
            .ok_or(RuntimeError::InvalidMainPrototype(main))?;
        if !prototype.upvalues.is_empty() {
            return Err(RuntimeError::UnsupportedBluV1Structure {
                what: "main prototype upvalues",
            });
        }

        let previous_profile = self.active_profile.replace(prototype.profile);
        let result = self.run_blu_v1_artifact(Arc::new(artifact), main);
        self.active_profile = previous_profile;
        match result {
            Ok(values) => {
                self.retain_host_occurrences(&values)?;
                Ok(values)
            }
            Err(error) => {
                self.retain_error_occurrences(&error)?;
                Err(error)
            }
        }
    }

    fn run_blu_v1_artifact(
        &mut self,
        artifact: Arc<BluArtifact>,
        main: usize,
    ) -> Result<Vec<Value>, RuntimeError> {
        let charge = artifact
            .prototypes
            .iter()
            .try_fold(0usize, |total, prototype| {
                total
                    .checked_add(blu_v1_execution_bytes(prototype)?)
                    .ok_or_else(|| RuntimeError::from(HeapError::Memory(MemoryError::SizeOverflow)))
            })?;
        let roots = GcRoots::default();
        self.collect_if_needed(charge, &roots, core::iter::empty(), core::iter::empty())?;
        self.heap.charge_external(charge)?;
        let result = self.run_charged_blu_v1_artifact(artifact, main);
        self.heap.release_external(charge)?;
        result
    }

    fn run_charged_blu_v1_artifact(
        &mut self,
        artifact: Arc<BluArtifact>,
        main: usize,
    ) -> Result<Vec<Value>, RuntimeError> {
        let mut artifact = artifact;
        let mut prototype_index = main;
        let mut constants = materialize_blu_constants(&artifact.prototypes[main])?;
        let prototype = &artifact.prototypes[main];
        let register_count = usize::from(prototype.register_count);
        let mut registers = try_vec_with_capacity(register_count, "BluV1 runtime registers")?;
        registers.resize(register_count, Value::Nil);
        let mut varargs = Vec::new();
        let mut dynamic_results = Vec::new();
        let mut open_upvalues = try_vec_with_capacity(register_count, "BluV1 open upvalues")?;
        open_upvalues.resize(register_count, None);
        let mut closure = None;
        let mut callers =
            try_vec_with_capacity(self.call_limit.min(16), "BluV1 caller frame stack")?;
        let mut remaining = self.instruction_limit;
        let mut pc = 0usize;
        loop {
            let prototype = artifact
                .prototypes
                .get(prototype_index)
                .ok_or(RuntimeError::InvalidPrototype(prototype_index))?;
            let Some(instruction) = prototype.code.get(pc).copied() else {
                return Err(RuntimeError::InvalidProgramCounter {
                    pc,
                    code_words: prototype.code.len(),
                });
            };
            if remaining == 0 {
                return Err(RuntimeError::InstructionLimit {
                    limit: self.instruction_limit,
                });
            }
            remaining -= 1;
            match instruction {
                BluInstruction::LoadConstant {
                    destination,
                    constant,
                } => {
                    let value = constants.get(constant as usize).cloned().ok_or(
                        RuntimeError::Constant {
                            constant: constant as usize,
                            count: constants.len(),
                        },
                    )?;
                    set_blu_register(
                        &mut self.heap,
                        &mut registers,
                        &open_upvalues,
                        destination,
                        value,
                    )?;
                }
                BluInstruction::Varargs { destination, count } => {
                    for offset in 0..count {
                        let register =
                            destination
                                .checked_add(offset)
                                .ok_or(RuntimeError::Register {
                                    register: usize::MAX,
                                    count: registers.len(),
                                })?;
                        let value = varargs.get(offset as usize).cloned().unwrap_or(Value::Nil);
                        set_blu_register(
                            &mut self.heap,
                            &mut registers,
                            &open_upvalues,
                            register,
                            value,
                        )?;
                    }
                }
                BluInstruction::LoadGlobal { destination, name } => {
                    let Value::String(name) =
                        constants.get(name as usize).ok_or(RuntimeError::Constant {
                            constant: name as usize,
                            count: constants.len(),
                        })?
                    else {
                        return Err(RuntimeError::UnsupportedBluV1Structure {
                            what: "validated global name is not a string",
                        });
                    };
                    let value = self.globals.get(name).cloned().unwrap_or(Value::Nil);
                    set_blu_register(
                        &mut self.heap,
                        &mut registers,
                        &open_upvalues,
                        destination,
                        value,
                    )?;
                }
                BluInstruction::StoreGlobal { name, source } => {
                    let Value::String(name) =
                        constants.get(name as usize).ok_or(RuntimeError::Constant {
                            constant: name as usize,
                            count: constants.len(),
                        })?
                    else {
                        return Err(RuntimeError::UnsupportedBluV1Structure {
                            what: "validated global name is not a string",
                        });
                    };
                    let value = blu_register(&registers, source)?.clone();
                    self.set_global(name.clone(), value);
                }
                BluInstruction::NewTable { destination } => {
                    let roots =
                        blu_frame_roots(&registers, &varargs, &open_upvalues, closure, &callers)?;
                    let table = self.allocate_table(0, 0, &roots)?;
                    set_blu_register(
                        &mut self.heap,
                        &mut registers,
                        &open_upvalues,
                        destination,
                        Value::Table(table),
                    )?;
                }
                BluInstruction::GetTable {
                    destination,
                    table,
                    key,
                } => {
                    let table_value = blu_register(&registers, table)?;
                    let Value::Table(table) = table_value else {
                        return Err(RuntimeError::Type {
                            operation: "table index",
                            expected: "table",
                            actual: table_value.type_name(),
                        });
                    };
                    let key = blu_register(&registers, key)?.clone();
                    match self.resolve_blu_index(*table, &key, prototype.profile)? {
                        BluIndexResolution::Value(value) => {
                            set_blu_register(
                                &mut self.heap,
                                &mut registers,
                                &open_upvalues,
                                destination,
                                value,
                            )?;
                        }
                        BluIndexResolution::Call { function, receiver } => {
                            let mut arguments =
                                try_vec_with_capacity(2, "BluV1 __index arguments")?;
                            arguments.push(receiver);
                            arguments.push(key);
                            if let Value::Closure(child_closure) = &function
                                && self.heap.is_blu_closure(*child_closure)?
                            {
                                if callers.len() >= self.call_limit {
                                    return Err(RuntimeError::CallLimit {
                                        limit: self.call_limit,
                                    });
                                }
                                let (child_artifact, child, profile, _) =
                                    self.heap.blu_closure_parts(*child_closure)?;
                                let child_prototype = child_artifact
                                    .prototypes
                                    .get(child)
                                    .ok_or(RuntimeError::InvalidPrototype(child))?;
                                let child_constants = materialize_blu_constants(child_prototype)?;
                                let child_register_count =
                                    usize::from(child_prototype.register_count);
                                let mut child_registers = try_vec_with_capacity(
                                    child_register_count,
                                    "BluV1 runtime registers",
                                )?;
                                child_registers.resize(child_register_count, Value::Nil);
                                let copied = arguments
                                    .len()
                                    .min(usize::from(child_prototype.parameter_count));
                                child_registers[..copied].clone_from_slice(&arguments[..copied]);
                                let child_varargs = if child_prototype.is_vararg {
                                    try_clone_values(
                                        arguments
                                            .get(usize::from(child_prototype.parameter_count)..)
                                            .unwrap_or_default(),
                                        "BluV1 frame varargs",
                                    )?
                                } else {
                                    Vec::new()
                                };
                                let mut child_open_upvalues = try_vec_with_capacity(
                                    child_register_count,
                                    "BluV1 open upvalues",
                                )?;
                                child_open_upvalues.resize(child_register_count, None);
                                try_reserve_exact(&mut callers, 1, "BluV1 __index caller frame")?;
                                callers.push(BluCaller {
                                    artifact,
                                    prototype: prototype_index,
                                    constants,
                                    registers,
                                    varargs,
                                    open_upvalues,
                                    closure,
                                    pc: pc + 1,
                                    result: BluCallResult::Fixed {
                                        destination,
                                        count: 1,
                                    },
                                });
                                artifact = child_artifact;
                                prototype_index = child;
                                constants = child_constants;
                                registers = child_registers;
                                varargs = child_varargs;
                                open_upvalues = child_open_upvalues;
                                closure = Some(*child_closure);
                                pc = 0;
                                self.active_profile = Some(profile);
                                continue;
                            }
                            let roots = blu_frame_roots(
                                &registers,
                                &varargs,
                                &open_upvalues,
                                closure,
                                &callers,
                            )?;
                            let value = self
                                .call_value(
                                    function,
                                    &arguments,
                                    &mut remaining,
                                    callers.len(),
                                    roots,
                                )?
                                .into_iter()
                                .next()
                                .unwrap_or(Value::Nil);
                            set_blu_register(
                                &mut self.heap,
                                &mut registers,
                                &open_upvalues,
                                destination,
                                value,
                            )?;
                        }
                    }
                }
                BluInstruction::SetTable { table, key, value } => {
                    let table_value = blu_register(&registers, table)?;
                    let Value::Table(table) = table_value else {
                        return Err(RuntimeError::Type {
                            operation: "table assignment",
                            expected: "table",
                            actual: table_value.type_name(),
                        });
                    };
                    let table = *table;
                    let key = blu_register(&registers, key)?.clone();
                    let value = blu_register(&registers, value)?.clone();
                    match self.resolve_blu_new_index(table, &key, prototype.profile)? {
                        BluNewIndexResolution::Raw(target) => {
                            let roots = blu_frame_roots(
                                &registers,
                                &varargs,
                                &open_upvalues,
                                closure,
                                &callers,
                            )?;
                            self.table_set(target, key, value, &roots)?;
                        }
                        BluNewIndexResolution::Call { function, receiver } => {
                            let mut arguments =
                                try_vec_with_capacity(3, "BluV1 __newindex arguments")?;
                            arguments.push(receiver);
                            arguments.push(key);
                            arguments.push(value);
                            if let Value::Closure(child_closure) = &function
                                && self.heap.is_blu_closure(*child_closure)?
                            {
                                if callers.len() >= self.call_limit {
                                    return Err(RuntimeError::CallLimit {
                                        limit: self.call_limit,
                                    });
                                }
                                let (child_artifact, child, profile, _) =
                                    self.heap.blu_closure_parts(*child_closure)?;
                                let child_prototype = child_artifact
                                    .prototypes
                                    .get(child)
                                    .ok_or(RuntimeError::InvalidPrototype(child))?;
                                let child_constants = materialize_blu_constants(child_prototype)?;
                                let child_register_count =
                                    usize::from(child_prototype.register_count);
                                let mut child_registers = try_vec_with_capacity(
                                    child_register_count,
                                    "BluV1 runtime registers",
                                )?;
                                child_registers.resize(child_register_count, Value::Nil);
                                let copied = arguments
                                    .len()
                                    .min(usize::from(child_prototype.parameter_count));
                                child_registers[..copied].clone_from_slice(&arguments[..copied]);
                                let child_varargs = if child_prototype.is_vararg {
                                    try_clone_values(
                                        arguments
                                            .get(usize::from(child_prototype.parameter_count)..)
                                            .unwrap_or_default(),
                                        "BluV1 frame varargs",
                                    )?
                                } else {
                                    Vec::new()
                                };
                                let mut child_open_upvalues = try_vec_with_capacity(
                                    child_register_count,
                                    "BluV1 open upvalues",
                                )?;
                                child_open_upvalues.resize(child_register_count, None);
                                try_reserve_exact(
                                    &mut callers,
                                    1,
                                    "BluV1 __newindex caller frame",
                                )?;
                                callers.push(BluCaller {
                                    artifact,
                                    prototype: prototype_index,
                                    constants,
                                    registers,
                                    varargs,
                                    open_upvalues,
                                    closure,
                                    pc: pc + 1,
                                    result: BluCallResult::Fixed {
                                        destination: 0,
                                        count: 0,
                                    },
                                });
                                artifact = child_artifact;
                                prototype_index = child;
                                constants = child_constants;
                                registers = child_registers;
                                varargs = child_varargs;
                                open_upvalues = child_open_upvalues;
                                closure = Some(*child_closure);
                                pc = 0;
                                self.active_profile = Some(profile);
                                continue;
                            }
                            let roots = blu_frame_roots(
                                &registers,
                                &varargs,
                                &open_upvalues,
                                closure,
                                &callers,
                            )?;
                            self.call_value(
                                function,
                                &arguments,
                                &mut remaining,
                                callers.len(),
                                roots,
                            )?;
                        }
                    }
                }
                BluInstruction::SetListVarargs { table, start } => {
                    let table_value = blu_register(&registers, table)?;
                    let Value::Table(table) = table_value else {
                        return Err(RuntimeError::Type {
                            operation: "table assignment",
                            expected: "table",
                            actual: table_value.type_name(),
                        });
                    };
                    let table = *table;
                    for (offset, value) in varargs.iter().cloned().enumerate() {
                        let index = u64::from(start).checked_add(offset as u64).ok_or(
                            RuntimeError::StackLimit {
                                required: usize::MAX,
                                limit: MAX_DYNAMIC_REGISTERS,
                            },
                        )?;
                        let key = if matches!(
                            prototype.profile,
                            SemanticProfile::Lua53
                                | SemanticProfile::Lua54
                                | SemanticProfile::Lua55
                        ) {
                            Value::Integer(index as i64)
                        } else {
                            Value::Number(index as f64)
                        };
                        let roots = blu_frame_roots(
                            &registers,
                            &varargs,
                            &open_upvalues,
                            closure,
                            &callers,
                        )?;
                        self.table_set(table, key, value, &roots)?;
                    }
                }
                BluInstruction::SetListCall {
                    table,
                    start,
                    function,
                    arguments,
                    argument_count,
                }
                | BluInstruction::SetListCallVarargs {
                    table,
                    start,
                    function,
                    arguments,
                    argument_count,
                } => {
                    let expands_varargs =
                        matches!(instruction, BluInstruction::SetListCallVarargs { .. });
                    let table_value = blu_register(&registers, table)?;
                    let Value::Table(table) = table_value else {
                        return Err(RuntimeError::Type {
                            operation: "table assignment",
                            expected: "table",
                            actual: table_value.type_name(),
                        });
                    };
                    let table = *table;
                    let function = blu_register(&registers, function)?.clone();
                    let argument_start = usize::from(arguments);
                    let argument_end = argument_start
                        .checked_add(usize::from(argument_count))
                        .ok_or(RuntimeError::Register {
                            register: usize::MAX,
                            count: registers.len(),
                        })?;
                    let fixed_arguments = registers.get(argument_start..argument_end).ok_or(
                        RuntimeError::Register {
                            register: argument_end.saturating_sub(1),
                            count: registers.len(),
                        },
                    )?;
                    let arguments = if expands_varargs {
                        append_blu_varargs(
                            fixed_arguments,
                            &varargs,
                            "BluV1 dynamic table call arguments",
                        )?
                    } else {
                        try_clone_values(fixed_arguments, "BluV1 table call arguments")?
                    };
                    let (function, arguments) =
                        self.resolve_blu_callable(function, arguments, prototype.profile)?;
                    if let Value::Closure(child_closure) = &function
                        && self.heap.is_blu_closure(*child_closure)?
                    {
                        if callers.len() >= self.call_limit {
                            return Err(RuntimeError::CallLimit {
                                limit: self.call_limit,
                            });
                        }
                        let (child_artifact, child, profile, _) =
                            self.heap.blu_closure_parts(*child_closure)?;
                        let child_prototype = child_artifact
                            .prototypes
                            .get(child)
                            .ok_or(RuntimeError::InvalidPrototype(child))?;
                        let child_constants = materialize_blu_constants(child_prototype)?;
                        let child_register_count = usize::from(child_prototype.register_count);
                        let mut child_registers =
                            try_vec_with_capacity(child_register_count, "BluV1 runtime registers")?;
                        child_registers.resize(child_register_count, Value::Nil);
                        let copied = arguments
                            .len()
                            .min(usize::from(child_prototype.parameter_count));
                        child_registers[..copied].clone_from_slice(&arguments[..copied]);
                        let child_varargs = if child_prototype.is_vararg {
                            try_clone_values(
                                arguments
                                    .get(usize::from(child_prototype.parameter_count)..)
                                    .unwrap_or_default(),
                                "BluV1 frame varargs",
                            )?
                        } else {
                            Vec::new()
                        };
                        let mut child_open_upvalues =
                            try_vec_with_capacity(child_register_count, "BluV1 open upvalues")?;
                        child_open_upvalues.resize(child_register_count, None);
                        try_reserve_exact(&mut callers, 1, "BluV1 table-list caller frame")?;
                        callers.push(BluCaller {
                            artifact,
                            prototype: prototype_index,
                            constants,
                            registers,
                            varargs,
                            open_upvalues,
                            closure,
                            pc: pc + 1,
                            result: BluCallResult::TableList { table, start },
                        });
                        artifact = child_artifact;
                        prototype_index = child;
                        constants = child_constants;
                        registers = child_registers;
                        varargs = child_varargs;
                        open_upvalues = child_open_upvalues;
                        closure = Some(*child_closure);
                        pc = 0;
                        self.active_profile = Some(profile);
                        continue;
                    }
                    let mut roots =
                        blu_frame_roots(&registers, &varargs, &open_upvalues, closure, &callers)?;
                    for argument in &arguments {
                        roots.push_value(argument.clone())?;
                    }
                    let values = self.call_value(
                        function,
                        &arguments,
                        &mut remaining,
                        callers.len(),
                        roots,
                    )?;
                    let mut roots =
                        blu_frame_roots(&registers, &varargs, &open_upvalues, closure, &callers)?;
                    for value in &values {
                        roots.push_value(value.clone())?;
                    }
                    self.set_blu_table_list(table, start, values, prototype.profile, &roots)?;
                }
                BluInstruction::Call {
                    destination,
                    function,
                    arguments,
                    argument_count,
                } => {
                    let function = blu_register(&registers, function)?.clone();
                    let start = usize::from(arguments);
                    let end = start.checked_add(usize::from(argument_count)).ok_or(
                        RuntimeError::Register {
                            register: usize::MAX,
                            count: registers.len(),
                        },
                    )?;
                    let arguments = registers.get(start..end).ok_or(RuntimeError::Register {
                        register: end.saturating_sub(1),
                        count: registers.len(),
                    })?;
                    let arguments = try_clone_values(arguments, "BluV1 call arguments")?;
                    let (function, arguments) =
                        self.resolve_blu_callable(function, arguments, prototype.profile)?;
                    if let Value::Closure(child_closure) = &function
                        && self.heap.is_blu_closure(*child_closure)?
                    {
                        if callers.len() >= self.call_limit {
                            return Err(RuntimeError::CallLimit {
                                limit: self.call_limit,
                            });
                        }
                        let (child_artifact, child, profile, _) =
                            self.heap.blu_closure_parts(*child_closure)?;
                        let child_prototype = child_artifact
                            .prototypes
                            .get(child)
                            .ok_or(RuntimeError::InvalidPrototype(child))?;
                        let child_constants = materialize_blu_constants(child_prototype)?;
                        let child_register_count = usize::from(child_prototype.register_count);
                        let mut child_registers =
                            try_vec_with_capacity(child_register_count, "BluV1 runtime registers")?;
                        child_registers.resize(child_register_count, Value::Nil);
                        let copied = arguments
                            .len()
                            .min(usize::from(child_prototype.parameter_count));
                        child_registers[..copied].clone_from_slice(&arguments[..copied]);
                        let child_varargs = if child_prototype.is_vararg {
                            try_clone_values(
                                arguments
                                    .get(usize::from(child_prototype.parameter_count)..)
                                    .unwrap_or_default(),
                                "BluV1 frame varargs",
                            )?
                        } else {
                            Vec::new()
                        };
                        let mut child_open_upvalues =
                            try_vec_with_capacity(child_register_count, "BluV1 open upvalues")?;
                        child_open_upvalues.resize(child_register_count, None);
                        try_reserve_exact(&mut callers, 1, "BluV1 caller frame stack")?;
                        callers.push(BluCaller {
                            artifact,
                            prototype: prototype_index,
                            constants,
                            registers,
                            varargs,
                            open_upvalues,
                            closure,
                            pc: pc + 1,
                            result: BluCallResult::Fixed {
                                destination,
                                count: 1,
                            },
                        });
                        artifact = child_artifact;
                        prototype_index = child;
                        constants = child_constants;
                        registers = child_registers;
                        varargs = child_varargs;
                        open_upvalues = child_open_upvalues;
                        closure = Some(*child_closure);
                        pc = 0;
                        self.active_profile = Some(profile);
                        continue;
                    }
                    let mut roots =
                        blu_frame_roots(&registers, &varargs, &open_upvalues, closure, &callers)?;
                    for argument in &arguments {
                        roots.push_value(argument.clone())?;
                    }
                    let values = self.call_value(
                        function,
                        &arguments,
                        &mut remaining,
                        callers.len(),
                        roots,
                    )?;
                    let value = values.into_iter().next().unwrap_or(Value::Nil);
                    set_blu_register(
                        &mut self.heap,
                        &mut registers,
                        &open_upvalues,
                        destination,
                        value,
                    )?;
                }
                BluInstruction::CallResults {
                    destination,
                    function,
                    arguments,
                    argument_count,
                    result_count,
                }
                | BluInstruction::CallVarargsResults {
                    destination,
                    function,
                    arguments,
                    argument_count,
                    result_count,
                }
                | BluInstruction::CallDynamicResults {
                    destination,
                    function,
                    arguments,
                    argument_count,
                    result_count,
                } => {
                    let expands_varargs =
                        matches!(instruction, BluInstruction::CallVarargsResults { .. });
                    let expands_dynamic =
                        matches!(instruction, BluInstruction::CallDynamicResults { .. });
                    let function = blu_register(&registers, function)?.clone();
                    let start = usize::from(arguments);
                    let end = start.checked_add(usize::from(argument_count)).ok_or(
                        RuntimeError::Register {
                            register: usize::MAX,
                            count: registers.len(),
                        },
                    )?;
                    let fixed_arguments =
                        registers.get(start..end).ok_or(RuntimeError::Register {
                            register: end.saturating_sub(1),
                            count: registers.len(),
                        })?;
                    let arguments = if expands_varargs {
                        append_blu_varargs(
                            fixed_arguments,
                            &varargs,
                            "BluV1 dynamic call arguments",
                        )?
                    } else if expands_dynamic {
                        let mut arguments =
                            try_clone_values(fixed_arguments, "BluV1 call arguments")?;
                        try_reserve_exact(
                            &mut arguments,
                            dynamic_results.len(),
                            "BluV1 dynamic call arguments",
                        )?;
                        arguments.append(&mut dynamic_results);
                        arguments
                    } else {
                        try_clone_values(fixed_arguments, "BluV1 call arguments")?
                    };
                    let (function, arguments) =
                        self.resolve_blu_callable(function, arguments, prototype.profile)?;
                    if let Value::Closure(child_closure) = &function
                        && self.heap.is_blu_closure(*child_closure)?
                    {
                        if callers.len() >= self.call_limit {
                            return Err(RuntimeError::CallLimit {
                                limit: self.call_limit,
                            });
                        }
                        let (child_artifact, child, profile, _) =
                            self.heap.blu_closure_parts(*child_closure)?;
                        let child_prototype = child_artifact
                            .prototypes
                            .get(child)
                            .ok_or(RuntimeError::InvalidPrototype(child))?;
                        let child_constants = materialize_blu_constants(child_prototype)?;
                        let child_register_count = usize::from(child_prototype.register_count);
                        let mut child_registers =
                            try_vec_with_capacity(child_register_count, "BluV1 runtime registers")?;
                        child_registers.resize(child_register_count, Value::Nil);
                        let copied = arguments
                            .len()
                            .min(usize::from(child_prototype.parameter_count));
                        child_registers[..copied].clone_from_slice(&arguments[..copied]);
                        let child_varargs = if child_prototype.is_vararg {
                            try_clone_values(
                                arguments
                                    .get(usize::from(child_prototype.parameter_count)..)
                                    .unwrap_or_default(),
                                "BluV1 frame varargs",
                            )?
                        } else {
                            Vec::new()
                        };
                        let mut child_open_upvalues =
                            try_vec_with_capacity(child_register_count, "BluV1 open upvalues")?;
                        child_open_upvalues.resize(child_register_count, None);
                        try_reserve_exact(&mut callers, 1, "BluV1 caller frame stack")?;
                        callers.push(BluCaller {
                            artifact,
                            prototype: prototype_index,
                            constants,
                            registers,
                            varargs,
                            open_upvalues,
                            closure,
                            pc: pc + 1,
                            result: BluCallResult::Fixed {
                                destination,
                                count: result_count,
                            },
                        });
                        artifact = child_artifact;
                        prototype_index = child;
                        constants = child_constants;
                        registers = child_registers;
                        varargs = child_varargs;
                        open_upvalues = child_open_upvalues;
                        closure = Some(*child_closure);
                        pc = 0;
                        self.active_profile = Some(profile);
                        continue;
                    }
                    let mut roots =
                        blu_frame_roots(&registers, &varargs, &open_upvalues, closure, &callers)?;
                    for argument in &arguments {
                        roots.push_value(argument.clone())?;
                    }
                    let values = self.call_value(
                        function,
                        &arguments,
                        &mut remaining,
                        callers.len(),
                        roots,
                    )?;
                    let mut values = values.into_iter();
                    for offset in 0..result_count {
                        let target =
                            destination
                                .checked_add(offset)
                                .ok_or(RuntimeError::Register {
                                    register: usize::MAX,
                                    count: registers.len(),
                                })?;
                        set_blu_register(
                            &mut self.heap,
                            &mut registers,
                            &open_upvalues,
                            target,
                            values.next().unwrap_or(Value::Nil),
                        )?;
                    }
                }
                BluInstruction::CallAllResults {
                    function,
                    arguments,
                    argument_count,
                }
                | BluInstruction::CallVarargsAllResults {
                    function,
                    arguments,
                    argument_count,
                }
                | BluInstruction::CallDynamicAllResults {
                    function,
                    arguments,
                    argument_count,
                } => {
                    let expands_varargs =
                        matches!(instruction, BluInstruction::CallVarargsAllResults { .. });
                    let expands_dynamic =
                        matches!(instruction, BluInstruction::CallDynamicAllResults { .. });
                    let function = blu_register(&registers, function)?.clone();
                    let start = usize::from(arguments);
                    let end = start.checked_add(usize::from(argument_count)).ok_or(
                        RuntimeError::Register {
                            register: usize::MAX,
                            count: registers.len(),
                        },
                    )?;
                    let fixed_arguments =
                        registers.get(start..end).ok_or(RuntimeError::Register {
                            register: end.saturating_sub(1),
                            count: registers.len(),
                        })?;
                    let arguments = if expands_varargs {
                        append_blu_varargs(
                            fixed_arguments,
                            &varargs,
                            "BluV1 dynamic result producer arguments",
                        )?
                    } else if expands_dynamic {
                        let mut arguments = try_clone_values(
                            fixed_arguments,
                            "BluV1 dynamic result producer arguments",
                        )?;
                        try_reserve_exact(
                            &mut arguments,
                            dynamic_results.len(),
                            "BluV1 nested dynamic call arguments",
                        )?;
                        arguments.append(&mut dynamic_results);
                        arguments
                    } else {
                        try_clone_values(
                            fixed_arguments,
                            "BluV1 dynamic result producer arguments",
                        )?
                    };
                    let (function, arguments) =
                        self.resolve_blu_callable(function, arguments, prototype.profile)?;
                    if let Value::Closure(child_closure) = &function
                        && self.heap.is_blu_closure(*child_closure)?
                    {
                        if callers.len() >= self.call_limit {
                            return Err(RuntimeError::CallLimit {
                                limit: self.call_limit,
                            });
                        }
                        let (child_artifact, child, profile, _) =
                            self.heap.blu_closure_parts(*child_closure)?;
                        let child_prototype = child_artifact
                            .prototypes
                            .get(child)
                            .ok_or(RuntimeError::InvalidPrototype(child))?;
                        let child_constants = materialize_blu_constants(child_prototype)?;
                        let child_register_count = usize::from(child_prototype.register_count);
                        let mut child_registers =
                            try_vec_with_capacity(child_register_count, "BluV1 runtime registers")?;
                        child_registers.resize(child_register_count, Value::Nil);
                        let copied = arguments
                            .len()
                            .min(usize::from(child_prototype.parameter_count));
                        child_registers[..copied].clone_from_slice(&arguments[..copied]);
                        let child_varargs = if child_prototype.is_vararg {
                            try_clone_values(
                                arguments
                                    .get(usize::from(child_prototype.parameter_count)..)
                                    .unwrap_or_default(),
                                "BluV1 frame varargs",
                            )?
                        } else {
                            Vec::new()
                        };
                        let mut child_open_upvalues =
                            try_vec_with_capacity(child_register_count, "BluV1 open upvalues")?;
                        child_open_upvalues.resize(child_register_count, None);
                        try_reserve_exact(&mut callers, 1, "BluV1 caller frame stack")?;
                        callers.push(BluCaller {
                            artifact,
                            prototype: prototype_index,
                            constants,
                            registers,
                            varargs,
                            open_upvalues,
                            closure,
                            pc: pc + 1,
                            result: BluCallResult::Dynamic,
                        });
                        artifact = child_artifact;
                        prototype_index = child;
                        constants = child_constants;
                        registers = child_registers;
                        varargs = child_varargs;
                        open_upvalues = child_open_upvalues;
                        closure = Some(*child_closure);
                        pc = 0;
                        self.active_profile = Some(profile);
                        continue;
                    }
                    let roots =
                        blu_frame_roots(&registers, &varargs, &open_upvalues, closure, &callers)?;
                    dynamic_results = self.call_value(
                        function,
                        &arguments,
                        &mut remaining,
                        callers.len(),
                        roots,
                    )?;
                    if dynamic_results.len() > MAX_DYNAMIC_REGISTERS {
                        return Err(RuntimeError::StackLimit {
                            required: dynamic_results.len(),
                            limit: MAX_DYNAMIC_REGISTERS,
                        });
                    }
                }
                BluInstruction::ReturnCall { .. }
                | BluInstruction::ReturnCallPrefix { .. }
                | BluInstruction::ReturnCallVarargs { .. }
                | BluInstruction::ReturnCallVarargsPrefix { .. }
                | BluInstruction::ReturnCallDynamic { .. }
                | BluInstruction::ReturnCallDynamicPrefix { .. } => {
                    let (prefix, function, arguments, argument_count, expands_varargs) =
                        match instruction {
                            BluInstruction::ReturnCall {
                                function,
                                arguments,
                                argument_count,
                            } => (None, function, arguments, argument_count, false),
                            BluInstruction::ReturnCallPrefix {
                                first,
                                count,
                                function,
                                arguments,
                                argument_count,
                            } => (
                                Some((first, count)),
                                function,
                                arguments,
                                argument_count,
                                false,
                            ),
                            BluInstruction::ReturnCallVarargs {
                                function,
                                arguments,
                                argument_count,
                            } => (None, function, arguments, argument_count, true),
                            BluInstruction::ReturnCallVarargsPrefix {
                                first,
                                count,
                                function,
                                arguments,
                                argument_count,
                            } => (
                                Some((first, count)),
                                function,
                                arguments,
                                argument_count,
                                true,
                            ),
                            BluInstruction::ReturnCallDynamic {
                                function,
                                arguments,
                                argument_count,
                            } => (None, function, arguments, argument_count, false),
                            BluInstruction::ReturnCallDynamicPrefix {
                                first,
                                count,
                                function,
                                arguments,
                                argument_count,
                            } => (
                                Some((first, count)),
                                function,
                                arguments,
                                argument_count,
                                false,
                            ),
                            _ => unreachable!(),
                        };
                    let expands_dynamic = matches!(
                        instruction,
                        BluInstruction::ReturnCallDynamic { .. }
                            | BluInstruction::ReturnCallDynamicPrefix { .. }
                    );
                    let function = blu_register(&registers, function)?.clone();
                    let start = usize::from(arguments);
                    let end = start.checked_add(usize::from(argument_count)).ok_or(
                        RuntimeError::Register {
                            register: usize::MAX,
                            count: registers.len(),
                        },
                    )?;
                    let fixed_arguments =
                        registers.get(start..end).ok_or(RuntimeError::Register {
                            register: end.saturating_sub(1),
                            count: registers.len(),
                        })?;
                    let arguments = if expands_varargs {
                        append_blu_varargs(
                            fixed_arguments,
                            &varargs,
                            "BluV1 dynamic return call arguments",
                        )?
                    } else if expands_dynamic {
                        let mut arguments =
                            try_clone_values(fixed_arguments, "BluV1 return call arguments")?;
                        try_reserve_exact(
                            &mut arguments,
                            dynamic_results.len(),
                            "BluV1 dynamic return call arguments",
                        )?;
                        arguments.append(&mut dynamic_results);
                        arguments
                    } else {
                        try_clone_values(fixed_arguments, "BluV1 return call arguments")?
                    };
                    let (function, arguments) =
                        self.resolve_blu_callable(function, arguments, prototype.profile)?;
                    if let Value::Closure(child_closure) = &function
                        && self.heap.is_blu_closure(*child_closure)?
                    {
                        if prefix.is_some() && callers.len() >= self.call_limit {
                            return Err(RuntimeError::CallLimit {
                                limit: self.call_limit,
                            });
                        }
                        let (child_artifact, child, profile, _) =
                            self.heap.blu_closure_parts(*child_closure)?;
                        let child_prototype = child_artifact
                            .prototypes
                            .get(child)
                            .ok_or(RuntimeError::InvalidPrototype(child))?;
                        let child_constants = materialize_blu_constants(child_prototype)?;
                        let child_register_count = usize::from(child_prototype.register_count);
                        let mut child_registers =
                            try_vec_with_capacity(child_register_count, "BluV1 runtime registers")?;
                        child_registers.resize(child_register_count, Value::Nil);
                        let copied = arguments
                            .len()
                            .min(usize::from(child_prototype.parameter_count));
                        child_registers[..copied].clone_from_slice(&arguments[..copied]);
                        let child_varargs = if child_prototype.is_vararg {
                            try_clone_values(
                                arguments
                                    .get(usize::from(child_prototype.parameter_count)..)
                                    .unwrap_or_default(),
                                "BluV1 frame varargs",
                            )?
                        } else {
                            Vec::new()
                        };
                        let mut child_open_upvalues =
                            try_vec_with_capacity(child_register_count, "BluV1 open upvalues")?;
                        child_open_upvalues.resize(child_register_count, None);
                        if let Some((first, count)) = prefix {
                            try_reserve_exact(&mut callers, 1, "BluV1 return-prefix caller frame")?;
                            callers.push(BluCaller {
                                artifact,
                                prototype: prototype_index,
                                constants,
                                registers,
                                varargs,
                                open_upvalues,
                                closure,
                                pc: pc + 1,
                                result: BluCallResult::ReturnPrefix { first, count },
                            });
                            artifact = child_artifact;
                            prototype_index = child;
                            constants = child_constants;
                            registers = child_registers;
                            varargs = child_varargs;
                            open_upvalues = child_open_upvalues;
                            closure = Some(*child_closure);
                            pc = 0;
                            self.active_profile = Some(profile);
                            continue;
                        }
                        artifact = child_artifact;
                        prototype_index = child;
                        constants = child_constants;
                        registers = child_registers;
                        varargs = child_varargs;
                        open_upvalues = child_open_upvalues;
                        closure = Some(*child_closure);
                        pc = 0;
                        self.active_profile = Some(profile);
                        continue;
                    }
                    let mut roots =
                        blu_frame_roots(&registers, &varargs, &open_upvalues, closure, &callers)?;
                    for argument in &arguments {
                        roots.push_value(argument.clone())?;
                    }
                    let mut values = self.call_value(
                        function,
                        &arguments,
                        &mut remaining,
                        callers.len(),
                        roots,
                    )?;
                    if let Some((first, count)) = prefix {
                        let start = usize::from(first);
                        let end = start.checked_add(usize::from(count)).ok_or(
                            RuntimeError::Register {
                                register: usize::MAX,
                                count: registers.len(),
                            },
                        )?;
                        let prefix = registers.get(start..end).ok_or(RuntimeError::Register {
                            register: end.saturating_sub(1),
                            count: registers.len(),
                        })?;
                        let mut combined = try_clone_values(prefix, "BluV1 return prefix")?;
                        try_reserve_exact(
                            &mut combined,
                            values.len(),
                            "BluV1 dynamic return values",
                        )?;
                        combined.append(&mut values);
                        values = combined;
                    }
                    let mut caller = loop {
                        let Some(caller) = callers.pop() else {
                            return Ok(values);
                        };
                        match self.apply_blu_return(caller, values, &callers)? {
                            BluReturnDisposition::Resume(caller) => break caller,
                            BluReturnDisposition::Propagate(next) => values = next,
                        }
                    };
                    dynamic_results =
                        match core::mem::replace(&mut caller.result, BluCallResult::Dynamic) {
                            BluCallResult::ReadyDynamic(values) => values,
                            _ => Vec::new(),
                        };
                    artifact = caller.artifact;
                    prototype_index = caller.prototype;
                    constants = caller.constants;
                    registers = caller.registers;
                    varargs = caller.varargs;
                    open_upvalues = caller.open_upvalues;
                    closure = caller.closure;
                    pc = caller.pc;
                    self.active_profile = Some(
                        artifact
                            .prototypes
                            .get(prototype_index)
                            .ok_or(RuntimeError::InvalidPrototype(prototype_index))?
                            .profile,
                    );
                    continue;
                }
                BluInstruction::NewClosure { destination, child } => {
                    let child_index = *prototype
                        .children
                        .get(child as usize)
                        .ok_or(RuntimeError::InvalidPrototype(child as usize))?
                        as usize;
                    let child_prototype = artifact
                        .prototypes
                        .get(child_index)
                        .ok_or(RuntimeError::InvalidPrototype(child_index))?;
                    let mut roots =
                        blu_frame_roots(&registers, &varargs, &open_upvalues, closure, &callers)?;
                    let child_closure = self.allocate_blu_closure(
                        artifact.clone(),
                        child_index,
                        child_prototype.profile,
                        child_prototype.upvalues.len(),
                        &roots,
                    )?;
                    roots.push_value(Value::Closure(child_closure))?;
                    let parent_upvalues = if child_prototype.upvalues.iter().any(|capture| {
                        matches!(capture, blu_bytecode::blu::Upvalue::ParentUpvalue(_))
                    }) {
                        let parent = closure.ok_or(RuntimeError::MissingClosure)?;
                        self.heap.blu_closure_parts(parent)?.3
                    } else {
                        Vec::new()
                    };
                    for capture in &child_prototype.upvalues {
                        let upvalue = match *capture {
                            blu_bytecode::blu::Upvalue::ParentRegister(register) => {
                                let slot = open_upvalues.get_mut(register as usize).ok_or(
                                    RuntimeError::Register {
                                        register: register as usize,
                                        count: registers.len(),
                                    },
                                )?;
                                if let Some(upvalue) = *slot {
                                    upvalue
                                } else {
                                    let value = blu_register(&registers, register)?.clone();
                                    let upvalue = self.allocate_upvalue(value, &roots)?;
                                    try_reserve_exact(
                                        &mut roots.upvalues,
                                        1,
                                        "BluV1 open upvalue roots",
                                    )?;
                                    roots.upvalues.push(upvalue);
                                    *slot = Some(upvalue);
                                    upvalue
                                }
                            }
                            blu_bytecode::blu::Upvalue::ParentUpvalue(upvalue) => *parent_upvalues
                                .get(upvalue as usize)
                                .ok_or(RuntimeError::Upvalue {
                                    upvalue: upvalue as usize,
                                    count: parent_upvalues.len(),
                                })?,
                        };
                        self.closure_push_upvalue(child_closure, upvalue, &roots)?;
                    }
                    set_blu_register(
                        &mut self.heap,
                        &mut registers,
                        &open_upvalues,
                        destination,
                        Value::Closure(child_closure),
                    )?;
                }
                BluInstruction::GetUpvalue {
                    destination,
                    upvalue,
                } => {
                    let closure = closure.ok_or(RuntimeError::MissingClosure)?;
                    let (_, _, _, upvalues) = self.heap.blu_closure_parts(closure)?;
                    let upvalue = *upvalues
                        .get(upvalue as usize)
                        .ok_or(RuntimeError::Upvalue {
                            upvalue: upvalue as usize,
                            count: upvalues.len(),
                        })?;
                    let value = self.heap.upvalue_get(upvalue)?;
                    set_blu_register(
                        &mut self.heap,
                        &mut registers,
                        &open_upvalues,
                        destination,
                        value,
                    )?;
                }
                BluInstruction::SetUpvalue { upvalue, source } => {
                    let closure = closure.ok_or(RuntimeError::MissingClosure)?;
                    let (_, _, _, upvalues) = self.heap.blu_closure_parts(closure)?;
                    let upvalue = *upvalues
                        .get(upvalue as usize)
                        .ok_or(RuntimeError::Upvalue {
                            upvalue: upvalue as usize,
                            count: upvalues.len(),
                        })?;
                    let value = blu_register(&registers, source)?.clone();
                    self.heap.upvalue_set(upvalue, value)?;
                }
                BluInstruction::Add { .. }
                | BluInstruction::Subtract { .. }
                | BluInstruction::Multiply { .. }
                | BluInstruction::Divide { .. }
                | BluInstruction::Modulo { .. }
                | BluInstruction::Power { .. }
                | BluInstruction::FloorDivide { .. } => {
                    let (destination, left_register, right_register, opcode, event) =
                        match instruction {
                            BluInstruction::Add {
                                destination,
                                left,
                                right,
                            } => (destination, left, right, Opcode::Add, "__add"),
                            BluInstruction::Subtract {
                                destination,
                                left,
                                right,
                            } => (destination, left, right, Opcode::Sub, "__sub"),
                            BluInstruction::Multiply {
                                destination,
                                left,
                                right,
                            } => (destination, left, right, Opcode::Mul, "__mul"),
                            BluInstruction::Divide {
                                destination,
                                left,
                                right,
                            } => (destination, left, right, Opcode::Div, "__div"),
                            BluInstruction::Modulo {
                                destination,
                                left,
                                right,
                            } => (destination, left, right, Opcode::Mod, "__mod"),
                            BluInstruction::Power {
                                destination,
                                left,
                                right,
                            } => (destination, left, right, Opcode::Pow, "__pow"),
                            BluInstruction::FloorDivide {
                                destination,
                                left,
                                right,
                            } => (destination, left, right, Opcode::IDiv, "__idiv"),
                            _ => unreachable!(),
                        };
                    let left = blu_register(&registers, left_register)?.clone();
                    let right = blu_register(&registers, right_register)?.clone();
                    let numeric_left = arithmetic_numeric_value(&left, prototype.profile);
                    let numeric_right = arithmetic_numeric_value(&right, prototype.profile);
                    if let (Some(numeric_left), Some(numeric_right)) = (numeric_left, numeric_right)
                    {
                        let value = if opcode == Opcode::Mod
                            && let (Value::Integer(left), Value::Integer(right)) =
                                (&numeric_left, &numeric_right)
                        {
                            Value::Integer(integer_floor_mod(*left, *right)?)
                        } else {
                            arithmetic(opcode, &numeric_left, &numeric_right)?
                        };
                        set_blu_register(
                            &mut self.heap,
                            &mut registers,
                            &open_upvalues,
                            destination,
                            value,
                        )?;
                    } else {
                        let function = self
                            .metamethod(&left, event)?
                            .or(self.metamethod(&right, event)?)
                            .ok_or(RuntimeError::Type {
                                operation: "arithmetic",
                                expected: "number or arithmetic metamethod",
                                actual: left.type_name(),
                            })?;
                        let mut arguments =
                            try_vec_with_capacity(2, "BluV1 arithmetic metamethod arguments")?;
                        arguments.push(left);
                        arguments.push(right);
                        let (function, arguments) =
                            self.resolve_blu_callable(function, arguments, prototype.profile)?;
                        if let Value::Closure(child_closure) = &function
                            && self.heap.is_blu_closure(*child_closure)?
                        {
                            if callers.len() >= self.call_limit {
                                return Err(RuntimeError::CallLimit {
                                    limit: self.call_limit,
                                });
                            }
                            let (child_artifact, child, profile, _) =
                                self.heap.blu_closure_parts(*child_closure)?;
                            let child_prototype = child_artifact
                                .prototypes
                                .get(child)
                                .ok_or(RuntimeError::InvalidPrototype(child))?;
                            let child_constants = materialize_blu_constants(child_prototype)?;
                            let child_register_count = usize::from(child_prototype.register_count);
                            let mut child_registers = try_vec_with_capacity(
                                child_register_count,
                                "BluV1 runtime registers",
                            )?;
                            child_registers.resize(child_register_count, Value::Nil);
                            let copied = arguments
                                .len()
                                .min(usize::from(child_prototype.parameter_count));
                            child_registers[..copied].clone_from_slice(&arguments[..copied]);
                            let child_varargs = if child_prototype.is_vararg {
                                try_clone_values(
                                    arguments
                                        .get(usize::from(child_prototype.parameter_count)..)
                                        .unwrap_or_default(),
                                    "BluV1 frame varargs",
                                )?
                            } else {
                                Vec::new()
                            };
                            let mut child_open_upvalues =
                                try_vec_with_capacity(child_register_count, "BluV1 open upvalues")?;
                            child_open_upvalues.resize(child_register_count, None);
                            try_reserve_exact(
                                &mut callers,
                                1,
                                "BluV1 arithmetic metamethod caller frame",
                            )?;
                            callers.push(BluCaller {
                                artifact,
                                prototype: prototype_index,
                                constants,
                                registers,
                                varargs,
                                open_upvalues,
                                closure,
                                pc: pc + 1,
                                result: BluCallResult::Fixed {
                                    destination,
                                    count: 1,
                                },
                            });
                            artifact = child_artifact;
                            prototype_index = child;
                            constants = child_constants;
                            registers = child_registers;
                            varargs = child_varargs;
                            open_upvalues = child_open_upvalues;
                            closure = Some(*child_closure);
                            pc = 0;
                            self.active_profile = Some(profile);
                            continue;
                        }
                        let roots = blu_frame_roots(
                            &registers,
                            &varargs,
                            &open_upvalues,
                            closure,
                            &callers,
                        )?;
                        let value = self
                            .call_value(function, &arguments, &mut remaining, callers.len(), roots)?
                            .into_iter()
                            .next()
                            .unwrap_or(Value::Nil);
                        set_blu_register(
                            &mut self.heap,
                            &mut registers,
                            &open_upvalues,
                            destination,
                            value,
                        )?;
                    }
                }
                BluInstruction::BitwiseAnd { .. }
                | BluInstruction::BitwiseOr { .. }
                | BluInstruction::BitwiseExclusiveOr { .. }
                | BluInstruction::ShiftLeft { .. }
                | BluInstruction::ShiftRight { .. }
                | BluInstruction::BitwiseNot { .. } => {
                    let (destination, left_register, right_register, event) = match instruction {
                        BluInstruction::BitwiseAnd {
                            destination,
                            left,
                            right,
                        } => (destination, left, right, "__band"),
                        BluInstruction::BitwiseOr {
                            destination,
                            left,
                            right,
                        } => (destination, left, right, "__bor"),
                        BluInstruction::BitwiseExclusiveOr {
                            destination,
                            left,
                            right,
                        } => (destination, left, right, "__bxor"),
                        BluInstruction::ShiftLeft {
                            destination,
                            left,
                            right,
                        } => (destination, left, right, "__shl"),
                        BluInstruction::ShiftRight {
                            destination,
                            left,
                            right,
                        } => (destination, left, right, "__shr"),
                        BluInstruction::BitwiseNot {
                            destination,
                            source,
                        } => (destination, source, source, "__bnot"),
                        _ => unreachable!("bitwise execution arm filters the instruction"),
                    };
                    let left = blu_register(&registers, left_register)?.clone();
                    let right = blu_register(&registers, right_register)?.clone();
                    let converted_left =
                        blu_bitwise_integer(&left, prototype.profile, "bitwise operation");
                    let converted_right =
                        blu_bitwise_integer(&right, prototype.profile, "bitwise operation");
                    if let (Ok(left_integer), Ok(right_integer)) = (converted_left, converted_right)
                    {
                        let result = match instruction {
                            BluInstruction::BitwiseAnd { .. } => left_integer & right_integer,
                            BluInstruction::BitwiseOr { .. } => left_integer | right_integer,
                            BluInstruction::BitwiseExclusiveOr { .. } => {
                                left_integer ^ right_integer
                            }
                            BluInstruction::ShiftLeft { .. } => {
                                lua_shift_left(left_integer, right_integer)
                            }
                            BluInstruction::ShiftRight { .. } => {
                                lua_shift_left(left_integer, right_integer.wrapping_neg())
                            }
                            BluInstruction::BitwiseNot { .. } => !left_integer,
                            _ => unreachable!("bitwise execution arm filters the instruction"),
                        };
                        set_blu_register(
                            &mut self.heap,
                            &mut registers,
                            &open_upvalues,
                            destination,
                            Value::Integer(result),
                        )?;
                    } else {
                        let function = self
                            .metamethod(&left, event)?
                            .or(self.metamethod(&right, event)?);
                        let Some(function) = function else {
                            blu_bitwise_integer(&left, prototype.profile, "bitwise operation")?;
                            blu_bitwise_integer(&right, prototype.profile, "bitwise operation")?;
                            unreachable!("one bitwise operand conversion failed");
                        };
                        let mut arguments =
                            try_vec_with_capacity(2, "BluV1 bitwise metamethod arguments")?;
                        arguments.push(left);
                        arguments.push(right);
                        let (function, arguments) =
                            self.resolve_blu_callable(function, arguments, prototype.profile)?;
                        if let Value::Closure(child_closure) = &function
                            && self.heap.is_blu_closure(*child_closure)?
                        {
                            if callers.len() >= self.call_limit {
                                return Err(RuntimeError::CallLimit {
                                    limit: self.call_limit,
                                });
                            }
                            let (child_artifact, child, profile, _) =
                                self.heap.blu_closure_parts(*child_closure)?;
                            let child_prototype = child_artifact
                                .prototypes
                                .get(child)
                                .ok_or(RuntimeError::InvalidPrototype(child))?;
                            let child_constants = materialize_blu_constants(child_prototype)?;
                            let child_register_count = usize::from(child_prototype.register_count);
                            let mut child_registers = try_vec_with_capacity(
                                child_register_count,
                                "BluV1 runtime registers",
                            )?;
                            child_registers.resize(child_register_count, Value::Nil);
                            let copied = arguments
                                .len()
                                .min(usize::from(child_prototype.parameter_count));
                            child_registers[..copied].clone_from_slice(&arguments[..copied]);
                            let child_varargs = if child_prototype.is_vararg {
                                try_clone_values(
                                    arguments
                                        .get(usize::from(child_prototype.parameter_count)..)
                                        .unwrap_or_default(),
                                    "BluV1 frame varargs",
                                )?
                            } else {
                                Vec::new()
                            };
                            let mut child_open_upvalues =
                                try_vec_with_capacity(child_register_count, "BluV1 open upvalues")?;
                            child_open_upvalues.resize(child_register_count, None);
                            try_reserve_exact(
                                &mut callers,
                                1,
                                "BluV1 bitwise metamethod caller frame",
                            )?;
                            callers.push(BluCaller {
                                artifact,
                                prototype: prototype_index,
                                constants,
                                registers,
                                varargs,
                                open_upvalues,
                                closure,
                                pc: pc + 1,
                                result: BluCallResult::Fixed {
                                    destination,
                                    count: 1,
                                },
                            });
                            artifact = child_artifact;
                            prototype_index = child;
                            constants = child_constants;
                            registers = child_registers;
                            varargs = child_varargs;
                            open_upvalues = child_open_upvalues;
                            closure = Some(*child_closure);
                            pc = 0;
                            self.active_profile = Some(profile);
                            continue;
                        }
                        let roots = blu_frame_roots(
                            &registers,
                            &varargs,
                            &open_upvalues,
                            closure,
                            &callers,
                        )?;
                        let value = self
                            .call_value(function, &arguments, &mut remaining, callers.len(), roots)?
                            .into_iter()
                            .next()
                            .unwrap_or(Value::Nil);
                        set_blu_register(
                            &mut self.heap,
                            &mut registers,
                            &open_upvalues,
                            destination,
                            value,
                        )?;
                    }
                }
                BluInstruction::Move {
                    destination,
                    source,
                } => {
                    let value = blu_register(&registers, source)?.clone();
                    set_blu_register(
                        &mut self.heap,
                        &mut registers,
                        &open_upvalues,
                        destination,
                        value,
                    )?;
                }
                BluInstruction::Not {
                    destination,
                    source,
                } => {
                    let value = Value::Boolean(!blu_register(&registers, source)?.is_truthy());
                    set_blu_register(
                        &mut self.heap,
                        &mut registers,
                        &open_upvalues,
                        destination,
                        value,
                    )?;
                }
                BluInstruction::Negate {
                    destination,
                    source,
                } => {
                    let source = blu_register(&registers, source)?.clone();
                    let value = match &source {
                        Value::Integer(value) => Value::Integer(value.wrapping_neg()),
                        Value::Number(value) => Value::Number(-value),
                        other => {
                            let function =
                                self.metamethod(other, "__unm")?.ok_or(RuntimeError::Type {
                                    operation: "unary minus",
                                    expected: "number or __unm metamethod",
                                    actual: other.type_name(),
                                })?;
                            let mut arguments = try_vec_with_capacity(2, "BluV1 __unm arguments")?;
                            arguments.push(source.clone());
                            arguments.push(source);
                            let (function, arguments) =
                                self.resolve_blu_callable(function, arguments, prototype.profile)?;
                            if let Value::Closure(child_closure) = &function
                                && self.heap.is_blu_closure(*child_closure)?
                            {
                                if callers.len() >= self.call_limit {
                                    return Err(RuntimeError::CallLimit {
                                        limit: self.call_limit,
                                    });
                                }
                                let (child_artifact, child, profile, _) =
                                    self.heap.blu_closure_parts(*child_closure)?;
                                let child_prototype = child_artifact
                                    .prototypes
                                    .get(child)
                                    .ok_or(RuntimeError::InvalidPrototype(child))?;
                                let child_constants = materialize_blu_constants(child_prototype)?;
                                let child_register_count =
                                    usize::from(child_prototype.register_count);
                                let mut child_registers = try_vec_with_capacity(
                                    child_register_count,
                                    "BluV1 runtime registers",
                                )?;
                                child_registers.resize(child_register_count, Value::Nil);
                                let copied = arguments
                                    .len()
                                    .min(usize::from(child_prototype.parameter_count));
                                child_registers[..copied].clone_from_slice(&arguments[..copied]);
                                let child_varargs = if child_prototype.is_vararg {
                                    try_clone_values(
                                        arguments
                                            .get(usize::from(child_prototype.parameter_count)..)
                                            .unwrap_or_default(),
                                        "BluV1 frame varargs",
                                    )?
                                } else {
                                    Vec::new()
                                };
                                let mut child_open_upvalues = try_vec_with_capacity(
                                    child_register_count,
                                    "BluV1 open upvalues",
                                )?;
                                child_open_upvalues.resize(child_register_count, None);
                                try_reserve_exact(&mut callers, 1, "BluV1 __unm caller frame")?;
                                callers.push(BluCaller {
                                    artifact,
                                    prototype: prototype_index,
                                    constants,
                                    registers,
                                    varargs,
                                    open_upvalues,
                                    closure,
                                    pc: pc + 1,
                                    result: BluCallResult::Fixed {
                                        destination,
                                        count: 1,
                                    },
                                });
                                artifact = child_artifact;
                                prototype_index = child;
                                constants = child_constants;
                                registers = child_registers;
                                varargs = child_varargs;
                                open_upvalues = child_open_upvalues;
                                closure = Some(*child_closure);
                                pc = 0;
                                self.active_profile = Some(profile);
                                continue;
                            }
                            let roots = blu_frame_roots(
                                &registers,
                                &varargs,
                                &open_upvalues,
                                closure,
                                &callers,
                            )?;
                            self.call_value(
                                function,
                                &arguments,
                                &mut remaining,
                                callers.len(),
                                roots,
                            )?
                            .into_iter()
                            .next()
                            .unwrap_or(Value::Nil)
                        }
                    };
                    set_blu_register(
                        &mut self.heap,
                        &mut registers,
                        &open_upvalues,
                        destination,
                        value,
                    )?;
                }
                BluInstruction::Length {
                    destination,
                    source,
                } => {
                    let source = blu_register(&registers, source)?.clone();
                    let (length, handler) = match &source {
                        Value::String(bytes) => (Some(bytes.len()), None),
                        Value::Table(table) => {
                            if prototype.profile != SemanticProfile::Lua51
                                && let Some(handler) = self.metamethod(&source, "__len")?
                            {
                                (None, Some(handler))
                            } else {
                                (Some(self.heap.table_length(*table)?), None)
                            }
                        }
                        other => {
                            return Err(RuntimeError::Type {
                                operation: "length",
                                expected: "string or table",
                                actual: other.type_name(),
                            });
                        }
                    };
                    if let Some(function) = handler {
                        let mut arguments = try_vec_with_capacity(1, "BluV1 __len arguments")?;
                        arguments.push(source);
                        let (function, arguments) =
                            self.resolve_blu_callable(function, arguments, prototype.profile)?;
                        if let Value::Closure(child_closure) = &function
                            && self.heap.is_blu_closure(*child_closure)?
                        {
                            if callers.len() >= self.call_limit {
                                return Err(RuntimeError::CallLimit {
                                    limit: self.call_limit,
                                });
                            }
                            let (child_artifact, child, profile, _) =
                                self.heap.blu_closure_parts(*child_closure)?;
                            let child_prototype = child_artifact
                                .prototypes
                                .get(child)
                                .ok_or(RuntimeError::InvalidPrototype(child))?;
                            let child_constants = materialize_blu_constants(child_prototype)?;
                            let child_register_count = usize::from(child_prototype.register_count);
                            let mut child_registers = try_vec_with_capacity(
                                child_register_count,
                                "BluV1 runtime registers",
                            )?;
                            child_registers.resize(child_register_count, Value::Nil);
                            let copied = arguments
                                .len()
                                .min(usize::from(child_prototype.parameter_count));
                            child_registers[..copied].clone_from_slice(&arguments[..copied]);
                            let child_varargs = if child_prototype.is_vararg {
                                try_clone_values(
                                    arguments
                                        .get(usize::from(child_prototype.parameter_count)..)
                                        .unwrap_or_default(),
                                    "BluV1 frame varargs",
                                )?
                            } else {
                                Vec::new()
                            };
                            let mut child_open_upvalues =
                                try_vec_with_capacity(child_register_count, "BluV1 open upvalues")?;
                            child_open_upvalues.resize(child_register_count, None);
                            try_reserve_exact(&mut callers, 1, "BluV1 __len caller frame")?;
                            callers.push(BluCaller {
                                artifact,
                                prototype: prototype_index,
                                constants,
                                registers,
                                varargs,
                                open_upvalues,
                                closure,
                                pc: pc + 1,
                                result: BluCallResult::Fixed {
                                    destination,
                                    count: 1,
                                },
                            });
                            artifact = child_artifact;
                            prototype_index = child;
                            constants = child_constants;
                            registers = child_registers;
                            varargs = child_varargs;
                            open_upvalues = child_open_upvalues;
                            closure = Some(*child_closure);
                            pc = 0;
                            self.active_profile = Some(profile);
                            continue;
                        }
                        let roots = blu_frame_roots(
                            &registers,
                            &varargs,
                            &open_upvalues,
                            closure,
                            &callers,
                        )?;
                        let value = self
                            .call_value(function, &arguments, &mut remaining, callers.len(), roots)?
                            .into_iter()
                            .next()
                            .unwrap_or(Value::Nil);
                        set_blu_register(
                            &mut self.heap,
                            &mut registers,
                            &open_upvalues,
                            destination,
                            value,
                        )?;
                        pc += 1;
                        continue;
                    }
                    let length = length.ok_or(RuntimeError::UnsupportedBluV1Structure {
                        what: "length result is missing",
                    })?;
                    let length = i64::try_from(length).map_err(|_| {
                        RuntimeError::UnsupportedBluV1Structure {
                            what: "length exceeds i64",
                        }
                    })?;
                    let value = if matches!(
                        prototype.profile,
                        SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
                    ) {
                        Value::Integer(length)
                    } else {
                        Value::Number(length as f64)
                    };
                    set_blu_register(
                        &mut self.heap,
                        &mut registers,
                        &open_upvalues,
                        destination,
                        value,
                    )?;
                }
                BluInstruction::Concatenate {
                    destination,
                    left,
                    right,
                } => {
                    let left = blu_register(&registers, left)?.clone();
                    let right = blu_register(&registers, right)?.clone();
                    if try_concat_bytes(&left)?.is_some() && try_concat_bytes(&right)?.is_some() {
                        let value = self.concat_value(
                            left,
                            right,
                            CallContext::new(&mut remaining, 0, GcRoots::default()),
                        )?;
                        set_blu_register(
                            &mut self.heap,
                            &mut registers,
                            &open_upvalues,
                            destination,
                            value,
                        )?;
                    } else {
                        let function = self
                            .metamethod(&left, "__concat")?
                            .or(self.metamethod(&right, "__concat")?)
                            .ok_or(RuntimeError::Type {
                                operation: "concatenation",
                                expected: "string, number, or __concat metamethod",
                                actual: left.type_name(),
                            })?;
                        let mut arguments = try_vec_with_capacity(2, "BluV1 __concat arguments")?;
                        arguments.push(left);
                        arguments.push(right);
                        let (function, arguments) =
                            self.resolve_blu_callable(function, arguments, prototype.profile)?;
                        if let Value::Closure(child_closure) = &function
                            && self.heap.is_blu_closure(*child_closure)?
                        {
                            if callers.len() >= self.call_limit {
                                return Err(RuntimeError::CallLimit {
                                    limit: self.call_limit,
                                });
                            }
                            let (child_artifact, child, profile, _) =
                                self.heap.blu_closure_parts(*child_closure)?;
                            let child_prototype = child_artifact
                                .prototypes
                                .get(child)
                                .ok_or(RuntimeError::InvalidPrototype(child))?;
                            let child_constants = materialize_blu_constants(child_prototype)?;
                            let child_register_count = usize::from(child_prototype.register_count);
                            let mut child_registers = try_vec_with_capacity(
                                child_register_count,
                                "BluV1 runtime registers",
                            )?;
                            child_registers.resize(child_register_count, Value::Nil);
                            let copied = arguments
                                .len()
                                .min(usize::from(child_prototype.parameter_count));
                            child_registers[..copied].clone_from_slice(&arguments[..copied]);
                            let child_varargs = if child_prototype.is_vararg {
                                try_clone_values(
                                    arguments
                                        .get(usize::from(child_prototype.parameter_count)..)
                                        .unwrap_or_default(),
                                    "BluV1 frame varargs",
                                )?
                            } else {
                                Vec::new()
                            };
                            let mut child_open_upvalues =
                                try_vec_with_capacity(child_register_count, "BluV1 open upvalues")?;
                            child_open_upvalues.resize(child_register_count, None);
                            try_reserve_exact(&mut callers, 1, "BluV1 __concat caller frame")?;
                            callers.push(BluCaller {
                                artifact,
                                prototype: prototype_index,
                                constants,
                                registers,
                                varargs,
                                open_upvalues,
                                closure,
                                pc: pc + 1,
                                result: BluCallResult::Fixed {
                                    destination,
                                    count: 1,
                                },
                            });
                            artifact = child_artifact;
                            prototype_index = child;
                            constants = child_constants;
                            registers = child_registers;
                            varargs = child_varargs;
                            open_upvalues = child_open_upvalues;
                            closure = Some(*child_closure);
                            pc = 0;
                            self.active_profile = Some(profile);
                            continue;
                        }
                        let roots = blu_frame_roots(
                            &registers,
                            &varargs,
                            &open_upvalues,
                            closure,
                            &callers,
                        )?;
                        let value = self
                            .call_value(function, &arguments, &mut remaining, callers.len(), roots)?
                            .into_iter()
                            .next()
                            .unwrap_or(Value::Nil);
                        set_blu_register(
                            &mut self.heap,
                            &mut registers,
                            &open_upvalues,
                            destination,
                            value,
                        )?;
                    }
                }
                BluInstruction::Equal {
                    destination,
                    left,
                    right,
                }
                | BluInstruction::LessThan {
                    destination,
                    left,
                    right,
                }
                | BluInstruction::LessEqual {
                    destination,
                    left,
                    right,
                } => {
                    let opcode = match instruction {
                        BluInstruction::Equal { .. } => Opcode::JumpIfEq,
                        BluInstruction::LessThan { .. } => Opcode::JumpIfLt,
                        BluInstruction::LessEqual { .. } => Opcode::JumpIfLe,
                        _ => unreachable!(),
                    };
                    let left = blu_register(&registers, left)?.clone();
                    let right = blu_register(&registers, right)?.clone();
                    let modern = matches!(
                        prototype.profile,
                        SemanticProfile::Blu
                            | SemanticProfile::Lua53
                            | SemanticProfile::Lua54
                            | SemanticProfile::Lua55
                    );
                    let raw = match opcode {
                        Opcode::JumpIfEq if left == right => Some(true),
                        Opcode::JumpIfEq
                            if !matches!((&left, &right), (Value::Table(_), Value::Table(_))) =>
                        {
                            Some(false)
                        }
                        Opcode::JumpIfLt => {
                            if let Some(value) = left.numeric_less(&right) {
                                Some(value)
                            } else if let (Value::String(left), Value::String(right)) =
                                (&left, &right)
                            {
                                Some(left < right)
                            } else {
                                None
                            }
                        }
                        Opcode::JumpIfLe => {
                            if let Some(value) = left.numeric_less_equal(&right) {
                                Some(value)
                            } else if let (Value::String(left), Value::String(right)) =
                                (&left, &right)
                            {
                                Some(left <= right)
                            } else {
                                None
                            }
                        }
                        _ => None,
                    };
                    if let Some(value) = raw {
                        set_blu_register(
                            &mut self.heap,
                            &mut registers,
                            &open_upvalues,
                            destination,
                            Value::Boolean(value),
                        )?;
                    } else {
                        let select =
                            |vm: &Self,
                             left: &Value,
                             right: &Value,
                             name: &'static str|
                             -> Result<Option<Value>, RuntimeError> {
                                if modern {
                                    Ok(vm.metamethod(left, name)?.or(vm.metamethod(right, name)?))
                                } else {
                                    vm.shared_metamethod(left, right, name)
                                }
                            };
                        let (function, arguments, negate) = match opcode {
                            Opcode::JumpIfEq => match select(self, &left, &right, "__eq")? {
                                Some(function) => (function, [left, right], false),
                                None => {
                                    set_blu_register(
                                        &mut self.heap,
                                        &mut registers,
                                        &open_upvalues,
                                        destination,
                                        Value::Boolean(false),
                                    )?;
                                    pc += 1;
                                    continue;
                                }
                            },
                            Opcode::JumpIfLt => {
                                let function = select(self, &left, &right, "__lt")?.ok_or(
                                    RuntimeError::Type {
                                        operation: "comparison",
                                        expected: "matching values or __lt metamethods",
                                        actual: left.type_name(),
                                    },
                                )?;
                                (function, [left, right], false)
                            }
                            Opcode::JumpIfLe => {
                                if let Some(function) = select(self, &left, &right, "__le")? {
                                    (function, [left, right], false)
                                } else if prototype.profile != SemanticProfile::Lua55
                                    && let Some(function) = select(self, &right, &left, "__lt")?
                                {
                                    (function, [right, left], true)
                                } else {
                                    return Err(RuntimeError::Type {
                                        operation: "comparison",
                                        expected: "matching values or __le/__lt metamethods",
                                        actual: left.type_name(),
                                    });
                                }
                            }
                            _ => return Err(RuntimeError::UnsupportedComparison(opcode)),
                        };
                        let arguments = try_clone_values(&arguments, "BluV1 comparison arguments")?;
                        let (function, arguments) =
                            self.resolve_blu_callable(function, arguments, prototype.profile)?;
                        if let Value::Closure(child_closure) = &function
                            && self.heap.is_blu_closure(*child_closure)?
                        {
                            if callers.len() >= self.call_limit {
                                return Err(RuntimeError::CallLimit {
                                    limit: self.call_limit,
                                });
                            }
                            let (child_artifact, child, profile, _) =
                                self.heap.blu_closure_parts(*child_closure)?;
                            let child_prototype = child_artifact
                                .prototypes
                                .get(child)
                                .ok_or(RuntimeError::InvalidPrototype(child))?;
                            let child_constants = materialize_blu_constants(child_prototype)?;
                            let child_register_count = usize::from(child_prototype.register_count);
                            let mut child_registers = try_vec_with_capacity(
                                child_register_count,
                                "BluV1 runtime registers",
                            )?;
                            child_registers.resize(child_register_count, Value::Nil);
                            let copied = arguments
                                .len()
                                .min(usize::from(child_prototype.parameter_count));
                            child_registers[..copied].clone_from_slice(&arguments[..copied]);
                            let child_varargs = if child_prototype.is_vararg {
                                try_clone_values(
                                    arguments
                                        .get(usize::from(child_prototype.parameter_count)..)
                                        .unwrap_or_default(),
                                    "BluV1 frame varargs",
                                )?
                            } else {
                                Vec::new()
                            };
                            let mut child_open_upvalues =
                                try_vec_with_capacity(child_register_count, "BluV1 open upvalues")?;
                            child_open_upvalues.resize(child_register_count, None);
                            try_reserve_exact(&mut callers, 1, "BluV1 comparison caller frame")?;
                            callers.push(BluCaller {
                                artifact,
                                prototype: prototype_index,
                                constants,
                                registers,
                                varargs,
                                open_upvalues,
                                closure,
                                pc: pc + 1,
                                result: BluCallResult::Truthy {
                                    destination,
                                    negate,
                                },
                            });
                            artifact = child_artifact;
                            prototype_index = child;
                            constants = child_constants;
                            registers = child_registers;
                            varargs = child_varargs;
                            open_upvalues = child_open_upvalues;
                            closure = Some(*child_closure);
                            pc = 0;
                            self.active_profile = Some(profile);
                            continue;
                        }
                        let roots = blu_frame_roots(
                            &registers,
                            &varargs,
                            &open_upvalues,
                            closure,
                            &callers,
                        )?;
                        let value = self
                            .call_value(function, &arguments, &mut remaining, callers.len(), roots)?
                            .first()
                            .is_some_and(Value::is_truthy)
                            != negate;
                        set_blu_register(
                            &mut self.heap,
                            &mut registers,
                            &open_upvalues,
                            destination,
                            Value::Boolean(value),
                        )?;
                    }
                }
                BluInstruction::JumpIfTruthy { condition, target }
                | BluInstruction::JumpIfFalsy { condition, target } => {
                    let should_jump = match instruction {
                        BluInstruction::JumpIfTruthy { .. } => {
                            blu_register(&registers, condition)?.is_truthy()
                        }
                        BluInstruction::JumpIfFalsy { .. } => {
                            !blu_register(&registers, condition)?.is_truthy()
                        }
                        _ => unreachable!(),
                    };
                    if should_jump {
                        pc = usize::try_from(target).map_err(|_| {
                            RuntimeError::InvalidProgramCounter {
                                pc: usize::MAX,
                                code_words: prototype.code.len(),
                            }
                        })?;
                        if pc >= prototype.code.len() {
                            return Err(RuntimeError::InvalidProgramCounter {
                                pc,
                                code_words: prototype.code.len(),
                            });
                        }
                        continue;
                    }
                }
                BluInstruction::Jump { target } => {
                    pc = usize::try_from(target).map_err(|_| {
                        RuntimeError::InvalidProgramCounter {
                            pc: usize::MAX,
                            code_words: prototype.code.len(),
                        }
                    })?;
                    if pc >= prototype.code.len() {
                        return Err(RuntimeError::InvalidProgramCounter {
                            pc,
                            code_words: prototype.code.len(),
                        });
                    }
                    continue;
                }
                BluInstruction::Return { first, count } => {
                    let start = usize::from(first);
                    let end =
                        start
                            .checked_add(usize::from(count))
                            .ok_or(RuntimeError::Register {
                                register: usize::MAX,
                                count: registers.len(),
                            })?;
                    let values = registers.get(start..end).ok_or(RuntimeError::Register {
                        register: end.saturating_sub(1),
                        count: registers.len(),
                    })?;
                    let values = try_clone_values(values, "BluV1 return values")?;
                    let mut values = values;
                    let mut caller = loop {
                        let Some(caller) = callers.pop() else {
                            return Ok(values);
                        };
                        match self.apply_blu_return(caller, values, &callers)? {
                            BluReturnDisposition::Resume(caller) => break caller,
                            BluReturnDisposition::Propagate(next) => values = next,
                        }
                    };
                    dynamic_results =
                        match core::mem::replace(&mut caller.result, BluCallResult::Dynamic) {
                            BluCallResult::ReadyDynamic(values) => values,
                            _ => Vec::new(),
                        };
                    artifact = caller.artifact;
                    prototype_index = caller.prototype;
                    constants = caller.constants;
                    registers = caller.registers;
                    varargs = caller.varargs;
                    open_upvalues = caller.open_upvalues;
                    closure = caller.closure;
                    pc = caller.pc;
                    self.active_profile = Some(
                        artifact
                            .prototypes
                            .get(prototype_index)
                            .ok_or(RuntimeError::InvalidPrototype(prototype_index))?
                            .profile,
                    );
                    continue;
                }
                BluInstruction::ReturnVarargs { first, count } => {
                    let start = usize::from(first);
                    let end =
                        start
                            .checked_add(usize::from(count))
                            .ok_or(RuntimeError::Register {
                                register: usize::MAX,
                                count: registers.len(),
                            })?;
                    let prefix = registers.get(start..end).ok_or(RuntimeError::Register {
                        register: end.saturating_sub(1),
                        count: registers.len(),
                    })?;
                    let mut values = try_clone_values(prefix, "BluV1 vararg return prefix")?;
                    try_reserve_exact(&mut values, varargs.len(), "BluV1 dynamic return values")?;
                    values.extend(varargs.iter().cloned());
                    let mut caller = loop {
                        let Some(caller) = callers.pop() else {
                            return Ok(values);
                        };
                        match self.apply_blu_return(caller, values, &callers)? {
                            BluReturnDisposition::Resume(caller) => break caller,
                            BluReturnDisposition::Propagate(next) => values = next,
                        }
                    };
                    dynamic_results =
                        match core::mem::replace(&mut caller.result, BluCallResult::Dynamic) {
                            BluCallResult::ReadyDynamic(values) => values,
                            _ => Vec::new(),
                        };
                    artifact = caller.artifact;
                    prototype_index = caller.prototype;
                    constants = caller.constants;
                    registers = caller.registers;
                    varargs = caller.varargs;
                    open_upvalues = caller.open_upvalues;
                    closure = caller.closure;
                    pc = caller.pc;
                    self.active_profile = Some(
                        artifact
                            .prototypes
                            .get(prototype_index)
                            .ok_or(RuntimeError::InvalidPrototype(prototype_index))?
                            .profile,
                    );
                    continue;
                }
            }
            pc += 1;
        }
    }

    fn apply_blu_return(
        &mut self,
        mut caller: BluCaller,
        values: Vec<Value>,
        outer_callers: &[BluCaller],
    ) -> Result<BluReturnDisposition, RuntimeError> {
        refresh_blu_open_upvalues(&self.heap, &mut caller.registers, &caller.open_upvalues)?;
        match caller.result {
            BluCallResult::Fixed { destination, count } => {
                let mut values = values.into_iter();
                for offset in 0..count {
                    let destination =
                        destination
                            .checked_add(offset)
                            .ok_or(RuntimeError::Register {
                                register: usize::MAX,
                                count: caller.registers.len(),
                            })?;
                    set_blu_register(
                        &mut self.heap,
                        &mut caller.registers,
                        &caller.open_upvalues,
                        destination,
                        values.next().unwrap_or(Value::Nil),
                    )?;
                }
                Ok(BluReturnDisposition::Resume(caller))
            }
            BluCallResult::Truthy {
                destination,
                negate,
            } => {
                let value = values.first().is_some_and(Value::is_truthy) != negate;
                set_blu_register(
                    &mut self.heap,
                    &mut caller.registers,
                    &caller.open_upvalues,
                    destination,
                    Value::Boolean(value),
                )?;
                Ok(BluReturnDisposition::Resume(caller))
            }
            BluCallResult::ReturnPrefix { first, count } => {
                let start = usize::from(first);
                let end = start
                    .checked_add(usize::from(count))
                    .ok_or(RuntimeError::Register {
                        register: usize::MAX,
                        count: caller.registers.len(),
                    })?;
                let prefix = caller
                    .registers
                    .get(start..end)
                    .ok_or(RuntimeError::Register {
                        register: end.saturating_sub(1),
                        count: caller.registers.len(),
                    })?;
                let mut combined = try_clone_values(prefix, "BluV1 return prefix")?;
                try_reserve_exact(&mut combined, values.len(), "BluV1 dynamic return values")?;
                combined.extend(values);
                Ok(BluReturnDisposition::Propagate(combined))
            }
            BluCallResult::TableList { table, start } => {
                let profile = caller
                    .artifact
                    .prototypes
                    .get(caller.prototype)
                    .ok_or(RuntimeError::InvalidPrototype(caller.prototype))?
                    .profile;
                let mut roots = blu_frame_roots(
                    &caller.registers,
                    &caller.varargs,
                    &caller.open_upvalues,
                    caller.closure,
                    outer_callers,
                )?;
                for value in &values {
                    roots.push_value(value.clone())?;
                }
                self.set_blu_table_list(table, start, values, profile, &roots)?;
                Ok(BluReturnDisposition::Resume(caller))
            }
            BluCallResult::Dynamic => {
                if values.len() > MAX_DYNAMIC_REGISTERS {
                    return Err(RuntimeError::StackLimit {
                        required: values.len(),
                        limit: MAX_DYNAMIC_REGISTERS,
                    });
                }
                caller.result = BluCallResult::ReadyDynamic(values);
                Ok(BluReturnDisposition::Resume(caller))
            }
            BluCallResult::ReadyDynamic(_) => Err(RuntimeError::UnsupportedBluV1Structure {
                what: "completed dynamic call continuation",
            }),
        }
    }

    fn set_blu_table_list(
        &mut self,
        table: TableId,
        start: u32,
        values: Vec<Value>,
        profile: SemanticProfile,
        roots: &GcRoots,
    ) -> Result<(), RuntimeError> {
        if values.len() > MAX_DYNAMIC_REGISTERS {
            return Err(RuntimeError::StackLimit {
                required: values.len(),
                limit: MAX_DYNAMIC_REGISTERS,
            });
        }
        for (offset, value) in values.into_iter().enumerate() {
            let index =
                u64::from(start)
                    .checked_add(offset as u64)
                    .ok_or(RuntimeError::StackLimit {
                        required: usize::MAX,
                        limit: MAX_DYNAMIC_REGISTERS,
                    })?;
            let key = if matches!(
                profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ) {
                Value::Integer(index as i64)
            } else {
                Value::Number(index as f64)
            };
            self.table_set(table, key, value, roots)?;
        }
        Ok(())
    }

    pub fn execute_validated_owned(
        &mut self,
        chunk: ValidatedChunk,
    ) -> Result<Vec<Value>, RuntimeError> {
        let profile = match chunk.semantic_profile() {
            Some(profile @ (SemanticProfile::Blu | SemanticProfile::Luau)) => profile,
            Some(profile) => return Err(RuntimeError::SemanticProfileNotImplemented(profile)),
            None => self.configured_profile()?,
        };
        let chunk = Arc::new(chunk.into_chunk());
        let mut remaining = self.instruction_limit;
        let target = FrameTarget {
            prototype_index: chunk.main,
            profile,
        };
        match self.execute_frame(&chunk, target, None, &[], &mut remaining, 0) {
            Ok(values) => {
                self.retain_host_occurrences(&values)?;
                Ok(values)
            }
            Err(error) => {
                self.retain_error_occurrences(&error)?;
                Err(error)
            }
        }
    }

    fn retain_error_occurrences(&mut self, error: &RuntimeError) -> Result<(), RuntimeError> {
        match error {
            RuntimeError::Raised(value) => {
                self.retain_host_occurrences(std::slice::from_ref(value))
            }
            RuntimeError::CoroutineYield(values) => self.retain_host_occurrences(values),
            _ => Ok(()),
        }
    }

    fn retain_host_occurrences(&mut self, values: &[Value]) -> Result<(), RuntimeError> {
        let additional = values
            .iter()
            .filter(|value| HostRoot::from_value(value).is_some())
            .count();
        let Some(required) = self.host_root_count.checked_add(additional) else {
            return Err(RuntimeError::HostValueLimit {
                required: usize::MAX,
                limit: self.host_value_limit,
            });
        };
        if required > self.host_value_limit {
            return Err(RuntimeError::HostValueLimit {
                required,
                limit: self.host_value_limit,
            });
        }
        self.host_roots
            .try_reserve(additional)
            .map_err(|_| RuntimeError::Allocation {
                what: "host value roots",
            })?;
        // `required` is the checked sum of every existing and new occurrence,
        // so no individual counter can overflow while committing this batch.
        for root in values.iter().filter_map(HostRoot::from_value) {
            *self.host_roots.entry(root).or_insert(0) += 1;
        }
        self.host_root_count = required;
        Ok(())
    }

    fn execute_frame(
        &mut self,
        chunk: &Arc<Chunk>,
        target: FrameTarget,
        closure: Option<ClosureId>,
        arguments: &[Value],
        remaining: &mut u64,
        depth: usize,
    ) -> Result<Vec<Value>, RuntimeError> {
        let active_root_count = self.active_roots.len();
        let result = self.execute_frame_inner(chunk, target, closure, arguments, remaining, depth);
        self.active_roots.truncate(active_root_count);
        result
    }

    fn push_active_roots(&mut self, roots: GcRoots) -> Result<(), RuntimeError> {
        try_reserve_exact(&mut self.active_roots, 1, "active GC roots")?;
        self.active_roots.push(roots);
        Ok(())
    }

    fn execute_frame_inner(
        &mut self,
        chunk: &Arc<Chunk>,
        target: FrameTarget,
        closure: Option<ClosureId>,
        arguments: &[Value],
        remaining: &mut u64,
        depth: usize,
    ) -> Result<Vec<Value>, RuntimeError> {
        if depth > self.call_limit {
            return Err(RuntimeError::CallLimit {
                limit: self.call_limit,
            });
        }
        let prototype = chunk
            .prototypes
            .get(target.prototype_index)
            .ok_or(RuntimeError::InvalidPrototype(target.prototype_index))?;
        let constants = materialize_constants(chunk, prototype)?;
        let frame = Frame::new(
            chunk.clone(),
            target.prototype_index,
            target.profile,
            constants,
            closure,
            arguments,
        )?;
        self.run_frames(frame, Vec::new(), remaining, depth)
    }

    fn run_frames(
        &mut self,
        frame: Frame,
        callers: Vec<Caller>,
        remaining: &mut u64,
        depth: usize,
    ) -> Result<Vec<Value>, RuntimeError> {
        let previous_profile = self.active_profile;
        let result = self.run_frames_inner(frame, callers, remaining, depth);
        self.active_profile = previous_profile;
        result
    }

    fn run_frames_inner(
        &mut self,
        mut frame: Frame,
        mut callers: Vec<Caller>,
        remaining: &mut u64,
        depth: usize,
    ) -> Result<Vec<Value>, RuntimeError> {
        loop {
            self.active_profile = Some(frame.profile);
            let depth = depth + callers.len();
            if *remaining == 0 {
                return Err(RuntimeError::InstructionLimit {
                    limit: self.instruction_limit,
                });
            }
            *remaining -= 1;
            frame.sync_open_upvalues(&mut self.heap)?;

            let instruction = frame.instruction()?;
            let next_pc = instruction.pc() + usize::from(instruction.opcode().words());
            frame.pc = next_pc;
            let chunk = frame.chunk.clone();
            let prototype = chunk
                .prototypes
                .get(frame.prototype_index)
                .ok_or(RuntimeError::InvalidPrototype(frame.prototype_index))?;

            match instruction.opcode() {
                Opcode::Nop
                | Opcode::Coverage
                | Opcode::PrepVarargs
                | Opcode::FastCall
                | Opcode::FastCall1
                | Opcode::FastCall2
                | Opcode::FastCall2K
                | Opcode::FastCall3 => {}
                Opcode::Break => {
                    return Err(RuntimeError::Breakpoint {
                        pc: instruction.pc(),
                    });
                }
                Opcode::LoadNil => frame.set(instruction.a(), Value::Nil)?,
                Opcode::LoadB => {
                    frame.set(instruction.a(), Value::Boolean(instruction.b() != 0))?;
                    if instruction.c() != 0 {
                        frame.pc = instruction.jump_target().ok_or(RuntimeError::InvalidJump {
                            pc: instruction.pc(),
                            target: None,
                        })?;
                    }
                }
                Opcode::LoadN => {
                    frame.set(instruction.a(), Value::Number(f64::from(instruction.d())))?
                }
                Opcode::LoadK => {
                    let value = frame.constant(instruction.d() as i32)?;
                    frame.set(instruction.a(), value)?;
                }
                Opcode::LoadKx => {
                    let index = instruction.aux().ok_or(RuntimeError::MissingAux {
                        pc: instruction.pc(),
                        opcode: instruction.opcode(),
                    })?;
                    let value = frame.constant_u32(index)?;
                    frame.set(instruction.a(), value)?;
                }
                Opcode::Move => {
                    let value = frame.get(instruction.b())?.clone();
                    frame.set(instruction.a(), value)?;
                }
                Opcode::GetGlobal => {
                    let name = frame.constant_u32(instruction.aux().ok_or(
                        RuntimeError::MissingAux {
                            pc: instruction.pc(),
                            opcode: instruction.opcode(),
                        },
                    )?)?;
                    let name = string_bytes(&name, "global lookup")?;
                    let value = self.globals.get(name).cloned().unwrap_or(Value::Nil);
                    frame.set(instruction.a(), value)?;
                }
                Opcode::SetGlobal => {
                    let name = frame.constant_u32(instruction.aux().ok_or(
                        RuntimeError::MissingAux {
                            pc: instruction.pc(),
                            opcode: instruction.opcode(),
                        },
                    )?)?;
                    let name = Arc::<[u8]>::from(string_bytes(&name, "global assignment")?);
                    self.globals
                        .insert(name, frame.get(instruction.a())?.clone());
                }
                Opcode::GetImport => {
                    let path = instruction.aux().ok_or(RuntimeError::MissingAux {
                        pc: instruction.pc(),
                        opcode: instruction.opcode(),
                    })?;
                    let count = path >> 30;
                    let mut value = Value::Nil;
                    for part in 0..count {
                        let shift = 20 - 10 * part;
                        let key = frame.constant_u32((path >> shift) & 1023)?;
                        if part == 0 {
                            value = self
                                .globals
                                .get(string_bytes(&key, "import")?)
                                .cloned()
                                .unwrap_or(Value::Nil);
                        } else {
                            let table = table_id(&value)?;
                            value = self.heap.table_get(table, &key)?;
                        }
                    }
                    frame.set(instruction.a(), value)?;
                }
                Opcode::GetUpval => {
                    let upvalue = frame.upvalue(&self.heap, instruction.b())?;
                    let value = self.heap.upvalue_get(upvalue)?;
                    frame.set(instruction.a(), value)?;
                }
                Opcode::SetUpval => {
                    let value = frame.get(instruction.a())?.clone();
                    let upvalue = frame.upvalue(&self.heap, instruction.b())?;
                    self.heap.upvalue_set(upvalue, value)?;
                }
                Opcode::CloseUpvals => {
                    frame.close_upvalues(&mut self.heap, instruction.a())?;
                }
                Opcode::NewClosure | Opcode::DupClosure => {
                    let child = match instruction.opcode() {
                        Opcode::NewClosure => {
                            let child = usize::try_from(instruction.d())
                                .map_err(|_| RuntimeError::InvalidPrototype(usize::MAX))?;
                            *prototype
                                .children
                                .get(child)
                                .ok_or(RuntimeError::InvalidPrototype(child))?
                        }
                        Opcode::DupClosure => {
                            let constant = usize::try_from(instruction.d()).map_err(|_| {
                                RuntimeError::Constant {
                                    constant: usize::MAX,
                                    count: prototype.constants.len(),
                                }
                            })?;
                            match prototype.constants.get(constant) {
                                Some(Constant::Closure(child)) => *child,
                                _ => {
                                    return Err(RuntimeError::Constant {
                                        constant,
                                        count: prototype.constants.len(),
                                    });
                                }
                            }
                        }
                        _ => unreachable!(),
                    };
                    let upvalue_count = chunk
                        .prototypes
                        .get(child)
                        .ok_or(RuntimeError::InvalidPrototype(child))?
                        .upvalue_count;
                    let frame_roots = frame.gc_roots(&self.heap)?;
                    // A validated Luau chunk has no per-prototype profile
                    // field, so nested legacy prototypes necessarily inherit
                    // their creating frame's profile. BluV1 translation
                    // rejects nested prototypes until that metadata can be
                    // preserved instead of silently collapsing a mixed child.
                    let closure = self.allocate_closure(
                        chunk.clone(),
                        child,
                        frame.profile,
                        usize::from(upvalue_count),
                        &frame_roots,
                    )?;
                    for capture_index in 0..upvalue_count {
                        let capture = frame.instruction()?;
                        if capture.opcode() != Opcode::Capture {
                            return Err(RuntimeError::MissingCapture {
                                pc: instruction.pc(),
                                capture: capture_index,
                                expected: upvalue_count,
                            });
                        }
                        frame.pc = capture.pc() + 1;
                        let mut roots = frame.gc_roots(&self.heap)?;
                        roots.push_value(Value::Closure(closure))?;
                        let upvalue = match capture.a() {
                            0 if capture.b() == instruction.a() => {
                                self.allocate_upvalue(Value::Closure(closure), &roots)?
                            }
                            0 => self.allocate_upvalue(frame.get(capture.b())?.clone(), &roots)?,
                            1 => match frame.open_upvalue(capture.b()) {
                                Some(upvalue) => upvalue,
                                None => {
                                    let upvalue = self.allocate_upvalue(
                                        frame.get(capture.b())?.clone(),
                                        &roots,
                                    )?;
                                    frame.insert_open_upvalue(capture.b(), upvalue)?;
                                    upvalue
                                }
                            },
                            2 => frame.upvalue(&self.heap, capture.b())?,
                            kind => {
                                return Err(RuntimeError::CaptureType {
                                    pc: capture.pc(),
                                    kind,
                                });
                            }
                        };
                        self.closure_push_upvalue(closure, upvalue, &roots)?;
                    }
                    frame.set(instruction.a(), Value::Closure(closure))?;
                }
                Opcode::Capture => {
                    return Err(RuntimeError::UnexpectedCapture {
                        pc: instruction.pc(),
                    });
                }
                Opcode::GetVarargs => {
                    let values = try_clone_values(&frame.varargs, "vararg results")?;
                    frame.write_results(instruction.a(), instruction.b(), values)?;
                }
                Opcode::Add
                | Opcode::Sub
                | Opcode::Mul
                | Opcode::Div
                | Opcode::Mod
                | Opcode::Pow
                | Opcode::IDiv => {
                    let left = frame.get(instruction.b())?.clone();
                    let right = frame.get(instruction.c())?.clone();
                    let value = self.arithmetic_value(
                        instruction.opcode(),
                        left,
                        right,
                        CallContext::new(remaining, depth, frame.gc_roots(&self.heap)?),
                    )?;
                    frame.refresh_open_upvalues(&self.heap)?;
                    frame.set(instruction.a(), value)?;
                }
                Opcode::AddK
                | Opcode::SubK
                | Opcode::MulK
                | Opcode::DivK
                | Opcode::ModK
                | Opcode::PowK
                | Opcode::IDivK => {
                    let left = frame.get(instruction.b())?.clone();
                    let right = frame.constant_u32(u32::from(instruction.c()))?;
                    let value = self.arithmetic_value(
                        instruction.opcode(),
                        left,
                        right,
                        CallContext::new(remaining, depth, frame.gc_roots(&self.heap)?),
                    )?;
                    frame.refresh_open_upvalues(&self.heap)?;
                    frame.set(instruction.a(), value)?;
                }
                Opcode::SubRk | Opcode::DivRk => {
                    let left = frame.constant_u32(u32::from(instruction.b()))?;
                    let right = frame.get(instruction.c())?.clone();
                    let value = self.arithmetic_value(
                        instruction.opcode(),
                        left,
                        right,
                        CallContext::new(remaining, depth, frame.gc_roots(&self.heap)?),
                    )?;
                    frame.refresh_open_upvalues(&self.heap)?;
                    frame.set(instruction.a(), value)?;
                }
                Opcode::And | Opcode::Or => {
                    let left = frame.get(instruction.b())?.clone();
                    let use_right = match instruction.opcode() {
                        Opcode::And => left.is_truthy(),
                        Opcode::Or => !left.is_truthy(),
                        _ => unreachable!(),
                    };
                    let value = if use_right {
                        frame.get(instruction.c())?.clone()
                    } else {
                        left
                    };
                    frame.set(instruction.a(), value)?;
                }
                Opcode::AndK | Opcode::OrK => {
                    let left = frame.get(instruction.b())?.clone();
                    let use_right = match instruction.opcode() {
                        Opcode::AndK => left.is_truthy(),
                        Opcode::OrK => !left.is_truthy(),
                        _ => unreachable!(),
                    };
                    let value = if use_right {
                        frame.constant_u32(u32::from(instruction.c()))?
                    } else {
                        left
                    };
                    frame.set(instruction.a(), value)?;
                }
                Opcode::Not => {
                    let value = !frame.get(instruction.b())?.is_truthy();
                    frame.set(instruction.a(), Value::Boolean(value))?;
                }
                Opcode::Minus => {
                    let value = frame.get(instruction.b())?.clone();
                    let value = match value {
                        Value::Integer(value) => Value::Integer(value.wrapping_neg()),
                        Value::Number(value) => Value::Number(-value),
                        other => {
                            let actual = other.type_name();
                            let function =
                                self.metamethod(&other, "__unm")?
                                    .ok_or(RuntimeError::Type {
                                        operation: "unary minus",
                                        expected: "number or __unm metamethod",
                                        actual,
                                    })?;
                            let arguments = [other.clone(), other];
                            let result = self.call_value(
                                function,
                                &arguments,
                                remaining,
                                depth,
                                frame.gc_roots(&self.heap)?,
                            )?;
                            frame.refresh_open_upvalues(&self.heap)?;
                            result.into_iter().next().unwrap_or(Value::Nil)
                        }
                    };
                    frame.set(instruction.a(), value)?;
                }
                Opcode::Length => {
                    let value = frame.get(instruction.b())?.clone();
                    let result = match value {
                        Value::String(value) => Value::Number(value.len() as f64),
                        Value::Table(table) => {
                            if let Some(function) =
                                self.metamethod(&Value::Table(table), "__len")?
                            {
                                let result = self.call_value(
                                    function,
                                    &[Value::Table(table)],
                                    remaining,
                                    depth,
                                    frame.gc_roots(&self.heap)?,
                                )?;
                                frame.refresh_open_upvalues(&self.heap)?;
                                result.into_iter().next().unwrap_or(Value::Nil)
                            } else {
                                Value::Number(self.heap.table_length(table)? as f64)
                            }
                        }
                        other => {
                            return Err(RuntimeError::Type {
                                operation: "length",
                                expected: "string or table",
                                actual: other.type_name(),
                            });
                        }
                    };
                    frame.set(instruction.a(), result)?;
                }
                Opcode::NewTable => {
                    let array_capacity = instruction.aux().ok_or(RuntimeError::MissingAux {
                        pc: instruction.pc(),
                        opcode: instruction.opcode(),
                    })? as usize;
                    if array_capacity > MAX_TABLE_INITIAL_CAPACITY {
                        return Err(RuntimeError::TableCapacity {
                            kind: "array",
                            requested: array_capacity as u64,
                            limit: MAX_TABLE_INITIAL_CAPACITY,
                        });
                    }
                    let hash_capacity = if instruction.b() == 0 {
                        0
                    } else {
                        1usize
                            .checked_shl(u32::from(instruction.b() - 1))
                            .unwrap_or(usize::MAX)
                    };
                    if hash_capacity > MAX_TABLE_INITIAL_CAPACITY {
                        return Err(RuntimeError::TableCapacity {
                            kind: "hash",
                            requested: u64::try_from(hash_capacity).unwrap_or(u64::MAX),
                            limit: MAX_TABLE_INITIAL_CAPACITY,
                        });
                    }
                    let roots = frame.gc_roots(&self.heap)?;
                    let table = self.allocate_table(array_capacity, hash_capacity, &roots)?;
                    frame.set(instruction.a(), Value::Table(table))?;
                }
                Opcode::DupTable => {
                    let constant =
                        usize::try_from(instruction.d()).map_err(|_| RuntimeError::Constant {
                            constant: usize::MAX,
                            count: prototype.constants.len(),
                        })?;
                    let template =
                        prototype
                            .constants
                            .get(constant)
                            .ok_or(RuntimeError::Constant {
                                constant,
                                count: prototype.constants.len(),
                            })?;
                    let entries = match template {
                        Constant::Table(keys) => keys
                            .iter()
                            .map(|key| Ok((*key, Value::Number(0.0))))
                            .collect::<Result<Vec<_>, RuntimeError>>()?,
                        Constant::TableWithConstants(entries) => entries
                            .iter()
                            .map(|(key, value)| {
                                let value = if *value < 0 {
                                    Value::Number(0.0)
                                } else {
                                    materialize_constant(&chunk, prototype, *value as usize)?
                                };
                                Ok((*key, value))
                            })
                            .collect::<Result<Vec<_>, RuntimeError>>()?,
                        _ => {
                            return Err(RuntimeError::Constant {
                                constant,
                                count: prototype.constants.len(),
                            });
                        }
                    };
                    let roots = frame.gc_roots(&self.heap)?;
                    let table = self.allocate_table(0, entries.len(), &roots)?;
                    for (key, value) in entries {
                        let key = materialize_constant(&chunk, prototype, key)?;
                        self.table_set(table, key, value, &roots)?;
                    }
                    frame.set(instruction.a(), Value::Table(table))?;
                }
                Opcode::GetTable => {
                    let table = frame.get(instruction.b())?.clone();
                    let key = frame.get(instruction.c())?.clone();
                    let value = self.index_value(
                        table,
                        key,
                        "table access",
                        CallContext::new(remaining, depth, frame.gc_roots(&self.heap)?),
                    )?;
                    frame.refresh_open_upvalues(&self.heap)?;
                    frame.set(instruction.a(), value)?;
                }
                Opcode::SetTable => {
                    let value = frame.get(instruction.a())?.clone();
                    let table = frame.get(instruction.b())?.clone();
                    let key = frame.get(instruction.c())?.clone();
                    self.set_index(
                        table,
                        key,
                        value,
                        CallContext::new(remaining, depth, frame.gc_roots(&self.heap)?),
                    )?;
                    frame.refresh_open_upvalues(&self.heap)?;
                }
                Opcode::GetTableKs | Opcode::GetUdataKs => {
                    let table = frame.get(instruction.b())?.clone();
                    let index = table_string_constant(instruction)?;
                    let key = frame.constant_u32(index)?;
                    let value = self.index_value(
                        table,
                        key,
                        "table access",
                        CallContext::new(remaining, depth, frame.gc_roots(&self.heap)?),
                    )?;
                    frame.refresh_open_upvalues(&self.heap)?;
                    frame.set(instruction.a(), value)?;
                }
                Opcode::SetTableKs | Opcode::SetUdataKs => {
                    let value = frame.get(instruction.a())?.clone();
                    let table = frame.get(instruction.b())?.clone();
                    let index = table_string_constant(instruction)?;
                    let key = frame.constant_u32(index)?;
                    self.set_index(
                        table,
                        key,
                        value,
                        CallContext::new(remaining, depth, frame.gc_roots(&self.heap)?),
                    )?;
                    frame.refresh_open_upvalues(&self.heap)?;
                }
                Opcode::GetTableN => {
                    let table = frame.get(instruction.b())?.clone();
                    let key = Value::Integer(i64::from(instruction.c()) + 1);
                    let value = self.index_value(
                        table,
                        key,
                        "table access",
                        CallContext::new(remaining, depth, frame.gc_roots(&self.heap)?),
                    )?;
                    frame.refresh_open_upvalues(&self.heap)?;
                    frame.set(instruction.a(), value)?;
                }
                Opcode::SetTableN => {
                    let value = frame.get(instruction.a())?.clone();
                    let table = frame.get(instruction.b())?.clone();
                    let key = Value::Integer(i64::from(instruction.c()) + 1);
                    self.set_index(
                        table,
                        key,
                        value,
                        CallContext::new(remaining, depth, frame.gc_roots(&self.heap)?),
                    )?;
                    frame.refresh_open_upvalues(&self.heap)?;
                }
                Opcode::SetList => {
                    let table = table_id(frame.get(instruction.a())?)?;
                    let start = instruction.aux().ok_or(RuntimeError::MissingAux {
                        pc: instruction.pc(),
                        opcode: instruction.opcode(),
                    })? as usize;
                    let source = usize::from(instruction.b());
                    let count = if instruction.c() == 0 {
                        frame.top.saturating_sub(source)
                    } else {
                        usize::from(instruction.c() - 1)
                    };
                    for offset in 0..count {
                        let register =
                            u8::try_from(source + offset).map_err(|_| RuntimeError::Register {
                                register: source + offset,
                                count: frame.registers.len(),
                            })?;
                        let value = frame.get(register)?.clone();
                        let roots = frame.gc_roots(&self.heap)?;
                        self.table_set(
                            table,
                            Value::Integer((start + offset) as i64),
                            value,
                            &roots,
                        )?;
                    }
                }
                Opcode::NameCall => {
                    let receiver = frame.get(instruction.b())?.clone();
                    let key = frame.constant_u32(table_string_constant(instruction)?)?;
                    let method = self.index_value(
                        receiver.clone(),
                        key,
                        "method lookup",
                        CallContext::new(remaining, depth, frame.gc_roots(&self.heap)?),
                    )?;
                    frame.refresh_open_upvalues(&self.heap)?;
                    let receiver_register =
                        instruction
                            .a()
                            .checked_add(1)
                            .ok_or(RuntimeError::Register {
                                register: usize::from(instruction.a()) + 1,
                                count: frame.registers.len(),
                            })?;
                    frame.set(instruction.a(), method)?;
                    frame.set(receiver_register, receiver)?;
                }
                Opcode::ForNPrep | Opcode::ForNLoop => {
                    let base = instruction.a();
                    let limit = frame.get(base)?.as_number().ok_or(RuntimeError::Type {
                        operation: "numeric for limit",
                        expected: "number",
                        actual: frame.get(base)?.type_name(),
                    })?;
                    let step_register = base + 1;
                    let step = frame
                        .get(step_register)?
                        .as_number()
                        .ok_or(RuntimeError::Type {
                            operation: "numeric for step",
                            expected: "number",
                            actual: frame.get(step_register)?.type_name(),
                        })?;
                    let index_register = base + 2;
                    let mut index =
                        frame
                            .get(index_register)?
                            .as_number()
                            .ok_or(RuntimeError::Type {
                                operation: "numeric for index",
                                expected: "number",
                                actual: frame.get(index_register)?.type_name(),
                            })?;
                    if instruction.opcode() == Opcode::ForNLoop {
                        index += step;
                        frame.set(index_register, Value::Number(index))?;
                    }
                    let continues = if step > 0.0 {
                        index <= limit
                    } else {
                        limit <= index
                    };
                    if continues == (instruction.opcode() == Opcode::ForNLoop) {
                        frame.jump(instruction)?;
                    }
                }
                Opcode::ForGPrep => {
                    let base = instruction.a();
                    if let Value::Table(table) = frame.get(base)?.clone() {
                        let next =
                            self.globals
                                .get(&b"next"[..])
                                .cloned()
                                .ok_or(RuntimeError::Type {
                                    operation: "iterate",
                                    expected: "function",
                                    actual: "nil",
                                })?;
                        let state_register = base.checked_add(1).ok_or(RuntimeError::Register {
                            register: usize::from(base) + 1,
                            count: frame.registers.len(),
                        })?;
                        let index_register = base.checked_add(2).ok_or(RuntimeError::Register {
                            register: usize::from(base) + 2,
                            count: frame.registers.len(),
                        })?;
                        frame.set(base, next)?;
                        frame.set(state_register, Value::Table(table))?;
                        frame.set(index_register, Value::Nil)?;
                    }
                    frame.jump(instruction)?;
                }
                Opcode::ForGPrepInext | Opcode::ForGPrepNext => {
                    frame.jump(instruction)?;
                }
                Opcode::ForGLoop => {
                    let base = instruction.a();
                    let function = frame.get(base)?.clone();
                    let state_register = base.checked_add(1).ok_or(RuntimeError::Register {
                        register: usize::from(base) + 1,
                        count: frame.registers.len(),
                    })?;
                    let index_register = base.checked_add(2).ok_or(RuntimeError::Register {
                        register: usize::from(base) + 2,
                        count: frame.registers.len(),
                    })?;
                    let state = frame.get(state_register)?.clone();
                    let control = frame.get(index_register)?.clone();
                    let mut arguments = try_vec_with_capacity(2, "generic-for arguments")?;
                    arguments.push(state.clone());
                    arguments.push(control.clone());
                    let variable_count = usize::try_from(
                        instruction.aux().ok_or(RuntimeError::MissingAux {
                            pc: instruction.pc(),
                            opcode: instruction.opcode(),
                        })? & 0xff,
                    )
                    .expect("u8 fits usize");
                    if let Value::Closure(closure) = function {
                        let (child_chunk, child, child_profile, _) =
                            self.heap.closure_parts(closure)?;
                        if depth + 1 > self.call_limit {
                            return Err(RuntimeError::CallLimit {
                                limit: self.call_limit,
                            });
                        }
                        let child_prototype = child_chunk
                            .prototypes
                            .get(child)
                            .ok_or(RuntimeError::InvalidPrototype(child))?;
                        let constants = materialize_constants(&child_chunk, child_prototype)?;
                        let child_frame = Frame::new(
                            child_chunk,
                            child,
                            child_profile,
                            constants,
                            Some(closure),
                            &arguments,
                        )?;
                        let caller = Caller {
                            frame,
                            register: base,
                            encoded_count: 0,
                            return_mode: ReturnMode::Operation(PendingOperation::GenericForStep {
                                function: Value::Closure(closure),
                                state,
                                control,
                                base,
                                variable_count,
                                instruction,
                            }),
                        };
                        let roots = caller.gc_roots(&self.heap)?;
                        try_reserve_exact(&mut callers, 1, "VM caller stack")?;
                        self.push_active_roots(roots)?;
                        callers.push(caller);
                        frame = child_frame;
                        continue;
                    }
                    let results = self.call_value(
                        function,
                        &arguments,
                        remaining,
                        depth,
                        frame.gc_roots(&self.heap)?,
                    )?;
                    frame.refresh_open_upvalues(&self.heap)?;
                    for offset in 0..variable_count {
                        let register = usize::from(base) + 3 + offset;
                        let register =
                            u8::try_from(register).map_err(|_| RuntimeError::Register {
                                register,
                                count: frame.registers.len(),
                            })?;
                        frame.set(register, results.get(offset).cloned().unwrap_or(Value::Nil))?;
                    }
                    let first = results.first().cloned().unwrap_or(Value::Nil);
                    frame.set(index_register, first.clone())?;
                    if !matches!(first, Value::Nil) {
                        frame.jump(instruction)?;
                    }
                }
                Opcode::Jump | Opcode::JumpBack | Opcode::JumpX => {
                    frame.jump(instruction)?;
                }
                Opcode::JumpIf => {
                    if frame.get(instruction.a())?.is_truthy() {
                        frame.jump(instruction)?;
                    }
                }
                Opcode::JumpIfNot => {
                    if !frame.get(instruction.a())?.is_truthy() {
                        frame.jump(instruction)?;
                    }
                }
                Opcode::JumpIfEq
                | Opcode::JumpIfLe
                | Opcode::JumpIfLt
                | Opcode::JumpIfNotEq
                | Opcode::JumpIfNotLe
                | Opcode::JumpIfNotLt => {
                    let right_register = instruction.aux().ok_or(RuntimeError::MissingAux {
                        pc: instruction.pc(),
                        opcode: instruction.opcode(),
                    })?;
                    let right_register =
                        u8::try_from(right_register).map_err(|_| RuntimeError::Register {
                            register: right_register as usize,
                            count: frame.registers.len(),
                        })?;
                    let left = frame.get(instruction.a())?.clone();
                    let right = frame.get(right_register)?.clone();
                    if self.compare_value(
                        instruction.opcode(),
                        left,
                        right,
                        CallContext::new(remaining, depth, frame.gc_roots(&self.heap)?),
                    )? {
                        frame.jump(instruction)?;
                    }
                    frame.refresh_open_upvalues(&self.heap)?;
                }
                Opcode::JumpXEqKNil | Opcode::JumpXEqKB | Opcode::JumpXEqKN | Opcode::JumpXEqKS => {
                    let left = frame.get(instruction.a())?;
                    let aux = instruction.aux().ok_or(RuntimeError::MissingAux {
                        pc: instruction.pc(),
                        opcode: instruction.opcode(),
                    })?;
                    let expected = match instruction.opcode() {
                        Opcode::JumpXEqKNil => Value::Nil,
                        Opcode::JumpXEqKB => Value::Boolean(aux & 1 != 0),
                        Opcode::JumpXEqKN | Opcode::JumpXEqKS => {
                            frame.constant_u32(aux & 0x00ff_ffff)?
                        }
                        _ => unreachable!(),
                    };
                    let equal = left == &expected;
                    let negate = aux >> 31 != 0;
                    if equal != negate {
                        frame.jump(instruction)?;
                    }
                }
                Opcode::Concat => {
                    let mut result = frame.get(instruction.c())?.clone();
                    for register in (instruction.b()..instruction.c()).rev() {
                        let left = frame.get(register)?.clone();
                        result = self.concat_value(
                            left,
                            result,
                            CallContext::new(remaining, depth, frame.gc_roots(&self.heap)?),
                        )?;
                        frame.refresh_open_upvalues(&self.heap)?;
                    }
                    frame.set(instruction.a(), result)?;
                }
                Opcode::Call | Opcode::CallFb => {
                    let function = frame.get(instruction.a())?.clone();
                    let start = usize::from(instruction.a()) + 1;
                    let count = if instruction.b() == 0 {
                        frame.top.saturating_sub(start)
                    } else {
                        usize::from(instruction.b() - 1)
                    };
                    let arguments =
                        try_clone_values(frame.register_slice(start, count)?, "call arguments")?;
                    let suspended_return_mode = match &function {
                        Value::NativeFunction(function)
                            if self.protected_call == Some(*function) =>
                        {
                            ReturnMode::Protected
                        }
                        Value::NativeFunction(function)
                            if self.error_handler_call == Some(*function) =>
                        {
                            ReturnMode::ErrorHandler(arguments.get(1).cloned().ok_or(
                                RuntimeError::Argument {
                                    function: "xpcall",
                                    index: 2,
                                },
                            )?)
                        }
                        _ => ReturnMode::Direct,
                    };
                    let results = if let Value::Closure(closure) = function {
                        let (child_chunk, child, child_profile, _) =
                            self.heap.closure_parts(closure)?;
                        if depth + 1 > self.call_limit {
                            return Err(RuntimeError::CallLimit {
                                limit: self.call_limit,
                            });
                        }
                        let child_prototype = child_chunk
                            .prototypes
                            .get(child)
                            .ok_or(RuntimeError::InvalidPrototype(child))?;
                        let constants = materialize_constants(&child_chunk, child_prototype)?;
                        let child_frame = Frame::new(
                            child_chunk,
                            child,
                            child_profile,
                            constants,
                            Some(closure),
                            &arguments,
                        )?;
                        let roots = frame.gc_roots(&self.heap)?;
                        try_reserve_exact(&mut callers, 1, "VM caller stack")?;
                        self.push_active_roots(roots)?;
                        callers.push(Caller {
                            frame,
                            register: instruction.a(),
                            encoded_count: instruction.c(),
                            return_mode: ReturnMode::Direct,
                        });
                        frame = child_frame;
                        continue;
                    } else {
                        match self.call_value(
                            function,
                            &arguments,
                            remaining,
                            depth,
                            frame.gc_roots(&self.heap)?,
                        ) {
                            Err(RuntimeError::CoroutineYield(values)) => {
                                let thread = self
                                    .running_thread
                                    .ok_or(RuntimeError::CoroutineYieldOutside)?;
                                self.suspend_thread(
                                    thread,
                                    frame,
                                    callers,
                                    instruction.a(),
                                    instruction.c(),
                                    suspended_return_mode,
                                    depth,
                                )?;
                                return Err(RuntimeError::CoroutineYield(values));
                            }
                            result => result?,
                        }
                    };
                    frame.refresh_open_upvalues(&self.heap)?;
                    frame.write_results(instruction.a(), instruction.c(), results)?;
                }
                Opcode::Return => {
                    let start = usize::from(instruction.a());
                    let count = if instruction.b() == 0 {
                        frame.top.saturating_sub(start)
                    } else {
                        usize::from(instruction.b() - 1)
                    };
                    let end = start.checked_add(count).ok_or(RuntimeError::Register {
                        register: usize::MAX,
                        count: frame.registers.len(),
                    })?;
                    if end > frame.registers.len() {
                        return Err(RuntimeError::Register {
                            register: end,
                            count: frame.registers.len(),
                        });
                    }
                    let results = try_clone_values(&frame.registers[start..end], "call results")?;
                    if let Some(caller) = callers.pop() {
                        self.active_roots.pop();
                        frame = caller.complete_success(&self.heap, results)?;
                        continue;
                    }
                    return Ok(results);
                }
                opcode => {
                    return Err(RuntimeError::UnsupportedOpcode {
                        pc: instruction.pc(),
                        opcode,
                    });
                }
            }
        }
    }

    fn call_value(
        &mut self,
        function: Value,
        arguments: &[Value],
        remaining: &mut u64,
        depth: usize,
        roots: GcRoots,
    ) -> Result<Vec<Value>, RuntimeError> {
        match function {
            Value::Closure(closure) => {
                let (closure_chunk, child, profile, _) = self.heap.closure_parts(closure)?;
                self.push_active_roots(roots)?;
                let result = self.execute_frame(
                    &closure_chunk,
                    FrameTarget {
                        prototype_index: child,
                        profile,
                    },
                    Some(closure),
                    arguments,
                    remaining,
                    depth + 1,
                );
                self.active_roots.pop();
                result
            }
            Value::CoroutineFunction(thread) => {
                let resume_arguments = try_prefixed_values(
                    Value::Thread(thread),
                    arguments,
                    "coroutine resume arguments",
                )?;
                let mut results = self.resume_thread(&resume_arguments, remaining, depth, roots)?;
                if results.first().is_some_and(Value::is_truthy) {
                    results.remove(0);
                    Ok(results)
                } else {
                    Err(RuntimeError::Raised(
                        results
                            .get(1)
                            .cloned()
                            .unwrap_or_else(|| Value::String(Arc::from(&b"coroutine failed"[..]))),
                    ))
                }
            }
            Value::NativeFunction(function) => {
                if self.coroutine_resume == Some(function) {
                    return self.resume_thread(arguments, remaining, depth, roots);
                }
                if self.coroutine_yield == Some(function) {
                    return Err(RuntimeError::CoroutineYield(try_clone_values(
                        arguments,
                        "coroutine yield values",
                    )?));
                }
                if self.protected_call == Some(function) {
                    let target = arguments.first().cloned().ok_or(RuntimeError::Argument {
                        function: "pcall",
                        index: 1,
                    })?;
                    let result = self.call_value(
                        target,
                        arguments.get(1..).unwrap_or_default(),
                        remaining,
                        depth,
                        roots,
                    );
                    return Ok(match result {
                        Ok(values) => try_prepend_value(
                            values,
                            Value::Boolean(true),
                            "protected call results",
                        )?,
                        Err(error @ RuntimeError::CoroutineYield(_)) => return Err(error),
                        Err(RuntimeError::Raised(value)) => {
                            vec![Value::Boolean(false), value]
                        }
                        Err(error) => vec![
                            Value::Boolean(false),
                            Value::String(Arc::from(error.to_string().into_bytes())),
                        ],
                    });
                }
                if self.error_handler_call == Some(function) {
                    let target = arguments.first().cloned().ok_or(RuntimeError::Argument {
                        function: "xpcall",
                        index: 1,
                    })?;
                    let handler = arguments.get(1).cloned().ok_or(RuntimeError::Argument {
                        function: "xpcall",
                        index: 2,
                    })?;
                    let result = self.call_value(
                        target,
                        arguments.get(2..).unwrap_or_default(),
                        remaining,
                        depth,
                        roots.try_clone()?,
                    );
                    return Ok(match result {
                        Ok(values) => try_prepend_value(
                            values,
                            Value::Boolean(true),
                            "protected call results",
                        )?,
                        Err(error @ RuntimeError::CoroutineYield(_)) => return Err(error),
                        Err(error) => {
                            let handled = self.call_value(
                                handler,
                                &[runtime_error_value(error)],
                                remaining,
                                depth,
                                roots,
                            );
                            match handled {
                                Ok(values) => vec![
                                    Value::Boolean(false),
                                    values.into_iter().next().unwrap_or(Value::Nil),
                                ],
                                Err(error) => {
                                    vec![Value::Boolean(false), runtime_error_value(error)]
                                }
                            }
                        }
                    });
                }
                let function = self
                    .native_functions
                    .get(function.0 as usize)
                    .cloned()
                    .ok_or(RuntimeError::NativeFunction(function.0))?;
                self.push_active_roots(roots)?;
                let result = function(self, arguments);
                self.active_roots.pop();
                let values = result?;
                if values.len() > self.native_result_limit {
                    return Err(RuntimeError::NativeResultLimit {
                        required: values.len(),
                        limit: self.native_result_limit,
                    });
                }
                Ok(values)
            }
            Value::Table(table) => {
                let function =
                    self.metamethod(&Value::Table(table), "__call")?
                        .ok_or(RuntimeError::Type {
                            operation: "call",
                            expected: "function or __call metamethod",
                            actual: "table",
                        })?;
                let metamethod_arguments =
                    try_prefixed_values(Value::Table(table), arguments, "metamethod arguments")?;
                self.call_value(function, &metamethod_arguments, remaining, depth, roots)
            }
            other => Err(RuntimeError::Type {
                operation: "call",
                expected: "function",
                actual: other.type_name(),
            }),
        }
    }

    fn index_value(
        &mut self,
        value: Value,
        key: Value,
        operation: &'static str,
        context: CallContext<'_>,
    ) -> Result<Value, RuntimeError> {
        let mut table = match &value {
            Value::Table(table) => *table,
            Value::String(_) => {
                table_id(self.globals.get(&b"string"[..]).ok_or(RuntimeError::Type {
                    operation,
                    expected: "table",
                    actual: value.type_name(),
                })?)?
            }
            other => {
                return Err(RuntimeError::Type {
                    operation,
                    expected: "table",
                    actual: other.type_name(),
                });
            }
        };
        for _ in 0..100 {
            let result = self.heap.table_get(table, &key)?;
            if !matches!(result, Value::Nil) {
                return Ok(result);
            }
            let Some(metatable) = self.heap.table_metatable(table)? else {
                return Ok(Value::Nil);
            };
            let index = self
                .heap
                .table_get(metatable, &Value::String(Arc::from(&b"__index"[..])))?;
            match index {
                Value::Nil => return Ok(Value::Nil),
                Value::Table(next) => table = next,
                function @ (Value::Closure(_)
                | Value::CoroutineFunction(_)
                | Value::NativeFunction(_)) => {
                    return Ok(self
                        .call_value(
                            function,
                            &[Value::Table(table), key],
                            context.remaining,
                            context.depth,
                            context.roots,
                        )?
                        .into_iter()
                        .next()
                        .unwrap_or(Value::Nil));
                }
                other => {
                    return Err(RuntimeError::UnsupportedMetamethod {
                        name: "__index",
                        actual: other.type_name(),
                    });
                }
            }
        }
        Err(RuntimeError::MetatableLoop)
    }

    fn set_index(
        &mut self,
        value: Value,
        key: Value,
        assigned: Value,
        context: CallContext<'_>,
    ) -> Result<(), RuntimeError> {
        let mut table = table_id(&value)?;
        for _ in 0..100 {
            let existing = self.heap.table_get(table, &key)?;
            if !matches!(existing, Value::Nil) {
                self.table_set(table, key, assigned, &context.roots)?;
                return Ok(());
            }
            let Some(metatable) = self.heap.table_metatable(table)? else {
                self.table_set(table, key, assigned, &context.roots)?;
                return Ok(());
            };
            let newindex = self
                .heap
                .table_get(metatable, &Value::String(Arc::from(&b"__newindex"[..])))?;
            match newindex {
                Value::Nil => {
                    self.table_set(table, key, assigned, &context.roots)?;
                    return Ok(());
                }
                Value::Table(next) => table = next,
                function @ (Value::Closure(_)
                | Value::CoroutineFunction(_)
                | Value::NativeFunction(_)) => {
                    self.call_value(
                        function,
                        &[Value::Table(table), key, assigned],
                        context.remaining,
                        context.depth,
                        context.roots,
                    )?;
                    return Ok(());
                }
                other => {
                    return Err(RuntimeError::UnsupportedMetamethod {
                        name: "__newindex",
                        actual: other.type_name(),
                    });
                }
            }
        }
        Err(RuntimeError::MetatableLoop)
    }

    fn arithmetic_value(
        &mut self,
        opcode: Opcode,
        left: Value,
        right: Value,
        context: CallContext<'_>,
    ) -> Result<Value, RuntimeError> {
        let profile = self.active_profile()?;
        if let (Some(left), Some(right)) = (
            arithmetic_numeric_value(&left, profile),
            arithmetic_numeric_value(&right, profile),
        ) {
            return arithmetic(opcode, &left, &right);
        }
        let name = match opcode {
            Opcode::Add | Opcode::AddK => "__add",
            Opcode::Sub | Opcode::SubK | Opcode::SubRk => "__sub",
            Opcode::Mul | Opcode::MulK => "__mul",
            Opcode::Div | Opcode::DivK | Opcode::DivRk => "__div",
            Opcode::Mod | Opcode::ModK => "__mod",
            Opcode::Pow | Opcode::PowK => "__pow",
            Opcode::IDiv | Opcode::IDivK => "__idiv",
            _ => return Err(RuntimeError::UnsupportedArithmetic(opcode)),
        };
        let function = self
            .metamethod(&left, name)?
            .or(self.metamethod(&right, name)?)
            .ok_or(RuntimeError::Type {
                operation: "arithmetic",
                expected: "number or arithmetic metamethod",
                actual: left.type_name(),
            })?;
        Ok(self
            .call_value(
                function,
                &[left, right],
                context.remaining,
                context.depth,
                context.roots,
            )?
            .into_iter()
            .next()
            .unwrap_or(Value::Nil))
    }

    fn concat_value(
        &mut self,
        left: Value,
        right: Value,
        context: CallContext<'_>,
    ) -> Result<Value, RuntimeError> {
        if let (Some(left), Some(right)) = (try_concat_bytes(&left)?, try_concat_bytes(&right)?) {
            let required =
                left.len()
                    .checked_add(right.len())
                    .ok_or(RuntimeError::StringLimit {
                        required: usize::MAX,
                        limit: MAX_STRING_BYTES,
                    })?;
            if required > MAX_STRING_BYTES {
                return Err(RuntimeError::StringLimit {
                    required,
                    limit: MAX_STRING_BYTES,
                });
            }
            let mut result = try_vec_with_capacity(required, "concatenated string")?;
            result.extend_from_slice(&left);
            result.extend_from_slice(&right);
            return Ok(Value::String(Arc::from(result)));
        }
        let function = self
            .metamethod(&left, "__concat")?
            .or(self.metamethod(&right, "__concat")?)
            .ok_or(RuntimeError::Type {
                operation: "concatenation",
                expected: "string, number, or __concat metamethod",
                actual: left.type_name(),
            })?;
        Ok(self
            .call_value(
                function,
                &[left, right],
                context.remaining,
                context.depth,
                context.roots,
            )?
            .into_iter()
            .next()
            .unwrap_or(Value::Nil))
    }

    fn metamethod(&self, value: &Value, name: &'static str) -> Result<Option<Value>, RuntimeError> {
        let Value::Table(table) = value else {
            return Ok(None);
        };
        let Some(metatable) = self.heap.table_metatable(*table)? else {
            return Ok(None);
        };
        let value = self
            .heap
            .table_get(metatable, &Value::String(Arc::from(name.as_bytes())))?;
        if matches!(value, Value::Nil) {
            Ok(None)
        } else {
            Ok(Some(value))
        }
    }

    fn resolve_blu_callable(
        &self,
        mut function: Value,
        mut arguments: Vec<Value>,
        profile: SemanticProfile,
    ) -> Result<(Value, Vec<Value>), RuntimeError> {
        for _ in 0..metatable_loop_limit(profile) {
            let Value::Table(table) = function else {
                return Ok((function, arguments));
            };
            let Some(metatable) = self.heap.table_metatable(table)? else {
                return Err(RuntimeError::Type {
                    operation: "call",
                    expected: "function or __call metamethod",
                    actual: "table",
                });
            };
            function = self
                .heap
                .table_get(metatable, &Value::String(Arc::from(&b"__call"[..])))?;
            if matches!(function, Value::Nil) {
                return Err(RuntimeError::Type {
                    operation: "call",
                    expected: "function or __call metamethod",
                    actual: "table",
                });
            }
            arguments =
                try_prefixed_values(Value::Table(table), &arguments, "BluV1 __call arguments")?;
        }
        Err(RuntimeError::MetatableLoop)
    }

    fn resolve_blu_index(
        &self,
        mut table: TableId,
        key: &Value,
        profile: SemanticProfile,
    ) -> Result<BluIndexResolution, RuntimeError> {
        for _ in 0..metatable_loop_limit(profile) {
            let value = self.heap.table_get(table, key)?;
            if !matches!(value, Value::Nil) {
                return Ok(BluIndexResolution::Value(value));
            }
            let Some(metatable) = self.heap.table_metatable(table)? else {
                return Ok(BluIndexResolution::Value(Value::Nil));
            };
            let handler = self
                .heap
                .table_get(metatable, &Value::String(Arc::from(&b"__index"[..])))?;
            match handler {
                Value::Nil => return Ok(BluIndexResolution::Value(Value::Nil)),
                Value::Table(next) => table = next,
                function @ (Value::Closure(_)
                | Value::CoroutineFunction(_)
                | Value::NativeFunction(_)) => {
                    return Ok(BluIndexResolution::Call {
                        function,
                        receiver: Value::Table(table),
                    });
                }
                other => {
                    return Err(RuntimeError::UnsupportedMetamethod {
                        name: "__index",
                        actual: other.type_name(),
                    });
                }
            }
        }
        Err(RuntimeError::MetatableLoop)
    }

    fn resolve_blu_new_index(
        &self,
        mut table: TableId,
        key: &Value,
        profile: SemanticProfile,
    ) -> Result<BluNewIndexResolution, RuntimeError> {
        for _ in 0..metatable_loop_limit(profile) {
            if !matches!(self.heap.table_get(table, key)?, Value::Nil) {
                return Ok(BluNewIndexResolution::Raw(table));
            }
            let Some(metatable) = self.heap.table_metatable(table)? else {
                return Ok(BluNewIndexResolution::Raw(table));
            };
            let handler = self
                .heap
                .table_get(metatable, &Value::String(Arc::from(&b"__newindex"[..])))?;
            match handler {
                Value::Nil => return Ok(BluNewIndexResolution::Raw(table)),
                Value::Table(next) => table = next,
                function @ (Value::Closure(_)
                | Value::CoroutineFunction(_)
                | Value::NativeFunction(_)) => {
                    return Ok(BluNewIndexResolution::Call {
                        function,
                        receiver: Value::Table(table),
                    });
                }
                other => {
                    return Err(RuntimeError::UnsupportedMetamethod {
                        name: "__newindex",
                        actual: other.type_name(),
                    });
                }
            }
        }
        Err(RuntimeError::MetatableLoop)
    }

    fn compare_value(
        &mut self,
        opcode: Opcode,
        left: Value,
        right: Value,
        context: CallContext<'_>,
    ) -> Result<bool, RuntimeError> {
        let negate = matches!(
            opcode,
            Opcode::JumpIfNotEq | Opcode::JumpIfNotLe | Opcode::JumpIfNotLt
        );
        let base = match opcode {
            Opcode::JumpIfEq | Opcode::JumpIfNotEq => {
                if left == right {
                    true
                } else if matches!((&left, &right), (Value::Table(_), Value::Table(_))) {
                    match self.shared_metamethod(&left, &right, "__eq")? {
                        Some(function) => self
                            .call_value(
                                function,
                                &[left, right],
                                context.remaining,
                                context.depth,
                                context.roots,
                            )?
                            .first()
                            .is_some_and(Value::is_truthy),
                        None => false,
                    }
                } else {
                    false
                }
            }
            Opcode::JumpIfLt | Opcode::JumpIfNotLt => {
                if let Some(value) = left.numeric_less(&right) {
                    value
                } else if let (Value::String(left), Value::String(right)) = (&left, &right) {
                    left < right
                } else {
                    let function = self.shared_metamethod(&left, &right, "__lt")?.ok_or(
                        RuntimeError::Type {
                            operation: "comparison",
                            expected: "matching values or __lt metamethods",
                            actual: left.type_name(),
                        },
                    )?;
                    self.call_value(
                        function,
                        &[left, right],
                        context.remaining,
                        context.depth,
                        context.roots,
                    )?
                    .first()
                    .is_some_and(Value::is_truthy)
                }
            }
            Opcode::JumpIfLe | Opcode::JumpIfNotLe => {
                if let Some(value) = left.numeric_less_equal(&right) {
                    value
                } else if let (Value::String(left), Value::String(right)) = (&left, &right) {
                    left <= right
                } else if let Some(function) = self.shared_metamethod(&left, &right, "__le")? {
                    self.call_value(
                        function,
                        &[left, right],
                        context.remaining,
                        context.depth,
                        context.roots,
                    )?
                    .first()
                    .is_some_and(Value::is_truthy)
                } else {
                    let function = self.shared_metamethod(&right, &left, "__lt")?.ok_or(
                        RuntimeError::Type {
                            operation: "comparison",
                            expected: "matching values or __le/__lt metamethods",
                            actual: left.type_name(),
                        },
                    )?;
                    !self
                        .call_value(
                            function,
                            &[right, left],
                            context.remaining,
                            context.depth,
                            context.roots,
                        )?
                        .first()
                        .is_some_and(Value::is_truthy)
                }
            }
            _ => return Err(RuntimeError::UnsupportedComparison(opcode)),
        };
        Ok(base != negate)
    }

    fn shared_metamethod(
        &self,
        left: &Value,
        right: &Value,
        name: &'static str,
    ) -> Result<Option<Value>, RuntimeError> {
        let Some(left) = self.metamethod(left, name)? else {
            return Ok(None);
        };
        let Some(right) = self.metamethod(right, name)? else {
            return Ok(None);
        };
        Ok((left == right).then_some(left))
    }

    fn install_base_library(&mut self) -> Result<(), RuntimeError> {
        let require = self.register_function(|vm, arguments| {
            let name = arguments.first().ok_or(RuntimeError::Argument {
                function: "require",
                index: 1,
            })?;
            let name = Arc::<[u8]>::from(string_bytes(name, "require")?);
            if let Some(value) = vm.module_cache.get(&name) {
                return Ok(vec![value.clone()]);
            }
            if vm.loading_modules.contains(&name) {
                return Err(RuntimeError::CircularModule(name));
            }
            vm.loading_modules
                .try_reserve(1)
                .map_err(|_| RuntimeError::Allocation {
                    what: "loading module set",
                })?;
            vm.loading_modules.insert(name.clone());
            let result = match vm.module_loader.clone() {
                Some(loader) => loader(vm, &name),
                None => Err(RuntimeError::ModuleLoaderMissing),
            };
            vm.loading_modules.remove(&name);
            let value = result?;
            vm.module_cache
                .try_reserve(1)
                .map_err(|_| RuntimeError::Allocation {
                    what: "module cache",
                })?;
            vm.module_cache.insert(name, value.clone());
            Ok(vec![value])
        });
        self.set_global(&b"require"[..], Value::NativeFunction(require));

        let next = self.register_function(|vm, arguments| {
            let table = arguments.first().ok_or(RuntimeError::Argument {
                function: "next",
                index: 1,
            })?;
            let table = table_id(table)?;
            let key = arguments.get(1).unwrap_or(&Value::Nil);
            let Some((key, value)) = vm.heap.table_next(table, key)? else {
                return Ok(Vec::new());
            };
            let key = match key {
                Value::Integer(value) => profiled_integral_math_result(vm, "next", value as f64)?,
                key => key,
            };
            Ok(vec![key, value])
        });
        self.set_global(&b"next"[..], Value::NativeFunction(next));

        let pairs = self.register_function(move |_, arguments| {
            let table = arguments.first().ok_or(RuntimeError::Argument {
                function: "pairs",
                index: 1,
            })?;
            table_id(table)?;
            Ok(vec![Value::NativeFunction(next), table.clone(), Value::Nil])
        });
        self.set_global(&b"pairs"[..], Value::NativeFunction(pairs));

        let inext = self.register_function(|vm, arguments| {
            let table = arguments.first().ok_or(RuntimeError::Argument {
                function: "ipairs",
                index: 1,
            })?;
            let table = table_id(table)?;
            let index = arguments.get(1).and_then(Value::as_number).unwrap_or(0.0) as i64 + 1;
            let value = vm.heap.table_get(table, &Value::Integer(index))?;
            if matches!(value, Value::Nil) {
                Ok(Vec::new())
            } else {
                Ok(vec![
                    profiled_integral_math_result(vm, "ipairs", index as f64)?,
                    value,
                ])
            }
        });
        let ipairs = self.register_function(move |vm, arguments| {
            let table = arguments.first().ok_or(RuntimeError::Argument {
                function: "ipairs",
                index: 1,
            })?;
            table_id(table)?;
            Ok(vec![
                Value::NativeFunction(inext),
                table.clone(),
                profiled_integral_math_result(vm, "ipairs", 0.0)?,
            ])
        });
        self.set_global(&b"ipairs"[..], Value::NativeFunction(ipairs));

        let type_function = self.register_function(|_, arguments| {
            let value = arguments.first().ok_or(RuntimeError::Argument {
                function: "type",
                index: 1,
            })?;
            Ok(vec![Value::String(Arc::from(value.type_name().as_bytes()))])
        });
        self.set_global(&b"type"[..], Value::NativeFunction(type_function));
        self.set_global(&b"typeof"[..], Value::NativeFunction(type_function));

        let tonumber = self.register_function(|vm, arguments| {
            let value = arguments.first().ok_or(RuntimeError::Argument {
                function: "tonumber",
                index: 1,
            })?;
            let explicit_base = arguments
                .get(1)
                .map(|value| {
                    value.as_number().ok_or(RuntimeError::Type {
                        operation: "tonumber",
                        expected: "number",
                        actual: value.type_name(),
                    })
                })
                .transpose()?
                .map(|base| base as u32);
            let base = explicit_base.unwrap_or(10);
            if !(2..=36).contains(&base) {
                return Err(RuntimeError::ConversionBase(base));
            }
            let profile = vm.active_profile()?;
            let modern = matches!(
                profile,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            );
            if explicit_base.is_none() && value.as_number().is_some() {
                return Ok(vec![if modern {
                    value.clone()
                } else {
                    Value::Number(
                        value
                            .as_number()
                            .expect("numeric tonumber input was checked above"),
                    )
                }]);
            }
            let Value::String(value) = value else {
                return if explicit_base.is_none() {
                    Ok(vec![Value::Nil])
                } else {
                    Err(RuntimeError::Type {
                        operation: "tonumber",
                        expected: "string",
                        actual: value.type_name(),
                    })
                };
            };
            let value = trim_ascii_bytes(value);
            let parsed = if explicit_base.is_some() {
                parse_based_number(value, base, profile)
            } else {
                parse_default_number(value, profile)
            };
            Ok(vec![parsed.unwrap_or(Value::Nil)])
        });
        self.set_global(&b"tonumber"[..], Value::NativeFunction(tonumber));

        let tostring = self.register_function(|_, arguments| {
            let value = arguments.first().ok_or(RuntimeError::Argument {
                function: "tostring",
                index: 1,
            })?;
            let mut result = try_vec_with_capacity(rendered_value_len(value), "tostring result")?;
            append_value(&mut result, value);
            Ok(vec![Value::String(Arc::from(result))])
        });
        self.set_global(&b"tostring"[..], Value::NativeFunction(tostring));

        let getmetatable = self.register_function(|vm, arguments| {
            let value = arguments.first().ok_or(RuntimeError::Argument {
                function: "getmetatable",
                index: 1,
            })?;
            let table = table_id(value)?;
            let Some(metatable) = vm.heap.table_metatable(table)? else {
                return Ok(vec![Value::Nil]);
            };
            let protected = vm
                .heap
                .table_get(metatable, &Value::String(Arc::from(&b"__metatable"[..])))?;
            Ok(vec![if matches!(protected, Value::Nil) {
                Value::Table(metatable)
            } else {
                protected
            }])
        });
        self.set_global(&b"getmetatable"[..], Value::NativeFunction(getmetatable));

        let setmetatable = self.register_function(|vm, arguments| {
            let value = arguments.first().ok_or(RuntimeError::Argument {
                function: "setmetatable",
                index: 1,
            })?;
            let table = table_id(value)?;
            if let Some(current) = vm.heap.table_metatable(table)? {
                let protected = vm
                    .heap
                    .table_get(current, &Value::String(Arc::from(&b"__metatable"[..])))?;
                if !matches!(protected, Value::Nil) {
                    return Err(RuntimeError::MetatableProtected);
                }
            }
            let metatable = match arguments.get(1) {
                Some(Value::Table(metatable)) => Some(*metatable),
                Some(Value::Nil) => None,
                Some(other) => {
                    return Err(RuntimeError::Type {
                        operation: "setmetatable",
                        expected: "table or nil",
                        actual: other.type_name(),
                    });
                }
                None => {
                    return Err(RuntimeError::Argument {
                        function: "setmetatable",
                        index: 2,
                    });
                }
            };
            vm.heap.set_table_metatable(table, metatable)?;
            Ok(vec![value.clone()])
        });
        self.set_global(&b"setmetatable"[..], Value::NativeFunction(setmetatable));

        let rawget = self.register_function(|vm, arguments| {
            let table = arguments.first().ok_or(RuntimeError::Argument {
                function: "rawget",
                index: 1,
            })?;
            let table = table_id(table)?;
            let key = arguments.get(1).ok_or(RuntimeError::Argument {
                function: "rawget",
                index: 2,
            })?;
            Ok(vec![vm.heap.table_get(table, key)?])
        });
        self.set_global(&b"rawget"[..], Value::NativeFunction(rawget));

        let rawset = self.register_function(|vm, arguments| {
            let value = arguments.first().ok_or(RuntimeError::Argument {
                function: "rawset",
                index: 1,
            })?;
            let table = table_id(value)?;
            let key = arguments.get(1).ok_or(RuntimeError::Argument {
                function: "rawset",
                index: 2,
            })?;
            let assigned = arguments.get(2).ok_or(RuntimeError::Argument {
                function: "rawset",
                index: 3,
            })?;
            let roots = GcRoots::from_values(arguments)?;
            vm.table_set(table, key.clone(), assigned.clone(), &roots)?;
            Ok(vec![value.clone()])
        });
        self.set_global(&b"rawset"[..], Value::NativeFunction(rawset));

        let rawequal = self.register_function(|_, arguments| {
            let left = arguments.first().ok_or(RuntimeError::Argument {
                function: "rawequal",
                index: 1,
            })?;
            let right = arguments.get(1).ok_or(RuntimeError::Argument {
                function: "rawequal",
                index: 2,
            })?;
            Ok(vec![Value::Boolean(left == right)])
        });
        self.set_global(&b"rawequal"[..], Value::NativeFunction(rawequal));

        let rawlen = self.register_function(|vm, arguments| {
            let value = arguments.first().ok_or(RuntimeError::Argument {
                function: "rawlen",
                index: 1,
            })?;
            let length = match value {
                Value::String(value) => value.len(),
                Value::Table(table) => vm.heap.table_length(*table)?,
                other => {
                    return Err(RuntimeError::Type {
                        operation: "rawlen",
                        expected: "string or table",
                        actual: other.type_name(),
                    });
                }
            };
            Ok(vec![profiled_integral_math_result(
                vm,
                "rawlen",
                length as f64,
            )?])
        });
        self.set_global(&b"rawlen"[..], Value::NativeFunction(rawlen));

        let error = self.register_function(|_, arguments| {
            Err(RuntimeError::Raised(
                arguments.first().cloned().unwrap_or(Value::Nil),
            ))
        });
        self.set_global(&b"error"[..], Value::NativeFunction(error));

        let assert = self.register_function(|_, arguments| {
            let value = arguments.first().ok_or(RuntimeError::Argument {
                function: "assert",
                index: 1,
            })?;
            if value.is_truthy() {
                try_clone_values(arguments, "assert results")
            } else {
                Err(RuntimeError::Raised(arguments.get(1).cloned().unwrap_or(
                    Value::String(Arc::from(&b"assertion failed!"[..])),
                )))
            }
        });
        self.set_global(&b"assert"[..], Value::NativeFunction(assert));

        let select = self.register_function(|vm, arguments| {
            let selector = arguments.first().ok_or(RuntimeError::Argument {
                function: "select",
                index: 1,
            })?;
            if matches!(selector, Value::String(value) if &**value == b"#") {
                return Ok(vec![profiled_integral_math_result(
                    vm,
                    "select",
                    arguments.len().saturating_sub(1) as f64,
                )?]);
            }
            let index = selector.as_number().ok_or(RuntimeError::Type {
                operation: "select",
                expected: "number or '#'",
                actual: selector.type_name(),
            })? as i64;
            if index == 0 {
                return Err(RuntimeError::SelectIndex(index));
            }
            let count = arguments.len().saturating_sub(1) as i64;
            let index = if index < 0 { count + index + 1 } else { index };
            if index < 1 {
                return Err(RuntimeError::SelectIndex(index));
            }
            try_clone_values(
                arguments.get(index as usize..).unwrap_or_default(),
                "select results",
            )
        });
        self.set_global(&b"select"[..], Value::NativeFunction(select));

        let collectgarbage = self.register_function(|vm, arguments| {
            let command = match arguments.first() {
                None => &b"collect"[..],
                Some(value) => string_bytes(value, "collectgarbage")?,
            };
            match command {
                b"collect" => {
                    vm.collect(std::iter::empty())?;
                    if vm.active_profile()? == SemanticProfile::Luau {
                        Ok(Vec::new())
                    } else {
                        Ok(vec![profiled_integral_math_result(
                            vm,
                            "collectgarbage",
                            0.0,
                        )?])
                    }
                }
                b"count" => Ok(vec![Value::Number(
                    vm.memory_usage().current_bytes as f64 / 1024.0,
                )]),
                _ => Err(RuntimeError::UnsupportedLibraryFeature {
                    function: "collectgarbage",
                    feature: "this profile-specific command",
                }),
            }
        });
        self.set_global(
            &b"collectgarbage"[..],
            Value::NativeFunction(collectgarbage),
        );

        let pcall = self.register_function(|_, _| Err(RuntimeError::NativeFunction(u32::MAX)));
        self.protected_call = Some(pcall);
        self.set_global(&b"pcall"[..], Value::NativeFunction(pcall));

        let xpcall = self.register_function(|_, _| Err(RuntimeError::NativeFunction(u32::MAX)));
        self.error_handler_call = Some(xpcall);
        self.set_global(&b"xpcall"[..], Value::NativeFunction(xpcall));

        let print = self.register_function(|vm, arguments| {
            let mut added = 1usize;
            for (index, argument) in arguments.iter().enumerate() {
                let Some(length) = added.checked_add(usize::from(index != 0)) else {
                    added = usize::MAX;
                    break;
                };
                let Some(length) = length.checked_add(rendered_value_len(argument)) else {
                    added = usize::MAX;
                    break;
                };
                added = length;
            }
            let required = vm.output.len().saturating_add(added);
            if required > vm.output_limit {
                return Err(RuntimeError::OutputLimit {
                    required,
                    limit: vm.output_limit,
                });
            }
            vm.output
                .try_reserve(added)
                .map_err(|_| RuntimeError::Allocation { what: "VM output" })?;
            for (index, value) in arguments.iter().enumerate() {
                if index != 0 {
                    vm.output.push(b'\t');
                }
                append_value(&mut vm.output, value);
            }
            vm.output.push(b'\n');
            Ok(Vec::new())
        });
        self.set_global(&b"print"[..], Value::NativeFunction(print));

        let string_sub = self.register_function(|_, arguments| {
            let string = arguments.first().ok_or(RuntimeError::Argument {
                function: "string.sub",
                index: 1,
            })?;
            let string = string_bytes(string, "string.sub")?;
            let start = integer_argument(arguments, 1, "string.sub")?;
            let end = match arguments.get(2) {
                Some(value) => value.as_number().ok_or(RuntimeError::Type {
                    operation: "string.sub",
                    expected: "number",
                    actual: value.type_name(),
                })? as i64,
                None => string.len() as i64,
            };
            let start = relative_index(start, string.len()).clamp(1, string.len() as i64 + 1);
            let end = relative_index(end, string.len()).clamp(0, string.len() as i64);
            let result = if start > end {
                &[][..]
            } else {
                &string[(start - 1) as usize..end as usize]
            };
            Ok(vec![Value::String(Arc::from(result))])
        });
        let string_find = self.register_function(|vm, arguments| {
            let haystack = string_bytes(
                arguments.first().ok_or(RuntimeError::Argument {
                    function: "string.find",
                    index: 1,
                })?,
                "string.find",
            )?;
            let needle = string_bytes(
                arguments.get(1).ok_or(RuntimeError::Argument {
                    function: "string.find",
                    index: 2,
                })?,
                "string.find",
            )?;
            let initial = arguments
                .get(2)
                .map(|_| integer_argument(arguments, 2, "string.find"))
                .transpose()?
                .unwrap_or(1);
            let initial = relative_index(initial, haystack.len()).max(1);
            if initial > haystack.len() as i64 + 1 {
                return Ok(vec![Value::Nil]);
            }
            let plain = arguments.get(3).is_some_and(Value::is_truthy);
            let start = initial as usize - 1;
            let found = if plain {
                let offset = if needle.is_empty() {
                    Some(0)
                } else {
                    haystack[start..]
                        .windows(needle.len())
                        .position(|window| window == needle)
                };
                offset.map(|offset| BasicPatternMatch {
                    start: start + offset,
                    end: start + offset + needle.len(),
                    captures: [BasicPatternCapture::default(); 32],
                    capture_count: 0,
                })
            } else {
                find_basic_lua_pattern(
                    haystack,
                    needle,
                    start,
                    "string.find",
                    vm.active_profile()?,
                )?
            };
            let Some(found) = found else {
                return Ok(vec![Value::Nil]);
            };
            let mut values = try_vec_with_capacity(2 + found.capture_count, "string.find results")?;
            values.push(profiled_integral_math_result(
                vm,
                "string.find",
                (found.start + 1) as f64,
            )?);
            values.push(profiled_integral_math_result(
                vm,
                "string.find",
                found.end as f64,
            )?);
            append_basic_capture_values(&mut values, vm, haystack, &found, "string.find")?;
            Ok(values)
        });
        let string_match = self.register_function(|vm, arguments| {
            let haystack = string_bytes(
                arguments.first().ok_or(RuntimeError::Argument {
                    function: "string.match",
                    index: 1,
                })?,
                "string.match",
            )?;
            let pattern = string_bytes(
                arguments.get(1).ok_or(RuntimeError::Argument {
                    function: "string.match",
                    index: 2,
                })?,
                "string.match",
            )?;
            let initial = arguments
                .get(2)
                .map(|_| integer_argument(arguments, 2, "string.match"))
                .transpose()?
                .unwrap_or(1);
            let initial = relative_index(initial, haystack.len()).max(1);
            if initial > haystack.len() as i64 + 1 {
                return Ok(vec![Value::Nil]);
            }
            let Some(found) = find_basic_lua_pattern(
                haystack,
                pattern,
                initial as usize - 1,
                "string.match",
                vm.active_profile()?,
            )?
            else {
                return Ok(vec![Value::Nil]);
            };
            if found.capture_count == 0 {
                Ok(vec![Value::String(Arc::from(
                    &haystack[found.start..found.end],
                ))])
            } else {
                let mut values =
                    try_vec_with_capacity(found.capture_count, "string.match captures")?;
                append_basic_capture_values(&mut values, vm, haystack, &found, "string.match")?;
                Ok(values)
            }
        });
        let string_gsub = self.register_function(|vm, arguments| {
            let haystack = string_bytes(
                arguments.first().ok_or(RuntimeError::Argument {
                    function: "string.gsub",
                    index: 1,
                })?,
                "string.gsub",
            )?;
            let pattern = string_bytes(
                arguments.get(1).ok_or(RuntimeError::Argument {
                    function: "string.gsub",
                    index: 2,
                })?,
                "string.gsub",
            )?;
            let replacement_value = arguments.get(2).ok_or(RuntimeError::Argument {
                function: "string.gsub",
                index: 3,
            })?;
            let replacement = try_concat_bytes(replacement_value)?;
            let replacement_table = match (replacement.as_ref(), replacement_value) {
                (Some(_), _) => None,
                (None, Value::Table(table)) => Some(*table),
                (None, Value::Closure(_) | Value::NativeFunction(_)) => {
                    return Err(RuntimeError::UnsupportedLibraryFeature {
                        function: "string.gsub",
                        feature: "callback replacements",
                    });
                }
                (None, other) => {
                    return Err(RuntimeError::Type {
                        operation: "string.gsub",
                        expected: "string, number, table, or function replacement",
                        actual: other.type_name(),
                    });
                }
            };
            let explicit_limit = arguments.get(3).is_some();
            let replacement_limit = arguments
                .get(3)
                .map(|_| integer_argument(arguments, 3, "string.gsub"))
                .transpose()?
                .map_or(MAX_DYNAMIC_REGISTERS, |limit| {
                    usize::try_from(limit.max(0)).unwrap_or(usize::MAX)
                });
            if replacement_limit > MAX_DYNAMIC_REGISTERS {
                return Err(RuntimeError::StackLimit {
                    required: replacement_limit,
                    limit: MAX_DYNAMIC_REGISTERS,
                });
            }

            let mut result = try_vec_with_capacity(haystack.len(), "string.gsub result")?;
            let mut search_start = 0;
            let mut copied_until = 0;
            let mut replacements = 0;
            while replacements < replacement_limit && search_start <= haystack.len() {
                let Some(found) = find_basic_lua_pattern(
                    haystack,
                    pattern,
                    search_start,
                    "string.gsub",
                    vm.active_profile()?,
                )?
                else {
                    break;
                };
                let first = found.start;
                let end = found.end;
                append_limited_string(&mut result, &haystack[copied_until..first])?;
                if let Some(replacement) = replacement.as_ref() {
                    append_gsub_replacement(
                        &mut result,
                        replacement,
                        haystack,
                        &found,
                        vm.active_profile()?,
                    )?;
                } else {
                    let table = replacement_table.expect("replacement kind was validated");
                    let key = gsub_table_key(vm, haystack, &found)?;
                    let value = vm.heap.table_get(table, &key)?;
                    match value {
                        Value::Nil => {
                            if vm.metamethod(&Value::Table(table), "__index")?.is_some() {
                                return Err(RuntimeError::UnsupportedLibraryFeature {
                                    function: "string.gsub",
                                    feature: "table replacement __index metamethods",
                                });
                            }
                            append_limited_string(&mut result, &haystack[found.start..found.end])?;
                        }
                        Value::Boolean(false) => {
                            append_limited_string(&mut result, &haystack[found.start..found.end])?
                        }
                        Value::String(value) => append_limited_string(&mut result, &value)?,
                        Value::Integer(_) | Value::Number(_) => {
                            let value = try_concat_bytes(&value)?
                                .expect("numeric replacements are concatenable");
                            append_limited_string(&mut result, &value)?;
                        }
                        other => {
                            return Err(RuntimeError::Type {
                                operation: "string.gsub table replacement",
                                expected: "string, number, false, or nil",
                                actual: other.type_name(),
                            });
                        }
                    }
                }
                replacements += 1;
                if end > first {
                    search_start = end;
                    copied_until = end;
                } else if first < haystack.len() {
                    append_limited_string(&mut result, &haystack[first..first + 1])?;
                    search_start = first + 1;
                    copied_until = search_start;
                } else {
                    search_start = haystack.len() + 1;
                    copied_until = haystack.len();
                }
            }
            if !explicit_limit
                && replacements == MAX_DYNAMIC_REGISTERS
                && find_basic_lua_pattern(
                    haystack,
                    pattern,
                    search_start,
                    "string.gsub",
                    vm.active_profile()?,
                )?
                .is_some()
            {
                return Err(RuntimeError::StackLimit {
                    required: MAX_DYNAMIC_REGISTERS + 1,
                    limit: MAX_DYNAMIC_REGISTERS,
                });
            }
            append_limited_string(&mut result, &haystack[copied_until..])?;
            Ok(vec![
                Value::String(Arc::from(result)),
                profiled_integral_math_result(vm, "string.gsub", replacements as f64)?,
            ])
        });
        let string = self.heap.allocate_table(0, 1)?;
        self.heap.table_set(
            string,
            Value::String(Arc::from(&b"sub"[..])),
            Value::NativeFunction(string_sub),
        )?;
        self.heap.table_set(
            string,
            Value::String(Arc::from(&b"find"[..])),
            Value::NativeFunction(string_find),
        )?;
        self.heap.table_set(
            string,
            Value::String(Arc::from(&b"match"[..])),
            Value::NativeFunction(string_match),
        )?;
        self.heap.table_set(
            string,
            Value::String(Arc::from(&b"gsub"[..])),
            Value::NativeFunction(string_gsub),
        )?;
        let string_len = self.register_function(|vm, arguments| {
            let string = arguments.first().ok_or(RuntimeError::Argument {
                function: "string.len",
                index: 1,
            })?;
            Ok(vec![profiled_integral_math_result(
                vm,
                "string.len",
                string_bytes(string, "string.len")?.len() as f64,
            )?])
        });
        self.heap.table_set(
            string,
            Value::String(Arc::from(&b"len"[..])),
            Value::NativeFunction(string_len),
        )?;
        let string_byte = self.register_function(|vm, arguments| {
            let string = arguments.first().ok_or(RuntimeError::Argument {
                function: "string.byte",
                index: 1,
            })?;
            let string = string_bytes(string, "string.byte")?;
            let start = arguments
                .get(1)
                .map(|_| integer_argument(arguments, 1, "string.byte"))
                .transpose()?
                .unwrap_or(1);
            let end = arguments
                .get(2)
                .map(|_| integer_argument(arguments, 2, "string.byte"))
                .transpose()?
                .unwrap_or(start);
            let start = relative_index(start, string.len()).clamp(1, string.len() as i64 + 1);
            let end = relative_index(end, string.len()).clamp(0, string.len() as i64);
            if start > end {
                return Ok(Vec::new());
            }
            let bytes = &string[(start - 1) as usize..end as usize];
            if bytes.len() > vm.native_result_limit {
                return Err(RuntimeError::NativeResultLimit {
                    required: bytes.len(),
                    limit: vm.native_result_limit,
                });
            }
            let mut values = try_vec_with_capacity(bytes.len(), "string.byte results")?;
            for value in bytes {
                values.push(profiled_integral_math_result(
                    vm,
                    "string.byte",
                    f64::from(*value),
                )?);
            }
            Ok(values)
        });
        self.heap.table_set(
            string,
            Value::String(Arc::from(&b"byte"[..])),
            Value::NativeFunction(string_byte),
        )?;
        let string_reverse = self.register_function(|_, arguments| {
            let string = arguments.first().ok_or(RuntimeError::Argument {
                function: "string.reverse",
                index: 1,
            })?;
            let mut result =
                try_clone_bytes(string_bytes(string, "string.reverse")?, "reversed string")?;
            result.reverse();
            Ok(vec![Value::String(Arc::from(result))])
        });
        self.heap.table_set(
            string,
            Value::String(Arc::from(&b"reverse"[..])),
            Value::NativeFunction(string_reverse),
        )?;
        let string_char = self.register_function(|_, arguments| {
            let mut result = try_vec_with_capacity(arguments.len(), "string.char result")?;
            for argument in arguments {
                let index = result.len();
                let value = argument.as_number().ok_or(RuntimeError::Type {
                    operation: "string.char",
                    expected: "number",
                    actual: argument.type_name(),
                })? as i64;
                if !(0..=255).contains(&value) {
                    return Err(RuntimeError::StringByte { index, value });
                }
                result.push(value as u8);
            }
            Ok(vec![Value::String(Arc::from(result))])
        });
        self.heap.table_set(
            string,
            Value::String(Arc::from(&b"char"[..])),
            Value::NativeFunction(string_char),
        )?;
        let string_rep = self.register_function(|vm, arguments| {
            let value = arguments.first().ok_or(RuntimeError::Argument {
                function: "string.rep",
                index: 1,
            })?;
            let value = string_bytes(value, "string.rep")?;
            let count = integer_argument(arguments, 1, "string.rep")?.max(0) as usize;
            let separator = match (vm.active_profile()?, arguments.get(2)) {
                (SemanticProfile::Luau, _) | (_, None) => &[],
                (_, Some(separator)) => string_bytes(separator, "string.rep")?,
            };
            let value_bytes = value
                .len()
                .checked_mul(count)
                .ok_or(RuntimeError::StringLimit {
                    required: usize::MAX,
                    limit: MAX_STRING_BYTES,
                })?;
            let separator_bytes = separator.len().checked_mul(count.saturating_sub(1)).ok_or(
                RuntimeError::StringLimit {
                    required: usize::MAX,
                    limit: MAX_STRING_BYTES,
                },
            )?;
            let required =
                value_bytes
                    .checked_add(separator_bytes)
                    .ok_or(RuntimeError::StringLimit {
                        required: usize::MAX,
                        limit: MAX_STRING_BYTES,
                    })?;
            if required > MAX_STRING_BYTES {
                return Err(RuntimeError::StringLimit {
                    required,
                    limit: MAX_STRING_BYTES,
                });
            }
            let mut result = try_vec_with_capacity(required, "repeated string")?;
            for index in 0..count {
                if index != 0 {
                    result.extend_from_slice(separator);
                }
                result.extend_from_slice(value);
            }
            Ok(vec![Value::String(Arc::from(result))])
        });
        self.heap.table_set(
            string,
            Value::String(Arc::from(&b"rep"[..])),
            Value::NativeFunction(string_rep),
        )?;
        let string_lower = self.register_function(|_, arguments| {
            let value = arguments.first().ok_or(RuntimeError::Argument {
                function: "string.lower",
                index: 1,
            })?;
            let mut result =
                try_clone_bytes(string_bytes(value, "string.lower")?, "lowercase string")?;
            result.make_ascii_lowercase();
            Ok(vec![Value::String(Arc::from(result))])
        });
        self.heap.table_set(
            string,
            Value::String(Arc::from(&b"lower"[..])),
            Value::NativeFunction(string_lower),
        )?;
        let string_upper = self.register_function(|_, arguments| {
            let value = arguments.first().ok_or(RuntimeError::Argument {
                function: "string.upper",
                index: 1,
            })?;
            let mut result =
                try_clone_bytes(string_bytes(value, "string.upper")?, "uppercase string")?;
            result.make_ascii_uppercase();
            Ok(vec![Value::String(Arc::from(result))])
        });
        self.heap.table_set(
            string,
            Value::String(Arc::from(&b"upper"[..])),
            Value::NativeFunction(string_upper),
        )?;
        let string_split = self.register_function(|vm, arguments| {
            let profile = vm.active_profile()?;
            if !matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
                return Err(RuntimeError::UnsupportedSemanticProfile {
                    operation: "string.split",
                    profile,
                });
            }
            let input = arguments.first().ok_or(RuntimeError::Argument {
                function: "string.split",
                index: 1,
            })?;
            let input = try_concat_bytes(input)?.ok_or(RuntimeError::Type {
                operation: "string.split",
                expected: "string or number",
                actual: input.type_name(),
            })?;
            let separator = match arguments.get(1) {
                None | Some(Value::Nil) => Cow::Borrowed(&b","[..]),
                Some(value) => try_concat_bytes(value)?.ok_or(RuntimeError::Type {
                    operation: "string.split",
                    expected: "string or number separator",
                    actual: value.type_name(),
                })?,
            };
            let field_count = if separator.is_empty() {
                input.len()
            } else {
                let mut count = 1usize;
                let mut start = 0usize;
                while start <= input.len() {
                    let Some(offset) = input[start..]
                        .windows(separator.len())
                        .position(|candidate| candidate == separator.as_ref())
                    else {
                        break;
                    };
                    count = count.saturating_add(1);
                    start += offset + separator.len();
                }
                count
            };
            if field_count > MAX_TABLE_INITIAL_CAPACITY {
                return Err(RuntimeError::TableCapacity {
                    kind: "array",
                    requested: field_count as u64,
                    limit: MAX_TABLE_INITIAL_CAPACITY,
                });
            }
            let roots = GcRoots::from_values(arguments)?;
            let table = vm.allocate_table(field_count, 0, &roots)?;
            let mut field = 0usize;
            let mut start = 0usize;
            if separator.is_empty() {
                for byte in input.iter() {
                    field += 1;
                    vm.table_set(
                        table,
                        Value::Integer(field as i64),
                        Value::String(Arc::from(std::slice::from_ref(byte))),
                        &roots,
                    )?;
                }
            } else {
                loop {
                    let next = input[start..]
                        .windows(separator.len())
                        .position(|candidate| candidate == separator.as_ref());
                    let end = next.map_or(input.len(), |offset| start + offset);
                    field += 1;
                    vm.table_set(
                        table,
                        Value::Integer(field as i64),
                        Value::String(Arc::from(&input[start..end])),
                        &roots,
                    )?;
                    let Some(_) = next else {
                        break;
                    };
                    start = end + separator.len();
                }
            }
            Ok(vec![Value::Table(table)])
        });
        self.heap.table_set(
            string,
            Value::String(Arc::from(&b"split"[..])),
            Value::NativeFunction(string_split),
        )?;
        self.set_global(&b"string"[..], Value::Table(string));

        self.install_table_library()?;
        self.install_math_library()?;
        self.install_bit32_library()?;
        self.install_coroutine_library()?;
        Ok(())
    }

    fn install_coroutine_library(&mut self) -> Result<(), RuntimeError> {
        let create = self.register_function(|vm, arguments| {
            let function = arguments.first().cloned().ok_or(RuntimeError::Argument {
                function: "coroutine.create",
                index: 1,
            })?;
            if !matches!(
                function,
                Value::Closure(_) | Value::CoroutineFunction(_) | Value::NativeFunction(_)
            ) {
                return Err(RuntimeError::Type {
                    operation: "coroutine.create",
                    expected: "function",
                    actual: function.type_name(),
                });
            }
            let roots = GcRoots::from_values(arguments)?;
            vm.threads
                .try_reserve(1)
                .map_err(|_| RuntimeError::Allocation {
                    what: "coroutine state",
                })?;
            let thread = vm.allocate_thread(std::slice::from_ref(&function), &roots)?;
            vm.threads.insert(thread, ThreadState::New(function));
            Ok(vec![Value::Thread(thread)])
        });
        let status = self.register_function(|vm, arguments| {
            let thread = thread_argument(arguments, 0, "coroutine.status")?;
            let status = match vm.threads.get(&thread) {
                Some(ThreadState::New(_) | ThreadState::Suspended(_)) => b"suspended".as_slice(),
                Some(ThreadState::Running) => b"running".as_slice(),
                Some(ThreadState::Dead(_)) => b"dead".as_slice(),
                None => return Err(RuntimeError::Heap(HeapError::StaleThread(thread))),
            };
            Ok(vec![Value::String(Arc::from(status))])
        });
        let resume = self.register_function(|_, _| Err(RuntimeError::NativeFunction(u32::MAX)));
        self.coroutine_resume = Some(resume);
        let yield_function =
            self.register_function(|_, _| Err(RuntimeError::NativeFunction(u32::MAX)));
        self.coroutine_yield = Some(yield_function);
        let wrap = self.register_function(|vm, arguments| {
            let function = arguments.first().cloned().ok_or(RuntimeError::Argument {
                function: "coroutine.wrap",
                index: 1,
            })?;
            if !matches!(
                function,
                Value::Closure(_) | Value::CoroutineFunction(_) | Value::NativeFunction(_)
            ) {
                return Err(RuntimeError::Type {
                    operation: "coroutine.wrap",
                    expected: "function",
                    actual: function.type_name(),
                });
            }
            let roots = GcRoots::from_values(arguments)?;
            vm.threads
                .try_reserve(1)
                .map_err(|_| RuntimeError::Allocation {
                    what: "coroutine state",
                })?;
            let thread = vm.allocate_thread(std::slice::from_ref(&function), &roots)?;
            vm.threads.insert(thread, ThreadState::New(function));
            Ok(vec![Value::CoroutineFunction(thread)])
        });
        let running = self.register_function(|vm, _| {
            let profile = vm.active_profile()?;
            if profile == SemanticProfile::Lua51 && vm.running_thread.is_none() {
                return Ok(vec![Value::Nil]);
            }
            let thread = Value::Thread(vm.running_thread.unwrap_or(vm.main_thread));
            match profile {
                SemanticProfile::Luau | SemanticProfile::Lua51 => Ok(vec![thread]),
                SemanticProfile::Blu
                | SemanticProfile::Lua52
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55 => {
                    Ok(vec![thread, Value::Boolean(vm.running_thread.is_none())])
                }
                _ => Err(RuntimeError::UnsupportedSemanticProfile {
                    operation: "coroutine.running",
                    profile,
                }),
            }
        });
        let isyieldable = self.register_function(|vm, _| {
            let profile = vm.active_profile()?;
            match profile {
                SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55 => Ok(vec![Value::Boolean(vm.running_thread.is_some())]),
                SemanticProfile::Luau => Ok(vec![Value::Boolean(true)]),
                SemanticProfile::Lua51 | SemanticProfile::Lua52 => {
                    Err(RuntimeError::UnsupportedSemanticProfile {
                        operation: "coroutine.isyieldable",
                        profile,
                    })
                }
                _ => Err(RuntimeError::UnsupportedSemanticProfile {
                    operation: "coroutine.isyieldable",
                    profile,
                }),
            }
        });
        let close = self.register_function(|vm, arguments| {
            let thread = thread_argument(arguments, 0, "coroutine.close")?;
            let result = match vm.threads.get(&thread) {
                None => return Err(RuntimeError::Heap(HeapError::StaleThread(thread))),
                Some(ThreadState::Running) => vec![
                    Value::Boolean(false),
                    Value::String(Arc::from(&b"cannot close a running coroutine"[..])),
                ],
                Some(ThreadState::Dead(error)) => match error {
                    Some(error) => vec![Value::Boolean(false), error.clone()],
                    None => vec![Value::Boolean(true)],
                },
                Some(ThreadState::New(_) | ThreadState::Suspended(_)) => {
                    let empty = GcRoots::default();
                    let roots = GcRoots::from_values(arguments)?;
                    vm.thread_set_roots(thread, &empty, &roots)?;
                    vm.threads.insert(thread, ThreadState::Dead(None));
                    vec![Value::Boolean(true)]
                }
            };
            Ok(result)
        });

        let table = self.heap.allocate_table(0, 8)?;
        for (name, function) in [
            (&b"create"[..], create),
            (&b"status"[..], status),
            (&b"resume"[..], resume),
            (&b"yield"[..], yield_function),
            (&b"wrap"[..], wrap),
            (&b"running"[..], running),
            (&b"isyieldable"[..], isyieldable),
            (&b"close"[..], close),
        ] {
            self.heap.table_set(
                table,
                Value::String(Arc::from(name)),
                Value::NativeFunction(function),
            )?;
        }
        self.set_global(&b"coroutine"[..], Value::Table(table));
        Ok(())
    }

    fn resume_thread(
        &mut self,
        arguments: &[Value],
        remaining: &mut u64,
        depth: usize,
        roots: GcRoots,
    ) -> Result<Vec<Value>, RuntimeError> {
        let thread = thread_argument(arguments, 0, "coroutine.resume")?;
        match self.threads.get(&thread) {
            None => return Err(RuntimeError::Heap(HeapError::StaleThread(thread))),
            Some(ThreadState::Running | ThreadState::Dead(_)) => {
                return Ok(vec![
                    Value::Boolean(false),
                    Value::String(Arc::from(&b"cannot resume non-suspended coroutine"[..])),
                ]);
            }
            Some(ThreadState::New(_) | ThreadState::Suspended(_)) => {}
        }
        let mut thread_roots = roots;
        thread_roots.push_value(Value::Thread(thread))?;
        let resumed_arguments = arguments.get(1..).unwrap_or_default();
        try_reserve_exact(
            &mut thread_roots.values,
            resumed_arguments.len(),
            "coroutine GC roots",
        )?;
        thread_roots.values.extend_from_slice(resumed_arguments);
        let argument_roots = GcRoots::from_values(arguments)?;
        self.thread_set_roots(thread, &thread_roots, &argument_roots)?;
        let state = self
            .threads
            .remove(&thread)
            .expect("resumable thread was checked above");
        let resumable = match state {
            ThreadState::New(function) => Resumable::New(function),
            ThreadState::Suspended(continuation) => Resumable::Continuation(continuation),
            ThreadState::Running | ThreadState::Dead(_) => {
                unreachable!("resumable thread state changed without re-entry")
            }
        };
        self.threads.insert(thread, ThreadState::Running);
        let previous_thread = self.running_thread.replace(thread);
        let result = match resumable {
            Resumable::New(function) => self.call_value(
                function,
                arguments.get(1..).unwrap_or_default(),
                remaining,
                depth,
                thread_roots,
            ),
            Resumable::Continuation(mut continuation) => {
                let active_root_count = self.active_roots.len();
                let result = (|| {
                    continuation.frame.write_results(
                        continuation.register,
                        continuation.encoded_count,
                        try_clone_values(
                            arguments.get(1..).unwrap_or_default(),
                            "coroutine continuation arguments",
                        )?,
                    )?;
                    for caller in &continuation.callers {
                        let roots = caller.gc_roots(&self.heap)?;
                        self.push_active_roots(roots)?;
                    }
                    self.run_resumed_frames(
                        continuation.frame,
                        continuation.callers,
                        remaining,
                        continuation.depth,
                    )
                })();
                self.active_roots.truncate(active_root_count);
                result
            }
        };
        self.running_thread = previous_thread;
        let finalized = match result {
            Ok(values) => {
                let resumed = match try_prepend_value(
                    values,
                    Value::Boolean(true),
                    "coroutine resume results",
                ) {
                    Ok(resumed) => resumed,
                    Err(error) => {
                        self.threads.insert(thread, ThreadState::Dead(None));
                        return Err(error);
                    }
                };
                self.threads.insert(thread, ThreadState::Dead(None));
                let empty = GcRoots::default();
                self.thread_set_roots(thread, &empty, &empty)?;
                resumed
            }
            Err(RuntimeError::CoroutineYield(values))
                if matches!(self.threads.get(&thread), Some(ThreadState::Suspended(_))) =>
            {
                match try_prepend_value(values, Value::Boolean(true), "coroutine yield results") {
                    Ok(yielded) => yielded,
                    Err(error) => {
                        self.threads.insert(thread, ThreadState::Dead(None));
                        return Err(error);
                    }
                }
            }
            Err(error) => {
                let error_value = runtime_error_value(error);
                let empty = GcRoots::default();
                let error_roots = match GcRoots::from_values(std::slice::from_ref(&error_value)) {
                    Ok(roots) => roots,
                    Err(error) => {
                        self.threads.insert(thread, ThreadState::Dead(None));
                        return Err(error);
                    }
                };
                let mut failed = match try_vec_with_capacity(2, "coroutine error results") {
                    Ok(failed) => failed,
                    Err(error) => {
                        self.threads.insert(thread, ThreadState::Dead(None));
                        return Err(error);
                    }
                };
                failed.push(Value::Boolean(false));
                failed.push(error_value.clone());
                if let Err(error) = self.thread_set_roots(thread, &error_roots, &empty) {
                    self.threads.insert(thread, ThreadState::Dead(None));
                    return Err(error);
                }
                self.threads
                    .insert(thread, ThreadState::Dead(Some(error_value)));
                failed
            }
        };
        Ok(finalized)
    }

    fn run_resumed_frames(
        &mut self,
        mut frame: Frame,
        mut callers: Vec<Caller>,
        remaining: &mut u64,
        depth: usize,
    ) -> Result<Vec<Value>, RuntimeError> {
        loop {
            let mut saved_callers = try_clone_callers(&callers)?;
            match self.run_frames(frame, callers, remaining, depth) {
                Ok(values) => return Ok(values),
                Err(error @ RuntimeError::CoroutineYield(_)) => return Err(error),
                Err(error) => {
                    let Some(protected_index) = saved_callers
                        .iter()
                        .rposition(|caller| caller.return_mode.catches_errors())
                    else {
                        return Err(error);
                    };
                    let mut protected = saved_callers.remove(protected_index);
                    saved_callers.truncate(protected_index);
                    callers = saved_callers;
                    let error_value = runtime_error_value(error);
                    let results = match protected.return_mode.clone() {
                        ReturnMode::Protected => {
                            vec![Value::Boolean(false), error_value]
                        }
                        ReturnMode::ErrorHandler(handler) => {
                            let roots = protected.frame.gc_roots(&self.heap)?;
                            match self.call_value(handler, &[error_value], remaining, depth, roots)
                            {
                                Ok(values) => vec![
                                    Value::Boolean(false),
                                    values.into_iter().next().unwrap_or(Value::Nil),
                                ],
                                Err(RuntimeError::CoroutineYield(values)) => {
                                    let thread = self
                                        .running_thread
                                        .ok_or(RuntimeError::CoroutineYieldOutside)?;
                                    self.suspend_thread(
                                        thread,
                                        protected.frame,
                                        callers,
                                        protected.register,
                                        protected.encoded_count,
                                        ReturnMode::ErrorHandlerResult,
                                        depth,
                                    )?;
                                    return Err(RuntimeError::CoroutineYield(values));
                                }
                                Err(handler_error) => {
                                    vec![Value::Boolean(false), runtime_error_value(handler_error)]
                                }
                            }
                        }
                        ReturnMode::Direct
                        | ReturnMode::ErrorHandlerResult
                        | ReturnMode::Operation(_) => {
                            unreachable!("only protected callers catch errors")
                        }
                    };
                    protected.frame.refresh_open_upvalues(&self.heap)?;
                    protected.frame.write_results(
                        protected.register,
                        protected.encoded_count,
                        results,
                    )?;
                    frame = protected.frame;
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn suspend_thread(
        &mut self,
        thread: ThreadId,
        frame: Frame,
        mut callers: Vec<Caller>,
        register: u8,
        encoded_count: u8,
        return_mode: ReturnMode,
        depth: usize,
    ) -> Result<(), RuntimeError> {
        if let Some(ThreadState::Suspended(continuation)) = self.threads.get(&thread) {
            let additional =
                continuation
                    .callers
                    .len()
                    .checked_add(1)
                    .ok_or(RuntimeError::Allocation {
                        what: "coroutine continuation callers",
                    })?;
            try_reserve_exact(&mut callers, additional, "coroutine continuation callers")?;
        }
        let continuation = match self.threads.remove(&thread) {
            Some(ThreadState::Suspended(mut continuation)) => {
                let mut prefix = callers;
                prefix.push(Caller {
                    frame,
                    register,
                    encoded_count,
                    return_mode,
                });
                prefix.append(&mut continuation.callers);
                continuation.callers = prefix;
                continuation
            }
            Some(ThreadState::Running) => Continuation {
                frame,
                callers,
                register,
                encoded_count,
                depth,
            },
            Some(state) => {
                self.threads.insert(thread, state);
                return Err(RuntimeError::CoroutineYieldOutside);
            }
            None => return Err(RuntimeError::Heap(HeapError::StaleThread(thread))),
        };
        let roots = match continuation_roots(&continuation.frame, &continuation.callers, &self.heap)
        {
            Ok(roots) => roots,
            Err(error) => {
                self.threads
                    .insert(thread, ThreadState::Suspended(continuation));
                return Err(error);
            }
        };
        if let Err(error) = self.thread_set_roots(thread, &roots, &GcRoots::default()) {
            self.threads
                .insert(thread, ThreadState::Suspended(continuation));
            return Err(error);
        }
        self.threads
            .insert(thread, ThreadState::Suspended(continuation));
        Ok(())
    }

    fn install_table_library(&mut self) -> Result<(), RuntimeError> {
        let insert = self.register_function(|vm, arguments| {
            let roots = GcRoots::from_values(arguments)?;
            let table = arguments.first().ok_or(RuntimeError::Argument {
                function: "table.insert",
                index: 1,
            })?;
            let table = table_id(table)?;
            let length = vm.heap.table_length(table)?;
            let (position, value) = match arguments {
                [_, value] => (length + 1, value.clone()),
                [_, position, value, ..] => {
                    let position = position.as_number().ok_or(RuntimeError::Type {
                        operation: "table.insert",
                        expected: "number",
                        actual: position.type_name(),
                    })? as i64;
                    let position =
                        usize::try_from(position).map_err(|_| RuntimeError::TablePosition {
                            function: "table.insert",
                            position,
                            length,
                        })?;
                    (position, value.clone())
                }
                _ => {
                    return Err(RuntimeError::Argument {
                        function: "table.insert",
                        index: 2,
                    });
                }
            };
            if !(1..=length + 1).contains(&position) {
                return Err(RuntimeError::TablePosition {
                    function: "table.insert",
                    position: position as i64,
                    length,
                });
            }
            for index in (position..=length).rev() {
                let value = vm.heap.table_get(table, &Value::Integer(index as i64))?;
                vm.table_set(table, Value::Integer((index + 1) as i64), value, &roots)?;
            }
            vm.table_set(table, Value::Integer(position as i64), value, &roots)?;
            Ok(Vec::new())
        });

        let remove = self.register_function(|vm, arguments| {
            let table = arguments.first().ok_or(RuntimeError::Argument {
                function: "table.remove",
                index: 1,
            })?;
            let table = table_id(table)?;
            let length = vm.heap.table_length(table)?;
            let position = arguments
                .get(1)
                .and_then(Value::as_number)
                .map_or(length as i64, |value| value as i64);
            if position < 1 || position > length as i64 {
                return Ok(vec![Value::Nil]);
            }
            let removed = vm.heap.table_get(table, &Value::Integer(position))?;
            let mut roots = GcRoots::from_values(arguments)?;
            roots.push_value(removed.clone())?;
            for index in position as usize..length {
                let value = vm
                    .heap
                    .table_get(table, &Value::Integer((index + 1) as i64))?;
                vm.table_set(table, Value::Integer(index as i64), value, &roots)?;
            }
            vm.table_set(table, Value::Integer(length as i64), Value::Nil, &roots)?;
            Ok(vec![removed])
        });

        let concat = self.register_function(|vm, arguments| {
            let table = arguments.first().ok_or(RuntimeError::Argument {
                function: "table.concat",
                index: 1,
            })?;
            let table = table_id(table)?;
            let separator = match arguments.get(1) {
                Some(value) => try_concat_bytes(value)?.ok_or(RuntimeError::Type {
                    operation: "table.concat",
                    expected: "string or number",
                    actual: value.type_name(),
                })?,
                None => Cow::Borrowed(&[] as &[u8]),
            };
            let start = arguments
                .get(2)
                .map(|_| integer_argument(arguments, 2, "table.concat"))
                .transpose()?
                .unwrap_or(1);
            let end = arguments
                .get(3)
                .map(|_| integer_argument(arguments, 3, "table.concat"))
                .transpose()?
                .unwrap_or(vm.heap.table_length(table)? as i64);
            let count = if start <= end {
                usize::try_from(i128::from(end) - i128::from(start) + 1).map_err(|_| {
                    RuntimeError::StackLimit {
                        required: usize::MAX,
                        limit: MAX_DYNAMIC_REGISTERS,
                    }
                })?
            } else {
                0
            };
            if count > MAX_DYNAMIC_REGISTERS {
                return Err(RuntimeError::StackLimit {
                    required: count,
                    limit: MAX_DYNAMIC_REGISTERS,
                });
            }

            let mut required = separator.len().checked_mul(count.saturating_sub(1)).ok_or(
                RuntimeError::StringLimit {
                    required: usize::MAX,
                    limit: MAX_STRING_BYTES,
                },
            )?;
            for offset in 0..count {
                let index = start + offset as i64;
                let value = vm.heap.table_get(table, &Value::Integer(index))?;
                if !matches!(
                    value,
                    Value::String(_) | Value::Integer(_) | Value::Number(_)
                ) {
                    return Err(RuntimeError::Type {
                        operation: "table.concat",
                        expected: "string or number",
                        actual: value.type_name(),
                    });
                }
                required = required.checked_add(rendered_value_len(&value)).ok_or(
                    RuntimeError::StringLimit {
                        required: usize::MAX,
                        limit: MAX_STRING_BYTES,
                    },
                )?;
                if required > MAX_STRING_BYTES {
                    return Err(RuntimeError::StringLimit {
                        required,
                        limit: MAX_STRING_BYTES,
                    });
                }
            }

            let mut result = try_vec_with_capacity(required, "table.concat result")?;
            for offset in 0..count {
                if offset != 0 {
                    result.extend_from_slice(&separator);
                }
                let index = start + offset as i64;
                let value = vm.heap.table_get(table, &Value::Integer(index))?;
                append_value(&mut result, &value);
            }
            Ok(vec![Value::String(Arc::from(result))])
        });
        let pack = self.register_function(|vm, arguments| {
            let roots = GcRoots::from_values(arguments)?;
            let count = profiled_integral_math_result(vm, "table.pack", arguments.len() as f64)?;
            let table = vm.allocate_table(arguments.len(), 1, &roots)?;
            for (index, value) in arguments.iter().enumerate() {
                vm.table_set(
                    table,
                    Value::Integer((index + 1) as i64),
                    value.clone(),
                    &roots,
                )?;
            }
            vm.table_set(table, Value::String(Arc::from(&b"n"[..])), count, &roots)?;
            Ok(vec![Value::Table(table)])
        });
        let unpack = self.register_function(|vm, arguments| {
            let table = arguments.first().ok_or(RuntimeError::Argument {
                function: "table.unpack",
                index: 1,
            })?;
            let table = table_id(table)?;
            let start = arguments
                .get(1)
                .map(|_| integer_argument(arguments, 1, "table.unpack"))
                .transpose()?
                .unwrap_or(1);
            let end = arguments
                .get(2)
                .map(|_| integer_argument(arguments, 2, "table.unpack"))
                .transpose()?
                .unwrap_or(vm.heap.table_length(table)? as i64);
            if end < start {
                return Ok(Vec::new());
            }
            let count = usize::try_from(end - start + 1).map_err(|_| RuntimeError::StackLimit {
                required: usize::MAX,
                limit: MAX_DYNAMIC_REGISTERS,
            })?;
            if count > MAX_DYNAMIC_REGISTERS {
                return Err(RuntimeError::StackLimit {
                    required: count,
                    limit: MAX_DYNAMIC_REGISTERS,
                });
            }
            let mut values = try_vec_with_capacity(count, "table.unpack results")?;
            for index in start..=end {
                values.push(vm.heap.table_get(table, &Value::Integer(index))?);
            }
            Ok(values)
        });
        let move_values = self.register_function(|vm, arguments| {
            let profile = vm.active_profile()?;
            if matches!(profile, SemanticProfile::Lua51 | SemanticProfile::Lua52) {
                return Err(RuntimeError::UnsupportedSemanticProfile {
                    operation: "table.move",
                    profile,
                });
            }
            let source = arguments.first().ok_or(RuntimeError::Argument {
                function: "table.move",
                index: 1,
            })?;
            let source = table_id(source)?;
            let first = integer_argument(arguments, 1, "table.move")?;
            let last = integer_argument(arguments, 2, "table.move")?;
            let target = integer_argument(arguments, 3, "table.move")?;
            let destination = arguments.get(4).map_or(Ok(source), table_id)?;
            if last < first {
                return Ok(vec![Value::Table(destination)]);
            }
            let count = i128::from(last) - i128::from(first) + 1;
            let final_target = i128::from(target) + count - 1;
            if final_target > i128::from(i64::MAX) {
                return Err(RuntimeError::TablePosition {
                    function: "table.move",
                    position: i64::MAX,
                    length: usize::MAX,
                });
            }
            let count = usize::try_from(count).map_err(|_| RuntimeError::StackLimit {
                required: usize::MAX,
                limit: MAX_DYNAMIC_REGISTERS,
            })?;
            if count > MAX_DYNAMIC_REGISTERS {
                return Err(RuntimeError::StackLimit {
                    required: count,
                    limit: MAX_DYNAMIC_REGISTERS,
                });
            }
            let roots = GcRoots::from_values(arguments)?;
            let backwards =
                source == destination && target > first && i128::from(target) <= i128::from(last);
            for offset in 0..count {
                let offset = if backwards {
                    count - offset - 1
                } else {
                    offset
                };
                let offset = i64::try_from(offset).map_err(|_| RuntimeError::StackLimit {
                    required: count,
                    limit: MAX_DYNAMIC_REGISTERS,
                })?;
                let value = vm.heap.table_get(source, &Value::Integer(first + offset))?;
                vm.table_set(destination, Value::Integer(target + offset), value, &roots)?;
            }
            Ok(vec![Value::Table(destination)])
        });
        let sort = self.register_function(|vm, arguments| {
            let table = arguments.first().ok_or(RuntimeError::Argument {
                function: "table.sort",
                index: 1,
            })?;
            let table = table_id(table)?;
            if arguments
                .get(1)
                .is_some_and(|value| !matches!(value, Value::Nil))
            {
                return Err(RuntimeError::UnsupportedLibraryFeature {
                    function: "table.sort",
                    feature: "custom comparators",
                });
            }
            let length = vm.heap.table_length(table)?;
            if length > MAX_DYNAMIC_REGISTERS {
                return Err(RuntimeError::StackLimit {
                    required: length,
                    limit: MAX_DYNAMIC_REGISTERS,
                });
            }
            let mut values = try_vec_with_capacity(length, "table.sort values")?;
            for index in 1..=length {
                values.push(vm.heap.table_get(table, &Value::Integer(index as i64))?);
            }
            let numeric = values
                .iter()
                .all(|value| value.as_number().is_some_and(|value| !value.is_nan()));
            let strings = values.iter().all(|value| matches!(value, Value::String(_)));
            if !numeric && !strings {
                let actual = values
                    .iter()
                    .find(|value| {
                        if numeric {
                            value.as_number().is_none()
                        } else {
                            !matches!(value, Value::String(_))
                        }
                    })
                    .map_or("nil", Value::type_name);
                return Err(RuntimeError::Type {
                    operation: "table.sort",
                    expected: "uniform ordered numbers or strings",
                    actual,
                });
            }
            if numeric {
                values.sort_unstable_by(|left, right| {
                    if left.numeric_less(right) == Some(true) {
                        core::cmp::Ordering::Less
                    } else if right.numeric_less(left) == Some(true) {
                        core::cmp::Ordering::Greater
                    } else {
                        core::cmp::Ordering::Equal
                    }
                });
            } else {
                values.sort_unstable_by(|left, right| match (left, right) {
                    (Value::String(left), Value::String(right)) => left.cmp(right),
                    _ => unreachable!("string sort values were validated"),
                });
            }
            let mut roots = GcRoots::from_values(arguments)?;
            roots.extend(GcRoots::from_values(&values)?)?;
            for (offset, value) in values.into_iter().enumerate() {
                vm.table_set(table, Value::Integer((offset + 1) as i64), value, &roots)?;
            }
            Ok(Vec::new())
        });
        let create = self.register_function(|vm, arguments| {
            let profile = vm.active_profile()?;
            if !matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
                return Err(RuntimeError::UnsupportedSemanticProfile {
                    operation: "table.create",
                    profile,
                });
            }
            let count = integer_argument(arguments, 0, "table.create")?;
            if count < 0 {
                return Err(RuntimeError::InvalidRange {
                    operation: "table.create",
                });
            }
            let count = usize::try_from(count).map_err(|_| RuntimeError::TableCapacity {
                kind: "array",
                requested: u64::MAX,
                limit: MAX_TABLE_INITIAL_CAPACITY,
            })?;
            if count > MAX_TABLE_INITIAL_CAPACITY {
                return Err(RuntimeError::TableCapacity {
                    kind: "array",
                    requested: count as u64,
                    limit: MAX_TABLE_INITIAL_CAPACITY,
                });
            }
            let fill = arguments.get(1).cloned().unwrap_or(Value::Nil);
            let roots = GcRoots::from_values(arguments)?;
            let table = vm.allocate_table(count, 0, &roots)?;
            if !matches!(fill, Value::Nil) {
                for index in 1..=count {
                    vm.table_set(table, Value::Integer(index as i64), fill.clone(), &roots)?;
                }
            }
            Ok(vec![Value::Table(table)])
        });
        let find = self.register_function(|vm, arguments| {
            let profile = vm.active_profile()?;
            if !matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
                return Err(RuntimeError::UnsupportedSemanticProfile {
                    operation: "table.find",
                    profile,
                });
            }
            let table = arguments.first().ok_or(RuntimeError::Argument {
                function: "table.find",
                index: 1,
            })?;
            let table = table_id(table)?;
            let needle = arguments.get(1).ok_or(RuntimeError::Argument {
                function: "table.find",
                index: 2,
            })?;
            let start = arguments
                .get(2)
                .map(|_| integer_argument(arguments, 2, "table.find"))
                .transpose()?
                .unwrap_or(1);
            if start < 1 {
                return Err(RuntimeError::InvalidRange {
                    operation: "table.find",
                });
            }
            let length = vm.heap.table_length(table)? as i64;
            for index in start..=length {
                if vm.heap.table_get(table, &Value::Integer(index))? == *needle {
                    return Ok(vec![profiled_integral_math_result(
                        vm,
                        "table.find",
                        index as f64,
                    )?]);
                }
            }
            Ok(vec![Value::Nil])
        });
        let clear = self.register_function(|vm, arguments| {
            let profile = vm.active_profile()?;
            if !matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
                return Err(RuntimeError::UnsupportedSemanticProfile {
                    operation: "table.clear",
                    profile,
                });
            }
            let table = arguments.first().ok_or(RuntimeError::Argument {
                function: "table.clear",
                index: 1,
            })?;
            vm.heap.table_clear(table_id(table)?)?;
            Ok(Vec::new())
        });
        let clone_table = self.register_function(|vm, arguments| {
            let profile = vm.active_profile()?;
            if !matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
                return Err(RuntimeError::UnsupportedSemanticProfile {
                    operation: "table.clone",
                    profile,
                });
            }
            let source = arguments.first().ok_or(RuntimeError::Argument {
                function: "table.clone",
                index: 1,
            })?;
            let source = table_id(source)?;
            let metatable = vm.heap.table_metatable(source)?;
            if let Some(metatable) = metatable
                && !matches!(
                    vm.heap
                        .table_get(metatable, &Value::String(Arc::from(&b"__metatable"[..])))?,
                    Value::Nil
                )
            {
                return Err(RuntimeError::MetatableProtected);
            }
            let mut entries = Vec::new();
            let mut key = Value::Nil;
            while let Some((next_key, value)) = vm.heap.table_next(source, &key)? {
                try_reserve_exact(&mut entries, 1, "table.clone entries")?;
                key = next_key.clone();
                entries.push((next_key, value));
            }
            let array_capacity = vm.heap.table_length(source)?;
            if array_capacity > MAX_TABLE_INITIAL_CAPACITY
                || entries.len() > MAX_TABLE_INITIAL_CAPACITY
            {
                return Err(RuntimeError::TableCapacity {
                    kind: "clone",
                    requested: array_capacity.max(entries.len()) as u64,
                    limit: MAX_TABLE_INITIAL_CAPACITY,
                });
            }
            let mut roots = GcRoots::from_values(arguments)?;
            for (key, value) in &entries {
                roots.push_value(key.clone())?;
                roots.push_value(value.clone())?;
            }
            if let Some(metatable) = metatable {
                roots.push_value(Value::Table(metatable))?;
            }
            let clone = vm.allocate_table(array_capacity, entries.len(), &roots)?;
            for (key, value) in entries {
                vm.table_set(clone, key, value, &roots)?;
            }
            vm.heap.set_table_metatable(clone, metatable)?;
            Ok(vec![Value::Table(clone)])
        });
        let freeze = self.register_function(|vm, arguments| {
            let profile = vm.active_profile()?;
            if !matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
                return Err(RuntimeError::UnsupportedSemanticProfile {
                    operation: "table.freeze",
                    profile,
                });
            }
            let value = arguments.first().ok_or(RuntimeError::Argument {
                function: "table.freeze",
                index: 1,
            })?;
            let table = table_id(value)?;
            if let Some(metatable) = vm.heap.table_metatable(table)?
                && !matches!(
                    vm.heap
                        .table_get(metatable, &Value::String(Arc::from(&b"__metatable"[..])))?,
                    Value::Nil
                )
            {
                return Err(RuntimeError::MetatableProtected);
            }
            vm.heap.table_freeze(table)?;
            Ok(vec![value.clone()])
        });
        let is_frozen = self.register_function(|vm, arguments| {
            let profile = vm.active_profile()?;
            if !matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
                return Err(RuntimeError::UnsupportedSemanticProfile {
                    operation: "table.isfrozen",
                    profile,
                });
            }
            let table = arguments.first().ok_or(RuntimeError::Argument {
                function: "table.isfrozen",
                index: 1,
            })?;
            Ok(vec![Value::Boolean(
                vm.heap.table_is_frozen(table_id(table)?)?,
            )])
        });

        let table = self.heap.allocate_table(0, 13)?;
        for (name, function) in [
            (&b"insert"[..], insert),
            (&b"remove"[..], remove),
            (&b"concat"[..], concat),
            (&b"pack"[..], pack),
            (&b"unpack"[..], unpack),
            (&b"move"[..], move_values),
            (&b"sort"[..], sort),
            (&b"create"[..], create),
            (&b"find"[..], find),
            (&b"clear"[..], clear),
            (&b"clone"[..], clone_table),
            (&b"freeze"[..], freeze),
            (&b"isfrozen"[..], is_frozen),
        ] {
            self.heap.table_set(
                table,
                Value::String(Arc::from(name)),
                Value::NativeFunction(function),
            )?;
        }
        self.set_global(&b"table"[..], Value::Table(table));
        Ok(())
    }

    fn next_random_u64(&mut self) -> u64 {
        let mut state = self.random_state;
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        self.random_state = state;
        state.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn random_integer_inclusive(&mut self, lower: i64, upper: i64) -> i64 {
        let width = (upper as i128 - lower as i128 + 1) as u128;
        if width == (1u128 << 64) {
            return self.next_random_u64() as i64;
        }
        let domain = 1u128 << 64;
        let limit = domain - domain % width;
        let sample = loop {
            let sample = self.next_random_u64() as u128;
            if sample < limit {
                break sample;
            }
        };
        (lower as i128 + (sample % width) as i128) as i64
    }

    fn install_math_library(&mut self) -> Result<(), RuntimeError> {
        let abs = self.register_function(|vm, arguments| {
            let value = arguments.first().ok_or(RuntimeError::Argument {
                function: "math.abs",
                index: 1,
            })?;
            if matches!(
                vm.active_profile()?,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                if let Value::Integer(value) = value {
                    return Ok(vec![Value::Integer(value.wrapping_abs())]);
                }
            }
            let value = value.as_number().ok_or(RuntimeError::Type {
                operation: "math.abs",
                expected: "number",
                actual: value.type_name(),
            })?;
            Ok(vec![Value::Number(value.abs())])
        });
        let floor = self.register_function(|vm, arguments| {
            let value = number_argument(arguments, 0, "math.floor")?;
            Ok(vec![profiled_integral_math_result(
                vm,
                "math.floor",
                value.floor(),
            )?])
        });
        let ceil = self.register_function(|vm, arguments| {
            let value = number_argument(arguments, 0, "math.ceil")?;
            Ok(vec![profiled_integral_math_result(
                vm,
                "math.ceil",
                value.ceil(),
            )?])
        });
        let sqrt = self.register_function(|_, arguments| {
            let value = number_argument(arguments, 0, "math.sqrt")?;
            Ok(vec![Value::Number(value.sqrt())])
        });
        let exp = self.register_function(|_, arguments| {
            let value = number_argument(arguments, 0, "math.exp")?;
            Ok(vec![Value::Number(value.exp())])
        });
        let log = self.register_function(|vm, arguments| {
            let value = number_argument(arguments, 0, "math.log")?;
            let result = match (vm.active_profile()?, arguments.get(1)) {
                (SemanticProfile::Lua51, _) | (_, None) => value.ln(),
                (_, Some(_)) => value.log(number_argument(arguments, 1, "math.log")?),
            };
            Ok(vec![Value::Number(result)])
        });
        let sin = self.register_function(|_, arguments| {
            Ok(vec![Value::Number(
                number_argument(arguments, 0, "math.sin")?.sin(),
            )])
        });
        let cos = self.register_function(|_, arguments| {
            Ok(vec![Value::Number(
                number_argument(arguments, 0, "math.cos")?.cos(),
            )])
        });
        let tan = self.register_function(|_, arguments| {
            Ok(vec![Value::Number(
                number_argument(arguments, 0, "math.tan")?.tan(),
            )])
        });
        let atan = self.register_function(|vm, arguments| {
            let y = number_argument(arguments, 0, "math.atan")?;
            let result = match vm.active_profile()? {
                SemanticProfile::Luau | SemanticProfile::Lua51 | SemanticProfile::Lua52 => y.atan(),
                SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55 => {
                    let x = arguments
                        .get(1)
                        .map(|_| number_argument(arguments, 1, "math.atan"))
                        .transpose()?
                        .unwrap_or(1.0);
                    y.atan2(x)
                }
                profile => {
                    return Err(RuntimeError::UnsupportedSemanticProfile {
                        operation: "math.atan",
                        profile,
                    });
                }
            };
            Ok(vec![Value::Number(result)])
        });
        let asin = self.register_function(|_, arguments| {
            Ok(vec![Value::Number(
                number_argument(arguments, 0, "math.asin")?.asin(),
            )])
        });
        let acos = self.register_function(|_, arguments| {
            Ok(vec![Value::Number(
                number_argument(arguments, 0, "math.acos")?.acos(),
            )])
        });
        let rad = self.register_function(|_, arguments| {
            Ok(vec![Value::Number(
                number_argument(arguments, 0, "math.rad")?.to_radians(),
            )])
        });
        let deg = self.register_function(|_, arguments| {
            Ok(vec![Value::Number(
                number_argument(arguments, 0, "math.deg")?.to_degrees(),
            )])
        });
        let fmod = self.register_function(|vm, arguments| {
            let dividend = arguments.first().ok_or(RuntimeError::Argument {
                function: "math.fmod",
                index: 1,
            })?;
            let divisor = arguments.get(1).ok_or(RuntimeError::Argument {
                function: "math.fmod",
                index: 2,
            })?;
            if matches!(
                vm.active_profile()?,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                if let (Value::Integer(dividend), Value::Integer(divisor)) = (dividend, divisor) {
                    if *divisor == 0 {
                        return Err(RuntimeError::DivideByZero);
                    }
                    let result = if *dividend == i64::MIN && *divisor == -1 {
                        0
                    } else {
                        dividend % divisor
                    };
                    return Ok(vec![Value::Integer(result)]);
                }
            }
            let dividend = dividend.as_number().ok_or(RuntimeError::Type {
                operation: "math.fmod",
                expected: "number",
                actual: dividend.type_name(),
            })?;
            let divisor = divisor.as_number().ok_or(RuntimeError::Type {
                operation: "math.fmod",
                expected: "number",
                actual: divisor.type_name(),
            })?;
            Ok(vec![Value::Number(dividend % divisor)])
        });
        let modf = self.register_function(|vm, arguments| {
            let value = number_argument(arguments, 0, "math.modf")?;
            let integral = value.trunc();
            let modern = matches!(
                vm.active_profile()?,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            );
            let fractional = if value == integral {
                if modern { 0.0 } else { 0.0_f64.copysign(value) }
            } else {
                value - integral
            };
            Ok(vec![
                profiled_integral_math_result(vm, "math.modf", integral)?,
                Value::Number(fractional),
            ])
        });
        let min = self.register_function(|vm, arguments| {
            let mut values = arguments.iter();
            let mut selected = values.next().ok_or(RuntimeError::Argument {
                function: "math.min",
                index: 1,
            })?;
            selected.as_number().ok_or(RuntimeError::Type {
                operation: "math.min",
                expected: "number",
                actual: selected.type_name(),
            })?;
            for value in values {
                value.as_number().ok_or(RuntimeError::Type {
                    operation: "math.min",
                    expected: "number",
                    actual: value.type_name(),
                })?;
                if value.numeric_less(selected) == Some(true) {
                    selected = value;
                }
            }
            if matches!(
                vm.active_profile()?,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                Ok(vec![selected.clone()])
            } else {
                Ok(vec![Value::Number(
                    selected
                        .as_number()
                        .expect("selected math.min value was validated"),
                )])
            }
        });
        let max = self.register_function(|vm, arguments| {
            let mut values = arguments.iter();
            let mut selected = values.next().ok_or(RuntimeError::Argument {
                function: "math.max",
                index: 1,
            })?;
            selected.as_number().ok_or(RuntimeError::Type {
                operation: "math.max",
                expected: "number",
                actual: selected.type_name(),
            })?;
            for value in values {
                value.as_number().ok_or(RuntimeError::Type {
                    operation: "math.max",
                    expected: "number",
                    actual: value.type_name(),
                })?;
                if selected.numeric_less(value) == Some(true) {
                    selected = value;
                }
            }
            if matches!(
                vm.active_profile()?,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                Ok(vec![selected.clone()])
            } else {
                Ok(vec![Value::Number(
                    selected
                        .as_number()
                        .expect("selected math.max value was validated"),
                )])
            }
        });
        let math_type = self.register_function(|vm, arguments| {
            let profile = vm.active_profile()?;
            if !matches!(
                profile,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                return Err(RuntimeError::UnsupportedSemanticProfile {
                    operation: "math.type",
                    profile,
                });
            }
            let value = arguments.first().ok_or(RuntimeError::Argument {
                function: "math.type",
                index: 1,
            })?;
            Ok(vec![match value {
                Value::Integer(_) => Value::String(Arc::from(&b"integer"[..])),
                Value::Number(_) => Value::String(Arc::from(&b"float"[..])),
                _ => Value::Nil,
            }])
        });
        let to_integer = self.register_function(|vm, arguments| {
            let profile = vm.active_profile()?;
            if !matches!(
                profile,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                return Err(RuntimeError::UnsupportedSemanticProfile {
                    operation: "math.tointeger",
                    profile,
                });
            }
            let value = arguments.first().ok_or(RuntimeError::Argument {
                function: "math.tointeger",
                index: 1,
            })?;
            let integer = exact_integer_conversion(value, profile);
            Ok(vec![integer.map_or(Value::Nil, Value::Integer)])
        });
        let unsigned_less = self.register_function(|vm, arguments| {
            let profile = vm.active_profile()?;
            if !matches!(
                profile,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                return Err(RuntimeError::UnsupportedSemanticProfile {
                    operation: "math.ult",
                    profile,
                });
            }
            let left = arguments.first().ok_or(RuntimeError::Argument {
                function: "math.ult",
                index: 1,
            })?;
            let right = arguments.get(1).ok_or(RuntimeError::Argument {
                function: "math.ult",
                index: 2,
            })?;
            let left = exact_integer_conversion(left, profile).ok_or(RuntimeError::Type {
                operation: "math.ult",
                expected: "integer",
                actual: left.type_name(),
            })?;
            let right = exact_integer_conversion(right, profile).ok_or(RuntimeError::Type {
                operation: "math.ult",
                expected: "integer",
                actual: right.type_name(),
            })?;
            Ok(vec![Value::Boolean((left as u64) < right as u64)])
        });
        let clamp = self.register_function(|vm, arguments| {
            let profile = vm.active_profile()?;
            if !matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
                return Err(RuntimeError::UnsupportedSemanticProfile {
                    operation: "math.clamp",
                    profile,
                });
            }
            let value = number_argument(arguments, 0, "math.clamp")?;
            let minimum = number_argument(arguments, 1, "math.clamp")?;
            let maximum = number_argument(arguments, 2, "math.clamp")?;
            if maximum < minimum {
                return Err(RuntimeError::InvalidRange {
                    operation: "math.clamp",
                });
            }
            Ok(vec![Value::Number(if value < minimum {
                minimum
            } else if value > maximum {
                maximum
            } else {
                value
            })])
        });
        let sign = self.register_function(|vm, arguments| {
            let profile = vm.active_profile()?;
            if !matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
                return Err(RuntimeError::UnsupportedSemanticProfile {
                    operation: "math.sign",
                    profile,
                });
            }
            let value = number_argument(arguments, 0, "math.sign")?;
            Ok(vec![Value::Number(if value < 0.0 {
                -1.0
            } else if value > 0.0 {
                1.0
            } else {
                0.0
            })])
        });
        let round = self.register_function(|vm, arguments| {
            let profile = vm.active_profile()?;
            if !matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau) {
                return Err(RuntimeError::UnsupportedSemanticProfile {
                    operation: "math.round",
                    profile,
                });
            }
            Ok(vec![Value::Number(
                number_argument(arguments, 0, "math.round")?.round(),
            )])
        });
        let random = self.register_function(|vm, arguments| {
            if arguments.len() > 2 {
                return Err(RuntimeError::ArgumentCount {
                    function: "math.random",
                    expected: "zero, one, or two",
                    actual: arguments.len(),
                });
            }
            if arguments.is_empty() {
                let bits = vm.next_random_u64() >> 11;
                return Ok(vec![Value::Number(
                    bits as f64 * (1.0 / (1u64 << 53) as f64),
                )]);
            }
            let profile = vm.active_profile()?;
            let (lower, upper) = if arguments.len() == 1 {
                let upper = random_integer_argument(profile, arguments, 0, "math.random")?;
                if upper == 0
                    && matches!(
                        profile,
                        SemanticProfile::Blu | SemanticProfile::Lua54 | SemanticProfile::Lua55
                    )
                {
                    (i64::MIN, i64::MAX)
                } else {
                    (1, upper)
                }
            } else {
                (
                    random_integer_argument(profile, arguments, 0, "math.random")?,
                    random_integer_argument(profile, arguments, 1, "math.random")?,
                )
            };
            if lower > upper {
                return Err(RuntimeError::InvalidRange {
                    operation: "math.random",
                });
            }
            let value = vm.random_integer_inclusive(lower, upper);
            Ok(vec![if matches!(
                profile,
                SemanticProfile::Blu
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            ) {
                Value::Integer(value)
            } else {
                Value::Number(value as f64)
            }])
        });
        let randomseed = self.register_function(|vm, arguments| {
            let profile = vm.active_profile()?;
            if arguments.len() > 2 {
                return Err(RuntimeError::ArgumentCount {
                    function: "math.randomseed",
                    expected: "at most two",
                    actual: arguments.len(),
                });
            }
            let modern = matches!(
                profile,
                SemanticProfile::Blu | SemanticProfile::Lua54 | SemanticProfile::Lua55
            );
            if arguments.is_empty() && !modern {
                return Err(RuntimeError::Argument {
                    function: "math.randomseed",
                    index: 1,
                });
            }
            let first = if arguments.is_empty() {
                vm.next_random_u64() as i64
            } else {
                random_seed_argument(profile, arguments, 0)?
            };
            let second = if modern {
                arguments
                    .get(1)
                    .map(|_| random_seed_argument(profile, arguments, 1))
                    .transpose()?
                    .unwrap_or(0)
            } else {
                0
            };
            vm.random_state = mix_random_seed(first as u64, second as u64);
            if modern {
                Ok(vec![Value::Integer(first), Value::Integer(second)])
            } else {
                Ok(Vec::new())
            }
        });

        let table = self.heap.allocate_table(0, 28)?;
        for (name, value) in [
            (&b"abs"[..], Value::NativeFunction(abs)),
            (&b"floor"[..], Value::NativeFunction(floor)),
            (&b"ceil"[..], Value::NativeFunction(ceil)),
            (&b"sqrt"[..], Value::NativeFunction(sqrt)),
            (&b"exp"[..], Value::NativeFunction(exp)),
            (&b"log"[..], Value::NativeFunction(log)),
            (&b"sin"[..], Value::NativeFunction(sin)),
            (&b"cos"[..], Value::NativeFunction(cos)),
            (&b"tan"[..], Value::NativeFunction(tan)),
            (&b"atan"[..], Value::NativeFunction(atan)),
            (&b"asin"[..], Value::NativeFunction(asin)),
            (&b"acos"[..], Value::NativeFunction(acos)),
            (&b"rad"[..], Value::NativeFunction(rad)),
            (&b"deg"[..], Value::NativeFunction(deg)),
            (&b"fmod"[..], Value::NativeFunction(fmod)),
            (&b"modf"[..], Value::NativeFunction(modf)),
            (&b"min"[..], Value::NativeFunction(min)),
            (&b"max"[..], Value::NativeFunction(max)),
            (&b"type"[..], Value::NativeFunction(math_type)),
            (&b"tointeger"[..], Value::NativeFunction(to_integer)),
            (&b"ult"[..], Value::NativeFunction(unsigned_less)),
            (&b"clamp"[..], Value::NativeFunction(clamp)),
            (&b"sign"[..], Value::NativeFunction(sign)),
            (&b"round"[..], Value::NativeFunction(round)),
            (&b"random"[..], Value::NativeFunction(random)),
            (&b"randomseed"[..], Value::NativeFunction(randomseed)),
            (&b"pi"[..], Value::Number(core::f64::consts::PI)),
            (&b"huge"[..], Value::Number(f64::INFINITY)),
        ] {
            self.heap
                .table_set(table, Value::String(Arc::from(name)), value)?;
        }
        self.set_global(&b"math"[..], Value::Table(table));
        Ok(())
    }

    fn install_bit32_library(&mut self) -> Result<(), RuntimeError> {
        let band = self.register_function(|vm, arguments| {
            bit32_profile(vm, "bit32.band")?;
            let mut result = u32::MAX;
            for index in 0..arguments.len() {
                result &= bit32_argument(vm, arguments, index, "bit32.band")?;
            }
            Ok(vec![profiled_integral_math_result(
                vm,
                "bit32.band",
                f64::from(result),
            )?])
        });
        let bor = self.register_function(|vm, arguments| {
            bit32_profile(vm, "bit32.bor")?;
            let mut result = 0u32;
            for index in 0..arguments.len() {
                result |= bit32_argument(vm, arguments, index, "bit32.bor")?;
            }
            Ok(vec![profiled_integral_math_result(
                vm,
                "bit32.bor",
                f64::from(result),
            )?])
        });
        let bxor = self.register_function(|vm, arguments| {
            bit32_profile(vm, "bit32.bxor")?;
            let mut result = 0u32;
            for index in 0..arguments.len() {
                result ^= bit32_argument(vm, arguments, index, "bit32.bxor")?;
            }
            Ok(vec![profiled_integral_math_result(
                vm,
                "bit32.bxor",
                f64::from(result),
            )?])
        });
        let bnot = self.register_function(|vm, arguments| {
            bit32_profile(vm, "bit32.bnot")?;
            let result = !bit32_argument(vm, arguments, 0, "bit32.bnot")?;
            Ok(vec![profiled_integral_math_result(
                vm,
                "bit32.bnot",
                f64::from(result),
            )?])
        });
        let lshift = self.register_function(|vm, arguments| {
            bit32_profile(vm, "bit32.lshift")?;
            let value = bit32_argument(vm, arguments, 0, "bit32.lshift")?;
            let displacement = bit32_argument(vm, arguments, 1, "bit32.lshift")? as i32;
            let result = bit32_shift(value, displacement);
            Ok(vec![profiled_integral_math_result(
                vm,
                "bit32.lshift",
                f64::from(result),
            )?])
        });
        let rshift = self.register_function(|vm, arguments| {
            bit32_profile(vm, "bit32.rshift")?;
            let value = bit32_argument(vm, arguments, 0, "bit32.rshift")?;
            let displacement = bit32_argument(vm, arguments, 1, "bit32.rshift")? as i32;
            let result = bit32_shift(value, displacement.saturating_neg());
            Ok(vec![profiled_integral_math_result(
                vm,
                "bit32.rshift",
                f64::from(result),
            )?])
        });
        let arshift = self.register_function(|vm, arguments| {
            bit32_profile(vm, "bit32.arshift")?;
            let value = bit32_argument(vm, arguments, 0, "bit32.arshift")?;
            let displacement = bit32_argument(vm, arguments, 1, "bit32.arshift")? as i32;
            let result = if displacement < 0 {
                bit32_shift(value, displacement.saturating_neg())
            } else if displacement >= 32 {
                if value & 0x8000_0000 == 0 {
                    0
                } else {
                    u32::MAX
                }
            } else {
                ((value as i32) >> displacement) as u32
            };
            Ok(vec![profiled_integral_math_result(
                vm,
                "bit32.arshift",
                f64::from(result),
            )?])
        });
        let lrotate = self.register_function(|vm, arguments| {
            bit32_profile(vm, "bit32.lrotate")?;
            let value = bit32_argument(vm, arguments, 0, "bit32.lrotate")?;
            let displacement = bit32_argument(vm, arguments, 1, "bit32.lrotate")?;
            let result = value.rotate_left(displacement);
            Ok(vec![profiled_integral_math_result(
                vm,
                "bit32.lrotate",
                f64::from(result),
            )?])
        });
        let rrotate = self.register_function(|vm, arguments| {
            bit32_profile(vm, "bit32.rrotate")?;
            let value = bit32_argument(vm, arguments, 0, "bit32.rrotate")?;
            let displacement = bit32_argument(vm, arguments, 1, "bit32.rrotate")?;
            let result = value.rotate_right(displacement);
            Ok(vec![profiled_integral_math_result(
                vm,
                "bit32.rrotate",
                f64::from(result),
            )?])
        });
        let extract = self.register_function(|vm, arguments| {
            bit32_profile(vm, "bit32.extract")?;
            let value = bit32_argument(vm, arguments, 0, "bit32.extract")?;
            let field = bit32_argument(vm, arguments, 1, "bit32.extract")?;
            let width = if arguments.len() > 2 {
                bit32_argument(vm, arguments, 2, "bit32.extract")?
            } else {
                1
            };
            let mask = bit32_field_mask(field, width, "bit32.extract")?;
            let result = (value >> field) & mask;
            Ok(vec![profiled_integral_math_result(
                vm,
                "bit32.extract",
                f64::from(result),
            )?])
        });
        let replace = self.register_function(|vm, arguments| {
            bit32_profile(vm, "bit32.replace")?;
            let value = bit32_argument(vm, arguments, 0, "bit32.replace")?;
            let replacement = bit32_argument(vm, arguments, 1, "bit32.replace")?;
            let field = bit32_argument(vm, arguments, 2, "bit32.replace")?;
            let width = if arguments.len() > 3 {
                bit32_argument(vm, arguments, 3, "bit32.replace")?
            } else {
                1
            };
            let mask = bit32_field_mask(field, width, "bit32.replace")?;
            let shifted_mask = mask << field;
            let result = (value & !shifted_mask) | ((replacement & mask) << field);
            Ok(vec![profiled_integral_math_result(
                vm,
                "bit32.replace",
                f64::from(result),
            )?])
        });

        let table = self.heap.allocate_table(0, 11)?;
        for (name, function) in [
            (&b"band"[..], band),
            (&b"bor"[..], bor),
            (&b"bxor"[..], bxor),
            (&b"bnot"[..], bnot),
            (&b"lshift"[..], lshift),
            (&b"rshift"[..], rshift),
            (&b"arshift"[..], arshift),
            (&b"lrotate"[..], lrotate),
            (&b"rrotate"[..], rrotate),
            (&b"extract"[..], extract),
            (&b"replace"[..], replace),
        ] {
            self.heap.table_set(
                table,
                Value::String(Arc::from(name)),
                Value::NativeFunction(function),
            )?;
        }
        self.set_global(&b"bit32"[..], Value::Table(table));
        Ok(())
    }
}

fn string_bytes<'a>(value: &'a Value, operation: &'static str) -> Result<&'a [u8], RuntimeError> {
    match value {
        Value::String(value) => Ok(value),
        other => Err(RuntimeError::Type {
            operation,
            expected: "string",
            actual: other.type_name(),
        }),
    }
}

fn integer_argument(
    arguments: &[Value],
    zero_based_index: usize,
    function: &'static str,
) -> Result<i64, RuntimeError> {
    let value = arguments
        .get(zero_based_index)
        .ok_or(RuntimeError::Argument {
            function,
            index: zero_based_index + 1,
        })?;
    value
        .as_number()
        .map(|value| value as i64)
        .ok_or(RuntimeError::Type {
            operation: function,
            expected: "number",
            actual: value.type_name(),
        })
}

fn random_integer_argument(
    profile: SemanticProfile,
    arguments: &[Value],
    index: usize,
    function: &'static str,
) -> Result<i64, RuntimeError> {
    let value = arguments.get(index).ok_or(RuntimeError::Argument {
        function,
        index: index + 1,
    })?;
    if matches!(
        profile,
        SemanticProfile::Blu
            | SemanticProfile::Lua53
            | SemanticProfile::Lua54
            | SemanticProfile::Lua55
    ) {
        return exact_integer_conversion(value, profile).ok_or(RuntimeError::Type {
            operation: function,
            expected: "integer-representable number",
            actual: value.type_name(),
        });
    }
    let parsed;
    let value = if let Value::String(bytes) = value {
        parsed = parse_default_number(trim_ascii_bytes(bytes), profile);
        parsed.as_ref().ok_or(RuntimeError::Type {
            operation: function,
            expected: "number",
            actual: "string",
        })?
    } else {
        value
    };
    let number = value.as_number().ok_or(RuntimeError::Type {
        operation: function,
        expected: "number",
        actual: value.type_name(),
    })?;
    if !number.is_finite() {
        return Err(RuntimeError::Type {
            operation: function,
            expected: "finite number",
            actual: value.type_name(),
        });
    }
    Ok(match profile {
        SemanticProfile::Lua52 => number.round(),
        SemanticProfile::Luau | SemanticProfile::Lua51 => number.trunc(),
        _ => unreachable!("modern random arguments are handled above"),
    } as i64)
}

fn random_seed_argument(
    profile: SemanticProfile,
    arguments: &[Value],
    index: usize,
) -> Result<i64, RuntimeError> {
    if matches!(
        profile,
        SemanticProfile::Blu | SemanticProfile::Lua54 | SemanticProfile::Lua55
    ) {
        return random_integer_argument(profile, arguments, index, "math.randomseed");
    }
    let value = arguments.get(index).ok_or(RuntimeError::Argument {
        function: "math.randomseed",
        index: index + 1,
    })?;
    let parsed;
    let value = if let Value::String(bytes) = value {
        parsed = parse_default_number(trim_ascii_bytes(bytes), profile);
        parsed.as_ref().ok_or(RuntimeError::Type {
            operation: "math.randomseed",
            expected: "number",
            actual: "string",
        })?
    } else {
        value
    };
    let number = value.as_number().ok_or(RuntimeError::Type {
        operation: "math.randomseed",
        expected: "number",
        actual: value.type_name(),
    })?;
    if !number.is_finite() {
        return Err(RuntimeError::Type {
            operation: "math.randomseed",
            expected: "finite number",
            actual: value.type_name(),
        });
    }
    Ok(number.trunc() as i64)
}

fn mix_random_seed(first: u64, second: u64) -> u64 {
    let mut state = first ^ second.rotate_left(32) ^ 0x9e37_79b9_7f4a_7c15;
    state = (state ^ (state >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    state = (state ^ (state >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    state ^= state >> 31;
    if state == 0 {
        0x4d59_5df4_d0f3_3173
    } else {
        state
    }
}

fn trim_ascii_bytes(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn parse_based_number(bytes: &[u8], base: u32, profile: SemanticProfile) -> Option<Value> {
    if base == 10 && matches!(profile, SemanticProfile::Luau | SemanticProfile::Lua51) {
        return core::str::from_utf8(bytes)
            .ok()?
            .parse::<f64>()
            .ok()
            .map(Value::Number);
    }
    let (negative, digits) = match bytes.first() {
        Some(b'-') => (true, &bytes[1..]),
        Some(b'+') => (false, &bytes[1..]),
        _ => (false, bytes),
    };
    if digits.is_empty() {
        return None;
    }
    let magnitude = parse_unsigned_based_integer(digits, base)?;
    if matches!(
        profile,
        SemanticProfile::Blu
            | SemanticProfile::Lua53
            | SemanticProfile::Lua54
            | SemanticProfile::Lua55
    ) {
        let value = magnitude as i64;
        Some(Value::Integer(if negative {
            value.wrapping_neg()
        } else {
            value
        }))
    } else {
        let value = magnitude as f64;
        Some(Value::Number(if negative { -value } else { value }))
    }
}

fn parse_unsigned_based_integer(digits: &[u8], base: u32) -> Option<u64> {
    let mut value = 0_u64;
    for byte in digits {
        let digit = match *byte {
            b'0'..=b'9' => u32::from(*byte - b'0'),
            b'a'..=b'z' => u32::from(*byte - b'a') + 10,
            b'A'..=b'Z' => u32::from(*byte - b'A') + 10,
            _ => return None,
        };
        if digit >= base {
            return None;
        }
        value = value
            .checked_mul(u64::from(base))?
            .checked_add(u64::from(digit))?;
    }
    Some(value)
}

fn parse_hex_float(bytes: &[u8], negative: bool) -> Option<f64> {
    let exponent_at = bytes.iter().position(|byte| matches!(*byte, b'p' | b'P'));
    let (mantissa, exponent) = match exponent_at {
        Some(index) => {
            let exponent = bytes.get(index + 1..)?;
            if exponent.is_empty()
                || bytes[index + 1..]
                    .iter()
                    .any(|byte| matches!(*byte, b'p' | b'P'))
            {
                return None;
            }
            (
                &bytes[..index],
                parse_saturating_decimal_exponent(exponent)?,
            )
        }
        None => (bytes, 0),
    };
    if mantissa.is_empty() {
        return None;
    }
    let mut value = 0.0;
    let mut fractional_scale = None;
    let mut digits = 0usize;
    for byte in mantissa {
        if *byte == b'.' {
            if fractional_scale.is_some() {
                return None;
            }
            fractional_scale = Some(1.0 / 16.0);
            continue;
        }
        let digit = match *byte {
            b'0'..=b'9' => u32::from(*byte - b'0'),
            b'a'..=b'f' => u32::from(*byte - b'a') + 10,
            b'A'..=b'F' => u32::from(*byte - b'A') + 10,
            _ => return None,
        };
        digits += 1;
        if let Some(scale) = fractional_scale.as_mut() {
            value += f64::from(digit) * *scale;
            *scale /= 16.0;
        } else {
            value = value * 16.0 + f64::from(digit);
        }
    }
    if digits == 0 {
        return None;
    }
    value *= 2.0f64.powi(exponent);
    Some(if negative { -value } else { value })
}

fn parse_saturating_decimal_exponent(bytes: &[u8]) -> Option<i32> {
    let (negative, digits) = match bytes.first() {
        Some(b'-') => (true, &bytes[1..]),
        Some(b'+') => (false, &bytes[1..]),
        _ => (false, bytes),
    };
    if digits.is_empty() || digits.iter().any(|byte| !byte.is_ascii_digit()) {
        return None;
    }
    let mut value = 0i32;
    for byte in digits {
        value = value
            .saturating_mul(10)
            .saturating_add(i32::from(*byte - b'0'));
    }
    Some(if negative {
        value.saturating_neg()
    } else {
        value
    })
}

fn parse_default_number(bytes: &[u8], profile: SemanticProfile) -> Option<Value> {
    let modern = matches!(
        profile,
        SemanticProfile::Blu
            | SemanticProfile::Lua53
            | SemanticProfile::Lua54
            | SemanticProfile::Lua55
    );
    let (negative, unsigned) = match bytes.first() {
        Some(b'-') => (true, &bytes[1..]),
        Some(b'+') => (false, &bytes[1..]),
        _ => (false, bytes),
    };
    if let Some(hex) = unsigned
        .strip_prefix(b"0x")
        .or_else(|| unsigned.strip_prefix(b"0X"))
    {
        if hex.iter().any(|byte| matches!(*byte, b'.' | b'p' | b'P')) {
            return parse_hex_float(hex, negative).map(Value::Number);
        }
        let magnitude = parse_unsigned_based_integer(hex, 16)?;
        return Some(if modern {
            let integer = magnitude as i64;
            Value::Integer(if negative {
                integer.wrapping_neg()
            } else {
                integer
            })
        } else {
            let value = magnitude as f64;
            Value::Number(if negative { -value } else { value })
        });
    }
    let text = core::str::from_utf8(bytes).ok()?;
    if modern
        && !unsigned.is_empty()
        && unsigned.iter().all(u8::is_ascii_digit)
        && let Ok(integer) = text.parse::<i64>()
    {
        return Some(Value::Integer(integer));
    }
    let number = text.parse::<f64>().ok()?;
    if !matches!(profile, SemanticProfile::Luau | SemanticProfile::Lua51)
        && unsigned.iter().any(|byte| {
            !byte.is_ascii_digit() && !matches!(*byte, b'.' | b'e' | b'E' | b'+' | b'-')
        })
    {
        return None;
    }
    Some(Value::Number(number))
}

fn exact_integer_conversion(value: &Value, profile: SemanticProfile) -> Option<i64> {
    let parsed;
    let value = if let Value::String(bytes) = value {
        parsed = parse_default_number(trim_ascii_bytes(bytes), profile);
        parsed.as_ref()?
    } else {
        value
    };
    match value {
        Value::Integer(value) => Some(*value),
        Value::Number(value)
            if value.is_finite()
                && value.fract() == 0.0
                && *value >= i64::MIN as f64
                && *value < -(i64::MIN as f64) =>
        {
            Some(*value as i64)
        }
        _ => None,
    }
}

fn blu_bitwise_integer(
    value: &Value,
    profile: SemanticProfile,
    operation: &'static str,
) -> Result<i64, RuntimeError> {
    let parsed;
    let value = if let Value::String(bytes) = value {
        if profile == SemanticProfile::Lua53 {
            parsed = parse_default_number(trim_ascii_bytes(bytes), profile);
            parsed.as_ref().unwrap_or(value)
        } else {
            return Err(RuntimeError::Type {
                operation,
                expected: "integer-representable number",
                actual: "string",
            });
        }
    } else {
        value
    };
    exact_integer_conversion(value, profile).ok_or(RuntimeError::Type {
        operation,
        expected: "integer-representable number",
        actual: value.type_name(),
    })
}

fn lua_shift_left(value: i64, displacement: i64) -> i64 {
    if displacement >= i64::from(i64::BITS) || displacement <= -i64::from(i64::BITS) {
        0
    } else if displacement >= 0 {
        ((value as u64) << displacement) as i64
    } else {
        ((value as u64) >> displacement.unsigned_abs()) as i64
    }
}

fn bit32_profile(vm: &Vm, operation: &'static str) -> Result<SemanticProfile, RuntimeError> {
    let profile = vm.active_profile()?;
    if matches!(
        profile,
        SemanticProfile::Blu
            | SemanticProfile::Luau
            | SemanticProfile::Lua52
            | SemanticProfile::Lua53
    ) {
        Ok(profile)
    } else {
        Err(RuntimeError::UnsupportedSemanticProfile { operation, profile })
    }
}

fn bit32_argument(
    vm: &Vm,
    arguments: &[Value],
    index: usize,
    function: &'static str,
) -> Result<u32, RuntimeError> {
    let profile = bit32_profile(vm, function)?;
    let value = arguments.get(index).ok_or(RuntimeError::Argument {
        function,
        index: index + 1,
    })?;
    let parsed;
    let value = if let Value::String(bytes) = value {
        parsed = parse_default_number(trim_ascii_bytes(bytes), profile);
        parsed.as_ref().ok_or(RuntimeError::Type {
            operation: function,
            expected: "number",
            actual: "string",
        })?
    } else {
        value
    };
    if profile == SemanticProfile::Lua53 {
        return Ok(
            exact_integer_conversion(value, profile).ok_or(RuntimeError::Type {
                operation: function,
                expected: "integer-representable number",
                actual: value.type_name(),
            })? as u32,
        );
    }
    let number = value.as_number().ok_or(RuntimeError::Type {
        operation: function,
        expected: "number",
        actual: value.type_name(),
    })?;
    let number = match profile {
        SemanticProfile::Lua52 => number.round_ties_even(),
        SemanticProfile::Blu | SemanticProfile::Luau => number.trunc(),
        SemanticProfile::Lua53 => unreachable!("Lua 5.3 integers are handled above"),
        _ => {
            return Err(RuntimeError::UnsupportedSemanticProfile {
                operation: function,
                profile,
            });
        }
    };
    if !number.is_finite() {
        return Err(RuntimeError::Type {
            operation: function,
            expected: "finite number",
            actual: value.type_name(),
        });
    }
    Ok(number.rem_euclid(4_294_967_296.0) as u32)
}

fn bit32_shift(value: u32, displacement: i32) -> u32 {
    if displacement >= 32 || displacement <= -32 {
        0
    } else if displacement >= 0 {
        value << displacement
    } else {
        value >> displacement.unsigned_abs()
    }
}

fn bit32_field_mask(field: u32, width: u32, operation: &'static str) -> Result<u32, RuntimeError> {
    if field >= 32 || width == 0 || width > 32 - field {
        return Err(RuntimeError::InvalidRange { operation });
    }
    Ok(if width == 32 {
        u32::MAX
    } else {
        (1u32 << width) - 1
    })
}

fn number_argument(
    arguments: &[Value],
    zero_based_index: usize,
    function: &'static str,
) -> Result<f64, RuntimeError> {
    let value = arguments
        .get(zero_based_index)
        .ok_or(RuntimeError::Argument {
            function,
            index: zero_based_index + 1,
        })?;
    value.as_number().ok_or(RuntimeError::Type {
        operation: function,
        expected: "number",
        actual: value.type_name(),
    })
}

fn profiled_integral_math_result(
    vm: &Vm,
    operation: &'static str,
    value: f64,
) -> Result<Value, RuntimeError> {
    match vm.active_profile()? {
        SemanticProfile::Luau | SemanticProfile::Lua51 | SemanticProfile::Lua52 => {
            Ok(Value::Number(value))
        }
        SemanticProfile::Blu
        | SemanticProfile::Lua53
        | SemanticProfile::Lua54
        | SemanticProfile::Lua55 => {
            let upper_exclusive = -(i64::MIN as f64);
            if value.is_finite() && value >= i64::MIN as f64 && value < upper_exclusive {
                Ok(Value::Integer(value as i64))
            } else {
                Ok(Value::Number(value))
            }
        }
        profile => Err(RuntimeError::UnsupportedSemanticProfile { operation, profile }),
    }
}

fn thread_argument(
    arguments: &[Value],
    zero_based_index: usize,
    function: &'static str,
) -> Result<ThreadId, RuntimeError> {
    let value = arguments
        .get(zero_based_index)
        .ok_or(RuntimeError::Argument {
            function,
            index: zero_based_index + 1,
        })?;
    match value {
        Value::Thread(thread) => Ok(*thread),
        other => Err(RuntimeError::Type {
            operation: function,
            expected: "thread",
            actual: other.type_name(),
        }),
    }
}

fn relative_index(index: i64, length: usize) -> i64 {
    if index < 0 {
        length as i64 + index + 1
    } else {
        index
    }
}

#[derive(Clone, Copy)]
enum BasicPatternAtom {
    Literal(u8),
    Any,
    Class(u8),
    Set([u64; 4]),
    Backreference(u8),
    Balanced(u8, u8),
    Frontier([u64; 4]),
    CaptureStart(u8),
    CaptureEnd(u8),
    PositionCapture(u8),
}

#[derive(Clone, Copy)]
enum BasicPatternRepetition {
    One,
    ZeroOrMore,
    OneOrMore,
    Optional,
    Minimal,
}

#[derive(Clone, Copy)]
struct BasicPatternPiece {
    atom: BasicPatternAtom,
    repetition: BasicPatternRepetition,
}

#[derive(Clone, Copy, Debug, Default)]
struct BasicPatternCapture {
    start: usize,
    end: usize,
    position: bool,
    set: bool,
}

struct BasicPatternMatch {
    start: usize,
    end: usize,
    captures: [BasicPatternCapture; 32],
    capture_count: usize,
}

const MAX_PATTERN_WORK: usize = 10_000_000;

fn find_basic_lua_pattern(
    haystack: &[u8],
    pattern: &[u8],
    start: usize,
    operation: &'static str,
    profile: SemanticProfile,
) -> Result<Option<BasicPatternMatch>, RuntimeError> {
    let anchored = pattern.first() == Some(&b'^');
    let mut index = usize::from(anchored);
    let mut end_anchor = false;
    let mut pieces = try_vec_with_capacity(pattern.len(), "string.find pattern pieces")?;
    let mut open_captures = try_vec_with_capacity(32, "Lua pattern capture stack")?;
    let mut capture_count = 0usize;
    while index < pattern.len() {
        let byte = pattern[index];
        if byte == b'$' && index + 1 == pattern.len() {
            end_anchor = true;
            index += 1;
            continue;
        }
        let atom = if byte == b'.' {
            index += 1;
            BasicPatternAtom::Any
        } else if byte == b'%' {
            let escaped =
                pattern
                    .get(index + 1)
                    .copied()
                    .ok_or(RuntimeError::UnsupportedLibraryFeature {
                        function: operation,
                        feature: "malformed Lua patterns",
                    })?;
            if escaped == b'b' {
                let open = pattern.get(index + 2).copied().ok_or(
                    RuntimeError::UnsupportedLibraryFeature {
                        function: operation,
                        feature: "malformed Lua balanced patterns",
                    },
                )?;
                let close = pattern.get(index + 3).copied().ok_or(
                    RuntimeError::UnsupportedLibraryFeature {
                        function: operation,
                        feature: "malformed Lua balanced patterns",
                    },
                )?;
                index += 2;
                BasicPatternAtom::Balanced(open, close)
            } else if escaped == b'f' {
                if pattern.get(index + 2) != Some(&b'[') {
                    return Err(RuntimeError::UnsupportedLibraryFeature {
                        function: operation,
                        feature: "malformed Lua frontier patterns",
                    });
                }
                let (set, next) = parse_basic_pattern_set(pattern, index + 2, operation, profile)?;
                index = next - 2;
                BasicPatternAtom::Frontier(set)
            } else if matches!(
                escaped.to_ascii_lowercase(),
                b'a' | b'c' | b'd' | b'g' | b'l' | b'p' | b's' | b'u' | b'w' | b'x' | b'z'
            ) {
                BasicPatternAtom::Class(escaped)
            } else if matches!(escaped, b'1'..=b'9') {
                BasicPatternAtom::Backreference(escaped - b'1')
            } else if escaped.is_ascii_alphanumeric() {
                return Err(RuntimeError::UnsupportedLibraryFeature {
                    function: operation,
                    feature: "dialect-specific Lua pattern classes and captures",
                });
            } else {
                BasicPatternAtom::Literal(escaped)
            }
        } else if byte == b'[' {
            let (set, next) = parse_basic_pattern_set(pattern, index, operation, profile)?;
            index = next;
            BasicPatternAtom::Set(set)
        } else if byte == b'(' {
            if capture_count == 32 {
                return Err(RuntimeError::UnsupportedLibraryFeature {
                    function: operation,
                    feature: "more than 32 Lua pattern captures",
                });
            }
            let capture = capture_count as u8;
            capture_count += 1;
            if pattern.get(index + 1) == Some(&b')') {
                index += 2;
                BasicPatternAtom::PositionCapture(capture)
            } else {
                index += 1;
                open_captures.push(capture);
                BasicPatternAtom::CaptureStart(capture)
            }
        } else if byte == b')' {
            let capture = open_captures
                .pop()
                .ok_or(RuntimeError::UnsupportedLibraryFeature {
                    function: operation,
                    feature: "malformed Lua pattern captures",
                })?;
            index += 1;
            BasicPatternAtom::CaptureEnd(capture)
        } else if b"*+?-]".contains(&byte) {
            return Err(RuntimeError::UnsupportedLibraryFeature {
                function: operation,
                feature: "malformed Lua pattern repetition",
            });
        } else {
            index += 1;
            BasicPatternAtom::Literal(byte)
        };
        if byte == b'%' {
            index += 2;
        }
        let repetition = match pattern.get(index) {
            Some(b'*') => BasicPatternRepetition::ZeroOrMore,
            Some(b'+') => BasicPatternRepetition::OneOrMore,
            Some(b'?') => BasicPatternRepetition::Optional,
            Some(b'-') => BasicPatternRepetition::Minimal,
            _ => BasicPatternRepetition::One,
        };
        index += usize::from(!matches!(repetition, BasicPatternRepetition::One));
        pieces.push(BasicPatternPiece { atom, repetition });
    }
    if !open_captures.is_empty() {
        return Err(RuntimeError::UnsupportedLibraryFeature {
            function: operation,
            feature: "malformed Lua pattern captures",
        });
    }

    let candidates = if anchored {
        1
    } else {
        haystack.len().saturating_sub(start).saturating_add(1)
    };
    let required = candidates.saturating_mul(pieces.len().max(1));
    if required > MAX_PATTERN_WORK {
        return Err(RuntimeError::PatternWorkLimit {
            required,
            limit: MAX_PATTERN_WORK,
        });
    }
    let mut work = 0;
    for position in start..start.saturating_add(candidates) {
        if let Some((end, captures)) = match_basic_pattern_at(
            haystack, &pieces, position, end_anchor, &mut work, operation, profile,
        )? {
            return Ok(Some(BasicPatternMatch {
                start: position,
                end,
                captures,
                capture_count,
            }));
        }
    }
    Ok(None)
}

#[derive(Clone, Copy)]
enum BasicPatternState {
    Match {
        piece: usize,
        position: usize,
        event: Option<usize>,
    },
    Repeat {
        piece: usize,
        position: usize,
        count: usize,
        bound: usize,
        ascending: bool,
        event: Option<usize>,
    },
}

#[derive(Clone, Copy)]
enum BasicCaptureEventKind {
    Start,
    End,
    Position,
}

struct BasicCaptureEvent {
    parent: Option<usize>,
    capture: u8,
    position: usize,
    kind: BasicCaptureEventKind,
}

fn match_basic_pattern_at(
    haystack: &[u8],
    pieces: &[BasicPatternPiece],
    start: usize,
    end_anchor: bool,
    work: &mut usize,
    operation: &'static str,
    profile: SemanticProfile,
) -> Result<Option<(usize, [BasicPatternCapture; 32])>, RuntimeError> {
    let capacity = pieces.len().saturating_add(1);
    let mut states = try_vec_with_capacity(capacity, "string.find backtracking states")?;
    let mut events = try_vec_with_capacity(pieces.len().min(32), "string.find capture events")?;
    states.push(BasicPatternState::Match {
        piece: 0,
        position: start,
        event: None,
    });
    while let Some(state) = states.pop() {
        charge_pattern_work(work, 1, MAX_PATTERN_WORK)?;
        match state {
            BasicPatternState::Match {
                piece,
                position,
                event,
            } => {
                let Some(current) = pieces.get(piece) else {
                    if !end_anchor || position == haystack.len() {
                        return Ok(Some((position, materialize_basic_captures(&events, event))));
                    }
                    continue;
                };
                let capture_event = match current.atom {
                    BasicPatternAtom::CaptureStart(capture) => {
                        Some((capture, BasicCaptureEventKind::Start))
                    }
                    BasicPatternAtom::CaptureEnd(capture) => {
                        Some((capture, BasicCaptureEventKind::End))
                    }
                    BasicPatternAtom::PositionCapture(capture) => {
                        Some((capture, BasicCaptureEventKind::Position))
                    }
                    _ => None,
                };
                if let Some((capture, kind)) = capture_event {
                    if !matches!(current.repetition, BasicPatternRepetition::One) {
                        return Err(RuntimeError::UnsupportedLibraryFeature {
                            function: operation,
                            feature: "repetition applied to Lua pattern captures",
                        });
                    }
                    if events.len() >= 100_000 {
                        return Err(RuntimeError::PatternWorkLimit {
                            required: events.len() + 1,
                            limit: 100_000,
                        });
                    }
                    try_reserve_exact(&mut events, 1, "string.find capture events")?;
                    let next_event = events.len();
                    events.push(BasicCaptureEvent {
                        parent: event,
                        capture,
                        position,
                        kind,
                    });
                    states.push(BasicPatternState::Match {
                        piece: piece + 1,
                        position,
                        event: Some(next_event),
                    });
                    continue;
                }
                if let BasicPatternAtom::Frontier(set) = current.atom {
                    if !matches!(current.repetition, BasicPatternRepetition::One) {
                        return Err(RuntimeError::UnsupportedLibraryFeature {
                            function: operation,
                            feature: "repetition applied to Lua frontier patterns",
                        });
                    }
                    charge_pattern_work(work, 2, MAX_PATTERN_WORK)?;
                    let previous = position
                        .checked_sub(1)
                        .and_then(|previous| haystack.get(previous))
                        .copied()
                        .unwrap_or(0);
                    let next = haystack.get(position).copied().unwrap_or(0);
                    if !pattern_set_contains(&set, previous) && pattern_set_contains(&set, next) {
                        states.push(BasicPatternState::Match {
                            piece: piece + 1,
                            position,
                            event,
                        });
                    }
                    continue;
                }
                if let BasicPatternAtom::Balanced(open, close) = current.atom {
                    if !matches!(current.repetition, BasicPatternRepetition::One) {
                        return Err(RuntimeError::UnsupportedLibraryFeature {
                            function: operation,
                            feature: "repetition applied to Lua balanced patterns",
                        });
                    }
                    charge_pattern_work(work, 1, MAX_PATTERN_WORK)?;
                    if haystack.get(position) != Some(&open) {
                        continue;
                    }
                    let mut depth = 1usize;
                    let mut end = position + 1;
                    while let Some(byte) = haystack.get(end) {
                        charge_pattern_work(work, 1, MAX_PATTERN_WORK)?;
                        end += 1;
                        if *byte == open {
                            depth += 1;
                        } else if *byte == close {
                            depth -= 1;
                            if depth == 0 {
                                states.push(BasicPatternState::Match {
                                    piece: piece + 1,
                                    position: end,
                                    event,
                                });
                                break;
                            }
                        }
                    }
                    continue;
                }
                if let BasicPatternAtom::Backreference(capture) = current.atom {
                    if !matches!(current.repetition, BasicPatternRepetition::One) {
                        return Err(RuntimeError::UnsupportedLibraryFeature {
                            function: operation,
                            feature: "repetition applied to Lua pattern backreferences",
                        });
                    }
                    let captures = materialize_basic_captures(&events, event);
                    let capture = captures[usize::from(capture)];
                    if !capture.set {
                        return Err(RuntimeError::UnsupportedLibraryFeature {
                            function: operation,
                            feature: "invalid Lua pattern capture reference",
                        });
                    }
                    if capture.position {
                        continue;
                    }
                    let captured = &haystack[capture.start..capture.end];
                    charge_pattern_work(work, captured.len().max(1), MAX_PATTERN_WORK)?;
                    let Some(end) = position.checked_add(captured.len()) else {
                        continue;
                    };
                    if haystack.get(position..end) == Some(captured) {
                        states.push(BasicPatternState::Match {
                            piece: piece + 1,
                            position: end,
                            event,
                        });
                    }
                    continue;
                }
                if matches!(current.repetition, BasicPatternRepetition::One) {
                    let Some(byte) = haystack.get(position) else {
                        continue;
                    };
                    charge_pattern_work(work, 1, MAX_PATTERN_WORK)?;
                    if basic_pattern_atom_matches(&current.atom, *byte, profile) {
                        states.push(BasicPatternState::Match {
                            piece: piece + 1,
                            position: position + 1,
                            event,
                        });
                    }
                    continue;
                }

                let mut maximum = 0;
                while let Some(byte) = haystack.get(position + maximum) {
                    charge_pattern_work(work, 1, MAX_PATTERN_WORK)?;
                    if !basic_pattern_atom_matches(&current.atom, *byte, profile) {
                        break;
                    }
                    maximum += 1;
                    if matches!(current.repetition, BasicPatternRepetition::Optional) {
                        break;
                    }
                }
                let minimum = usize::from(matches!(
                    current.repetition,
                    BasicPatternRepetition::OneOrMore
                ));
                if maximum < minimum {
                    continue;
                }
                let ascending = matches!(current.repetition, BasicPatternRepetition::Minimal);
                states.push(BasicPatternState::Repeat {
                    piece,
                    position,
                    count: if ascending { minimum } else { maximum },
                    bound: if ascending { maximum } else { minimum },
                    ascending,
                    event,
                });
            }
            BasicPatternState::Repeat {
                piece,
                position,
                count,
                bound,
                ascending,
                event,
            } => {
                if ascending && count < bound {
                    states.push(BasicPatternState::Repeat {
                        piece,
                        position,
                        count: count + 1,
                        bound,
                        ascending,
                        event,
                    });
                } else if !ascending && count > bound {
                    states.push(BasicPatternState::Repeat {
                        piece,
                        position,
                        count: count - 1,
                        bound,
                        ascending,
                        event,
                    });
                }
                states.push(BasicPatternState::Match {
                    piece: piece + 1,
                    position: position + count,
                    event,
                });
            }
        }
    }
    Ok(None)
}

fn materialize_basic_captures(
    events: &[BasicCaptureEvent],
    mut event: Option<usize>,
) -> [BasicPatternCapture; 32] {
    let mut captures = [BasicPatternCapture::default(); 32];
    while let Some(index) = event {
        let current = &events[index];
        let capture = &mut captures[usize::from(current.capture)];
        match current.kind {
            BasicCaptureEventKind::Position if !capture.set => {
                *capture = BasicPatternCapture {
                    start: current.position,
                    end: current.position,
                    position: true,
                    set: true,
                };
            }
            BasicCaptureEventKind::End if !capture.set => {
                capture.end = current.position;
                capture.set = true;
            }
            BasicCaptureEventKind::Start if capture.set && !capture.position => {
                capture.start = current.position;
            }
            _ => {}
        }
        event = current.parent;
    }
    captures
}

fn charge_pattern_work(work: &mut usize, amount: usize, limit: usize) -> Result<(), RuntimeError> {
    *work = work.saturating_add(amount);
    if *work > limit {
        Err(RuntimeError::PatternWorkLimit {
            required: *work,
            limit,
        })
    } else {
        Ok(())
    }
}

fn basic_pattern_atom_matches(atom: &BasicPatternAtom, byte: u8, profile: SemanticProfile) -> bool {
    match atom {
        BasicPatternAtom::Literal(expected) => *expected == byte,
        BasicPatternAtom::Any => true,
        BasicPatternAtom::Class(class) => byte_matches_pattern_class(byte, *class, profile),
        BasicPatternAtom::Set(set) => pattern_set_contains(set, byte),
        BasicPatternAtom::Backreference(_)
        | BasicPatternAtom::Balanced(_, _)
        | BasicPatternAtom::Frontier(_)
        | BasicPatternAtom::CaptureStart(_)
        | BasicPatternAtom::CaptureEnd(_)
        | BasicPatternAtom::PositionCapture(_) => false,
    }
}

fn parse_basic_pattern_set(
    pattern: &[u8],
    start: usize,
    operation: &'static str,
    profile: SemanticProfile,
) -> Result<([u64; 4], usize), RuntimeError> {
    let mut cursor = start + 1;
    let negated = pattern.get(cursor) == Some(&b'^');
    cursor += usize::from(negated);
    let first_item = cursor;
    let mut closing = None;
    while cursor < pattern.len() {
        if pattern[cursor] == b']' && cursor > first_item {
            closing = Some(cursor);
            break;
        }
        if pattern[cursor] == b'%' {
            cursor = cursor.saturating_add(1);
        }
        cursor = cursor.saturating_add(1);
    }
    let closing = closing.ok_or(RuntimeError::UnsupportedLibraryFeature {
        function: operation,
        feature: "malformed Lua pattern sets",
    })?;

    let mut set = [0_u64; 4];
    cursor = first_item;
    while cursor < closing {
        let byte = pattern[cursor];
        if byte == b'%' {
            let escaped = pattern.get(cursor + 1).copied().ok_or(
                RuntimeError::UnsupportedLibraryFeature {
                    function: operation,
                    feature: "malformed Lua pattern sets",
                },
            )?;
            if matches!(
                escaped.to_ascii_lowercase(),
                b'a' | b'c' | b'd' | b'g' | b'l' | b'p' | b's' | b'u' | b'w' | b'x' | b'z'
            ) {
                for candidate in u8::MIN..=u8::MAX {
                    if byte_matches_pattern_class(candidate, escaped, profile) {
                        pattern_set_insert(&mut set, candidate);
                    }
                }
            } else if escaped.is_ascii_alphanumeric() {
                return Err(RuntimeError::UnsupportedLibraryFeature {
                    function: operation,
                    feature: "dialect-specific Lua pattern classes and captures",
                });
            } else {
                pattern_set_insert(&mut set, escaped);
            }
            cursor += 2;
        } else if cursor + 2 < closing && pattern[cursor + 1] == b'-' {
            for candidate in byte..=pattern[cursor + 2] {
                pattern_set_insert(&mut set, candidate);
            }
            cursor += 3;
        } else {
            pattern_set_insert(&mut set, byte);
            cursor += 1;
        }
    }
    if negated {
        for word in &mut set {
            *word = !*word;
        }
    }
    Ok((set, closing + 1))
}

fn pattern_set_insert(set: &mut [u64; 4], byte: u8) {
    set[usize::from(byte) / 64] |= 1_u64 << (byte % 64);
}

fn pattern_set_contains(set: &[u64; 4], byte: u8) -> bool {
    set[usize::from(byte) / 64] & (1_u64 << (byte % 64)) != 0
}

fn byte_matches_pattern_class(byte: u8, class: u8, profile: SemanticProfile) -> bool {
    if profile == SemanticProfile::Lua51 && class.eq_ignore_ascii_case(&b'g') {
        return byte == class;
    }
    let matches = match class.to_ascii_lowercase() {
        b'a' => byte.is_ascii_alphabetic(),
        b'c' => byte.is_ascii_control(),
        b'd' => byte.is_ascii_digit(),
        b'g' => byte.is_ascii_graphic(),
        b'l' => byte.is_ascii_lowercase(),
        b'p' => byte.is_ascii_punctuation(),
        b's' => byte.is_ascii_whitespace(),
        b'u' => byte.is_ascii_uppercase(),
        b'w' => byte.is_ascii_alphanumeric(),
        b'x' => byte.is_ascii_hexdigit(),
        b'z' => byte == 0,
        _ => false,
    };
    if class.is_ascii_lowercase() {
        matches
    } else {
        !matches
    }
}

fn append_limited_string(result: &mut Vec<u8>, bytes: &[u8]) -> Result<(), RuntimeError> {
    let required = result
        .len()
        .checked_add(bytes.len())
        .ok_or(RuntimeError::StringLimit {
            required: usize::MAX,
            limit: MAX_STRING_BYTES,
        })?;
    if required > MAX_STRING_BYTES {
        return Err(RuntimeError::StringLimit {
            required,
            limit: MAX_STRING_BYTES,
        });
    }
    try_reserve_exact(result, bytes.len(), "string result")?;
    result.extend_from_slice(bytes);
    Ok(())
}

fn append_basic_capture_values(
    values: &mut Vec<Value>,
    vm: &Vm,
    haystack: &[u8],
    found: &BasicPatternMatch,
    operation: &'static str,
) -> Result<(), RuntimeError> {
    for capture in found.captures.iter().take(found.capture_count) {
        if !capture.set {
            return Err(RuntimeError::UnsupportedLibraryFeature {
                function: operation,
                feature: "unfinished Lua pattern captures",
            });
        }
        if capture.position {
            values.push(profiled_integral_math_result(
                vm,
                operation,
                (capture.start + 1) as f64,
            )?);
        } else {
            values.push(Value::String(Arc::from(
                &haystack[capture.start..capture.end],
            )));
        }
    }
    Ok(())
}

fn gsub_table_key(
    vm: &Vm,
    haystack: &[u8],
    found: &BasicPatternMatch,
) -> Result<Value, RuntimeError> {
    let Some(capture) = found.captures.first().filter(|_| found.capture_count != 0) else {
        return Ok(Value::String(Arc::from(&haystack[found.start..found.end])));
    };
    if !capture.set {
        return Err(RuntimeError::UnsupportedLibraryFeature {
            function: "string.gsub",
            feature: "unfinished Lua pattern captures",
        });
    }
    if capture.position {
        profiled_integral_math_result(vm, "string.gsub", (capture.start + 1) as f64)
    } else {
        Ok(Value::String(Arc::from(
            &haystack[capture.start..capture.end],
        )))
    }
}

fn append_gsub_replacement(
    result: &mut Vec<u8>,
    replacement: &[u8],
    haystack: &[u8],
    found: &BasicPatternMatch,
    profile: SemanticProfile,
) -> Result<(), RuntimeError> {
    let mut index = 0;
    while index < replacement.len() {
        if replacement[index] != b'%' {
            append_limited_string(result, &replacement[index..index + 1])?;
            index += 1;
            continue;
        }
        let escaped =
            replacement
                .get(index + 1)
                .copied()
                .ok_or(RuntimeError::UnsupportedLibraryFeature {
                    function: "string.gsub",
                    feature: "malformed replacement escapes",
                })?;
        match escaped {
            b'0' => append_limited_string(result, &haystack[found.start..found.end])?,
            b'%' => append_limited_string(result, b"%")?,
            b'1'..=b'9' => {
                let capture_index = usize::from(escaped - b'1');
                if found.capture_count == 0 && capture_index == 0 {
                    append_limited_string(result, &haystack[found.start..found.end])?;
                } else {
                    let capture = found
                        .captures
                        .get(capture_index)
                        .filter(|capture| capture_index < found.capture_count && capture.set);
                    let Some(capture) = capture else {
                        return Err(RuntimeError::UnsupportedLibraryFeature {
                            function: "string.gsub",
                            feature: "invalid replacement capture reference",
                        });
                    };
                    if capture.position {
                        append_usize_decimal(result, capture.start + 1)?;
                    } else {
                        append_limited_string(result, &haystack[capture.start..capture.end])?;
                    }
                }
            }
            _ => {
                if profile == SemanticProfile::Lua51 {
                    append_limited_string(result, &replacement[index + 1..index + 2])?;
                } else {
                    return Err(RuntimeError::UnsupportedLibraryFeature {
                        function: "string.gsub",
                        feature: "nonportable replacement escapes",
                    });
                }
            }
        }
        index += 2;
    }
    Ok(())
}

fn append_usize_decimal(result: &mut Vec<u8>, mut value: usize) -> Result<(), RuntimeError> {
    let mut digits = [0_u8; 40];
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    append_limited_string(result, &digits[start..])
}

fn try_concat_bytes(value: &Value) -> Result<Option<Cow<'_, [u8]>>, RuntimeError> {
    match value {
        Value::String(value) => Ok(Some(Cow::Borrowed(value))),
        Value::Integer(_) | Value::Number(_) => {
            let mut rendered =
                try_vec_with_capacity(rendered_value_len(value), "numeric string coercion")?;
            append_value(&mut rendered, value);
            Ok(Some(Cow::Owned(rendered)))
        }
        _ => Ok(None),
    }
}

fn try_vec_with_capacity<T>(capacity: usize, what: &'static str) -> Result<Vec<T>, RuntimeError> {
    let mut values = Vec::new();
    try_reserve_exact(&mut values, capacity, what)?;
    Ok(values)
}

fn try_reserve_exact<T>(
    values: &mut Vec<T>,
    additional: usize,
    what: &'static str,
) -> Result<(), RuntimeError> {
    values
        .try_reserve_exact(additional)
        .map_err(|_| RuntimeError::Allocation { what })
}

fn try_clone_values(values: &[Value], what: &'static str) -> Result<Vec<Value>, RuntimeError> {
    try_clone_slice(values, what)
}

fn append_blu_varargs(
    prefix: &[Value],
    varargs: &[Value],
    what: &'static str,
) -> Result<Vec<Value>, RuntimeError> {
    let required = prefix
        .len()
        .checked_add(varargs.len())
        .ok_or(RuntimeError::StackLimit {
            required: usize::MAX,
            limit: MAX_DYNAMIC_REGISTERS,
        })?;
    if required > MAX_DYNAMIC_REGISTERS {
        return Err(RuntimeError::StackLimit {
            required,
            limit: MAX_DYNAMIC_REGISTERS,
        });
    }
    let mut arguments = try_clone_values(prefix, what)?;
    try_reserve_exact(&mut arguments, varargs.len(), what)?;
    arguments.extend(varargs.iter().cloned());
    Ok(arguments)
}

const fn metatable_loop_limit(profile: SemanticProfile) -> usize {
    match profile {
        SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55 => 2_000,
        SemanticProfile::Blu
        | SemanticProfile::Luau
        | SemanticProfile::Lua51
        | SemanticProfile::Lua52 => 100,
        _ => 100,
    }
}

fn try_clone_callers(callers: &[Caller]) -> Result<Vec<Caller>, RuntimeError> {
    let mut cloned = try_vec_with_capacity(callers.len(), "resumed caller stack")?;
    for caller in callers {
        cloned.push(caller.try_clone_for_unwind()?);
    }
    Ok(cloned)
}

fn try_clone_slice<T: Clone>(values: &[T], what: &'static str) -> Result<Vec<T>, RuntimeError> {
    let mut cloned = try_vec_with_capacity(values.len(), what)?;
    cloned.extend_from_slice(values);
    Ok(cloned)
}

fn try_prefixed_values(
    prefix: Value,
    values: &[Value],
    what: &'static str,
) -> Result<Vec<Value>, RuntimeError> {
    let capacity = values
        .len()
        .checked_add(1)
        .ok_or(RuntimeError::Allocation { what })?;
    let mut prefixed = try_vec_with_capacity(capacity, what)?;
    prefixed.push(prefix);
    prefixed.extend_from_slice(values);
    Ok(prefixed)
}

fn try_prepend_value(
    mut values: Vec<Value>,
    prefix: Value,
    what: &'static str,
) -> Result<Vec<Value>, RuntimeError> {
    try_reserve_exact(&mut values, 1, what)?;
    values.insert(0, prefix);
    Ok(values)
}

fn try_clone_bytes(bytes: &[u8], what: &'static str) -> Result<Vec<u8>, RuntimeError> {
    let mut result = try_vec_with_capacity(bytes.len(), what)?;
    result.extend_from_slice(bytes);
    Ok(result)
}

fn runtime_error_value(error: RuntimeError) -> Value {
    match error {
        RuntimeError::Raised(value) => value,
        error => Value::String(Arc::from(error.to_string().into_bytes())),
    }
}

fn append_value(output: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Nil => output.extend_from_slice(b"nil"),
        Value::Boolean(true) => output.extend_from_slice(b"true"),
        Value::Boolean(false) => output.extend_from_slice(b"false"),
        Value::Number(value) => {
            write!(&mut ByteWriter(output), "{value}").expect("writing to bytes cannot fail");
        }
        Value::Integer(value) => {
            write!(&mut ByteWriter(output), "{value}").expect("writing to bytes cannot fail");
        }
        Value::String(value) => output.extend_from_slice(value),
        Value::Table(value) => {
            write!(&mut ByteWriter(output), "{value:?}").expect("writing to bytes cannot fail");
        }
        Value::Closure(value) => {
            write!(&mut ByteWriter(output), "{value:?}").expect("writing to bytes cannot fail");
        }
        Value::Thread(value) => {
            write!(&mut ByteWriter(output), "{value:?}").expect("writing to bytes cannot fail");
        }
        Value::CoroutineFunction(value) => {
            write!(&mut ByteWriter(output), "CoroutineFunction({value:?})")
                .expect("writing to bytes cannot fail");
        }
        Value::NativeFunction(value) => {
            write!(&mut ByteWriter(output), "{value:?}").expect("writing to bytes cannot fail");
        }
    }
}

fn rendered_value_len(value: &Value) -> usize {
    match value {
        Value::Nil => 3,
        Value::Boolean(true) => 4,
        Value::Boolean(false) => 5,
        Value::Number(value) => formatted_len(format_args!("{value}")),
        Value::Integer(value) => formatted_len(format_args!("{value}")),
        Value::String(value) => value.len(),
        Value::Table(value) => formatted_len(format_args!("{value:?}")),
        Value::Closure(value) => formatted_len(format_args!("{value:?}")),
        Value::Thread(value) => formatted_len(format_args!("{value:?}")),
        Value::CoroutineFunction(value) => {
            formatted_len(format_args!("CoroutineFunction({value:?})"))
        }
        Value::NativeFunction(value) => formatted_len(format_args!("{value:?}")),
    }
}

struct ByteWriter<'a>(&'a mut Vec<u8>);

impl fmt::Write for ByteWriter<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.0.extend_from_slice(value.as_bytes());
        Ok(())
    }
}

#[derive(Default)]
struct LengthWriter {
    length: usize,
}

impl fmt::Write for LengthWriter {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.length = self
            .length
            .checked_add(value.len())
            .expect("a formatted runtime value cannot exceed usize");
        Ok(())
    }
}

fn formatted_len(arguments: fmt::Arguments<'_>) -> usize {
    let mut writer = LengthWriter::default();
    fmt::write(&mut writer, arguments).expect("counting formatted bytes cannot fail");
    writer.length
}

impl fmt::Debug for Vm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vm")
            .field("dialect", &self.dialect)
            .field("instruction_limit", &self.instruction_limit)
            .field("call_limit", &self.call_limit)
            .field("heap_object_limit", &self.heap_object_limit)
            .field("output_limit", &self.output_limit)
            .field("heap", &self.heap)
            .field("globals", &self.globals)
            .field("native_function_count", &self.native_functions.len())
            .field("protected_call", &self.protected_call)
            .field("error_handler_call", &self.error_handler_call)
            .field("has_module_loader", &self.module_loader.is_some())
            .field("module_cache", &self.module_cache)
            .field("active_frame_count", &self.active_roots.len())
            .field("retained_value_count", &self.host_root_count)
            .field("host_value_limit", &self.host_value_limit)
            .field("native_result_limit", &self.native_result_limit)
            .finish_non_exhaustive()
    }
}

struct CallContext<'a> {
    remaining: &'a mut u64,
    depth: usize,
    roots: GcRoots,
}

impl<'a> CallContext<'a> {
    fn new(remaining: &'a mut u64, depth: usize, roots: GcRoots) -> Self {
        Self {
            remaining,
            depth,
            roots,
        }
    }
}

#[derive(Clone, Debug)]
struct Continuation {
    frame: Frame,
    callers: Vec<Caller>,
    register: u8,
    encoded_count: u8,
    depth: usize,
}

#[derive(Clone, Debug)]
struct Caller {
    frame: Frame,
    register: u8,
    encoded_count: u8,
    return_mode: ReturnMode,
}

impl Caller {
    fn try_clone_for_unwind(&self) -> Result<Self, RuntimeError> {
        Ok(Self {
            frame: self.frame.try_clone_for_unwind()?,
            register: self.register,
            encoded_count: self.encoded_count,
            return_mode: self.return_mode.clone(),
        })
    }

    fn gc_roots(&self, heap: &Heap) -> Result<GcRoots, RuntimeError> {
        let mut roots = self.frame.gc_roots(heap)?;
        if let ReturnMode::Operation(operation) = &self.return_mode {
            try_reserve_exact(&mut roots.values, 3, "pending operation GC roots")?;
            roots.values.extend(operation.values().into_iter().cloned());
        }
        Ok(roots)
    }

    fn complete_success(mut self, heap: &Heap, results: Vec<Value>) -> Result<Frame, RuntimeError> {
        self.frame.refresh_open_upvalues(heap)?;
        match self.return_mode {
            ReturnMode::Operation(operation) => operation.complete(&mut self.frame, results)?,
            return_mode => {
                let results = return_mode.success_results(results)?;
                self.frame
                    .write_results(self.register, self.encoded_count, results)?;
            }
        }
        Ok(self.frame)
    }
}

#[derive(Clone, Debug)]
enum ReturnMode {
    Direct,
    Protected,
    ErrorHandler(Value),
    ErrorHandlerResult,
    Operation(PendingOperation),
}

impl ReturnMode {
    fn catches_errors(&self) -> bool {
        matches!(self, Self::Protected | Self::ErrorHandler(_))
    }

    fn success_results(&self, results: Vec<Value>) -> Result<Vec<Value>, RuntimeError> {
        match self {
            Self::Direct => Ok(results),
            Self::Protected | Self::ErrorHandler(_) => {
                try_prepend_value(results, Value::Boolean(true), "protected call results")
            }
            Self::ErrorHandlerResult => Ok(vec![
                Value::Boolean(false),
                results.into_iter().next().unwrap_or(Value::Nil),
            ]),
            Self::Operation(_) => unreachable!("operations complete through Caller"),
        }
    }
}

#[derive(Clone, Debug)]
enum PendingOperation {
    GenericForStep {
        function: Value,
        state: Value,
        control: Value,
        base: u8,
        variable_count: usize,
        instruction: Instruction,
    },
}

impl PendingOperation {
    fn values(&self) -> [&Value; 3] {
        match self {
            Self::GenericForStep {
                function,
                state,
                control,
                ..
            } => [function, state, control],
        }
    }

    fn complete(self, frame: &mut Frame, results: Vec<Value>) -> Result<(), RuntimeError> {
        match self {
            Self::GenericForStep {
                base,
                variable_count,
                instruction,
                ..
            } => {
                for offset in 0..variable_count {
                    let register = usize::from(base) + 3 + offset;
                    let register = u8::try_from(register).map_err(|_| RuntimeError::Register {
                        register,
                        count: frame.registers.len(),
                    })?;
                    frame.set(register, results.get(offset).cloned().unwrap_or(Value::Nil))?;
                }
                let index_register = base.checked_add(2).ok_or(RuntimeError::Register {
                    register: usize::from(base) + 2,
                    count: frame.registers.len(),
                })?;
                let first = results.first().cloned().unwrap_or(Value::Nil);
                frame.set(index_register, first.clone())?;
                if !matches!(first, Value::Nil) {
                    frame.jump(instruction)?;
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct Frame {
    chunk: Arc<Chunk>,
    prototype_index: usize,
    profile: SemanticProfile,
    constants: Vec<Value>,
    registers: Vec<Value>,
    varargs: Vec<Value>,
    closure: Option<ClosureId>,
    open_upvalues: HashMap<u8, UpvalueId>,
    open_upvalues_dirty: bool,
    pc: usize,
    top: usize,
}

impl Frame {
    fn new(
        chunk: Arc<Chunk>,
        prototype_index: usize,
        profile: SemanticProfile,
        constants: Vec<Value>,
        closure: Option<ClosureId>,
        arguments: &[Value],
    ) -> Result<Self, RuntimeError> {
        let prototype = chunk
            .prototypes
            .get(prototype_index)
            .ok_or(RuntimeError::InvalidPrototype(prototype_index))?;
        let register_count = usize::from(prototype.max_stack_size);
        let mut registers = try_vec_with_capacity(register_count, "VM frame registers")?;
        registers.resize(register_count, Value::Nil);
        let parameter_count = usize::from(prototype.parameter_count);
        let copied = arguments.len().min(parameter_count).min(registers.len());
        registers[..copied].clone_from_slice(&arguments[..copied]);
        let varargs = try_clone_values(
            arguments.get(parameter_count..).unwrap_or_default(),
            "VM frame varargs",
        )?;
        Ok(Self {
            chunk,
            prototype_index,
            profile,
            constants,
            registers,
            varargs,
            closure,
            open_upvalues: HashMap::new(),
            open_upvalues_dirty: false,
            pc: 0,
            top: copied,
        })
    }

    fn try_clone_for_unwind(&self) -> Result<Self, RuntimeError> {
        let mut open_upvalues = HashMap::new();
        open_upvalues
            .try_reserve(self.open_upvalues.len())
            .map_err(|_| RuntimeError::Allocation {
                what: "resumed frame open upvalues",
            })?;
        open_upvalues.extend(
            self.open_upvalues
                .iter()
                .map(|(&register, &upvalue)| (register, upvalue)),
        );
        Ok(Self {
            chunk: self.chunk.clone(),
            prototype_index: self.prototype_index,
            profile: self.profile,
            constants: try_clone_values(&self.constants, "resumed frame constants")?,
            registers: try_clone_values(&self.registers, "resumed frame registers")?,
            varargs: try_clone_values(&self.varargs, "resumed frame varargs")?,
            closure: self.closure,
            open_upvalues,
            open_upvalues_dirty: self.open_upvalues_dirty,
            pc: self.pc,
            top: self.top,
        })
    }

    fn prototype(&self) -> Result<&Prototype, RuntimeError> {
        self.chunk
            .prototypes
            .get(self.prototype_index)
            .ok_or(RuntimeError::InvalidPrototype(self.prototype_index))
    }

    fn instruction(&self) -> Result<Instruction, RuntimeError> {
        let prototype = self.prototype()?;
        prototype
            .instructions
            .binary_search_by_key(&self.pc, |instruction| instruction.pc())
            .ok()
            .map(|index| prototype.instructions[index])
            .ok_or(RuntimeError::InvalidProgramCounter {
                pc: self.pc,
                code_words: prototype.code.len(),
            })
    }

    fn get(&self, register: u8) -> Result<&Value, RuntimeError> {
        self.registers
            .get(usize::from(register))
            .ok_or(RuntimeError::Register {
                register: usize::from(register),
                count: self.registers.len(),
            })
    }

    fn set(&mut self, register: u8, value: Value) -> Result<(), RuntimeError> {
        let register = usize::from(register);
        let count = self.registers.len();
        let slot = self
            .registers
            .get_mut(register)
            .ok_or(RuntimeError::Register { register, count })?;
        *slot = value;
        self.top = self.top.max(register + 1);
        if self.open_upvalues.contains_key(&(register as u8)) {
            self.open_upvalues_dirty = true;
        }
        Ok(())
    }

    fn register_slice(&self, start: usize, count: usize) -> Result<&[Value], RuntimeError> {
        let end = start.checked_add(count).ok_or(RuntimeError::Register {
            register: usize::MAX,
            count: self.registers.len(),
        })?;
        self.registers
            .get(start..end)
            .ok_or(RuntimeError::Register {
                register: end,
                count: self.registers.len(),
            })
    }

    fn write_results(
        &mut self,
        register: u8,
        encoded_count: u8,
        results: Vec<Value>,
    ) -> Result<(), RuntimeError> {
        let count = if encoded_count == 0 {
            results.len()
        } else {
            usize::from(encoded_count - 1)
        };
        let start = usize::from(register);
        let required = start.checked_add(count).ok_or(RuntimeError::Register {
            register: usize::MAX,
            count: self.registers.len(),
        })?;
        if encoded_count != 0 && required > self.registers.len() {
            return Err(RuntimeError::Register {
                register: required.saturating_sub(1),
                count: self.registers.len(),
            });
        }
        if encoded_count == 0 {
            self.ensure_dynamic(required)?;
        }
        for offset in 0..count {
            self.registers[start + offset] = results.get(offset).cloned().unwrap_or(Value::Nil);
        }
        if self.open_upvalues.keys().any(|register| {
            let register = usize::from(*register);
            start <= register && register < required
        }) {
            self.open_upvalues_dirty = true;
        }
        self.top = self.top.max(required);
        if encoded_count == 0 {
            self.top = required;
        }
        Ok(())
    }

    fn ensure_dynamic(&mut self, required: usize) -> Result<(), RuntimeError> {
        if required > MAX_DYNAMIC_REGISTERS {
            return Err(RuntimeError::StackLimit {
                required,
                limit: MAX_DYNAMIC_REGISTERS,
            });
        }
        if required > self.registers.len() {
            let additional = required - self.registers.len();
            try_reserve_exact(&mut self.registers, additional, "VM stack")?;
            self.registers.resize(required, Value::Nil);
        }
        Ok(())
    }

    fn upvalue(&self, heap: &Heap, index: u8) -> Result<UpvalueId, RuntimeError> {
        let closure = self.closure.ok_or(RuntimeError::MissingClosure)?;
        let (_, _, _, upvalues) = heap.closure_parts(closure)?;
        upvalues
            .get(index as usize)
            .copied()
            .ok_or(RuntimeError::Upvalue {
                upvalue: index as usize,
                count: upvalues.len(),
            })
    }

    fn open_upvalue(&self, register: u8) -> Option<UpvalueId> {
        self.open_upvalues.get(&register).copied()
    }

    fn insert_open_upvalue(
        &mut self,
        register: u8,
        upvalue: UpvalueId,
    ) -> Result<(), RuntimeError> {
        if !self.open_upvalues.contains_key(&register) {
            self.open_upvalues
                .try_reserve(1)
                .map_err(|_| RuntimeError::Allocation {
                    what: "frame open upvalues",
                })?;
        }
        self.open_upvalues.insert(register, upvalue);
        Ok(())
    }

    fn sync_open_upvalues(&mut self, heap: &mut Heap) -> Result<(), RuntimeError> {
        if !self.open_upvalues_dirty {
            return Ok(());
        }
        for (&register, &upvalue) in &self.open_upvalues {
            heap.upvalue_set(upvalue, self.get(register)?.clone())?;
        }
        self.open_upvalues_dirty = false;
        Ok(())
    }

    fn refresh_open_upvalues(&mut self, heap: &Heap) -> Result<(), RuntimeError> {
        for (&register, &upvalue) in &self.open_upvalues {
            self.registers[register as usize] = heap.upvalue_get(upvalue)?;
        }
        self.open_upvalues_dirty = false;
        Ok(())
    }

    fn close_upvalues(&mut self, heap: &mut Heap, from: u8) -> Result<(), RuntimeError> {
        self.sync_open_upvalues(heap)?;
        self.open_upvalues.retain(|register, _| *register < from);
        Ok(())
    }

    fn gc_roots(&self, heap: &Heap) -> Result<GcRoots, RuntimeError> {
        let capacity = self
            .constants
            .len()
            .checked_add(self.registers.len())
            .and_then(|capacity| capacity.checked_add(self.varargs.len()))
            .and_then(|capacity| capacity.checked_add(self.open_upvalues.len()))
            .and_then(|capacity| capacity.checked_add(usize::from(self.closure.is_some())))
            .ok_or(RuntimeError::Allocation {
                what: "frame GC roots",
            })?;
        let mut values = try_vec_with_capacity(capacity, "frame GC roots")?;
        values.extend(self.constants.iter().cloned());
        values.extend(self.registers.iter().cloned());
        values.extend(self.varargs.iter().cloned());
        if let Some(closure) = self.closure {
            values.push(Value::Closure(closure));
        }
        for upvalue in self.open_upvalues.values() {
            values.push(heap.upvalue_get(*upvalue)?);
        }
        let mut upvalues =
            try_vec_with_capacity(self.open_upvalues.len(), "frame GC upvalue roots")?;
        upvalues.extend(self.open_upvalues.values().copied());
        Ok(GcRoots { values, upvalues })
    }

    fn constant(&self, index: i32) -> Result<Value, RuntimeError> {
        let index = usize::try_from(index).map_err(|_| RuntimeError::Constant {
            constant: usize::MAX,
            count: self.constants.len(),
        })?;
        self.constants
            .get(index)
            .cloned()
            .ok_or(RuntimeError::Constant {
                constant: index,
                count: self.constants.len(),
            })
    }

    fn constant_u32(&self, index: u32) -> Result<Value, RuntimeError> {
        self.constants
            .get(index as usize)
            .cloned()
            .ok_or(RuntimeError::Constant {
                constant: index as usize,
                count: self.constants.len(),
            })
    }

    fn jump(&mut self, instruction: Instruction) -> Result<(), RuntimeError> {
        let target = instruction.jump_target();
        let prototype = self.prototype()?;
        let valid = target.is_some_and(|target| {
            prototype
                .instructions
                .binary_search_by_key(&target, |candidate| candidate.pc())
                .is_ok()
        });
        if !valid {
            return Err(RuntimeError::InvalidJump {
                pc: instruction.pc(),
                target,
            });
        }
        self.pc = target.unwrap();
        Ok(())
    }
}

fn continuation_roots(
    frame: &Frame,
    callers: &[Caller],
    heap: &Heap,
) -> Result<GcRoots, RuntimeError> {
    let mut roots = frame.gc_roots(heap)?;
    for caller in callers {
        roots.extend(caller.gc_roots(heap)?)?;
    }
    Ok(roots)
}

fn materialize_constants(chunk: &Chunk, prototype: &Prototype) -> Result<Vec<Value>, RuntimeError> {
    let mut values = try_vec_with_capacity(prototype.constants.len(), "VM frame constants")?;
    for (index, constant) in prototype.constants.iter().enumerate() {
        let value = match constant {
            Constant::Nil => Ok(Value::Nil),
            Constant::Boolean(value) => Ok(Value::Boolean(*value)),
            Constant::Number(value) => Ok(Value::Number(*value)),
            Constant::Integer(value) => Ok(Value::Integer(*value)),
            Constant::String(index) => chunk
                .strings
                .get(*index)
                .cloned()
                .map(Arc::<[u8]>::from)
                .map(Value::String)
                .ok_or(RuntimeError::String {
                    string: *index,
                    count: chunk.strings.len(),
                }),
            Constant::Import(_)
            | Constant::Table(_)
            | Constant::TableWithConstants(_)
            | Constant::Closure(_) => Ok(Value::Nil),
            _ => Err(RuntimeError::UnsupportedConstant { constant: index }),
        }?;
        values.push(value);
    }
    Ok(values)
}

fn materialize_constant(
    chunk: &Chunk,
    prototype: &Prototype,
    index: usize,
) -> Result<Value, RuntimeError> {
    match prototype.constants.get(index) {
        Some(Constant::Nil) => Ok(Value::Nil),
        Some(Constant::Boolean(value)) => Ok(Value::Boolean(*value)),
        Some(Constant::Number(value)) => Ok(Value::Number(*value)),
        Some(Constant::Integer(value)) => Ok(Value::Integer(*value)),
        Some(Constant::String(string)) => chunk
            .strings
            .get(*string)
            .cloned()
            .map(Arc::<[u8]>::from)
            .map(Value::String)
            .ok_or(RuntimeError::String {
                string: *string,
                count: chunk.strings.len(),
            }),
        _ => Err(RuntimeError::UnsupportedConstant { constant: index }),
    }
}

fn table_id(value: &Value) -> Result<TableId, RuntimeError> {
    match value {
        Value::Table(table) => Ok(*table),
        other => Err(RuntimeError::Type {
            operation: "table access",
            expected: "table",
            actual: other.type_name(),
        }),
    }
}

fn table_string_constant(instruction: Instruction) -> Result<u32, RuntimeError> {
    let aux = instruction.aux().ok_or(RuntimeError::MissingAux {
        pc: instruction.pc(),
        opcode: instruction.opcode(),
    })?;
    Ok(
        if matches!(
            instruction.opcode(),
            Opcode::GetUdataKs | Opcode::SetUdataKs
        ) {
            aux & 0xffff
        } else {
            aux
        },
    )
}

fn blu_v1_execution_bytes(prototype: &blu_bytecode::blu::Prototype) -> Result<usize, RuntimeError> {
    let runtime_memory_error = |error| RuntimeError::from(HeapError::Memory(error));
    let constant_bytes =
        checked_vector_bytes::<Value>(prototype.constants.len()).map_err(runtime_memory_error)?;
    let register_bytes = checked_vector_bytes::<Value>(usize::from(prototype.register_count))
        .map_err(runtime_memory_error)?;
    let return_capacity = prototype
        .code
        .iter()
        .filter_map(|instruction| match instruction {
            BluInstruction::Return { count, .. } => Some(usize::from(*count)),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let return_bytes =
        checked_vector_bytes::<Value>(return_capacity).map_err(runtime_memory_error)?;
    let string_bytes = prototype
        .constants
        .iter()
        .try_fold(0usize, |total, constant| {
            let bytes = match constant {
                BluConstant::String(bytes) => bytes.len(),
                _ => 0,
            };
            total.checked_add(bytes).ok_or(MemoryError::SizeOverflow)
        })
        .map_err(runtime_memory_error)?;
    constant_bytes
        .checked_add(register_bytes)
        .and_then(|total| total.checked_add(return_bytes))
        .and_then(|total| total.checked_add(string_bytes))
        .ok_or_else(|| RuntimeError::from(HeapError::Memory(MemoryError::SizeOverflow)))
}

fn materialize_blu_constants(
    prototype: &blu_bytecode::blu::Prototype,
) -> Result<Vec<Value>, RuntimeError> {
    let mut constants =
        try_vec_with_capacity(prototype.constants.len(), "BluV1 runtime constants")?;
    for (index, constant) in prototype.constants.iter().enumerate() {
        constants.push(match constant {
            BluConstant::Nil => Value::Nil,
            BluConstant::Boolean(value) => Value::Boolean(*value),
            BluConstant::Number(value) => Value::Number(*value),
            BluConstant::Integer(value)
                if matches!(
                    prototype.profile,
                    SemanticProfile::Blu
                        | SemanticProfile::Lua53
                        | SemanticProfile::Lua54
                        | SemanticProfile::Lua55
                ) =>
            {
                Value::Integer(*value)
            }
            BluConstant::Integer(_) => {
                return Err(RuntimeError::UnsupportedBluV1Constant {
                    profile: prototype.profile,
                    constant: index,
                    kind: "integer",
                });
            }
            BluConstant::String(value) => Value::String(Arc::from(value.as_slice())),
        });
    }
    Ok(constants)
}

fn blu_frame_roots(
    registers: &[Value],
    varargs: &[Value],
    open_upvalues: &[Option<UpvalueId>],
    closure: Option<ClosureId>,
    callers: &[BluCaller],
) -> Result<GcRoots, RuntimeError> {
    let mut roots = GcRoots::from_values(registers)?;
    try_reserve_exact(&mut roots.values, varargs.len(), "BluV1 vararg GC roots")?;
    roots.values.extend(varargs.iter().cloned());
    try_reserve_exact(
        &mut roots.upvalues,
        open_upvalues.iter().flatten().count(),
        "BluV1 open upvalue roots",
    )?;
    roots
        .upvalues
        .extend(open_upvalues.iter().copied().flatten());
    if let Some(closure) = closure {
        roots.push_value(Value::Closure(closure))?;
    }
    for caller in callers {
        let caller_values = try_clone_values(&caller.registers, "BluV1 caller GC roots")?;
        try_reserve_exact(
            &mut roots.values,
            caller_values.len(),
            "BluV1 caller GC roots",
        )?;
        roots.values.extend(caller_values);
        try_reserve_exact(
            &mut roots.values,
            caller.varargs.len(),
            "BluV1 caller vararg GC roots",
        )?;
        roots.values.extend(caller.varargs.iter().cloned());
        try_reserve_exact(
            &mut roots.upvalues,
            caller.open_upvalues.iter().flatten().count(),
            "BluV1 caller upvalue roots",
        )?;
        roots
            .upvalues
            .extend(caller.open_upvalues.iter().copied().flatten());
        if let Some(closure) = caller.closure {
            roots.push_value(Value::Closure(closure))?;
        }
    }
    Ok(roots)
}

fn refresh_blu_open_upvalues(
    heap: &Heap,
    registers: &mut [Value],
    open_upvalues: &[Option<UpvalueId>],
) -> Result<(), RuntimeError> {
    for (register, upvalue) in open_upvalues.iter().copied().enumerate() {
        if let Some(upvalue) = upvalue {
            registers[register] = heap.upvalue_get(upvalue)?;
        }
    }
    Ok(())
}

fn blu_register(registers: &[Value], register: u16) -> Result<&Value, RuntimeError> {
    registers
        .get(usize::from(register))
        .ok_or(RuntimeError::Register {
            register: usize::from(register),
            count: registers.len(),
        })
}

fn set_blu_register(
    heap: &mut Heap,
    registers: &mut [Value],
    open_upvalues: &[Option<UpvalueId>],
    register: u16,
    value: Value,
) -> Result<(), RuntimeError> {
    let count = registers.len();
    let slot = registers
        .get_mut(usize::from(register))
        .ok_or(RuntimeError::Register {
            register: usize::from(register),
            count,
        })?;
    *slot = value.clone();
    if let Some(upvalue) = open_upvalues.get(usize::from(register)).copied().flatten() {
        heap.upvalue_set(upvalue, value)?;
    }
    Ok(())
}

fn arithmetic(opcode: Opcode, left: &Value, right: &Value) -> Result<Value, RuntimeError> {
    if let (Value::Integer(left), Value::Integer(right)) = (left, right) {
        return match opcode {
            Opcode::Add | Opcode::AddK => Ok(Value::Integer(left.wrapping_add(*right))),
            Opcode::Sub | Opcode::SubK => Ok(Value::Integer(left.wrapping_sub(*right))),
            Opcode::SubRk => Ok(Value::Integer(left.wrapping_sub(*right))),
            Opcode::Mul | Opcode::MulK => Ok(Value::Integer(left.wrapping_mul(*right))),
            Opcode::IDiv | Opcode::IDivK => integer_floor_div(*left, *right).map(Value::Integer),
            _ => numeric_arithmetic(opcode, *left as f64, *right as f64),
        };
    }
    let left = left.as_number().ok_or(RuntimeError::Type {
        operation: "arithmetic",
        expected: "number",
        actual: left.type_name(),
    })?;
    let right = right.as_number().ok_or(RuntimeError::Type {
        operation: "arithmetic",
        expected: "number",
        actual: right.type_name(),
    })?;
    numeric_arithmetic(opcode, left, right)
}

fn arithmetic_numeric_value(value: &Value, profile: SemanticProfile) -> Option<Value> {
    match value {
        Value::Integer(_) | Value::Number(_) => Some(value.clone()),
        Value::String(bytes) => {
            let parsed = parse_default_number(trim_ascii_bytes(bytes), profile)?;
            match (profile, parsed) {
                (SemanticProfile::Lua53, Value::Integer(value)) => {
                    Some(Value::Number(value as f64))
                }
                (_, parsed) => Some(parsed),
            }
        }
        _ => None,
    }
}

fn numeric_arithmetic(opcode: Opcode, left: f64, right: f64) -> Result<Value, RuntimeError> {
    let value = match opcode {
        Opcode::Add | Opcode::AddK => left + right,
        Opcode::Sub | Opcode::SubK | Opcode::SubRk => left - right,
        Opcode::Mul | Opcode::MulK => left * right,
        Opcode::Div | Opcode::DivK | Opcode::DivRk => left / right,
        Opcode::Mod | Opcode::ModK => left - (left / right).floor() * right,
        Opcode::Pow | Opcode::PowK => left.powf(right),
        Opcode::IDiv | Opcode::IDivK => (left / right).floor(),
        _ => return Err(RuntimeError::UnsupportedArithmetic(opcode)),
    };
    Ok(Value::Number(value))
}

fn integer_floor_div(left: i64, right: i64) -> Result<i64, RuntimeError> {
    if right == 0 {
        return Err(RuntimeError::DivideByZero);
    }
    if left == i64::MIN && right == -1 {
        return Ok(i64::MIN);
    }
    let quotient = left / right;
    let remainder = left % right;
    Ok(if remainder != 0 && (remainder < 0) != (right < 0) {
        quotient - 1
    } else {
        quotient
    })
}

fn integer_floor_mod(left: i64, right: i64) -> Result<i64, RuntimeError> {
    let quotient = integer_floor_div(left, right)?;
    Ok(left.wrapping_sub(quotient.wrapping_mul(right)))
}

#[derive(Clone, Debug, PartialEq)]
pub enum RuntimeError {
    Validation(ValidationError),
    BluValidation(BluValidationError),
    Allocation {
        what: &'static str,
    },
    DialectNotImplemented(Dialect),
    SemanticProfileNotImplemented(SemanticProfile),
    UnsupportedBluV1Structure {
        what: &'static str,
    },
    UnsupportedBluV1Constant {
        profile: SemanticProfile,
        constant: usize,
        kind: &'static str,
    },
    SemanticProfileMismatch {
        configured: Dialect,
        artifact: SemanticProfile,
    },
    InvalidMainPrototype(usize),
    InvalidPrototype(usize),
    InvalidProgramCounter {
        pc: usize,
        code_words: usize,
    },
    InvalidJump {
        pc: usize,
        target: Option<usize>,
    },
    Register {
        register: usize,
        count: usize,
    },
    Constant {
        constant: usize,
        count: usize,
    },
    String {
        string: usize,
        count: usize,
    },
    Upvalue {
        upvalue: usize,
        count: usize,
    },
    MissingClosure,
    MissingCapture {
        pc: usize,
        capture: u8,
        expected: u8,
    },
    UnexpectedCapture {
        pc: usize,
    },
    CaptureType {
        pc: usize,
        kind: u8,
    },
    MissingAux {
        pc: usize,
        opcode: Opcode,
    },
    UnsupportedOpcode {
        pc: usize,
        opcode: Opcode,
    },
    UnsupportedConstant {
        constant: usize,
    },
    UnsupportedArithmetic(Opcode),
    UnsupportedComparison(Opcode),
    UnsupportedSemanticProfile {
        operation: &'static str,
        profile: SemanticProfile,
    },
    UnsupportedLibraryFeature {
        function: &'static str,
        feature: &'static str,
    },
    PatternWorkLimit {
        required: usize,
        limit: usize,
    },
    Type {
        operation: &'static str,
        expected: &'static str,
        actual: &'static str,
    },
    InvalidRange {
        operation: &'static str,
    },
    NativeFunction(u32),
    Argument {
        function: &'static str,
        index: usize,
    },
    ArgumentCount {
        function: &'static str,
        expected: &'static str,
        actual: usize,
    },
    Heap(HeapError),
    DivideByZero,
    Breakpoint {
        pc: usize,
    },
    InstructionLimit {
        limit: u64,
    },
    CallLimit {
        limit: usize,
    },
    StackLimit {
        required: usize,
        limit: usize,
    },
    HeapObjectLimit {
        required: usize,
        limit: usize,
    },
    HostValueLimit {
        required: usize,
        limit: usize,
    },
    NativeResultLimit {
        required: usize,
        limit: usize,
    },
    NativeFunctionLimit {
        required: usize,
        limit: usize,
    },
    GlobalLimit {
        required: usize,
        limit: usize,
    },
    MetatableProtected,
    MetatableLoop,
    UnsupportedMetamethod {
        name: &'static str,
        actual: &'static str,
    },
    Raised(Value),
    SelectIndex(i64),
    TablePosition {
        function: &'static str,
        position: i64,
        length: usize,
    },
    ConversionBase(u32),
    StringByte {
        index: usize,
        value: i64,
    },
    StringLimit {
        required: usize,
        limit: usize,
    },
    OutputLimit {
        required: usize,
        limit: usize,
    },
    TableCapacity {
        kind: &'static str,
        requested: u64,
        limit: usize,
    },
    CoroutineYield(Vec<Value>),
    CoroutineYieldOutside,
    ModuleLoaderMissing,
    CircularModule(Arc<[u8]>),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(error) => write!(f, "bytecode validation failed: {error}"),
            Self::BluValidation(error) => write!(f, "BluV1 validation failed: {error}"),
            Self::Allocation { what } => write!(f, "{what} allocation failed"),
            Self::DialectNotImplemented(dialect) => {
                write!(f, "{dialect:?} execution is not implemented")
            }
            Self::SemanticProfileNotImplemented(profile) => {
                write!(f, "{profile} execution is not implemented")
            }
            Self::UnsupportedBluV1Structure { what } => {
                write!(f, "BluV1 execution does not support {what}")
            }
            Self::UnsupportedBluV1Constant {
                profile,
                constant,
                kind,
            } => write!(
                f,
                "BluV1 {profile} constant {constant} uses unsupported {kind} semantics"
            ),
            Self::SemanticProfileMismatch {
                configured,
                artifact,
            } => write!(
                f,
                "translated {artifact} artifact cannot execute in {configured:?} mode"
            ),
            Self::InvalidMainPrototype(index) => write!(f, "invalid main prototype {index}"),
            Self::InvalidPrototype(index) => write!(f, "invalid prototype {index}"),
            Self::InvalidProgramCounter { pc, code_words } => {
                write!(f, "program counter {pc} is invalid for {code_words} words")
            }
            Self::InvalidJump { pc, target } => {
                write!(f, "invalid jump from word {pc} to {target:?}")
            }
            Self::Register { register, count } => {
                write!(f, "register {register} is invalid for frame size {count}")
            }
            Self::Constant { constant, count } => {
                write!(f, "constant {constant} is invalid for table size {count}")
            }
            Self::String { string, count } => {
                write!(f, "string {string} is invalid for table size {count}")
            }
            Self::Upvalue { upvalue, count } => {
                write!(f, "upvalue {upvalue} is invalid for closure size {count}")
            }
            Self::MissingClosure => f.write_str("frame has no closure for upvalue access"),
            Self::MissingCapture {
                pc,
                capture,
                expected,
            } => write!(
                f,
                "closure at word {pc} is missing capture {} of {expected}",
                capture + 1
            ),
            Self::UnexpectedCapture { pc } => {
                write!(f, "CAPTURE at word {pc} is not attached to a closure")
            }
            Self::CaptureType { pc, kind } => {
                write!(f, "CAPTURE at word {pc} has invalid type {kind}")
            }
            Self::MissingAux { pc, opcode } => {
                write!(f, "{opcode} at word {pc} is missing auxiliary data")
            }
            Self::UnsupportedOpcode { pc, opcode } => {
                write!(f, "{opcode} at word {pc} is not implemented")
            }
            Self::UnsupportedConstant { constant } => {
                write!(f, "constant {constant} requires an unimplemented heap type")
            }
            Self::UnsupportedArithmetic(opcode) => {
                write!(f, "{opcode} arithmetic is not implemented")
            }
            Self::UnsupportedComparison(opcode) => {
                write!(f, "{opcode} comparison is not implemented")
            }
            Self::UnsupportedSemanticProfile { operation, profile } => {
                write!(f, "{operation} has no semantics assigned for {profile}")
            }
            Self::UnsupportedLibraryFeature { function, feature } => {
                write!(f, "{function} does not yet support {feature}")
            }
            Self::PatternWorkLimit { required, limit } => {
                write!(
                    f,
                    "pattern match requires {required} steps, limit is {limit}"
                )
            }
            Self::Type {
                operation,
                expected,
                actual,
            } => write!(f, "{operation} expected {expected}, received {actual}"),
            Self::InvalidRange { operation } => {
                write!(f, "{operation} received an invalid range")
            }
            Self::NativeFunction(index) => write!(f, "invalid native function {index}"),
            Self::Argument { function, index } => {
                write!(f, "{function} requires argument {index}")
            }
            Self::ArgumentCount {
                function,
                expected,
                actual,
            } => write!(
                f,
                "{function} expected {expected} arguments, received {actual}"
            ),
            Self::Heap(error) => error.fmt(f),
            Self::DivideByZero => f.write_str("integer divide by zero"),
            Self::Breakpoint { pc } => write!(f, "breakpoint at word {pc}"),
            Self::InstructionLimit { limit } => {
                write!(f, "instruction limit {limit} exceeded")
            }
            Self::CallLimit { limit } => write!(f, "call depth limit {limit} exceeded"),
            Self::StackLimit { required, limit } => {
                write!(
                    f,
                    "dynamic stack requires {required} values, limit is {limit}"
                )
            }
            Self::HeapObjectLimit { required, limit } => {
                write!(f, "heap requires {required} live objects, limit is {limit}")
            }
            Self::HostValueLimit { required, limit } => {
                write!(
                    f,
                    "retained host values require {required} entries, limit is {limit}"
                )
            }
            Self::NativeResultLimit { required, limit } => {
                write!(
                    f,
                    "native callback returned {required} values, limit is {limit}"
                )
            }
            Self::NativeFunctionLimit { required, limit } => {
                write!(
                    f,
                    "native function registry requires {required} entries, limit is {limit}"
                )
            }
            Self::GlobalLimit { required, limit } => {
                write!(
                    f,
                    "global registry requires {required} distinct names, limit is {limit}"
                )
            }
            Self::MetatableProtected => f.write_str("cannot change a protected metatable"),
            Self::MetatableLoop => f.write_str("metatable lookup chain is too long"),
            Self::UnsupportedMetamethod { name, actual } => {
                write!(
                    f,
                    "{name} metamethod with {actual} value is not implemented"
                )
            }
            Self::Raised(value) => write!(f, "runtime error: {value:?}"),
            Self::SelectIndex(index) => write!(f, "select index {index} is out of range"),
            Self::TablePosition {
                function,
                position,
                length,
            } => write!(
                f,
                "{function} position {position} is invalid for length {length}"
            ),
            Self::ConversionBase(base) => {
                write!(f, "tonumber base {base} is outside the range 2..=36")
            }
            Self::StringByte { index, value } => {
                write!(
                    f,
                    "string.char argument {} is outside 0..=255: {value}",
                    index + 1
                )
            }
            Self::StringLimit { required, limit } => {
                write!(
                    f,
                    "string result requires {required} bytes, limit is {limit}"
                )
            }
            Self::OutputLimit { required, limit } => {
                write!(f, "VM output requires {required} bytes, limit is {limit}")
            }
            Self::TableCapacity {
                kind,
                requested,
                limit,
            } => write!(
                f,
                "table {kind} capacity {requested} exceeds initial capacity limit {limit}"
            ),
            Self::CoroutineYield(values) => {
                write!(f, "coroutine yielded {} values", values.len())
            }
            Self::CoroutineYieldOutside => f.write_str("cannot yield outside a running coroutine"),
            Self::ModuleLoaderMissing => f.write_str("require has no configured module loader"),
            Self::CircularModule(name) => write!(
                f,
                "circular require while loading {:?}",
                String::from_utf8_lossy(name)
            ),
        }
    }
}

impl From<HeapError> for RuntimeError {
    fn from(error: HeapError) -> Self {
        Self::Heap(error)
    }
}

impl std::error::Error for RuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Validation(error) => Some(error),
            Self::BluValidation(error) => Some(error),
            Self::Heap(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blu_bytecode::{
        LoadLimits,
        blu::{
            Artifact as BluArtifact, BluLimits, BytecodeFormat, Constant as BluConstant,
            FeatureBits, Instruction as BluInstruction, Prototype as BluPrototype, SourceRecord,
            TranslatedChunk, Upvalue as BluUpvalue, ValidatedArtifact, decode_validated, encode,
            translate_baseline_to_luau,
        },
        load,
    };
    use blu_core::{
        ByteSpan, CompilerId, CompilerIdentity, IdentityLimits, SourceId, SourceIdentity,
    };

    const RETURN_THREE_V12: &[u8] = &[
        0x0c, 0x03, 0x00, 0x00, 0x01, 0x23, 0x01, 0x00, 0x00, 0x01, 0x0a, 0x00, 0x03, 0x41, 0x00,
        0x00, 0x00, 0x04, 0x00, 0x03, 0x00, 0x16, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01,
        0x18, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    fn abc(opcode: Opcode, a: u8, b: u8, c: u8) -> u32 {
        u32::from(opcode as u8) | (u32::from(a) << 8) | (u32::from(b) << 16) | (u32::from(c) << 24)
    }

    fn ad(opcode: Opcode, a: u8, d: i16) -> u32 {
        u32::from(opcode as u8) | (u32::from(a) << 8) | ((d as u16 as u32) << 16)
    }

    fn test_chunk(
        strings: &[&[u8]],
        constants: Vec<Constant>,
        code: Vec<u32>,
        max_stack_size: u8,
    ) -> Chunk {
        let mut chunk = load(RETURN_THREE_V12, LoadLimits::default()).unwrap();
        chunk.strings = strings.iter().map(|value| value.to_vec()).collect();
        let prototype = &mut chunk.prototypes[0];
        prototype.constants = constants;
        prototype.code = code;
        prototype.instructions = blu_bytecode::decode(&prototype.code).unwrap();
        prototype.max_stack_size = max_stack_size;
        prototype.parameter_count = 0;
        prototype.upvalue_count = 0;
        prototype.children.clear();
        chunk
    }

    fn validated_blu_program(
        profile: SemanticProfile,
        constants: Vec<BluConstant>,
        code: Vec<BluInstruction>,
        required_features: FeatureBits,
        register_count: u16,
    ) -> ValidatedArtifact {
        let identity_limits = IdentityLimits::default();
        let source = SourceId::new(1);
        let source_span = ByteSpan::from_usize(source, 0, 0).unwrap();
        let source_map = vec![source_span; code.len()];
        let artifact = BluArtifact {
            format: BytecodeFormat::BluV1,
            compiler: CompilerIdentity::new(
                CompilerId::new([7; 16]),
                "blu-test",
                "1",
                None,
                identity_limits,
            )
            .unwrap(),
            sources: vec![SourceRecord {
                identity: SourceIdentity::new(source, "baseline.blu", identity_limits).unwrap(),
                byte_len: 1,
                digest: [0; 32],
            }],
            prototypes: vec![BluPrototype {
                profile,
                source,
                register_count,
                parameter_count: 0,
                is_vararg: false,
                required_features,
                constants,
                upvalues: Vec::new(),
                children: Vec::new(),
                code,
                source_map,
                locals: Vec::new(),
                upvalue_debug: Vec::new(),
            }],
            main: 0,
        };
        ValidatedArtifact::new(artifact, BluLimits::default()).unwrap()
    }

    fn translated_baseline(profile: SemanticProfile) -> TranslatedChunk {
        let artifact = validated_blu_program(
            profile,
            vec![BluConstant::Number(40.0), BluConstant::Number(2.0)],
            vec![
                BluInstruction::LoadConstant {
                    destination: 0,
                    constant: 0,
                },
                BluInstruction::LoadConstant {
                    destination: 1,
                    constant: 1,
                },
                BluInstruction::Add {
                    destination: 2,
                    left: 0,
                    right: 1,
                },
                BluInstruction::Return { first: 2, count: 1 },
            ],
            FeatureBits::BASELINE,
            3,
        );
        translate_baseline_to_luau(artifact, profile, BluLimits::default()).unwrap()
    }

    fn native(vm: &Vm, table: &[u8], name: &[u8]) -> NativeFunction {
        let table = table_id(vm.global(table).unwrap()).unwrap();
        let value = vm
            .heap
            .table_get(table, &Value::String(Arc::from(name)))
            .unwrap();
        let Value::NativeFunction(id) = value else {
            panic!("native function expected, received {value:?}");
        };
        vm.native_functions[id.0 as usize].clone()
    }

    #[test]
    fn executes_real_pinned_upstream_chunk() {
        let chunk = load(RETURN_THREE_V12, LoadLimits::default()).unwrap();
        let result = Vm::default().execute(&chunk).unwrap();
        assert_eq!(result, vec![Value::Number(3.0)]);
    }

    #[test]
    fn translated_blu_v1_execution_uses_its_authorized_profile() {
        assert_eq!(
            Vm::new(Dialect::Blu).execute_translated(translated_baseline(SemanticProfile::Blu)),
            Ok(vec![Value::Number(42.0)])
        );
        assert_eq!(
            Vm::new(Dialect::Luau).execute_translated(translated_baseline(SemanticProfile::Luau)),
            Ok(vec![Value::Number(42.0)])
        );
        assert_eq!(
            Vm::new(Dialect::Luau).execute_translated(translated_baseline(SemanticProfile::Blu)),
            Ok(vec![Value::Number(42.0)])
        );
        let extracted = translated_baseline(SemanticProfile::Blu).into_validated_chunk();
        assert_eq!(
            Vm::new(Dialect::Luau).execute_validated_owned(extracted),
            Ok(vec![Value::Number(42.0)])
        );
    }

    #[test]
    fn direct_blu_v1_baseline_executes_under_every_semantic_profile() {
        for profile in SemanticProfile::ALL {
            let artifact = validated_blu_program(
                profile,
                vec![BluConstant::Number(40.0), BluConstant::Number(2.0)],
                vec![
                    BluInstruction::LoadConstant {
                        destination: 0,
                        constant: 0,
                    },
                    BluInstruction::LoadConstant {
                        destination: 1,
                        constant: 1,
                    },
                    BluInstruction::Add {
                        destination: 2,
                        left: 0,
                        right: 1,
                    },
                    BluInstruction::Return { first: 2, count: 1 },
                ],
                FeatureBits::BASELINE,
                3,
            );
            assert_eq!(
                Vm::new(Dialect::Blu).execute_blu_v1(artifact, BluLimits::default()),
                Ok(vec![Value::Number(42.0)]),
                "{profile}"
            );
        }
    }

    #[test]
    fn direct_blu_v1_closures_share_mutable_captures_on_bounded_frames() {
        let mut validated = validated_blu_program(
            SemanticProfile::Blu,
            vec![BluConstant::Number(10.0)],
            vec![
                BluInstruction::LoadConstant {
                    destination: 0,
                    constant: 0,
                },
                BluInstruction::Return { first: 0, count: 1 },
            ],
            FeatureBits::BASELINE,
            4,
        )
        .into_artifact();
        validated.prototypes[0].code = vec![
            BluInstruction::LoadConstant {
                destination: 0,
                constant: 0,
            },
            BluInstruction::NewClosure {
                destination: 1,
                child: 0,
            },
            BluInstruction::Call {
                destination: 2,
                function: 1,
                arguments: 0,
                argument_count: 0,
            },
            BluInstruction::Call {
                destination: 3,
                function: 1,
                arguments: 0,
                argument_count: 0,
            },
            BluInstruction::Return { first: 2, count: 2 },
        ];
        validated.prototypes[0].required_features =
            FeatureBits::BASELINE | FeatureBits::FIXED_CALLS | FeatureBits::CLOSURES;
        let source = validated.prototypes[0].source;
        let span = ByteSpan::from_usize(source, 0, 0).unwrap();
        validated.prototypes[0].source_map = vec![span; 5];
        validated.prototypes[0].children = vec![1];
        validated.prototypes.push(BluPrototype {
            profile: SemanticProfile::Blu,
            source,
            register_count: 2,
            parameter_count: 0,
            is_vararg: false,
            required_features: FeatureBits::BASELINE
                | FeatureBits::CLOSURES
                | FeatureBits::FIXED_CALLS,
            constants: Vec::new(),
            upvalues: vec![BluUpvalue::ParentRegister(0)],
            children: vec![2],
            code: vec![
                BluInstruction::NewClosure {
                    destination: 0,
                    child: 0,
                },
                BluInstruction::Call {
                    destination: 1,
                    function: 0,
                    arguments: 0,
                    argument_count: 0,
                },
                BluInstruction::Return { first: 1, count: 1 },
            ],
            source_map: vec![span; 3],
            locals: Vec::new(),
            upvalue_debug: Vec::new(),
        });
        validated.prototypes.push(BluPrototype {
            profile: SemanticProfile::Blu,
            source,
            register_count: 2,
            parameter_count: 0,
            is_vararg: false,
            required_features: FeatureBits::BASELINE | FeatureBits::CLOSURES,
            constants: vec![BluConstant::Number(1.0)],
            upvalues: vec![BluUpvalue::ParentUpvalue(0)],
            children: Vec::new(),
            code: vec![
                BluInstruction::GetUpvalue {
                    destination: 0,
                    upvalue: 0,
                },
                BluInstruction::LoadConstant {
                    destination: 1,
                    constant: 0,
                },
                BluInstruction::Add {
                    destination: 0,
                    left: 0,
                    right: 1,
                },
                BluInstruction::SetUpvalue {
                    upvalue: 0,
                    source: 0,
                },
                BluInstruction::Return { first: 0, count: 1 },
            ],
            source_map: vec![span; 5],
            locals: Vec::new(),
            upvalue_debug: Vec::new(),
        });
        let validated = ValidatedArtifact::new(validated, BluLimits::default()).unwrap();
        let encoded = encode(&validated, BluLimits::default()).unwrap();
        assert_eq!(
            Vm::default().with_call_limit(1).execute_blu_v1(
                decode_validated(&encoded, BluLimits::default()).unwrap(),
                BluLimits::default(),
            ),
            Err(RuntimeError::CallLimit { limit: 1 })
        );
        assert_eq!(
            Vm::default().execute_blu_v1(validated, BluLimits::default()),
            Ok(vec![Value::Number(11.0), Value::Number(12.0)])
        );
    }

    #[test]
    fn direct_blu_v1_transient_storage_is_memory_accounted_and_released() {
        let program = || {
            validated_blu_program(
                SemanticProfile::Blu,
                vec![BluConstant::String(b"blu".to_vec())],
                vec![
                    BluInstruction::LoadConstant {
                        destination: 0,
                        constant: 0,
                    },
                    BluInstruction::Return { first: 0, count: 1 },
                ],
                FeatureBits::BASELINE,
                1,
            )
        };
        let charge = blu_v1_execution_bytes(program().main()).unwrap();
        assert!(charge > b"blu".len());

        let baseline = Vm::try_new(Dialect::Blu)
            .unwrap()
            .memory_usage()
            .current_bytes;
        let rejected_limit = baseline + charge - 1;
        let mut rejected = Vm::try_new_with_memory(
            Dialect::Blu,
            MemoryConfig {
                hard_limit_bytes: Some(rejected_limit),
                ..MemoryConfig::default()
            },
        )
        .unwrap();
        assert!(matches!(
            rejected.execute_blu_v1(program(), BluLimits::default()),
            Err(RuntimeError::Heap(HeapError::Memory(
                MemoryError::LimitExceeded {
                    requested,
                    used,
                    limit,
                }
            ))) if requested == charge && used == baseline && limit == rejected_limit
        ));
        assert_eq!(rejected.memory_usage().current_bytes, baseline);

        let accepted_limit = baseline + charge;
        let mut accepted = Vm::try_new_with_memory(
            Dialect::Blu,
            MemoryConfig {
                hard_limit_bytes: Some(accepted_limit),
                ..MemoryConfig::default()
            },
        )
        .unwrap();
        assert_eq!(
            accepted.execute_blu_v1(program(), BluLimits::default()),
            Ok(vec![Value::String(Arc::from(&b"blu"[..]))])
        );
        assert_eq!(accepted.memory_usage().current_bytes, baseline);
        assert_eq!(accepted.memory_usage().peak_bytes, accepted_limit);

        let mut failing = Vm::try_new_with_memory(
            Dialect::Blu,
            MemoryConfig {
                hard_limit_bytes: Some(accepted_limit),
                ..MemoryConfig::default()
            },
        )
        .unwrap()
        .with_instruction_limit(0);
        assert_eq!(
            failing.execute_blu_v1(program(), BluLimits::default()),
            Err(RuntimeError::InstructionLimit { limit: 0 })
        );
        assert_eq!(failing.memory_usage().current_bytes, baseline);
        assert_eq!(failing.memory_usage().peak_bytes, accepted_limit);
    }

    #[test]
    fn direct_blu_v1_floor_division_obeys_authorized_profile_semantics() {
        let floor_program = |profile, constants| {
            validated_blu_program(
                profile,
                constants,
                vec![
                    BluInstruction::LoadConstant {
                        destination: 0,
                        constant: 0,
                    },
                    BluInstruction::LoadConstant {
                        destination: 1,
                        constant: 1,
                    },
                    BluInstruction::FloorDivide {
                        destination: 2,
                        left: 0,
                        right: 1,
                    },
                    BluInstruction::Return { first: 2, count: 1 },
                ],
                FeatureBits::BASELINE
                    | FeatureBits::INTEGER_CONSTANTS
                    | FeatureBits::FLOOR_DIVISION,
                3,
            )
        };

        for profile in [SemanticProfile::Blu, SemanticProfile::Luau] {
            assert_eq!(
                Vm::new(Dialect::Blu).execute_blu_v1(
                    floor_program(
                        profile,
                        vec![BluConstant::Number(-7.0), BluConstant::Number(3.0)]
                    ),
                    BluLimits::default()
                ),
                Ok(vec![Value::Number(-3.0)]),
                "{profile}"
            );
        }
        for profile in [
            SemanticProfile::Lua53,
            SemanticProfile::Lua54,
            SemanticProfile::Lua55,
        ] {
            assert_eq!(
                Vm::new(Dialect::Blu).execute_blu_v1(
                    floor_program(
                        profile,
                        vec![BluConstant::Integer(-7), BluConstant::Integer(3)]
                    ),
                    BluLimits::default()
                ),
                Ok(vec![Value::Integer(-3)]),
                "{profile}"
            );
        }
    }

    #[test]
    fn direct_blu_v1_integer_modulo_uses_floor_semantics_and_rejects_zero() {
        let modulo_program = |left, right| {
            validated_blu_program(
                SemanticProfile::Lua54,
                vec![BluConstant::Integer(left), BluConstant::Integer(right)],
                vec![
                    BluInstruction::LoadConstant {
                        destination: 0,
                        constant: 0,
                    },
                    BluInstruction::LoadConstant {
                        destination: 1,
                        constant: 1,
                    },
                    BluInstruction::Modulo {
                        destination: 2,
                        left: 0,
                        right: 1,
                    },
                    BluInstruction::Return { first: 2, count: 1 },
                ],
                FeatureBits::BASELINE | FeatureBits::INTEGER_CONSTANTS,
                3,
            )
        };

        let mut vm = Vm::new(Dialect::Blu);
        assert_eq!(
            vm.execute_blu_v1(modulo_program(-7, 3), BluLimits::default()),
            Ok(vec![Value::Integer(2)])
        );
        assert_eq!(
            vm.execute_blu_v1(modulo_program(7, -3), BluLimits::default()),
            Ok(vec![Value::Integer(-2)])
        );
        assert_eq!(
            vm.execute_blu_v1(modulo_program(7, 0), BluLimits::default()),
            Err(RuntimeError::DivideByZero)
        );
    }

    #[test]
    fn direct_blu_v1_byte_string_length_is_profile_typed_and_structured() {
        let length_program = |profile, constant| {
            validated_blu_program(
                profile,
                vec![constant],
                vec![
                    BluInstruction::LoadConstant {
                        destination: 0,
                        constant: 0,
                    },
                    BluInstruction::Length {
                        destination: 1,
                        source: 0,
                    },
                    BluInstruction::Return { first: 1, count: 1 },
                ],
                FeatureBits::BASELINE,
                2,
            )
        };

        let mut vm = Vm::new(Dialect::Blu);
        assert_eq!(
            vm.execute_blu_v1(
                length_program(SemanticProfile::Luau, BluConstant::String(b"blu".to_vec())),
                BluLimits::default()
            ),
            Ok(vec![Value::Number(3.0)])
        );
        assert_eq!(
            vm.execute_blu_v1(
                length_program(SemanticProfile::Lua54, BluConstant::String(b"blu".to_vec())),
                BluLimits::default()
            ),
            Ok(vec![Value::Integer(3)])
        );
        assert_eq!(
            vm.execute_blu_v1(
                length_program(SemanticProfile::Blu, BluConstant::Boolean(true)),
                BluLimits::default()
            ),
            Err(RuntimeError::Type {
                operation: "length",
                expected: "string or table",
                actual: "boolean",
            })
        );
    }

    #[test]
    fn direct_blu_v1_accepts_assigned_blu_integers_and_rejects_luau_integers() {
        let blu = validated_blu_program(
            SemanticProfile::Blu,
            vec![BluConstant::Integer(1)],
            vec![
                BluInstruction::LoadConstant {
                    destination: 0,
                    constant: 0,
                },
                BluInstruction::Return { first: 0, count: 1 },
            ],
            FeatureBits::BASELINE | FeatureBits::INTEGER_CONSTANTS,
            1,
        );
        assert_eq!(
            Vm::new(Dialect::Blu).execute_blu_v1(blu, BluLimits::default()),
            Ok(vec![Value::Integer(1)])
        );

        let artifact = validated_blu_program(
            SemanticProfile::Luau,
            vec![BluConstant::Integer(1)],
            vec![
                BluInstruction::LoadConstant {
                    destination: 0,
                    constant: 0,
                },
                BluInstruction::Return { first: 0, count: 1 },
            ],
            FeatureBits::BASELINE | FeatureBits::INTEGER_CONSTANTS,
            1,
        );
        assert_eq!(
            Vm::new(Dialect::Blu).execute_blu_v1(artifact, BluLimits::default()),
            Err(RuntimeError::UnsupportedBluV1Constant {
                profile: SemanticProfile::Luau,
                constant: 0,
                kind: "integer",
            })
        );
    }

    #[test]
    fn direct_blu_v1_revalidates_under_execution_limits() {
        let artifact = validated_blu_program(
            SemanticProfile::Blu,
            vec![BluConstant::Number(1.0)],
            vec![
                BluInstruction::LoadConstant {
                    destination: 0,
                    constant: 0,
                },
                BluInstruction::Return { first: 0, count: 1 },
            ],
            FeatureBits::BASELINE,
            1,
        );
        let limits = BluLimits {
            max_registers_per_prototype: 0,
            ..BluLimits::default()
        };
        assert!(matches!(
            Vm::new(Dialect::Blu).execute_blu_v1(artifact, limits),
            Err(RuntimeError::BluValidation(_))
        ));
    }

    #[test]
    fn frame_runner_restores_active_profile_after_success_and_error() {
        let success = Arc::new(test_chunk(
            &[],
            vec![],
            vec![ad(Opcode::LoadN, 0, 7), abc(Opcode::Return, 0, 2, 0)],
            1,
        ));
        let success_frame =
            Frame::new(success, 0, SemanticProfile::Blu, Vec::new(), None, &[]).unwrap();
        let mut vm = Vm::new(Dialect::Luau);
        vm.active_profile = Some(SemanticProfile::Luau);
        let mut remaining = 8;
        assert_eq!(
            vm.run_frames(success_frame, Vec::new(), &mut remaining, 0),
            Ok(vec![Value::Number(7.0)])
        );
        assert_eq!(vm.active_profile, Some(SemanticProfile::Luau));

        let failure = Arc::new(test_chunk(
            &[b"missing"],
            vec![Constant::String(0)],
            vec![
                abc(Opcode::GetGlobal, 0, 0, 0),
                0,
                abc(Opcode::Call, 0, 1, 1),
                abc(Opcode::Return, 0, 1, 0),
            ],
            1,
        ));
        let failure_frame = Frame::new(
            failure,
            0,
            SemanticProfile::Blu,
            vec![Value::String(Arc::from(&b"missing"[..]))],
            None,
            &[],
        )
        .unwrap();
        vm.active_profile = None;
        let mut remaining = 8;
        assert!(matches!(
            vm.run_frames(failure_frame, Vec::new(), &mut remaining, 0),
            Err(RuntimeError::Type {
                operation: "call",
                ..
            })
        ));
        assert_eq!(vm.active_profile, None);
    }

    #[test]
    fn artifact_profile_controls_observable_root_frame_semantics() {
        // BluV1's current bootstrap instruction set cannot express a global
        // lookup or call, and its translator rejects nested prototypes. Use a
        // real translated artifact as the authorized source of the profile,
        // then exercise that profile on the root-frame substrate which the
        // translated code enters. This avoids exposing a way to forge a
        // profiled ValidatedChunk merely for a test or pretending mixed-profile
        // callees are representable.
        let translated = translated_baseline(SemanticProfile::Blu);
        let artifact_profile = translated.profile();
        let chunk = Arc::new(test_chunk(
            &[b"string", b"rep", b"ab", b"-"],
            vec![
                Constant::String(0),
                Constant::String(1),
                Constant::String(2),
                Constant::String(3),
            ],
            vec![
                abc(Opcode::GetGlobal, 0, 0, 0),
                0,
                abc(Opcode::GetTableKs, 0, 0, 0),
                1,
                ad(Opcode::LoadK, 1, 2),
                ad(Opcode::LoadN, 2, 3),
                ad(Opcode::LoadK, 3, 3),
                abc(Opcode::Call, 0, 4, 2),
                abc(Opcode::Return, 0, 2, 0),
            ],
            4,
        ));
        let mut vm = Vm::new(Dialect::Luau);
        assert_eq!(vm.configured_profile(), Ok(SemanticProfile::Luau));
        assert_ne!(artifact_profile, vm.configured_profile().unwrap());
        let frame = Frame::new(
            chunk,
            0,
            artifact_profile,
            vec![
                Value::String(Arc::from(&b"string"[..])),
                Value::String(Arc::from(&b"rep"[..])),
                Value::String(Arc::from(&b"ab"[..])),
                Value::String(Arc::from(&b"-"[..])),
            ],
            None,
            &[],
        )
        .unwrap();
        let mut remaining = 32;

        assert_eq!(
            vm.run_frames(frame, Vec::new(), &mut remaining, 0),
            Ok(vec![Value::String(Arc::from(&b"ab-ab-ab"[..]))])
        );
        assert_eq!(vm.active_profile, None);
    }

    #[test]
    fn instruction_limit_interrupts_backward_loop() {
        let mut chunk = load(RETURN_THREE_V12, LoadLimits::default()).unwrap();
        let prototype = &mut chunk.prototypes[0];
        prototype.code = vec![Opcode::JumpBack as u32 | ((-1_i16 as u16 as u32) << 16)];
        prototype.instructions = blu_bytecode::decode(&prototype.code).unwrap();
        let error = Vm::default()
            .with_instruction_limit(8)
            .execute(&chunk)
            .unwrap_err();
        assert_eq!(error, RuntimeError::InstructionLimit { limit: 8 });
    }

    #[test]
    fn oversized_table_capacities_fail_validation_without_allocating() {
        let return_none = abc(Opcode::Return, 0, 1, 0);
        let chunk = test_chunk(
            &[],
            vec![],
            vec![
                abc(Opcode::NewTable, 0, 0, 0),
                u32::try_from(MAX_TABLE_INITIAL_CAPACITY + 1).unwrap(),
                return_none,
            ],
            1,
        );
        assert!(matches!(
            Vm::default().execute(&chunk),
            Err(RuntimeError::Validation(error)) if error.message.contains("array capacity")
        ));

        let chunk = test_chunk(
            &[],
            vec![],
            vec![abc(Opcode::NewTable, 0, 255, 0), 0, return_none],
            1,
        );
        assert!(matches!(
            Vm::default().execute(&chunk),
            Err(RuntimeError::Validation(error)) if error.message.contains("hash capacity")
        ));
    }

    #[test]
    fn rejects_unimplemented_dialect_explicitly() {
        let chunk = load(RETURN_THREE_V12, LoadLimits::default()).unwrap();
        assert_eq!(
            Vm::new(Dialect::Lua54).execute(&chunk),
            Err(RuntimeError::DialectNotImplemented(Dialect::Lua54))
        );
    }

    #[test]
    fn builtins_use_native_registry_and_capture_output() {
        let mut vm = Vm::default();
        let print = match vm.global(b"print").cloned().unwrap() {
            Value::NativeFunction(function) => function,
            other => panic!("print is {other:?}"),
        };
        let function = vm.native_functions[print.0 as usize].clone();
        function(
            &mut vm,
            &[Value::String(Arc::from(&b"blu"[..])), Value::Number(3.0)],
        )
        .unwrap();
        assert_eq!(vm.take_output(), b"blu\t3\n");

        let mut limited = Vm::default().with_output_limit(3);
        let Value::NativeFunction(print) = limited.global(b"print").cloned().unwrap() else {
            panic!("print is not native");
        };
        let print = limited.native_functions[print.0 as usize].clone();
        let error = print(&mut limited, &[Value::String(Arc::from(&b"blu"[..]))]).unwrap_err();
        assert_eq!(
            error,
            RuntimeError::OutputLimit {
                required: 4,
                limit: 3,
            }
        );
        assert!(limited.take_output().is_empty());

        let string = table_id(vm.global(b"string").unwrap()).unwrap();
        let sub = vm
            .heap
            .table_get(string, &Value::String(Arc::from(&b"sub"[..])))
            .unwrap();
        let Value::NativeFunction(sub) = sub else {
            panic!("string.sub is not native");
        };
        let function = vm.native_functions[sub.0 as usize].clone();
        let result = function(
            &mut vm,
            &[Value::String(Arc::from(&b"blu"[..])), Value::Number(-2.0)],
        )
        .unwrap();
        assert_eq!(result, [Value::String(Arc::from(&b"lu"[..]))]);
    }

    #[test]
    fn runtime_value_rendering_uses_exact_single_buffers() {
        let vm = Vm::default();
        let values = [
            Value::Nil,
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Number(-12.5),
            Value::Integer(i64::MIN),
            Value::String(Arc::from(&b"\0\xffblu"[..])),
            vm.global(b"string").cloned().unwrap(),
            vm.global(b"print").cloned().unwrap(),
            Value::Thread(vm.main_thread),
        ];

        for value in values {
            let expected = rendered_value_len(&value);
            let mut rendered = try_vec_with_capacity(expected, "test rendering").unwrap();
            append_value(&mut rendered, &value);
            assert_eq!(rendered.len(), expected, "rendered {value:?}");
            assert_eq!(rendered.capacity(), expected, "rendered {value:?}");
        }

        let tostring = match vm.global(b"tostring").cloned().unwrap() {
            Value::NativeFunction(function) => vm.native_functions[function.0 as usize].clone(),
            other => panic!("tostring is {other:?}"),
        };
        let mut vm = vm;
        assert_eq!(
            tostring(&mut vm, &[Value::Integer(i64::MIN)]),
            Ok(vec![Value::String(Arc::from(
                i64::MIN.to_string().into_bytes()
            ))])
        );

        let mut remaining = 1;
        assert_eq!(
            vm.concat_value(
                Value::Integer(i64::MIN),
                Value::Number(2.5),
                CallContext::new(&mut remaining, 0, GcRoots::default()),
            ),
            Ok(Value::String(Arc::from(
                [i64::MIN.to_string().as_bytes(), b"2.5"].concat(),
            )))
        );
    }

    #[test]
    fn table_concat_preflights_work_and_allocates_one_bounded_result() {
        let mut vm = Vm::default();
        let table = vm.heap.allocate_table(3, 0).unwrap();
        for (index, value) in [
            Value::String(Arc::from(&b"a\0"[..])),
            Value::Integer(i64::MIN),
            Value::Number(2.5),
        ]
        .into_iter()
        .enumerate()
        {
            vm.heap
                .table_set(table, Value::Integer((index + 1) as i64), value)
                .unwrap();
        }
        let concat = native(&vm, b"table", b"concat");
        assert_eq!(
            concat(
                &mut vm,
                &[Value::Table(table), Value::String(Arc::from(&b"\xff"[..])),],
            ),
            Ok(vec![Value::String(Arc::from(
                [
                    b"a\0".as_slice(),
                    b"\xff",
                    i64::MIN.to_string().as_bytes(),
                    b"\xff",
                    b"2.5",
                ]
                .concat(),
            ))])
        );
        assert_eq!(
            concat(
                &mut vm,
                &[
                    Value::Table(table),
                    Value::Integer(-7),
                    Value::Integer(1),
                    Value::Integer(2),
                ],
            ),
            Ok(vec![Value::String(Arc::from(
                [b"a\0".as_slice(), b"-7", i64::MIN.to_string().as_bytes()].concat(),
            ))])
        );

        assert_eq!(
            concat(
                &mut vm,
                &[
                    Value::Table(table),
                    Value::String(Arc::from(&b""[..])),
                    Value::Integer(0),
                    Value::Integer(MAX_DYNAMIC_REGISTERS as i64),
                ],
            ),
            Err(RuntimeError::StackLimit {
                required: MAX_DYNAMIC_REGISTERS + 1,
                limit: MAX_DYNAMIC_REGISTERS,
            })
        );
        assert_eq!(
            concat(
                &mut vm,
                &[
                    Value::Table(table),
                    Value::String(Arc::from(&b""[..])),
                    Value::String(Arc::from(&b"1"[..])),
                ],
            ),
            Err(RuntimeError::Type {
                operation: "table.concat",
                expected: "number",
                actual: "string",
            })
        );
    }

    #[test]
    fn registered_native_results_and_errors_propagate_through_calls() {
        let code = vec![
            abc(Opcode::GetGlobal, 0, 0, 0),
            0,
            abc(Opcode::Call, 0, 1, 2),
            abc(Opcode::Return, 0, 2, 0),
        ];
        let mut chunk = test_chunk(&[b"native"], vec![Constant::String(0)], code, 1);
        let mut vm = Vm::default();
        let id = vm.register_function(|_, _| Ok(vec![Value::Integer(42)]));
        vm.set_global(&b"native"[..], Value::NativeFunction(id));
        assert_eq!(vm.execute(&chunk), Ok(vec![Value::Integer(42)]));

        let id = vm.register_function(|_, _| Err(RuntimeError::Breakpoint { pc: 91 }));
        vm.set_global(&b"native"[..], Value::NativeFunction(id));
        assert_eq!(vm.execute(&chunk), Err(RuntimeError::Breakpoint { pc: 91 }));

        chunk.prototypes[0].code[1] = 99;
        chunk.prototypes[0].instructions = blu_bytecode::decode(&chunk.prototypes[0].code).unwrap();
        assert!(matches!(
            vm.execute(&chunk),
            Err(RuntimeError::Validation(error)) if error.message.contains("constant 99")
        ));
    }

    #[test]
    fn native_result_limit_rejects_results_before_frame_writes() {
        let code = vec![
            abc(Opcode::GetGlobal, 0, 0, 0),
            0,
            abc(Opcode::Call, 0, 1, 2),
            abc(Opcode::Return, 0, 2, 0),
        ];
        let chunk = test_chunk(&[b"native"], vec![Constant::String(0)], code, 1);
        let mut vm = Vm::default().with_native_result_limit(1);
        assert_eq!(vm.native_result_limit(), 1);
        let id = vm.register_function(|_, _| Ok(vec![Value::Integer(41), Value::Integer(42)]));
        vm.set_global(&b"native"[..], Value::NativeFunction(id));

        assert_eq!(
            vm.execute(&chunk),
            Err(RuntimeError::NativeResultLimit {
                required: 2,
                limit: 1,
            })
        );
        assert_eq!(vm.retained_value_count(), 0);

        let mut builtin_vm = Vm::default().with_native_result_limit(1);
        let string_byte = native(&builtin_vm, b"string", b"byte");
        assert_eq!(
            string_byte(
                &mut builtin_vm,
                &[
                    Value::String(Arc::from(&b"ab"[..])),
                    Value::Integer(1),
                    Value::Integer(2),
                ],
            ),
            Err(RuntimeError::NativeResultLimit {
                required: 2,
                limit: 1,
            })
        );
    }

    #[test]
    fn fallible_embedding_registries_enforce_limits_without_partial_mutation() {
        let mut vm = Vm::default();
        let native_count = vm.native_functions.len();
        vm.native_function_limit = native_count;
        assert_eq!(
            vm.try_register_function(|_, _| Ok(Vec::new())),
            Err(RuntimeError::NativeFunctionLimit {
                required: native_count + 1,
                limit: native_count,
            })
        );
        assert_eq!(vm.native_functions.len(), native_count);

        let global_count = vm.globals.len();
        vm.global_limit = global_count;
        assert_eq!(
            vm.try_set_global(&b"new-global"[..], Value::Integer(1)),
            Err(RuntimeError::GlobalLimit {
                required: global_count + 1,
                limit: global_count,
            })
        );
        assert_eq!(vm.global(&b"new-global"[..]), None);

        let previous = vm.try_set_global(&b"print"[..], Value::Integer(7)).unwrap();
        assert!(matches!(previous, Some(Value::NativeFunction(_))));
        assert_eq!(vm.global(&b"print"[..]), Some(&Value::Integer(7)));
        assert_eq!(vm.globals.len(), global_count);
    }

    #[test]
    fn dynamic_stack_limit_is_atomic_and_legal_growth_succeeds() {
        let chunk = Arc::new(test_chunk(&[], Vec::new(), vec![Opcode::Return as u32], 1));
        let mut frame = Frame::new(chunk, 0, SemanticProfile::Blu, Vec::new(), None, &[]).unwrap();
        let before = frame.registers.clone();

        assert_eq!(
            frame.write_results(0, 3, vec![Value::Integer(1), Value::Integer(2)],),
            Err(RuntimeError::Register {
                register: 1,
                count: 1,
            })
        );
        assert_eq!(frame.registers, before);
        assert_eq!(frame.top, 0);

        frame
            .write_results(u8::MAX, 0, vec![Value::Integer(1), Value::Integer(2)])
            .unwrap();
        assert_eq!(frame.registers.len(), usize::from(u8::MAX) + 2);
        assert_eq!(frame.registers[usize::from(u8::MAX)], Value::Integer(1));
        assert_eq!(frame.registers[usize::from(u8::MAX) + 1], Value::Integer(2));
        assert_eq!(frame.top, usize::from(u8::MAX) + 2);

        let chunk = Arc::new(test_chunk(&[], Vec::new(), vec![Opcode::Return as u32], 1));
        let mut frame = Frame::new(chunk, 0, SemanticProfile::Blu, Vec::new(), None, &[]).unwrap();
        let before = frame.registers.clone();
        assert_eq!(
            frame.ensure_dynamic(MAX_DYNAMIC_REGISTERS + 1),
            Err(RuntimeError::StackLimit {
                required: MAX_DYNAMIC_REGISTERS + 1,
                limit: MAX_DYNAMIC_REGISTERS,
            })
        );
        assert_eq!(frame.registers, before);
        assert_eq!(frame.top, 0);

        frame.ensure_dynamic(3).unwrap();
        assert_eq!(frame.registers, vec![Value::Nil; 3]);
        assert_eq!(frame.top, 0);
    }

    #[test]
    fn guest_vector_capacity_overflow_is_structured() {
        assert_eq!(
            try_vec_with_capacity::<Value>(usize::MAX, "test values").unwrap_err(),
            RuntimeError::Allocation {
                what: "test values",
            }
        );
    }

    #[test]
    fn failed_root_reservation_preserves_logical_contents() {
        let mut values = vec![Value::Integer(7)];
        assert_eq!(
            try_reserve_exact(&mut values, usize::MAX, "test roots"),
            Err(RuntimeError::Allocation { what: "test roots" })
        );
        assert_eq!(values, [Value::Integer(7)]);

        let mut roots = GcRoots::from_values(&[Value::Integer(1)]).unwrap();
        roots.push_value(Value::Integer(2)).unwrap();
        roots
            .extend(GcRoots::from_values(&[Value::Integer(3)]).unwrap())
            .unwrap();
        assert_eq!(
            roots.values,
            [Value::Integer(1), Value::Integer(2), Value::Integer(3)]
        );
    }

    #[test]
    fn frame_construction_fallibly_copies_registers_varargs_and_constants() {
        let mut chunk = test_chunk(&[], Vec::new(), vec![Opcode::Return as u32], 2);
        chunk.prototypes[0].parameter_count = 1;
        let constants = vec![Value::Integer(9)];
        let frame = Frame::new(
            Arc::new(chunk),
            0,
            SemanticProfile::Blu,
            constants.clone(),
            None,
            &[Value::Integer(1), Value::Integer(2), Value::Integer(3)],
        )
        .unwrap();

        assert_eq!(frame.constants, constants);
        assert_eq!(frame.registers, [Value::Integer(1), Value::Nil]);
        assert_eq!(frame.varargs, [Value::Integer(2), Value::Integer(3)]);
        assert_eq!(frame.top, 1);
    }

    #[test]
    fn continuation_root_assembly_preserves_frame_and_caller_values() {
        let mut heap = Heap::default();
        let current = heap.allocate_table(0, 0).unwrap();
        let caller_value = heap.allocate_table(0, 0).unwrap();
        let garbage = heap.allocate_table(0, 0).unwrap();
        let chunk = Arc::new(test_chunk(&[], Vec::new(), vec![Opcode::Return as u32], 1));
        let frame = Frame::new(
            chunk.clone(),
            0,
            SemanticProfile::Blu,
            vec![Value::Table(current)],
            None,
            &[],
        )
        .unwrap();
        let caller = Caller {
            frame: Frame::new(
                chunk,
                0,
                SemanticProfile::Blu,
                vec![Value::Table(caller_value)],
                None,
                &[],
            )
            .unwrap(),
            register: 0,
            encoded_count: 1,
            return_mode: ReturnMode::Direct,
        };

        let roots = continuation_roots(&frame, &[caller], &heap).unwrap();
        heap.collect(&roots.values).unwrap();
        assert_eq!(heap.table_get(current, &Value::Integer(1)), Ok(Value::Nil));
        assert_eq!(
            heap.table_get(caller_value, &Value::Integer(1)),
            Ok(Value::Nil)
        );
        assert_eq!(
            heap.table_get(garbage, &Value::Integer(1)),
            Err(HeapError::StaleTable(garbage))
        );
    }

    #[test]
    fn resumed_caller_snapshots_preserve_state_without_shared_vectors() {
        let mut heap = Heap::default();
        let upvalue = heap.allocate_upvalue(Value::Integer(9)).unwrap();
        let chunk = Arc::new(test_chunk(&[], Vec::new(), vec![Opcode::Return as u32], 2));
        let mut frame = Frame::new(
            chunk,
            0,
            SemanticProfile::Blu,
            vec![Value::Integer(3)],
            None,
            &[],
        )
        .unwrap();
        frame.registers[0] = Value::Integer(4);
        frame.varargs.push(Value::Integer(5));
        frame.insert_open_upvalue(0, upvalue).unwrap();
        frame.open_upvalues_dirty = true;
        frame.pc = 7;
        frame.top = 1;
        let callers = vec![Caller {
            frame,
            register: 1,
            encoded_count: 2,
            return_mode: ReturnMode::ErrorHandler(Value::Integer(6)),
        }];

        let mut cloned = try_clone_callers(&callers).unwrap();
        assert_eq!(cloned[0].frame.constants, [Value::Integer(3)]);
        assert_eq!(cloned[0].frame.registers[0], Value::Integer(4));
        assert_eq!(cloned[0].frame.varargs, [Value::Integer(5)]);
        assert_eq!(cloned[0].frame.open_upvalue(0), Some(upvalue));
        assert!(cloned[0].frame.open_upvalues_dirty);
        assert_eq!(cloned[0].frame.pc, 7);
        assert_eq!(cloned[0].frame.top, 1);
        assert_eq!(cloned[0].register, 1);
        assert_eq!(cloned[0].encoded_count, 2);

        cloned[0].frame.registers[0] = Value::Integer(8);
        assert_eq!(callers[0].frame.registers[0], Value::Integer(4));
    }

    #[test]
    fn resume_root_allocation_failure_preserves_resumable_state() {
        let probe = Vm::default();
        let baseline = probe.memory_usage().current_bytes;
        let thread_allocation = probe.heap.thread_allocation_bytes(1).unwrap();
        let limit = baseline.checked_add(thread_allocation).unwrap();
        let mut vm = Vm::try_new_with_memory(
            Dialect::Blu,
            MemoryConfig {
                hard_limit_bytes: Some(limit),
                gc_start_bytes: limit,
                gc_growth_percent: 50,
                max_single_allocation_bytes: usize::MAX,
            },
        )
        .unwrap();
        let function = vm.global(b"print").unwrap().clone();
        let roots = GcRoots::from_values(std::slice::from_ref(&function)).unwrap();
        let thread = vm
            .allocate_thread(std::slice::from_ref(&function), &roots)
            .unwrap();
        vm.threads
            .insert(thread, ThreadState::New(function.clone()));
        assert_eq!(vm.memory_usage().current_bytes, limit);

        let arguments = [Value::Thread(thread), Value::Integer(1)];
        let call_roots = GcRoots::from_values(&arguments).unwrap();
        let mut remaining = 10;
        assert!(matches!(
            vm.resume_thread(&arguments, &mut remaining, 0, call_roots),
            Err(RuntimeError::Heap(HeapError::Memory(
                crate::MemoryError::LimitExceeded {
                    used,
                    limit: error_limit,
                    ..
                }
            ))) if used == limit && error_limit == limit
        ));
        assert!(
            matches!(vm.threads.get(&thread), Some(ThreadState::New(value)) if value == &function)
        );
        assert_eq!(vm.running_thread, None);
        assert_eq!(vm.memory_usage().current_bytes, limit);
    }

    #[test]
    fn close_root_update_failure_preserves_resumable_state() {
        let mut vm = Vm::default();
        let close = native(&vm, b"coroutine", b"close");
        let function = vm.global(b"print").unwrap().clone();
        let thread = vm
            .heap
            .allocate_thread(std::slice::from_ref(&function))
            .unwrap();
        vm.threads
            .insert(thread, ThreadState::New(function.clone()));
        vm.heap.collect(std::iter::empty()).unwrap();

        assert_eq!(
            close(&mut vm, &[Value::Thread(thread)]),
            Err(RuntimeError::Heap(HeapError::StaleThread(thread)))
        );
        assert!(
            matches!(vm.threads.get(&thread), Some(ThreadState::New(value)) if value == &function)
        );
    }

    #[test]
    fn post_execution_root_failure_leaves_coroutine_dead() {
        let mut vm = Vm::default();
        let function = Value::NativeFunction(vm.register_function(|vm, _| {
            vm.heap.collect(std::iter::empty())?;
            Ok(Vec::new())
        }));
        let thread = vm
            .heap
            .allocate_thread(std::slice::from_ref(&function))
            .unwrap();
        vm.threads.insert(thread, ThreadState::New(function));
        let arguments = [Value::Thread(thread)];
        let roots = GcRoots::from_values(&arguments).unwrap();
        let mut remaining = 10;

        assert_eq!(
            vm.resume_thread(&arguments, &mut remaining, 0, roots),
            Err(RuntimeError::Heap(HeapError::StaleThread(thread)))
        );
        assert!(matches!(
            vm.threads.get(&thread),
            Some(ThreadState::Dead(None))
        ));
        assert_eq!(vm.running_thread, None);
    }

    #[test]
    fn dead_coroutine_heap_error_value_remains_rooted() {
        let mut vm = Vm::default();
        let function = Value::NativeFunction(vm.register_function(|vm, _| {
            let error = vm.heap.allocate_table(0, 0)?;
            Err(RuntimeError::Raised(Value::Table(error)))
        }));
        let thread = vm
            .heap
            .allocate_thread(std::slice::from_ref(&function))
            .unwrap();
        vm.threads.insert(thread, ThreadState::New(function));
        let arguments = [Value::Thread(thread)];
        let roots = GcRoots::from_values(&arguments).unwrap();
        let mut remaining = 10;

        let result = vm
            .resume_thread(&arguments, &mut remaining, 0, roots)
            .unwrap();
        let Value::Table(error) = &result[1] else {
            panic!("coroutine error result is not a table");
        };
        let error = *error;
        assert!(matches!(
            vm.threads.get(&thread),
            Some(ThreadState::Dead(Some(Value::Table(stored)))) if *stored == error
        ));

        vm.heap.collect([&Value::Thread(thread)]).unwrap();
        assert_eq!(vm.heap.table_get(error, &Value::Integer(1)), Ok(Value::Nil));
    }

    #[test]
    fn globals_and_multi_part_imports_follow_table_paths() {
        let code = vec![
            ad(Opcode::LoadN, 0, 17),
            abc(Opcode::SetGlobal, 0, 0, 0),
            0,
            abc(Opcode::GetGlobal, 1, 0, 0),
            0,
            ad(Opcode::GetImport, 2, 3),
            (2 << 30) | (1 << 20) | (2 << 10),
            abc(Opcode::Return, 1, 3, 0),
        ];
        let chunk = test_chunk(
            &[b"answer", b"string", b"sub"],
            vec![
                Constant::String(0),
                Constant::String(1),
                Constant::String(2),
                Constant::Import((2 << 30) | (1 << 20) | (2 << 10)),
            ],
            code,
            3,
        );
        let mut vm = Vm::default();
        let result = vm.execute(&chunk).unwrap();
        assert_eq!(result[0], Value::Number(17.0));
        assert_eq!(vm.global(b"answer"), Some(&Value::Number(17.0)));
        assert!(matches!(result[1], Value::NativeFunction(_)));
    }

    #[test]
    fn recursive_calls_stop_at_the_configured_limit() {
        let mut chunk = test_chunk(
            &[b"recurse"],
            vec![Constant::String(0)],
            vec![
                ad(Opcode::NewClosure, 0, 0),
                abc(Opcode::SetGlobal, 0, 0, 0),
                0,
                abc(Opcode::Call, 0, 1, 1),
                abc(Opcode::Return, 0, 1, 0),
            ],
            1,
        );
        let mut child = chunk.prototypes[0].clone();
        child.code = vec![
            abc(Opcode::GetGlobal, 0, 0, 0),
            0,
            abc(Opcode::Call, 0, 1, 1),
            abc(Opcode::Return, 0, 1, 0),
        ];
        child.instructions = blu_bytecode::decode(&child.code).unwrap();
        child.children.clear();
        chunk.prototypes[0].children = vec![1];
        chunk.prototypes.push(child);

        assert_eq!(
            Vm::default().with_call_limit(3).execute(&chunk),
            Err(RuntimeError::CallLimit { limit: 3 })
        );
    }

    #[test]
    fn active_registers_are_roots_when_native_code_collects() {
        let chunk = test_chunk(
            &[b"collect"],
            vec![Constant::String(0)],
            vec![
                abc(Opcode::NewTable, 0, 0, 0),
                0,
                abc(Opcode::GetGlobal, 1, 0, 0),
                0,
                abc(Opcode::Call, 1, 1, 1),
                abc(Opcode::Return, 0, 2, 0),
            ],
            2,
        );
        let mut vm = Vm::default();
        let id = vm.register_function(|vm, _| {
            vm.collect(std::iter::empty::<&Value>())?;
            Ok(Vec::new())
        });
        vm.set_global(&b"collect"[..], Value::NativeFunction(id));
        let result = vm.execute(&chunk).unwrap();
        let table = table_id(&result[0]).unwrap();
        assert_eq!(vm.heap.table_get(table, &Value::Integer(1)), Ok(Value::Nil));
    }

    #[test]
    fn string_sub_uses_lua_byte_indices_and_reports_type_errors() {
        let mut vm = Vm::default();
        let sub = native(&vm, b"string", b"sub");
        let string = Value::String(Arc::from(&b"a\xc3\xa9z"[..]));
        let cases = [
            (
                vec![string.clone(), Value::Integer(2), Value::Integer(3)],
                b"\xc3\xa9".as_slice(),
            ),
            (vec![string.clone(), Value::Integer(-1)], b"z".as_slice()),
            (
                vec![string.clone(), Value::Integer(3), Value::Integer(2)],
                b"".as_slice(),
            ),
            (
                vec![string.clone(), Value::Integer(-99), Value::Integer(99)],
                b"a\xc3\xa9z".as_slice(),
            ),
        ];
        for (arguments, expected) in cases {
            assert_eq!(
                sub(&mut vm, &arguments),
                Ok(vec![Value::String(Arc::from(expected))])
            );
        }
        assert!(matches!(
            sub(&mut vm, &[Value::Boolean(false), Value::Integer(1)]),
            Err(RuntimeError::Type {
                operation: "string.sub",
                expected: "string",
                actual: "boolean",
            })
        ));
        assert!(matches!(
            sub(&mut vm, &[string, Value::String(Arc::from(&b"1"[..]))]),
            Err(RuntimeError::Type {
                operation: "string.sub",
                expected: "number",
                actual: "string",
            })
        ));
    }

    #[test]
    fn global_values_remain_gc_roots() {
        let mut vm = Vm::default();
        let string = table_id(vm.global(b"string").unwrap()).unwrap();
        let garbage = vm.heap.allocate_table(0, 0).unwrap();
        let stats = vm.collect(std::iter::empty::<&Value>()).unwrap();
        assert_eq!(stats.collected, 1);
        assert_eq!(stats.before, stats.retained + stats.collected);
        assert!(
            vm.heap
                .table_get(string, &Value::String(Arc::from(&b"sub"[..])))
                .is_ok()
        );
        assert_eq!(
            vm.heap.table_get(garbage, &Value::Integer(1)),
            Err(HeapError::StaleTable(garbage))
        );
    }

    #[test]
    fn fallible_constructor_installs_builtin_heap_state() {
        let vm = Vm::try_new(Dialect::Blu).unwrap();
        assert!(matches!(vm.global(b"string"), Some(Value::Table(_))));
        assert!(matches!(vm.global(b"table"), Some(Value::Table(_))));
        assert!(matches!(vm.global(b"coroutine"), Some(Value::Table(_))));
        assert!(matches!(vm.global(b"math"), Some(Value::Table(_))));
    }

    #[test]
    fn configured_constructor_reports_heap_memory_limit_structurally() {
        let error = Vm::try_new_with_memory(
            Dialect::Blu,
            MemoryConfig {
                hard_limit_bytes: Some(0),
                gc_start_bytes: 0,
                gc_growth_percent: 50,
                max_single_allocation_bytes: usize::MAX,
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            RuntimeError::Heap(HeapError::Memory(crate::MemoryError::LimitExceeded {
                used: 0,
                limit: 0,
                ..
            }))
        ));
    }

    #[test]
    fn automatic_byte_collection_makes_room_for_heap_allocation() {
        let probe = Vm::default();
        let baseline = probe.memory_usage().current_bytes;
        let allocation = probe.heap.table_allocation_bytes(4, 0).unwrap();
        let limit = baseline.checked_add(allocation).unwrap();
        let mut vm = Vm::try_new_with_memory(
            Dialect::Blu,
            MemoryConfig {
                hard_limit_bytes: Some(limit),
                gc_start_bytes: limit,
                gc_growth_percent: 50,
                max_single_allocation_bytes: usize::MAX,
            },
        )
        .unwrap();
        assert_eq!(vm.memory_usage().current_bytes, baseline);

        let garbage = vm.heap.allocate_table(4, 0).unwrap();
        assert_eq!(vm.memory_usage().current_bytes, limit);
        let replacement = vm.allocate_table(4, 0, &GcRoots::default()).unwrap();

        assert_eq!(vm.memory_usage().collections, 1);
        assert_eq!(vm.memory_usage().current_bytes, limit);
        assert_eq!(
            vm.heap.table_get(garbage, &Value::Integer(1)),
            Err(HeapError::StaleTable(garbage))
        );
        assert_eq!(
            vm.heap.table_get(replacement, &Value::Integer(1)),
            Ok(Value::Nil)
        );
    }

    #[test]
    fn automatic_byte_collection_makes_room_for_table_growth() {
        let mut probe = Vm::default();
        probe.heap.allocate_table(0, 0).unwrap();
        probe.heap.allocate_table(4, 0).unwrap();
        let limit = probe.memory_usage().current_bytes;
        let mut vm = Vm::try_new_with_memory(
            Dialect::Blu,
            MemoryConfig {
                hard_limit_bytes: Some(limit),
                gc_start_bytes: limit,
                gc_growth_percent: 50,
                max_single_allocation_bytes: usize::MAX,
            },
        )
        .unwrap();

        let retained = vm.heap.allocate_table(0, 0).unwrap();
        vm.set_global(&b"retained"[..], Value::Table(retained));
        let garbage = vm.heap.allocate_table(4, 0).unwrap();
        assert_eq!(vm.memory_usage().current_bytes, limit);

        vm.table_set(
            retained,
            Value::Integer(1),
            Value::Integer(42),
            &GcRoots::default(),
        )
        .unwrap();

        assert_eq!(vm.memory_usage().collections, 1);
        assert_eq!(
            vm.heap.table_get(retained, &Value::Integer(1)),
            Ok(Value::Integer(42))
        );
        assert_eq!(
            vm.heap.table_get(garbage, &Value::Integer(1)),
            Err(HeapError::StaleTable(garbage))
        );
    }

    #[test]
    fn retained_bytes_fail_with_the_structured_memory_error_after_collection() {
        let probe = Vm::default();
        let baseline = probe.memory_usage().current_bytes;
        let allocation = probe.heap.table_allocation_bytes(4, 0).unwrap();
        let limit = baseline.checked_add(allocation).unwrap();
        let mut vm = Vm::try_new_with_memory(
            Dialect::Blu,
            MemoryConfig {
                hard_limit_bytes: Some(limit),
                gc_start_bytes: limit,
                gc_growth_percent: 50,
                max_single_allocation_bytes: usize::MAX,
            },
        )
        .unwrap();
        let retained = vm.heap.allocate_table(4, 0).unwrap();
        vm.set_global(&b"retained"[..], Value::Table(retained));

        assert_eq!(
            vm.allocate_table(4, 0, &GcRoots::default()),
            Err(RuntimeError::Heap(HeapError::Memory(
                crate::MemoryError::LimitExceeded {
                    requested: allocation,
                    used: limit,
                    limit,
                }
            )))
        );
        assert_eq!(vm.memory_usage().collections, 1);
        assert_eq!(vm.memory_usage().current_bytes, limit);
        assert_eq!(
            vm.heap.table_get(retained, &Value::Integer(1)),
            Ok(Value::Nil)
        );
    }

    #[test]
    fn automatic_collection_preserves_globals_active_frames_and_threads() {
        let mut probe = Vm::default();
        probe.heap.allocate_table(1, 0).unwrap();
        probe.heap.allocate_table(1, 0).unwrap();
        let thread_value = probe.heap.allocate_table(1, 0).unwrap();
        probe
            .heap
            .allocate_thread(&[Value::Table(thread_value)])
            .unwrap();
        probe.heap.allocate_table(4, 0).unwrap();
        let threshold = probe.memory_usage().current_bytes;

        let mut vm = Vm::try_new_with_memory(
            Dialect::Blu,
            MemoryConfig {
                hard_limit_bytes: None,
                gc_start_bytes: threshold,
                gc_growth_percent: 50,
                max_single_allocation_bytes: usize::MAX,
            },
        )
        .unwrap();
        let global = vm.heap.allocate_table(1, 0).unwrap();
        vm.set_global(&b"retained"[..], Value::Table(global));
        let active = vm.heap.allocate_table(1, 0).unwrap();
        vm.active_roots
            .push(GcRoots::from_values(&[Value::Table(active)]).unwrap());
        let thread_value = vm.heap.allocate_table(1, 0).unwrap();
        let thread = vm
            .heap
            .allocate_thread(&[Value::Table(thread_value)])
            .unwrap();
        vm.threads.insert(thread, ThreadState::Dead(None));
        vm.set_global(&b"thread"[..], Value::Thread(thread));
        let garbage = vm.heap.allocate_table(4, 0).unwrap();
        assert_eq!(vm.memory_usage().current_bytes, threshold);

        vm.allocate_table(0, 0, &GcRoots::default()).unwrap();

        assert_eq!(vm.memory_usage().collections, 1);
        assert_eq!(
            vm.heap.table_get(global, &Value::Integer(1)),
            Ok(Value::Nil)
        );
        assert_eq!(
            vm.heap.table_get(active, &Value::Integer(1)),
            Ok(Value::Nil)
        );
        assert_eq!(
            vm.heap.table_get(thread_value, &Value::Integer(1)),
            Ok(Value::Nil)
        );
        assert!(vm.heap.contains_thread(thread));
        assert!(vm.threads.contains_key(&thread));
        assert_eq!(
            vm.heap.table_get(garbage, &Value::Integer(1)),
            Err(HeapError::StaleTable(garbage))
        );
    }

    #[test]
    fn every_vm_heap_allocator_enforces_the_object_limit() {
        let artifact = Arc::new(
            validated_blu_program(
                SemanticProfile::Blu,
                Vec::new(),
                vec![BluInstruction::Return { first: 0, count: 0 }],
                FeatureBits::BASELINE,
                0,
            )
            .into_artifact(),
        );

        let mut table_vm = Vm::default();
        let table_limit = table_vm.heap.live_objects();
        table_vm.heap_object_limit = table_limit;
        assert_eq!(
            table_vm.allocate_table(0, 0, &GcRoots::default()),
            Err(RuntimeError::HeapObjectLimit {
                required: table_limit + 1,
                limit: table_limit,
            })
        );

        let mut upvalue_vm = Vm::default();
        let upvalue_limit = upvalue_vm.heap.live_objects();
        upvalue_vm.heap_object_limit = upvalue_limit;
        assert_eq!(
            upvalue_vm.allocate_upvalue(Value::Nil, &GcRoots::default()),
            Err(RuntimeError::HeapObjectLimit {
                required: upvalue_limit + 1,
                limit: upvalue_limit,
            })
        );

        let mut closure_vm = Vm::default();
        let closure_limit = closure_vm.heap.live_objects();
        closure_vm.heap_object_limit = closure_limit;
        assert_eq!(
            closure_vm.allocate_blu_closure(
                artifact,
                0,
                SemanticProfile::Blu,
                0,
                &GcRoots::default(),
            ),
            Err(RuntimeError::HeapObjectLimit {
                required: closure_limit + 1,
                limit: closure_limit,
            })
        );

        let mut thread_vm = Vm::default();
        let thread_limit = thread_vm.heap.live_objects();
        thread_vm.heap_object_limit = thread_limit;
        assert_eq!(
            thread_vm.allocate_thread(&[], &GcRoots::default()),
            Err(RuntimeError::HeapObjectLimit {
                required: thread_limit + 1,
                limit: thread_limit,
            })
        );
    }

    #[test]
    fn object_limit_collection_makes_room_for_blu_closures() {
        let artifact = Arc::new(
            validated_blu_program(
                SemanticProfile::Blu,
                Vec::new(),
                vec![BluInstruction::Return { first: 0, count: 0 }],
                FeatureBits::BASELINE,
                0,
            )
            .into_artifact(),
        );
        let mut vm = Vm::default();
        let garbage = vm.heap.allocate_table(0, 0).unwrap();
        vm.heap_object_limit = vm.heap.live_objects();

        let closure = vm
            .allocate_blu_closure(artifact, 0, SemanticProfile::Blu, 0, &GcRoots::default())
            .unwrap();

        assert!(vm.heap.blu_closure_parts(closure).is_ok());
        assert_eq!(
            vm.heap.table_get(garbage, &Value::Integer(1)),
            Err(HeapError::StaleTable(garbage))
        );
    }
}
