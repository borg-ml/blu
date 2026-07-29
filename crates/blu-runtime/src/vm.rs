use crate::heap::UpvalueId;
use crate::{ClosureId, Dialect, Heap, HeapError, NativeFunctionId, TableId, Value};
use blu_bytecode::{Chunk, Constant, Instruction, Opcode, Prototype};
use core::fmt;
use std::{collections::HashMap, sync::Arc};

const MAX_DYNAMIC_REGISTERS: usize = 1_000_000;

type NativeFunction =
    Arc<dyn Fn(&mut Vm, &[Value]) -> Result<Vec<Value>, RuntimeError> + Send + Sync>;

#[derive(Clone)]
pub struct Vm {
    dialect: Dialect,
    instruction_limit: u64,
    call_limit: usize,
    heap: Heap,
    globals: HashMap<Arc<[u8]>, Value>,
    native_functions: Vec<NativeFunction>,
    protected_call: Option<NativeFunctionId>,
    output: Vec<u8>,
    active_roots: Vec<Vec<Value>>,
}

impl Default for Vm {
    fn default() -> Self {
        Self::new(Dialect::Luau)
    }
}

impl Vm {
    #[must_use]
    pub fn new(dialect: Dialect) -> Self {
        let mut vm = Self {
            dialect,
            instruction_limit: 10_000_000,
            call_limit: 1_000,
            heap: Heap::default(),
            globals: HashMap::new(),
            native_functions: Vec::new(),
            protected_call: None,
            output: Vec::new(),
            active_roots: Vec::new(),
        };
        vm.install_base_library();
        vm
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
    pub const fn heap(&self) -> &Heap {
        &self.heap
    }

    pub fn register_function(
        &mut self,
        function: impl Fn(&mut Vm, &[Value]) -> Result<Vec<Value>, RuntimeError> + Send + Sync + 'static,
    ) -> NativeFunctionId {
        let id = NativeFunctionId(self.native_functions.len() as u32);
        self.native_functions.push(Arc::new(function));
        id
    }

    pub fn set_global(&mut self, name: impl Into<Arc<[u8]>>, value: Value) {
        self.globals.insert(name.into(), value);
    }

    #[must_use]
    pub fn global(&self, name: &[u8]) -> Option<&Value> {
        self.globals.get(name)
    }

    pub fn take_output(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.output)
    }

