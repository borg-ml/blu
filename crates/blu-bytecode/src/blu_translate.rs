use crate::{
    BYTECODE_VERSION_TARGET, Chunk, DecodeError, Opcode, Prototype as LuauPrototype,
    TYPEINFO_VERSION_TARGET, ValidatedChunk, ValidationError as LuauValidationError,
    blu::{
        Artifact, BluLimits, Constant as BluConstant, Instruction as BluInstruction,
        ValidatedArtifact, ValidationError as BluValidationError,
    },
    decode,
};
use blu_core::SemanticProfile;
use core::fmt;

/// Translate the deliberately small BluV1 baseline into validated Luau
/// bytecode for the bootstrap VM.
///
/// This adapter is not a general Blu backend. It accepts only a single
/// `blu` or `luau` execution profile and rejects nested prototypes and
/// upvalues, whose profile and capture metadata cannot yet be represented
/// faithfully by the bootstrap chunk model.
pub fn translate_baseline_to_luau(
    artifact: ValidatedArtifact,
    execution_profile: SemanticProfile,
    execution_limits: BluLimits,
) -> Result<TranslatedChunk, TranslationError> {
    if !matches!(
        execution_profile,
        SemanticProfile::Blu | SemanticProfile::Luau
    ) {
        return Err(TranslationError::UnsupportedExecutionProfile(
            execution_profile,
        ));
    }

    // Structural load validation may have used a more permissive policy.
    // Revalidate the immutable payload under the execution policy before
    // translating it into a chunk that the bootstrap VM can authorize.
    let artifact = ValidatedArtifact::new(artifact.into_artifact(), execution_limits)
        .map_err(TranslationError::ExecutionValidation)?
        .into_artifact();
    let Artifact {
        prototypes, main, ..
    } = artifact;

    let mut string_count = 0usize;
    for prototype in &prototypes {
        for constant in &prototype.constants {
            if matches!(constant, BluConstant::String(_)) {
                string_count = string_count
                    .checked_add(1)
                    .ok_or(TranslationError::TooLarge {
                        prototype: None,
                        what: "string count",
                        actual: usize::MAX,
                        limit: usize::MAX - 1,
                    })?;
            }
        }
    }

    let mut strings = Vec::new();
    reserve(&mut strings, string_count, "strings")?;
    let mut translated = Vec::new();
    reserve(&mut translated, prototypes.len(), "prototypes")?;

    for (prototype_index, prototype) in prototypes.into_iter().enumerate() {
        if prototype.profile != execution_profile {
            return Err(TranslationError::ProfileMismatch {
                prototype: prototype_index,
                artifact: prototype.profile,
                execution: execution_profile,
            });
        }
        if !prototype.children.is_empty() {
            return Err(TranslationError::UnsupportedStructure {
                prototype: prototype_index,
                what: "child prototypes",
            });
        }
        if !prototype.upvalues.is_empty() {
            return Err(TranslationError::UnsupportedStructure {
                prototype: prototype_index,
                what: "upvalues",
            });
        }

        let max_stack_size =
            u8::try_from(prototype.register_count).map_err(|_| TranslationError::TooLarge {
                prototype: Some(prototype_index),
                what: "register count",
                actual: usize::from(prototype.register_count),
                limit: usize::from(u8::MAX),
            })?;
        let parameter_count =
            u8::try_from(prototype.parameter_count).map_err(|_| TranslationError::TooLarge {
                prototype: Some(prototype_index),
                what: "parameter count",
                actual: usize::from(prototype.parameter_count),
                limit: usize::from(u8::MAX),
            })?;

        let mut constants = Vec::new();
        reserve(
            &mut constants,
            prototype.constants.len(),
            "prototype constants",
        )?;
        for constant in prototype.constants {
            constants.push(match constant {
                BluConstant::Nil => crate::Constant::Nil,
                BluConstant::Boolean(value) => crate::Constant::Boolean(value),
                BluConstant::Number(value) => crate::Constant::Number(value),
                BluConstant::String(bytes) => {
                    let index = strings.len();
                    strings.push(bytes);
                    crate::Constant::String(index)
                }
            });
        }

        let mut code = Vec::new();
        reserve(&mut code, prototype.code.len(), "prototype code")?;
        for instruction in prototype.code {
            code.push(translate_instruction(prototype_index, instruction)?);
        }
        let instructions = decode(&code).map_err(|source| TranslationError::InstructionDecode {
            prototype: prototype_index,
            source,
        })?;

        translated.push(LuauPrototype {
            max_stack_size,
            parameter_count,
            upvalue_count: 0,
            is_vararg: prototype.is_vararg,
            flags: 0,
            typeinfo: Vec::new(),
            code,
            instructions,
            constants,
            children: Vec::new(),
            line_defined: 0,
            debug_name: None,
            line_info: None,
            debug_info: None,
            feedback: Vec::new(),
            cost: None,
        });
    }

    let chunk = ValidatedChunk::new_profiled(
        Chunk {
            version: BYTECODE_VERSION_TARGET,
            typeinfo_version: TYPEINFO_VERSION_TARGET,
            strings,
            userdata_types: Vec::new(),
            prototypes: translated,
            main: main as usize,
        },
        execution_profile,
    )
    .map_err(TranslationError::Validation)?;
    Ok(TranslatedChunk {
        profile: execution_profile,
        chunk,
    })
}

