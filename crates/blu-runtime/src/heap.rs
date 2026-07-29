use crate::Value;
use core::fmt;
use std::{
    collections::{HashMap, VecDeque},
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
    StaleTable(TableId),
    StaleClosure(ClosureId),
    StaleUpvalue(UpvalueId),
    NilKey,
    NanKey,
    InvalidIterationKey,
}

impl fmt::Display for HeapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleTable(value) => write!(f, "stale or invalid table handle {value:?}"),
            Self::StaleClosure(value) => write!(f, "stale or invalid closure handle {value:?}"),
            Self::StaleUpvalue(value) => write!(f, "stale or invalid upvalue handle {value:?}"),
            Self::NilKey => f.write_str("table index is nil"),
            Self::NanKey => f.write_str("table index is NaN"),
            Self::InvalidIterationKey => f.write_str("invalid key to table iteration"),
        }
    }
}

impl std::error::Error for HeapError {}

#[derive(Clone, Debug, Default)]
pub struct Heap {
    slots: Vec<Slot>,
    free: Vec<u32>,
    live: usize,
}

impl Heap {
    #[must_use]
    pub fn allocate_table(&mut self, array_capacity: usize, hash_capacity: usize) -> TableId {
        let id = self.allocate(Object::Table(Table {
            array: Vec::with_capacity(array_capacity),
            hash: HashMap::with_capacity(hash_capacity),
            metatable: None,
        }));
        TableId {
            index: id.index,
            generation: id.generation,
        }
    }

    pub(crate) fn allocate_upvalue(&mut self, value: Value) -> UpvalueId {
        let id = self.allocate(Object::Upvalue(value));
        UpvalueId {
            index: id.index,
            generation: id.generation,
        }
    }

    pub(crate) fn allocate_closure(
        &mut self,
        prototype: usize,
        upvalues: Vec<UpvalueId>,
    ) -> ClosureId {
        let id = self.allocate(Object::Closure(Closure {
            prototype,
            upvalues,
        }));
        ClosureId {
            index: id.index,
            generation: id.generation,
        }
    }

    #[must_use]
    pub const fn live_objects(&self) -> usize {
        self.live
    }

    pub fn table_get(&self, table: TableId, key: &Value) -> Result<Value, HeapError> {
        let key = Key::from_value(key)?;
        let table = self.table(table)?;
        Ok(table.get(&key).cloned().unwrap_or(Value::Nil))
    }

    pub fn table_set(&mut self, table: TableId, key: Value, value: Value) -> Result<(), HeapError> {
        let key = Key::from_value(&key)?;
        self.table_mut(table)?.set(key, value);
        Ok(())
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
    ) -> Result<(usize, Vec<UpvalueId>), HeapError> {
        let closure = self.closure(closure)?;
        Ok((closure.prototype, closure.upvalues.clone()))
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

    pub fn collect<'a>(&mut self, roots: impl IntoIterator<Item = &'a Value>) -> CollectionStats {
        let before = self.live;
        let mut queue = VecDeque::new();
        for root in roots {
            enqueue_value(root, &mut queue);
        }

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
            }
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
        CollectionStats {
            before,
            retained: self.live,
            collected: before - self.live,
        }
    }

    fn allocate(&mut self, object: Object) -> ObjectId {
        self.live += 1;
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            debug_assert!(slot.object.is_none());
            slot.object = Some(object);
            ObjectId {
                index,
                generation: slot.generation,
            }
        } else {
            let index = self.slots.len() as u32;
            self.slots.push(Slot {
                generation: 0,
                marked: false,
                object: Some(object),
            });
            ObjectId {
                index,
                generation: 0,
            }
        }
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

fn enqueue_value(value: &Value, queue: &mut VecDeque<ObjectId>) {
    match value {
        Value::Table(value) => queue.push_back((*value).into()),
        Value::Closure(value) => queue.push_back((*value).into()),
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
}

#[derive(Clone, Debug)]
struct Closure {
    prototype: usize,
    upvalues: Vec<UpvalueId>,
}

#[derive(Clone, Debug)]
struct Table {
    array: Vec<Value>,
    hash: HashMap<Key, Value>,
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

    fn set(&mut self, key: Key, value: Value) {
        if let Some(index) = key.array_index() {
            if index <= self.array.len() {
                self.array[index - 1] = value;
                self.trim_array();
                return;
            }
            if index == self.array.len() + 1 && !matches!(value, Value::Nil) {
                self.array.push(value);
                self.promote_contiguous();
                return;
            }
        }
        if matches!(value, Value::Nil) {
            self.hash.remove(&key);
        } else {
            self.hash.insert(key, value);
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

    fn promote_contiguous(&mut self) {
        loop {
            let key = Key::Integer((self.array.len() + 1) as i64);
            let Some(value) = self.hash.remove(&key) else {
                break;
            };
            self.array.push(value);
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum Key {
    Boolean(bool),
    Integer(i64),
    Number(u64),
    String(Arc<[u8]>),
    Table(TableId),
    Closure(ClosureId),
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
            Self::NativeFunction(value) => Value::NativeFunction(*value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_split_dense_integer_keys_and_hash_keys() {
        let mut heap = Heap::default();
        let table = heap.allocate_table(0, 0);
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
        let retained = heap.allocate_table(0, 0);
        let child = heap.allocate_table(0, 0);
        let garbage = heap.allocate_table(0, 0);
        heap.table_set(retained, Value::Integer(1), Value::Table(child))
            .unwrap();
        heap.table_set(child, Value::Integer(1), Value::Table(retained))
            .unwrap();
        let upvalue = heap.allocate_upvalue(Value::Table(retained));
        let closure = heap.allocate_closure(0, vec![upvalue]);

        let root = Value::Closure(closure);
        assert_eq!(
            heap.collect([&root]),
            CollectionStats {
                before: 5,
                retained: 4,
                collected: 1
            }
        );
        assert_eq!(
            heap.table_get(garbage, &Value::Integer(1)),
            Err(HeapError::StaleTable(garbage))
        );
        assert_eq!(
            heap.collect(std::iter::empty()),
            CollectionStats {
                before: 4,
                retained: 0,
                collected: 4,
            }
        );
    }

    #[test]
    fn metatables_are_traced_and_support_iteration() {
        let mut heap = Heap::default();
        let table = heap.allocate_table(0, 0);
        let metatable = heap.allocate_table(0, 1);
        heap.table_set(
            metatable,
            Value::String(Arc::from(&b"answer"[..])),
            Value::Integer(42),
        )
        .unwrap();
        heap.set_table_metatable(table, Some(metatable)).unwrap();

        assert_eq!(
            heap.collect([&Value::Table(table)]),
            CollectionStats {
                before: 2,
                retained: 2,
                collected: 0,
            }
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
}
