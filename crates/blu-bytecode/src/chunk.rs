// Ported from Luau VM/src/lvmload.cpp and Bytecode/src/BytecodeBuilder.cpp at
// the pinned revision. Luau is copyright Roblox Corporation and Lua.org,
// PUC-Rio, MIT licensed.

use crate::{
    BYTECODE_VERSION_MAX, BYTECODE_VERSION_MIN, DecodeError, Instruction, TYPEINFO_VERSION_MAX,
    TYPEINFO_VERSION_MIN, ValidationError, decode, validate,
};
use core::fmt;

const PROTO_FLAG_INLINABLE: u8 = 1 << 3;
const FEEDBACK_CALL_TARGET: u8 = 0;

#[derive(Clone, Copy, Debug)]
pub struct LoadLimits {
    pub max_bytes: usize,
    pub max_strings: usize,
    pub max_string_bytes: usize,
    pub max_prototypes: usize,
    pub max_code_words: usize,
    pub max_constants: usize,
    pub max_debug_entries: usize,
}

impl Default for LoadLimits {
    fn default() -> Self {
        Self {
            max_bytes: 64 * 1024 * 1024,
            max_strings: 1_000_000,
            max_string_bytes: 32 * 1024 * 1024,
            max_prototypes: 100_000,
            max_code_words: 8_000_000,
            max_constants: 8_000_000,
            max_debug_entries: 8_000_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chunk {
    pub version: u8,
    pub typeinfo_version: u8,
    pub strings: Vec<Vec<u8>>,
    pub userdata_types: Vec<(u8, usize)>,
    pub prototypes: Vec<Prototype>,
    pub main: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Prototype {
    pub max_stack_size: u8,
    pub parameter_count: u8,
    pub upvalue_count: u8,
    pub is_vararg: bool,
    pub flags: u8,
    pub typeinfo: Vec<u8>,
    pub code: Vec<u32>,
    pub instructions: Vec<Instruction>,
    pub constants: Vec<Constant>,
    pub children: Vec<usize>,
    pub line_defined: u32,
    pub debug_name: Option<usize>,
    pub line_info: Option<LineInfo>,
    pub debug_info: Option<DebugInfo>,
    pub feedback: Vec<FeedbackSlot>,
    pub cost: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Constant {
    Nil,
    Boolean(bool),
    Number(f64),
    Integer(i64),
    Vector([f32; 4]),
    VectorDouble([f64; 4]),
    String(usize),
    Import(u32),
    Table(Vec<usize>),
    TableWithConstants(Vec<(usize, i32)>),
    Closure(usize),
    ClassShape {
        class_name: usize,
        properties: Vec<usize>,
        methods: Vec<usize>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LineInfo {
    pub gap_log2: u8,
    pub deltas: Vec<u8>,
    pub absolute_deltas: Vec<i32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DebugInfo {
    pub locals: Vec<DebugLocal>,
    pub upvalue_names: Vec<Option<usize>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DebugLocal {
    pub name: Option<usize>,
    pub start_pc: u32,
    pub end_pc: u32,
    pub register: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeedbackSlot {
    pub pc: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChunkError {
    TooLarge {
        what: &'static str,
        actual: usize,
        limit: usize,
    },
    UnexpectedEnd {
        offset: usize,
        needed: usize,
        remaining: usize,
    },
    InvalidVarint {
        offset: usize,
    },
    CompileError(String),
    UnsupportedVersion(u8),
    UnsupportedTypeinfoVersion(u8),
    InvalidConstantTag {
        offset: usize,
        tag: u8,
    },
    InvalidStringRef {
        offset: usize,
        reference: u32,
        count: usize,
    },
    InvalidIndex {
        what: &'static str,
        index: usize,
        count: usize,
    },
    InvalidFeedbackType {
        offset: usize,
        kind: u8,
    },
    InvalidPrototypeSize {
        prototype: usize,
        declared_end: usize,
        parsed_end: usize,
    },
    TrailingBytes {
        count: usize,
    },
    Instruction {
        prototype: usize,
        source: DecodeError,
    },
    Validation(ValidationError),
}

impl fmt::Display for ChunkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge {
                what,
                actual,
                limit,
            } => write!(f, "{what} count/size {actual} exceeds limit {limit}"),
            Self::UnexpectedEnd {
                offset,
                needed,
                remaining,
            } => write!(
                f,
                "truncated bytecode at offset {offset}: need {needed} bytes, have {remaining}"
            ),
            Self::InvalidVarint { offset } => write!(f, "invalid varint at offset {offset}"),
            Self::CompileError(error) => write!(f, "upstream compiler error: {error}"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported Luau bytecode version {version}")
            }
            Self::UnsupportedTypeinfoVersion(version) => {
                write!(f, "unsupported Luau typeinfo version {version}")
            }
            Self::InvalidConstantTag { offset, tag } => {
                write!(f, "invalid constant tag {tag} at offset {offset}")
            }
            Self::InvalidStringRef {
                offset,
                reference,
                count,
            } => write!(
                f,
                "invalid string reference {reference} at offset {offset} (table has {count})"
            ),
            Self::InvalidIndex { what, index, count } => {
                write!(f, "invalid {what} index {index} (table has {count})")
            }
            Self::InvalidFeedbackType { offset, kind } => {
                write!(f, "invalid feedback type {kind} at offset {offset}")
            }
            Self::InvalidPrototypeSize {
                prototype,
                declared_end,
                parsed_end,
            } => write!(
                f,
                "prototype {prototype} declared end {declared_end}, parsed through {parsed_end}"
            ),
            Self::TrailingBytes { count } => write!(f, "{count} trailing bytecode bytes"),
            Self::Instruction { prototype, source } => {
                write!(f, "prototype {prototype}: {source}")
            }
            Self::Validation(source) => source.fmt(f),
        }
    }
}

impl std::error::Error for ChunkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Instruction { source, .. } => Some(source),
            Self::Validation(source) => Some(source),
            _ => None,
        }
    }
}

pub fn load(bytes: &[u8], limits: LoadLimits) -> Result<Chunk, ChunkError> {
    ensure_limit("bytecode bytes", bytes.len(), limits.max_bytes)?;
    let mut reader = Reader::new(bytes);
    let version = reader.byte()?;
    if version == 0 {
        let error = String::from_utf8_lossy(reader.rest()).into_owned();
        return Err(ChunkError::CompileError(error));
    }
    if !(BYTECODE_VERSION_MIN..=BYTECODE_VERSION_MAX).contains(&version) {
        return Err(ChunkError::UnsupportedVersion(version));
    }

    let typeinfo_version = if version >= 4 { reader.byte()? } else { 0 };
    if version >= 4 && !(TYPEINFO_VERSION_MIN..=TYPEINFO_VERSION_MAX).contains(&typeinfo_version) {
        return Err(ChunkError::UnsupportedTypeinfoVersion(typeinfo_version));
    }

    let string_count = reader.count("strings", limits.max_strings)?;
    let mut strings = Vec::with_capacity(string_count);
    let mut total_string_bytes = 0usize;
    for _ in 0..string_count {
        let length = reader.count("string bytes", limits.max_string_bytes)?;
        total_string_bytes =
            total_string_bytes
                .checked_add(length)
                .ok_or(ChunkError::TooLarge {
                    what: "total string bytes",
                    actual: usize::MAX,
                    limit: limits.max_string_bytes,
                })?;
        ensure_limit(
            "total string bytes",
            total_string_bytes,
            limits.max_string_bytes,
        )?;
        strings.push(reader.bytes(length)?.to_vec());
    }

    let mut userdata_types = Vec::new();
    if typeinfo_version == 3 {
        loop {
            let index = reader.byte()?;
            if index == 0 {
                break;
            }
            let name = reader
                .string_ref(strings.len())?
                .ok_or(ChunkError::InvalidStringRef {
                    offset: reader.offset,
                    reference: 0,
                    count: strings.len(),
                })?;
            userdata_types.push((index, name));
        }
    }

    let prototype_count = reader.count("prototypes", limits.max_prototypes)?;
    let mut prototypes = Vec::with_capacity(prototype_count);
    for prototype_index in 0..prototype_count {
        let declared_end = if version >= 12 {
            let size = reader.usize_varint()?;
            let start = reader.offset;
            Some(start.checked_add(size).ok_or(ChunkError::TooLarge {
                what: "prototype bytes",
                actual: usize::MAX,
                limit: bytes.len(),
            })?)
        } else {
            None
        };

        let prototype = load_prototype(
            &mut reader,
            version,
            typeinfo_version,
            prototype_index,
            prototype_count,
            strings.len(),
            limits,
        )?;

        if let Some(declared_end) = declared_end {
            if reader.offset > declared_end || declared_end > bytes.len() {
                return Err(ChunkError::InvalidPrototypeSize {
                    prototype: prototype_index,
                    declared_end,
                    parsed_end: reader.offset,
                });
            }
            reader.offset = declared_end;
        }
        prototypes.push(prototype);
    }

    let main = reader.usize_varint()?;
    valid_index("main prototype", main, prototypes.len())?;
    if reader.offset != bytes.len() {
        return Err(ChunkError::TrailingBytes {
            count: bytes.len() - reader.offset,
        });
    }

    let chunk = Chunk {
        version,
        typeinfo_version,
        strings,
        userdata_types,
        prototypes,
        main,
    };
    validate(&chunk).map_err(ChunkError::Validation)?;
    Ok(chunk)
}

fn load_prototype(
    reader: &mut Reader<'_>,
    version: u8,
    _typeinfo_version: u8,
    prototype_index: usize,
    prototype_count: usize,
    string_count: usize,
    limits: LoadLimits,
) -> Result<Prototype, ChunkError> {
    let max_stack_size = reader.byte()?;
    let parameter_count = reader.byte()?;
    let upvalue_count = reader.byte()?;
    let is_vararg = reader.byte()? != 0;
    let flags = if version >= 4 { reader.byte()? } else { 0 };
    let typeinfo = if version >= 4 {
        let size = reader.count("typeinfo bytes", limits.max_bytes)?;
        reader.bytes(size)?.to_vec()
    } else {
        Vec::new()
    };

    let code_count = reader.count("code words", limits.max_code_words)?;
    let mut code = Vec::with_capacity(code_count);
    for _ in 0..code_count {
        code.push(reader.u32()?);
    }
    let instructions = decode(&code).map_err(|source| ChunkError::Instruction {
        prototype: prototype_index,
        source,
    })?;

    let constant_count = reader.count("constants", limits.max_constants)?;
    let mut constants = Vec::with_capacity(constant_count);
    for _ in 0..constant_count {
        constants.push(load_constant(
            reader,
            version,
            prototype_count,
            string_count,
            constant_count,
            limits,
        )?);
    }

    let child_count = reader.count("child prototypes", limits.max_prototypes)?;
    let mut children = Vec::with_capacity(child_count);
    for _ in 0..child_count {
        let child = reader.usize_varint()?;
        valid_index("child prototype", child, prototype_count)?;
        children.push(child);
    }

    let line_defined = reader.u32_varint()?;
    let debug_name = reader.string_ref(string_count)?;
    let line_info = if reader.byte()? != 0 {
        let gap_log2 = reader.byte()?;
        if gap_log2 >= usize::BITS as u8 {
            return Err(ChunkError::InvalidIndex {
                what: "line gap shift",
                index: usize::from(gap_log2),
                count: usize::BITS as usize,
            });
        }
        let deltas = reader.bytes(code_count)?.to_vec();
        let intervals = if code_count == 0 {
            0
        } else {
            ((code_count - 1) >> gap_log2) + 1
        };
        let mut absolute_deltas = Vec::with_capacity(intervals);
        for _ in 0..intervals {
            absolute_deltas.push(reader.i32()?);
        }
        Some(LineInfo {
            gap_log2,
            deltas,
            absolute_deltas,
        })
    } else {
        None
    };

    let debug_info = if reader.byte()? != 0 {
        let local_count = reader.count("debug locals", limits.max_debug_entries)?;
        let mut locals = Vec::with_capacity(local_count);
        for _ in 0..local_count {
            locals.push(DebugLocal {
                name: reader.string_ref(string_count)?,
                start_pc: reader.u32_varint()?,
                end_pc: reader.u32_varint()?,
                register: reader.byte()?,
            });
        }
        let named_upvalues = reader.count("debug upvalues", limits.max_debug_entries)?;
        if named_upvalues != usize::from(upvalue_count) {
            return Err(ChunkError::InvalidIndex {
                what: "debug upvalue count",
                index: named_upvalues,
                count: usize::from(upvalue_count),
            });
        }
        let mut upvalue_names = Vec::with_capacity(named_upvalues);
        for _ in 0..named_upvalues {
            upvalue_names.push(reader.string_ref(string_count)?);
        }
        Some(DebugInfo {
            locals,
            upvalue_names,
        })
    } else {
        None
    };

    let feedback = if version >= 11 {
        let count = reader.count("feedback slots", limits.max_code_words)?;
        let mut feedback = Vec::with_capacity(count);
        for _ in 0..count {
            let offset = reader.offset;
            let kind = reader.byte()?;
            if kind != FEEDBACK_CALL_TARGET {
                return Err(ChunkError::InvalidFeedbackType { offset, kind });
            }
            feedback.push(FeedbackSlot {
                pc: reader.u32_varint()?,
            });
        }
        feedback
    } else {
        Vec::new()
    };

    let cost = if version >= 12 && flags & PROTO_FLAG_INLINABLE != 0 {
        Some(reader.u64_varint()?)
    } else {
        None
    };

    Ok(Prototype {
        max_stack_size,
        parameter_count,
        upvalue_count,
        is_vararg,
        flags,
        typeinfo,
        code,
        instructions,
        constants,
        children,
        line_defined,
        debug_name,
        line_info,
        debug_info,
        feedback,
        cost,
    })
}

fn load_constant(
    reader: &mut Reader<'_>,
    version: u8,
    prototype_count: usize,
    string_count: usize,
    constant_count: usize,
    limits: LoadLimits,
) -> Result<Constant, ChunkError> {
    let offset = reader.offset;
    let tag = reader.byte()?;
    match tag {
        0 => Ok(Constant::Nil),
        1 => Ok(Constant::Boolean(reader.byte()? != 0)),
        2 => Ok(Constant::Number(reader.f64()?)),
        3 => Ok(Constant::String(reader.string_ref(string_count)?.ok_or(
            ChunkError::InvalidStringRef {
                offset,
                reference: 0,
                count: string_count,
            },
        )?)),
        4 => Ok(Constant::Import(reader.u32()?)),
        5 => {
            let count = reader.count("table keys", limits.max_constants)?;
            let mut keys = Vec::with_capacity(count);
            for _ in 0..count {
                let key = reader.usize_varint()?;
                valid_index("table key constant", key, constant_count)?;
                keys.push(key);
            }
            Ok(Constant::Table(keys))
        }
        6 => {
            let prototype = reader.usize_varint()?;
            valid_index("closure prototype", prototype, prototype_count)?;
            Ok(Constant::Closure(prototype))
        }
        7 => Ok(Constant::Vector([
            reader.f32()?,
            reader.f32()?,
            reader.f32()?,
            reader.f32()?,
        ])),
        8 if version >= 7 => {
            let count = reader.count("constant table keys", limits.max_constants)?;
            let mut entries = Vec::with_capacity(count);
            for _ in 0..count {
                let key = reader.usize_varint()?;
                valid_index("table key constant", key, constant_count)?;
                let value = reader.i32()?;
                if value >= 0 {
                    valid_index("table value constant", value as usize, constant_count)?;
                }
                entries.push((key, value));
            }
            Ok(Constant::TableWithConstants(entries))
        }
        9 if version >= 8 => {
            let negative = reader.byte()? != 0;
            let magnitude = reader.u64_varint()?;
            let value = if negative {
                if magnitude == 1_u64 << 63 {
                    i64::MIN
                } else {
                    -i64::try_from(magnitude).map_err(|_| ChunkError::InvalidVarint { offset })?
                }
            } else {
                i64::try_from(magnitude).map_err(|_| ChunkError::InvalidVarint { offset })?
            };
            Ok(Constant::Integer(value))
        }
        10 if version >= 10 => {
            let class_name = reader.usize_varint()?;
            valid_index("class name constant", class_name, constant_count)?;
            let property_count = reader.count("class properties", limits.max_constants)?;
            let method_count = reader.count("class methods", limits.max_constants)?;
            let mut properties = Vec::with_capacity(property_count);
            let mut methods = Vec::with_capacity(method_count);
            for _ in 0..property_count {
                let value = reader.usize_varint()?;
                valid_index("class property constant", value, constant_count)?;
                properties.push(value);
            }
            for _ in 0..method_count {
                let value = reader.usize_varint()?;
                valid_index("class method constant", value, constant_count)?;
                methods.push(value);
            }
            Ok(Constant::ClassShape {
                class_name,
                properties,
                methods,
            })
        }
        11 if version >= 12 => Ok(Constant::VectorDouble([
            reader.f64()?,
            reader.f64()?,
            reader.f64()?,
            reader.f64()?,
        ])),
        _ => Err(ChunkError::InvalidConstantTag { offset, tag }),
    }
}

fn ensure_limit(what: &'static str, actual: usize, limit: usize) -> Result<(), ChunkError> {
    if actual > limit {
        Err(ChunkError::TooLarge {
            what,
            actual,
            limit,
        })
    } else {
        Ok(())
    }
}

fn valid_index(what: &'static str, index: usize, count: usize) -> Result<(), ChunkError> {
    if index < count {
        Ok(())
    } else {
        Err(ChunkError::InvalidIndex { what, index, count })
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn rest(&mut self) -> &'a [u8] {
        let rest = &self.bytes[self.offset..];
        self.offset = self.bytes.len();
        rest
    }

    fn bytes(&mut self, count: usize) -> Result<&'a [u8], ChunkError> {
        let remaining = self.bytes.len().saturating_sub(self.offset);
        if count > remaining {
            return Err(ChunkError::UnexpectedEnd {
                offset: self.offset,
                needed: count,
                remaining,
            });
        }
        let start = self.offset;
        self.offset += count;
        Ok(&self.bytes[start..start + count])
    }

    fn byte(&mut self) -> Result<u8, ChunkError> {
        Ok(self.bytes(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, ChunkError> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }

    fn i32(&mut self) -> Result<i32, ChunkError> {
        Ok(i32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }

    fn f32(&mut self) -> Result<f32, ChunkError> {
        Ok(f32::from_bits(self.u32()?))
    }

    fn f64(&mut self) -> Result<f64, ChunkError> {
        Ok(f64::from_bits(u64::from_le_bytes(
            self.bytes(8)?.try_into().unwrap(),
        )))
    }

    fn usize_varint(&mut self) -> Result<usize, ChunkError> {
        usize::try_from(self.u64_varint()?).map_err(|_| ChunkError::InvalidVarint {
            offset: self.offset,
        })
    }

    fn u32_varint(&mut self) -> Result<u32, ChunkError> {
        u32::try_from(self.u64_varint()?).map_err(|_| ChunkError::InvalidVarint {
            offset: self.offset,
        })
    }

    fn u64_varint(&mut self) -> Result<u64, ChunkError> {
        let start = self.offset;
        let mut result = 0u64;
        for shift in (0..=63).step_by(7) {
            let byte = self.byte()?;
            let payload = u64::from(byte & 0x7f);
            if shift == 63 && payload > 1 {
                return Err(ChunkError::InvalidVarint { offset: start });
            }
            result |= payload << shift;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
        }
        Err(ChunkError::InvalidVarint { offset: start })
    }

    fn count(&mut self, what: &'static str, limit: usize) -> Result<usize, ChunkError> {
        let count = self.usize_varint()?;
        ensure_limit(what, count, limit)?;
        Ok(count)
    }

    fn string_ref(&mut self, count: usize) -> Result<Option<usize>, ChunkError> {
        let offset = self.offset;
        let reference = self.u32_varint()?;
        if reference == 0 {
            return Ok(None);
        }
        let index = (reference - 1) as usize;
        if index >= count {
            return Err(ChunkError::InvalidStringRef {
                offset,
                reference,
                count,
            });
        }
        Ok(Some(index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Opcode;

    // `return 1 + 2`, compiled by pinned luau-compile with default flags.
    const RETURN_THREE_V12: &[u8] = &[
        0x0c, 0x03, 0x00, 0x00, 0x01, 0x23, 0x01, 0x00, 0x00, 0x01, 0x0a, 0x00, 0x03, 0x41, 0x00,
        0x00, 0x00, 0x04, 0x00, 0x03, 0x00, 0x16, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01,
        0x18, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn loads_real_pinned_upstream_v12_chunk() {
        let chunk = load(RETURN_THREE_V12, LoadLimits::default()).unwrap();
        assert_eq!(chunk.version, 12);
        assert_eq!(chunk.typeinfo_version, 3);
        assert_eq!(chunk.main, 0);
        assert_eq!(chunk.prototypes.len(), 1);
        let main = &chunk.prototypes[0];
        assert_eq!(main.max_stack_size, 1);
        assert!(main.is_vararg);
        assert_eq!(main.instructions.len(), 3);
        assert_eq!(main.instructions[0].opcode(), Opcode::PrepVarargs);
        assert_eq!(main.instructions[1].opcode(), Opcode::LoadN);
        assert_eq!(main.instructions[1].d(), 3);
        assert_eq!(main.instructions[2].opcode(), Opcode::Return);
        assert_eq!(main.cost, Some(0));
    }

    #[test]
    fn rejects_truncation_at_every_byte_boundary_without_panicking() {
        for end in 0..RETURN_THREE_V12.len() {
            assert!(load(&RETURN_THREE_V12[..end], LoadLimits::default()).is_err());
        }
    }

    #[test]
    fn reports_compiler_error_payload() {
        assert_eq!(
            load(b"\0syntax error", LoadLimits::default()),
            Err(ChunkError::CompileError("syntax error".into()))
        );
    }

    #[test]
    fn enforces_input_limit_before_parsing() {
        let limits = LoadLimits {
            max_bytes: RETURN_THREE_V12.len() - 1,
            ..LoadLimits::default()
        };
        assert!(matches!(
            load(RETURN_THREE_V12, limits),
            Err(ChunkError::TooLarge {
                what: "bytecode bytes",
                ..
            })
        ));
    }
}