fn translate_instruction(
    prototype: usize,
    instruction: BluInstruction,
) -> Result<u32, TranslationError> {
    match instruction {
        BluInstruction::LoadConstant {
            destination,
            constant,
        } => {
            let destination = register(prototype, destination)?;
            let constant = i16::try_from(constant).map_err(|_| TranslationError::TooLarge {
                prototype: Some(prototype),
                what: "LOADK constant index",
                actual: constant as usize,
                limit: i16::MAX as usize,
            })?;
            Ok(ad(Opcode::LoadK, destination, constant))
        }
        BluInstruction::Add {
            destination,
            left,
            right,
        } => Ok(abc(
            Opcode::Add,
            register(prototype, destination)?,
            register(prototype, left)?,
            register(prototype, right)?,
        )),
        BluInstruction::Return { first, count } => {
            let result_field = count
                .checked_add(1)
                .and_then(|value| u8::try_from(value).ok())
                .ok_or(TranslationError::TooLarge {
                    prototype: Some(prototype),
                    what: "fixed return count",
                    actual: usize::from(count),
                    limit: usize::from(u8::MAX - 1),
                })?;
            Ok(abc(
                Opcode::Return,
                register(prototype, first)?,
                result_field,
                0,
            ))
        }
    }
}

fn register(prototype: usize, register: u16) -> Result<u8, TranslationError> {
    u8::try_from(register).map_err(|_| TranslationError::TooLarge {
        prototype: Some(prototype),
        what: "register index",
        actual: usize::from(register),
        limit: usize::from(u8::MAX),
    })
}

fn abc(opcode: Opcode, a: u8, b: u8, c: u8) -> u32 {
    u32::from(opcode as u8) | (u32::from(a) << 8) | (u32::from(b) << 16) | (u32::from(c) << 24)
}

fn ad(opcode: Opcode, a: u8, d: i16) -> u32 {
    u32::from(opcode as u8) | (u32::from(a) << 8) | (u32::from(d as u16) << 16)
}

fn reserve<T>(
    values: &mut Vec<T>,
    additional: usize,
    what: &'static str,
) -> Result<(), TranslationError> {
    values
        .try_reserve_exact(additional)
        .map_err(|_| TranslationError::Allocation {
            what,
            requested: additional,
        })
}

/// A translated bootstrap chunk that retains its authorized semantic profile.
///
/// Use the profile-checking runtime entry point for execution. Extracting the
/// inner chunk is provided for low-level tooling and transfers responsibility
/// for preserving the profile to the caller.
#[derive(Debug, PartialEq)]
pub struct TranslatedChunk {
    profile: SemanticProfile,
    chunk: ValidatedChunk,
}

impl TranslatedChunk {
    #[must_use]
    pub const fn profile(&self) -> SemanticProfile {
        self.profile
    }

    #[must_use]
    pub fn into_validated_chunk(self) -> ValidatedChunk {
        self.chunk
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum TranslationError {
    UnsupportedExecutionProfile(SemanticProfile),
    ProfileMismatch {
        prototype: usize,
        artifact: SemanticProfile,
        execution: SemanticProfile,
    },
    UnsupportedStructure {
        prototype: usize,
        what: &'static str,
    },
    TooLarge {
        prototype: Option<usize>,
        what: &'static str,
        actual: usize,
        limit: usize,
    },
    Allocation {
        what: &'static str,
        requested: usize,
    },
    InstructionDecode {
        prototype: usize,
        source: DecodeError,
    },
    ExecutionValidation(BluValidationError),
    Validation(LuauValidationError),
}

impl fmt::Display for TranslationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedExecutionProfile(profile) => {
                write!(
                    formatter,
                    "{profile} cannot execute through the Luau bootstrap"
                )
            }
            Self::ProfileMismatch {
                prototype,
                artifact,
                execution,
            } => write!(
                formatter,
                "prototype {prototype} uses {artifact} semantics but execution selected {execution}"
            ),
            Self::UnsupportedStructure { prototype, what } => write!(
                formatter,
                "prototype {prototype} uses {what}, which the baseline translator cannot preserve"
            ),
            Self::TooLarge {
                prototype,
                what,
                actual,
                limit,
            } => {
                if let Some(prototype) = prototype {
                    write!(
                        formatter,
                        "prototype {prototype} {what} {actual} exceeds bootstrap limit {limit}"
                    )
                } else {
                    write!(formatter, "{what} {actual} exceeds bootstrap limit {limit}")
                }
            }
            Self::Allocation { what, requested } => {
                write!(formatter, "failed to allocate {requested} {what}")
            }
            Self::InstructionDecode { prototype, source } => {
                write!(
                    formatter,
                    "translated prototype {prototype} is invalid: {source}"
                )
            }
            Self::ExecutionValidation(source) => {
                write!(
                    formatter,
                    "BluV1 execution-policy validation failed: {source}"
                )
            }
            Self::Validation(source) => {
                write!(formatter, "translated chunk validation failed: {source}")
            }
        }
    }
}

impl std::error::Error for TranslationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InstructionDecode { source, .. } => Some(source),
            Self::ExecutionValidation(source) => Some(source),
            Self::Validation(source) => Some(source),
            _ => None,
        }
    }
}
