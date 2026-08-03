use crate::{
    MemoryAccount, MemoryConfig, MemoryError, MemoryUsage, Value, checked_hash_bytes,
    checked_vector_bytes,
};
use blu_bytecode::{Chunk, blu::Artifact as BluArtifact};
use blu_core::SemanticProfile;
use core::fmt;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    hash::Hash,
    sync::Arc,
};

const HASH_ITERATION_ORDER_THRESHOLD: usize = 1024;
const MIN_ACCOUNTED_STRING_BYTES: usize = 4096;

macro_rules! object_id {
    ($name:ident, $label:literal) => {
        #[derive(Clone, Copy, Eq, Hash, PartialEq)]
        pub struct $name {
            index: u32,
            generation: u32,
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($label, "({}:{})"), self.index, self.generation)
            }
        }

        impl From<$name> for ObjectId {
            fn from(value: $name) -> Self {
                Self {
                    index: value.index,
                    generation: value.generation,
                }
            }
        }
    };
}

object_id!(TableId, "Table");
object_id!(ClosureId, "Closure");
object_id!(UpvalueId, "Upvalue");
object_id!(ThreadId, "Thread");
object_id!(UserDataId, "UserData");

impl UpvalueId {
    pub(crate) const fn from_light_userdata_token(token: u64) -> Self {
        Self {
            index: token as u32,
            generation: (token >> 32) as u32,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct ObjectId {
    index: u32,
    generation: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FinalizerTarget {
    Table(TableId),
    UserData(UserDataId),
}

/// Selects the guest-visible traversal policy for tables allocated by a VM.
///
/// Host-created heap tables retain the historical unordered policy. Blu keeps
/// its insertion-compatible guest order, while Luau walks the hash slots in
/// their deterministic bucket order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HashIterationOrder {
    Unordered,
    Insertion,
    Luau,
}

type Finalizer = (FinalizerTarget, Value, SemanticProfile, u64);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CollectionStats {
    pub before: usize,
    pub retained: usize,
    pub collected: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HeapError {
    Memory(MemoryError),
    StaleTable(TableId),
    StaleClosure(ClosureId),
    ClosureFormat {
        closure: ClosureId,
        expected: &'static str,
    },
    StaleUpvalue(UpvalueId),
    StaleThread(ThreadId),
    StaleUserData(UserDataId),
    NilKey,
    NanKey,
    InvalidIterationKey,
    FrozenTable,
    AlreadyFrozen,
}

impl fmt::Display for HeapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Memory(error) => error.fmt(f),
            Self::StaleTable(value) => write!(f, "stale or invalid table handle {value:?}"),
            Self::StaleClosure(value) => write!(f, "stale or invalid closure handle {value:?}"),
            Self::ClosureFormat { closure, expected } => {
                write!(f, "closure {closure:?} is not a {expected} closure")
            }
            Self::StaleUpvalue(value) => write!(f, "stale or invalid upvalue handle {value:?}"),
            Self::StaleThread(value) => write!(f, "stale or invalid thread handle {value:?}"),
            Self::StaleUserData(value) => write!(f, "stale or invalid userdata handle {value:?}"),
            Self::NilKey => f.write_str("table index is nil"),
            Self::NanKey => f.write_str("table index is NaN"),
            Self::InvalidIterationKey => f.write_str("invalid key to table iteration"),
            Self::FrozenTable => f.write_str("attempt to modify a frozen table"),
            Self::AlreadyFrozen => f.write_str("table is already frozen"),
        }
    }
}

impl From<MemoryError> for HeapError {
    fn from(error: MemoryError) -> Self {
        Self::Memory(error)
    }
}

impl std::error::Error for HeapError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Memory(error) => Some(error),
            _ => None,
        }
    }
}

/// A generational object arena with deterministic logical memory accounting.
///
/// Cloning a heap duplicates its object graph and accounting snapshot. The
/// original and clone then account and collect independently; handles retain
/// their numeric identities but belong to the corresponding heap snapshot.
///
/// `Heap` is the low-level embedding surface and does not retain handles
/// returned by allocation or lookup methods. Callers must include every live
/// returned handle in the roots passed to [`Self::collect`]. Hosts using
/// [`crate::Vm`] should call [`crate::Vm::retain_value`] for handles cloned
/// through [`crate::Vm::heap`] that must outlive their current VM root.
#[derive(Clone, Debug, Default)]
pub struct Heap {
    slots: Vec<Slot>,
    free: Vec<u32>,
    live_indices: Vec<u32>,
    live: usize,
    memory: MemoryAccount,
    string_bytes: usize,
    has_weak_tables: bool,
    next_finalizer_order: u64,
}

impl Heap {
    /// Creates an empty heap with deterministic logical-capacity accounting.
    ///
    /// Accounted storage is limited to one logical `Slot` charge for every
    /// arena index ever created plus heap-owned table, closure, and thread
    /// logical capacities. Slot charges persist when an index moves to the
    /// free list and are reused with that index.
    ///
    /// These deterministic charges are not actual `Vec`/`HashMap` capacities
    /// or process memory confinement. Allocator slack, the persistent free
    /// list, strings, chunks, native-owned values, and collection work queues
    /// are not metered.
    pub fn try_new(memory: MemoryConfig) -> Result<Self, HeapError> {
        Ok(Self {
            slots: Vec::new(),
            free: Vec::new(),
            live_indices: Vec::new(),
            live: 0,
            memory: MemoryAccount::new(memory),
            string_bytes: 0,
            has_weak_tables: false,
            next_finalizer_order: 0,
        })
    }

    #[must_use]
    pub const fn memory_usage(&self) -> MemoryUsage {
        self.memory.usage()
    }

    /// Returns the live-object view used by `collectgarbage("count")`.
    ///
    /// Arena slots are retained for generational handle reuse after an object
    /// is collected, but PUC Lua's public count is about live storage. Remove
    /// those dead-slot charges while preserving all live and external charges.
    pub(crate) fn collectgarbage_bytes(&self) -> usize {
        let dead_slots = self.slots.len().saturating_sub(self.live);
        let dead_slot_bytes = dead_slots.saturating_mul(core::mem::size_of::<Slot>());
        self.memory
            .usage()
            .current_bytes
            .saturating_sub(dead_slot_bytes)
    }

    #[must_use]
    pub(crate) const fn should_collect(&self, requested: usize) -> bool {
        self.memory.should_collect(requested)
    }

    #[must_use]
    pub(crate) const fn has_weak_tables(&self) -> bool {
        self.has_weak_tables
    }

    pub(crate) fn charge_external(&mut self, bytes: usize) -> Result<(), HeapError> {
        self.memory.reserve(bytes)?.commit();
        Ok(())
    }

    pub(crate) fn release_external(&mut self, bytes: usize) -> Result<(), HeapError> {
        self.memory.release(bytes)?;
        Ok(())
    }

    pub(crate) fn reconcile_string_bytes(&mut self) -> Result<(), HeapError> {
        let desired = self.live_string_bytes()?;
        if desired > self.string_bytes {
            let additional = desired - self.string_bytes;
            self.memory.reserve(additional)?.commit();
        } else if desired < self.string_bytes {
            self.memory.release(self.string_bytes - desired)?;
        }
        self.string_bytes = desired;
        Ok(())
    }

    fn live_string_bytes(&self) -> Result<usize, HeapError> {
        let mut seen = HashSet::new();
        let mut total = 0usize;
        for index in &self.live_indices {
            let Some(object) = self
                .slots
                .get(*index as usize)
                .and_then(|slot| slot.object.as_ref())
            else {
                continue;
            };
            match object {
                Object::Table(table) => {
                    for value in &table.array {
                        add_string_bytes(value, &mut seen, &mut total)?;
                    }
                    for (key, value) in &table.hash {
                        if !matches!(value, Value::Nil) {
                            add_key_string_bytes(key, &mut seen, &mut total)?;
                        }
                        add_string_bytes(value, &mut seen, &mut total)?;
                    }
                }
                Object::Upvalue(value) => {
                    add_string_bytes(value, &mut seen, &mut total)?;
                }
                Object::Thread(thread) => {
                    for value in &thread.roots {
                        add_string_bytes(value, &mut seen, &mut total)?;
                    }
                }
                Object::UserData(userdata) => {
                    for value in &userdata.user_values {
                        add_string_bytes(value, &mut seen, &mut total)?;
                    }
                }
                Object::Closure(_) | Object::NativeClosure { .. } => {}
            }
        }
        Ok(total)
    }

    pub(crate) fn table_allocation_bytes(
        &self,
        array_capacity: usize,
        hash_capacity: usize,
    ) -> Result<usize, HeapError> {
        let array_bytes = checked_vector_bytes::<Value>(array_capacity)?;
        let hash_bytes = checked_hash_bytes::<Key, Value>(hash_capacity)?;
        self.allocation_bytes(checked_add(array_bytes, hash_bytes)?)
    }

    pub(crate) fn upvalue_allocation_bytes(&self) -> Result<usize, HeapError> {
        self.allocation_bytes(0)
    }

    pub(crate) fn closure_allocation_bytes(
        &self,
        upvalue_capacity: usize,
    ) -> Result<usize, HeapError> {
        self.allocation_bytes(checked_vector_bytes::<UpvalueId>(upvalue_capacity)?)
    }

    pub(crate) fn thread_allocation_bytes(&self, root_capacity: usize) -> Result<usize, HeapError> {
        self.allocation_bytes(checked_vector_bytes::<Value>(root_capacity)?)
    }

    pub fn allocate_table(
        &mut self,
        array_capacity: usize,
        hash_capacity: usize,
    ) -> Result<TableId, HeapError> {
        self.allocate_table_with_hash_order(array_capacity, hash_capacity, false)
    }

    pub(crate) fn allocate_table_with_hash_order(
        &mut self,
        array_capacity: usize,
        hash_capacity: usize,
        preserve_hash_order: bool,
    ) -> Result<TableId, HeapError> {
        let order = if preserve_hash_order {
            HashIterationOrder::Insertion
        } else {
            HashIterationOrder::Unordered
        };
        self.allocate_table_with_iteration_order(array_capacity, hash_capacity, order)
    }

    pub(crate) fn allocate_table_with_iteration_order(
        &mut self,
        array_capacity: usize,
        hash_capacity: usize,
        hash_iteration_order: HashIterationOrder,
    ) -> Result<TableId, HeapError> {
        let array_bytes = checked_vector_bytes::<Value>(array_capacity)?;
        let hash_bytes = checked_hash_bytes::<Key, Value>(hash_capacity)?;
        let dynamic_bytes = checked_add(array_bytes, hash_bytes)?;
        let id = self.allocate(dynamic_bytes, || {
            let mut array = Vec::new();
            try_reserve_vec_exact(&mut array, array_capacity)?;
            let mut hash = HashMap::new();
            try_reserve_hash(&mut hash, hash_capacity)?;
            Ok(Object::Table(Table {
                array,
                array_capacity,
                legacy_array_capacity: array_capacity,
                tracks_legacy_array_layout: array_capacity > 0 || hash_capacity > 0,
                preallocated_array_boundary: false,
                nil_write_boundary: false,
                array_contains_heap_reference: false,
                hash_contains_heap_key: false,
                hash_contains_heap_value: false,
                preserve_hash_order: !matches!(hash_iteration_order, HashIterationOrder::Unordered),
                hash_iteration_order,
                hash_order_enabled: false,
                hash,
                hash_capacity,
                hash_order: Vec::new(),
                hash_order_capacity: 0,
                hash_order_positions: HashMap::new(),
                mutation: 0,
                metatable: None,
                finalizer_armed: false,
                finalizer_done: false,
                finalizer_profile: None,
                finalizer_order: 0,
                frozen: false,
            }))
        })?;
        Ok(TableId {
            index: id.index,
            generation: id.generation,
        })
    }

    pub(crate) fn allocate_upvalue(&mut self, value: Value) -> Result<UpvalueId, HeapError> {
        let id = self.allocate(0, || Ok(Object::Upvalue(value)))?;
        Ok(UpvalueId {
            index: id.index,
            generation: id.generation,
        })
    }

    pub(crate) fn allocate_closure(
        &mut self,
        chunk: Arc<Chunk>,
        prototype: usize,
        profile: SemanticProfile,
        upvalue_capacity: usize,
    ) -> Result<ClosureId, HeapError> {
        let dynamic_bytes = checked_vector_bytes::<UpvalueId>(upvalue_capacity)?;
        let id = self.allocate(dynamic_bytes, || {
            let mut upvalues = Vec::new();
            try_reserve_vec_exact(&mut upvalues, upvalue_capacity)?;
            Ok(Object::Closure(Closure {
                code: ClosureCode::Luau(chunk),
                prototype,
                profile,
                upvalues,
                upvalue_capacity,
                environment: None,
            }))
        })?;
        Ok(ClosureId {
            index: id.index,
            generation: id.generation,
        })
    }

    pub(crate) fn allocate_blu_closure(
        &mut self,
        artifact: Arc<BluArtifact>,
        prototype: usize,
        profile: SemanticProfile,
        upvalue_capacity: usize,
    ) -> Result<ClosureId, HeapError> {
        let dynamic_bytes = checked_vector_bytes::<UpvalueId>(upvalue_capacity)?;
        let id = self.allocate(dynamic_bytes, || {
            let mut upvalues = Vec::new();
            try_reserve_vec_exact(&mut upvalues, upvalue_capacity)?;
            Ok(Object::Closure(Closure {
                code: ClosureCode::Blu(artifact),
                prototype,
                profile,
                upvalues,
                upvalue_capacity,
                environment: None,
            }))
        })?;
        Ok(ClosureId {
            index: id.index,
            generation: id.generation,
        })
    }

