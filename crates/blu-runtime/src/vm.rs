use crate::{Dialect, Heap, HeapError, TableId, Value};
use blu_bytecode::{Chunk, Constant, Instruction, Opcode, Prototype};
use core::fmt;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Vm {
    dialect: Dialect,
    instruction_limit: u64,
    heap: Heap,
}

impl Default for Vm {
    fn default() -> Self {
        Self {
            dialect: Dialect::Luau,
            instruction_limit: 10_000_000,
            heap: Heap::default(),
        }
    }
}

impl Vm {
    #[must_use]
    pub fn new(dialect: Dialect) -> Self {
        Self {
            dialect,
            instruction_limit: 10_000_000,
            heap: Heap::default(),
        }
    }

    #[must_use]
    pub const fn with_instruction_limit(mut self, limit: u64) -> Self {
        self.instruction_limit = limit;
        self
    }

    #[must_use]
    pub const fn heap(&self) -> &Heap {
        &self.heap
    }

    pub fn collect<'a>(
        &mut self,
        roots: impl IntoIterator<Item = &'a Value>,
    ) -> crate::CollectionStats {
        self.heap.collect(roots)
    }

    pub fn execute(&mut self, chunk: &Chunk) -> Result<Vec<Value>, RuntimeError> {
        if self.dialect != Dialect::Luau {
            return Err(RuntimeError::DialectNotImplemented(self.dialect));
        }
        let prototype = chunk
            .prototypes
            .get(chunk.main)
            .ok_or(RuntimeError::InvalidMainPrototype(chunk.main))?;
        let constants = materialize_constants(chunk, prototype)?;
        let mut frame = Frame::new(prototype, constants);
        let mut remaining = self.instruction_limit;

        loop {
            if remaining == 0 {
                return Err(RuntimeError::InstructionLimit {
                    limit: self.instruction_limit,
                });
            }
            remaining -= 1;

            let instruction = frame.instruction()?;
            let next_pc = instruction.pc() + usize::from(instruction.opcode().words());
            frame.pc = next_pc;

            match instruction.opcode() {
                Opcode::Nop | Opcode::Coverage | Opcode::PrepVarargs => {}
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
                Opcode::Add
                | Opcode::Sub
                | Opcode::Mul
                | Opcode::Div
                | Opcode::Mod
                | Opcode::Pow
                | Opcode::IDiv => {
                    let left = frame.get(instruction.b())?.clone();
                    let right = frame.get(instruction.c())?.clone();
                    let value = arithmetic(instruction.opcode(), &left, &right)?;
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
                    let value = arithmetic(instruction.opcode(), &left, &right)?;
                    frame.set(instruction.a(), value)?;
                }
                Opcode::SubRk | Opcode::DivRk => {
                    let left = frame.constant_u32(u32::from(instruction.b()))?;
                    let right = frame.get(instruction.c())?.clone();
                    let value = arithmetic(instruction.opcode(), &left, &right)?;
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
                            return Err(RuntimeError::Type {
                                operation: "unary minus",
                                expected: "number",
                                actual: other.type_name(),
                            });
                        }
                    };
                    frame.set(instruction.a(), value)?;
                }
                Opcode::Length => {
                    let value = frame.get(instruction.b())?.clone();
                    let length = match value {
                        Value::String(value) => value.len(),
                        Value::Table(table) => self.heap.table_length(table)?,
                        other => {
                            return Err(RuntimeError::Type {
                                operation: "length",
                                expected: "string or table",
                                actual: other.type_name(),
                            });
                        }
                    };
                    frame.set(instruction.a(), Value::Number(length as f64))?;
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
                Opcode::GetTable => {
                    let table = table_id(frame.get(instruction.b())?)?;
                    let key = frame.get(instruction.c())?.clone();
                    let value = self.heap.table_get(table, &key)?;
                    frame.set(instruction.a(), value)?;
                }
                Opcode::SetTable => {
                    let value = frame.get(instruction.a())?.clone();
                    let table = table_id(frame.get(instruction.b())?)?;
                    let key = frame.get(instruction.c())?.clone();
                    self.heap.table_set(table, key, value)?;
                }
                Opcode::GetTableKs | Opcode::GetUdataKs => {
                    let table = table_id(frame.get(instruction.b())?)?;
                    let index = table_string_constant(instruction)?;
                    let key = frame.constant_u32(index)?;
                    let value = self.heap.table_get(table, &key)?;
                    frame.set(instruction.a(), value)?;
                }
                Opcode::SetTableKs | Opcode::SetUdataKs => {
                    let value = frame.get(instruction.a())?.clone();
                    let table = table_id(frame.get(instruction.b())?)?;
                    let index = table_string_constant(instruction)?;
                    let key = frame.constant_u32(index)?;
                    self.heap.table_set(table, key, value)?;
                }
                Opcode::GetTableN => {
                    let table = table_id(frame.get(instruction.b())?)?;
                    let key = Value::Integer(i64::from(instruction.c()) + 1);
                    let value = self.heap.table_get(table, &key)?;
                    frame.set(instruction.a(), value)?;
                }
                Opcode::SetTableN => {
                    let value = frame.get(instruction.a())?.clone();
                    let table = table_id(frame.get(instruction.b())?)?;
                    let key = Value::Integer(i64::from(instruction.c()) + 1);
                    self.heap.table_set(table, key, value)?;
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
                    let left = frame.get(instruction.a())?;
                    let right = frame.get(right_register)?;
                    if compare(instruction.opcode(), left, right)? {
                        frame.jump(instruction)?;
                    }
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
                    let mut bytes = Vec::new();
                    for register in instruction.b()..=instruction.c() {
                        match frame.get(register)? {
                            Value::String(value) => bytes.extend_from_slice(value),
                            Value::Integer(value) => {
                                bytes.extend_from_slice(value.to_string().as_bytes());
                            }
                            Value::Number(value) => {
                                bytes.extend_from_slice(value.to_string().as_bytes());
                            }
                            other => {
                                return Err(RuntimeError::Type {
                                    operation: "concatenation",
                                    expected: "string or number",
                                    actual: other.type_name(),
                                });
                            }
                        }
                    }
                    frame.set(instruction.a(), Value::String(Arc::from(bytes)))?;
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
}

struct Frame<'a> {
    prototype: &'a Prototype,
    constants: Vec<Value>,
    registers: Vec<Value>,
    pc: usize,
    top: usize,
}

impl<'a> Frame<'a> {
    fn new(prototype: &'a Prototype, constants: Vec<Value>) -> Self {
        Self {
            prototype,
            constants,
            registers: vec![Value::Nil; usize::from(prototype.max_stack_size)],
            pc: 0,
            top: 0,
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
        Ok(())
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
            _ => Err(RuntimeError::UnsupportedConstant { constant: index }),
        })
        .collect()
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

fn compare(opcode: Opcode, left: &Value, right: &Value) -> Result<bool, RuntimeError> {
    let equal = left == right;
    match opcode {
        Opcode::JumpIfEq => Ok(equal),
        Opcode::JumpIfNotEq => Ok(!equal),
        Opcode::JumpIfLe | Opcode::JumpIfLt | Opcode::JumpIfNotLe | Opcode::JumpIfNotLt => {
            let left_number = left.as_number().ok_or(RuntimeError::Type {
                operation: "comparison",
                expected: "number",
                actual: left.type_name(),
            })?;
            let right_number = right.as_number().ok_or(RuntimeError::Type {
                operation: "comparison",
                expected: "number",
                actual: right.type_name(),
            })?;
            Ok(match opcode {
                Opcode::JumpIfLe => left_number <= right_number,
                Opcode::JumpIfLt => left_number < right_number,
                Opcode::JumpIfNotLe => left_number > right_number,
                Opcode::JumpIfNotLt => left_number >= right_number,
                _ => unreachable!(),
            })
        }
        _ => Err(RuntimeError::UnsupportedComparison(opcode)),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RuntimeError {
    DialectNotImplemented(Dialect),
    InvalidMainPrototype(usize),
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
    Heap(HeapError),
    DivideByZero,
    Breakpoint {
        pc: usize,
    },
    InstructionLimit {
        limit: u64,
    },
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DialectNotImplemented(dialect) => {
                write!(f, "{dialect:?} execution is not implemented")
            }
            Self::InvalidMainPrototype(index) => write!(f, "invalid main prototype {index}"),
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
            Self::Heap(error) => error.fmt(f),
            Self::DivideByZero => f.write_str("integer divide by zero"),
            Self::Breakpoint { pc } => write!(f, "breakpoint at word {pc}"),
            Self::InstructionLimit { limit } => {
                write!(f, "instruction limit {limit} exceeded")
            }
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
}
