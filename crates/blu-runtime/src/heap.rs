use crate::{
    MemoryAccount, MemoryConfig, MemoryError, MemoryUsage, Value, checked_hash_bytes,
    checked_vector_bytes,
};
use blu_bytecode::Chunk;
use core::fmt;
use std::{
    collections::{HashMap, VecDeque},
    hash::Hash,
    sync::Arc,
};

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

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct ObjectId {
    index: u32,
    generation: u32,
}

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
    StaleUpvalue(UpvalueId),
    StaleThread(ThreadId),
    NilKey,
    NanKey,
    InvalidIterationKey,
}

impl fmt::Display for HeapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Memory(error) => error.fmt(f),
            Self::StaleTable(value) => write!(f, "stale or invalid table handle {value:?}"),
            Self::StaleClosure(value) => write!(f, "stale or invalid closure handle {value:?}"),
            Self::StaleUpvalue(value) => write!(f, "stale or invalid upvalue handle {value:?}"),
            Self::StaleThread(value) => write!(f, "stale or invalid thread handle {value:?}"),
            Self::NilKey => f.write_str("table index is nil"),
            Self::NanKey => f.write_str("table index is NaN"),
            Self::InvalidIterationKey => f.write_str("invalid key to table iteration"),
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
    live: usize,
    memory: MemoryAccount,
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
            live: 0,
            memory: MemoryAccount::new(memory),
        })
    }

    #[must_use]
    pub const fn memory_usage(&self) -> MemoryUsage {
        self.memory.usage()
    }

    #[must_use]
    pub(crate) const fn should_collect(&self, requested: usize) -> bool {
        self.memory.should_collect(requested)
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
                hash,
                hash_capacity,
                metatable: None,
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
        upvalue_capacity: usize,
    ) -> Result<ClosureId, HeapError> {
        let dynamic_bytes = checked_vector_bytes::<UpvalueId>(upvalue_capacity)?;
        let id = self.allocate(dynamic_bytes, || {
            let mut upvalues = Vec::new();
            try_reserve_vec_exact(&mut upvalues, upvalue_capacity)?;
            Ok(Object::Closure(Closure {
                chunk,
                prototype,
                upvalues,
                upvalue_capacity,
            }))
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
        let key = Key::from_value(key)?;
        let table = self.table(table)?;
        Ok(table.get(&key).cloned().unwrap_or(Value::Nil))
    }

    pub fn table_set(&mut self, table: TableId, key: Value, value: Value) -> Result<(), HeapError> {
        let key = Key::from_value(&key)?;
        let Self { slots, memory, .. } = self;
        table_mut_in_slots(slots, table)?.set(key, value, memory)
    }

    pub(crate) fn table_set_bytes(
        &self,
        table: TableId,
        key: &Value,
        value: &Value,
    ) -> Result<usize, HeapError> {
        let key = Key::from_value(key)?;
        self.table(table)?.set_bytes(&key, value)
    }

    pub fn table_length(&self, table: TableId) -> Result<usize, HeapError> {
        Ok(self.table(table)?.length())
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
        self.table_mut(table)?.metatable = metatable;
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
        let mut entries = table
            .array
            .iter()
            .enumerate()
            .filter(|(_, value)| !matches!(value, Value::Nil))
            .map(|(index, value)| (Value::Integer((index + 1) as i64), value.clone()))
            .chain(
                table
                    .hash
                    .iter()
                    .map(|(key, value)| (key.to_value(), value.clone())),
            );
        if matches!(key, Value::Nil) {
            return Ok(entries.next());
        }
        let key = Key::from_value(key)?;
        while let Some((candidate, _)) = entries.next() {
            if Key::from_value(&candidate)? == key {
                return Ok(entries.next());
            }
        }
        Err(HeapError::InvalidIterationKey)
    }

    pub(crate) fn closure_parts(
        &self,
        closure: ClosureId,
    ) -> Result<(Arc<Chunk>, usize, Vec<UpvalueId>), HeapError> {
        let closure = self.closure(closure)?;
        Ok((
            closure.chunk.clone(),
            closure.prototype,
            try_clone_slice(&closure.upvalues)?,
        ))
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
        let before = self.live;
        let mut queue = VecDeque::new();
        for root in roots {
            enqueue_value(root, &mut queue);
        }
        queue.extend(upvalues.into_iter().map(ObjectId::from));

        while let Some(id) = queue.pop_front() {
            let Some(slot) = self.slots.get_mut(id.index as usize) else {
                continue;
            };
            if slot.generation != id.generation || slot.marked {
                continue;
            }
            let Some(object) = slot.object.as_ref() else {
                continue;
            };
            slot.marked = true;
            match object {
                Object::Table(table) => {
                    if let Some(metatable) = table.metatable {
                        queue.push_back(metatable.into());
                    }
                    for key in table.hash.keys() {
                        key.enqueue(&mut queue);
                    }
                    for value in table.array.iter().chain(table.hash.values()) {
                        enqueue_value(value, &mut queue);
                    }
                }
                Object::Closure(closure) => {
                    queue.extend(closure.upvalues.iter().copied().map(ObjectId::from));
                }
                Object::Upvalue(value) => enqueue_value(value, &mut queue),
                Object::Thread(thread) => {
                    for root in &thread.roots {
                        enqueue_value(root, &mut queue);
                    }
                    queue.extend(thread.upvalues.iter().copied().map(ObjectId::from));
                }
            }
        }

        let sweep = self
            .slots
            .iter()
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

        for (index, slot) in self.slots.iter_mut().enumerate() {
            if slot.object.is_none() {
                continue;
            }
            if slot.marked {
                slot.marked = false;
            } else {
                slot.object = None;
                slot.generation = slot.generation.wrapping_add(1);
                self.free.push(index as u32);
                self.live -= 1;
            }
        }
        self.memory.finish_collection();
        Ok(CollectionStats {
            before,
            retained: self.live,
            collected: before - self.live,
        })
    }

    fn clear_marks(&mut self) {
        for slot in &mut self.slots {
            slot.marked = false;
        }
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
        let object = build()?;
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            debug_assert!(slot.object.is_none());
            slot.object = Some(object);
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
        Value::NativeFunction(_) => {}
        _ => {}
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
    Upvalue(Value),
    Thread(Thread),
}

impl Object {
    fn dynamic_bytes(&self) -> Result<usize, MemoryError> {
        match self {
            Self::Table(table) => checked_add(
                checked_vector_bytes::<Value>(table.array_capacity)?,
                checked_hash_bytes::<Key, Value>(table.hash_capacity)?,
            ),
            Self::Closure(closure) => checked_vector_bytes::<UpvalueId>(closure.upvalue_capacity),
            Self::Thread(thread) => checked_add(
                checked_vector_bytes::<Value>(thread.root_capacity)?,
                checked_vector_bytes::<UpvalueId>(thread.upvalue_capacity)?,
            ),
            Self::Upvalue(_) => Ok(0),
        }
    }
}

#[derive(Clone, Debug)]
struct Closure {
    chunk: Arc<Chunk>,
    prototype: usize,
    upvalues: Vec<UpvalueId>,
    upvalue_capacity: usize,
}

#[derive(Clone, Debug)]
struct Thread {
    roots: Vec<Value>,
    root_capacity: usize,
    upvalues: Vec<UpvalueId>,
    upvalue_capacity: usize,
}

#[derive(Clone, Debug)]
struct Table {
    array: Vec<Value>,
    array_capacity: usize,
    hash: HashMap<Key, Value>,
    hash_capacity: usize,
    metatable: Option<TableId>,
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
        if let Some(index) = key.array_index() {
            if index <= self.array.len() {
                self.array[index - 1] = value;
                self.trim_array();
                return Ok(());
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
                self.grow_array(required, memory)?;
                self.array.push(value);
                self.promote_contiguous(promoted);
                return Ok(());
            }
        }
        if matches!(value, Value::Nil) {
            self.hash.remove(&key);
        } else {
            if !self.hash.contains_key(&key) {
                let required = self
                    .hash
                    .len()
                    .checked_add(1)
                    .ok_or(MemoryError::SizeOverflow)?;
                self.grow_hash(required, memory)?;
            }
            self.hash.insert(key, value);
        }
        Ok(())
    }

    fn set_bytes(&self, key: &Key, value: &Value) -> Result<usize, HeapError> {
        if let Some(index) = key.array_index() {
            if index <= self.array.len() {
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
                return if required <= self.array_capacity {
                    Ok(0)
                } else {
                    Ok(checked_vector_bytes::<Value>(required)?)
                };
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
        if required <= self.hash_capacity {
            Ok(0)
        } else {
            Ok(checked_hash_bytes::<Key, Value>(required)?)
        }
    }

    fn length(&self) -> usize {
        self.array
            .iter()
            .rposition(|value| !matches!(value, Value::Nil))
            .map_or(0, |index| index + 1)
    }

    fn trim_array(&mut self) {
        while self
            .array
            .last()
            .is_some_and(|value| matches!(value, Value::Nil))
        {
            self.array.pop();
        }
    }

    fn promote_contiguous(&mut self, count: usize) {
        for _ in 0..count {
            let key = Key::Integer((self.array.len() + 1) as i64);
            let Some(value) = self.hash.remove(&key) else {
                return;
            };
            self.array.push(value);
        }
    }

    fn contiguous_hash_values_after(&self, index: usize) -> Result<usize, HeapError> {
        let mut next = index.checked_add(1).ok_or(MemoryError::SizeOverflow)?;
        let mut count = 0usize;
        loop {
            let key = i64::try_from(next).map_err(|_| MemoryError::SizeOverflow)?;
            if !self.hash.contains_key(&Key::Integer(key)) {
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
        let old_bytes = checked_vector_bytes::<Value>(self.array_capacity)?;
        let new_bytes = checked_vector_bytes::<Value>(required)?;
        let reservation = memory.reserve(new_bytes)?;
        try_reserve_vec_to(&mut self.array, required)?;
        reservation.commit_replacing(old_bytes)?;
        self.array_capacity = required;
        Ok(())
    }

    fn grow_hash(&mut self, required: usize, memory: &mut MemoryAccount) -> Result<(), HeapError> {
        if required <= self.hash_capacity {
            return Ok(());
        }
        let old_bytes = checked_hash_bytes::<Key, Value>(self.hash_capacity)?;
        let new_bytes = checked_hash_bytes::<Key, Value>(required)?;
        let reservation = memory.reserve(new_bytes)?;
        try_reserve_hash_to(&mut self.hash, required)?;
        reservation.commit_replacing(old_bytes)?;
        self.hash_capacity = required;
        Ok(())
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
            _ => {}
        }
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
            Self::NativeFunction(value) => Value::NativeFunction(*value),
        }
    }
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
        let closure = heap.allocate_closure(empty_chunk(), 0, 1).unwrap();
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
            heap.allocate_closure(empty_chunk(), 0, usize::MAX),
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

        heap.allocate_closure(empty_chunk(), 0, 2).unwrap();
        assert_eq!(heap.memory_usage().current_bytes, limit);
        heap.collect(std::iter::empty()).unwrap();
        assert_eq!(heap.memory_usage().current_bytes, slot_bytes);
    }
}
