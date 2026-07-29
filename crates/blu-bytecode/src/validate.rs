// Ported from Luau Bytecode/src/BytecodeBuilder.cpp at the pinned revision.
// Luau is copyright Roblox Corporation and Lua.org, PUC-Rio, MIT licensed.

use crate::{Chunk, Constant, Instruction, MAX_TABLE_INITIAL_CAPACITY, Opcode, Prototype};
use core::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationError {
    pub prototype: usize,
    pub pc: Option<usize>,
    pub message: String,
    pub allocation: Option<ValidationAllocation>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ValidationAllocation {
    pub what: &'static str,
    pub requested: usize,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(allocation) = self.allocation {
            return write!(
                f,
                "prototype {}: failed to allocate {} entries for {}",
                self.prototype, allocation.requested, allocation.what
            );
        }
        if let Some(pc) = self.pc {
            write!(
                f,
                "prototype {}, word {pc}: {}",
                self.prototype, self.message
            )
        } else {
            write!(f, "prototype {}: {}", self.prototype, self.message)
        }
    }
}

impl std::error::Error for ValidationError {}

pub fn validate(chunk: &Chunk) -> Result<(), ValidationError> {
    validate_chunk_structure(chunk)?;
    for (prototype_index, prototype) in chunk.prototypes.iter().enumerate() {
        Validator {
            chunk,
            prototype,
            prototype_index,
        }
        .validate()?;
    }
    Ok(())
}

fn validate_chunk_structure(chunk: &Chunk) -> Result<(), ValidationError> {
    let error = |prototype: usize, message: String| ValidationError {
        prototype,
        pc: None,
        message,
        allocation: None,
    };
    if chunk.main >= chunk.prototypes.len() {
        return Err(error(
            0,
            format!(
                "main prototype {} exceeds prototype count {}",
                chunk.main,
                chunk.prototypes.len()
            ),
        ));
    }
    for (_, string) in &chunk.userdata_types {
        if *string >= chunk.strings.len() {
            return Err(error(
                chunk.main,
                format!(
                    "userdata type string {string} exceeds string count {}",
                    chunk.strings.len()
                ),
            ));
        }
    }
    for (prototype_index, prototype) in chunk.prototypes.iter().enumerate() {
        let check_string = |index: usize, what: &str| {
            if index < chunk.strings.len() {
                Ok(())
            } else {
                Err(error(
                    prototype_index,
                    format!(
                        "{what} string {index} exceeds string count {}",
                        chunk.strings.len()
                    ),
                ))
            }
        };
        let check_prototype = |index: usize, what: &str| {
            if index < chunk.prototypes.len() {
                Ok(())
            } else {
                Err(error(
                    prototype_index,
                    format!(
                        "{what} prototype {index} exceeds prototype count {}",
                        chunk.prototypes.len()
                    ),
                ))
            }
        };
        if let Some(name) = prototype.debug_name {
            check_string(name, "debug name")?;
        }
        for child in &prototype.children {
            check_prototype(*child, "child")?;
        }
        if let Some(debug) = &prototype.debug_info {
            for local in &debug.locals {
                if let Some(name) = local.name {
                    check_string(name, "debug local")?;
                }
            }
            for name in debug.upvalue_names.iter().flatten() {
                check_string(*name, "upvalue")?;
            }
        }
        for constant in &prototype.constants {
            match constant {
                Constant::String(string) => check_string(*string, "constant")?,
                Constant::Closure(child) => check_prototype(*child, "closure constant")?,
                Constant::Table(keys) => {
                    for key in keys {
                        check_constant(prototype_index, *key, prototype.constants.len(), "table")?;
                    }
                }
                Constant::TableWithConstants(entries) => {
                    for (key, value) in entries {
                        check_constant(prototype_index, *key, prototype.constants.len(), "table")?;
                        if let Ok(value) = usize::try_from(*value) {
                            check_constant(
                                prototype_index,
                                value,
                                prototype.constants.len(),
                                "table value",
                            )?;
                        }
                    }
                }
                Constant::ClassShape {
                    class_name,
                    properties,
                    methods,
                } => {
                    check_string(*class_name, "class")?;
                    for property in properties {
                        check_string(*property, "class property")?;
                    }
                    for method in methods {
                        check_string(*method, "class method")?;
                    }
                }
                Constant::Nil
                | Constant::Boolean(_)
                | Constant::Number(_)
                | Constant::Integer(_)
                | Constant::Vector(_)
                | Constant::VectorDouble(_)
                | Constant::Import(_) => {}
            }
        }
    }
    Ok(())
}