    pub(crate) fn allocate_native_closure(
        &mut self,
        function: u32,
        upvalues: Vec<UpvalueId>,
    ) -> Result<ClosureId, HeapError> {
        let upvalue_capacity = upvalues.len();
        let dynamic_bytes = checked_vector_bytes::<UpvalueId>(upvalue_capacity)?;
        let id = self.allocate(dynamic_bytes, || {
            Ok(Object::NativeClosure {
                function,
                upvalues,
                upvalue_capacity,
            })
        })?;
        Ok(ClosureId {
            index: id.index,
            generation: id.generation,
        })
    }

    pub(crate) fn allocate_thread(&mut self, roots: &[Value]) -> Result<ThreadId, HeapError> {
        let root_capacity = roots.len();
        let dynamic_bytes = checked_vector_bytes::<Value>(root_capacity)?;
        let id = self.allocate(dynamic_bytes, || {
            Ok(Object::Thread(Thread {
                roots: try_clone_slice(roots)?,
                root_capacity,
                upvalues: Vec::new(),
                upvalue_capacity: 0,
            }))
        })?;
        Ok(ThreadId {
            index: id.index,
            generation: id.generation,
        })
    }

    pub fn allocate_userdata(&mut self, tag: Arc<[u8]>) -> Result<UserDataId, HeapError> {
        self.allocate_userdata_with_capacity(tag, 1)
    }

    pub(crate) fn userdata_allocation_bytes_with_capacity(
        &self,
        user_value_capacity: usize,
    ) -> Result<usize, HeapError> {
        self.allocation_bytes(checked_vector_bytes::<Value>(user_value_capacity)?)
    }

    pub fn allocate_userdata_with_capacity(
        &mut self,
        tag: Arc<[u8]>,
        user_value_capacity: usize,
    ) -> Result<UserDataId, HeapError> {
        let dynamic_bytes = checked_vector_bytes::<Value>(user_value_capacity)?;
        let id = self.allocate(dynamic_bytes, || {
            let mut user_values = Vec::new();
            try_reserve_vec_exact(&mut user_values, user_value_capacity)?;
            user_values.resize(user_value_capacity, Value::Nil);
            Ok(Object::UserData(UserData {
                tag,
                metatable: None,
                user_values,
                user_value_capacity,
                finalizer_armed: false,
                finalizer_done: false,
                finalizer_profile: None,
                finalizer_order: 0,
            }))
        })?;
        Ok(UserDataId {
            index: id.index,
            generation: id.generation,
        })
    }

    pub fn userdata_tag(&self, userdata: UserDataId) -> Result<Arc<[u8]>, HeapError> {
        match self.object(userdata.into()) {
            Some(Object::UserData(value)) => Ok(Arc::clone(&value.tag)),
            _ => Err(HeapError::StaleUserData(userdata)),
        }
    }

    pub fn userdata_metatable(&self, userdata: UserDataId) -> Result<Option<TableId>, HeapError> {
        match self.object(userdata.into()) {
            Some(Object::UserData(value)) => Ok(value.metatable),
            _ => Err(HeapError::StaleUserData(userdata)),
        }
    }

    pub fn set_userdata_metatable(
        &mut self,
        userdata: UserDataId,
        metatable: Option<TableId>,
    ) -> Result<(), HeapError> {
        if let Some(metatable) = metatable {
            self.table(metatable)?;
        }
        match self.object_mut(userdata.into()) {
            Some(Object::UserData(value)) => {
                value.metatable = metatable;
                value.finalizer_armed = false;
                value.finalizer_done = false;
                value.finalizer_profile = None;
                value.finalizer_order = 0;
                Ok(())
            }
            _ => Err(HeapError::StaleUserData(userdata)),
        }
    }

    pub(crate) fn contains_userdata(&self, userdata: UserDataId) -> bool {
        matches!(self.object(userdata.into()), Some(Object::UserData(_)))
    }

    pub(crate) fn userdata_finalizer_done(&self, userdata: UserDataId) -> Result<bool, HeapError> {
        match self.object(userdata.into()) {
            Some(Object::UserData(value)) => Ok(value.finalizer_done),
            _ => Err(HeapError::StaleUserData(userdata)),
        }
    }

    pub(crate) fn set_userdata_finalizer_state(
        &mut self,
        userdata: UserDataId,
        armed: bool,
        done: bool,
        profile: Option<SemanticProfile>,
    ) -> Result<(), HeapError> {
        let order = if armed && !done {
            let order = self.next_finalizer_order;
            self.next_finalizer_order = self.next_finalizer_order.wrapping_add(1);
            Some(order)
        } else {
            None
        };
        let value = match self.object_mut(userdata.into()) {
            Some(Object::UserData(value)) => value,
            _ => return Err(HeapError::StaleUserData(userdata)),
        };
        value.finalizer_armed = armed;
        value.finalizer_done = done;
        value.finalizer_profile = if armed { profile } else { None };
        if let Some(order) = order {
            value.finalizer_order = order;
        }
        Ok(())
    }

    pub(crate) fn userdata_user_value(
        &self,
        userdata: UserDataId,
        index: usize,
    ) -> Result<Option<Value>, HeapError> {
        if index == 0 {
            return Ok(None);
        }
        match self.object(userdata.into()) {
            Some(Object::UserData(value)) => Ok(value.user_values.get(index - 1).cloned()),
            _ => Err(HeapError::StaleUserData(userdata)),
        }
    }

    pub(crate) fn userdata_set_user_value(
        &mut self,
        userdata: UserDataId,
        index: usize,
        user_value: Value,
    ) -> Result<bool, HeapError> {
        if index == 0 {
            return Ok(false);
        }
        match self.object_mut(userdata.into()) {
            Some(Object::UserData(value)) => {
                let Some(slot) = value.user_values.get_mut(index - 1) else {
                    return Ok(false);
                };
                *slot = user_value;
                Ok(true)
            }
            _ => Err(HeapError::StaleUserData(userdata)),
        }
    }

    #[cfg(test)]
    pub(crate) fn thread_set_roots(
        &mut self,
        thread: ThreadId,
        roots: &[Value],
    ) -> Result<(), HeapError> {
        self.thread_set_gc_roots(thread, roots, &[])
    }

    pub(crate) fn thread_set_gc_roots(
        &mut self,
        thread: ThreadId,
        roots: &[Value],
        upvalues: &[UpvalueId],
    ) -> Result<(), HeapError> {
        let (old_root_capacity, old_upvalue_capacity) = match self.object(thread.into()) {
            Some(Object::Thread(value)) => (value.root_capacity, value.upvalue_capacity),
            _ => return Err(HeapError::StaleThread(thread)),
        };
        let old_bytes = checked_add(
            checked_vector_bytes::<Value>(old_root_capacity)?,
            checked_vector_bytes::<UpvalueId>(old_upvalue_capacity)?,
        )?;
        let root_capacity = roots.len();
        let upvalue_capacity = upvalues.len();
        let new_bytes = checked_add(
            checked_vector_bytes::<Value>(root_capacity)?,
            checked_vector_bytes::<UpvalueId>(upvalue_capacity)?,
        )?;
        let reservation = self.memory.reserve(new_bytes)?;
        let roots = try_clone_slice(roots)?;
        let upvalues = try_clone_slice(upvalues)?;
        reservation.commit_replacing(old_bytes)?;
        match self.object_mut(thread.into()) {
            Some(Object::Thread(value)) => {
                value.roots = roots;
                value.root_capacity = root_capacity;
                value.upvalues = upvalues;
                value.upvalue_capacity = upvalue_capacity;
                Ok(())
            }
            _ => Err(HeapError::StaleThread(thread)),
        }
    }

    pub(crate) fn thread_set_gc_roots_bytes(
        &self,
        thread: ThreadId,
        root_capacity: usize,
        upvalue_capacity: usize,
    ) -> Result<usize, HeapError> {
        match self.object(thread.into()) {
            Some(Object::Thread(_)) => Ok(checked_add(
                checked_vector_bytes::<Value>(root_capacity)?,
                checked_vector_bytes::<UpvalueId>(upvalue_capacity)?,
            )?),
            _ => Err(HeapError::StaleThread(thread)),
        }
    }

    pub(crate) fn contains_thread(&self, thread: ThreadId) -> bool {
        matches!(self.object(thread.into()), Some(Object::Thread(_)))
    }

    #[must_use]
    pub const fn live_objects(&self) -> usize {
        self.live
    }

    /// Reads a table value without adding a GC root for a returned heap handle.
    ///
    /// Standalone heap callers must pass any live returned handle to
    /// [`Self::collect`]. VM callers can explicitly retain it with
    /// [`crate::Vm::retain_value`].
    pub fn table_get(&self, table: TableId, key: &Value) -> Result<Value, HeapError> {
        let table = self.table(table)?;
        // Lua treats nil and NaN reads as absent keys.  Writes still pass
        // through `Key::from_value` below so both remain rejected for table
        // mutation, matching the language's read/write asymmetry.
        if matches!(key, Value::Nil) || matches!(key, Value::Number(value) if value.is_nan()) {
            return Ok(Value::Nil);
        }
        let key = Key::from_value(key)?;
        Ok(table.get(&key).cloned().unwrap_or(Value::Nil))
    }

    pub fn table_mutation(&self, table: TableId) -> Result<u64, HeapError> {
        Ok(self.table(table)?.mutation)
    }

    pub fn table_set(&mut self, table: TableId, key: Value, value: Value) -> Result<(), HeapError> {
        let key = Key::from_value(&key)?;
        let mode_mutation = matches!(&key, Key::String(name) if name.as_ref() == b"__mode")
            && self.table_is_metatable(table)?;
        let Self { slots, memory, .. } = self;
        let table = table_mut_in_slots(slots, table)?;
        if table.frozen {
            return Err(HeapError::FrozenTable);
        }
        table.mutation = table.mutation.wrapping_add(1);
        let result = table.set(key, value, memory);
        if result.is_ok() && mode_mutation {
            self.has_weak_tables = true;
        }
        result
    }

    pub(crate) fn table_set_bytes(
        &self,
        table: TableId,
        key: &Value,
        value: &Value,
    ) -> Result<usize, HeapError> {
        let key = Key::from_value(key)?;
        let table = self.table(table)?;
        if table.frozen {
            return Err(HeapError::FrozenTable);
        }
        table.set_bytes(&key, value)
    }

    pub fn table_length(&self, table: TableId) -> Result<usize, HeapError> {
        Ok(self.table(table)?.length())
    }

    /// Returns the compact length boundary used by Lua 5.5 and Blu.
    pub fn table_modern_length(&self, table: TableId) -> Result<usize, HeapError> {
        Ok(self.table(table)?.modern_length())
    }

    /// Returns Luau's length boundary for assignment-created sparse tables.
    pub fn table_luau_length(&self, table: TableId) -> Result<usize, HeapError> {
        let table = self.table(table)?;
        if !table.tracks_legacy_array_layout
            && table.nil_write_boundary
            && table
                .hash
                .get(&Key::Integer(2))
                .is_some_and(|value| !matches!(value, Value::Nil))
            && table
                .hash
                .get(&Key::Integer(3))
                .is_none_or(|value| matches!(value, Value::Nil))
        {
            return Ok(2);
        }
        Ok(table.legacy_length())
    }

    /// Returns the length boundary used by the legacy Lua table APIs.
    ///
    /// Legacy Lua keeps an array-part boundary even when a literal contains a
    /// hole. Numeric keys beyond that allocated array part remain hash keys and
    /// do not extend `#`, `rawlen`, or `table.getn`.
    pub fn table_legacy_length(&self, table: TableId) -> Result<usize, HeapError> {
        Ok(self.table(table)?.legacy_length())
    }

    pub fn table_max_numeric_key(&self, table: TableId) -> Result<f64, HeapError> {
        let table = self.table(table)?;
        let mut maximum = table
            .array
            .iter()
            .rposition(|value| !matches!(value, Value::Nil))
            .map_or(0.0, |index| (index + 1) as f64);
        for (key, value) in &table.hash {
            if matches!(value, Value::Nil) {
                continue;
            }
            let number = match key {
                Key::Integer(value) => *value as f64,
                Key::Number(bits) => f64::from_bits(*bits),
                _ => continue,
            };
            if number > maximum {
                maximum = number;
            }
        }
        Ok(maximum)
    }

    pub fn table_clear(&mut self, table: TableId) -> Result<(), HeapError> {
        let table = self.table_mut(table)?;
        if table.frozen {
            return Err(HeapError::FrozenTable);
        }
        table.mutation = table.mutation.wrapping_add(1);
        let had_array = !table.array.is_empty();
        table.array.fill(Value::Nil);
        table.hash.clear();
        table.hash_order.clear();
        table.hash_order_positions.clear();
        table.hash_order_enabled = false;
        if had_array {
            table.preallocated_array_boundary = true;
        }
        Ok(())
    }

    pub(crate) fn table_mark_preallocated_array_boundary(
        &mut self,
        table: TableId,
    ) -> Result<(), HeapError> {
        self.table_mut(table)?.preallocated_array_boundary = true;
        Ok(())
    }

    pub(crate) fn table_has_preallocated_array_boundary(
        &self,
        table: TableId,
    ) -> Result<bool, HeapError> {
        Ok(self.table(table)?.preallocated_array_boundary)
    }

    pub(crate) fn table_array_capacity(&self, table: TableId) -> Result<usize, HeapError> {
        Ok(self.table(table)?.array_capacity)
    }

