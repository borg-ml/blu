//! The bounded, owned Blu bytecode container.
//!
//! This module intentionally implements only the baseline needed to carry the
//! first compiler slice. Its validated type proves this module's structural
//! policy only. The separate baseline translator revalidates under an
//! execution policy and produces a profile-tagged bootstrap chunk; validation
//! alone is not execution authorization.

use blu_core::{
    ByteOffset, ByteSpan, CompilerId, CompilerIdentity, IdentityLimits, SemanticProfile, SourceId,
    SourceIdentity,
};
use core::{fmt, ops::BitOr};
use std::collections::HashMap;

pub use crate::blu_translate::{TranslatedChunk, TranslationError, translate_baseline_to_luau};

/// Bytes which cannot be confused with a serialized Luau chunk.
pub const MAGIC: [u8; 4] = *b"BLU\0";
pub const BLU_V1_VERSION: u16 = 1;

// BluV1 logical decoded-storage charges. These deliberately do not use Rust
// layout or allocator capacity: every target charges the same conservative
// amount per retained record/element, then adds owned payload bytes exactly.
const DECODED_ARTIFACT_CHARGE: usize = 256;
const DECODED_SOURCE_CHARGE: usize = 96;
const DECODED_PROTOTYPE_CHARGE: usize = 256;
const DECODED_CONSTANT_CHARGE: usize = 32;
const DECODED_INSTRUCTION_CHARGE: usize = 16;
const DECODED_CHILD_CHARGE: usize = 4;
const DECODED_UPVALUE_CHARGE: usize = 4;
const DECODED_SOURCE_MAP_CHARGE: usize = 12;
const DECODED_LOCAL_DEBUG_CHARGE: usize = 40;
const DECODED_UPVALUE_DEBUG_CHARGE: usize = 40;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BytecodeFormat {
    BluV1,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct FeatureBits(u64);

impl FeatureBits {
    /// Straight-line constant loads, numeric addition, and return.
    pub const BASELINE: Self = Self(1);
    pub const SUPPORTED: Self = Self(Self::BASELINE.0);

    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    #[must_use]
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    #[must_use]
    pub const fn bits(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for FeatureBits {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BluLimits {
    pub max_bytes: usize,
    pub max_sources: usize,
    pub max_prototypes: usize,
    pub max_registers_per_prototype: usize,
    pub max_constants_per_prototype: usize,
    pub max_constant_bytes: usize,
    pub max_debug_name_bytes: usize,
    pub max_code_per_prototype: usize,
    pub max_children_per_prototype: usize,
    pub max_upvalues_per_prototype: usize,
    pub max_debug_entries_per_prototype: usize,
    pub max_total_registers: usize,
    pub max_total_constants: usize,
    pub max_total_constant_bytes: usize,
    pub max_total_code: usize,
    pub max_total_children: usize,
    pub max_total_upvalues: usize,
    pub max_total_debug_entries: usize,
    pub max_total_debug_bytes: usize,
    pub max_total_source_map_entries: usize,
    /// BluV1 logical decoded charges plus exact owned payload bytes.
    ///
    /// Charges are versioned constants independent of Rust/target layout.
    pub max_decoded_bytes: usize,
    pub identity: IdentityLimits,
}

impl Default for BluLimits {
    fn default() -> Self {
        Self {
            max_bytes: 64 * 1024 * 1024,
            max_sources: 1024,
            max_prototypes: 100_000,
            max_registers_per_prototype: u16::MAX as usize,
            max_constants_per_prototype: 1_000_000,
            max_constant_bytes: 32 * 1024 * 1024,
            max_debug_name_bytes: 4 * 1024,
            max_code_per_prototype: 8_000_000,
            max_children_per_prototype: 100_000,
            max_upvalues_per_prototype: u16::MAX as usize,
            max_debug_entries_per_prototype: 8_000_000,
            max_total_registers: 8_000_000,
            max_total_constants: 8_000_000,
            max_total_constant_bytes: 32 * 1024 * 1024,
            max_total_code: 8_000_000,
            max_total_children: 100_000,
            max_total_upvalues: 1_000_000,
            max_total_debug_entries: 8_000_000,
            max_total_debug_bytes: 32 * 1024 * 1024,
            max_total_source_map_entries: 8_000_000,
            max_decoded_bytes: 256 * 1024 * 1024,
            identity: IdentityLimits::default(),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct SourceRecord {
    pub identity: SourceIdentity,
    pub byte_len: u32,
    /// Digest bytes are carried opaquely; this layer does not choose a hash.
    pub digest: [u8; 32],
}

/// An owned, unvalidated BluV1 artifact.
///
/// Owning artifact structures intentionally do not implement `Clone`: a deep
/// clone could allocate without a structured failure path. Use canonical
/// encode/decode when a fallible independent copy is required.
#[derive(Debug, PartialEq)]
pub struct Artifact {
    pub format: BytecodeFormat,
    pub compiler: CompilerIdentity,
    pub sources: Vec<SourceRecord>,
    pub prototypes: Vec<Prototype>,
    pub main: u32,
}

#[derive(Debug, PartialEq)]
pub struct Prototype {
    pub profile: SemanticProfile,
    pub source: SourceId,
    pub register_count: u16,
    pub parameter_count: u16,
    pub is_vararg: bool,
    pub required_features: FeatureBits,
    pub constants: Vec<Constant>,
    pub upvalues: Vec<Upvalue>,
    pub children: Vec<u32>,
    pub code: Vec<Instruction>,
    /// Exactly one source span per instruction (PC is the vector index).
    pub source_map: Vec<ByteSpan>,
    pub locals: Vec<LocalDebug>,
    pub upvalue_debug: Vec<UpvalueDebug>,
}

#[derive(Debug)]
pub enum Constant {
    Nil,
    Boolean(bool),
    Number(f64),
    String(Vec<u8>),
}

/// Numeric constants use bitwise IEEE-754 identity.
///
/// Encoding preserves every `f64::to_bits` pattern exactly, including NaN
/// payloads, signed zero, infinities, and subnormals. Distinct bit patterns
/// are distinct BluV1 constants.
impl PartialEq for Constant {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Nil, Self::Nil) => true,
            (Self::Boolean(left), Self::Boolean(right)) => left == right,
            (Self::Number(left), Self::Number(right)) => left.to_bits() == right.to_bits(),
            (Self::String(left), Self::String(right)) => left == right,
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Upvalue {
    ParentRegister(u16),
    ParentUpvalue(u16),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Instruction {
    LoadConstant {
        destination: u16,
        constant: u32,
    },
    Add {
        destination: u16,
        left: u16,
        right: u16,
    },
    Return {
        first: u16,
        count: u16,
    },
}

/// BluV1's versioned profile-by-instruction legality table.
///
/// The current baseline operations have identical legality in all seven
/// established profiles. Future `SemanticProfile` variants default to illegal
/// until BluV1 (or a later format version) assigns them an explicit policy.
#[must_use]
pub const fn instruction_is_legal(profile: SemanticProfile, instruction: Instruction) -> bool {
    match profile {
        SemanticProfile::Blu
        | SemanticProfile::Luau
        | SemanticProfile::Lua51
        | SemanticProfile::Lua52
        | SemanticProfile::Lua53
        | SemanticProfile::Lua54
        | SemanticProfile::Lua55 => match instruction {
            Instruction::LoadConstant { .. }
            | Instruction::Add { .. }
            | Instruction::Return { .. } => true,
        },
        _ => false,
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct LocalDebug {
    pub name: Vec<u8>,
    pub register: u16,
    pub start_pc: u32,
    pub end_pc: u32,
}

#[derive(Debug, Eq, PartialEq)]
pub struct UpvalueDebug {
    pub name: Vec<u8>,
    pub upvalue: u16,
    pub start_pc: u32,
    pub end_pc: u32,
}

/// Proof that the structural BluV1 policy in this module succeeded.
///
/// Fields are deliberately inaccessible, so mutation requires returning to an
/// unvalidated `Artifact` and validating again.
///
/// This is not VM execution authorization and does not claim the future full
/// profile/opcode validator. Any future executor must revalidate under its
/// execution policy before treating this artifact as executable.
///
/// This proof also intentionally does not implement `Clone`, because cloning
/// its owned artifact would introduce an infallible deep-allocation path.
#[derive(Debug, PartialEq)]
pub struct ValidatedArtifact {
    artifact: Artifact,
    policy: BluLimits,
}

impl ValidatedArtifact {
    pub fn new(artifact: Artifact, limits: BluLimits) -> Result<Self, ValidationError> {
        validate(&artifact, limits)?;
        Ok(Self {
            artifact,
            policy: limits,
        })
    }

    #[must_use]
    pub const fn artifact(&self) -> &Artifact {
        &self.artifact
    }

    /// Limits under which this structural validation proof was created.
    #[must_use]
    pub const fn validation_policy(&self) -> BluLimits {
        self.policy
    }

    #[must_use]
    pub fn into_artifact(self) -> Artifact {
        self.artifact
    }

    #[must_use]
    pub fn compiler(&self) -> &CompilerIdentity {
        &self.artifact.compiler
    }

    #[must_use]
    pub fn sources(&self) -> &[SourceRecord] {
        &self.artifact.sources
    }

    #[must_use]
    pub fn prototypes(&self) -> &[Prototype] {
        &self.artifact.prototypes
    }

    #[must_use]
    pub fn main(&self) -> &Prototype {
        &self.artifact.prototypes[self.artifact.main as usize]
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidationError {
    Limit {
        what: &'static str,
        actual: usize,
        limit: usize,
    },
    Unrepresentable {
        what: &'static str,
        length: usize,
    },
    Allocation {
        what: &'static str,
        requested: usize,
    },
    UnsupportedFormat,
    UnsupportedFeatures {
        prototype: usize,
        bits: u64,
    },
    MissingFeature {
        prototype: usize,
        feature: &'static str,
    },
    InvalidReference {
        prototype: Option<usize>,
        what: &'static str,
        index: usize,
        count: usize,
    },
    DuplicateSource(SourceId),
    InvalidPrototypeTree {
        prototype: usize,
        message: &'static str,
    },
    InvalidMetadata {
        prototype: usize,
        what: &'static str,
    },
    InvalidSourceMap {
        prototype: usize,
        pc: Option<usize>,
        what: &'static str,
    },
    InvalidInstruction {
        prototype: usize,
        pc: usize,
        what: &'static str,
    },
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Limit {
                what,
                actual,
                limit,
            } => {
                write!(f, "{what} {actual} exceeds limit {limit}")
            }
            Self::Unrepresentable { what, length } => {
                write!(f, "{what} length {length} cannot be represented in BluV1")
            }
            Self::Allocation { what, requested } => {
                write!(f, "could not allocate {requested} entries for {what}")
            }
            Self::UnsupportedFormat => f.write_str("artifact is not BluV1"),
            Self::UnsupportedFeatures { prototype, bits } => write!(
                f,
                "prototype {prototype} requires unsupported feature bits {bits:#x}"
            ),
            Self::MissingFeature { prototype, feature } => {
                write!(
                    f,
                    "prototype {prototype} is missing required feature {feature}"
                )
            }
            Self::InvalidReference {
                prototype,
                what,
                index,
                count,
            } => {
                if let Some(prototype) = prototype {
                    write!(
                        f,
                        "prototype {prototype} has invalid {what} {index} (count {count})"
                    )
                } else {
                    write!(f, "invalid {what} {index} (count {count})")
                }
            }
            Self::DuplicateSource(source) => write!(f, "duplicate source identity {source}"),
            Self::InvalidPrototypeTree { prototype, message } => {
                write!(f, "prototype {prototype}: {message}")
            }
            Self::InvalidMetadata { prototype, what } => {
                write!(f, "prototype {prototype} has invalid {what}")
            }
            Self::InvalidSourceMap {
                prototype,
                pc,
                what,
            } => match pc {
                Some(pc) => write!(
                    f,
                    "prototype {prototype}, pc {pc}: invalid source map: {what}"
                ),
                None => write!(f, "prototype {prototype}: invalid source map: {what}"),
            },
            Self::InvalidInstruction {
                prototype,
                pc,
                what,
            } => write!(f, "prototype {prototype}, pc {pc}: {what}"),
        }
    }
}

impl std::error::Error for ValidationError {}

pub fn validate(artifact: &Artifact, limits: BluLimits) -> Result<(), ValidationError> {
    if artifact.format != BytecodeFormat::BluV1 {
        return Err(ValidationError::UnsupportedFormat);
    }
    check_limit("source count", artifact.sources.len(), limits.max_sources)?;
    check_wire_len("source count", artifact.sources.len())?;
    check_limit(
        "prototype count",
        artifact.prototypes.len(),
        limits.max_prototypes,
    )?;
    check_wire_len("prototype count", artifact.prototypes.len())?;
    check_limit(
        "compiler name bytes",
        artifact.compiler.name().len(),
        limits.identity.max_compiler_name_bytes,
    )?;
    check_wire_len("compiler name", artifact.compiler.name().len())?;
    check_limit(
        "compiler version bytes",
        artifact.compiler.version().len(),
        limits.identity.max_compiler_version_bytes,
    )?;
    check_wire_len("compiler version", artifact.compiler.version().len())?;
    if let Some(revision) = artifact.compiler.revision() {
        check_wire_len("compiler revision", revision.len())?;
        check_limit(
            "compiler revision bytes",
            revision.len(),
            limits.identity.max_compiler_revision_bytes,
        )?;
    }
    if artifact.main as usize >= artifact.prototypes.len() {
        return Err(ValidationError::InvalidReference {
            prototype: None,
            what: "main prototype",
            index: artifact.main as usize,
            count: artifact.prototypes.len(),
        });
    }
    validate_aggregate_limits(artifact, limits)?;
    let encoded_size = encoded_size(artifact).map_err(|_| ValidationError::Limit {
        what: "encoded artifact bytes",
        actual: usize::MAX,
        limit: limits.max_bytes,
    })?;
    check_limit("encoded artifact bytes", encoded_size, limits.max_bytes)?;

    let mut sources = HashMap::new();
    sources
        .try_reserve(artifact.sources.len())
        .map_err(|_| ValidationError::Allocation {
            what: "source index",
            requested: artifact.sources.len(),
        })?;
    for source in &artifact.sources {
        check_wire_len("source name", source.identity.name().len())?;
        check_limit(
            "source name bytes",
            source.identity.name().len(),
            limits.identity.max_source_name_bytes,
        )?;
        if sources
            .insert(source.identity.id(), source.byte_len)
            .is_some()
        {
            return Err(ValidationError::DuplicateSource(source.identity.id()));
        }
    }

    let mut parents = Vec::new();
    try_reserve_validation(
        &mut parents,
        "prototype parent index",
        artifact.prototypes.len(),
    )?;
    parents.resize(artifact.prototypes.len(), None);
    for (index, prototype) in artifact.prototypes.iter().enumerate() {
        validate_prototype(index, prototype, &sources, limits)?;
        check_limit(
            "child count",
            prototype.children.len(),
            limits.max_children_per_prototype,
        )?;
        for &child in &prototype.children {
            let child = child as usize;
            if child >= artifact.prototypes.len() {
                return Err(ValidationError::InvalidReference {
                    prototype: Some(index),
                    what: "child prototype",
                    index: child,
                    count: artifact.prototypes.len(),
                });
            }
            if child == index {
                return Err(ValidationError::InvalidPrototypeTree {
                    prototype: index,
                    message: "prototype is its own child",
                });
            }
            if parents[child].replace(index).is_some() {
                return Err(ValidationError::InvalidPrototypeTree {
                    prototype: child,
                    message: "prototype has more than one parent",
                });
            }
        }
    }
    let main = artifact.main as usize;
    if parents[main].is_some() {
        return Err(ValidationError::InvalidPrototypeTree {
            prototype: main,
            message: "main prototype has a parent",
        });
    }
    validate_prototype_graph(&artifact.prototypes, main)?;
    for (index, prototype) in artifact.prototypes.iter().enumerate() {
        if let Some(parent) = parents[index] {
            let parent = &artifact.prototypes[parent];
            for upvalue in &prototype.upvalues {
                let (what, value, count) = match *upvalue {
                    Upvalue::ParentRegister(value) => (
                        "parent register",
                        value as usize,
                        parent.register_count as usize,
                    ),
                    Upvalue::ParentUpvalue(value) => {
                        ("parent upvalue", value as usize, parent.upvalues.len())
                    }
                };
                if value >= count {
                    return Err(ValidationError::InvalidReference {
                        prototype: Some(index),
                        what,
                        index: value,
                        count,
                    });
                }
            }
        } else if !prototype.upvalues.is_empty() {
            return Err(ValidationError::InvalidMetadata {
                prototype: index,
                what: "main prototype upvalues",
            });
        }
    }
    Ok(())
}

fn validate_aggregate_limits(
    artifact: &Artifact,
    limits: BluLimits,
) -> Result<(), ValidationError> {
    let prototypes = &artifact.prototypes;
    let mut registers = 0usize;
    let mut constants = 0usize;
    let mut code = 0usize;
    let mut children = 0usize;
    let mut upvalues = 0usize;
    let mut debug_entries = 0usize;
    let mut debug_bytes = 0usize;
    let mut source_map_entries = 0usize;
    let mut decoded_bytes = DECODED_ARTIFACT_CHARGE;
    add_validation_product(
        &mut decoded_bytes,
        artifact.sources.len(),
        DECODED_SOURCE_CHARGE,
        "estimated decoded bytes",
        limits.max_decoded_bytes,
    )?;
    add_validation_product(
        &mut decoded_bytes,
        prototypes.len(),
        DECODED_PROTOTYPE_CHARGE,
        "estimated decoded bytes",
        limits.max_decoded_bytes,
    )?;
    for bytes in [
        artifact.compiler.name().len(),
        artifact.compiler.version().len(),
        artifact.compiler.revision().map_or(0, str::len),
    ] {
        add_validation_total(
            &mut decoded_bytes,
            bytes,
            "estimated decoded bytes",
            limits.max_decoded_bytes,
        )?;
    }
    for source in &artifact.sources {
        add_validation_total(
            &mut decoded_bytes,
            source.identity.name().len(),
            "estimated decoded bytes",
            limits.max_decoded_bytes,
        )?;
    }
    for prototype in prototypes {
        add_validation_total(
            &mut registers,
            usize::from(prototype.register_count),
            "total register count",
            limits.max_total_registers,
        )?;
        add_validation_total(
            &mut constants,
            prototype.constants.len(),
            "total constant count",
            limits.max_total_constants,
        )?;
        add_validation_total(
            &mut code,
            prototype.code.len(),
            "total instruction count",
            limits.max_total_code,
        )?;
        add_validation_total(
            &mut children,
            prototype.children.len(),
            "total child count",
            limits.max_total_children,
        )?;
        add_validation_total(
            &mut upvalues,
            prototype.upvalues.len(),
            "total upvalue count",
            limits.max_total_upvalues,
        )?;
        add_validation_total(
            &mut debug_entries,
            prototype.locals.len(),
            "total debug entry count",
            limits.max_total_debug_entries,
        )?;
        add_validation_total(
            &mut debug_entries,
            prototype.upvalue_debug.len(),
            "total debug entry count",
            limits.max_total_debug_entries,
        )?;
        add_validation_total(
            &mut source_map_entries,
            prototype.source_map.len(),
            "total source map entry count",
            limits.max_total_source_map_entries,
        )?;
        for (count, width) in [
            (prototype.constants.len(), DECODED_CONSTANT_CHARGE),
            (prototype.code.len(), DECODED_INSTRUCTION_CHARGE),
            (prototype.children.len(), DECODED_CHILD_CHARGE),
            (prototype.upvalues.len(), DECODED_UPVALUE_CHARGE),
            (prototype.source_map.len(), DECODED_SOURCE_MAP_CHARGE),
            (prototype.locals.len(), DECODED_LOCAL_DEBUG_CHARGE),
            (prototype.upvalue_debug.len(), DECODED_UPVALUE_DEBUG_CHARGE),
        ] {
            add_validation_product(
                &mut decoded_bytes,
                count,
                width,
                "estimated decoded bytes",
                limits.max_decoded_bytes,
            )?;
        }
    }

    let mut constant_bytes = 0usize;
    for constant in prototypes.iter().flat_map(|prototype| &prototype.constants) {
        if let Constant::String(bytes) = constant {
            add_validation_total(
                &mut constant_bytes,
                bytes.len(),
                "total constant bytes",
                limits.max_total_constant_bytes,
            )?;
            add_validation_total(
                &mut decoded_bytes,
                bytes.len(),
                "estimated decoded bytes",
                limits.max_decoded_bytes,
            )?;
        }
    }
    for name in prototypes.iter().flat_map(|prototype| {
        prototype
            .locals
            .iter()
            .map(|local| local.name.as_slice())
            .chain(
                prototype
                    .upvalue_debug
                    .iter()
                    .map(|upvalue| upvalue.name.as_slice()),
            )
    }) {
        add_validation_total(
            &mut debug_bytes,
            name.len(),
            "total debug bytes",
            limits.max_total_debug_bytes,
        )?;
        add_validation_total(
            &mut decoded_bytes,
            name.len(),
            "estimated decoded bytes",
            limits.max_decoded_bytes,
        )?;
    }
    check_limit(
        "estimated decoded bytes",
        decoded_bytes,
        limits.max_decoded_bytes,
    )?;
    Ok(())
}

fn add_validation_product(
    total: &mut usize,
    count: usize,
    width: usize,
    what: &'static str,
    limit: usize,
) -> Result<(), ValidationError> {
    let amount = count.checked_mul(width).ok_or(ValidationError::Limit {
        what,
        actual: usize::MAX,
        limit,
    })?;
    add_validation_total(total, amount, what, limit)
}

fn add_validation_total(
    total: &mut usize,
    amount: usize,
    what: &'static str,
    limit: usize,
) -> Result<(), ValidationError> {
    *total = total.checked_add(amount).ok_or(ValidationError::Limit {
        what,
        actual: usize::MAX,
        limit,
    })?;
    check_limit(what, *total, limit)
}

fn validate_prototype_graph(prototypes: &[Prototype], main: usize) -> Result<(), ValidationError> {
    const WHITE: u8 = 0;
    const GRAY: u8 = 1;
    const BLACK: u8 = 2;

    let mut colors = Vec::new();
    try_reserve_validation(&mut colors, "prototype traversal colors", prototypes.len())?;
    colors.resize(prototypes.len(), WHITE);
    let mut stack = Vec::new();
    try_reserve_validation(&mut stack, "prototype cycle traversal", prototypes.len())?;
    for start in 0..prototypes.len() {
        if colors[start] != WHITE {
            continue;
        }
        colors[start] = GRAY;
        stack.push((start, 0usize));
        while let Some(&(prototype, next_child)) = stack.last() {
            if next_child == prototypes[prototype].children.len() {
                colors[prototype] = BLACK;
                stack.pop();
                continue;
            }
            stack.last_mut().expect("stack is non-empty").1 += 1;
            let child = prototypes[prototype].children[next_child] as usize;
            match colors[child] {
                WHITE => {
                    colors[child] = GRAY;
                    stack.push((child, 0));
                }
                GRAY => {
                    return Err(ValidationError::InvalidPrototypeTree {
                        prototype: child,
                        message: "prototype graph contains a cycle",
                    });
                }
                BLACK => {}
                _ => unreachable!("prototype color is internal"),
            }
        }
    }

    let mut reachable = Vec::new();
    try_reserve_validation(&mut reachable, "prototype reachability", prototypes.len())?;
    reachable.resize(prototypes.len(), false);
    let mut pending = Vec::new();
    try_reserve_validation(
        &mut pending,
        "prototype reachability traversal",
        prototypes.len(),
    )?;
    pending.push(main);
    while let Some(index) = pending.pop() {
        reachable[index] = true;
        pending.extend(
            prototypes[index]
                .children
                .iter()
                .map(|child| *child as usize),
        );
    }
    for (index, is_reachable) in reachable.into_iter().enumerate() {
        if !is_reachable {
            return Err(ValidationError::InvalidPrototypeTree {
                prototype: index,
                message: "prototype is unreachable from the main prototype",
            });
        }
    }
    Ok(())
}

fn validate_prototype(
    index: usize,
    prototype: &Prototype,
    sources: &HashMap<SourceId, u32>,
    limits: BluLimits,
) -> Result<(), ValidationError> {
    check_limit(
        "register count",
        prototype.register_count as usize,
        limits.max_registers_per_prototype,
    )?;
    check_limit(
        "constant count",
        prototype.constants.len(),
        limits.max_constants_per_prototype,
    )?;
    check_wire_len("constant count", prototype.constants.len())?;
    check_limit(
        "instruction count",
        prototype.code.len(),
        limits.max_code_per_prototype,
    )?;
    check_wire_len("instruction count", prototype.code.len())?;
    check_limit(
        "upvalue count",
        prototype.upvalues.len(),
        limits.max_upvalues_per_prototype,
    )?;
    check_wire_len("upvalue count", prototype.upvalues.len())?;
    check_wire_len("child count", prototype.children.len())?;
    check_limit(
        "local debug count",
        prototype.locals.len(),
        limits.max_debug_entries_per_prototype,
    )?;
    check_wire_len("local debug count", prototype.locals.len())?;
    check_limit(
        "upvalue debug count",
        prototype.upvalue_debug.len(),
        limits.max_debug_entries_per_prototype,
    )?;
    check_wire_len("upvalue debug count", prototype.upvalue_debug.len())?;
    if prototype.parameter_count > prototype.register_count {
        return Err(ValidationError::InvalidMetadata {
            prototype: index,
            what: "parameter count",
        });
    }
    let unsupported = prototype.required_features.bits() & !FeatureBits::SUPPORTED.bits();
    if unsupported != 0 {
        return Err(ValidationError::UnsupportedFeatures {
            prototype: index,
            bits: unsupported,
        });
    }
    if !prototype.required_features.contains(FeatureBits::BASELINE) {
        return Err(ValidationError::MissingFeature {
            prototype: index,
            feature: "baseline",
        });
    }
    let Some(&source_len) = sources.get(&prototype.source) else {
        return Err(ValidationError::InvalidSourceMap {
            prototype: index,
            pc: None,
            what: "prototype source is not declared",
        });
    };
    if prototype.source_map.len() != prototype.code.len() {
        return Err(ValidationError::InvalidSourceMap {
            prototype: index,
            pc: None,
            what: "map length does not equal instruction count",
        });
    }
    for (pc, span) in prototype.source_map.iter().copied().enumerate() {
        if span.source() != prototype.source {
            return Err(ValidationError::InvalidSourceMap {
                prototype: index,
                pc: Some(pc),
                what: "span belongs to a different source",
            });
        }
        if span.end().get() > source_len {
            return Err(ValidationError::InvalidSourceMap {
                prototype: index,
                pc: Some(pc),
                what: "span exceeds source length",
            });
        }
    }
    for constant in &prototype.constants {
        if let Constant::String(bytes) = constant {
            check_limit("constant bytes", bytes.len(), limits.max_constant_bytes)?;
            check_wire_len("constant bytes", bytes.len())?;
        }
    }

    let registers = prototype.register_count as usize;
    let mut initialized = Vec::new();
    try_reserve_validation(&mut initialized, "register initialization", registers)?;
    initialized.resize(registers, false);
    initialized[..prototype.parameter_count as usize].fill(true);
    for (pc, instruction) in prototype.code.iter().copied().enumerate() {
        if !instruction_is_legal(prototype.profile, instruction) {
            return Err(ValidationError::InvalidInstruction {
                prototype: index,
                pc,
                what: "instruction is not legal for semantic profile",
            });
        }
        match instruction {
            Instruction::LoadConstant {
                destination,
                constant,
            } => {
                check_register(index, pc, destination, registers)?;
                if constant as usize >= prototype.constants.len() {
                    return Err(ValidationError::InvalidReference {
                        prototype: Some(index),
                        what: "constant",
                        index: constant as usize,
                        count: prototype.constants.len(),
                    });
                }
                initialized[destination as usize] = true;
            }
            Instruction::Add {
                destination,
                left,
                right,
            } => {
                check_register(index, pc, destination, registers)?;
                check_read(index, pc, left, &initialized)?;
                check_read(index, pc, right, &initialized)?;
                initialized[destination as usize] = true;
            }
            Instruction::Return { first, count } => {
                let end = usize::from(first).checked_add(usize::from(count)).ok_or(
                    ValidationError::InvalidInstruction {
                        prototype: index,
                        pc,
                        what: "return register range overflows",
                    },
                )?;
                if count == 0 || end > registers {
                    return Err(ValidationError::InvalidInstruction {
                        prototype: index,
                        pc,
                        what: "return register range is invalid",
                    });
                }
                for register in first..first + count {
                    check_read(index, pc, register, &initialized)?;
                }
                if pc + 1 != prototype.code.len() {
                    return Err(ValidationError::InvalidInstruction {
                        prototype: index,
                        pc,
                        what: "return must terminate straight-line code",
                    });
                }
            }
        }
    }
    if !matches!(prototype.code.last(), Some(Instruction::Return { .. })) {
        return Err(ValidationError::InvalidInstruction {
            prototype: index,
            pc: prototype.code.len(),
            what: "prototype must end with return",
        });
    }
    validate_debug(index, prototype, limits)
}

fn validate_debug(
    index: usize,
    prototype: &Prototype,
    limits: BluLimits,
) -> Result<(), ValidationError> {
    let code_len = prototype.code.len();
    for local in &prototype.locals {
        check_wire_len("local debug name", local.name.len())?;
        check_limit(
            "debug name bytes",
            local.name.len(),
            limits.max_debug_name_bytes,
        )?;
        if local.register >= prototype.register_count
            || local.start_pc >= local.end_pc
            || local.end_pc as usize > code_len
        {
            return Err(ValidationError::InvalidMetadata {
                prototype: index,
                what: "local debug range",
            });
        }
    }
    for upvalue in &prototype.upvalue_debug {
        check_wire_len("upvalue debug name", upvalue.name.len())?;
        check_limit(
            "debug name bytes",
            upvalue.name.len(),
            limits.max_debug_name_bytes,
        )?;
        if upvalue.upvalue as usize >= prototype.upvalues.len()
            || upvalue.start_pc >= upvalue.end_pc
            || upvalue.end_pc as usize > code_len
        {
            return Err(ValidationError::InvalidMetadata {
                prototype: index,
                what: "upvalue debug range",
            });
        }
    }
    Ok(())
}

fn check_register(
    prototype: usize,
    pc: usize,
    register: u16,
    count: usize,
) -> Result<(), ValidationError> {
    if register as usize >= count {
        Err(ValidationError::InvalidInstruction {
            prototype,
            pc,
            what: "register is out of bounds",
        })
    } else {
        Ok(())
    }
}

fn check_read(
    prototype: usize,
    pc: usize,
    register: u16,
    initialized: &[bool],
) -> Result<(), ValidationError> {
    check_register(prototype, pc, register, initialized.len())?;
    if initialized[register as usize] {
        Ok(())
    } else {
        Err(ValidationError::InvalidInstruction {
            prototype,
            pc,
            what: "register is read before initialization",
        })
    }
}

fn check_limit(what: &'static str, actual: usize, limit: usize) -> Result<(), ValidationError> {
    if actual > limit {
        Err(ValidationError::Limit {
            what,
            actual,
            limit,
        })
    } else {
        Ok(())
    }
}

fn check_wire_len(what: &'static str, length: usize) -> Result<(), ValidationError> {
    if u32::try_from(length).is_err() {
        Err(ValidationError::Unrepresentable { what, length })
    } else {
        Ok(())
    }
}

fn try_reserve_validation<T>(
    values: &mut Vec<T>,
    what: &'static str,
    requested: usize,
) -> Result<(), ValidationError> {
    values
        .try_reserve_exact(requested)
        .map_err(|_| ValidationError::Allocation { what, requested })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EncodeError {
    Validation(ValidationError),
    UnsupportedProfile(SemanticProfile),
    LengthOverflow { what: &'static str, length: usize },
    SizeOverflow,
    Allocation { requested: usize },
    TooLarge { actual: usize, limit: usize },
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(error) => error.fmt(f),
            Self::UnsupportedProfile(profile) => {
                write!(f, "semantic profile {profile} has no BluV1 wire tag")
            }
            Self::LengthOverflow { what, length } => {
                write!(f, "{what} length {length} cannot be represented in BluV1")
            }
            Self::SizeOverflow => f.write_str("encoded artifact size overflows usize"),
            Self::Allocation { requested } => {
                write!(f, "could not allocate {requested} encoded artifact bytes")
            }
            Self::TooLarge { actual, limit } => {
                write!(f, "encoded artifact size {actual} exceeds limit {limit}")
            }
        }
    }
}

impl std::error::Error for EncodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Validation(error) => Some(error),
            Self::UnsupportedProfile(_)
            | Self::LengthOverflow { .. }
            | Self::SizeOverflow
            | Self::Allocation { .. }
            | Self::TooLarge { .. } => None,
        }
    }
}

/// Canonically encode a validated artifact using fixed-width little-endian
/// integers. Reserved fields are always zero.
pub fn encode(artifact: &ValidatedArtifact, limits: BluLimits) -> Result<Vec<u8>, EncodeError> {
    validate(artifact.artifact(), limits).map_err(EncodeError::Validation)?;
    let artifact = artifact.artifact();
    let encoded_size = encoded_size(artifact)?;
    if encoded_size > limits.max_bytes {
        return Err(EncodeError::TooLarge {
            actual: encoded_size,
            limit: limits.max_bytes,
        });
    }
    let mut out = Vec::new();
    out.try_reserve_exact(encoded_size)
        .map_err(|_| EncodeError::Allocation {
            requested: encoded_size,
        })?;
    out.extend_from_slice(&MAGIC);
    put_u16(&mut out, BLU_V1_VERSION);
    put_u16(&mut out, 0);
    put_compiler(&mut out, &artifact.compiler)?;
    put_len(&mut out, "source count", artifact.sources.len())?;
    for source in &artifact.sources {
        put_u32(&mut out, source.identity.id().get());
        put_bytes(&mut out, "source name", source.identity.name().as_bytes())?;
        put_u32(&mut out, source.byte_len);
        out.extend_from_slice(&source.digest);
    }
    put_len(&mut out, "prototype count", artifact.prototypes.len())?;
    put_u32(&mut out, artifact.main);
    for prototype in &artifact.prototypes {
        put_prototype(&mut out, prototype)?;
    }
    debug_assert_eq!(out.len(), encoded_size);
    Ok(out)
}

fn encoded_size(artifact: &Artifact) -> Result<usize, EncodeError> {
    let mut size = 8usize;
    add_size(&mut size, 16 + 4 + artifact.compiler.name().len())?;
    add_size(&mut size, 4 + artifact.compiler.version().len() + 1)?;
    if let Some(revision) = artifact.compiler.revision() {
        add_size(&mut size, 4 + revision.len())?;
    }
    add_size(&mut size, 4)?;
    for source in &artifact.sources {
        add_size(&mut size, 4 + 4 + source.identity.name().len() + 4 + 32)?;
    }
    add_size(&mut size, 4 + 4)?;
    for prototype in &artifact.prototypes {
        profile_tag(prototype.profile)?;
        add_size(&mut size, 20 + 4)?;
        for constant in &prototype.constants {
            add_size(
                &mut size,
                match constant {
                    Constant::Nil | Constant::Boolean(_) => 1,
                    Constant::Number(_) => 9,
                    Constant::String(bytes) => 1 + 4 + bytes.len(),
                },
            )?;
        }
        add_size(&mut size, 4)?;
        add_scaled(&mut size, prototype.upvalues.len(), 3)?;
        add_size(&mut size, 4)?;
        add_scaled(&mut size, prototype.children.len(), 4)?;
        add_size(&mut size, 4)?;
        for instruction in &prototype.code {
            add_size(
                &mut size,
                match instruction {
                    Instruction::LoadConstant { .. } | Instruction::Add { .. } => 7,
                    Instruction::Return { .. } => 5,
                },
            )?;
        }
        add_size(&mut size, 4)?;
        add_scaled(&mut size, prototype.source_map.len(), 12)?;
        add_size(&mut size, 4)?;
        for local in &prototype.locals {
            add_size(&mut size, 4 + local.name.len() + 2 + 4 + 4)?;
        }
        add_size(&mut size, 4)?;
        for upvalue in &prototype.upvalue_debug {
            add_size(&mut size, 4 + upvalue.name.len() + 2 + 4 + 4)?;
        }
    }
    Ok(size)
}

fn add_size(size: &mut usize, amount: usize) -> Result<(), EncodeError> {
    *size = size.checked_add(amount).ok_or(EncodeError::SizeOverflow)?;
    Ok(())
}

fn add_scaled(size: &mut usize, count: usize, width: usize) -> Result<(), EncodeError> {
    let amount = count.checked_mul(width).ok_or(EncodeError::SizeOverflow)?;
    add_size(size, amount)
}

fn put_compiler(out: &mut Vec<u8>, compiler: &CompilerIdentity) -> Result<(), EncodeError> {
    out.extend_from_slice(compiler.id().as_bytes());
    put_bytes(out, "compiler name", compiler.name().as_bytes())?;
    put_bytes(out, "compiler version", compiler.version().as_bytes())?;
    match compiler.revision() {
        Some(revision) => {
            out.push(1);
            put_bytes(out, "compiler revision", revision.as_bytes())?;
        }
        None => out.push(0),
    }
    Ok(())
}

fn put_prototype(out: &mut Vec<u8>, prototype: &Prototype) -> Result<(), EncodeError> {
    out.push(profile_tag(prototype.profile)?);
    out.push(u8::from(prototype.is_vararg));
    put_u16(out, 0);
    put_u32(out, prototype.source.get());
    put_u16(out, prototype.register_count);
    put_u16(out, prototype.parameter_count);
    put_u64(out, prototype.required_features.bits());
    put_len(out, "constant count", prototype.constants.len())?;
    for constant in &prototype.constants {
        match constant {
            Constant::Nil => out.push(0),
            Constant::Boolean(false) => out.push(1),
            Constant::Boolean(true) => out.push(2),
            Constant::Number(value) => {
                out.push(3);
                put_u64(out, value.to_bits());
            }
            Constant::String(bytes) => {
                out.push(4);
                put_bytes(out, "constant bytes", bytes)?;
            }
        }
    }
    put_len(out, "upvalue count", prototype.upvalues.len())?;
    for upvalue in &prototype.upvalues {
        match upvalue {
            Upvalue::ParentRegister(index) => {
                out.push(0);
                put_u16(out, *index);
            }
            Upvalue::ParentUpvalue(index) => {
                out.push(1);
                put_u16(out, *index);
            }
        }
    }
    put_len(out, "child count", prototype.children.len())?;
    for child in &prototype.children {
        put_u32(out, *child);
    }
    put_len(out, "instruction count", prototype.code.len())?;
    for instruction in &prototype.code {
        match instruction {
            Instruction::LoadConstant {
                destination,
                constant,
            } => {
                out.push(0);
                put_u16(out, *destination);
                put_u32(out, *constant);
            }
            Instruction::Add {
                destination,
                left,
                right,
            } => {
                out.push(1);
                put_u16(out, *destination);
                put_u16(out, *left);
                put_u16(out, *right);
            }
            Instruction::Return { first, count } => {
                out.push(2);
                put_u16(out, *first);
                put_u16(out, *count);
            }
        }
    }
    put_len(out, "source map count", prototype.source_map.len())?;
    for span in &prototype.source_map {
        put_u32(out, span.source().get());
        put_u32(out, span.start().get());
        put_u32(out, span.end().get());
    }
    put_len(out, "local debug count", prototype.locals.len())?;
    for local in &prototype.locals {
        put_bytes(out, "local name", &local.name)?;
        put_u16(out, local.register);
        put_u32(out, local.start_pc);
        put_u32(out, local.end_pc);
    }
    put_len(out, "upvalue debug count", prototype.upvalue_debug.len())?;
    for upvalue in &prototype.upvalue_debug {
        put_bytes(out, "upvalue name", &upvalue.name)?;
        put_u16(out, upvalue.upvalue);
        put_u32(out, upvalue.start_pc);
        put_u32(out, upvalue.end_pc);
    }
    Ok(())
}

fn put_bytes(out: &mut Vec<u8>, what: &'static str, bytes: &[u8]) -> Result<(), EncodeError> {
    put_len(out, what, bytes.len())?;
    out.extend_from_slice(bytes);
    Ok(())
}

fn put_len(out: &mut Vec<u8>, what: &'static str, length: usize) -> Result<(), EncodeError> {
    let length = u32::try_from(length).map_err(|_| EncodeError::LengthOverflow { what, length })?;
    put_u32(out, length);
    Ok(())
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodeError {
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
    CountOverflow {
        offset: usize,
        what: &'static str,
        count: usize,
        minimum_width: usize,
    },
    DeclaredBodyTooLarge {
        offset: usize,
        what: &'static str,
        count: usize,
        minimum_width: usize,
        required: usize,
        remaining: usize,
    },
    Allocation {
        what: &'static str,
        requested: usize,
    },
    InvalidMagic([u8; 4]),
    UnsupportedVersion(u16),
    UnsupportedField {
        offset: usize,
        what: &'static str,
        value: u64,
    },
    InvalidTag {
        offset: usize,
        what: &'static str,
        tag: u8,
    },
    InvalidUtf8 {
        offset: usize,
        what: &'static str,
    },
    InvalidIdentity {
        offset: usize,
        what: &'static str,
    },
    InvalidSpan {
        offset: usize,
        start: u32,
        end: u32,
    },
    TrailingBytes {
        count: usize,
    },
    Validation(ValidationError),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge {
                what,
                actual,
                limit,
            } => {
                write!(f, "{what} {actual} exceeds limit {limit}")
            }
            Self::UnexpectedEnd {
                offset,
                needed,
                remaining,
            } => write!(
                f,
                "truncated BluV1 at offset {offset}: need {needed} bytes, have {remaining}"
            ),
            Self::CountOverflow {
                offset,
                what,
                count,
                minimum_width,
            } => write!(
                f,
                "{what} count {count} times minimum width {minimum_width} overflows at offset {offset}"
            ),
            Self::DeclaredBodyTooLarge {
                offset,
                what,
                count,
                minimum_width,
                required,
                remaining,
            } => write!(
                f,
                "{what} count {count} requires at least {required} bytes \
                 ({minimum_width} each) at offset {offset}, but only {remaining} remain"
            ),
            Self::Allocation { what, requested } => {
                write!(f, "could not allocate {requested} entries/bytes for {what}")
            }
            Self::InvalidMagic(magic) => write!(f, "invalid BluV1 magic {magic:?}"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported Blu bytecode version {version}")
            }
            Self::UnsupportedField {
                offset,
                what,
                value,
            } => write!(f, "unsupported {what} value {value} at offset {offset}"),
            Self::InvalidTag { offset, what, tag } => {
                write!(f, "invalid {what} tag {tag} at offset {offset}")
            }
            Self::InvalidUtf8 { offset, what } => {
                write!(f, "invalid UTF-8 in {what} at offset {offset}")
            }
            Self::InvalidIdentity { offset, what } => {
                write!(f, "invalid {what} at offset {offset}")
            }
            Self::InvalidSpan { offset, start, end } => {
                write!(f, "invalid span {start}..{end} at offset {offset}")
            }
            Self::TrailingBytes { count } => write!(f, "{count} trailing BluV1 bytes"),
            Self::Validation(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for DecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Validation(error) => Some(error),
            _ => None,
        }
    }
}

pub fn decode(bytes: &[u8], limits: BluLimits) -> Result<Artifact, DecodeError> {
    if bytes.len() > limits.max_bytes {
        return Err(DecodeError::TooLarge {
            what: "artifact bytes",
            actual: bytes.len(),
            limit: limits.max_bytes,
        });
    }
    let mut reader = Reader { bytes, offset: 0 };
    let mut budget = DecodeBudget::new(limits)?;
    let magic = reader.array::<4>()?;
    if magic != MAGIC {
        return Err(DecodeError::InvalidMagic(magic));
    }
    let version = reader.u16()?;
    if version != BLU_V1_VERSION {
        return Err(DecodeError::UnsupportedVersion(version));
    }
    let reserved_offset = reader.offset;
    let reserved = reader.u16()?;
    if reserved != 0 {
        return Err(DecodeError::UnsupportedField {
            offset: reserved_offset,
            what: "header flags",
            value: u64::from(reserved),
        });
    }
    let compiler = read_compiler(&mut reader, limits, &mut budget)?;
    let source_count = reader.count("source count", limits.max_sources, 44)?;
    budget.charge_fixed(source_count, DECODED_SOURCE_CHARGE, limits)?;
    let mut sources = Vec::new();
    try_reserve_decode(&mut sources, "sources", source_count)?;
    for _ in 0..source_count {
        let id = SourceId::new(reader.u32()?);
        let name_offset = reader.offset;
        let name = reader.text(
            "source name",
            limits.identity.max_source_name_bytes,
            &mut budget,
            limits,
        )?;
        let identity = SourceIdentity::new(id, name, limits.identity).map_err(|_| {
            DecodeError::InvalidIdentity {
                offset: name_offset,
                what: "source identity",
            }
        })?;
        let byte_len = reader.u32()?;
        let digest = reader.array::<32>()?;
        sources.push(SourceRecord {
            identity,
            byte_len,
            digest,
        });
    }
    let prototype_count =
        reader.count_with_extra("prototype count", limits.max_prototypes, 48, 4)?;
    let main = reader.u32()?;
    budget.charge_fixed(prototype_count, DECODED_PROTOTYPE_CHARGE, limits)?;
    let mut prototypes = Vec::new();
    try_reserve_decode(&mut prototypes, "prototypes", prototype_count)?;
    for _ in 0..prototype_count {
        prototypes.push(read_prototype(&mut reader, limits, &mut budget)?);
    }
    if reader.offset != bytes.len() {
        return Err(DecodeError::TrailingBytes {
            count: bytes.len() - reader.offset,
        });
    }
    Ok(Artifact {
        format: BytecodeFormat::BluV1,
        compiler,
        sources,
        prototypes,
        main,
    })
}

pub fn decode_validated(bytes: &[u8], limits: BluLimits) -> Result<ValidatedArtifact, DecodeError> {
    let artifact = decode(bytes, limits)?;
    ValidatedArtifact::new(artifact, limits).map_err(DecodeError::Validation)
}

fn read_compiler(
    reader: &mut Reader<'_>,
    limits: BluLimits,
    budget: &mut DecodeBudget,
) -> Result<CompilerIdentity, DecodeError> {
    let id = CompilerId::new(reader.array::<16>()?);
    let start = reader.offset;
    let name = reader.text(
        "compiler name",
        limits.identity.max_compiler_name_bytes,
        budget,
        limits,
    )?;
    let version = reader.text(
        "compiler version",
        limits.identity.max_compiler_version_bytes,
        budget,
        limits,
    )?;
    let revision_offset = reader.offset;
    let revision = match reader.u8()? {
        0 => None,
        1 => Some(reader.text(
            "compiler revision",
            limits.identity.max_compiler_revision_bytes,
            budget,
            limits,
        )?),
        tag => {
            return Err(DecodeError::InvalidTag {
                offset: revision_offset,
                what: "compiler revision presence",
                tag,
            });
        }
    };
    CompilerIdentity::new(id, name, version, revision, limits.identity).map_err(|_| {
        DecodeError::InvalidIdentity {
            offset: start,
            what: "compiler identity",
        }
    })
}

struct DecodeBudget {
    registers: usize,
    constants: usize,
    constant_bytes: usize,
    code: usize,
    children: usize,
    upvalues: usize,
    debug_entries: usize,
    debug_bytes: usize,
    source_map_entries: usize,
    decoded_bytes: usize,
}

impl DecodeBudget {
    fn new(limits: BluLimits) -> Result<Self, DecodeError> {
        let decoded_bytes = DECODED_ARTIFACT_CHARGE;
        if decoded_bytes > limits.max_decoded_bytes {
            return Err(DecodeError::TooLarge {
                what: "estimated decoded bytes",
                actual: decoded_bytes,
                limit: limits.max_decoded_bytes,
            });
        }
        Ok(Self {
            registers: 0,
            constants: 0,
            constant_bytes: 0,
            code: 0,
            children: 0,
            upvalues: 0,
            debug_entries: 0,
            debug_bytes: 0,
            source_map_entries: 0,
            decoded_bytes,
        })
    }

    fn add(
        total: &mut usize,
        amount: usize,
        what: &'static str,
        limit: usize,
    ) -> Result<(), DecodeError> {
        *total = total.checked_add(amount).ok_or(DecodeError::TooLarge {
            what,
            actual: usize::MAX,
            limit,
        })?;
        if *total > limit {
            Err(DecodeError::TooLarge {
                what,
                actual: *total,
                limit,
            })
        } else {
            Ok(())
        }
    }

    fn charge_fixed(
        &mut self,
        count: usize,
        logical_width: usize,
        limits: BluLimits,
    ) -> Result<(), DecodeError> {
        let amount = count
            .checked_mul(logical_width)
            .ok_or(DecodeError::TooLarge {
                what: "estimated decoded bytes",
                actual: usize::MAX,
                limit: limits.max_decoded_bytes,
            })?;
        self.charge_owned(amount, limits)
    }

    fn charge_owned(&mut self, bytes: usize, limits: BluLimits) -> Result<(), DecodeError> {
        Self::add(
            &mut self.decoded_bytes,
            bytes,
            "estimated decoded bytes",
            limits.max_decoded_bytes,
        )
    }

    fn charge_debug(&mut self, bytes: usize, limits: BluLimits) -> Result<(), DecodeError> {
        Self::add(
            &mut self.debug_bytes,
            bytes,
            "total debug bytes",
            limits.max_total_debug_bytes,
        )?;
        self.charge_owned(bytes, limits)
    }
}

fn read_prototype(
    reader: &mut Reader<'_>,
    limits: BluLimits,
    budget: &mut DecodeBudget,
) -> Result<Prototype, DecodeError> {
    let profile_offset = reader.offset;
    let profile = tag_profile(reader.u8()?).ok_or(DecodeError::InvalidTag {
        offset: profile_offset,
        what: "semantic profile",
        tag: reader.bytes[profile_offset],
    })?;
    let vararg_offset = reader.offset;
    let is_vararg = match reader.u8()? {
        0 => false,
        1 => true,
        tag => {
            return Err(DecodeError::InvalidTag {
                offset: vararg_offset,
                what: "vararg",
                tag,
            });
        }
    };
    let reserved_offset = reader.offset;
    let reserved = reader.u16()?;
    if reserved != 0 {
        return Err(DecodeError::UnsupportedField {
            offset: reserved_offset,
            what: "prototype flags",
            value: u64::from(reserved),
        });
    }
    let source = SourceId::new(reader.u32()?);
    let register_count = reader.u16()?;
    if usize::from(register_count) > limits.max_registers_per_prototype {
        return Err(DecodeError::TooLarge {
            what: "register count",
            actual: usize::from(register_count),
            limit: limits.max_registers_per_prototype,
        });
    }
    DecodeBudget::add(
        &mut budget.registers,
        usize::from(register_count),
        "total register count",
        limits.max_total_registers,
    )?;
    let parameter_count = reader.u16()?;
    let required_features = FeatureBits::from_bits(reader.u64()?);

    let constant_count = reader.count("constant count", limits.max_constants_per_prototype, 1)?;
    DecodeBudget::add(
        &mut budget.constants,
        constant_count,
        "total constant count",
        limits.max_total_constants,
    )?;
    budget.charge_fixed(constant_count, DECODED_CONSTANT_CHARGE, limits)?;
    let mut constants = Vec::new();
    try_reserve_decode(&mut constants, "constants", constant_count)?;
    for _ in 0..constant_count {
        let offset = reader.offset;
        constants.push(match reader.u8()? {
            0 => Constant::Nil,
            1 => Constant::Boolean(false),
            2 => Constant::Boolean(true),
            3 => Constant::Number(f64::from_bits(reader.u64()?)),
            4 => {
                let bytes = reader.sized_bytes("constant bytes", limits.max_constant_bytes)?;
                DecodeBudget::add(
                    &mut budget.constant_bytes,
                    bytes.len(),
                    "total constant bytes",
                    limits.max_total_constant_bytes,
                )?;
                budget.charge_owned(bytes.len(), limits)?;
                Constant::String(owned_decode_bytes("constant bytes", bytes)?)
            }
            tag => {
                return Err(DecodeError::InvalidTag {
                    offset,
                    what: "constant",
                    tag,
                });
            }
        });
    }

    let upvalue_count = reader.count("upvalue count", limits.max_upvalues_per_prototype, 3)?;
    DecodeBudget::add(
        &mut budget.upvalues,
        upvalue_count,
        "total upvalue count",
        limits.max_total_upvalues,
    )?;
    budget.charge_fixed(upvalue_count, DECODED_UPVALUE_CHARGE, limits)?;
    let mut upvalues = Vec::new();
    try_reserve_decode(&mut upvalues, "upvalues", upvalue_count)?;
    for _ in 0..upvalue_count {
        let offset = reader.offset;
        let tag = reader.u8()?;
        let index = reader.u16()?;
        upvalues.push(match tag {
            0 => Upvalue::ParentRegister(index),
            1 => Upvalue::ParentUpvalue(index),
            _ => {
                return Err(DecodeError::InvalidTag {
                    offset,
                    what: "upvalue",
                    tag,
                });
            }
        });
    }

    let child_count = reader.count("child count", limits.max_children_per_prototype, 4)?;
    DecodeBudget::add(
        &mut budget.children,
        child_count,
        "total child count",
        limits.max_total_children,
    )?;
    budget.charge_fixed(child_count, DECODED_CHILD_CHARGE, limits)?;
    let mut children = Vec::new();
    try_reserve_decode(&mut children, "children", child_count)?;
    for _ in 0..child_count {
        children.push(reader.u32()?);
    }

    let instruction_count = reader.count("instruction count", limits.max_code_per_prototype, 5)?;
    DecodeBudget::add(
        &mut budget.code,
        instruction_count,
        "total instruction count",
        limits.max_total_code,
    )?;
    budget.charge_fixed(instruction_count, DECODED_INSTRUCTION_CHARGE, limits)?;
    let mut code = Vec::new();
    try_reserve_decode(&mut code, "instructions", instruction_count)?;
    for _ in 0..instruction_count {
        let offset = reader.offset;
        code.push(match reader.u8()? {
            0 => Instruction::LoadConstant {
                destination: reader.u16()?,
                constant: reader.u32()?,
            },
            1 => Instruction::Add {
                destination: reader.u16()?,
                left: reader.u16()?,
                right: reader.u16()?,
            },
            2 => Instruction::Return {
                first: reader.u16()?,
                count: reader.u16()?,
            },
            tag => {
                return Err(DecodeError::InvalidTag {
                    offset,
                    what: "instruction",
                    tag,
                });
            }
        });
    }

    let map_count = reader.count("source map count", limits.max_code_per_prototype, 12)?;
    DecodeBudget::add(
        &mut budget.source_map_entries,
        map_count,
        "total source map entry count",
        limits.max_total_source_map_entries,
    )?;
    budget.charge_fixed(map_count, DECODED_SOURCE_MAP_CHARGE, limits)?;
    let mut source_map = Vec::new();
    try_reserve_decode(&mut source_map, "source map", map_count)?;
    for _ in 0..map_count {
        let span_source = SourceId::new(reader.u32()?);
        let offset = reader.offset;
        let start = reader.u32()?;
        let end = reader.u32()?;
        source_map.push(
            ByteSpan::new(span_source, ByteOffset::new(start), ByteOffset::new(end))
                .map_err(|_| DecodeError::InvalidSpan { offset, start, end })?,
        );
    }

    let local_count = reader.count(
        "local debug count",
        limits.max_debug_entries_per_prototype,
        14,
    )?;
    DecodeBudget::add(
        &mut budget.debug_entries,
        local_count,
        "total debug entry count",
        limits.max_total_debug_entries,
    )?;
    budget.charge_fixed(local_count, DECODED_LOCAL_DEBUG_CHARGE, limits)?;
    let mut locals = Vec::new();
    try_reserve_decode(&mut locals, "local debug entries", local_count)?;
    for _ in 0..local_count {
        let name = reader.sized_bytes("local name", limits.max_debug_name_bytes)?;
        budget.charge_debug(name.len(), limits)?;
        locals.push(LocalDebug {
            name: owned_decode_bytes("local name", name)?,
            register: reader.u16()?,
            start_pc: reader.u32()?,
            end_pc: reader.u32()?,
        });
    }
    let debug_count = reader.count(
        "upvalue debug count",
        limits.max_debug_entries_per_prototype,
        14,
    )?;
    DecodeBudget::add(
        &mut budget.debug_entries,
        debug_count,
        "total debug entry count",
        limits.max_total_debug_entries,
    )?;
    budget.charge_fixed(debug_count, DECODED_UPVALUE_DEBUG_CHARGE, limits)?;
    let mut upvalue_debug = Vec::new();
    try_reserve_decode(&mut upvalue_debug, "upvalue debug entries", debug_count)?;
    for _ in 0..debug_count {
        let name = reader.sized_bytes("upvalue name", limits.max_debug_name_bytes)?;
        budget.charge_debug(name.len(), limits)?;
        upvalue_debug.push(UpvalueDebug {
            name: owned_decode_bytes("upvalue name", name)?,
            upvalue: reader.u16()?,
            start_pc: reader.u32()?,
            end_pc: reader.u32()?,
        });
    }

    Ok(Prototype {
        profile,
        source,
        register_count,
        parameter_count,
        is_vararg,
        required_features,
        constants,
        upvalues,
        children,
        code,
        source_map,
        locals,
        upvalue_debug,
    })
}

/// BluV1 deliberately matches the established profile table.
fn profile_tag(profile: SemanticProfile) -> Result<u8, EncodeError> {
    match profile {
        SemanticProfile::Blu => Ok(1),
        SemanticProfile::Luau => Ok(2),
        SemanticProfile::Lua51 => Ok(3),
        SemanticProfile::Lua52 => Ok(4),
        SemanticProfile::Lua53 => Ok(5),
        SemanticProfile::Lua54 => Ok(6),
        SemanticProfile::Lua55 => Ok(7),
        _ => Err(EncodeError::UnsupportedProfile(profile)),
    }
}

fn tag_profile(tag: u8) -> Option<SemanticProfile> {
    match tag {
        1 => Some(SemanticProfile::Blu),
        2 => Some(SemanticProfile::Luau),
        3 => Some(SemanticProfile::Lua51),
        4 => Some(SemanticProfile::Lua52),
        5 => Some(SemanticProfile::Lua53),
        6 => Some(SemanticProfile::Lua54),
        7 => Some(SemanticProfile::Lua55),
        _ => None,
    }
}

fn try_reserve_decode<T>(
    values: &mut Vec<T>,
    what: &'static str,
    requested: usize,
) -> Result<(), DecodeError> {
    values
        .try_reserve_exact(requested)
        .map_err(|_| DecodeError::Allocation { what, requested })
}

fn owned_decode_bytes(what: &'static str, bytes: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let mut value = Vec::new();
    try_reserve_decode(&mut value, what, bytes.len())?;
    value.extend_from_slice(bytes);
    Ok(value)
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], DecodeError> {
        let remaining = self.bytes.len().saturating_sub(self.offset);
        if count > remaining {
            return Err(DecodeError::UnexpectedEnd {
                offset: self.offset,
                needed: count,
                remaining,
            });
        }
        let start = self.offset;
        self.offset += count;
        Ok(&self.bytes[start..self.offset])
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        self.take(N)?
            .try_into()
            .map_err(|_| DecodeError::UnexpectedEnd {
                offset: self.offset,
                needed: N,
                remaining: 0,
            })
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn count(
        &mut self,
        what: &'static str,
        limit: usize,
        minimum_width: usize,
    ) -> Result<usize, DecodeError> {
        self.count_with_extra(what, limit, minimum_width, 0)
    }

    fn count_with_extra(
        &mut self,
        what: &'static str,
        limit: usize,
        minimum_width: usize,
        trailing_required: usize,
    ) -> Result<usize, DecodeError> {
        let offset = self.offset;
        let actual = self.u32()? as usize;
        if actual > limit {
            return Err(DecodeError::TooLarge {
                what,
                actual,
                limit,
            });
        }
        let body = actual
            .checked_mul(minimum_width)
            .ok_or(DecodeError::CountOverflow {
                offset,
                what,
                count: actual,
                minimum_width,
            })?;
        let required = body
            .checked_add(trailing_required)
            .ok_or(DecodeError::CountOverflow {
                offset,
                what,
                count: actual,
                minimum_width,
            })?;
        let remaining = self.remaining();
        if required > remaining {
            return Err(DecodeError::DeclaredBodyTooLarge {
                offset,
                what,
                count: actual,
                minimum_width,
                required,
                remaining,
            });
        }
        Ok(actual)
    }

    fn sized_bytes(&mut self, what: &'static str, limit: usize) -> Result<&'a [u8], DecodeError> {
        let count = self.count(what, limit, 1)?;
        self.take(count)
    }

    fn text(
        &mut self,
        what: &'static str,
        limit: usize,
        budget: &mut DecodeBudget,
        limits: BluLimits,
    ) -> Result<String, DecodeError> {
        let offset = self.offset;
        let bytes = self.sized_bytes(what, limit)?;
        let value =
            core::str::from_utf8(bytes).map_err(|_| DecodeError::InvalidUtf8 { offset, what })?;
        budget.charge_owned(value.len(), limits)?;
        let mut owned = String::new();
        owned
            .try_reserve_exact(value.len())
            .map_err(|_| DecodeError::Allocation {
                what,
                requested: value.len(),
            })?;
        owned.push_str(value);
        Ok(owned)
    }
}