fn check_constant(
    prototype: usize,
    index: usize,
    count: usize,
    what: &str,
) -> Result<(), ValidationError> {
    if index < count {
        Ok(())
    } else {
        Err(ValidationError {
            prototype,
            pc: None,
            message: format!("{what} constant {index} exceeds constant count {count}"),
            allocation: None,
        })
    }
}

struct Validator<'a> {
    chunk: &'a Chunk,
    prototype: &'a Prototype,
    prototype_index: usize,
}

impl Validator<'_> {
    fn validate(&self) -> Result<(), ValidationError> {
        let decoded = crate::decode(&self.prototype.code).map_err(|error| {
            self.make_error(None, format!("instruction decoding failed: {error}"))
        })?;
        if decoded != self.prototype.instructions {
            return self.error(
                None,
                "cached instructions do not match prototype code".into(),
            );
        }
        if self.prototype.parameter_count > self.prototype.max_stack_size {
            return self.error(
                None,
                format!(
                    "parameter count {} exceeds stack size {}",
                    self.prototype.parameter_count, self.prototype.max_stack_size
                ),
            );
        }
        let mut starts = self.bool_scratch("instruction-start map")?;
        for instruction in &self.prototype.instructions {
            starts[instruction.pc()] = true;
        }

        let mut open_captures = Vec::new();
        open_captures
            .try_reserve_exact(self.prototype.instructions.len())
            .map_err(|_| {
                self.allocation_error("open-capture stack", self.prototype.instructions.len())
            })?;
        for (index, instruction) in self.prototype.instructions.iter().copied().enumerate() {
            self.validate_instruction(index, instruction, &starts, &mut open_captures)?;
        }
        if !open_captures.is_empty() {
            return self.error(
                None,
                "captured register remains open without CLOSEUPVALS".into(),
            );
        }
        self.validate_variadic()?;
        self.validate_debug_info(&starts)
    }

    fn validate_variadic(&self) -> Result<(), ValidationError> {
        use Opcode::*;

        let mut targets = self.bool_scratch("variadic jump-target map")?;
        for instruction in &self.prototype.instructions {
            if !instruction.opcode().is_fast_call()
                && let Some(target) = instruction.jump_target()
                && target < targets.len()
            {
                targets[target] = true;
            }
        }

        let mut sequence = false;
        for instruction in &self.prototype.instructions {
            let pc = instruction.pc();
            if sequence && targets[pc] {
                return self.error(
                    Some(pc),
                    "jump target occurs inside a variadic result sequence".into(),
                );
            }

            match instruction.opcode() {
                Call | CallFb => {
                    if instruction.b() == 0 {
                        if !sequence {
                            return self.error(
                                Some(pc),
                                "variadic CALL arguments have no producer".into(),
                            );
                        }
                        sequence = false;
                    } else if sequence {
                        return self.error(
                            Some(pc),
                            "fixed-argument CALL interrupts a variadic sequence".into(),
                        );
                    }

                    if instruction.c() == 0 {
                        if sequence {
                            return self
                                .error(Some(pc), "CALL starts a nested variadic sequence".into());
                        }
                        sequence = true;
                    }
                }
                GetVarargs if instruction.b() == 0 => {
                    if sequence {
                        return self.error(
                            Some(pc),
                            "GETVARARGS starts a nested variadic sequence".into(),
                        );
                    }
                    sequence = true;
                }
                Return if instruction.b() == 0 => {
                    if !sequence {
                        return self
                            .error(Some(pc), "variadic RETURN has no result producer".into());
                    }
                    sequence = false;
                }
                SetList if instruction.c() == 0 => {
                    if !sequence {
                        return self
                            .error(Some(pc), "variadic SETLIST has no result producer".into());
                    }
                    sequence = false;
                }
                FastCall => {
                    let call_pc = pc + 1 + instruction.c() as usize;
                    let call = self
                        .prototype
                        .instructions
                        .iter()
                        .find(|candidate| candidate.pc() == call_pc)
                        .expect("FASTCALL target was validated");
                    if call.b() == 0 && !sequence {
                        return self.error(
                            Some(pc),
                            "variadic FASTCALL arguments have no producer".into(),
                        );
                    }
                    if call.b() != 0 && sequence {
                        return self.error(
                            Some(pc),
                            "fixed-argument FASTCALL interrupts a variadic sequence".into(),
                        );
                    }
                }
                CloseUpvals | NameCall | GetImport | Move | GetUpval | GetGlobal | GetTableKs
                | Coverage => {}
                _ if sequence => {
                    return self.error(
                        Some(pc),
                        format!(
                            "{} is not neutral inside a variadic sequence",
                            instruction.opcode()
                        ),
                    );
                }
                _ => {}
            }
        }

        if sequence {
            self.error(None, "unterminated variadic result sequence".into())
        } else {
            Ok(())
        }
    }

    fn validate_instruction(
        &self,
        index: usize,
        instruction: Instruction,
        starts: &[bool],
        open_captures: &mut Vec<u8>,
    ) -> Result<(), ValidationError> {
        use Opcode::*;

        let pc = instruction.pc();
        match instruction.opcode() {
            Nop | Break | Coverage => {}
            NativeCall => return self.error(Some(pc), "NATIVECALL cannot be serialized".into()),
            LoadNil | LoadN => self.register(pc, instruction.a())?,
            LoadB => {
                self.register(pc, instruction.a())?;
                if instruction.b() > 1 {
                    return self.error(Some(pc), "LOADB value must be 0 or 1".into());
                }
                if instruction.c() != 0 {
                    self.jump(pc, instruction.jump_target(), starts)?;
                }
            }
            LoadK => {
                self.register(pc, instruction.a())?;
                self.constant_i16(pc, instruction.d(), None)?;
            }
            Move => {
                self.register(pc, instruction.a())?;
                self.register(pc, instruction.b())?;
            }
            GetGlobal | SetGlobal => {
                self.register(pc, instruction.a())?;
                self.constant(pc, self.aux(pc, instruction)? as usize, Some("string"))?;
            }
            GetUpval | SetUpval => {
                self.register(pc, instruction.a())?;
                self.upvalue(pc, instruction.b())?;
            }
            CloseUpvals => {
                self.register(pc, instruction.a())?;
                while open_captures
                    .last()
                    .is_some_and(|register| *register >= instruction.a())
                {
                    open_captures.pop();
                }
            }
            GetImport => {
                self.register(pc, instruction.a())?;
                self.constant_i16(pc, instruction.d(), Some("import"))?;
                let import = self.aux(pc, instruction)?;
                let count = import >> 30;
                if count == 0 {
                    return self.error(Some(pc), "GETIMPORT path cannot be empty".into());
                }
                for part in 0..count {
                    let shift = 20 - 10 * part;
                    self.constant(pc, ((import >> shift) & 1023) as usize, Some("string"))?;
                }
            }
            GetTable | SetTable | Add | Sub | Mul | Div | Mod | Pow | And | Or | IDiv => {
                self.register(pc, instruction.a())?;
                self.register(pc, instruction.b())?;
                self.register(pc, instruction.c())?;
            }
            GetTableKs | SetTableKs => {
                self.register(pc, instruction.a())?;
                self.register(pc, instruction.b())?;
                self.constant(pc, self.aux(pc, instruction)? as usize, Some("string"))?;
            }
            GetTableN | SetTableN => {
                self.register(pc, instruction.a())?;
                self.register(pc, instruction.b())?;
            }
            NewClosure => {
                self.register(pc, instruction.a())?;
                let child_index = self.nonnegative_i16(pc, instruction.d(), "child prototype")?;
                let child = *self.prototype.children.get(child_index).ok_or_else(|| {
                    self.make_error(
                        Some(pc),
                        format!(
                            "child prototype index {child_index} exceeds {} children",
                            self.prototype.children.len()
                        ),
                    )
                })?;
                let captures = self.chunk.prototypes[child].upvalue_count;
                self.capture_sequence(index, pc, captures, false)?;
            }
            NameCall | NameCallUdata => {
                self.register(pc, instruction.a())?;
                self.register(pc, instruction.b())?;
                let constant = if instruction.opcode() == NameCall {
                    self.aux(pc, instruction)?
                } else {
                    self.aux(pc, instruction)? & 0xffff
                };
                self.constant(pc, constant as usize, Some("string"))?;
                if instruction.opcode() == NameCall {
                    self.expect_next(index, pc, &[Call, CallFb])?;
                } else {
                    self.expect_next(index, pc, &[Call])?;
                }
            }
            Call | CallFb => {
                self.register(pc, instruction.a())?;
                self.register_range(
                    pc,
                    usize::from(instruction.a()) + 1,
                    instruction.b().saturating_sub(1) as usize,
                )?;
                self.register_range(
                    pc,
                    usize::from(instruction.a()),
                    instruction.c().saturating_sub(1) as usize,
                )?;
            }
            Return => self.register_range(
                pc,
                usize::from(instruction.a()),
                instruction.b().saturating_sub(1) as usize,
            )?,
            Jump | JumpBack | JumpX => self.jump(pc, instruction.jump_target(), starts)?,
            JumpIf | JumpIfNot => {
                self.register(pc, instruction.a())?;
                self.jump(pc, instruction.jump_target(), starts)?;
            }
            JumpIfEq | JumpIfLe | JumpIfLt | JumpIfNotEq | JumpIfNotLe | JumpIfNotLt => {
                self.register(pc, instruction.a())?;
                self.register_u32(pc, self.aux(pc, instruction)?)?;
                self.jump(pc, instruction.jump_target(), starts)?;
            }
            JumpXEqKNil | JumpXEqKB => {
                self.register(pc, instruction.a())?;
                self.jump(pc, instruction.jump_target(), starts)?;
            }
            JumpXEqKN | JumpXEqKS => {
                self.register(pc, instruction.a())?;
                let kind = if instruction.opcode() == JumpXEqKN {
                    "number"
                } else {
                    "string"
                };
                self.constant(
                    pc,
                    (self.aux(pc, instruction)? & 0x00ff_ffff) as usize,
                    Some(kind),
                )?;
                self.jump(pc, instruction.jump_target(), starts)?;
            }
            AddK | SubK | MulK | DivK | ModK | PowK | IDivK => {
                self.register(pc, instruction.a())?;
                self.register(pc, instruction.b())?;
                self.constant(pc, instruction.c() as usize, Some("number"))?;
            }
            SubRk | DivRk => {
                self.register(pc, instruction.a())?;
                self.constant(pc, instruction.b() as usize, Some("number"))?;
                self.register(pc, instruction.c())?;
            }
            AndK | OrK => {
                self.register(pc, instruction.a())?;
                self.register(pc, instruction.b())?;
                self.constant(pc, instruction.c() as usize, None)?;
            }
            Concat => {
                self.register(pc, instruction.a())?;
                self.register(pc, instruction.b())?;
                self.register(pc, instruction.c())?;
                if instruction.b() > instruction.c() {
                    return self.error(Some(pc), "CONCAT register range is reversed".into());
                }
            }
            Not | Minus | Length => {
                self.register(pc, instruction.a())?;
                self.register(pc, instruction.b())?;
            }
            NewTable => {
                self.register(pc, instruction.a())?;
                let array_capacity = self.aux(pc, instruction)? as usize;
                if array_capacity > MAX_TABLE_INITIAL_CAPACITY {
                    return self.error(
                        Some(pc),
                        format!(
                            "NEWTABLE array capacity {array_capacity} exceeds limit \
                             {MAX_TABLE_INITIAL_CAPACITY}"
                        ),
                    );
                }
                let hash_capacity = if instruction.b() == 0 {
                    0
                } else {
                    1usize
                        .checked_shl(u32::from(instruction.b() - 1))
                        .unwrap_or(usize::MAX)
                };
                if hash_capacity > MAX_TABLE_INITIAL_CAPACITY {
                    return self.error(
                        Some(pc),
                        format!(
                            "NEWTABLE hash capacity {hash_capacity} exceeds limit \
                             {MAX_TABLE_INITIAL_CAPACITY}"
                        ),
                    );
                }
            }
            DupTable => {
                self.register(pc, instruction.a())?;
                self.constant_i16(pc, instruction.d(), Some("table"))?;
            }
            SetList => {
                self.register(pc, instruction.a())?;
                self.register_range(
                    pc,
                    instruction.b() as usize,
                    instruction.c().saturating_sub(1) as usize,
                )?;
            }
            ForNPrep | ForNLoop => {
                self.register_offset(pc, instruction.a(), 2)?;
                self.jump(pc, instruction.jump_target(), starts)?;
            }
            ForGPrep => {
                self.register_offset(pc, instruction.a(), 3)?;
                self.jump(pc, instruction.jump_target(), starts)?;
            }
            ForGLoop => {
                let variables = self.aux(pc, instruction)? as u8;
                if variables == 0 {
                    return self.error(Some(pc), "FORGLOOP requires a result variable".into());
                }
                self.register_offset(pc, instruction.a(), 2 + variables as usize)?;
                self.jump(pc, instruction.jump_target(), starts)?;
            }
            ForGPrepInext | ForGPrepNext => {
                self.register_offset(pc, instruction.a(), 4)?;
                self.jump(pc, instruction.jump_target(), starts)?;
            }
            GetVarargs => self.register_range(
                pc,
                instruction.a() as usize,
                instruction.b().saturating_sub(1) as usize,
            )?,
            DupClosure => {
                self.register(pc, instruction.a())?;
                let constant = self.constant_i16(pc, instruction.d(), Some("closure"))?;
                let child = match self.prototype.constants[constant] {
                    Constant::Closure(child) => child,
                    _ => unreachable!("constant kind was checked"),
                };
                let captures = self.chunk.prototypes[child].upvalue_count;
                self.capture_sequence(index, pc, captures, true)?;
            }
            PrepVarargs => {
                if !self.prototype.is_vararg {
                    return self.error(Some(pc), "PREPVARARGS in non-variadic function".into());
                }
                if instruction.a() != self.prototype.parameter_count {
                    return self.error(
                        Some(pc),
                        format!(
                            "PREPVARARGS has {} fixed parameters, expected {}",
                            instruction.a(),
                            self.prototype.parameter_count
                        ),
                    );
                }
            }
            LoadKx => {
                self.register(pc, instruction.a())?;
                self.constant(pc, self.aux(pc, instruction)? as usize, None)?;
            }
            FastCall | FastCall1 | FastCall2 | FastCall2K | FastCall3 => {
                if instruction.opcode() != FastCall {
                    self.register(pc, instruction.b())?;
                }
                match instruction.opcode() {
                    FastCall2 => self.register_u32(pc, self.aux(pc, instruction)?)?,
                    FastCall2K => {
                        self.constant(pc, self.aux(pc, instruction)? as usize, None)?;
                    }
                    FastCall3 => {
                        let aux = self.aux(pc, instruction)?;
                        self.register(pc, aux as u8)?;
                        self.register(pc, (aux >> 8) as u8)?;
                    }
                    _ => {}
                }
                let call_pc = pc
                    .checked_add(1 + instruction.c() as usize)
                    .ok_or_else(|| self.make_error(Some(pc), "FASTCALL target overflow".into()))?;
                let call = self
                    .prototype
                    .instructions
                    .iter()
                    .find(|candidate| candidate.pc() == call_pc);
                if call.map(|instruction| instruction.opcode()) != Some(Call) {
                    return self.error(
                        Some(pc),
                        format!("FASTCALL target word {call_pc} is not CALL"),
                    );
                }
            }
            Capture => match instruction.a() {
                0 => self.register(pc, instruction.b())?,
                1 => {
                    self.register(pc, instruction.b())?;
                    open_captures.push(instruction.b());
                }
                2 => self.upvalue(pc, instruction.b())?,
                kind => {
                    return self.error(Some(pc), format!("unsupported capture type {kind}"));
                }
            },
            GetUdataKs | SetUdataKs => {
                self.register(pc, instruction.a())?;
                self.register(pc, instruction.b())?;
                self.constant(
                    pc,
                    (self.aux(pc, instruction)? & 0xffff) as usize,
                    Some("string"),
                )?;
            }
            NewClassMember => {
                self.register(pc, instruction.a())?;
                if instruction.b() != 0 {
                    return self.error(Some(pc), "NEWCLASSMEMBER B must be zero".into());
                }
                self.register(pc, instruction.c())?;
                self.constant(pc, self.aux(pc, instruction)? as usize, Some("string"))?;
            }
            CmpProto => {
                self.register(pc, instruction.a())?;
                self.jump(pc, instruction.jump_target(), starts)?;
            }
        }
        Ok(())
    }

    fn validate_debug_info(&self, starts: &[bool]) -> Result<(), ValidationError> {
        if let Some(debug) = &self.prototype.debug_info {
            for local in &debug.locals {
                if local.start_pc > local.end_pc
                    || local.end_pc as usize > self.prototype.code.len()
                    || (local.start_pc as usize != self.prototype.code.len()
                        && !starts[local.start_pc as usize])
                    || (local.end_pc as usize != self.prototype.code.len()
                        && !starts[local.end_pc as usize])
                {
                    return self.error(
                        None,
                        format!(
                            "invalid debug local range {}..{}",
                            local.start_pc, local.end_pc
                        ),
                    );
                }
                self.register(local.start_pc as usize, local.register)?;
            }
        }
        for feedback in &self.prototype.feedback {
            let pc = feedback.pc as usize;
            if pc >= starts.len() || !starts[pc] {
                return self.error(None, format!("feedback word {pc} is not an instruction"));
            }
        }
        Ok(())
    }

    fn capture_sequence(
        &self,
        instruction_index: usize,
        pc: usize,
        count: u8,
        value_or_upvalue_only: bool,
    ) -> Result<(), ValidationError> {
        for offset in 1..=count as usize {
            let capture = self
                .prototype
                .instructions
                .get(instruction_index + offset)
                .copied()
                .ok_or_else(|| {
                    self.make_error(Some(pc), format!("missing capture {offset} of {count}"))
                })?;
            if capture.opcode() != Opcode::Capture {
                return self.error(
                    Some(pc),
                    format!(
                        "expected CAPTURE {} of {}, found {}",
                        offset,
                        count,
                        capture.opcode()
                    ),
                );
            }
            if value_or_upvalue_only && !matches!(capture.a(), 0 | 2) {
                return self.error(
                    Some(capture.pc()),
                    "DUPCLOSURE capture must be by value or upvalue".into(),
                );
            }
        }
        Ok(())
    }

    fn expect_next(
        &self,
        instruction_index: usize,
        pc: usize,
        expected: &[Opcode],
    ) -> Result<(), ValidationError> {
        let actual = self
            .prototype
            .instructions
            .get(instruction_index + 1)
            .map(|instruction| instruction.opcode());
        if actual.is_some_and(|opcode| expected.contains(&opcode)) {
            Ok(())
        } else {
            self.error(
                Some(pc),
                format!("expected next opcode {expected:?}, found {actual:?}"),
            )
        }
    }

    fn aux(&self, pc: usize, instruction: Instruction) -> Result<u32, ValidationError> {
        instruction
            .aux()
            .ok_or_else(|| self.make_error(Some(pc), "missing auxiliary word".into()))
    }

    fn register(&self, pc: usize, register: u8) -> Result<(), ValidationError> {
        if register < self.prototype.max_stack_size {
            Ok(())
        } else {
            self.error(
                Some(pc),
                format!(
                    "register {register} exceeds stack size {}",
                    self.prototype.max_stack_size
                ),
            )
        }
    }

    fn register_u32(&self, pc: usize, register: u32) -> Result<(), ValidationError> {
        let register = u8::try_from(register)
            .map_err(|_| self.make_error(Some(pc), format!("register {register} exceeds 255")))?;
        self.register(pc, register)
    }

    fn register_offset(
        &self,
        pc: usize,
        register: u8,
        offset: usize,
    ) -> Result<(), ValidationError> {
        let register = usize::from(register)
            .checked_add(offset)
            .ok_or_else(|| self.make_error(Some(pc), "register overflow".into()))?;
        if register < usize::from(self.prototype.max_stack_size) {
            Ok(())
        } else {
            self.error(
                Some(pc),
                format!(
                    "register {register} exceeds stack size {}",
                    self.prototype.max_stack_size
                ),
            )
        }
    }

    fn register_range(&self, pc: usize, start: usize, count: usize) -> Result<(), ValidationError> {
        let end = start
            .checked_add(count)
            .ok_or_else(|| self.make_error(Some(pc), "register range overflow".into()))?;
        if end <= usize::from(self.prototype.max_stack_size) {
            Ok(())
        } else {
            self.error(
                Some(pc),
                format!(
                    "register range {start}..{end} exceeds stack size {}",
                    self.prototype.max_stack_size
                ),
            )
        }
    }

    fn upvalue(&self, pc: usize, upvalue: u8) -> Result<(), ValidationError> {
        if upvalue < self.prototype.upvalue_count {
            Ok(())
        } else {
            self.error(
                Some(pc),
                format!(
                    "upvalue {upvalue} exceeds upvalue count {}",
                    self.prototype.upvalue_count
                ),
            )
        }
    }

    fn constant_i16(
        &self,
        pc: usize,
        constant: i16,
        kind: Option<&str>,
    ) -> Result<usize, ValidationError> {
        let constant = self.nonnegative_i16(pc, constant, "constant")?;
        self.constant(pc, constant, kind)?;
        Ok(constant)
    }

    fn nonnegative_i16(&self, pc: usize, value: i16, name: &str) -> Result<usize, ValidationError> {
        usize::try_from(value)
            .map_err(|_| self.make_error(Some(pc), format!("{name} index {value} is negative")))
    }

    fn constant(
        &self,
        pc: usize,
        index: usize,
        expected: Option<&str>,
    ) -> Result<(), ValidationError> {
        let Some(constant) = self.prototype.constants.get(index) else {
            return self.error(
                Some(pc),
                format!(
                    "constant {index} exceeds constant count {}",
                    self.prototype.constants.len()
                ),
            );
        };
        let actual = constant_kind(constant);
        if expected.is_none_or(|expected| expected == actual) {
            Ok(())
        } else {
            self.error(
                Some(pc),
                format!(
                    "constant {index} is {actual}, expected {}",
                    expected.unwrap()
                ),
            )
        }
    }

    fn jump(
        &self,
        pc: usize,
        target: Option<usize>,
        starts: &[bool],
    ) -> Result<(), ValidationError> {
        let Some(target) = target else {
            return self.error(Some(pc), "jump target underflow or overflow".into());
        };
        if target < starts.len() && starts[target] {
            Ok(())
        } else {
            self.error(
                Some(pc),
                format!("jump target word {target} is not an instruction"),
            )
        }
    }

    fn make_error(&self, pc: Option<usize>, message: String) -> ValidationError {
        ValidationError {
            prototype: self.prototype_index,
            pc,
            message,
            allocation: None,
        }
    }

    fn allocation_error(&self, what: &'static str, requested: usize) -> ValidationError {
        ValidationError {
            prototype: self.prototype_index,
            pc: None,
            message: String::new(),
            allocation: Some(ValidationAllocation { what, requested }),
        }
    }

    fn bool_scratch(&self, what: &'static str) -> Result<Vec<bool>, ValidationError> {
        try_bool_scratch(self.prototype_index, self.prototype.code.len(), what)
    }
}