    pub fn table_is_frozen(&self, table: TableId) -> Result<bool, HeapError> {
        Ok(self.table(table)?.frozen)
    }

    pub fn table_freeze(&mut self, table: TableId) -> Result<(), HeapError> {
        let table = self.table_mut(table)?;
        if table.frozen {
            return Err(HeapError::AlreadyFrozen);
        }
        table.frozen = true;
        Ok(())
    }

    pub fn table_metatable(&self, table: TableId) -> Result<Option<TableId>, HeapError> {
        Ok(self.table(table)?.metatable)
    }

    pub fn set_table_metatable(
        &mut self,
        table: TableId,
        metatable: Option<TableId>,
    ) -> Result<(), HeapError> {
        if let Some(metatable) = metatable {
            self.table(metatable)?;
        }
        let declares_weak_mode = metatable
            .and_then(|metatable| {
                self.table_get(metatable, &Value::String(Arc::from(&b"__mode"[..])))
                    .ok()
            })
            .is_some_and(|value| {
                matches!(
                    value,
                    Value::String(mode) if mode.iter().any(|byte| matches!(byte, b'k' | b'v'))
                )
            });
        let table = self.table_mut(table)?;
        if table.frozen {
            return Err(HeapError::FrozenTable);
        }
        table.metatable = metatable;
        table.finalizer_armed = false;
        table.finalizer_done = false;
        table.finalizer_profile = None;
        table.finalizer_order = 0;
        if declares_weak_mode {
            self.has_weak_tables = true;
        }
        Ok(())
    }

    fn table_is_metatable(&self, candidate: TableId) -> Result<bool, HeapError> {
        self.table(candidate)?;
        Ok(self.live_indices.iter().any(|index| {
            matches!(
                self.slots
                    .get(*index as usize)
                    .and_then(|slot| slot.object.as_ref()),
                Some(Object::Table(table)) if table.metatable == Some(candidate)
            )
        }))
    }

    pub(crate) fn table_finalizer_done(&self, table: TableId) -> Result<bool, HeapError> {
        Ok(self.table(table)?.finalizer_done)
    }

    pub(crate) fn set_table_finalizer_state(
        &mut self,
        table: TableId,
        armed: bool,
        done: bool,
        profile: Option<SemanticProfile>,
    ) -> Result<(), HeapError> {
        let order = if armed && !done {
            let order = self.next_finalizer_order;
            self.next_finalizer_order = self.next_finalizer_order.wrapping_add(1);
            Some(order)
        } else {
            None
        };
        let table = self.table_mut(table)?;
        table.finalizer_armed = armed;
        table.finalizer_done = done;
        table.finalizer_profile = if armed { profile } else { None };
        if let Some(order) = order {
            table.finalizer_order = order;
        }
        Ok(())
    }

    /// Iterates a table without adding GC roots for returned heap handles.
    ///
    /// Standalone heap callers must pass any live returned handles to
    /// [`Self::collect`]. VM callers can explicitly retain them with
    /// [`crate::Vm::retain_values`].
    pub fn table_next(
        &self,
        table: TableId,
        key: &Value,
    ) -> Result<Option<(Value, Value)>, HeapError> {
        let table = self.table(table)?;
        let array_start = if matches!(key, Value::Nil) {
            0
        } else {
            let key = Key::from_value(key)?;
            if let Some(index) = key.array_index()
                && index <= table.array.len()
            {
                let present = !matches!(table.array[index - 1], Value::Nil)
                    || table
                        .hash
                        .get(&key)
                        .is_some_and(|value| matches!(value, Value::Nil));
                if !present {
                    return Err(HeapError::InvalidIterationKey);
                }
                index
            } else {
                if table.hash_order_capacity != 0 {
                    if !table.hash.contains_key(&key) {
                        return Err(HeapError::InvalidIterationKey);
                    }
                    let luau_order;
                    let order = if matches!(table.hash_iteration_order, HashIterationOrder::Luau) {
                        luau_order = table.luau_hash_order();
                        &luau_order
                    } else {
                        &table.hash_order
                    };
                    let Some(position) = order.iter().position(|candidate| candidate == &key)
                    else {
                        return Err(HeapError::InvalidIterationKey);
                    };
                    if let Some(candidate) = order.get(position + 1)
                        && let Some(value) = table.hash.get(candidate)
                        && !matches!(value, Value::Nil)
                    {
                        return Ok(Some((candidate.to_value(), value.clone())));
                    }
                    return Ok(order[position + 1..].iter().find_map(|candidate| {
                        table
                            .hash
                            .get(candidate)
                            .filter(|value| !matches!(value, Value::Nil))
                            .map(|value| (candidate.to_value(), value.clone()))
                    }));
                }
                let mut found = false;
                for (candidate, value) in &table.hash {
                    if !found {
                        if candidate == &key {
                            found = true;
                        }
                        continue;
                    }
                    if !matches!(value, Value::Nil) {
                        return Ok(Some((candidate.to_value(), value.clone())));
                    }
                }
                return if found {
                    Ok(None)
                } else {
                    Err(HeapError::InvalidIterationKey)
                };
            }
        };

        if let Some(value) = table.array.get(array_start)
            && !matches!(value, Value::Nil)
        {
            return Ok(Some((
                Value::Integer((array_start + 1) as i64),
                value.clone(),
            )));
        }
        for (index, value) in table.array.iter().enumerate().skip(array_start + 1) {
            if !matches!(value, Value::Nil) {
                return Ok(Some((Value::Integer((index + 1) as i64), value.clone())));
            }
        }
        if table.hash_order_capacity != 0 {
            let luau_order;
            let order = if matches!(table.hash_iteration_order, HashIterationOrder::Luau) {
                luau_order = table.luau_hash_order();
                &luau_order
            } else {
                &table.hash_order
            };
            return Ok(order.iter().find_map(|key| {
                table
                    .hash
                    .get(key)
                    .filter(|value| !matches!(value, Value::Nil))
                    .map(|value| (key.to_value(), value.clone()))
            }));
        }
        Ok(table
            .hash
            .iter()
            .find(|(_, value)| !matches!(value, Value::Nil))
            .map(|(key, value)| (key.to_value(), value.clone())))
    }

    pub(crate) fn closure_parts(
        &self,
        closure_id: ClosureId,
    ) -> Result<(Arc<Chunk>, usize, SemanticProfile, Vec<UpvalueId>), HeapError> {
        let closure = self.closure(closure_id)?;
        let ClosureCode::Luau(chunk) = &closure.code else {
            return Err(HeapError::ClosureFormat {
                closure: closure_id,
                expected: "Luau",
            });
        };
        Ok((
            chunk.clone(),
            closure.prototype,
            closure.profile,
            try_clone_slice(&closure.upvalues)?,
        ))
    }

    pub(crate) fn blu_closure_parts(
        &self,
        closure: ClosureId,
    ) -> Result<(Arc<BluArtifact>, usize, SemanticProfile, Vec<UpvalueId>), HeapError> {
        let value = self.closure(closure)?;
        let ClosureCode::Blu(artifact) = &value.code else {
            return Err(HeapError::ClosureFormat {
                closure,
                expected: "BluV1",
            });
        };
        Ok((
            artifact.clone(),
            value.prototype,
            value.profile,
            try_clone_slice(&value.upvalues)?,
        ))
    }

    pub(crate) fn is_blu_closure(&self, closure: ClosureId) -> Result<bool, HeapError> {
        match self.object(closure.into()) {
            Some(Object::Closure(closure)) => Ok(matches!(closure.code, ClosureCode::Blu(_))),
            Some(Object::NativeClosure { .. }) => Ok(false),
            _ => Err(HeapError::StaleClosure(closure)),
        }
    }

    pub(crate) fn native_closure_function(
        &self,
        closure: ClosureId,
    ) -> Result<Option<u32>, HeapError> {
        match self.object(closure.into()) {
            Some(Object::Closure(_)) => Ok(None),
            Some(Object::NativeClosure { function, .. }) => Ok(Some(*function)),
            _ => Err(HeapError::StaleClosure(closure)),
        }
    }

    pub(crate) fn native_closure_upvalue(
        &self,
        closure: ClosureId,
    ) -> Result<Option<UpvalueId>, HeapError> {
        match self.object(closure.into()) {
            Some(Object::Closure(_)) => Ok(None),
            Some(Object::NativeClosure { upvalues, .. }) => Ok(upvalues.first().copied()),
            _ => Err(HeapError::StaleClosure(closure)),
        }
    }

    pub(crate) fn native_closure_upvalues(
        &self,
        closure: ClosureId,
    ) -> Result<Option<Vec<UpvalueId>>, HeapError> {
        match self.object(closure.into()) {
            Some(Object::Closure(_)) => Ok(None),
            Some(Object::NativeClosure { upvalues, .. }) => Ok(Some(upvalues.clone())),
            _ => Err(HeapError::StaleClosure(closure)),
        }
    }

    pub(crate) fn contains_closure(&self, closure: ClosureId) -> bool {
        matches!(
            self.object(closure.into()),
            Some(Object::Closure(_) | Object::NativeClosure { .. })
        )
    }

    pub(crate) fn blu_closure_environment(
        &self,
        closure: ClosureId,
    ) -> Result<Option<TableId>, HeapError> {
        let closure_value = self.closure(closure)?;
        if !matches!(closure_value.code, ClosureCode::Blu(_)) {
            return Err(HeapError::ClosureFormat {
                closure,
                expected: "BluV1",
            });
        }
        Ok(closure_value.environment)
    }

    pub(crate) fn closure_set_environment(
        &mut self,
        closure: ClosureId,
        environment: TableId,
    ) -> Result<(), HeapError> {
        self.table(environment)?;
        let closure_value = self.closure_mut(closure)?;
        if !matches!(closure_value.code, ClosureCode::Blu(_)) {
            return Err(HeapError::ClosureFormat {
                closure,
                expected: "BluV1",
            });
        }
        closure_value.environment = Some(environment);
        Ok(())
    }

    pub(crate) fn closure_push_upvalue(
        &mut self,
        closure: ClosureId,
        upvalue: UpvalueId,
    ) -> Result<(), HeapError> {
        let Self { slots, memory, .. } = self;
        let closure_value = closure_in_slots(slots, closure)?;
        let old_capacity = closure_value.upvalue_capacity;
        let required = closure_value
            .upvalues
            .len()
            .checked_add(1)
            .ok_or(MemoryError::SizeOverflow)?;
        if required <= old_capacity {
            match object_mut_in_slots(slots, closure.into()) {
                Some(Object::Closure(value)) => {
                    value.upvalues.push(upvalue);
                    return Ok(());
                }
                _ => return Err(HeapError::StaleClosure(closure)),
            }
        }
        let new_capacity = required;
        let old_bytes = checked_vector_bytes::<UpvalueId>(old_capacity)?;
        let new_bytes = checked_vector_bytes::<UpvalueId>(new_capacity)?;
        let reservation = memory.reserve(new_bytes)?;
        match object_mut_in_slots(slots, closure.into()) {
            Some(Object::Closure(value)) => {
                try_reserve_vec_to(&mut value.upvalues, new_capacity)?;
                reservation.commit_replacing(old_bytes)?;
                value.upvalue_capacity = new_capacity;
                value.upvalues.push(upvalue);
                Ok(())
            }
            _ => Err(HeapError::StaleClosure(closure)),
        }
    }

    pub(crate) fn closure_set_upvalue(
        &mut self,
        closure: ClosureId,
        index: usize,
        upvalue: UpvalueId,
    ) -> Result<(), HeapError> {
        let closure_id = closure;
        let closure = self.closure_mut(closure_id)?;
        let Some(slot) = closure.upvalues.get_mut(index) else {
            return Err(HeapError::ClosureFormat {
                closure: closure_id,
                expected: "closure with the requested upvalue",
            });
        };
        *slot = upvalue;
        Ok(())
    }

    pub(crate) fn closure_push_upvalue_bytes(
        &self,
        closure: ClosureId,
    ) -> Result<usize, HeapError> {
        let closure = self.closure(closure)?;
        let required = closure
            .upvalues
            .len()
            .checked_add(1)
            .ok_or(MemoryError::SizeOverflow)?;
        if required <= closure.upvalue_capacity {
            Ok(0)
        } else {
            Ok(checked_vector_bytes::<UpvalueId>(required)?)
        }
    }

    pub(crate) fn upvalue_get(&self, upvalue: UpvalueId) -> Result<Value, HeapError> {
        match self.object(upvalue.into()) {
            Some(Object::Upvalue(value)) => Ok(value.clone()),
            _ => Err(HeapError::StaleUpvalue(upvalue)),
        }
    }

    pub(crate) fn upvalue_set(
        &mut self,
        upvalue: UpvalueId,
        value: Value,
    ) -> Result<(), HeapError> {
        match self.object_mut(upvalue.into()) {
            Some(Object::Upvalue(slot)) => {
                *slot = value;
                Ok(())
            }
            _ => Err(HeapError::StaleUpvalue(upvalue)),
        }
    }