    pub fn collect<'a>(
        &mut self,
        roots: impl IntoIterator<Item = &'a Value>,
    ) -> crate::CollectionStats {
        let mut all_roots: Vec<Value> = self.globals.values().cloned().collect();
        all_roots.extend(self.active_roots.iter().flatten().cloned());
        all_roots.extend(roots.into_iter().cloned());
        self.heap.collect(&all_roots)
    }

    pub fn execute(&mut self, chunk: &Chunk) -> Result<Vec<Value>, RuntimeError> {
        if self.dialect != Dialect::Luau {
            return Err(RuntimeError::DialectNotImplemented(self.dialect));
        }
        let mut remaining = self.instruction_limit;
        self.execute_frame(chunk, chunk.main, None, &[], &mut remaining, 0)
    }

    fn execute_frame(
        &mut self,
        chunk: &Chunk,
        prototype_index: usize,
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
            .get(prototype_index)
            .ok_or(RuntimeError::InvalidPrototype(prototype_index))?;
        let constants = materialize_constants(chunk, prototype)?;
        let mut frame = Frame::new(prototype, constants, closure, arguments);

        loop {
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
                    let mut upvalues = Vec::with_capacity(upvalue_count as usize);
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
                        let upvalue = match capture.a() {
                            0 => self.heap.allocate_upvalue(frame.get(capture.b())?.clone()),
                            1 => frame.capture_ref(&mut self.heap, capture.b())?,
                            2 => frame.upvalue(&self.heap, capture.b())?,
                            kind => {
                                return Err(RuntimeError::CaptureType {
                                    pc: capture.pc(),
                                    kind,
                                });
                            }
                        };
                        upvalues.push(upvalue);
                    }
                    let closure = self.heap.allocate_closure(child, upvalues);
                    frame.set(instruction.a(), Value::Closure(closure))?;
                }
                Opcode::Capture => {
                    return Err(RuntimeError::UnexpectedCapture {
                        pc: instruction.pc(),
                    });
                }
                Opcode::GetVarargs => {
                    let count = if instruction.b() == 0 {
                        frame.varargs.len()
                    } else {
                        usize::from(instruction.b() - 1)
                    };
                    let values = frame.varargs.clone();
                    if instruction.b() == 0 {
                        frame.ensure_dynamic(usize::from(instruction.a()) + count)?;
                    }
                    for offset in 0..count {
                        let register = u8::try_from(usize::from(instruction.a()) + offset)
                            .map_err(|_| RuntimeError::Register {
                                register: usize::from(instruction.a()) + offset,
                                count: frame.registers.len(),
                            })?;
                        frame.set(register, values.get(offset).cloned().unwrap_or(Value::Nil))?;
                    }
                    if instruction.b() == 0 {
                        frame.top = usize::from(instruction.a()) + count;
                    }
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
                        CallContext::new(chunk, remaining, depth, frame.gc_roots(&self.heap)?),
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
                        CallContext::new(chunk, remaining, depth, frame.gc_roots(&self.heap)?),
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
                        CallContext::new(chunk, remaining, depth, frame.gc_roots(&self.heap)?),
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
                            let argument = other.clone();
                            let result = self.call_value(
                                chunk,
                                function,
                                &[argument],
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
                                    chunk,
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
                    let hash_capacity = if instruction.b() == 0 {
                        0
                    } else {
                        1usize
                            .checked_shl(u32::from(instruction.b() - 1))
                            .unwrap_or(usize::MAX)
                    };
                    let table = self
                        .heap
                        .allocate_table(array_capacity, hash_capacity.min(1 << 20));
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
                                    materialize_constant(chunk, prototype, *value as usize)?
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
                    let table = self.heap.allocate_table(0, entries.len());
                    for (key, value) in entries {
                        let key = materialize_constant(chunk, prototype, key)?;
                        self.heap.table_set(table, key, value)?;
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
                        CallContext::new(chunk, remaining, depth, frame.gc_roots(&self.heap)?),
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
                        CallContext::new(chunk, remaining, depth, frame.gc_roots(&self.heap)?),
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
                        CallContext::new(chunk, remaining, depth, frame.gc_roots(&self.heap)?),
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
                        CallContext::new(chunk, remaining, depth, frame.gc_roots(&self.heap)?),
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
                        CallContext::new(chunk, remaining, depth, frame.gc_roots(&self.heap)?),
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
                        CallContext::new(chunk, remaining, depth, frame.gc_roots(&self.heap)?),
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
                        self.heap.table_set(
                            table,
                            Value::Integer((start + offset) as i64),
                            value,
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
                        CallContext::new(chunk, remaining, depth, frame.gc_roots(&self.heap)?),
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
                    let arguments = vec![
                        frame.get(state_register)?.clone(),
                        frame.get(index_register)?.clone(),
                    ];
                    let variable_count = usize::try_from(
                        instruction.aux().ok_or(RuntimeError::MissingAux {
                            pc: instruction.pc(),
                            opcode: instruction.opcode(),
                        })? & 0xff,
                    )
                    .expect("u8 fits usize");
                    let results = self.call_value(
                        chunk,
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
                        CallContext::new(chunk, remaining, depth, frame.gc_roots(&self.heap)?),
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
                            CallContext::new(chunk, remaining, depth, frame.gc_roots(&self.heap)?),
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
                    let arguments = frame.register_slice(start, count)?.to_vec();
                    let results = self.call_value(
                        chunk,
                        function,
                        &arguments,
                        remaining,
                        depth,
                        frame.gc_roots(&self.heap)?,
                    )?;
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
                    return Ok(frame.registers[start..end].to_vec());
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
        chunk: &Chunk,
        function: Value,
        arguments: &[Value],
        remaining: &mut u64,
        depth: usize,
        roots: Vec<Value>,
    ) -> Result<Vec<Value>, RuntimeError> {
        match function {
            Value::Closure(closure) => {
                let (child, _) = self.heap.closure_parts(closure)?;
                self.active_roots.push(roots);
                let result = self.execute_frame(
                    chunk,
                    child,
                    Some(closure),
                    arguments,
                    remaining,
                    depth + 1,
                );
                self.active_roots.pop();
                result
            }
            Value::NativeFunction(function) => {
                if self.protected_call == Some(function) {
                    let target = arguments.first().cloned().ok_or(RuntimeError::Argument {
                        function: "pcall",
                        index: 1,
                    })?;
                    let result = self.call_value(
                        chunk,
                        target,
                        arguments.get(1..).unwrap_or_default(),
                        remaining,
                        depth,
                        roots,
                    );
                    return Ok(match result {
                        Ok(values) => {
                            let mut protected = Vec::with_capacity(values.len() + 1);
                            protected.push(Value::Boolean(true));
                            protected.extend(values);
                            protected
                        }
                        Err(RuntimeError::Raised(value)) => {
                            vec![Value::Boolean(false), value]
                        }
                        Err(error) => vec![
                            Value::Boolean(false),
                            Value::String(Arc::from(error.to_string().into_bytes())),
                        ],
                    });
                }
                let function = self
                    .native_functions
                    .get(function.0 as usize)
                    .cloned()
                    .ok_or(RuntimeError::NativeFunction(function.0))?;
                self.active_roots.push(roots);
                let result = function(self, arguments);
                self.active_roots.pop();
                result
            }
            Value::Table(table) => {
                let function =
                    self.metamethod(&Value::Table(table), "__call")?
                        .ok_or(RuntimeError::Type {
                            operation: "call",
                            expected: "function or __call metamethod",
                            actual: "table",
                        })?;
                let mut metamethod_arguments = Vec::with_capacity(arguments.len() + 1);
                metamethod_arguments.push(Value::Table(table));
                metamethod_arguments.extend_from_slice(arguments);
                self.call_value(
                    chunk,
                    function,
                    &metamethod_arguments,
                    remaining,
                    depth,
                    roots,
                )
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
                function @ (Value::Closure(_) | Value::NativeFunction(_)) => {
                    return Ok(self
                        .call_value(
                            context.chunk,
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
                self.heap.table_set(table, key, assigned)?;
                return Ok(());
            }
            let Some(metatable) = self.heap.table_metatable(table)? else {
                self.heap.table_set(table, key, assigned)?;
                return Ok(());
            };
            let newindex = self
                .heap
                .table_get(metatable, &Value::String(Arc::from(&b"__newindex"[..])))?;
            match newindex {
                Value::Nil => {
                    self.heap.table_set(table, key, assigned)?;
                    return Ok(());
                }
                Value::Table(next) => table = next,
                function @ (Value::Closure(_) | Value::NativeFunction(_)) => {
                    self.call_value(
                        context.chunk,
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
        if left.as_number().is_some() && right.as_number().is_some() {
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
                context.chunk,
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
        if let (Some(left), Some(right)) = (concat_bytes(&left), concat_bytes(&right)) {
            let mut result = Vec::with_capacity(left.len() + right.len());
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
                context.chunk,
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
        } else if matches!(value, Value::Closure(_) | Value::NativeFunction(_)) {
            Ok(Some(value))
        } else {
            Err(RuntimeError::UnsupportedMetamethod {
                name,
                actual: value.type_name(),
            })
        }
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
                                context.chunk,
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
                if let (Some(left), Some(right)) = (left.as_number(), right.as_number()) {
                    left < right
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
                        context.chunk,
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
                if let (Some(left), Some(right)) = (left.as_number(), right.as_number()) {
                    left <= right
                } else if let (Value::String(left), Value::String(right)) = (&left, &right) {
                    left <= right
                } else if let Some(function) = self.shared_metamethod(&left, &right, "__le")? {
                    self.call_value(
                        context.chunk,
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
                            context.chunk,
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

    fn install_base_library(&mut self) {
        let next = self.register_function(|vm, arguments| {
            let table = arguments.first().ok_or(RuntimeError::Argument {
                function: "next",
                index: 1,
            })?;
            let table = table_id(table)?;
            let key = arguments.get(1).unwrap_or(&Value::Nil);
            Ok(vm
                .heap
                .table_next(table, key)?
                .map_or_else(Vec::new, |(key, value)| vec![key, value]))
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
                Ok(vec![Value::Integer(index), value])
            }
        });
        let ipairs = self.register_function(move |_, arguments| {
            let table = arguments.first().ok_or(RuntimeError::Argument {
                function: "ipairs",
                index: 1,
            })?;
            table_id(table)?;
            Ok(vec![
                Value::NativeFunction(inext),
                table.clone(),
                Value::Integer(0),
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

        let tostring = self.register_function(|_, arguments| {
            let value = arguments.first().ok_or(RuntimeError::Argument {
                function: "tostring",
                index: 1,
            })?;
            let mut result = Vec::new();
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
            vm.heap.table_set(table, key.clone(), assigned.clone())?;
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
            Ok(vec![Value::Number(length as f64)])
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
                Ok(arguments.to_vec())
            } else {
                Err(RuntimeError::Raised(arguments.get(1).cloned().unwrap_or(
                    Value::String(Arc::from(&b"assertion failed!"[..])),
                )))
            }
        });
        self.set_global(&b"assert"[..], Value::NativeFunction(assert));

        let select = self.register_function(|_, arguments| {
            let selector = arguments.first().ok_or(RuntimeError::Argument {
                function: "select",
                index: 1,
            })?;
            if matches!(selector, Value::String(value) if &**value == b"#") {
                return Ok(vec![Value::Integer(
                    arguments.len().saturating_sub(1) as i64
                )]);
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
            Ok(arguments.get(index as usize..).unwrap_or_default().to_vec())
        });
        self.set_global(&b"select"[..], Value::NativeFunction(select));

        let pcall = self.register_function(|_, _| Err(RuntimeError::NativeFunction(u32::MAX)));
        self.protected_call = Some(pcall);
        self.set_global(&b"pcall"[..], Value::NativeFunction(pcall));

        let print = self.register_function(|vm, arguments| {
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
        let string = self.heap.allocate_table(0, 1);
        self.heap
            .table_set(
                string,
                Value::String(Arc::from(&b"sub"[..])),
                Value::NativeFunction(string_sub),
            )
            .expect("valid built-in table key");
        self.set_global(&b"string"[..], Value::Table(string));
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

fn relative_index(index: i64, length: usize) -> i64 {
    if index < 0 {
        length as i64 + index + 1
    } else {
        index
    }
}

fn concat_bytes(value: &Value) -> Option<Vec<u8>> {
    match value {
        Value::String(value) => Some(value.to_vec()),
        Value::Integer(value) => Some(value.to_string().into_bytes()),
        Value::Number(value) => Some(value.to_string().into_bytes()),
        _ => None,
    }
}

fn append_value(output: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Nil => output.extend_from_slice(b"nil"),
        Value::Boolean(true) => output.extend_from_slice(b"true"),
        Value::Boolean(false) => output.extend_from_slice(b"false"),
        Value::Number(value) => output.extend_from_slice(value.to_string().as_bytes()),
        Value::Integer(value) => output.extend_from_slice(value.to_string().as_bytes()),
        Value::String(value) => output.extend_from_slice(value),
        Value::Table(value) => output.extend_from_slice(format!("{value:?}").as_bytes()),
        Value::Closure(value) => output.extend_from_slice(format!("{value:?}").as_bytes()),
        Value::NativeFunction(value) => output.extend_from_slice(format!("{value:?}").as_bytes()),
    }
}

impl fmt::Debug for Vm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vm")
            .field("dialect", &self.dialect)
            .field("instruction_limit", &self.instruction_limit)
            .field("call_limit", &self.call_limit)
            .field("heap", &self.heap)
            .field("globals", &self.globals)
            .field("native_function_count", &self.native_functions.len())
            .field("protected_call", &self.protected_call)
            .field("active_frame_count", &self.active_roots.len())
            .finish_non_exhaustive()
    }
}

struct CallContext<'a> {
    chunk: &'a Chunk,
    remaining: &'a mut u64,
    depth: usize,
    roots: Vec<Value>,
}

impl<'a> CallContext<'a> {
    fn new(chunk: &'a Chunk, remaining: &'a mut u64, depth: usize, roots: Vec<Value>) -> Self {
        Self {
            chunk,
            remaining,
            depth,
            roots,
        }
    }
}

struct Frame<'a> {
    prototype: &'a Prototype,
    constants: Vec<Value>,
    registers: Vec<Value>,
    varargs: Vec<Value>,
    closure: Option<ClosureId>,
    open_upvalues: HashMap<u8, UpvalueId>,
    open_upvalues_dirty: bool,
    pc: usize,
    top: usize,
}

impl<'a> Frame<'a> {
    fn new(
        prototype: &'a Prototype,
        constants: Vec<Value>,
        closure: Option<ClosureId>,
        arguments: &[Value],
    ) -> Self {
        let mut registers = vec![Value::Nil; usize::from(prototype.max_stack_size)];
        let parameter_count = usize::from(prototype.parameter_count);
        let copied = arguments.len().min(parameter_count).min(registers.len());
        registers[..copied].clone_from_slice(&arguments[..copied]);
        Self {
            prototype,
            constants,
            registers,
            varargs: arguments
                .get(parameter_count..)
                .unwrap_or_default()
                .to_vec(),
            closure,
            open_upvalues: HashMap::new(),
            open_upvalues_dirty: false,
            pc: 0,
            top: copied,
        }
    }

    fn instruction(&self) -> Result<Instruction, RuntimeError> {
        self.prototype
            .instructions
            .binary_search_by_key(&self.pc, |instruction| instruction.pc())
            .ok()
            .map(|index| self.prototype.instructions[index])
            .ok_or(RuntimeError::InvalidProgramCounter {
                pc: self.pc,
                code_words: self.prototype.code.len(),
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
        if encoded_count == 0 {
            self.ensure_dynamic(usize::from(register) + count)?;
        }
        for offset in 0..count {
            let target = u8::try_from(usize::from(register) + offset).map_err(|_| {
                RuntimeError::Register {
                    register: usize::from(register) + offset,
                    count: self.registers.len(),
                }
            })?;
            self.set(target, results.get(offset).cloned().unwrap_or(Value::Nil))?;
        }
        if encoded_count == 0 {
            self.top = usize::from(register) + count;
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
            self.registers.resize(required, Value::Nil);
        }
        Ok(())
    }

    fn upvalue(&self, heap: &Heap, index: u8) -> Result<UpvalueId, RuntimeError> {
        let closure = self.closure.ok_or(RuntimeError::MissingClosure)?;
        let (_, upvalues) = heap.closure_parts(closure)?;
        upvalues
            .get(index as usize)
            .copied()
            .ok_or(RuntimeError::Upvalue {
                upvalue: index as usize,
                count: upvalues.len(),
            })
    }

    fn capture_ref(&mut self, heap: &mut Heap, register: u8) -> Result<UpvalueId, RuntimeError> {
        if let Some(upvalue) = self.open_upvalues.get(&register) {
            return Ok(*upvalue);
        }
        let upvalue = heap.allocate_upvalue(self.get(register)?.clone());
        self.open_upvalues.insert(register, upvalue);
        Ok(upvalue)
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

    fn gc_roots(&self, heap: &Heap) -> Result<Vec<Value>, RuntimeError> {
        let mut roots = Vec::with_capacity(
            self.constants.len()
                + self.registers.len()
                + self.varargs.len()
                + self.open_upvalues.len()
                + usize::from(self.closure.is_some()),
        );
        roots.extend(self.constants.iter().cloned());
        roots.extend(self.registers.iter().cloned());
        roots.extend(self.varargs.iter().cloned());
        if let Some(closure) = self.closure {
            roots.push(Value::Closure(closure));
        }
        for upvalue in self.open_upvalues.values() {
            roots.push(heap.upvalue_get(*upvalue)?);
        }
        Ok(roots)
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
        let valid = target.is_some_and(|target| {
            self.prototype
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

fn materialize_constants(chunk: &Chunk, prototype: &Prototype) -> Result<Vec<Value>, RuntimeError> {
    prototype
        .constants
        .iter()
        .enumerate()
        .map(|(index, constant)| match constant {
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
        })
        .collect()
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

#[derive(Clone, Debug, PartialEq)]
pub enum RuntimeError {
    DialectNotImplemented(Dialect),
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
    Type {
        operation: &'static str,
        expected: &'static str,
        actual: &'static str,
    },
    NativeFunction(u32),
    Argument {
        function: &'static str,
        index: usize,
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
    MetatableProtected,
    MetatableLoop,
    UnsupportedMetamethod {
        name: &'static str,
        actual: &'static str,
    },
    Raised(Value),
    SelectIndex(i64),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DialectNotImplemented(dialect) => {
                write!(f, "{dialect:?} execution is not implemented")
            }
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
            Self::Type {
                operation,
                expected,
                actual,
            } => write!(f, "{operation} expected {expected}, received {actual}"),
            Self::NativeFunction(index) => write!(f, "invalid native function {index}"),
            Self::Argument { function, index } => {
                write!(f, "{function} requires argument {index}")
            }
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
            Self::Heap(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blu_bytecode::{LoadLimits, load};

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
        assert_eq!(
            vm.execute(&chunk),
            Err(RuntimeError::Constant {
                constant: 99,
                count: 1,
            })
        );
    }

    #[test]
    fn globals_and_multi_part_imports_follow_table_paths() {
        let code = vec![
            ad(Opcode::LoadN, 0, 17),
            abc(Opcode::SetGlobal, 0, 0, 0),
            0,
            abc(Opcode::GetGlobal, 1, 0, 0),
            0,
            abc(Opcode::GetImport, 2, 0, 0),
            (2 << 30) | (1 << 20) | (2 << 10),
            abc(Opcode::Return, 1, 3, 0),
        ];
        let chunk = test_chunk(
            &[b"answer", b"string", b"sub"],
            vec![
                Constant::String(0),
                Constant::String(1),
                Constant::String(2),
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
            vm.collect(std::iter::empty::<&Value>());
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
        let garbage = vm.heap.allocate_table(0, 0);
        assert_eq!(
            vm.collect(std::iter::empty::<&Value>()),
            crate::CollectionStats {
                before: 2,
                retained: 1,
                collected: 1,
            }
        );
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
}