fn try_bool_scratch(
    prototype: usize,
    requested: usize,
    what: &'static str,
) -> Result<Vec<bool>, ValidationError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(requested)
        .map_err(|_| ValidationError {
            prototype,
            pc: None,
            message: String::new(),
            allocation: Some(ValidationAllocation { what, requested }),
        })?;
    values.resize(requested, false);
    Ok(values)
}

impl Validator<'_> {
    fn error<T>(&self, pc: Option<usize>, message: String) -> Result<T, ValidationError> {
        Err(self.make_error(pc, message))
    }
}

fn constant_kind(constant: &Constant) -> &'static str {
    match constant {
        Constant::Nil => "nil",
        Constant::Boolean(_) => "boolean",
        Constant::Number(_) => "number",
        Constant::Integer(_) => "integer",
        Constant::Vector(_) => "vector",
        Constant::VectorDouble(_) => "double vector",
        Constant::String(_) => "string",
        Constant::Import(_) => "import",
        Constant::Table(_) | Constant::TableWithConstants(_) => "table",
        Constant::Closure(_) => "closure",
        Constant::ClassShape { .. } => "class shape",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DebugInfo, LineInfo};

    #[test]
    fn validation_scratch_capacity_failure_is_structured() {
        assert_eq!(
            try_bool_scratch(7, usize::MAX, "test scratch"),
            Err(ValidationError {
                prototype: 7,
                pc: None,
                message: String::new(),
                allocation: Some(ValidationAllocation {
                    what: "test scratch",
                    requested: usize::MAX,
                }),
            })
        );
    }

    fn prototype(code: Vec<u32>, max_stack_size: u8) -> Prototype {
        let instructions = crate::decode(&code).unwrap();
        Prototype {
            max_stack_size,
            parameter_count: 0,
            upvalue_count: 0,
            is_vararg: false,
            flags: 0,
            typeinfo: Vec::new(),
            code,
            instructions,
            constants: Vec::new(),
            children: Vec::new(),
            line_defined: 0,
            debug_name: None,
            line_info: None::<LineInfo>,
            debug_info: None::<DebugInfo>,
            feedback: Vec::new(),
            cost: None,
        }
    }

    fn chunk(prototype: Prototype) -> Chunk {
        Chunk {
            version: 12,
            typeinfo_version: 3,
            strings: Vec::new(),
            userdata_types: Vec::new(),
            prototypes: vec![prototype],
            main: 0,
        }
    }

    #[test]
    fn rejects_registers_and_jumps_outside_verified_boundaries() {
        let load_bad_register = u32::from(Opcode::LoadNil as u8) | (1 << 8);
        let error = validate(&chunk(prototype(
            vec![load_bad_register, Opcode::Return as u32],
            1,
        )))
        .unwrap_err();
        assert!(error.message.contains("register 1"));

        let jump_into_aux = u32::from(Opcode::Jump as u8) | (1 << 16);
        let get_global = Opcode::GetGlobal as u32;
        let error = validate(&chunk(prototype(
            vec![jump_into_aux, get_global, 0, Opcode::Return as u32],
            1,
        )))
        .unwrap_err();
        assert!(error.message.contains("not an instruction"));
    }

    #[test]
    fn requires_variadic_producers_and_consumers_to_pair() {
        let get_all_varargs = u32::from(Opcode::GetVarargs as u8);
        let return_none = u32::from(Opcode::Return as u8) | (1 << 16);
        let error = validate(&chunk(prototype(vec![get_all_varargs, return_none], 1))).unwrap_err();
        assert!(error.message.contains("not neutral"));

        let return_all = Opcode::Return as u32;
        let mut valid = prototype(vec![get_all_varargs, return_all], 1);
        valid.is_vararg = true;
        validate(&chunk(valid)).unwrap();
    }

    #[test]
    fn rejects_table_capacities_that_exceed_the_runtime_limit() {
        let new_table = Opcode::NewTable as u32;
        let return_none = u32::from(Opcode::Return as u8) | (1 << 16);
        let oversized_array = u32::try_from(MAX_TABLE_INITIAL_CAPACITY + 1).unwrap();
        let error = validate(&chunk(prototype(
            vec![new_table, oversized_array, return_none],
            1,
        )))
        .unwrap_err();
        assert!(error.message.contains("array capacity"));

        let oversized_hash = new_table | (22 << 16);
        let error =
            validate(&chunk(prototype(vec![oversized_hash, 0, return_none], 1))).unwrap_err();
        assert!(error.message.contains("hash capacity"));

        let maximum_hash = new_table | (21 << 16);
        validate(&chunk(prototype(
            vec![
                maximum_hash,
                u32::try_from(MAX_TABLE_INITIAL_CAPACITY).unwrap(),
                return_none,
            ],
            1,
        )))
        .unwrap();
    }

    #[test]
    fn rejects_stale_or_forged_instruction_caches_without_panicking() {
        let mut stale = prototype(vec![Opcode::Return as u32], 1);
        stale.instructions = crate::decode(&[
            u32::from(Opcode::LoadNil as u8) | (255 << 8),
            Opcode::Return as u32,
        ])
        .unwrap();
        let error = validate(&chunk(stale)).unwrap_err();
        assert!(error.message.contains("do not match"));
    }

    #[test]
    fn rejects_mutated_cross_references_before_instruction_validation() {
        let mut invalid_child = prototype(vec![Opcode::Return as u32], 1);
        invalid_child.children.push(usize::MAX);
        let error = validate(&chunk(invalid_child)).unwrap_err();
        assert!(error.message.contains("child prototype"));

        let mut invalid_string = prototype(vec![Opcode::Return as u32], 1);
        invalid_string.constants.push(Constant::String(0));
        let error = validate(&chunk(invalid_string)).unwrap_err();
        assert!(error.message.contains("constant string"));

        let mut invalid_main = chunk(prototype(vec![Opcode::Return as u32], 1));
        invalid_main.main = 1;
        let error = validate(&invalid_main).unwrap_err();
        assert!(error.message.contains("main prototype"));
    }
}