    /// Collects objects unreachable from the supplied roots.
    ///
    /// Standalone `Heap` callers own this root contract: every live heap-handle
    /// `Value`, including values returned by [`Self::table_get`] or
    /// [`Self::table_next`], must be supplied on every collection. The heap
    /// does not retain those returned values automatically.
    pub fn collect<'a>(
        &mut self,
        roots: impl IntoIterator<Item = &'a Value>,
    ) -> Result<CollectionStats, HeapError> {
        self.collect_with_upvalues(roots, std::iter::empty())
    }

    pub(crate) fn collect_with_upvalues<'a>(
        &mut self,
        roots: impl IntoIterator<Item = &'a Value>,
        upvalues: impl IntoIterator<Item = UpvalueId>,
    ) -> Result<CollectionStats, HeapError> {
        self.collect_with_upvalues_and_finalizers(roots, upvalues)
            .map(|(stats, _)| stats)
    }

    pub(crate) fn collect_with_upvalues_and_finalizers<'a>(
        &mut self,
        roots: impl IntoIterator<Item = &'a Value>,
        upvalues: impl IntoIterator<Item = UpvalueId>,
    ) -> Result<(CollectionStats, Vec<Finalizer>), HeapError> {
        let before = self.live;
        let mut queue = VecDeque::new();
        for root in roots {
            enqueue_value(root, &mut queue);
        }
        queue.extend(upvalues.into_iter().map(ObjectId::from));

        loop {
            while let Some(id) = queue.pop_front() {
                let weak_mode = self
                    .object(id)
                    .and_then(|object| match object {
                        Object::Table(table) => Some(table_weak_mode(&self.slots, table)),
                        _ => None,
                    })
                    .unwrap_or_default();
                {
                    let Some(slot) = self.slots.get_mut(id.index as usize) else {
                        continue;
                    };
                    if slot.generation != id.generation || slot.marked {
                        continue;
                    }
                    slot.marked = true;
                }
                let Some(object) = self.object(id) else {
                    continue;
                };
                match object {
                    Object::Table(table) => {
                        if let Some(metatable) = table.metatable {
                            queue.push_back(metatable.into());
                        }
                        if !weak_mode.keys && table.hash_contains_heap_key {
                            for (key, value) in &table.hash {
                                if !matches!(value, Value::Nil) {
                                    key.enqueue(&mut queue);
                                }
                            }
                        }
                        if !weak_mode.values && table.array_contains_heap_reference {
                            for value in &table.array {
                                if !matches!(value, Value::Nil) {
                                    enqueue_value(value, &mut queue);
                                }
                            }
                        }
                        if !weak_mode.values && table.hash_contains_heap_value {
                            for (key, value) in &table.hash {
                                if !matches!(value, Value::Nil)
                                    && (!weak_mode.keys || key_is_marked(&self.slots, key))
                                    && !value_is_marked(&self.slots, value)
                                {
                                    enqueue_value(value, &mut queue);
                                }
                            }
                        }
                    }
                    Object::Closure(closure) => {
                        queue.extend(closure.upvalues.iter().copied().map(ObjectId::from));
                        if let Some(environment) = closure.environment {
                            queue.push_back(environment.into());
                        }
                    }
                    Object::NativeClosure { upvalues, .. } => {
                        queue.extend(upvalues.iter().copied().map(ObjectId::from));
                    }
                    Object::Upvalue(value) => enqueue_value(value, &mut queue),
                    Object::Thread(thread) => {
                        for root in &thread.roots {
                            enqueue_value(root, &mut queue);
                        }
                        queue.extend(thread.upvalues.iter().copied().map(ObjectId::from));
                    }
                    Object::UserData(userdata) => {
                        if let Some(metatable) = userdata.metatable {
                            queue.push_back(metatable.into());
                        }
                        for value in &userdata.user_values {
                            enqueue_value(value, &mut queue);
                        }
                    }
                }
            }

            for index in self.live_indices.iter().copied() {
                let Some(slot) = self.slots.get(index as usize) else {
                    continue;
                };
                let Some(Object::Table(table)) = slot.object.as_ref() else {
                    continue;
                };
                let mode = table_weak_mode(&self.slots, table);
                if !slot.marked || !mode.keys || mode.values {
                    continue;
                }
                for (key, value) in &table.hash {
                    if !matches!(value, Value::Nil)
                        && key_is_marked(&self.slots, key)
                        && !value_is_marked(&self.slots, value)
                    {
                        enqueue_value(value, &mut queue);
                    }
                }
            }
            if queue.is_empty() {
                break;
            }
        }

        let mut finalizers = Vec::new();
        for index in self.live_indices.iter().copied() {
            let Some(slot) = self.slots.get(index as usize) else {
                continue;
            };
            let Some(object) = slot.object.as_ref() else {
                continue;
            };
            let (target, metatable, armed, done, profile, order) = match object {
                Object::Table(table) => (
                    FinalizerTarget::Table(TableId {
                        index,
                        generation: slot.generation,
                    }),
                    table.metatable,
                    table.finalizer_armed,
                    table.finalizer_done,
                    table.finalizer_profile,
                    table.finalizer_order,
                ),
                Object::UserData(userdata) => (
                    FinalizerTarget::UserData(UserDataId {
                        index,
                        generation: slot.generation,
                    }),
                    userdata.metatable,
                    userdata.finalizer_armed,
                    userdata.finalizer_done,
                    userdata.finalizer_profile,
                    userdata.finalizer_order,
                ),
                _ => continue,
            };
            if slot.marked || !armed || done {
                continue;
            }
            let Some(metatable) = metatable else {
                continue;
            };
            let Some(Object::Table(metatable)) = self
                .slots
                .get(metatable.index as usize)
                .filter(|slot| slot.generation == metatable.generation)
                .and_then(|slot| slot.object.as_ref())
            else {
                continue;
            };
            let callback = metatable
                .get(&Key::String(Arc::from(&b"__gc"[..])))
                .cloned()
                .filter(|value| matches!(value, Value::Closure(_) | Value::NativeFunction(_)));
            let Some(callback) = callback else {
                continue;
            };
            let Some(profile) = profile else {
                continue;
            };
            finalizers.push((target, callback, profile, order));
        }
        finalizers.sort_by(|left, right| right.3.cmp(&left.3));
        for (target, _, _, _) in &finalizers {
            queue.push_back(match target {
                FinalizerTarget::Table(table) => (*table).into(),
                FinalizerTarget::UserData(userdata) => (*userdata).into(),
            });
        }
        loop {
            while let Some(id) = queue.pop_front() {
                let weak_mode = self
                    .object(id)
                    .and_then(|object| match object {
                        Object::Table(table) => Some(table_weak_mode(&self.slots, table)),
                        _ => None,
                    })
                    .unwrap_or_default();
                {
                    let Some(slot) = self.slots.get_mut(id.index as usize) else {
                        continue;
                    };
                    if slot.generation != id.generation || slot.marked {
                        continue;
                    }
                    slot.marked = true;
                }
                let Some(object) = self.object(id) else {
                    continue;
                };
                match object {
                    Object::Table(table) => {
                        if let Some(metatable) = table.metatable {
                            queue.push_back(metatable.into());
                        }
                        if !weak_mode.keys && table.hash_contains_heap_key {
                            for (key, value) in &table.hash {
                                if !matches!(value, Value::Nil) {
                                    key.enqueue(&mut queue);
                                }
                            }
                        }
                        if !weak_mode.values && table.array_contains_heap_reference {
                            for value in &table.array {
                                if !matches!(value, Value::Nil) {
                                    enqueue_value(value, &mut queue);
                                }
                            }
                        }
                        if !weak_mode.values && table.hash_contains_heap_value {
                            for (key, value) in &table.hash {
                                if !matches!(value, Value::Nil)
                                    && (!weak_mode.keys || key_is_marked(&self.slots, key))
                                    && !value_is_marked(&self.slots, value)
                                {
                                    enqueue_value(value, &mut queue);
                                }
                            }
                        }
                    }
                    Object::Closure(closure) => {
                        queue.extend(closure.upvalues.iter().copied().map(ObjectId::from));
                        if let Some(environment) = closure.environment {
                            queue.push_back(environment.into());
                        }
                    }
                    Object::NativeClosure { upvalues, .. } => {
                        queue.extend(upvalues.iter().copied().map(ObjectId::from));
                    }
                    Object::Upvalue(value) => enqueue_value(value, &mut queue),
                    Object::Thread(thread) => {
                        for root in &thread.roots {
                            enqueue_value(root, &mut queue);
                        }
                        queue.extend(thread.upvalues.iter().copied().map(ObjectId::from));
                    }
                    Object::UserData(userdata) => {
                        if let Some(metatable) = userdata.metatable {
                            queue.push_back(metatable.into());
                        }
                        for value in &userdata.user_values {
                            enqueue_value(value, &mut queue);
                        }
                    }
                }
            }

            for index in self.live_indices.iter().copied() {
                let Some(slot) = self.slots.get(index as usize) else {
                    continue;
                };
                let Some(Object::Table(table)) = slot.object.as_ref() else {
                    continue;
                };
                let mode = table_weak_mode(&self.slots, table);
                if !slot.marked || !mode.keys || mode.values {
                    continue;
                }
                for (key, value) in &table.hash {
                    if !matches!(value, Value::Nil)
                        && key_is_marked(&self.slots, key)
                        && !value_is_marked(&self.slots, value)
                    {
                        enqueue_value(value, &mut queue);
                    }
                }
            }
            if queue.is_empty() {
                break;
            }
        }

        self.clear_weak_references()?;

        let sweep = self
            .live_indices
            .iter()
            .filter_map(|index| self.slots.get(*index as usize))
            .filter(|slot| !slot.marked)
            .filter_map(|slot| slot.object.as_ref())
            .try_fold((0usize, 0usize), |(total, dead), object| {
                Ok::<_, MemoryError>((
                    checked_add(total, object.dynamic_bytes()?)?,
                    dead.checked_add(1).ok_or(MemoryError::SizeOverflow)?,
                ))
            });
        let (release, dead) = match sweep {
            Ok(sweep) => sweep,
            Err(error) => {
                self.clear_marks();
                return Err(error.into());
            }
        };
        if let Err(error) = try_reserve_vec_exact(&mut self.free, dead) {
            self.clear_marks();
            return Err(error);
        }
        if let Err(error) = self.memory.release(release) {
            self.clear_marks();
            return Err(error.into());
        }

        let mut retained = 0;
        for read in 0..self.live_indices.len() {
            let index = self.live_indices[read];
            let slot = &mut self.slots[index as usize];
            if slot.marked {
                slot.marked = false;
                self.live_indices[retained] = index;
                retained += 1;
            } else {
                slot.object = None;
                slot.generation = slot.generation.wrapping_add(1);
                self.free.push(index);
                self.live -= 1;
            }
        }
        self.live_indices.truncate(retained);
        for (target, _, _, _) in &finalizers {
            match target {
                FinalizerTarget::Table(table) => {
                    if let Some(Object::Table(table)) = self.object_mut((*table).into()) {
                        table.finalizer_done = true;
                    }
                }
                FinalizerTarget::UserData(userdata) => {
                    if let Some(Object::UserData(userdata)) = self.object_mut((*userdata).into()) {
                        userdata.finalizer_done = true;
                    }
                }
            }
        }
        self.memory.finish_collection();
        Ok((
            CollectionStats {
                before,
                retained: self.live,
                collected: before - self.live,
            },
            finalizers,
        ))
    }

    fn clear_marks(&mut self) {
        for index in self.live_indices.iter().copied() {
            if let Some(slot) = self.slots.get_mut(index as usize) {
                slot.marked = false;
            }
        }
    }

    fn clear_weak_references(&mut self) -> Result<(), HeapError> {
        let mut cleanup = Vec::new();
        for index in self.live_indices.iter().copied() {
            let Some(slot) = self.slots.get(index as usize) else {
                continue;
            };
            let Some(Object::Table(table)) = slot.object.as_ref() else {
                continue;
            };
            if !slot.marked {
                continue;
            }
            let mode = table_weak_mode(&self.slots, table);
            if !mode.keys && !mode.values {
                continue;
            }
            let table_id = TableId {
                index,
                generation: slot.generation,
            };
            let mut array = Vec::new();
            if mode.values {
                for (array_index, value) in table.array.iter().enumerate() {
                    if !value_is_marked(&self.slots, value) {
                        array.push(array_index);
                    }
                }
            }
            let mut hash = Vec::new();
            for (key, value) in &table.hash {
                let dead_key = mode.keys && !key_is_marked(&self.slots, key);
                let dead_value = mode.values && !value_is_marked(&self.slots, value);
                if dead_key || dead_value {
                    hash.push(key.clone());
                }
            }
            if !array.is_empty() || !hash.is_empty() {
                cleanup.push((table_id, array, hash));
            }
        }
        for (table_id, array, hash) in cleanup {
            let table = table_mut_in_slots(&mut self.slots, table_id)?;
            for index in array {
                if let Some(value) = table.array.get_mut(index) {
                    *value = Value::Nil;
                }
            }
            for key in hash {
                table.remove_hash_entry(&key);
            }
            table.trim_array();
        }
        Ok(())
    }

    fn allocate(
        &mut self,
        dynamic_bytes: usize,
        build: impl FnOnce() -> Result<Object, HeapError>,
    ) -> Result<ObjectId, HeapError> {
        let slot_bytes = if self.free.is_empty() {
            checked_vector_bytes::<Slot>(1)?
        } else {
            0
        };
        let reservation = self
            .memory
            .reserve(checked_add(slot_bytes, dynamic_bytes)?)?;
        try_reserve_vec_exact(&mut self.live_indices, 1)?;
        let object = build()?;
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            debug_assert!(slot.object.is_none());
            slot.object = Some(object);
            self.live_indices.push(index);
            self.live += 1;
            reservation.commit();
            Ok(ObjectId {
                index,
                generation: slot.generation,
            })
        } else {
            let index = u32::try_from(self.slots.len()).map_err(|_| MemoryError::SizeOverflow)?;
            try_reserve_vec_exact(&mut self.slots, 1)?;
            self.slots.push(Slot {
                generation: 0,
                marked: false,
                object: Some(object),
            });
            self.live_indices.push(index);
            self.live += 1;
            reservation.commit();
            Ok(ObjectId {
                index,
                generation: 0,
            })
        }
    }

    fn allocation_bytes(&self, dynamic_bytes: usize) -> Result<usize, HeapError> {
        let slot_bytes = if self.free.is_empty() {
            checked_vector_bytes::<Slot>(1)?
        } else {
            0
        };
        Ok(checked_add(slot_bytes, dynamic_bytes)?)
    }

    fn object(&self, id: ObjectId) -> Option<&Object> {
        self.slots
            .get(id.index as usize)
            .filter(|slot| slot.generation == id.generation)
            .and_then(|slot| slot.object.as_ref())
    }

    fn object_mut(&mut self, id: ObjectId) -> Option<&mut Object> {
        self.slots
            .get_mut(id.index as usize)
            .filter(|slot| slot.generation == id.generation)
            .and_then(|slot| slot.object.as_mut())
    }

    fn table(&self, id: TableId) -> Result<&Table, HeapError> {
        match self.object(id.into()) {
            Some(Object::Table(table)) => Ok(table),
            _ => Err(HeapError::StaleTable(id)),
        }
    }

    fn table_mut(&mut self, id: TableId) -> Result<&mut Table, HeapError> {
        match self.object_mut(id.into()) {
            Some(Object::Table(table)) => Ok(table),
            _ => Err(HeapError::StaleTable(id)),
        }
    }

    fn closure(&self, id: ClosureId) -> Result<&Closure, HeapError> {
        match self.object(id.into()) {
            Some(Object::Closure(closure)) => Ok(closure),
            _ => Err(HeapError::StaleClosure(id)),
        }
    }

    fn closure_mut(&mut self, id: ClosureId) -> Result<&mut Closure, HeapError> {
        match self.object_mut(id.into()) {
            Some(Object::Closure(closure)) => Ok(closure),
            _ => Err(HeapError::StaleClosure(id)),
        }
    }
}

