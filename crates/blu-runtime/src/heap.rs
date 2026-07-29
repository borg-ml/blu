use crate::Value;
use core::fmt;
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct TableId {
    index: u32,
    generation: u32,
}

impl fmt::Debug for TableId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Table({}:{})", self.index, self.generation)
    }
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
    NilKey,
    NanKey,
}

impl fmt::Display for HeapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleTable(table) => write!(f, "stale or invalid table handle {table:?}"),
            Self::NilKey => f.write_str("table index is nil"),
            Self::NanKey => f.write_str("table index is NaN"),
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
        let table = Table {
            array: Vec::with_capacity(array_capacity),
            hash: HashMap::with_capacity(hash_capacity),
        };
        self.live += 1;
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            debug_assert!(slot.object.is_none());
            slot.object = Some(Object::Table(table));
            TableId {
                index,
                generation: slot.generation,
            }
        } else {
            let index = self.slots.len() as u32;
            self.slots.push(Slot {
                generation: 0,
                marked: false,
                object: Some(Object::Table(table)),
            });
            TableId {
                index,
                generation: 0,
            }
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

    pub fn collect<'a>(&mut self, roots: impl IntoIterator<Item = &'a Value>) -> CollectionStats {
        let before = self.live;
        let mut queue = VecDeque::new();
        for root in roots {
            enqueue_value(root, &mut queue);
        }

        while let Some(table) = queue.pop_front() {
            let Some(slot) = self.slots.get_mut(table.index as usize) else {
                continue;
            };
            if slot.generation != table.generation || slot.marked {
                continue;
            }
            let Some(Object::Table(table)) = slot.object.as_ref() else {
                continue;
            };
            slot.marked = true;
            for key in table.hash.keys() {
                if let Key::Table(table) = key {
                    queue.push_back(*table);
                }
            }
            for value in table.array.iter().chain(table.hash.values()) {
                enqueue_value(value, &mut queue);
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

    fn table(&self, id: TableId) -> Result<&Table, HeapError> {
        let slot = self
            .slots
            .get(id.index as usize)
            .filter(|slot| slot.generation == id.generation)
            .ok_or(HeapError::StaleTable(id))?;
        match slot.object.as_ref() {
            Some(Object::Table(table)) => Ok(table),
            None => Err(HeapError::StaleTable(id)),
        }
    }

    fn table_mut(&mut self, id: TableId) -> Result<&mut Table, HeapError> {
        let slot = self
            .slots
            .get_mut(id.index as usize)
            .filter(|slot| slot.generation == id.generation)
            .ok_or(HeapError::StaleTable(id))?;
        match slot.object.as_mut() {
            Some(Object::Table(table)) => Ok(table),
            None => Err(HeapError::StaleTable(id)),
        }
    }
}

fn enqueue_value(value: &Value, queue: &mut VecDeque<TableId>) {
    if let Value::Table(table) = value {
        queue.push_back(*table);
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
}

#[derive(Clone, Debug)]
struct Table {
    array: Vec<Value>,
    hash: HashMap<Key, Value>,
}

impl Table {
    fn get(&self, key: &Key) -> Option<&Value> {
        if let Some(index) = key.array_index() {
            if index <= self.array.len() {
                return self.array.get(index - 1);
            }
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
        }
    }

    fn array_index(&self) -> Option<usize> {
        match self {
            Self::Integer(value) if *value > 0 => usize::try_from(*value).ok(),
            _ => None,
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
        assert_eq!(
            heap.table_get(table, &Value::String(Arc::from(&b"name"[..])))
                .unwrap(),
            Value::Boolean(true)
        );
    }

    #[test]
    fn tracing_collection_handles_cycles_and_invalidates_stale_handles() {
        let mut heap = Heap::default();
        let retained = heap.allocate_table(0, 0);
        let child = heap.allocate_table(0, 0);
        let garbage = heap.allocate_table(0, 0);
        heap.table_set(retained, Value::Integer(1), Value::Table(child))
            .unwrap();
        heap.table_set(child, Value::Integer(1), Value::Table(retained))
            .unwrap();

        let root = Value::Table(retained);
        assert_eq!(
            heap.collect([&root]),
            CollectionStats {
                before: 3,
                retained: 2,
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
                before: 2,
                retained: 0,
                collected: 2,
            }
        );
    }
}