fn object_mut_in_slots(slots: &mut [Slot], id: ObjectId) -> Option<&mut Object> {
    slots
        .get_mut(id.index as usize)
        .filter(|slot| slot.generation == id.generation)
        .and_then(|slot| slot.object.as_mut())
}

fn table_mut_in_slots(slots: &mut [Slot], id: TableId) -> Result<&mut Table, HeapError> {
    match object_mut_in_slots(slots, id.into()) {
        Some(Object::Table(table)) => Ok(table),
        _ => Err(HeapError::StaleTable(id)),
    }
}

fn closure_in_slots(slots: &[Slot], id: ClosureId) -> Result<&Closure, HeapError> {
    match slots
        .get(id.index as usize)
        .filter(|slot| slot.generation == id.generation)
        .and_then(|slot| slot.object.as_ref())
    {
        Some(Object::Closure(closure)) => Ok(closure),
        _ => Err(HeapError::StaleClosure(id)),
    }
}

fn enqueue_value(value: &Value, queue: &mut VecDeque<ObjectId>) {
    match value {
        Value::Table(value) => queue.push_back((*value).into()),
        Value::Closure(value) => queue.push_back((*value).into()),
        Value::Thread(value) => queue.push_back((*value).into()),
        Value::CoroutineFunction(value) => queue.push_back((*value).into()),
        Value::UserData(value) => queue.push_back((*value).into()),
        Value::NativeFunction(_) => {}
        _ => {}
    }
}

fn value_contains_heap_reference(value: &Value) -> bool {
    matches!(
        value,
        Value::Table(_)
            | Value::Closure(_)
            | Value::Thread(_)
            | Value::CoroutineFunction(_)
            | Value::UserData(_)
    )
}

#[derive(Clone, Copy, Debug, Default)]
struct WeakMode {
    keys: bool,
    values: bool,
}

fn table_weak_mode(slots: &[Slot], table: &Table) -> WeakMode {
    let Some(metatable) = table.metatable else {
        return WeakMode::default();
    };
    let Some(Object::Table(metatable)) = slots
        .get(metatable.index as usize)
        .filter(|slot| slot.generation == metatable.generation)
        .and_then(|slot| slot.object.as_ref())
    else {
        return WeakMode::default();
    };
    let Some(Value::String(mode)) = metatable.get(&Key::String(Arc::from(&b"__mode"[..]))) else {
        return WeakMode::default();
    };
    WeakMode {
        keys: mode.contains(&b'k'),
        values: mode.contains(&b'v'),
    }
}

fn value_is_marked(slots: &[Slot], value: &Value) -> bool {
    let id = match value {
        Value::Table(value) => ObjectId::from(*value),
        Value::Closure(value) => ObjectId::from(*value),
        Value::Thread(value) | Value::CoroutineFunction(value) => ObjectId::from(*value),
        Value::UserData(value) => ObjectId::from(*value),
        _ => return true,
    };
    slots.get(id.index as usize).is_some_and(|slot| {
        slot.generation == id.generation && slot.object.is_some() && slot.marked
    })
}

fn key_is_marked(slots: &[Slot], key: &Key) -> bool {
    match key {
        Key::Table(value) => value_is_marked(slots, &Value::Table(*value)),
        Key::Closure(value) => value_is_marked(slots, &Value::Closure(*value)),
        Key::Thread(value) | Key::CoroutineFunction(value) => {
            value_is_marked(slots, &Value::Thread(*value))
        }
        Key::UserData(value) => value_is_marked(slots, &Value::UserData(*value)),
        _ => true,
    }
}

#[derive(Clone, Debug)]
struct Slot {
    generation: u32,
    marked: bool,
    object: Option<Object>,
}

#[derive(Clone, Debug)]
enum Object {
    Table(Table),
    Closure(Closure),
    NativeClosure {
        function: u32,
        upvalues: Vec<UpvalueId>,
        upvalue_capacity: usize,
    },
    Upvalue(Value),
    Thread(Thread),
    UserData(UserData),
}

impl Object {
    fn dynamic_bytes(&self) -> Result<usize, MemoryError> {
        match self {
            Self::Table(table) => checked_add(
                checked_vector_bytes::<Value>(table.array_capacity)?,
                checked_add(
                    checked_hash_bytes::<Key, Value>(table.hash_capacity)?,
                    checked_add(
                        checked_vector_bytes::<Key>(table.hash_order_capacity)?,
                        checked_hash_bytes::<Key, usize>(table.hash_order_capacity)?,
                    )?,
                )?,
            ),
            Self::Closure(closure) => checked_vector_bytes::<UpvalueId>(closure.upvalue_capacity),
            Self::NativeClosure {
                upvalue_capacity, ..
            } => checked_vector_bytes::<UpvalueId>(*upvalue_capacity),
            Self::Thread(thread) => checked_add(
                checked_vector_bytes::<Value>(thread.root_capacity)?,
                checked_vector_bytes::<UpvalueId>(thread.upvalue_capacity)?,
            ),
            Self::Upvalue(_) => Ok(0),
            Self::UserData(userdata) => checked_vector_bytes::<Value>(userdata.user_value_capacity),
        }
    }
}

#[derive(Clone, Debug)]
struct Closure {
    code: ClosureCode,
    prototype: usize,
    profile: SemanticProfile,
    upvalues: Vec<UpvalueId>,
    upvalue_capacity: usize,
    environment: Option<TableId>,
}

#[derive(Clone, Debug)]
enum ClosureCode {
    Luau(Arc<Chunk>),
    Blu(Arc<BluArtifact>),
}

#[derive(Clone, Debug)]
struct Thread {
    roots: Vec<Value>,
    root_capacity: usize,
    upvalues: Vec<UpvalueId>,
    upvalue_capacity: usize,
}

#[derive(Clone, Debug)]
struct UserData {
    tag: Arc<[u8]>,
    metatable: Option<TableId>,
    user_values: Vec<Value>,
    user_value_capacity: usize,
    finalizer_armed: bool,
    finalizer_done: bool,
    finalizer_profile: Option<SemanticProfile>,
    finalizer_order: u64,
}

#[derive(Clone, Debug)]
struct Table {
    array: Vec<Value>,
    array_capacity: usize,
    legacy_array_capacity: usize,
    tracks_legacy_array_layout: bool,
    preallocated_array_boundary: bool,
    nil_write_boundary: bool,
    array_contains_heap_reference: bool,
    hash_contains_heap_key: bool,
    hash_contains_heap_value: bool,
    preserve_hash_order: bool,
    hash_iteration_order: HashIterationOrder,
    hash_order_enabled: bool,
    hash: HashMap<Key, Value>,
    hash_capacity: usize,
    hash_order: Vec<Key>,
    hash_order_capacity: usize,
    hash_order_positions: HashMap<Key, usize>,
    mutation: u64,
    metatable: Option<TableId>,
    finalizer_armed: bool,
    finalizer_done: bool,
    finalizer_profile: Option<SemanticProfile>,
    finalizer_order: u64,
    frozen: bool,
}

impl Table {
    fn get(&self, key: &Key) -> Option<&Value> {
        if let Some(index) = key.array_index()
            && index <= self.array.len()
        {
            return self.array.get(index - 1);
        }
        self.hash.get(key)
    }

    fn set(&mut self, key: Key, value: Value, memory: &mut MemoryAccount) -> Result<(), HeapError> {
        let value_contains_heap_reference = value_contains_heap_reference(&value);
        if let Some(index) = key.array_index() {
            if index <= self.array.len() {
                if matches!(value, Value::Nil) && !matches!(self.array[index - 1], Value::Nil) {
                    let tombstone =
                        Key::Integer(i64::try_from(index).map_err(|_| MemoryError::SizeOverflow)?);
                    if !self.hash.contains_key(&tombstone) {
                        let required = self
                            .hash
                            .len()
                            .checked_add(1)
                            .ok_or(MemoryError::SizeOverflow)?;
                        self.grow_hash(required, memory)?;
                        if self.hash_order_capacity != 0 {
                            self.push_hash_order_key(tombstone.clone());
                        }
                    }
                    self.hash.insert(tombstone, Value::Nil);
                } else if !matches!(value, Value::Nil) {
                    self.remove_hash_entry(&key);
                }
                self.array_contains_heap_reference |= value_contains_heap_reference;
                self.array[index - 1] = value;
                return Ok(());
            }
            let next_array_index = self
                .array
                .len()
                .checked_add(1)
                .ok_or(MemoryError::SizeOverflow)?;
            if index == next_array_index && matches!(value, Value::Nil) {
                self.nil_write_boundary = true;
                if self.tracks_legacy_array_layout || !self.array.is_empty() {
                    self.tracks_legacy_array_layout = true;
                    self.legacy_array_capacity = self.legacy_array_capacity.max(index);
                }
                return Ok(());
            }
            if index > next_array_index
                && self.tracks_legacy_array_layout
                && index <= self.legacy_array_capacity.saturating_add(1)
                && !matches!(value, Value::Nil)
            {
                let promoted = self.contiguous_hash_values_after(index)?;
                let required = index
                    .checked_add(promoted)
                    .ok_or(MemoryError::SizeOverflow)?;
                self.grow_array(required, memory)?;
                while self.array.len() + 1 < index {
                    self.array.push(Value::Nil);
                }
                self.array_contains_heap_reference |= value_contains_heap_reference;
                self.array.push(value);
                self.promote_contiguous(promoted);
                self.legacy_array_capacity = self.legacy_array_capacity.max(self.array.len());
                return Ok(());
            }
            if index == next_array_index && !matches!(value, Value::Nil) {
                let promoted = self.contiguous_hash_values_after(index)?;
                let additional = promoted.checked_add(1).ok_or(MemoryError::SizeOverflow)?;
                let required = self
                    .array
                    .len()
                    .checked_add(additional)
                    .ok_or(MemoryError::SizeOverflow)?;
                self.grow_array(required, memory)?;
                self.array_contains_heap_reference |= value_contains_heap_reference;
                self.array.push(value);
                self.promote_contiguous(promoted);
                self.legacy_array_capacity = self.legacy_array_capacity.max(self.array.len());
                return Ok(());
            }
        }
        if matches!(value, Value::Nil) {
            if self.hash.contains_key(&key) {
                // Keep a deleted hash key as an iteration tombstone. Lua's
                // `next` permits clearing the key it just returned and then
                // continuing with `next(table, key)`.
                self.hash_contains_heap_key |= key.contains_heap_reference();
                self.hash.insert(key, Value::Nil);
            }
        } else {
            self.hash_contains_heap_key |= key.contains_heap_reference();
            self.hash_contains_heap_value |= value_contains_heap_reference;
            if !self.hash.contains_key(&key) {
                self.hash_order_enabled |=
                    self.preserve_hash_order && !matches!(key, Key::Integer(_) | Key::Number(_));
                let required = self
                    .hash
                    .len()
                    .checked_add(1)
                    .ok_or(MemoryError::SizeOverflow)?;
                self.grow_hash(required, memory)?;
                if self.hash_order_capacity != 0 {
                    self.push_hash_order_key(key.clone());
                }
            }
            self.hash.insert(key, value);
        }
        Ok(())
    }

    fn set_bytes(&self, key: &Key, value: &Value) -> Result<usize, HeapError> {
        if let Some(index) = key.array_index() {
            if index <= self.array.len() {
                if matches!(value, Value::Nil)
                    && !matches!(self.array[index - 1], Value::Nil)
                    && !self.hash.contains_key(key)
                {
                    let required = self
                        .hash
                        .len()
                        .checked_add(1)
                        .ok_or(MemoryError::SizeOverflow)?;
                    return self.hash_growth_bytes(required, false);
                }
                return Ok(0);
            }
            let next_array_index = self
                .array
                .len()
                .checked_add(1)
                .ok_or(MemoryError::SizeOverflow)?;
            if index == next_array_index && !matches!(value, Value::Nil) {
                let promoted = self.contiguous_hash_values_after(index)?;
                let additional = promoted.checked_add(1).ok_or(MemoryError::SizeOverflow)?;
                let required = self
                    .array
                    .len()
                    .checked_add(additional)
                    .ok_or(MemoryError::SizeOverflow)?;
                if required <= self.array_capacity {
                    return Ok(0);
                }
                let new_bytes = checked_vector_bytes::<Value>(required)?;
                if required <= self.array.capacity() {
                    let old_bytes = checked_vector_bytes::<Value>(self.array_capacity)?;
                    return Ok(new_bytes.saturating_sub(old_bytes));
                }
                return Ok(new_bytes);
            }
        }
        if matches!(value, Value::Nil) || self.hash.contains_key(key) {
            return Ok(0);
        }
        let required = self
            .hash
            .len()
            .checked_add(1)
            .ok_or(MemoryError::SizeOverflow)?;
        let track_hash_order =
            self.preserve_hash_order && !matches!(key, Key::Integer(_) | Key::Number(_));
        self.hash_growth_bytes(required, track_hash_order)
    }

    fn length(&self) -> usize {
        self.array
            .iter()
            .position(|value| matches!(value, Value::Nil))
            .unwrap_or(self.array.len())
    }

    fn modern_length(&self) -> usize {
        if self.preallocated_array_boundary {
            return self
                .array
                .iter()
                .rposition(|value| !matches!(value, Value::Nil))
                .map_or(0, |index| index + 1);
        }
        let Some(first) = self.array.first() else {
            let capacity_value = i64::try_from(self.array_capacity)
                .ok()
                .and_then(|capacity| self.hash.get(&Key::Integer(capacity)));
            if self.array_capacity > 0
                && capacity_value.is_some_and(|value| !matches!(value, Value::Nil))
            {
                return self.array_capacity;
            }
            return self.dynamic_modern_length();
        };
        if !matches!(first, Value::Nil) {
            return self.length();
        }
        if self.array.len() == self.array_capacity
            && self
                .array
                .last()
                .is_some_and(|value| !matches!(value, Value::Nil))
        {
            return self.array_capacity;
        }
        if self.array.len() >= 3
            && self.array[1..]
                .iter()
                .all(|value| !matches!(value, Value::Nil))
        {
            return self.array.len();
        }
        0
    }

    fn dynamic_modern_length(&self) -> usize {
        if self.tracks_legacy_array_layout {
            return 0;
        }
        if self.preserve_hash_order
            && self
                .hash
                .get(&Key::Integer(1))
                .is_none_or(|value| matches!(value, Value::Nil))
        {
            return 0;
        }
        let mut index = 2usize;
        while self
            .hash
            .get(&Key::Integer(index.try_into().unwrap_or(i64::MAX)))
            .is_some_and(|value| !matches!(value, Value::Nil))
        {
            index = index.saturating_add(1);
        }
        if index >= 4 { index - 1 } else { 0 }
    }

    fn legacy_length(&self) -> usize {
        let mut maximum = self
            .array
            .iter()
            .rposition(|value| !matches!(value, Value::Nil))
            .map_or(0usize, |index| index + 1);
        for (key, value) in &self.hash {
            if matches!(value, Value::Nil) {
                continue;
            }
            let number = match key {
                Key::Integer(value) => *value,
                Key::Number(bits) => {
                    let number = f64::from_bits(*bits);
                    if !number.is_finite() || number < 1.0 || number.fract() != 0.0 {
                        continue;
                    }
                    if number > usize::MAX as f64 {
                        continue;
                    }
                    number as i64
                }
                _ => continue,
            };
            if number >= 1 {
                let number = usize::try_from(number).unwrap_or(usize::MAX);
                if number <= self.legacy_array_capacity {
                    maximum = maximum.max(number);
                }
            }
        }
        maximum
    }

    fn trim_array(&mut self) {
        while self
            .array
            .last()
            .is_some_and(|value| matches!(value, Value::Nil))
        {
            let index = self.array.len();
            self.remove_hash_entry(&Key::Integer(index as i64));
            self.array.pop();
        }
    }

    fn promote_contiguous(&mut self, count: usize) {
        for _ in 0..count {
            let key = Key::Integer((self.array.len() + 1) as i64);
            let Some(value) = self.remove_hash_entry(&key) else {
                return;
            };
            self.array_contains_heap_reference |= value_contains_heap_reference(&value);
            self.array.push(value);
        }
    }

    fn contiguous_hash_values_after(&self, index: usize) -> Result<usize, HeapError> {
        let mut next = index.checked_add(1).ok_or(MemoryError::SizeOverflow)?;
        let mut count = 0usize;
        loop {
            let key = i64::try_from(next).map_err(|_| MemoryError::SizeOverflow)?;
            if self
                .hash
                .get(&Key::Integer(key))
                .is_none_or(|value| matches!(value, Value::Nil))
            {
                return Ok(count);
            }
            count = count.checked_add(1).ok_or(MemoryError::SizeOverflow)?;
            next = next.checked_add(1).ok_or(MemoryError::SizeOverflow)?;
        }
    }

    fn grow_array(&mut self, required: usize, memory: &mut MemoryAccount) -> Result<(), HeapError> {
        if required <= self.array_capacity {
            return Ok(());
        }
        if required <= self.array.capacity() {
            // `array_capacity` is also the logical array boundary used by the
            // profile-specific length rules. Rust's Vec may already have
            // spare physical capacity after an earlier growth, so advance
            // that logical boundary without repeating the physical allocation
            // work, while charging the logical boundary growth.
            let old_bytes = checked_vector_bytes::<Value>(self.array_capacity)?;
            let new_bytes = checked_vector_bytes::<Value>(required)?;
            let additional_bytes = new_bytes.saturating_sub(old_bytes);
            if additional_bytes != 0 {
                memory.reserve(additional_bytes)?.commit();
            }
            self.array_capacity = required;
            return Ok(());
        }
        let old_bytes = checked_vector_bytes::<Value>(self.array_capacity)?;
        let new_bytes = checked_vector_bytes::<Value>(required)?;
        let reservation = memory.reserve(new_bytes)?;
        try_reserve_vec_to(&mut self.array, required)?;
        reservation.commit_replacing(old_bytes)?;
        self.array_capacity = required;
        Ok(())
    }

    fn grow_hash(&mut self, required: usize, memory: &mut MemoryAccount) -> Result<(), HeapError> {
        let hash_capacity = self.hash_capacity.max(required);
        let hash_order_capacity = if self.hash_order_enabled
            || self.hash_order_capacity != 0
            || required >= HASH_ITERATION_ORDER_THRESHOLD
        {
            self.hash_order_capacity.max(required)
        } else {
            0
        };
        if hash_capacity == self.hash_capacity && hash_order_capacity == self.hash_order_capacity {
            return Ok(());
        }
        let old_bytes = checked_hash_bytes::<Key, Value>(self.hash_capacity)?;
        let old_bytes = checked_add(
            old_bytes,
            checked_add(
                checked_vector_bytes::<Key>(self.hash_order_capacity)?,
                checked_hash_bytes::<Key, usize>(self.hash_order_capacity)?,
            )?,
        )?;
        let new_bytes = checked_add(
            checked_hash_bytes::<Key, Value>(hash_capacity)?,
            checked_add(
                checked_vector_bytes::<Key>(hash_order_capacity)?,
                checked_hash_bytes::<Key, usize>(hash_order_capacity)?,
            )?,
        )?;
        let reservation = memory.reserve(new_bytes)?;
        try_reserve_hash_to(&mut self.hash, hash_capacity)?;
        try_reserve_vec_to(&mut self.hash_order, hash_order_capacity)?;
        try_reserve_hash_to(&mut self.hash_order_positions, hash_order_capacity)?;
        if self.hash_order_capacity == 0 && hash_order_capacity != 0 {
            for key in self.hash.keys() {
                let position = self.hash_order.len();
                self.hash_order.push(key.clone());
                self.hash_order_positions.insert(key.clone(), position);
            }
        }
        reservation.commit_replacing(old_bytes)?;
        self.hash_capacity = hash_capacity;
        self.hash_order_capacity = hash_order_capacity;
        self.rebuild_hash_order();
        Ok(())
    }

    fn hash_growth_bytes(
        &self,
        required: usize,
        track_hash_order: bool,
    ) -> Result<usize, HeapError> {
        let hash_capacity = self.hash_capacity.max(required);
        let hash_order_capacity = if track_hash_order
            || self.hash_order_enabled
            || self.hash_order_capacity != 0
            || required >= HASH_ITERATION_ORDER_THRESHOLD
        {
            self.hash_order_capacity.max(required)
        } else {
            0
        };
        let old_bytes = checked_add(
            checked_hash_bytes::<Key, Value>(self.hash_capacity)?,
            checked_add(
                checked_vector_bytes::<Key>(self.hash_order_capacity)?,
                checked_hash_bytes::<Key, usize>(self.hash_order_capacity)?,
            )?,
        )?;
        let new_bytes = checked_add(
            checked_hash_bytes::<Key, Value>(hash_capacity)?,
            checked_add(
                checked_vector_bytes::<Key>(hash_order_capacity)?,
                checked_hash_bytes::<Key, usize>(hash_order_capacity)?,
            )?,
        )?;
        Ok(new_bytes.saturating_sub(old_bytes))
    }

    fn remove_hash_entry(&mut self, key: &Key) -> Option<Value> {
        self.hash.remove(key)
    }

    fn push_hash_order_key(&mut self, key: Key) {
        if self.hash_order_positions.contains_key(&key) {
            return;
        }
        self.hash_order.push(key);
        self.rebuild_hash_order();
    }

    fn rebuild_hash_order(&mut self) {
        self.hash_order_positions.clear();
        for (position, key) in self.hash_order.iter().enumerate() {
            self.hash_order_positions.insert(key.clone(), position);
        }
    }

    fn luau_hash_order(&self) -> Vec<Key> {
        let capacity = self
            .hash_capacity
            .max(self.hash_order.len())
            .max(1)
            .checked_next_power_of_two()
            .unwrap_or(usize::MAX);
        let mut layout = LuauHashLayout::new(capacity);
        for key in &self.hash_order {
            layout.insert(key.clone());
        }
        layout.slots.into_iter().flatten().collect::<Vec<_>>()
    }
}

struct LuauHashLayout {
    slots: Vec<Option<Key>>,
    next: Vec<Option<usize>>,
    last_free: usize,
}

impl LuauHashLayout {
    fn new(capacity: usize) -> Self {
        Self {
            slots: vec![None; capacity.max(1)],
            next: vec![None; capacity.max(1)],
            last_free: capacity.max(1),
        }
    }

    fn insert(&mut self, key: Key) {
        let mask = self.slots.len().saturating_sub(1) as u32;
        let main = (luau_hash(&key) & mask) as usize;
        if self.slots[main].is_none() {
            self.slots[main] = Some(key);
            return;
        }

        let Some(free) = self.free_slot() else {
            self.resize();
            self.insert(key);
            return;
        };
        let occupant_main = {
            let occupant = self.slots[main].as_ref().expect("main slot is occupied");
            (luau_hash(occupant) & mask) as usize
        };
        if occupant_main != main {
            let mut previous = occupant_main;
            while self.next[previous] != Some(main) {
                previous = self.next[previous].expect("Luau hash chain is connected");
            }
            self.slots[free] = self.slots[main].take();
            self.next[free] = self.next[main];
            self.next[main] = None;
            self.next[previous] = Some(free);
            self.slots[main] = Some(key);
        } else {
            self.next[free] = self.next[main];
            self.next[main] = Some(free);
            self.slots[free] = Some(key);
        }
    }

    fn free_slot(&mut self) -> Option<usize> {
        while self.last_free > 0 {
            self.last_free -= 1;
            if self.slots[self.last_free].is_none() {
                return Some(self.last_free);
            }
        }
        None
    }

    fn resize(&mut self) {
        let old_slots = core::mem::take(&mut self.slots);
        let old_size = old_slots.len();
        let old_next = core::mem::take(&mut self.next);
        let mut replacement = Self::new(old_size.saturating_mul(2).max(1));
        for index in (0..old_size).rev() {
            if let Some(key) = old_slots[index].clone() {
                replacement.insert(key);
            }
        }
        // `old_next` is intentionally consumed with the old slot array. The
        // node keys, not the old relative chain offsets, are what Luau's
        // resize path reinserts in the new table.
        drop(old_next);
        *self = replacement;
    }
}

fn try_clone_slice<T: Clone>(values: &[T]) -> Result<Vec<T>, HeapError> {
    let mut cloned = Vec::new();
    try_reserve_vec_exact(&mut cloned, values.len())?;
    cloned.extend_from_slice(values);
    Ok(cloned)
}

fn try_reserve_vec_exact<T>(values: &mut Vec<T>, additional: usize) -> Result<(), HeapError> {
    let required = values
        .len()
        .checked_add(additional)
        .ok_or(MemoryError::SizeOverflow)?;
    let requested = checked_vector_bytes::<T>(required)?;
    values
        .try_reserve_exact(additional)
        .map_err(|_| MemoryError::AllocationFailed { requested })?;
    Ok(())
}

fn try_reserve_vec_to<T>(values: &mut Vec<T>, capacity: usize) -> Result<(), HeapError> {
    let additional = capacity.saturating_sub(values.len());
    let requested = checked_vector_bytes::<T>(capacity)?;
    values
        .try_reserve(additional)
        .map_err(|_| MemoryError::AllocationFailed { requested })?;
    Ok(())
}

fn try_reserve_hash<K: Eq + Hash, V>(
    values: &mut HashMap<K, V>,
    additional: usize,
) -> Result<(), HeapError> {
    let required = values
        .len()
        .checked_add(additional)
        .ok_or(MemoryError::SizeOverflow)?;
    let requested = checked_hash_bytes::<K, V>(required)?;
    values
        .try_reserve(additional)
        .map_err(|_| MemoryError::AllocationFailed { requested })?;
    Ok(())
}

fn try_reserve_hash_to<K: Eq + Hash, V>(
    values: &mut HashMap<K, V>,
    capacity: usize,
) -> Result<(), HeapError> {
    let additional = capacity.saturating_sub(values.len());
    let requested = checked_hash_bytes::<K, V>(capacity)?;
    values
        .try_reserve(additional)
        .map_err(|_| MemoryError::AllocationFailed { requested })?;
    Ok(())
}

fn checked_add(left: usize, right: usize) -> Result<usize, MemoryError> {
    left.checked_add(right).ok_or(MemoryError::SizeOverflow)
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum Key {
    Boolean(bool),
    Integer(i64),
    Number(u64),
    String(Arc<[u8]>),
    Table(TableId),
    Closure(ClosureId),
    Thread(ThreadId),
    CoroutineFunction(ThreadId),
    LightUserData(UpvalueId),
    UserData(UserDataId),
    NativeFunction(crate::NativeFunctionId),
}

impl Key {
    fn from_value(value: &Value) -> Result<Self, HeapError> {
        match value {
            Value::Nil => Err(HeapError::NilKey),
            Value::Boolean(value) => Ok(Self::Boolean(*value)),
            Value::Integer(value) => Ok(Self::Integer(*value)),
            Value::Number(value) if value.is_nan() => Err(HeapError::NanKey),
            Value::Number(value)
                if value.fract() == 0.0
                    && *value >= i64::MIN as f64
                    && *value <= i64::MAX as f64 =>
            {
                Ok(Self::Integer(*value as i64))
            }
            Value::Number(value) => Ok(Self::Number(if *value == 0.0 {
                0.0f64.to_bits()
            } else {
                value.to_bits()
            })),
            Value::String(value) => Ok(Self::String(value.clone())),
            Value::Table(value) => Ok(Self::Table(*value)),
            Value::Closure(value) => Ok(Self::Closure(*value)),
            Value::Thread(value) => Ok(Self::Thread(*value)),
            Value::CoroutineFunction(value) => Ok(Self::CoroutineFunction(*value)),
            Value::LightUserData(value) => Ok(Self::LightUserData(*value)),
            Value::UserData(value) => Ok(Self::UserData(*value)),
            Value::NativeFunction(value) => Ok(Self::NativeFunction(*value)),
        }
    }

    fn array_index(&self) -> Option<usize> {
        match self {
            Self::Integer(value) if *value > 0 => usize::try_from(*value).ok(),
            _ => None,
        }
    }

    fn enqueue(&self, queue: &mut VecDeque<ObjectId>) {
        match self {
            Self::Table(value) => queue.push_back((*value).into()),
            Self::Closure(value) => queue.push_back((*value).into()),
            Self::Thread(value) => queue.push_back((*value).into()),
            Self::CoroutineFunction(value) => queue.push_back((*value).into()),
            Self::UserData(value) => queue.push_back((*value).into()),
            _ => {}
        }
    }

    fn contains_heap_reference(&self) -> bool {
        matches!(
            self,
            Self::Table(_)
                | Self::Closure(_)
                | Self::Thread(_)
                | Self::CoroutineFunction(_)
                | Self::UserData(_)
        )
    }

    fn to_value(&self) -> Value {
        match self {
            Self::Boolean(value) => Value::Boolean(*value),
            Self::Integer(value) => Value::Integer(*value),
            Self::Number(value) => Value::Number(f64::from_bits(*value)),
            Self::String(value) => Value::String(value.clone()),
            Self::Table(value) => Value::Table(*value),
            Self::Closure(value) => Value::Closure(*value),
            Self::Thread(value) => Value::Thread(*value),
            Self::CoroutineFunction(value) => Value::CoroutineFunction(*value),
            Self::LightUserData(value) => Value::LightUserData(*value),
            Self::UserData(value) => Value::UserData(*value),
            Self::NativeFunction(value) => Value::NativeFunction(*value),
        }
    }
}

fn luau_hash(key: &Key) -> u32 {
    match key {
        Key::Boolean(value) => u32::from(*value),
        Key::Integer(value) => luau_hash_u64(*value as u64),
        Key::Number(bits) => {
            let bits = if *bits == (-0.0f64).to_bits() {
                0.0f64.to_bits()
            } else {
                *bits
            };
            luau_hash_u64(bits)
        }
        Key::String(value) => {
            let mut hash = value.len() as u32;
            for byte in value.iter().rev() {
                hash ^= hash
                    .wrapping_shl(5)
                    .wrapping_add(hash.wrapping_shr(2))
                    .wrapping_add(u32::from(*byte));
            }
            hash
        }
        Key::Table(value) => luau_hash_object(value.index, value.generation),
        Key::Closure(value) => luau_hash_object(value.index, value.generation),
        Key::Thread(value) => luau_hash_object(value.index, value.generation),
        Key::CoroutineFunction(value) => luau_hash_object(value.index, value.generation),
        Key::LightUserData(value) => luau_hash_object(value.index, value.generation),
        Key::UserData(value) => luau_hash_object(value.index, value.generation),
        Key::NativeFunction(value) => luau_hash_u64(u64::from(value.0)),
    }
}

fn luau_hash_u64(value: u64) -> u32 {
    let mut low = value as u32;
    let mut high = (value >> 32) as u32;
    const M: u32 = 0x5bd1e995;
    low ^= high >> 18;
    low = low.wrapping_mul(M);
    high ^= low >> 22;
    high = high.wrapping_mul(M);
    low ^= high >> 17;
    low = low.wrapping_mul(M);
    high ^= low >> 19;
    high = high.wrapping_mul(M);
    high
}

fn luau_hash_object(index: u32, generation: u32) -> u32 {
    luau_hash_u64((u64::from(generation) << 32) | u64::from(index))
}

fn add_key_string_bytes(
    key: &Key,
    seen: &mut HashSet<usize>,
    total: &mut usize,
) -> Result<(), HeapError> {
    if let Key::String(bytes) = key {
        add_arc_string_bytes(bytes, seen, total)?;
    }
    Ok(())
}

fn add_string_bytes(
    value: &Value,
    seen: &mut HashSet<usize>,
    total: &mut usize,
) -> Result<(), HeapError> {
    if let Value::String(bytes) = value {
        add_arc_string_bytes(bytes, seen, total)?;
    }
    Ok(())
}

fn add_arc_string_bytes(
    bytes: &Arc<[u8]>,
    seen: &mut HashSet<usize>,
    total: &mut usize,
) -> Result<(), HeapError> {
    let pointer = bytes.as_ptr() as usize;
    if bytes.len() < MIN_ACCOUNTED_STRING_BYTES || !seen.insert(pointer) {
        return Ok(());
    }
    *total = total
        .checked_add(bytes.len())
        .ok_or(MemoryError::SizeOverflow)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limited_memory(bytes: usize) -> MemoryConfig {
        MemoryConfig {
            hard_limit_bytes: Some(bytes),
            gc_start_bytes: bytes,
            gc_growth_percent: 50,
            max_single_allocation_bytes: usize::MAX,
        }
    }

    fn empty_chunk() -> Arc<Chunk> {
        Arc::new(Chunk {
            version: 12,
            typeinfo_version: 3,
            strings: Vec::new(),
            userdata_types: Vec::new(),
            prototypes: Vec::new(),
            main: 0,
        })
    }

    #[test]
    fn tables_split_dense_integer_keys_and_hash_keys() {
        let mut heap = Heap::default();
        let table = heap.allocate_table(0, 0).unwrap();
        heap.table_set(table, Value::Integer(2), Value::Number(2.0))
            .unwrap();
        heap.table_set(table, Value::Integer(1), Value::Number(1.0))
            .unwrap();
        heap.table_set(
            table,
            Value::String(Arc::from(&b"name"[..])),
            Value::Boolean(true),
        )
        .unwrap();
        assert_eq!(heap.table_length(table).unwrap(), 2);
        assert_eq!(
            heap.table_get(table, &Value::Number(2.0)).unwrap(),
            Value::Number(2.0)
        );
    }

    #[test]
    fn ordered_guest_tables_defer_order_storage_for_numeric_hash_keys() {
        let mut heap = Heap::default();
        let table = heap.allocate_table_with_hash_order(0, 0, true).unwrap();
        heap.table_set(table, Value::Integer(2), Value::Boolean(true))
            .unwrap();
        let Object::Table(table_object) = heap.object(table.into()).unwrap() else {
            panic!("expected table object");
        };
        assert_eq!(table_object.hash_order_capacity, 0);

        heap.table_set(
            table,
            Value::String(Arc::from(&b"ordered"[..])),
            Value::Boolean(true),
        )
        .unwrap();
        let Object::Table(table_object) = heap.object(table.into()).unwrap() else {
            panic!("expected table object");
        };
        assert!(table_object.hash_order_capacity > 0);
    }

    #[test]
    fn luau_guest_tables_iterate_hash_buckets_in_slot_order() {
        let mut heap = Heap::default();
        let table = heap
            .allocate_table_with_iteration_order(3, 4, HashIterationOrder::Luau)
            .unwrap();
        for (key, value) in [
            (b"a".as_slice(), 5),
            (b"b".as_slice(), 6),
            (b"c".as_slice(), 7),
        ] {
            heap.table_set(table, Value::String(Arc::from(key)), Value::Integer(value))
                .unwrap();
        }

        let mut cursor = Value::Nil;
        let mut values = Vec::new();
        while let Some((key, value)) = heap.table_next(table, &cursor).unwrap() {
            if !matches!(key, Value::Integer(_)) {
                values.push(value);
            }
            cursor = key;
        }
        assert_eq!(
            values,
            [Value::Integer(5), Value::Integer(7), Value::Integer(6)]
        );
    }

    #[test]
    fn luau_guest_string_collisions_follow_chain_slot_order() {
        let mut heap = Heap::default();
        let table = heap
            .allocate_table_with_iteration_order(0, 0, HashIterationOrder::Luau)
            .unwrap();
        for (key, value) in [
            (b"foo".as_slice(), 1),
            (b"bar".as_slice(), 2),
            (b"thing".as_slice(), 3),
        ] {
            heap.table_set(table, Value::String(Arc::from(key)), Value::Integer(value))
                .unwrap();
        }

        let mut cursor = Value::Nil;
        let mut values = Vec::new();
        while let Some((key, value)) = heap.table_next(table, &cursor).unwrap() {
            values.push(value);
            cursor = key;
        }
        assert_eq!(
            values,
            [Value::Integer(3), Value::Integer(2), Value::Integer(1)]
        );
    }

    #[test]
    fn table_next_accepts_a_deleted_current_hash_key_only() {
        let mut heap = Heap::default();
        let table = heap.allocate_table(0, 0).unwrap();
        let key = Value::String(Arc::from(&b"deleted"[..]));
        heap.table_set(table, key.clone(), Value::Integer(1))
            .unwrap();
        assert_eq!(
            heap.table_next(table, &Value::Nil),
            Ok(Some((key.clone(), Value::Integer(1))))
        );
        heap.table_set(table, key.clone(), Value::Nil).unwrap();
        assert_eq!(heap.table_next(table, &key), Ok(None));
        assert_eq!(
            heap.table_next(table, &Value::String(Arc::from(&b"absent"[..]))),
            Err(HeapError::InvalidIterationKey)
        );
    }

    #[test]
    fn large_hash_tables_iterate_each_live_key_once() {
        let mut heap = Heap::default();
        let table = heap.allocate_table(0, 0).unwrap();
        let count = HASH_ITERATION_ORDER_THRESHOLD + 32;
        for index in 0..count {
            heap.table_set(
                table,
                Value::String(Arc::from(format!("key-{index}").into_bytes())),
                Value::Boolean(true),
            )
            .unwrap();
        }

        let mut cursor = Value::Nil;
        let mut seen = Vec::new();
        while let Some((key, value)) = heap.table_next(table, &cursor).unwrap() {
            assert!(!seen.contains(&key));
            assert_eq!(value, Value::Boolean(true));
            seen.push(key.clone());
            cursor = key;
        }
        assert_eq!(seen.len(), count);
    }

    #[test]
    fn table_lookup_with_nil_key_returns_nil_but_writes_reject_it() {
        let mut heap = Heap::default();
        let table = heap.allocate_table(0, 0).unwrap();
        assert_eq!(heap.table_get(table, &Value::Nil), Ok(Value::Nil));
        assert_eq!(
            heap.table_set(table, Value::Nil, Value::Integer(1)),
            Err(HeapError::NilKey)
        );
    }

    #[test]
    fn tracing_collection_handles_cycles_and_closure_graphs() {
        let mut heap = Heap::default();
        let retained = heap.allocate_table(0, 0).unwrap();
        let child = heap.allocate_table(0, 0).unwrap();
        let garbage = heap.allocate_table(0, 0).unwrap();
        heap.table_set(retained, Value::Integer(1), Value::Table(child))
            .unwrap();
        heap.table_set(child, Value::Integer(1), Value::Table(retained))
            .unwrap();
        let upvalue = heap.allocate_upvalue(Value::Table(retained)).unwrap();
        let closure = heap
            .allocate_closure(empty_chunk(), 0, SemanticProfile::Blu, 1)
            .unwrap();
        heap.closure_push_upvalue(closure, upvalue).unwrap();

        let root = Value::Closure(closure);
        assert_eq!(
            heap.collect([&root]),
            Ok(CollectionStats {
                before: 5,
                retained: 4,
                collected: 1
            })
        );
        assert_eq!(
            heap.table_get(garbage, &Value::Integer(1)),
            Err(HeapError::StaleTable(garbage))
        );
        assert_eq!(
            heap.collect(std::iter::empty()),
            Ok(CollectionStats {
                before: 4,
                retained: 0,
                collected: 4,
            })
        );
    }

    #[test]
    fn collection_traces_heap_references_after_scalar_table_storage() {
        let mut heap = Heap::default();
        let table = heap.allocate_table(4, 0).unwrap();
        for index in 1..=4 {
            heap.table_set(table, Value::Integer(index), Value::Integer(index))
                .unwrap();
        }
        assert!(!heap.table(table).unwrap().array_contains_heap_reference);

        let child = heap.allocate_table(0, 0).unwrap();
        heap.table_set(table, Value::Integer(2), Value::Table(child))
            .unwrap();
        assert!(heap.table(table).unwrap().array_contains_heap_reference);
        assert_eq!(
            heap.collect([&Value::Table(table)]),
            Ok(CollectionStats {
                before: 2,
                retained: 2,
                collected: 0,
            })
        );

        heap.table_set(table, Value::Integer(2), Value::Nil)
            .unwrap();
        assert_eq!(
            heap.collect([&Value::Table(table)]),
            Ok(CollectionStats {
                before: 2,
                retained: 1,
                collected: 1,
            })
        );
    }

    #[test]
    fn metatables_are_traced_and_support_iteration() {
        let mut heap = Heap::default();
        let table = heap.allocate_table(0, 0).unwrap();
        let metatable = heap.allocate_table(0, 1).unwrap();
        heap.table_set(
            metatable,
            Value::String(Arc::from(&b"answer"[..])),
            Value::Integer(42),
        )
        .unwrap();
        heap.set_table_metatable(table, Some(metatable)).unwrap();

        assert_eq!(
            heap.collect([&Value::Table(table)]),
            Ok(CollectionStats {
                before: 2,
                retained: 2,
                collected: 0,
            })
        );
        assert_eq!(heap.table_metatable(table), Ok(Some(metatable)));
        assert_eq!(
            heap.table_next(metatable, &Value::Nil),
            Ok(Some((
                Value::String(Arc::from(&b"answer"[..])),
                Value::Integer(42),
            )))
        );
    }

    #[test]
    fn userdata_values_are_traced_and_preserved() {
        let mut heap = Heap::default();
        let userdata = heap
            .allocate_userdata_with_capacity(Arc::from(&b"test"[..]), 1)
            .unwrap();
        let marker = heap.allocate_table(0, 0).unwrap();
        let metatable = heap.allocate_table(0, 0).unwrap();
        heap.userdata_set_user_value(userdata, 1, Value::Table(marker))
            .unwrap();
        heap.set_userdata_metatable(userdata, Some(metatable))
            .unwrap();
        let root = Value::UserData(userdata);

        assert_eq!(
            heap.collect([&root]),
            Ok(CollectionStats {
                before: 3,
                retained: 3,
                collected: 0,
            })
        );
        assert_eq!(
            heap.userdata_user_value(userdata, 1),
            Ok(Some(Value::Table(marker)))
        );
        assert_eq!(heap.userdata_metatable(userdata), Ok(Some(metatable)));
    }

    #[test]
    fn thread_roots_trace_suspended_values() {
        let mut heap = Heap::default();
        let retained = heap.allocate_table(0, 0).unwrap();
        let thread = heap.allocate_thread(&[Value::Table(retained)]).unwrap();
        assert_eq!(
            heap.collect([&Value::Thread(thread)]),
            Ok(CollectionStats {
                before: 2,
                retained: 2,
                collected: 0,
            })
        );
        heap.thread_set_roots(thread, &[]).unwrap();
        assert_eq!(
            heap.collect([&Value::Thread(thread)]),
            Ok(CollectionStats {
                before: 2,
                retained: 1,
                collected: 1,
            })
        );
        assert_eq!(
            heap.table_get(retained, &Value::Integer(1)),
            Err(HeapError::StaleTable(retained))
        );
    }

    #[test]
    fn oversized_initial_capacities_fail_without_consuming_an_arena_slot() {
        let mut heap = Heap::default();
        assert_eq!(
            heap.allocate_table(usize::MAX, 0),
            Err(HeapError::Memory(MemoryError::SizeOverflow))
        );
        assert_eq!(
            heap.allocate_table(0, usize::MAX),
            Err(HeapError::Memory(MemoryError::SizeOverflow))
        );
        assert_eq!(
            heap.allocate_closure(empty_chunk(), 0, SemanticProfile::Blu, usize::MAX),
            Err(HeapError::Memory(MemoryError::SizeOverflow))
        );
        assert_eq!(heap.live_objects(), 0);

        let table = heap.allocate_table(0, 0).unwrap();
        assert_eq!(format!("{table:?}"), "Table(0:0)");
    }

    #[test]
    fn dense_insertion_reserves_and_promotes_the_full_contiguous_run() {
        let mut heap = Heap::default();
        let table = heap.allocate_table(0, 0).unwrap();
        heap.table_set(table, Value::Integer(3), Value::Integer(30))
            .unwrap();
        heap.table_set(table, Value::Integer(2), Value::Integer(20))
            .unwrap();
        heap.table_set(table, Value::Integer(1), Value::Integer(10))
            .unwrap();

        let table = heap.table(table).unwrap();
        assert_eq!(
            table.array,
            [Value::Integer(10), Value::Integer(20), Value::Integer(30)]
        );
        assert!(table.array.capacity() >= 3);
        assert!(table.hash.is_empty());
    }

    #[test]
    fn fallible_allocation_preserves_generation_on_slot_reuse() {
        let mut heap = Heap::default();
        let old = heap.allocate_table(0, 0).unwrap();
        heap.collect(std::iter::empty()).unwrap();
        let replacement = heap.allocate_table(0, 0).unwrap();

        assert_eq!(format!("{old:?}"), "Table(0:0)");
        assert_eq!(format!("{replacement:?}"), "Table(0:1)");
        assert_eq!(
            heap.table_get(old, &Value::Integer(1)),
            Err(HeapError::StaleTable(old))
        );
    }

    #[test]
    fn native_capacity_failure_rolls_back_provisional_charge_and_slot() {
        let mut heap = Heap::try_new(MemoryConfig::default()).unwrap();
        let capacity = (isize::MAX as usize / core::mem::size_of::<Value>()) + 1;
        let requested = checked_vector_bytes::<Value>(capacity).unwrap();

        assert_eq!(
            heap.allocate_table(capacity, 0),
            Err(HeapError::Memory(MemoryError::AllocationFailed {
                requested,
            }))
        );
        assert_eq!(heap.live_objects(), 0);
        assert_eq!(heap.memory_usage().current_bytes, 0);
        assert!(heap.memory_usage().peak_bytes >= requested);

        let table = heap.allocate_table(0, 0).unwrap();
        assert_eq!(format!("{table:?}"), "Table(0:0)");
    }

    #[test]
    fn collection_reclaims_object_capacity_but_keeps_arena_slot_charge() {
        let slot_bytes = checked_vector_bytes::<Slot>(1).unwrap();
        let payload_bytes = checked_add(
            checked_vector_bytes::<Value>(4).unwrap(),
            checked_hash_bytes::<Key, Value>(2).unwrap(),
        )
        .unwrap();
        let limit = checked_add(slot_bytes, payload_bytes).unwrap();
        let mut heap = Heap::try_new(limited_memory(limit)).unwrap();

        let old = heap.allocate_table(4, 2).unwrap();
        assert_eq!(heap.memory_usage().current_bytes, limit);
        heap.collect(std::iter::empty()).unwrap();
        assert_eq!(heap.memory_usage().current_bytes, slot_bytes);
        assert_eq!(heap.memory_usage().collections, 1);

        let replacement = heap.allocate_table(4, 2).unwrap();
        assert_eq!(heap.memory_usage().current_bytes, limit);
        assert_eq!(format!("{old:?}"), "Table(0:0)");
        assert_eq!(format!("{replacement:?}"), "Table(0:1)");
    }

    #[test]
    fn retained_storage_hits_limit_without_mutating_the_heap() {
        let slot_bytes = checked_vector_bytes::<Slot>(1).unwrap();
        let payload_bytes = checked_vector_bytes::<Value>(2).unwrap();
        let limit = checked_add(slot_bytes, payload_bytes).unwrap();
        let mut heap = Heap::try_new(limited_memory(limit)).unwrap();
        let table = heap.allocate_table(2, 0).unwrap();

        heap.collect([&Value::Table(table)]).unwrap();
        assert_eq!(heap.memory_usage().current_bytes, limit);
        assert_eq!(
            heap.allocate_upvalue(Value::Nil),
            Err(HeapError::Memory(MemoryError::LimitExceeded {
                requested: slot_bytes,
                used: limit,
                limit,
            }))
        );
        assert_eq!(heap.live_objects(), 1);
        assert_eq!(heap.table_length(table), Ok(0));
    }

    #[test]
    fn failed_table_growth_and_root_replacement_preserve_state_and_charge() {
        let slot_bytes = checked_vector_bytes::<Slot>(1).unwrap();
        let mut table_heap = Heap::try_new(limited_memory(slot_bytes)).unwrap();
        let table = table_heap.allocate_table(0, 0).unwrap();
        assert!(matches!(
            table_heap.table_set(table, Value::Integer(1), Value::Integer(10)),
            Err(HeapError::Memory(MemoryError::LimitExceeded { .. }))
        ));
        assert_eq!(table_heap.table_length(table), Ok(0));
        assert_eq!(table_heap.memory_usage().current_bytes, slot_bytes);

        let root_bytes = checked_vector_bytes::<Value>(2).unwrap();
        let thread_limit = checked_add(slot_bytes, root_bytes).unwrap();
        let mut thread_heap = Heap::try_new(limited_memory(thread_limit)).unwrap();
        let roots = [Value::Integer(1), Value::Integer(2)];
        let thread = thread_heap.allocate_thread(&roots).unwrap();
        let before = thread_heap.memory_usage();
        assert!(matches!(
            thread_heap.thread_set_roots(
                thread,
                &[Value::Integer(1), Value::Integer(2), Value::Integer(3)]
            ),
            Err(HeapError::Memory(MemoryError::LimitExceeded { .. }))
        ));
        assert_eq!(thread_heap.memory_usage(), before);
        thread_heap.collect([&Value::Thread(thread)]).unwrap();
        assert_eq!(thread_heap.memory_usage().current_bytes, thread_limit);
    }

    #[test]
    fn replacing_thread_roots_releases_the_old_logical_capacity() {
        let mut heap = Heap::default();
        let roots = [
            Value::Integer(1),
            Value::Integer(2),
            Value::Integer(3),
            Value::Integer(4),
        ];
        let thread = heap.allocate_thread(&roots).unwrap();
        let slot_bytes = checked_vector_bytes::<Slot>(1).unwrap();

        heap.thread_set_roots(thread, &[Value::Integer(1)]).unwrap();
        assert_eq!(
            heap.memory_usage().current_bytes,
            checked_add(slot_bytes, checked_vector_bytes::<Value>(1).unwrap()).unwrap()
        );
        heap.thread_set_roots(thread, &[]).unwrap();
        assert_eq!(heap.memory_usage().current_bytes, slot_bytes);
    }

    #[test]
    fn hash_growth_is_charged_before_insertion_and_rolls_back_on_limit() {
        let slot_bytes = checked_vector_bytes::<Slot>(1).unwrap();
        let hash_bytes = checked_hash_bytes::<Key, Value>(1).unwrap();
        let limit = checked_add(slot_bytes, hash_bytes).unwrap();
        let mut heap = Heap::try_new(limited_memory(limit)).unwrap();
        let table = heap.allocate_table(0, 0).unwrap();

        let first = Value::String(Arc::from(&b"first"[..]));
        let second = Value::String(Arc::from(&b"second"[..]));
        heap.table_set(table, first.clone(), Value::Integer(1))
            .unwrap();
        assert_eq!(heap.memory_usage().current_bytes, limit);
        assert!(matches!(
            heap.table_set(table, second.clone(), Value::Integer(2)),
            Err(HeapError::Memory(MemoryError::LimitExceeded { .. }))
        ));
        assert_eq!(heap.table_get(table, &first), Ok(Value::Integer(1)));
        assert_eq!(heap.table_get(table, &second), Ok(Value::Nil));
        assert_eq!(heap.memory_usage().current_bytes, limit);
    }

    #[test]
    fn collection_releases_closure_upvalue_capacity() {
        let slot_bytes = checked_vector_bytes::<Slot>(1).unwrap();
        let upvalue_bytes = checked_vector_bytes::<UpvalueId>(2).unwrap();
        let limit = checked_add(slot_bytes, upvalue_bytes).unwrap();
        let mut heap = Heap::try_new(limited_memory(limit)).unwrap();

        heap.allocate_closure(empty_chunk(), 0, SemanticProfile::Blu, 2)
            .unwrap();
        assert_eq!(heap.memory_usage().current_bytes, limit);
        heap.collect(std::iter::empty()).unwrap();
        assert_eq!(heap.memory_usage().current_bytes, slot_bytes);
    }
}
