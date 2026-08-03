//! Blu-owned compilation for the first dependency-gated frontend slice.
//!
//! This module is intentionally separate from the legacy crate-level
//! [`crate::Compiler`]. It emits the shared BluV1 baseline for all seven
//! established profiles, parses through `blu-syntax`, resolves local names,
//! and round-trips the result through the canonical encoder and validated
//! decoder. It never calls the native Luau compiler. Bootstrap translation
//! remains separately restricted to `blu` and `luau`; direct baseline
//! execution supports every explicit profile.
//! Locals resolve in declaration order: use before declaration is an error,
//! while a repeated name explicitly shadows the earlier binding.
//! Lua 5.3--5.5 decimal literals use an exact `Integer` constant when they fit
//! `i64`, then fall back to normal IEEE-754 parsing. Lua 5.1, Lua 5.2, and Luau
//! always use IEEE-754 parsing. Blu currently follows that Number-only
//! bootstrap policy; this is not a final Blu numeric-semantics claim.

// Keeping the bounded Diagnostic inline avoids introducing an infallible
// boxing allocation on an error path.
#![allow(clippy::result_large_err)]

use blu_bytecode::blu::{
    Artifact, BluLimits, BytecodeFormat, Constant, DecodeError, EncodeError, FeatureBits,
    Instruction, LocalDebug, Prototype, SourceRecord, Upvalue, UpvalueDebug, ValidatedArtifact,
    ValidationError, decode_validated, encode,
};
use blu_core::{
    ByteSpan, CompilerIdentity, Diagnostic, DiagnosticError, IdentityError, Phase, SemanticProfile,
    Severity, SourceFile, SourceIdentity, SpanError,
};
use blu_syntax::{
    AssignmentListStatement, AssignmentStatement, AssignmentTarget, Ast, BinaryExpression,
    BinaryOperator, CallExpression, CompoundAssignmentOperator, CompoundAssignmentStatement,
    Expression, ExpressionId, ExpressionKind, FunctionId, FunctionStatement, GenericForStatement,
    GlobalStatement, GotoStatement, Identifier, IfStatement, InterpolatedString,
    InterpolatedStringPart, LabelStatement, LocalAttribute, LocalListStatement, LocalStatement,
    MethodCallExpression, NumericForStatement, ParseError, ParseLimits, ParseOutcome, Rejected,
    RepeatStatement, ReturnStatement, Statement, TableConstructor, TableField, UnaryOperator,
    WhileStatement, parse,
};
use core::fmt;
use sha2::{Digest, Sha256};

/// Explicit limits for one owned compilation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OwnedCompileLimits {
    pub parse: ParseLimits,
    pub artifact: BluLimits,
    pub max_bindings: usize,
    pub max_registers: usize,
    pub max_constants: usize,
    pub max_instructions: usize,
    pub max_return_values: usize,
    /// Maximum decimal-token length; defaults to 256 bytes.
    pub max_integer_literal_bytes: usize,
    /// Maximum fractional or exponent-form decimal-token length.
    pub max_number_literal_bytes: usize,
}

impl Default for OwnedCompileLimits {
    fn default() -> Self {
        Self {
            parse: ParseLimits::default(),
            artifact: BluLimits::default(),
            max_bindings: 65_535,
            max_registers: 65_535,
            max_constants: 1_000_000,
            max_instructions: 8_000_000,
            max_return_values: 65_535,
            // Bounded independently from source size while permitting the
            // decimal-to-float fallback used by the supported profiles.
            max_integer_literal_bytes: 256,
            max_number_literal_bytes: 256,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnedCompileLimit {
    Bindings,
    Registers,
    Constants,
    Instructions,
    ReturnValues,
    CallArguments,
    IntegerLiteralBytes,
    NumberLiteralBytes,
    StringLiteralBytes,
    TotalConstantBytes,
    SourceNameBytes,
    DebugNameBytes,
    TotalDebugBytes,
    Prototypes,
    Children,
    Upvalues,
}

impl fmt::Display for OwnedCompileLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bindings => formatter.write_str("local bindings"),
            Self::Registers => formatter.write_str("registers"),
            Self::Constants => formatter.write_str("constants"),
            Self::Instructions => formatter.write_str("instructions"),
            Self::ReturnValues => formatter.write_str("return values"),
            Self::CallArguments => formatter.write_str("call arguments"),
            Self::IntegerLiteralBytes => formatter.write_str("integer literal bytes"),
            Self::NumberLiteralBytes => formatter.write_str("number literal bytes"),
            Self::StringLiteralBytes => formatter.write_str("string literal bytes"),
            Self::TotalConstantBytes => formatter.write_str("total constant bytes"),
            Self::SourceNameBytes => formatter.write_str("source identity name bytes"),
            Self::DebugNameBytes => formatter.write_str("local debug name bytes"),
            Self::TotalDebugBytes => formatter.write_str("total local debug name bytes"),
            Self::Prototypes => formatter.write_str("prototypes"),
            Self::Children => formatter.write_str("child prototypes"),
            Self::Upvalues => formatter.write_str("upvalues"),
        }
    }
}

/// Stateless entry point for the explicitly selected owned backend.
#[derive(Clone, Copy, Debug, Default)]
pub struct OwnedCompiler {
    limits: OwnedCompileLimits,
}

impl OwnedCompiler {
    #[must_use]
    pub const fn new(limits: OwnedCompileLimits) -> Self {
        Self { limits }
    }

    #[must_use]
    pub const fn limits(&self) -> OwnedCompileLimits {
        self.limits
    }

    /// Compile one source using only Blu-owned Rust frontend code.
    ///
    /// `compiler_identity` is consumed into the artifact, avoiding an
    /// infallible deep clone. The source identity, source bytes, and semantic
    /// profile come from explicit caller-owned contracts. Source records use
    /// SHA-256 over the exact source bytes.
    pub fn compile(
        &self,
        source: &SourceFile,
        profile: SemanticProfile,
        compiler_identity: CompilerIdentity,
    ) -> Result<OwnedCompilation, OwnedCompileError> {
        match profile {
            SemanticProfile::Blu
            | SemanticProfile::Luau
            | SemanticProfile::Lua51
            | SemanticProfile::Lua52
            | SemanticProfile::Lua53
            | SemanticProfile::Lua54
            | SemanticProfile::Lua55 => {}
            _ => return Err(OwnedCompileError::UnsupportedProfile(profile)),
        }
        check_limit(
            OwnedCompileLimit::SourceNameBytes,
            source.identity().name().len(),
            self.limits.artifact.identity.max_source_name_bytes,
        )?;

        let parsed = match parse(source, profile, self.limits.parse)? {
            ParseOutcome::Accepted(parsed) => parsed,
            ParseOutcome::Rejected(rejected) => {
                return Err(OwnedCompileError::Syntax(rejected));
            }
        };
        let mut prototypes = allocate_vec(1, "artifact prototypes")?;
        let prototype = Lowerer::new(
            source,
            parsed.ast(),
            self.limits,
            &mut prototypes,
            &[],
            FunctionShape {
                is_vararg: matches!(
                    profile,
                    SemanticProfile::Blu
                        | SemanticProfile::Luau
                        | SemanticProfile::Lua51
                        | SemanticProfile::Lua52
                        | SemanticProfile::Lua53
                        | SemanticProfile::Lua54
                        | SemanticProfile::Lua55
                ),
                ..FunctionShape::default()
            },
            &[],
            None,
        )?
        .run(parsed.ast().statements())?;
        let main =
            u32::try_from(prototypes.len()).map_err(|_| OwnedCompileError::InternalInvariant {
                message: "prototype count passed limits but cannot fit BluV1",
            })?;
        push_fallible(&mut prototypes, prototype, "artifact prototypes")?;
        let source_name = copy_string(source.identity().name(), "source identity name")?;
        let source_identity = SourceIdentity::new(
            source.identity().id(),
            source_name,
            self.limits.artifact.identity,
        )
        .map_err(OwnedCompileError::Identity)?;
        let byte_len =
            u32::try_from(source.len()).map_err(|_| OwnedCompileError::InternalInvariant {
                message: "SourceFile length is not representable by its byte-span contract",
            })?;
        let digest: [u8; 32] = Sha256::digest(source.bytes()).into();

        let mut sources = allocate_vec(1, "artifact sources")?;
        sources.push(SourceRecord {
            identity: source_identity,
            byte_len,
            digest,
        });
        let artifact = Artifact {
            format: BytecodeFormat::BluV2,
            compiler: compiler_identity,
            sources,
            prototypes,
            main,
        };
        let validated = ValidatedArtifact::new(artifact, self.limits.artifact)
            .map_err(OwnedCompileError::Validation)?;
        let bytes = encode(&validated, self.limits.artifact).map_err(OwnedCompileError::Encode)?;

        // Return the canonical decoded representation. Dropping the original
        // first keeps the round trip fallible without retaining two artifacts.
        drop(validated);
        let artifact =
            decode_validated(&bytes, self.limits.artifact).map_err(OwnedCompileError::Decode)?;
        Ok(OwnedCompilation { bytes, artifact })
    }
}

/// Canonically encoded bytes and their validated decoded BluV1 artifact.
///
/// This owning result intentionally does not implement `Clone`. Callers can
/// borrow either representation or consume the result.
#[derive(Debug, PartialEq)]
pub struct OwnedCompilation {
    bytes: Vec<u8>,
    artifact: ValidatedArtifact,
}

impl OwnedCompilation {
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub const fn artifact(&self) -> &ValidatedArtifact {
        &self.artifact
    }

    #[must_use]
    pub fn into_validated_artifact(self) -> ValidatedArtifact {
        self.artifact
    }

    #[must_use]
    pub fn into_parts(self) -> (Vec<u8>, ValidatedArtifact) {
        (self.bytes, self.artifact)
    }
}

#[derive(Debug)]
pub enum OwnedCompileError {
    UnsupportedProfile(SemanticProfile),
    Parse(ParseError),
    Syntax(Rejected),
    Limit {
        kind: OwnedCompileLimit,
        required: usize,
        limit: usize,
    },
    Allocation {
        what: &'static str,
        requested: usize,
    },
    Diagnostic(Diagnostic),
    DiagnosticConstruction(DiagnosticError),
    Identity(IdentityError),
    Span(SpanError),
    Validation(ValidationError),
    Encode(EncodeError),
    Decode(DecodeError),
    InternalInvariant {
        message: &'static str,
    },
    ControlFlow {
        message: String,
    },
}

impl OwnedCompileError {
    #[must_use]
    pub const fn syntax(&self) -> Option<&Rejected> {
        match self {
            Self::Syntax(rejected) => Some(rejected),
            _ => None,
        }
    }

    #[must_use]
    pub const fn diagnostic(&self) -> Option<&Diagnostic> {
        match self {
            Self::Diagnostic(diagnostic) => Some(diagnostic),
            _ => None,
        }
    }
}

impl fmt::Display for OwnedCompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedProfile(profile) => {
                write!(formatter, "owned compiler profile {profile} is unsupported")
            }
            Self::Parse(error) => error.fmt(formatter),
            Self::Syntax(rejected) => {
                if let Some(diagnostic) = rejected.diagnostics().first() {
                    write!(formatter, "source rejected with {}", diagnostic.code())
                } else {
                    formatter.write_str("source rejected without a diagnostic")
                }
            }
            Self::Limit {
                kind,
                required,
                limit,
            } => write!(
                formatter,
                "{kind} require {required}, exceeding compiler limit {limit}"
            ),
            Self::Allocation { what, requested } => {
                write!(formatter, "failed to allocate {requested} {what}")
            }
            Self::Diagnostic(diagnostic) => write!(
                formatter,
                "{}: {}",
                diagnostic.code(),
                diagnostic.primary().message()
            ),
            Self::DiagnosticConstruction(error) => error.fmt(formatter),
            Self::Identity(error) => error.fmt(formatter),
            Self::Span(error) => error.fmt(formatter),
            Self::Validation(error) => error.fmt(formatter),
            Self::Encode(error) => error.fmt(formatter),
            Self::Decode(error) => error.fmt(formatter),
            Self::InternalInvariant { message } => {
                write!(formatter, "owned compiler invariant failed: {message}")
            }
            Self::ControlFlow { message } => formatter.write_str(message),
        }
    }
}

impl std::error::Error for OwnedCompileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Parse(error) => Some(error),
            Self::DiagnosticConstruction(error) => Some(error),
            Self::Identity(error) => Some(error),
            Self::Span(error) => Some(error),
            Self::Validation(error) => Some(error),
            Self::Encode(error) => Some(error),
            Self::Decode(error) => Some(error),
            Self::UnsupportedProfile(_)
            | Self::Syntax(_)
            | Self::Limit { .. }
            | Self::Allocation { .. }
            | Self::Diagnostic(_)
            | Self::InternalInvariant { .. }
            | Self::ControlFlow { .. } => None,
        }
    }
}

impl From<ParseError> for OwnedCompileError {
    fn from(error: ParseError) -> Self {
        Self::Parse(error)
    }
}

impl From<SpanError> for OwnedCompileError {
    fn from(error: SpanError) -> Self {
        Self::Span(error)
    }
}

impl From<DiagnosticError> for OwnedCompileError {
    fn from(error: DiagnosticError) -> Self {
        Self::DiagnosticConstruction(error)
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum BindingName {
    Source(ByteSpan),
    Global(ByteSpan),
    GlobalWildcard,
    GlobalDefault,
    ImplicitSelf,
    ImplicitEnvironment,
}

impl BindingName {
    fn matches(self, source: &SourceFile, name: ByteSpan) -> Result<bool, OwnedCompileError> {
        let expected = match self {
            Self::Source(span) => source.slice(span)?,
            Self::Global(span) => source.slice(span)?,
            Self::GlobalWildcard | Self::GlobalDefault => return Ok(false),
            Self::ImplicitSelf => b"self",
            Self::ImplicitEnvironment => b"_ENV",
        };
        Ok(expected == source.slice(name)?)
    }

    fn bytes(self, source: &SourceFile) -> Result<&[u8], OwnedCompileError> {
        match self {
            Self::Source(span) => Ok(source.slice(span)?),
            Self::Global(span) => Ok(source.slice(span)?),
            Self::GlobalWildcard | Self::GlobalDefault => Ok(b""),
            Self::ImplicitSelf => Ok(b"self"),
            Self::ImplicitEnvironment => Ok(b"_ENV"),
        }
    }

    fn is_hidden(self) -> bool {
        matches!(
            self,
            Self::Global(_)
                | Self::GlobalWildcard
                | Self::GlobalDefault
                | Self::ImplicitEnvironment
        )
    }

    fn is_global(self) -> bool {
        matches!(
            self,
            Self::Global(_) | Self::GlobalWildcard | Self::GlobalDefault
        )
    }
}

#[derive(Clone, Copy)]
struct Binding {
    name: BindingName,
    register: u16,
    constant: bool,
    to_close: bool,
    start_pc: u32,
    end_pc: Option<u32>,
}

const GLOBAL_BINDING_REGISTER: u16 = u16::MAX;
const LUAU_OWNED_NAMECALL_MARKER: u8 = 0;

#[derive(Clone, Copy)]
struct Label {
    name: ByteSpan,
    target: usize,
    scope: usize,
    block: usize,
}

#[derive(Clone, Copy)]
struct Goto {
    name: ByteSpan,
    instruction: usize,
    scope: usize,
    block: usize,
    global_scope: usize,
}

#[derive(Clone, Copy)]
struct PlannedLabel {
    name: ByteSpan,
    scope: usize,
    block: usize,
    local_name: Option<ByteSpan>,
    global_scope: usize,
    global_name: Option<ByteSpan>,
    terminal_scope_close: bool,
}

#[derive(Clone, Copy)]
struct PlannedGoto {
    name: ByteSpan,
    block: usize,
    global_scope: usize,
}

#[derive(Clone, Copy)]
struct PlannedBlock {
    pointer: usize,
    length: usize,
    parent: Option<usize>,
}

#[derive(Clone, Copy)]
struct OuterBinding {
    name: BindingName,
    constant: bool,
    source: Upvalue,
}

#[derive(Clone, Copy)]
enum AssignmentDestination {
    Local(u16),
    Upvalue(u16),
    Global(u32),
    Table { table: u16, key: u16 },
}

#[derive(Clone, Copy, Default)]
struct FunctionShape {
    implicit_self: bool,
    is_vararg: bool,
    vararg_name: Option<ByteSpan>,
}

struct Lowerer<'a, 'prototypes> {
    source: &'a SourceFile,
    ast: &'a Ast,
    profile: SemanticProfile,
    expressions: &'a [Expression],
    table_fields: &'a [TableField],
    interpolated_parts: &'a [InterpolatedStringPart],
    call_arguments: &'a [ExpressionId],
    limits: OwnedCompileLimits,
    prototypes: &'prototypes mut Vec<Prototype>,
    outer_bindings: Vec<OuterBinding>,
    upvalues: Vec<OuterBinding>,
    parameter_count: usize,
    is_vararg: bool,
    bindings: Vec<Binding>,
    closed_bindings: Vec<Binding>,
    loop_breaks: Vec<Vec<usize>>,
    loop_continues: Vec<Vec<usize>>,
    labels: Vec<Label>,
    gotos: Vec<Goto>,
    planned_labels: Vec<PlannedLabel>,
    planned_gotos: Vec<PlannedGoto>,
    planned_blocks: Vec<PlannedBlock>,
    active_block: Option<usize>,
    register_count: usize,
    constants: Vec<Constant>,
    constant_bytes: usize,
    code: Vec<Instruction>,
    source_map: Vec<ByteSpan>,
    line_defined: u32,
    last_line_defined: u32,
    implicit_return_span: Option<ByteSpan>,
    children: Vec<u32>,
    uses_environment: bool,
    implicit_environment: bool,
}

impl<'a, 'prototypes> Lowerer<'a, 'prototypes> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        source: &'a SourceFile,
        ast: &'a Ast,
        limits: OwnedCompileLimits,
        prototypes: &'prototypes mut Vec<Prototype>,
        parameters: &[Identifier],
        shape: FunctionShape,
        outer_bindings: &[OuterBinding],
        definition_span: Option<ByteSpan>,
    ) -> Result<Self, OwnedCompileError> {
        let capacity = ast.node_count().min(4_096);
        let mut copied_outer_bindings =
            allocate_vec(outer_bindings.len(), "outer lexical bindings")?;
        copied_outer_bindings.extend_from_slice(outer_bindings);
        let (line_defined, last_line_defined) = match definition_span {
            Some(span) => {
                let start = owned_source_line(source, ast.profile(), span.start().as_usize())?;
                let end_offset = span.end().as_usize().saturating_sub(1);
                let end = owned_source_line(source, ast.profile(), end_offset)?;
                (
                    start
                        .checked_add(1)
                        .ok_or(OwnedCompileError::InternalInvariant {
                            message: "source line number cannot fit BluV2 metadata",
                        })?,
                    end.checked_add(1)
                        .ok_or(OwnedCompileError::InternalInvariant {
                            message: "source line number cannot fit BluV2 metadata",
                        })?,
                )
            }
            None => (0, 0),
        };
        let implicit_return_span = definition_span
            .map(|span| {
                let end = span.end().as_usize();
                let offset = end.saturating_sub(1);
                source.span(offset, offset.saturating_add(1)).map_err(|_| {
                    OwnedCompileError::InternalInvariant {
                        message: "function implicit return span became invalid",
                    }
                })
            })
            .transpose()?;
        let mut lowerer = Self {
            source,
            ast,
            profile: ast.profile(),
            expressions: ast.expressions(),
            table_fields: ast.table_field_arena(),
            interpolated_parts: ast.interpolated_string_part_arena(),
            call_arguments: ast.call_argument_arena(),
            limits,
            prototypes,
            outer_bindings: copied_outer_bindings,
            upvalues: allocate_vec(4, "prototype upvalues")?,
            parameter_count: parameters
                .len()
                .saturating_add(usize::from(shape.implicit_self)),
            is_vararg: shape.is_vararg,
            bindings: allocate_vec(capacity.min(limits.max_bindings), "local bindings")?,
            closed_bindings: allocate_vec(
                capacity.min(limits.max_bindings),
                "closed local bindings",
            )?,
            loop_breaks: allocate_vec(8, "loop control stack")?,
            loop_continues: allocate_vec(8, "loop continue stack")?,
            labels: allocate_vec(8, "labels")?,
            gotos: allocate_vec(8, "goto branches")?,
            planned_labels: allocate_vec(8, "planned labels")?,
            planned_gotos: allocate_vec(8, "planned gotos")?,
            planned_blocks: allocate_vec(8, "planned blocks")?,
            active_block: None,
            register_count: 0,
            constants: allocate_vec(capacity.min(limits.max_constants), "constants")?,
            constant_bytes: 0,
            code: allocate_vec(capacity.min(limits.max_instructions), "instructions")?,
            source_map: allocate_vec(capacity.min(limits.max_instructions), "source map")?,
            line_defined,
            last_line_defined,
            implicit_return_span,
            children: allocate_vec(4, "prototype children")?,
            uses_environment: false,
            implicit_environment: false,
        };
        if shape.implicit_self {
            let register = lowerer.allocate_register()?;
            lowerer.push_binding(BindingName::ImplicitSelf, register, 0)?;
        }
        if outer_bindings.is_empty()
            && matches!(
                lowerer.profile,
                SemanticProfile::Lua52
                    | SemanticProfile::Lua53
                    | SemanticProfile::Lua54
                    | SemanticProfile::Lua55
            )
        {
            let register = lowerer.allocate_register()?;
            lowerer.implicit_environment = true;
            lowerer.push_binding(BindingName::ImplicitEnvironment, register, 0)?;
            lowerer.emit(
                Instruction::NewTable {
                    destination: register,
                },
                source.span(0, 0)?,
            )?;
        }
        if outer_bindings.is_empty()
            && matches!(
                lowerer.profile,
                SemanticProfile::Blu | SemanticProfile::Lua55
            )
        {
            lowerer.push_binding(BindingName::GlobalDefault, GLOBAL_BINDING_REGISTER, 0)?;
        }
        for parameter in parameters {
            let register = lowerer.allocate_register()?;
            lowerer.push_binding(BindingName::Source(parameter.span()), register, 0)?;
        }
        if let Some(vararg_name) = shape.vararg_name {
            let table = lowerer.allocate_register()?;
            lowerer.emit(Instruction::NewTable { destination: table }, vararg_name)?;
            lowerer.emit(Instruction::SetListVarargs { table, start: 0 }, vararg_name)?;
            lowerer.push_binding_with_flags(
                BindingName::Source(vararg_name),
                table,
                0,
                true,
                false,
            )?;
        }
        Ok(lowerer)
    }

    fn run(mut self, statements: &[Statement]) -> Result<Prototype, OwnedCompileError> {
        self.plan_labels(statements, self.local_binding_depth(), 0, None, true)?;
        if !self.lower_block_statements(statements)? {
            let return_span = self
                .implicit_return_span
                .unwrap_or(self.source.span(self.source.len(), self.source.len())?);
            self.emit_close_bindings(0, return_span)?;
            self.emit(Instruction::Return { first: 0, count: 0 }, return_span)?;
        }
        self.resolve_gotos()?;

        let mut line_info = allocate_vec(self.source_map.len(), "line info")?;
        for span in &self.source_map {
            let line = if span.is_empty() && span.start().as_usize() == 0 {
                // Lua's implicit environment setup is not guest source and
                // must not produce a line-hook event at line 1.
                0
            } else {
                let start = span.start().as_usize();
                let offset = if start == self.source.len() && start != 0 {
                    self.source
                        .bytes()
                        .iter()
                        .rposition(|byte| !byte.is_ascii_whitespace())
                        .unwrap_or(start - 1)
                } else {
                    start
                };
                owned_source_line(self.source, self.profile, offset)?
                    .checked_add(1)
                    .ok_or(OwnedCompileError::InternalInvariant {
                        message: "instruction source line cannot fit BluV2 metadata",
                    })?
            };
            line_info.push(line);
        }

        let end_pc =
            u32::try_from(self.code.len()).map_err(|_| OwnedCompileError::InternalInvariant {
                message: "instruction count passed limits but cannot fit a debug PC",
            })?;
        let mut debug_bytes = 0_usize;
        for binding in self
            .closed_bindings
            .iter()
            .chain(&self.bindings)
            .filter(|binding| !binding.name.is_hidden())
        {
            let name_len = binding.name.bytes(self.source)?.len();
            check_limit(
                OwnedCompileLimit::DebugNameBytes,
                name_len,
                self.limits.artifact.max_debug_name_bytes,
            )?;
            debug_bytes = debug_bytes
                .checked_add(name_len)
                .ok_or(OwnedCompileError::Limit {
                    kind: OwnedCompileLimit::TotalDebugBytes,
                    required: usize::MAX,
                    limit: self.limits.artifact.max_total_debug_bytes,
                })?;
            check_limit(
                OwnedCompileLimit::TotalDebugBytes,
                debug_bytes,
                self.limits.artifact.max_total_debug_bytes,
            )?;
        }
        for binding in &self.upvalues {
            let name_len = binding.name.bytes(self.source)?.len();
            check_limit(
                OwnedCompileLimit::DebugNameBytes,
                name_len,
                self.limits.artifact.max_debug_name_bytes,
            )?;
            debug_bytes = debug_bytes
                .checked_add(name_len)
                .ok_or(OwnedCompileError::Limit {
                    kind: OwnedCompileLimit::TotalDebugBytes,
                    required: usize::MAX,
                    limit: self.limits.artifact.max_total_debug_bytes,
                })?;
            check_limit(
                OwnedCompileLimit::TotalDebugBytes,
                debug_bytes,
                self.limits.artifact.max_total_debug_bytes,
            )?;
        }
        let binding_count = self
            .closed_bindings
            .iter()
            .chain(&self.bindings)
            .filter(|binding| !binding.name.is_hidden())
            .count();
        let mut locals = allocate_vec(binding_count, "local debug entries")?;
        for binding in self
            .closed_bindings
            .into_iter()
            .chain(self.bindings)
            .filter(|binding| !binding.name.is_hidden())
        {
            let end_pc = binding.end_pc.unwrap_or(end_pc);
            if binding.start_pc >= end_pc {
                continue;
            }
            let name = copy_bytes(binding.name.bytes(self.source)?, "local debug name")?;
            locals.push(LocalDebug {
                name,
                register: binding.register,
                start_pc: binding.start_pc,
                end_pc,
            });
        }
        locals.sort_by_key(|local| local.start_pc);
        let mut upvalue_debug = allocate_vec(self.upvalues.len(), "upvalue debug entries")?;
        for (index, binding) in self.upvalues.iter().copied().enumerate() {
            let name = copy_bytes(binding.name.bytes(self.source)?, "upvalue debug name")?;
            let upvalue =
                u16::try_from(index).map_err(|_| OwnedCompileError::InternalInvariant {
                    message: "upvalue count passed limits but cannot fit BluV1",
                })?;
            upvalue_debug.push(UpvalueDebug {
                name,
                upvalue,
                start_pc: 0,
                end_pc,
            });
        }

        let register_count = u16::try_from(self.register_count).map_err(|_| {
            OwnedCompileError::InternalInvariant {
                message: "register count passed limits but cannot fit BluV1",
            }
        })?;
        let mut upvalues = allocate_vec(self.upvalues.len(), "prototype upvalues")?;
        upvalues.extend(self.upvalues.iter().map(|binding| binding.source));
        let mut required_features = FeatureBits::BASELINE;
        if self
            .constants
            .iter()
            .any(|constant| matches!(constant, Constant::Integer(_)))
        {
            required_features = required_features | FeatureBits::INTEGER_CONSTANTS;
        }
        if self
            .code
            .iter()
            .any(|instruction| matches!(instruction, Instruction::FloorDivide { .. }))
        {
            required_features = required_features | FeatureBits::FLOOR_DIVISION;
        }
        if self
            .code
            .iter()
            .any(|instruction| matches!(instruction, Instruction::Concatenate { .. }))
        {
            required_features = required_features | FeatureBits::CONCATENATION;
        }
        if self.code.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::Equal { .. }
                    | Instruction::LessThan { .. }
                    | Instruction::LessEqual { .. }
            )
        }) {
            required_features = required_features | FeatureBits::COMPARISONS;
        }
        if self.code.iter().any(|instruction| match instruction {
            Instruction::JumpIfTruthy { .. } | Instruction::JumpIfFalsy { .. } => true,
            Instruction::Jump { target } => *target as usize > 0,
            _ => false,
        }) {
            required_features = required_features | FeatureBits::FORWARD_BRANCHES;
        }
        if self.code.iter().enumerate().any(|(pc, instruction)| {
            matches!(
                instruction,
                Instruction::Jump { target } if (*target as usize) <= pc
            )
        }) {
            required_features = required_features | FeatureBits::BACKWARD_BRANCHES;
        }
        if self.code.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::LoadGlobal { .. } | Instruction::StoreGlobal { .. }
            )
        }) || self.uses_environment
        {
            required_features = required_features | FeatureBits::GLOBALS;
        }
        if self.implicit_environment {
            required_features = required_features | FeatureBits::IMPLICIT_ENVIRONMENT;
        }
        if self.code.iter().enumerate().any(|(pc, instruction)| {
            matches!(
                instruction,
                Instruction::GetTable { .. }
                    | Instruction::SetTable { .. }
                    | Instruction::SetListVarargs { .. }
                    | Instruction::SetListCall { .. }
                    | Instruction::SetListCallVarargs { .. }
                    | Instruction::SetListCallDynamic { .. }
            ) || (matches!(instruction, Instruction::NewTable { .. })
                && !(pc == 0
                    && matches!(
                        self.profile,
                        SemanticProfile::Lua52
                            | SemanticProfile::Lua53
                            | SemanticProfile::Lua54
                            | SemanticProfile::Lua55
                    )))
        }) {
            required_features = required_features | FeatureBits::TABLES;
        }
        if self
            .code
            .iter()
            .any(|instruction| matches!(instruction, Instruction::Call { .. }))
        {
            required_features = required_features | FeatureBits::FIXED_CALLS;
        }
        if self.code.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::CallResults { .. }
                    | Instruction::CallVarargsResults { .. }
                    | Instruction::CallDynamicResults { .. }
                    | Instruction::CallDynamicAllResults { .. }
            )
        }) {
            required_features = required_features | FeatureBits::FIXED_MULTI_RESULTS;
        }
        if self.code.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::ReturnCall { .. }
                    | Instruction::ReturnCallPrefix { .. }
                    | Instruction::ReturnCallVarargs { .. }
                    | Instruction::ReturnCallVarargsPrefix { .. }
                    | Instruction::ReturnCallDynamic { .. }
                    | Instruction::ReturnCallDynamicPrefix { .. }
            )
        }) {
            required_features = required_features | FeatureBits::RETURN_CALLS;
        }
        if self.code.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::NewClosure { .. }
                    | Instruction::GetUpvalue { .. }
                    | Instruction::SetUpvalue { .. }
                    | Instruction::CloseUpvalues { .. }
            )
        }) {
            required_features = required_features | FeatureBits::CLOSURES;
        }
        if self.code.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::Varargs { .. }
                    | Instruction::ReturnVarargs { .. }
                    | Instruction::CallVarargsResults { .. }
                    | Instruction::CallVarargsAllResults { .. }
                    | Instruction::ReturnCallVarargs { .. }
                    | Instruction::ReturnCallVarargsPrefix { .. }
                    | Instruction::SetListVarargs { .. }
                    | Instruction::SetListCallVarargs { .. }
            )
        }) {
            required_features = required_features | FeatureBits::VARARGS;
        }
        if self.code.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::SetListCall { .. }
                    | Instruction::SetListCallVarargs { .. }
                    | Instruction::SetListCallDynamic { .. }
                    | Instruction::CallAllResults { .. }
                    | Instruction::CallVarargsAllResults { .. }
                    | Instruction::CallDynamicResults { .. }
                    | Instruction::CallDynamicAllResults { .. }
                    | Instruction::ReturnCallDynamic { .. }
                    | Instruction::ReturnCallDynamicPrefix { .. }
            )
        }) {
            required_features = required_features | FeatureBits::DYNAMIC_CALL_RESULTS;
        }
        if self.code.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::BitwiseAnd { .. }
                    | Instruction::BitwiseOr { .. }
                    | Instruction::BitwiseExclusiveOr { .. }
                    | Instruction::ShiftLeft { .. }
                    | Instruction::ShiftRight { .. }
                    | Instruction::BitwiseNot { .. }
            )
        }) {
            required_features = required_features | FeatureBits::BITWISE_OPERATORS;
        }
        Ok(Prototype {
            profile: self.profile,
            source: self.source.identity().id(),
            register_count,
            parameter_count: u16::try_from(self.parameter_count).map_err(|_| {
                OwnedCompileError::InternalInvariant {
                    message: "parameter count passed limits but cannot fit BluV1",
                }
            })?,
            is_vararg: self.is_vararg,
            required_features,
            constants: self.constants,
            upvalues,
            children: self.children,
            code: self.code,
            source_map: self.source_map,
            line_info,
            line_defined: self.line_defined,
            last_line_defined: self.last_line_defined,
            locals,
            upvalue_debug,
        })
    }

    fn lower_block_statements(
        &mut self,
        statements: &[Statement],
    ) -> Result<bool, OwnedCompileError> {
        let pointer = statements.as_ptr() as usize;
        let length = statements.len();
        let block = self
            .planned_blocks
            .iter()
            .position(|planned| planned.pointer == pointer && planned.length == length)
            .ok_or(OwnedCompileError::InternalInvariant {
                message: "statement block was not planned before lowering",
            })?;
        let previous = self.active_block.replace(block);
        let result = self.lower_statements(statements);
        self.active_block = previous;
        result
    }

    fn lower_statements(&mut self, statements: &[Statement]) -> Result<bool, OwnedCompileError> {
        let mut path_reachable = true;
        let mut index = 0;
        while index < statements.len() {
            if !path_reachable {
                let Some(offset) = statements[index..].iter().position(|statement| {
                    let Statement::Label(label) = statement else {
                        return false;
                    };
                    self.label_has_planned_goto(label.name().span())
                }) else {
                    return Ok(true);
                };
                path_reachable = statements[index..].iter().any(|statement| {
                    let Statement::Label(label) = statement else {
                        return false;
                    };
                    self.label_has_emitted_goto(label.name().span())
                });
                index = index.saturating_add(offset);
            }
            let statement = &statements[index];
            let terminated = match statement {
                Statement::Global(global) => {
                    self.lower_global(global)?;
                    false
                }
                Statement::Local(local) => {
                    self.lower_local(*local)?;
                    false
                }
                Statement::LocalFunction(function) => {
                    let destination = self.allocate_register()?;
                    let nil = self.push_constant(Constant::Nil)?;
                    let hidden_span = self.source.span(0, 0)?;
                    self.emit(
                        Instruction::LoadConstant {
                            destination,
                            constant: nil,
                        },
                        hidden_span,
                    )?;
                    let start_pc = u32::try_from(self.code.len()).map_err(|_| {
                        OwnedCompileError::InternalInvariant {
                            message: "instruction count passed limits but cannot fit a debug PC",
                        }
                    })?;
                    self.push_binding(
                        BindingName::Source(function.name().span()),
                        destination,
                        start_pc,
                    )?;
                    let closure =
                        self.lower_function(function.function(), function.span(), false)?;
                    self.emit(
                        Instruction::Move {
                            destination,
                            source: closure,
                        },
                        hidden_span,
                    )?;
                    false
                }
                Statement::Function(function) => {
                    self.lower_function_statement(function)?;
                    false
                }
                Statement::LocalList(local) => {
                    self.lower_local_list(local)?;
                    false
                }
                Statement::Assignment(assignment) => {
                    self.lower_assignment(*assignment)?;
                    false
                }
                Statement::CompoundAssignment(assignment) => {
                    self.lower_compound_assignment(*assignment)?;
                    false
                }
                Statement::AssignmentList(assignment) => {
                    self.lower_assignment_list(assignment)?;
                    false
                }
                Statement::Call(statement) => {
                    self.lower_expression(statement.call())?;
                    false
                }
                Statement::If(statement) => self.lower_if(statement)?,
                Statement::While(statement) => {
                    self.lower_while(statement)?;
                    false
                }
                Statement::Repeat(statement) => {
                    self.lower_repeat(statement)?;
                    false
                }
                Statement::Do(statement) => {
                    let scope = self.bindings.len();
                    let terminated = self.lower_block_statements(statement.body().statements())?;
                    let ends_in_goto = statement
                        .body()
                        .statements()
                        .last()
                        .is_some_and(|statement| matches!(statement, Statement::Goto(_)));
                    if !terminated && !ends_in_goto {
                        self.emit_close_bindings(scope, statement.span())?;
                        self.emit_close_upvalues(scope, statement.span())?;
                    }
                    self.close_scope(scope)?;
                    terminated
                }
                Statement::NumericFor(statement) => {
                    self.lower_numeric_for(statement)?;
                    false
                }
                Statement::GenericFor(statement) => {
                    self.lower_generic_for(statement)?;
                    false
                }
                Statement::Break(statement) => {
                    self.lower_break(statement.span())?;
                    true
                }
                Statement::Continue(statement) => {
                    self.lower_continue(statement.span())?;
                    true
                }
                Statement::Label(statement) => {
                    self.lower_label(statement)?;
                    false
                }
                Statement::Goto(statement) => {
                    self.lower_goto(statement)?;
                    true
                }
                Statement::Return(return_statement) => {
                    self.lower_return(return_statement)?;
                    true
                }
            };
            if path_reachable {
                path_reachable = !terminated;
            }
            index = index.saturating_add(1);
        }
        Ok(!path_reachable)
    }

    fn lower_if(&mut self, statement: &IfStatement) -> Result<bool, OwnedCompileError> {
        let mut end_jumps = allocate_vec(statement.clauses().len(), "if end branches")?;
        let mut all_clauses_terminate = true;
        for clause in statement.clauses() {
            let condition_span = self.expression(clause.condition())?.span();
            let branch_span = self.debug_branch_span(condition_span, clause.body().statements())?;
            let end_jump_span = clause
                .body()
                .statements()
                .last()
                .map_or(branch_span, Statement::span);
            let condition = self.lower_expression(clause.condition())?;
            let false_branch = self.code.len();
            self.emit(
                Instruction::JumpIfFalsy {
                    condition,
                    target: 0,
                },
                branch_span,
            )?;
            let scope = self.bindings.len();
            let terminated = self.lower_block_statements(clause.body().statements())?
                || clause
                    .body()
                    .statements()
                    .last()
                    .is_some_and(|statement| matches!(statement, Statement::Goto(_)));
            if !terminated {
                self.emit_close_bindings(scope, clause.span())?;
                self.emit_close_upvalues(scope, clause.span())?;
            }
            self.close_scope(scope)?;
            all_clauses_terminate &= terminated;
            if !terminated {
                let branch = self.code.len();
                self.emit(Instruction::Jump { target: 0 }, end_jump_span)?;
                push_fallible(&mut end_jumps, branch, "if end branches")?;
            }
            self.patch_forward_branch(false_branch, self.code.len())?;
        }

        let else_terminates = if let Some(body) = statement.else_body() {
            let scope = self.bindings.len();
            let terminated = self.lower_block_statements(body.statements())?;
            if !terminated {
                self.emit_close_bindings(scope, statement.span())?;
                self.emit_close_upvalues(scope, statement.span())?;
            }
            self.close_scope(scope)?;
            terminated
        } else {
            false
        };
        let end = self.code.len();
        for branch in end_jumps {
            self.patch_forward_branch(branch, end)?;
        }
        Ok(all_clauses_terminate && else_terminates)
    }

    fn lower_while(&mut self, statement: &WhileStatement) -> Result<(), OwnedCompileError> {
        let start = self.code.len();
        let condition = self.lower_expression(statement.condition())?;
        let exit = self.code.len();
        self.emit(
            Instruction::JumpIfFalsy {
                condition,
                target: 0,
            },
            statement.span(),
        )?;
        let scope = self.bindings.len();
        push_fallible(
            &mut self.loop_breaks,
            allocate_vec(2, "loop break branches")?,
            "loop control stack",
        )?;
        push_fallible(
            &mut self.loop_continues,
            allocate_vec(2, "loop continue branches")?,
            "loop continue stack",
        )?;
        let lowered = self.lower_block_statements(statement.body().statements());
        let continues = self
            .loop_continues
            .pop()
            .ok_or(OwnedCompileError::InternalInvariant {
                message: "loop continue stack became empty during lowering",
            })?;
        let breaks = self
            .loop_breaks
            .pop()
            .ok_or(OwnedCompileError::InternalInvariant {
                message: "loop control stack became empty during lowering",
            })?;
        let terminated = lowered?
            || statement
                .body()
                .statements()
                .last()
                .is_some_and(|statement| matches!(statement, Statement::Goto(_)));
        let needs_loop_back = !terminated || !continues.is_empty();
        let continue_target = self.code.len();
        let loop_back_span = statement
            .body()
            .statements()
            .last()
            .map_or(statement.span(), Statement::span);
        for branch in continues {
            self.patch_forward_branch(branch, continue_target)?;
        }
        if needs_loop_back {
            self.emit_close_bindings(scope, statement.span())?;
            self.emit_close_upvalues(scope, statement.span())?;
            let target =
                u32::try_from(start).map_err(|_| OwnedCompileError::InternalInvariant {
                    message: "loop target passed limits but cannot fit BluV1",
                })?;
            self.emit(Instruction::Jump { target }, loop_back_span)?;
        }
        if !breaks.is_empty() {
            let break_target = self.code.len();
            for branch in breaks {
                self.patch_forward_branch(branch, break_target)?;
            }
            self.emit_close_bindings(scope, statement.span())?;
            self.emit_close_upvalues(scope, statement.span())?;
        }
        self.close_scope(scope)?;
        let end = self.code.len();
        self.patch_forward_branch(exit, end)?;
        Ok(())
    }

    fn lower_repeat(&mut self, statement: &RepeatStatement) -> Result<(), OwnedCompileError> {
        let start = self.code.len();
        let scope = self.bindings.len();
        push_fallible(
            &mut self.loop_breaks,
            allocate_vec(2, "loop break branches")?,
            "loop control stack",
        )?;
        push_fallible(
            &mut self.loop_continues,
            allocate_vec(2, "loop continue branches")?,
            "loop continue stack",
        )?;
        let lowered = self.lower_block_statements(statement.body().statements());
        let continues = self
            .loop_continues
            .pop()
            .ok_or(OwnedCompileError::InternalInvariant {
                message: "loop continue stack became empty during lowering",
            })?;
        let breaks = self
            .loop_breaks
            .pop()
            .ok_or(OwnedCompileError::InternalInvariant {
                message: "loop control stack became empty during lowering",
            })?;
        let terminated = lowered?
            || statement
                .body()
                .statements()
                .last()
                .is_some_and(|statement| matches!(statement, Statement::Goto(_)));
        let needs_condition = !terminated || !continues.is_empty();
        if needs_condition {
            let condition_start = self.code.len();
            for branch in continues {
                self.patch_forward_branch(branch, condition_start)?;
            }
            let condition_span = self.expression(statement.condition())?.span();
            let condition = self.lower_expression(statement.condition())?;
            let exit_branch = self.code.len();
            self.emit(
                Instruction::JumpIfTruthy {
                    condition,
                    target: 0,
                },
                condition_span,
            )?;
            self.emit_close_bindings(scope, statement.span())?;
            self.emit_close_upvalues(scope, statement.span())?;
            let target =
                u32::try_from(start).map_err(|_| OwnedCompileError::InternalInvariant {
                    message: "loop target passed limits but cannot fit BluV1",
                })?;
            self.emit(Instruction::Jump { target }, condition_span)?;
            let exit_target = self.code.len();
            self.patch_forward_branch(exit_branch, exit_target)?;
            for branch in breaks {
                self.patch_forward_branch(branch, exit_target)?;
            }
            self.emit_close_bindings(scope, statement.span())?;
            self.emit_close_upvalues(scope, statement.span())?;
        } else if !breaks.is_empty() {
            let break_target = self.code.len();
            for branch in breaks {
                self.patch_forward_branch(branch, break_target)?;
            }
            self.emit_close_bindings(scope, statement.span())?;
            self.emit_close_upvalues(scope, statement.span())?;
        }
        self.close_scope(scope)?;
        Ok(())
    }

    fn lower_numeric_for(
        &mut self,
        statement: &NumericForStatement,
    ) -> Result<(), OwnedCompileError> {
        let initial_source = self.lower_expression(statement.initial())?;
        let initial =
            self.lower_numeric_for_control(initial_source, statement.span(), "initial")?;
        let limit_source = self.lower_expression(statement.limit())?;
        let limit = self.lower_numeric_for_control(limit_source, statement.span(), "limit")?;
        let index = self.allocate_register()?;
        self.emit(
            Instruction::Move {
                destination: index,
                source: initial,
            },
            statement.span(),
        )?;
        let (step, ascending) = if let Some(step_expression) = statement.step() {
            let sign = self.numeric_for_step_sign(step_expression, false)?;
            let ascending = if let Some(sign) = sign {
                if sign == 0 {
                    match self.profile {
                        SemanticProfile::Luau
                        | SemanticProfile::Lua51
                        | SemanticProfile::Lua52
                        | SemanticProfile::Lua53 => Some(false),
                        SemanticProfile::Blu | SemanticProfile::Lua54 | SemanticProfile::Lua55 => {
                            Some(false)
                        }
                        profile => return Err(OwnedCompileError::UnsupportedProfile(profile)),
                    }
                } else {
                    Some(sign > 0)
                }
            } else {
                match self.profile {
                    SemanticProfile::Luau
                    | SemanticProfile::Lua51
                    | SemanticProfile::Lua52
                    | SemanticProfile::Lua53 => None,
                    SemanticProfile::Blu | SemanticProfile::Lua54 | SemanticProfile::Lua55 => None,
                    profile => return Err(OwnedCompileError::UnsupportedProfile(profile)),
                }
            };
            let source = self.lower_expression(step_expression)?;
            let snapshot = self.lower_numeric_for_control(source, statement.span(), "step")?;
            (snapshot, ascending)
        } else {
            let step_constant = if matches!(
                self.profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ) {
                Constant::Integer(1)
            } else {
                Constant::Number(1.0)
            };
            (
                self.lower_constant(step_constant, statement.span())?,
                Some(true),
            )
        };

        if statement.step().is_some()
            && matches!(
                self.profile,
                SemanticProfile::Blu | SemanticProfile::Lua54 | SemanticProfile::Lua55
            )
        {
            let validator = self.lower_global_name(
                b"__blu_internal_validate_numeric_for_step",
                statement.span(),
            )?;
            let arguments = self.allocate_register()?;
            self.emit(
                Instruction::Move {
                    destination: arguments,
                    source: step,
                },
                statement.span(),
            )?;
            let result = self.allocate_register()?;
            self.emit(
                Instruction::Call {
                    destination: result,
                    function: validator,
                    arguments,
                    argument_count: 1,
                },
                statement.span(),
            )?;
        }

        let loop_scope = self.bindings.len();
        let start = self.code.len();
        let condition = self.allocate_register()?;
        if let Some(ascending) = ascending {
            self.emit(
                Instruction::LessEqual {
                    destination: condition,
                    left: if ascending { index } else { limit },
                    right: if ascending { limit } else { index },
                },
                statement.span(),
            )?;
        } else {
            let zero = self.lower_constant(
                if self.profile == SemanticProfile::Lua53 {
                    Constant::Integer(0)
                } else {
                    Constant::Number(0.0)
                },
                statement.span(),
            )?;
            let positive = self.allocate_register()?;
            self.emit(
                Instruction::LessThan {
                    destination: positive,
                    left: zero,
                    right: step,
                },
                statement.span(),
            )?;
            let descending = self.code.len();
            self.emit(
                Instruction::JumpIfFalsy {
                    condition: positive,
                    target: 0,
                },
                statement.span(),
            )?;
            self.emit(
                Instruction::LessEqual {
                    destination: condition,
                    left: index,
                    right: limit,
                },
                statement.span(),
            )?;
            let condition_ready = self.code.len();
            self.emit(Instruction::Jump { target: 0 }, statement.span())?;
            let descending_target = self.code.len();
            self.patch_forward_branch(descending, descending_target)?;
            self.emit(
                Instruction::LessEqual {
                    destination: condition,
                    left: limit,
                    right: index,
                },
                statement.span(),
            )?;
            let ready_target = self.code.len();
            self.patch_forward_branch(condition_ready, ready_target)?;
        }
        let exit = self.code.len();
        self.emit(
            Instruction::JumpIfFalsy {
                condition,
                target: 0,
            },
            statement.span(),
        )?;
        let binding_limit = self
            .limits
            .max_bindings
            .min(self.limits.artifact.max_debug_entries_per_prototype)
            .min(self.limits.artifact.max_total_debug_entries);
        check_limit(
            OwnedCompileLimit::Bindings,
            self.local_binding_count().saturating_add(1),
            binding_limit,
        )?;
        let binding = self.allocate_register()?;
        self.emit(
            Instruction::Move {
                destination: binding,
                source: index,
            },
            statement.span(),
        )?;
        let start_pc =
            u32::try_from(self.code.len()).map_err(|_| OwnedCompileError::InternalInvariant {
                message: "instruction count passed limits but cannot fit a debug PC",
            })?;
        push_fallible(
            &mut self.bindings,
            Binding {
                name: BindingName::Source(statement.name().span()),
                register: binding,
                constant: false,
                to_close: false,
                start_pc,
                end_pc: None,
            },
            "local bindings",
        )?;
        push_fallible(
            &mut self.loop_breaks,
            allocate_vec(2, "loop break branches")?,
            "loop control stack",
        )?;
        push_fallible(
            &mut self.loop_continues,
            allocate_vec(2, "loop continue branches")?,
            "loop continue stack",
        )?;
        let body_scope = self.bindings.len();
        let lowered = self.lower_block_statements(statement.body().statements());
        let continues = self
            .loop_continues
            .pop()
            .ok_or(OwnedCompileError::InternalInvariant {
                message: "loop continue stack became empty during lowering",
            })?;
        let breaks = self
            .loop_breaks
            .pop()
            .ok_or(OwnedCompileError::InternalInvariant {
                message: "loop control stack became empty during lowering",
            })?;
        let terminated = lowered?
            || statement
                .body()
                .statements()
                .last()
                .is_some_and(|statement| matches!(statement, Statement::Goto(_)));
        let needs_loop_back = !terminated || !continues.is_empty();
        let mut overflow_branches = allocate_vec(1, "numeric for overflow branches")?;
        let continue_target = self.code.len();
        for branch in continues {
            self.patch_forward_branch(branch, continue_target)?;
        }
        if needs_loop_back {
            let loop_back_span = statement
                .body()
                .statements()
                .last()
                .map_or(statement.span(), Statement::span);
            self.emit_close_bindings(body_scope, loop_back_span)?;
            self.emit_close_upvalues(body_scope, loop_back_span)?;
            self.emit_close_upvalues(loop_scope, loop_back_span)?;
            let previous = self.allocate_register()?;
            self.emit(
                Instruction::Move {
                    destination: previous,
                    source: index,
                },
                loop_back_span,
            )?;
            if let Some(ascending) = ascending {
                self.emit(
                    Instruction::Add {
                        destination: index,
                        left: index,
                        right: step,
                    },
                    loop_back_span,
                )?;
                let overflow = self.allocate_register()?;
                self.emit(
                    Instruction::LessThan {
                        destination: overflow,
                        left: if ascending { index } else { previous },
                        right: if ascending { previous } else { index },
                    },
                    loop_back_span,
                )?;
                let overflow_test = self.code.len();
                self.emit(
                    Instruction::JumpIfTruthy {
                        condition: overflow,
                        target: 0,
                    },
                    loop_back_span,
                )?;
                self.emit(
                    Instruction::LessEqual {
                        destination: condition,
                        left: if ascending { index } else { limit },
                        right: if ascending { limit } else { index },
                    },
                    loop_back_span,
                )?;
                let limit_exit = self.code.len();
                self.emit(
                    Instruction::JumpIfFalsy {
                        condition,
                        target: 0,
                    },
                    loop_back_span,
                )?;
                let target =
                    u32::try_from(start).map_err(|_| OwnedCompileError::InternalInvariant {
                        message: "loop target passed limits but cannot fit BluV1",
                    })?;
                self.emit(Instruction::Jump { target }, loop_back_span)?;
                let overflow_branch = self.code.len();
                self.emit(Instruction::Jump { target: 0 }, statement.span())?;
                self.patch_forward_branch(overflow_test, overflow_branch)?;
                self.patch_forward_branch(limit_exit, overflow_branch)?;
                push_fallible(
                    &mut overflow_branches,
                    overflow_branch,
                    "numeric for overflow branches",
                )?;
            } else {
                self.emit(
                    Instruction::Add {
                        destination: index,
                        left: index,
                        right: step,
                    },
                    loop_back_span,
                )?;
                let zero = self.lower_constant(
                    if self.profile == SemanticProfile::Lua53 {
                        Constant::Integer(0)
                    } else {
                        Constant::Number(0.0)
                    },
                    loop_back_span,
                )?;
                let positive = self.allocate_register()?;
                self.emit(
                    Instruction::LessThan {
                        destination: positive,
                        left: zero,
                        right: step,
                    },
                    loop_back_span,
                )?;
                let ascending_overflow = self.allocate_register()?;
                self.emit(
                    Instruction::LessThan {
                        destination: ascending_overflow,
                        left: index,
                        right: previous,
                    },
                    loop_back_span,
                )?;
                let descending_overflow = self.allocate_register()?;
                self.emit(
                    Instruction::LessThan {
                        destination: descending_overflow,
                        left: previous,
                        right: index,
                    },
                    loop_back_span,
                )?;
                let positive_branch = self.code.len();
                self.emit(
                    Instruction::JumpIfFalsy {
                        condition: positive,
                        target: 0,
                    },
                    loop_back_span,
                )?;
                let ascending_branch = self.code.len();
                self.emit(
                    Instruction::JumpIfTruthy {
                        condition: ascending_overflow,
                        target: 0,
                    },
                    loop_back_span,
                )?;
                let target =
                    u32::try_from(start).map_err(|_| OwnedCompileError::InternalInvariant {
                        message: "loop target passed limits but cannot fit BluV1",
                    })?;
                self.emit(Instruction::Jump { target }, loop_back_span)?;
                let descending_target = self.code.len();
                self.patch_forward_branch(positive_branch, descending_target)?;
                let descending_branch = self.code.len();
                self.emit(
                    Instruction::JumpIfTruthy {
                        condition: descending_overflow,
                        target: 0,
                    },
                    loop_back_span,
                )?;
                self.emit(Instruction::Jump { target }, loop_back_span)?;
                let overflow_target = self.code.len();
                self.patch_forward_branch(ascending_branch, overflow_target)?;
                self.patch_forward_branch(descending_branch, overflow_target)?;
                let overflow_branch = self.code.len();
                self.emit(Instruction::Jump { target: 0 }, loop_back_span)?;
                push_fallible(
                    &mut overflow_branches,
                    overflow_branch,
                    "numeric for overflow branches",
                )?;
            }
        }
        if !breaks.is_empty() {
            let break_target = self.code.len();
            for branch in breaks {
                self.patch_forward_branch(branch, break_target)?;
            }
            self.emit_close_bindings(body_scope, statement.span())?;
            self.emit_close_upvalues(body_scope, statement.span())?;
        }
        self.close_scope(body_scope)?;
        let end = self.code.len();
        self.patch_forward_branch(exit, end)?;
        for branch in overflow_branches {
            self.patch_forward_branch(branch, end)?;
        }
        let exit_span = self.debug_assignment_span(statement.span())?;
        self.emit_close_upvalues(loop_scope, exit_span)?;
        self.close_scope(loop_scope)
    }

    fn lower_numeric_for_control(
        &mut self,
        source: u16,
        span: ByteSpan,
        role: &'static str,
    ) -> Result<u16, OwnedCompileError> {
        let function = self.lower_global_name(b"__blu_internal_coerce_numeric_for", span)?;
        let arguments = self.allocate_register()?;
        let _role_register =
            self.lower_constant(Constant::String(role.as_bytes().to_vec()), span)?;
        self.emit(
            Instruction::Move {
                destination: arguments,
                source,
            },
            span,
        )?;
        let result = self.allocate_register()?;
        self.emit(
            Instruction::Call {
                destination: result,
                function,
                arguments,
                argument_count: 2,
            },
            span,
        )?;
        Ok(result)
    }

    fn lower_generic_for(
        &mut self,
        statement: &GenericForStatement,
    ) -> Result<(), OwnedCompileError> {
        let control_count = if matches!(
            self.profile,
            SemanticProfile::Lua54 | SemanticProfile::Lua55
        ) {
            4
        } else {
            3
        };
        let mut controls = allocate_vec(control_count, "generic for controls")?;
        for (index, value) in statement.values().iter().copied().enumerate() {
            let remaining = control_count.saturating_sub(index);
            let is_last = index + 1 == statement.values().len();
            let mut lowered = if is_last && remaining > 1 {
                self.lower_call_expression_results(value, remaining)?
                    .unwrap_or_else(Vec::new)
            } else {
                Vec::new()
            };
            if lowered.is_empty() {
                lowered.push(self.lower_expression(value)?);
            }
            for source in lowered.into_iter().take(remaining) {
                let snapshot = self.allocate_register()?;
                self.emit(
                    Instruction::Move {
                        destination: snapshot,
                        source,
                    },
                    self.expression(value)?.span(),
                )?;
                controls.push(snapshot);
            }
        }
        while controls.len() < control_count {
            controls.push(self.lower_constant(Constant::Nil, statement.span())?);
        }
        let (iterator, state, control) =
            if matches!(self.profile, SemanticProfile::Blu | SemanticProfile::Luau) {
                let prepare =
                    self.lower_global_name(b"__blu_internal_prepare_iter", statement.span())?;
                let prepared = self.emit_fixed_call_results(
                    prepare,
                    &controls,
                    control_count,
                    false,
                    statement.span(),
                )?;
                (prepared[0], prepared[1], prepared[2])
            } else {
                (controls[0], controls[1], controls[2])
            };
        let to_close = controls.get(3).copied();

        if let Some(to_close) = to_close {
            self.emit_mark_close_value(to_close, statement.span())?;
        }

        let loop_scope = self.bindings.len();
        let start = self.code.len();
        let results = self.emit_fixed_call_results(
            iterator,
            &[state, control],
            statement.names().len(),
            false,
            statement.span(),
        )?;
        self.emit(
            Instruction::Move {
                destination: control,
                source: results[0],
            },
            statement.span(),
        )?;
        let nil = self.lower_constant(Constant::Nil, statement.span())?;
        let finished = self.allocate_register()?;
        self.emit(
            Instruction::Equal {
                destination: finished,
                left: results[0],
                right: nil,
            },
            statement.span(),
        )?;
        let exit = self.code.len();
        self.emit(
            Instruction::JumpIfTruthy {
                condition: finished,
                target: 0,
            },
            statement.span(),
        )?;

        let start_pc =
            u32::try_from(self.code.len()).map_err(|_| OwnedCompileError::InternalInvariant {
                message: "instruction count passed limits but cannot fit a debug PC",
            })?;
        for (name, register) in statement
            .names()
            .iter()
            .copied()
            .zip(results.iter().copied())
        {
            self.push_binding(BindingName::Source(name.span()), register, start_pc)?;
        }
        push_fallible(
            &mut self.loop_breaks,
            allocate_vec(2, "loop break branches")?,
            "loop control stack",
        )?;
        push_fallible(
            &mut self.loop_continues,
            allocate_vec(2, "loop continue branches")?,
            "loop continue stack",
        )?;
        let body_scope = self.bindings.len();
        let lowered = self.lower_block_statements(statement.body().statements());
        let continues = self
            .loop_continues
            .pop()
            .ok_or(OwnedCompileError::InternalInvariant {
                message: "loop continue stack became empty during lowering",
            })?;
        let breaks = self
            .loop_breaks
            .pop()
            .ok_or(OwnedCompileError::InternalInvariant {
                message: "loop control stack became empty during lowering",
            })?;
        let terminated = lowered?
            || statement
                .body()
                .statements()
                .last()
                .is_some_and(|statement| matches!(statement, Statement::Goto(_)));
        let needs_loop_back = !terminated || !continues.is_empty();
        let continue_target = self.code.len();
        for branch in continues {
            self.patch_forward_branch(branch, continue_target)?;
        }
        if needs_loop_back {
            let loop_back_span = statement
                .body()
                .statements()
                .last()
                .map_or(statement.span(), Statement::span);
            self.emit_close_bindings(body_scope, loop_back_span)?;
            self.emit_close_upvalues(body_scope, loop_back_span)?;
            self.emit_close_upvalues(loop_scope, loop_back_span)?;
            let target =
                u32::try_from(start).map_err(|_| OwnedCompileError::InternalInvariant {
                    message: "loop target passed limits but cannot fit BluV1",
                })?;
            self.emit(Instruction::Jump { target }, loop_back_span)?;
        }
        if !breaks.is_empty() {
            let break_target = self.code.len();
            for branch in breaks {
                self.patch_forward_branch(branch, break_target)?;
            }
            self.emit_close_bindings(body_scope, statement.span())?;
            self.emit_close_upvalues(body_scope, statement.span())?;
        }
        self.close_scope(body_scope)?;
        let close_start = self.code.len();
        self.patch_forward_branch(exit, close_start)?;
        self.emit_close_upvalues(loop_scope, statement.span())?;
        if let Some(to_close) = to_close {
            let close = self.lower_global_name(b"__blu_internal_close", statement.span())?;
            let arguments = self.allocate_register()?;
            self.emit(
                Instruction::Move {
                    destination: arguments,
                    source: to_close,
                },
                statement.span(),
            )?;
            let result = self.allocate_register()?;
            self.emit(
                Instruction::Call {
                    destination: result,
                    function: close,
                    arguments,
                    argument_count: 1,
                },
                statement.span(),
            )?;
        }
        self.close_scope(loop_scope)
    }

    fn lower_global_name(&mut self, name: &[u8], span: ByteSpan) -> Result<u16, OwnedCompileError> {
        let constant = self.push_constant(Constant::String(name.to_vec()))?;
        let destination = self.allocate_register()?;
        self.emit(
            Instruction::LoadGlobal {
                destination,
                name: constant,
            },
            span,
        )?;
        Ok(destination)
    }

    fn lower_environment_key(&mut self, span: ByteSpan) -> Result<u16, OwnedCompileError> {
        let constant = self.push_constant(Constant::String(copy_bytes(
            self.source.slice(span)?,
            "lexical environment key",
        )?))?;
        let destination = self.allocate_register()?;
        self.emit(
            Instruction::LoadConstant {
                destination,
                constant,
            },
            span,
        )?;
        Ok(destination)
    }

    fn numeric_for_step_sign(
        &self,
        expression: ExpressionId,
        negated: bool,
    ) -> Result<Option<i8>, OwnedCompileError> {
        let expression = self.expression(expression)?;
        let sign = match expression.kind() {
            ExpressionKind::DecimalInteger => {
                Some(constant_sign(self.decimal_constant(expression.span())?))
            }
            ExpressionKind::DecimalNumber => Some(constant_sign(
                self.decimal_number_constant(expression.span())?,
            )),
            ExpressionKind::HexInteger => {
                Some(constant_sign(self.hex_integer_constant(expression.span())?))
            }
            ExpressionKind::HexNumber => {
                Some(constant_sign(self.hex_number_constant(expression.span())?))
            }
            ExpressionKind::BinaryInteger => Some(constant_sign(
                self.binary_integer_constant(expression.span())?,
            )),
            ExpressionKind::Group(inner) => return self.numeric_for_step_sign(inner, negated),
            ExpressionKind::Unary(unary) if unary.operator() == UnaryOperator::Negate => {
                return self.numeric_for_step_sign(unary.operand(), !negated);
            }
            _ => None,
        };
        Ok(sign.map(|sign| if negated { -sign } else { sign }))
    }

    fn lower_break(&mut self, span: ByteSpan) -> Result<(), OwnedCompileError> {
        if self.loop_breaks.is_empty() {
            return Err(OwnedCompileError::InternalInvariant {
                message: "parser exposed break outside a loop",
            });
        }
        let branch = self.code.len();
        self.emit(Instruction::Jump { target: 0 }, span)?;
        let Some(breaks) = self.loop_breaks.last_mut() else {
            return Err(OwnedCompileError::InternalInvariant {
                message: "loop control stack became empty during break lowering",
            });
        };
        push_fallible(breaks, branch, "loop break branches")
    }

    fn lower_continue(&mut self, span: ByteSpan) -> Result<(), OwnedCompileError> {
        if self.loop_continues.is_empty() {
            return Err(OwnedCompileError::InternalInvariant {
                message: "parser exposed continue outside a loop",
            });
        }
        let branch = self.code.len();
        self.emit(Instruction::Jump { target: 0 }, span)?;
        let Some(continues) = self.loop_continues.last_mut() else {
            return Err(OwnedCompileError::InternalInvariant {
                message: "loop continue stack became empty during continue lowering",
            });
        };
        push_fallible(continues, branch, "loop continue branches")
    }

    fn control_flow_name(&self, span: ByteSpan) -> Result<String, OwnedCompileError> {
        Ok(String::from_utf8_lossy(self.source.slice(span)?).into_owned())
    }

    fn undeclared_global_message(&self, span: ByteSpan) -> Result<String, OwnedCompileError> {
        let name = self.control_flow_name(span)?;
        Ok(format!("variable '{name}' is not declared as global"))
    }

    fn goto_scope_message(&self, name: &str, local: bool) -> String {
        if local && self.profile == SemanticProfile::Lua54 {
            format!("goto enters the scope of local '{name}'")
        } else {
            format!("goto enters the scope of '{name}'")
        }
    }

    fn lower_label(&mut self, statement: &LabelStatement) -> Result<(), OwnedCompileError> {
        let name = statement.name().span();
        let block = self
            .active_block
            .ok_or(OwnedCompileError::InternalInvariant {
                message: "label lowered outside a planned statement block",
            })?;
        if self.labels.iter().any(|label| {
            self.source.slice(label.name).ok() == self.source.slice(name).ok()
                && self.block_is_visible(label.block, block)
        }) {
            let name = self.control_flow_name(name)?;
            return Err(OwnedCompileError::ControlFlow {
                message: format!("label '{name}' is already defined"),
            });
        }
        // A label does not create a lexical scope. Keep the bindings live at
        // the point where it is emitted; enclosing constructs and cross-block
        // gotos perform the required cleanup.
        let scope = self.local_binding_depth();
        push_fallible(
            &mut self.labels,
            Label {
                name,
                target: self.code.len(),
                scope,
                block,
            },
            "labels",
        )
    }

    fn lower_goto(&mut self, statement: &GotoStatement) -> Result<(), OwnedCompileError> {
        let current_scope = self.local_binding_depth();
        let block = self
            .active_block
            .ok_or(OwnedCompileError::InternalInvariant {
                message: "goto lowered outside a planned statement block",
            })?;
        let current_global_scope = self
            .planned_gotos
            .iter()
            .find(|planned| planned.block == block && planned.name == statement.name().span())
            .map_or(0, |planned| planned.global_scope);
        if let Some(target) = self
            .planned_labels
            .iter()
            .find(|label| {
                self.source.slice(label.name).ok()
                    == self.source.slice(statement.name().span()).ok()
                    && self.block_is_visible(label.block, block)
            })
            .copied()
        {
            let target_scope = target.scope;
            let local_name = target
                .local_name
                .is_some_and(|local| local.start() > statement.name().span().start())
                .then(|| target.local_name.expect("checked above"));
            let local_name = local_name.filter(|_| !target.terminal_scope_close);
            let global_name = (target.global_scope > current_global_scope)
                .then(|| target.global_name.unwrap_or(target.name));
            let entering = match (local_name, global_name) {
                (Some(local), Some(global)) if local.start() <= global.start() => {
                    Some((local, true))
                }
                (Some(_), Some(global)) => Some((global, false)),
                (Some(local), None) => Some((local, true)),
                (None, Some(global)) => Some((global, false)),
                (None, None) => None,
            };
            if let Some((entering_name, local)) = entering {
                let entering_name = self.control_flow_name(entering_name)?;
                return Err(OwnedCompileError::ControlFlow {
                    message: self.goto_scope_message(&entering_name, local),
                });
            }
            if target_scope > current_scope {
                let local_name = target.local_name.unwrap_or(target.name);
                let local_name = self.control_flow_name(local_name)?;
                return Err(OwnedCompileError::ControlFlow {
                    message: self.goto_scope_message(&local_name, true),
                });
            }
            if target_scope < current_scope && target.block != block {
                let binding_start = self.binding_index_for_local_depth(target_scope);
                self.emit_close_bindings(binding_start, statement.span())?;
                self.emit_close_upvalues(binding_start, statement.span())?;
            }
        } else if self.planned_labels.iter().any(|label| {
            self.source.slice(label.name).ok() == self.source.slice(statement.name().span()).ok()
        }) {
            let name = self.control_flow_name(statement.name().span())?;
            return Err(OwnedCompileError::ControlFlow {
                message: format!("label '{name}' is not visible from this goto"),
            });
        }
        let instruction = self.code.len();
        self.emit(Instruction::Jump { target: 0 }, statement.span())?;
        push_fallible(
            &mut self.gotos,
            Goto {
                name: statement.name().span(),
                instruction,
                scope: current_scope,
                block,
                global_scope: current_global_scope,
            },
            "goto branches",
        )
    }

    fn resolve_gotos(&mut self) -> Result<(), OwnedCompileError> {
        for goto in self.gotos.clone() {
            let Some(label) = self.labels.iter().find(|label| {
                self.source.slice(label.name).ok() == self.source.slice(goto.name).ok()
                    && self.block_is_visible(label.block, goto.block)
            }) else {
                let name = self.control_flow_name(goto.name)?;
                return Err(OwnedCompileError::ControlFlow {
                    message: format!("label '{name}' is not defined"),
                });
            };
            let planned = self.planned_labels.iter().find(|planned| {
                planned.block == label.block
                    && self.source.slice(planned.name).ok() == self.source.slice(label.name).ok()
            });
            let target_scope = planned.map_or(label.scope, |planned| planned.scope);
            if let Some(planned) =
                planned.filter(|planned| planned.global_scope > goto.global_scope)
            {
                let global_name = planned.global_name.unwrap_or(planned.name);
                let global_name = self.control_flow_name(global_name)?;
                return Err(OwnedCompileError::ControlFlow {
                    message: self.goto_scope_message(&global_name, false),
                });
            }
            if target_scope > goto.scope {
                let local_name = planned
                    .and_then(|planned| planned.local_name)
                    .unwrap_or(label.name);
                let local_name = self.control_flow_name(local_name)?;
                return Err(OwnedCompileError::ControlFlow {
                    message: self.goto_scope_message(&local_name, true),
                });
            }
            self.patch_forward_branch(goto.instruction, label.target)?;
        }
        Ok(())
    }

    fn global_control_flow_name(
        &self,
        statement: &GlobalStatement,
    ) -> Result<ByteSpan, OwnedCompileError> {
        if let Some(name) = statement.names().first() {
            return Ok(name.span());
        }
        let span = statement.span();
        let start = span.start().as_usize();
        let wildcard = self.source.bytes()[start..]
            .iter()
            .position(|byte| *byte == b'*')
            .map(|offset| start.saturating_add(offset))
            .filter(|offset| {
                self.source.bytes()[start..=*offset]
                    .iter()
                    .all(|byte| !matches!(*byte, b'\n' | b'\r' | b';'))
            })
            .ok_or(OwnedCompileError::InternalInvariant {
                message: "global wildcard declaration has no wildcard token",
            })?;
        let end = wildcard
            .checked_add(1)
            .ok_or(OwnedCompileError::InternalInvariant {
                message: "global wildcard span end overflowed",
            })?;
        Ok(ByteSpan::from_usize(span.source(), wildcard, end)?)
    }

    fn plan_labels(
        &mut self,
        statements: &[Statement],
        mut scope: usize,
        mut global_scope: usize,
        parent: Option<usize>,
        allow_terminal_scope_close: bool,
    ) -> Result<usize, OwnedCompileError> {
        let block = self.planned_blocks.len();
        push_fallible(
            &mut self.planned_blocks,
            PlannedBlock {
                pointer: statements.as_ptr() as usize,
                length: statements.len(),
                parent,
            },
            "planned blocks",
        )?;
        let mut local_name = None;
        let mut global_name = None;
        for (index, statement) in statements.iter().enumerate() {
            match statement {
                Statement::Label(label) => {
                    let label_scope = self.terminal_label_scope(
                        statements,
                        index,
                        scope,
                        allow_terminal_scope_close,
                    );
                    push_fallible(
                        &mut self.planned_labels,
                        PlannedLabel {
                            name: label.name().span(),
                            scope: label_scope,
                            block,
                            local_name,
                            global_scope,
                            global_name,
                            terminal_scope_close: label_scope < scope,
                        },
                        "planned labels",
                    )?;
                }
                Statement::Goto(goto) => {
                    push_fallible(
                        &mut self.planned_gotos,
                        PlannedGoto {
                            name: goto.name().span(),
                            block,
                            global_scope,
                        },
                        "planned gotos",
                    )?;
                }
                Statement::Local(local) => {
                    scope = scope.saturating_add(1);
                    local_name = Some(local.name().span());
                }
                Statement::LocalList(local) => {
                    scope = scope.saturating_add(local.names().len());
                    local_name = local.names().first().map(|name| name.span());
                }
                Statement::Global(global) => {
                    global_scope = global_scope.saturating_add(1);
                    global_name = if global.wildcard() {
                        Some(self.global_control_flow_name(global)?)
                    } else {
                        global.names().first().map(|name| name.span())
                    };
                }
                Statement::Do(statement) => {
                    self.plan_labels(
                        statement.body().statements(),
                        scope,
                        global_scope,
                        Some(block),
                        true,
                    )?;
                }
                Statement::If(statement) => {
                    for clause in statement.clauses() {
                        self.plan_labels(
                            clause.body().statements(),
                            scope,
                            global_scope,
                            Some(block),
                            true,
                        )?;
                    }
                    if let Some(body) = statement.else_body() {
                        self.plan_labels(
                            body.statements(),
                            scope,
                            global_scope,
                            Some(block),
                            true,
                        )?;
                    }
                }
                Statement::While(statement) => {
                    self.plan_labels(
                        statement.body().statements(),
                        scope,
                        global_scope,
                        Some(block),
                        true,
                    )?;
                }
                Statement::Repeat(statement) => {
                    self.plan_labels(
                        statement.body().statements(),
                        scope,
                        global_scope,
                        Some(block),
                        false,
                    )?;
                }
                Statement::NumericFor(statement) => {
                    self.plan_labels(
                        statement.body().statements(),
                        scope.saturating_add(1),
                        global_scope,
                        Some(block),
                        true,
                    )?;
                }
                Statement::GenericFor(statement) => {
                    self.plan_labels(
                        statement.body().statements(),
                        scope.saturating_add(statement.names().len()),
                        global_scope,
                        Some(block),
                        true,
                    )?;
                }
                _ => {}
            }
        }
        Ok(block)
    }

    fn label_has_planned_goto(&self, name: ByteSpan) -> bool {
        let Some(block) = self.active_block else {
            return false;
        };
        self.planned_gotos.iter().any(|goto| {
            self.source.slice(goto.name).ok() == self.source.slice(name).ok()
                && self.planned_labels.iter().any(|label| {
                    self.source.slice(label.name).ok() == self.source.slice(name).ok()
                        && label.block == block
                        && self.block_is_visible(label.block, goto.block)
                })
        })
    }

    fn label_has_emitted_goto(&self, name: ByteSpan) -> bool {
        let Some(block) = self.active_block else {
            return false;
        };
        self.gotos.iter().any(|goto| {
            self.source.slice(goto.name).ok() == self.source.slice(name).ok()
                && self.planned_labels.iter().any(|label| {
                    self.source.slice(label.name).ok() == self.source.slice(name).ok()
                        && label.block == block
                        && self.block_is_visible(label.block, goto.block)
                })
        })
    }

    fn terminal_label_scope(
        &self,
        statements: &[Statement],
        index: usize,
        scope: usize,
        allow_terminal_scope_close: bool,
    ) -> usize {
        if !allow_terminal_scope_close {
            return scope;
        }
        if !statements[index.saturating_add(1)..]
            .iter()
            .all(|statement| matches!(statement, Statement::Label(_)))
        {
            return scope;
        }
        let mut adjusted = scope;
        for statement in &statements[..index] {
            match statement {
                Statement::Local(_) => {
                    adjusted = adjusted.saturating_sub(1);
                }
                Statement::LocalList(local) => {
                    adjusted = adjusted.saturating_sub(local.names().len());
                }
                _ => {}
            }
        }
        adjusted
    }

    fn block_is_ancestor(&self, ancestor: usize, mut block: usize) -> bool {
        loop {
            if ancestor == block {
                return true;
            }
            let Some(parent) = self
                .planned_blocks
                .get(block)
                .and_then(|block| block.parent)
            else {
                return false;
            };
            block = parent;
        }
    }

    fn block_is_visible(&self, label: usize, goto: usize) -> bool {
        self.block_is_ancestor(label, goto)
    }

    fn close_scope(&mut self, start: usize) -> Result<(), OwnedCompileError> {
        let start = start.min(self.bindings.len());
        let end_pc =
            u32::try_from(self.code.len()).map_err(|_| OwnedCompileError::InternalInvariant {
                message: "instruction count passed limits but cannot fit a debug PC",
            })?;
        let closing = self.bindings.len().saturating_sub(start);
        self.closed_bindings
            .try_reserve(closing)
            .map_err(|_| OwnedCompileError::Allocation {
                what: "closed local bindings",
                requested: self.closed_bindings.len().saturating_add(closing),
            })?;
        for binding in &self.bindings[start..] {
            let mut binding = *binding;
            binding.end_pc = Some(end_pc);
            self.closed_bindings.push(binding);
        }
        self.bindings.truncate(start);
        Ok(())
    }

    fn local_binding_depth(&self) -> usize {
        self.bindings
            .iter()
            .filter(|binding| !binding.name.is_global())
            .count()
    }

    fn local_binding_count(&self) -> usize {
        self.closed_bindings
            .iter()
            .chain(&self.bindings)
            .filter(|binding| !binding.name.is_global())
            .count()
    }

    fn binding_index_for_local_depth(&self, depth: usize) -> usize {
        if depth == 0 {
            return self
                .bindings
                .iter()
                .position(|binding| !binding.name.is_global())
                .unwrap_or(self.bindings.len());
        }
        let mut seen = 0_usize;
        for (index, binding) in self.bindings.iter().enumerate() {
            if !binding.name.is_global() {
                seen = seen.saturating_add(1);
                if seen == depth {
                    return index.saturating_add(1);
                }
            }
        }
        self.bindings.len()
    }

    fn emit_close_bindings(
        &mut self,
        start: usize,
        span: ByteSpan,
    ) -> Result<(), OwnedCompileError> {
        let start = start.min(self.bindings.len());
        let mut registers = allocate_vec(2, "to-be-closed local registers")?;
        for binding in self.bindings[start..].iter().rev() {
            if binding.to_close {
                push_fallible(
                    &mut registers,
                    binding.register,
                    "to-be-closed local registers",
                )?;
            }
        }
        for register in registers {
            self.emit_close_value(register, span)?;
        }
        Ok(())
    }

    fn emit_close_upvalues(
        &mut self,
        start: usize,
        span: ByteSpan,
    ) -> Result<(), OwnedCompileError> {
        let start = start.min(self.bindings.len());
        let Some(from) = self.bindings[start..]
            .iter()
            .filter(|binding| !binding.name.is_global())
            .map(|binding| binding.register)
            .min()
        else {
            return Ok(());
        };
        self.emit(Instruction::CloseUpvalues { from }, span)
    }

    fn emit_close_value(&mut self, value: u16, span: ByteSpan) -> Result<(), OwnedCompileError> {
        let close = self.lower_global_name(b"__blu_internal_close", span)?;
        let arguments = self.allocate_register()?;
        self.emit(
            Instruction::Move {
                destination: arguments,
                source: value,
            },
            span,
        )?;
        let result = self.allocate_register()?;
        self.emit(
            Instruction::Call {
                destination: result,
                function: close,
                arguments,
                argument_count: 1,
            },
            span,
        )
    }

    fn emit_mark_close_value(
        &mut self,
        value: u16,
        span: ByteSpan,
    ) -> Result<(), OwnedCompileError> {
        let mark = self.lower_global_name(b"__blu_internal_mark_close", span)?;
        let arguments = self.allocate_register()?;
        self.emit(
            Instruction::Move {
                destination: arguments,
                source: value,
            },
            span,
        )?;
        let result = self.allocate_register()?;
        self.emit(
            Instruction::Call {
                destination: result,
                function: mark,
                arguments,
                argument_count: 1,
            },
            span,
        )
    }

    fn lower_global(&mut self, statement: &GlobalStatement) -> Result<(), OwnedCompileError> {
        if statement
            .attributes()
            .iter()
            .any(|attribute| *attribute == LocalAttribute::Close)
        {
            return Err(OwnedCompileError::Diagnostic(self.source_diagnostic(
                "BLU-COMPILE-0011",
                Phase::Lower,
                statement.span(),
                "global variables cannot be to-be-closed",
            )?));
        }
        if let Some(invalid_name) = statement.names().iter().find(|name| {
            self.source
                .slice(name.span())
                .is_ok_and(|name| name == b"_ENV")
        }) {
            let accessed_name = statement
                .names()
                .iter()
                .find(|name| {
                    self.source
                        .slice(name.span())
                        .is_ok_and(|name| name != b"_ENV")
                })
                .unwrap_or(invalid_name);
            let accessed_display = self.control_flow_name(accessed_name.span())?;
            let message = format!("_ENV is global when accessing variable '{accessed_display}'");
            return Err(OwnedCompileError::Diagnostic(self.source_diagnostic(
                "BLU-COMPILE-0012",
                Phase::Lower,
                accessed_name.span(),
                &message,
            )?));
        }
        let mut values = allocate_vec(statement.values().len(), "global declaration values")?;
        for (index, value) in statement.values().iter().copied().enumerate() {
            let is_last = index + 1 == statement.values().len();
            let requested = statement.names().len().saturating_sub(index);
            if is_last && requested > 1 {
                if let Some(results) = self.lower_call_expression_results(value, requested)? {
                    values.extend(results);
                    continue;
                }
            }
            values.push(self.lower_expression(value)?);
        }
        while values.len() < statement.names().len() {
            values.push(self.lower_constant(Constant::Nil, statement.span())?);
        }

        if statement.wildcard() {
            self.push_binding_with_flags(
                BindingName::GlobalWildcard,
                GLOBAL_BINDING_REGISTER,
                0,
                statement.attribute() == LocalAttribute::Const,
                false,
            )?;
            return Ok(());
        }
        for (index, name) in statement.names().iter().enumerate() {
            let attribute = statement
                .attributes()
                .get(index)
                .copied()
                .unwrap_or(LocalAttribute::Regular);
            self.push_binding_with_flags(
                BindingName::Global(name.span()),
                GLOBAL_BINDING_REGISTER,
                0,
                attribute == LocalAttribute::Const,
                false,
            )?;
        }
        if statement.values().is_empty() {
            return Ok(());
        }
        for (name, value) in statement.names().iter().copied().zip(values.into_iter()) {
            if let Some(environment) = self.resolve_environment(statement.span())? {
                let key = self.lower_environment_key(name.span())?;
                if self.profile == SemanticProfile::Lua55 {
                    let function =
                        self.lower_global_name(b"__blu_internal_declare_global", statement.span())?;
                    let (arguments, argument_count) =
                        self.copy_call_arguments(&[environment, key, value], statement.span())?;
                    let result = self.allocate_register()?;
                    self.emit(
                        Instruction::Call {
                            destination: result,
                            function,
                            arguments,
                            argument_count,
                        },
                        statement.span(),
                    )?;
                } else {
                    self.emit(
                        Instruction::SetTable {
                            table: environment,
                            key,
                            value,
                        },
                        statement.span(),
                    )?;
                }
            } else {
                let constant = self.global_name_constant(name.span())?;
                self.emit(
                    Instruction::StoreGlobal {
                        name: constant,
                        source: value,
                    },
                    statement.span(),
                )?;
            }
        }
        Ok(())
    }

    fn lower_local(&mut self, statement: LocalStatement) -> Result<(), OwnedCompileError> {
        let register = match statement.value() {
            Some(value) => {
                let source = self.lower_expression(value)?;
                self.snapshot_if_bound(source, statement.span())?
            }
            None => self.lower_constant(Constant::Nil, statement.span())?,
        };
        let start_pc =
            u32::try_from(self.code.len()).map_err(|_| OwnedCompileError::InternalInvariant {
                message: "instruction count passed limits but cannot fit a debug PC",
            })?;
        let to_close = statement.attribute() == LocalAttribute::Close;
        self.push_binding_with_flags(
            BindingName::Source(statement.name().span()),
            register,
            start_pc,
            statement.attribute() == LocalAttribute::Const,
            to_close,
        )?;
        if to_close {
            self.emit_mark_close_value(register, statement.span())?;
        }
        Ok(())
    }

    fn push_binding(
        &mut self,
        name: BindingName,
        register: u16,
        start_pc: u32,
    ) -> Result<(), OwnedCompileError> {
        self.push_binding_with_flags(name, register, start_pc, false, false)
    }

    fn push_binding_with_flags(
        &mut self,
        name: BindingName,
        register: u16,
        start_pc: u32,
        constant: bool,
        to_close: bool,
    ) -> Result<(), OwnedCompileError> {
        let limit = self
            .limits
            .max_bindings
            .min(self.limits.artifact.max_debug_entries_per_prototype)
            .min(self.limits.artifact.max_total_debug_entries);
        check_limit(
            OwnedCompileLimit::Bindings,
            self.local_binding_count()
                .saturating_add(usize::from(!name.is_global())),
            limit,
        )?;
        push_fallible(
            &mut self.bindings,
            Binding {
                name,
                register,
                constant,
                to_close,
                start_pc,
                end_pc: None,
            },
            "local bindings",
        )
    }

    fn lower_function(
        &mut self,
        function: FunctionId,
        span: ByteSpan,
        implicit_self: bool,
    ) -> Result<u16, OwnedCompileError> {
        let body = self
            .ast
            .function(function)
            .ok_or(OwnedCompileError::InternalInvariant {
                message: "function expression references an absent AST body",
            })?;
        let mut demanded_upvalues = allocate_vec(self.outer_bindings.len(), "demanded upvalues")?;
        for binding in self.outer_bindings.iter().copied() {
            if !binding.name.is_global()
                && self.function_mentions_binding(body.span(), binding.name)?
            {
                push_fallible(&mut demanded_upvalues, binding, "demanded upvalues")?;
            }
        }
        for binding in demanded_upvalues {
            self.ensure_upvalue(binding)?;
        }
        let visible_count = self.upvalues.len().saturating_add(self.bindings.len());
        let mut outer_bindings = allocate_vec(visible_count, "child lexical bindings")?;
        // Global declarations are compile-time visibility metadata rather
        // than runtime captures. Preserve them through every nested function
        // boundary so a const global remains const when referenced from a
        // grandchild closure (Lua 5.5's `global a, value<const>` semantics).
        outer_bindings.extend(
            self.outer_bindings
                .iter()
                .copied()
                .filter(|binding| binding.name.is_global()),
        );
        for (upvalue, binding) in self.upvalues.iter().enumerate() {
            outer_bindings.push(OuterBinding {
                name: binding.name,
                constant: binding.constant,
                source: Upvalue::ParentUpvalue(u16::try_from(upvalue).map_err(|_| {
                    OwnedCompileError::InternalInvariant {
                        message: "upvalue count passed limits but cannot fit BluV1",
                    }
                })?),
            });
        }
        outer_bindings.extend(self.bindings.iter().map(|binding| OuterBinding {
            name: binding.name,
            constant: binding.constant,
            source: Upvalue::ParentRegister(binding.register),
        }));

        check_limit(
            OwnedCompileLimit::Prototypes,
            self.prototypes.len().saturating_add(1),
            self.limits.artifact.max_prototypes,
        )?;
        let child = Lowerer::new(
            self.source,
            self.ast,
            self.limits,
            self.prototypes,
            body.parameters(),
            FunctionShape {
                implicit_self,
                is_vararg: body.is_vararg(),
                vararg_name: body.vararg_name().map(|name| name.span()),
            },
            &outer_bindings,
            Some(body.span()),
        )?
        .run(body.body().statements())?;
        let child_index = u32::try_from(self.prototypes.len()).map_err(|_| {
            OwnedCompileError::InternalInvariant {
                message: "prototype count passed limits but cannot fit BluV1",
            }
        })?;
        push_fallible(self.prototypes, child, "artifact prototypes")?;

        let child_slot = self.children.len();
        check_limit(
            OwnedCompileLimit::Children,
            child_slot.saturating_add(1),
            self.limits
                .artifact
                .max_children_per_prototype
                .min(u16::MAX as usize),
        )?;
        push_fallible(&mut self.children, child_index, "prototype children")?;
        let destination = self.allocate_register()?;
        let closure_end = span.end().as_usize();
        let closure_span = self
            .source
            .span(closure_end.saturating_sub(1), closure_end)?;
        self.emit(
            Instruction::NewClosure {
                destination,
                child: u16::try_from(child_slot).map_err(|_| {
                    OwnedCompileError::InternalInvariant {
                        message: "child count passed limits but cannot fit BluV1",
                    }
                })?,
            },
            closure_span,
        )?;
        Ok(destination)
    }

    fn lower_function_statement(
        &mut self,
        statement: &FunctionStatement,
    ) -> Result<(), OwnedCompileError> {
        let Some(root) = statement.names().first().copied() else {
            return Err(OwnedCompileError::InternalInvariant {
                message: "named function statement has an empty name path",
            });
        };
        if statement.names().len() == 1 {
            if statement.is_global() {
                self.push_binding(BindingName::Global(root.span()), GLOBAL_BINDING_REGISTER, 0)?;
            }
            self.ensure_writable(root.span())?;
            let closure = self.lower_function(
                statement.function(),
                statement.span(),
                statement.is_method(),
            )?;

            // A plain `function name() ... end` declaration is assignment
            // syntax.  In particular, it must update an already-visible
            // local/upvalue instead of unconditionally replacing the global
            // binding with the same name.
            if !statement.is_global() {
                if let Some(destination) = self.resolve_local(root.span())? {
                    return self.emit(
                        Instruction::Move {
                            destination,
                            source: closure,
                        },
                        statement.span(),
                    );
                }
                if let Some(upvalue) = self.resolve_upvalue(root.span())? {
                    return self.emit(
                        Instruction::SetUpvalue {
                            upvalue,
                            source: closure,
                        },
                        statement.span(),
                    );
                }
                if self.global_status(root.span())? == Some(false) {
                    let message = self.undeclared_global_message(root.span())?;
                    return Err(OwnedCompileError::Diagnostic(self.source_diagnostic(
                        "BLU-COMPILE-0012",
                        Phase::Lower,
                        root.span(),
                        &message,
                    )?));
                }
            }
            if let Some(environment) = self.resolve_environment(statement.span())? {
                let key = self.lower_environment_key(root.span())?;
                return self.emit(
                    Instruction::SetTable {
                        table: environment,
                        key,
                        value: closure,
                    },
                    statement.span(),
                );
            }
            let name = self.global_name_constant(root.span())?;
            return self.emit(
                Instruction::StoreGlobal {
                    name,
                    source: closure,
                },
                statement.span(),
            );
        }

        let mut table = self.lower_identifier(root, statement.span())?;
        let field_count = statement.names().len();
        for field in &statement.names()[1..field_count - 1] {
            let key = self.lower_constant(
                Constant::String(copy_bytes(
                    self.source.slice(field.span())?,
                    "function path field",
                )?),
                field.span(),
            )?;
            let destination = self.allocate_register()?;
            self.emit(
                Instruction::GetTable {
                    destination,
                    table,
                    key,
                },
                field.span(),
            )?;
            table = destination;
        }
        let closure = self.lower_function(
            statement.function(),
            statement.span(),
            statement.is_method(),
        )?;
        let Some(field) = statement.names().last().copied() else {
            return Err(OwnedCompileError::InternalInvariant {
                message: "named function statement lost its final path component",
            });
        };
        let key = self.lower_constant(
            Constant::String(copy_bytes(
                self.source.slice(field.span())?,
                "function path field",
            )?),
            field.span(),
        )?;
        self.emit(
            Instruction::SetTable {
                table,
                key,
                value: closure,
            },
            statement.span(),
        )
    }

    fn lower_identifier(
        &mut self,
        identifier: Identifier,
        span: ByteSpan,
    ) -> Result<u16, OwnedCompileError> {
        if let Some(register) = self.resolve_local(identifier.span())? {
            return Ok(register);
        }
        let destination = self.allocate_register()?;
        if let Some(upvalue) = self.resolve_upvalue(identifier.span())? {
            self.emit(
                Instruction::GetUpvalue {
                    destination,
                    upvalue,
                },
                span,
            )?;
        } else if self.global_status(identifier.span())? == Some(false) {
            let message = self.undeclared_global_message(identifier.span())?;
            return Err(OwnedCompileError::Diagnostic(self.source_diagnostic(
                "BLU-COMPILE-0012",
                Phase::Lower,
                identifier.span(),
                &message,
            )?));
        } else if let Some(environment) = self.resolve_environment(span)? {
            let key = self.lower_environment_key(identifier.span())?;
            self.emit(
                Instruction::GetTable {
                    destination,
                    table: environment,
                    key,
                },
                span,
            )?;
        } else {
            let name = self.global_name_constant(identifier.span())?;
            self.emit(Instruction::LoadGlobal { destination, name }, span)?;
        }
        Ok(destination)
    }

    fn lower_assignment(
        &mut self,
        statement: AssignmentStatement,
    ) -> Result<(), OwnedCompileError> {
        let assignment_span = self.debug_assignment_span(statement.span())?;
        match statement.target() {
            AssignmentTarget::Identifier(identifier) => {
                self.ensure_writable(identifier.span())?;
                let source = self.lower_expression(statement.value())?;
                let local = self.resolve_local(identifier.span())?;
                let upvalue = if local.is_none() {
                    self.resolve_upvalue(identifier.span())?
                } else {
                    None
                };
                if local.is_none()
                    && upvalue.is_none()
                    && self.global_status(identifier.span())? == Some(false)
                {
                    let message = self.undeclared_global_message(identifier.span())?;
                    return Err(OwnedCompileError::Diagnostic(self.source_diagnostic(
                        "BLU-COMPILE-0012",
                        Phase::Lower,
                        identifier.span(),
                        &message,
                    )?));
                }
                if let Some(destination) = local {
                    self.emit(
                        Instruction::Move {
                            destination,
                            source,
                        },
                        assignment_span,
                    )
                } else if let Some(upvalue) = upvalue {
                    self.emit(Instruction::SetUpvalue { upvalue, source }, assignment_span)
                } else if let Some(environment) = self.resolve_environment(statement.span())? {
                    let key_start = self.code.len();
                    let key = self.lower_environment_key(identifier.span())?;
                    if assignment_span != statement.span() {
                        for span in &mut self.source_map[key_start..] {
                            *span = assignment_span;
                        }
                    }
                    self.emit(
                        Instruction::SetTable {
                            table: environment,
                            key,
                            value: source,
                        },
                        assignment_span,
                    )
                } else {
                    let name = self.global_name_constant(identifier.span())?;
                    self.emit(Instruction::StoreGlobal { name, source }, assignment_span)
                }
            }
            AssignmentTarget::Index(index) => {
                let table = self.lower_expression(index.table())?;
                let key = self.lower_expression(index.key())?;
                let value = self.lower_expression(statement.value())?;
                self.emit(Instruction::SetTable { table, key, value }, assignment_span)
            }
            AssignmentTarget::Field(field) => {
                let table = self.lower_expression(field.table())?;
                let key = self.lower_constant(
                    Constant::String(copy_bytes(
                        self.source.slice(field.name().span())?,
                        "field name",
                    )?),
                    field.name().span(),
                )?;
                let value = self.lower_expression(statement.value())?;
                self.emit(Instruction::SetTable { table, key, value }, assignment_span)
            }
        }
    }

    fn lower_compound_assignment(
        &mut self,
        statement: CompoundAssignmentStatement,
    ) -> Result<(), OwnedCompileError> {
        let (current, destination) = match statement.target() {
            AssignmentTarget::Identifier(identifier) => {
                self.ensure_writable(identifier.span())?;
                let current = self.lower_identifier(identifier, identifier.span())?;
                let destination = if let Some(register) = self.resolve_local(identifier.span())? {
                    AssignmentDestination::Local(register)
                } else if let Some(upvalue) = self.resolve_upvalue(identifier.span())? {
                    AssignmentDestination::Upvalue(upvalue)
                } else if let Some(environment) = self.resolve_environment(identifier.span())? {
                    let key = self.lower_environment_key(identifier.span())?;
                    AssignmentDestination::Table {
                        table: environment,
                        key,
                    }
                } else {
                    AssignmentDestination::Global(self.global_name_constant(identifier.span())?)
                };
                (current, destination)
            }
            AssignmentTarget::Index(index) => {
                let table = self.lower_expression(index.table())?;
                let table = self.snapshot_register(table, index.span())?;
                let key = self.lower_expression(index.key())?;
                let key = self.snapshot_register(key, index.span())?;
                let current = self.allocate_register()?;
                self.emit(
                    Instruction::GetTable {
                        destination: current,
                        table,
                        key,
                    },
                    index.span(),
                )?;
                (current, AssignmentDestination::Table { table, key })
            }
            AssignmentTarget::Field(field) => {
                let table = self.lower_expression(field.table())?;
                let table = self.snapshot_register(table, field.span())?;
                let key = self.lower_constant(
                    Constant::String(copy_bytes(
                        self.source.slice(field.name().span())?,
                        "compound assignment field name",
                    )?),
                    field.name().span(),
                )?;
                let current = self.allocate_register()?;
                self.emit(
                    Instruction::GetTable {
                        destination: current,
                        table,
                        key,
                    },
                    field.span(),
                )?;
                (current, AssignmentDestination::Table { table, key })
            }
        };
        let current = self.snapshot_register(current, statement.span())?;
        let right = self.lower_expression(statement.value())?;
        let value =
            self.emit_compound_operation(statement.operator(), current, right, statement.span())?;
        match destination {
            AssignmentDestination::Local(destination) => self.emit(
                Instruction::Move {
                    destination,
                    source: value,
                },
                statement.span(),
            ),
            AssignmentDestination::Upvalue(upvalue) => self.emit(
                Instruction::SetUpvalue {
                    upvalue,
                    source: value,
                },
                statement.span(),
            ),
            AssignmentDestination::Global(name) => self.emit(
                Instruction::StoreGlobal {
                    name,
                    source: value,
                },
                statement.span(),
            ),
            AssignmentDestination::Table { table, key } => self.emit(
                Instruction::SetTable { table, key, value },
                statement.span(),
            ),
        }
    }

    fn emit_compound_operation(
        &mut self,
        operator: CompoundAssignmentOperator,
        left: u16,
        right: u16,
        span: ByteSpan,
    ) -> Result<u16, OwnedCompileError> {
        let destination = self.allocate_register()?;
        let instruction = match operator {
            CompoundAssignmentOperator::Add => Instruction::Add {
                destination,
                left,
                right,
            },
            CompoundAssignmentOperator::Subtract => Instruction::Subtract {
                destination,
                left,
                right,
            },
            CompoundAssignmentOperator::Multiply => Instruction::Multiply {
                destination,
                left,
                right,
            },
            CompoundAssignmentOperator::Divide => Instruction::Divide {
                destination,
                left,
                right,
            },
            CompoundAssignmentOperator::FloorDivide => Instruction::FloorDivide {
                destination,
                left,
                right,
            },
            CompoundAssignmentOperator::Modulo => Instruction::Modulo {
                destination,
                left,
                right,
            },
            CompoundAssignmentOperator::Power => Instruction::Power {
                destination,
                left,
                right,
            },
            CompoundAssignmentOperator::Concatenate => Instruction::Concatenate {
                destination,
                left,
                right,
            },
        };
        self.emit(instruction, span)?;
        Ok(destination)
    }

    fn lower_local_list(
        &mut self,
        statement: &LocalListStatement,
    ) -> Result<(), OwnedCompileError> {
        let required = self
            .local_binding_count()
            .saturating_add(statement.names().len());
        let limit = self
            .limits
            .max_bindings
            .min(self.limits.artifact.max_debug_entries_per_prototype)
            .min(self.limits.artifact.max_total_debug_entries);
        check_limit(OwnedCompileLimit::Bindings, required, limit)?;

        if matches!(self.profile, SemanticProfile::Lua54 | SemanticProfile::Lua55)
            && statement
                .attributes()
                .iter()
                .filter(|attribute| **attribute == LocalAttribute::Close)
                .count()
                > 1
        {
            return Err(OwnedCompileError::Diagnostic(self.source_diagnostic(
                "BLU-COMPILE-0011",
                Phase::Lower,
                statement.span(),
                "multiple to-be-closed variables in local list",
            )?));
        }

        let capacity = statement.names().len().max(statement.values().len());
        let mut registers = allocate_vec(capacity, "local declaration registers")?;
        for (index, value) in statement.values().iter().copied().enumerate() {
            let is_last = index + 1 == statement.values().len();
            let requested = statement.names().len().saturating_sub(index);
            if is_last && requested > 1 {
                if let Some(results) = self.lower_call_expression_results(value, requested)? {
                    registers.extend(results);
                    continue;
                }
            }
            registers.push(self.lower_expression(value)?);
        }
        while registers.len() < statement.names().len() {
            let Some(name) = statement.names().get(registers.len()).copied() else {
                return Err(OwnedCompileError::InternalInvariant {
                    message: "local name/value adjustment index is out of bounds",
                });
            };
            registers.push(self.lower_constant(Constant::Nil, name.span())?);
        }
        let mut binding_registers =
            allocate_vec(statement.names().len(), "local binding registers")?;
        for source in registers.iter().copied().take(statement.names().len()) {
            binding_registers.push(self.snapshot_if_bound(source, statement.span())?);
        }
        let start_pc =
            u32::try_from(self.code.len()).map_err(|_| OwnedCompileError::InternalInvariant {
                message: "instruction count passed limits but cannot fit a debug PC",
            })?;
        for ((name, register), attribute) in statement
            .names()
            .iter()
            .copied()
            .zip(binding_registers)
            .zip(statement.attributes().iter().copied())
        {
            let to_close = attribute == LocalAttribute::Close;
            push_fallible(
                &mut self.bindings,
                Binding {
                    name: BindingName::Source(name.span()),
                    register,
                    constant: attribute == LocalAttribute::Const,
                    to_close,
                    start_pc,
                    end_pc: None,
                },
                "local bindings",
            )?;
            if to_close {
                self.emit_mark_close_value(register, statement.span())?;
            }
        }
        Ok(())
    }

    fn lower_assignment_list(
        &mut self,
        statement: &AssignmentListStatement,
    ) -> Result<(), OwnedCompileError> {
        let mut destinations = allocate_vec(statement.targets().len(), "assignment destinations")?;
        for target in statement.targets() {
            let destination = match *target {
                AssignmentTarget::Identifier(target) => {
                    self.ensure_writable(target.span())?;
                    if let Some(register) = self.resolve_local(target.span())? {
                        AssignmentDestination::Local(register)
                    } else if let Some(upvalue) = self.resolve_upvalue(target.span())? {
                        AssignmentDestination::Upvalue(upvalue)
                    } else if let Some(environment) = self.resolve_environment(target.span())? {
                        let key = self.lower_environment_key(target.span())?;
                        AssignmentDestination::Table {
                            table: environment,
                            key,
                        }
                    } else {
                        AssignmentDestination::Global(self.global_name_constant(target.span())?)
                    }
                }
                AssignmentTarget::Index(index) => {
                    let table = self.lower_expression(index.table())?;
                    let table = self.snapshot_register(table, index.span())?;
                    let key = self.lower_expression(index.key())?;
                    let key = self.snapshot_register(key, index.span())?;
                    AssignmentDestination::Table { table, key }
                }
                AssignmentTarget::Field(field) => {
                    let table = self.lower_expression(field.table())?;
                    let table = self.snapshot_register(table, field.span())?;
                    let key = self.lower_constant(
                        Constant::String(copy_bytes(
                            self.source.slice(field.name().span())?,
                            "assignment field name",
                        )?),
                        field.name().span(),
                    )?;
                    AssignmentDestination::Table { table, key }
                }
            };
            destinations.push(destination);
        }
        let capacity = statement.targets().len().max(statement.values().len());
        let mut sources = allocate_vec(capacity, "assignment source registers")?;
        for (index, value) in statement.values().iter().copied().enumerate() {
            let is_last = index + 1 == statement.values().len();
            let requested = destinations.len().saturating_sub(index);
            let mut values = if is_last && requested > 1 {
                self.lower_call_expression_results(value, requested)?
                    .unwrap_or_else(Vec::new)
            } else {
                Vec::new()
            };
            if values.is_empty() {
                values.push(self.lower_expression(value)?);
            }
            for source in values.into_iter().take(requested) {
                let snapshot = self.allocate_register()?;
                self.emit(
                    Instruction::Move {
                        destination: snapshot,
                        source,
                    },
                    self.expression(value)?.span(),
                )?;
                sources.push(snapshot);
            }
        }
        while sources.len() < destinations.len() {
            let Some(target) = statement.targets().get(sources.len()).copied() else {
                return Err(OwnedCompileError::InternalInvariant {
                    message: "assignment target/value adjustment index is out of bounds",
                });
            };
            sources.push(self.lower_constant(Constant::Nil, target.span())?);
        }
        // Lua evaluates every target and RHS before committing, then writes
        // the adjusted values from the final target back toward the first.
        // The reverse commit is observable for aliased table targets and
        // __newindex handlers, even though target/key/RHS evaluation remains
        // left-to-right above.
        for (destination, source) in destinations.into_iter().zip(sources).rev() {
            match destination {
                AssignmentDestination::Local(destination) => self.emit(
                    Instruction::Move {
                        destination,
                        source,
                    },
                    statement.span(),
                )?,
                AssignmentDestination::Upvalue(upvalue) => self.emit(
                    Instruction::SetUpvalue { upvalue, source },
                    statement.span(),
                )?,
                AssignmentDestination::Global(name) => {
                    self.emit(Instruction::StoreGlobal { name, source }, statement.span())?
                }
                AssignmentDestination::Table { table, key } => self.emit(
                    Instruction::SetTable {
                        table,
                        key,
                        value: source,
                    },
                    statement.span(),
                )?,
            }
        }
        Ok(())
    }

    fn snapshot_register(&mut self, source: u16, span: ByteSpan) -> Result<u16, OwnedCompileError> {
        let destination = self.allocate_register()?;
        self.emit(
            Instruction::Move {
                destination,
                source,
            },
            span,
        )?;
        Ok(destination)
    }

    fn snapshot_if_bound(&mut self, source: u16, span: ByteSpan) -> Result<u16, OwnedCompileError> {
        if self
            .bindings
            .iter()
            .any(|binding| binding.register == source)
        {
            self.snapshot_register(source, span)
        } else {
            Ok(source)
        }
    }

    fn lower_return(&mut self, statement: &ReturnStatement) -> Result<(), OwnedCompileError> {
        let values = statement.values();
        let has_to_close = self.bindings.iter().any(|binding| binding.to_close);
        let limit = self.limits.max_return_values.min(u16::MAX as usize);
        check_limit(OwnedCompileLimit::ReturnValues, values.len(), limit)?;
        if values.is_empty() {
            self.emit_close_bindings(0, statement.span())?;
            return self.emit(Instruction::Return { first: 0, count: 0 }, statement.span());
        }
        let last = values[values.len() - 1];
        if matches!(self.expression(last)?.kind(), ExpressionKind::Vararg) {
            let prefix_expressions = &values[..values.len() - 1];
            let mut prefix_registers =
                allocate_vec(prefix_expressions.len(), "vararg return prefix registers")?;
            for expression in prefix_expressions.iter().copied() {
                prefix_registers.push(self.lower_expression(expression)?);
            }
            let first = if prefix_registers.is_empty() {
                0
            } else if has_to_close {
                self.copy_return_values(prefix_expressions, &prefix_registers)?
            } else if prefix_registers
                .windows(2)
                .all(|pair| pair[0].checked_add(1) == Some(pair[1]))
            {
                prefix_registers[0]
            } else {
                self.copy_return_values(prefix_expressions, &prefix_registers)?
            };
            let count = u16::try_from(prefix_expressions.len()).map_err(|_| {
                OwnedCompileError::InternalInvariant {
                    message: "vararg return prefix passed limits but cannot fit BluV1",
                }
            })?;
            self.emit_close_bindings(0, statement.span())?;
            return self.emit(
                Instruction::ReturnVarargs { first, count },
                statement.span(),
            );
        }
        if !has_to_close
            && matches!(
                self.expression(last)?.kind(),
                ExpressionKind::Call(_) | ExpressionKind::MethodCall(_)
            )
        {
            if values.len() == 1 {
                if self.lower_return_call_expression(last, None)? {
                    return Ok(());
                }
            } else {
                let prefix_expressions = &values[..values.len() - 1];
                let mut prefix_registers =
                    allocate_vec(prefix_expressions.len(), "return prefix registers")?;
                for expression in prefix_expressions.iter().copied() {
                    prefix_registers.push(self.lower_expression(expression)?);
                }
                let contiguous = prefix_registers
                    .windows(2)
                    .all(|pair| pair[0].checked_add(1) == Some(pair[1]));
                let first = if contiguous {
                    prefix_registers[0]
                } else {
                    self.copy_return_values(prefix_expressions, &prefix_registers)?
                };
                let count = u16::try_from(prefix_expressions.len()).map_err(|_| {
                    OwnedCompileError::InternalInvariant {
                        message: "return prefix count passed limits but cannot fit BluV1",
                    }
                })?;
                if self.lower_return_call_expression(last, Some((first, count)))? {
                    return Ok(());
                }
            }
        }
        let mut registers = allocate_vec(values.len(), "return registers")?;
        for expression_id in values.iter().copied() {
            registers.push(self.lower_expression(expression_id)?);
        }
        let contiguous = registers
            .windows(2)
            .all(|pair| pair[0].checked_add(1) == Some(pair[1]));
        let first = if has_to_close {
            self.copy_return_values(values, &registers)?
        } else if contiguous {
            registers[0]
        } else {
            self.copy_return_values(values, &registers)?
        };
        let count =
            u16::try_from(values.len()).map_err(|_| OwnedCompileError::InternalInvariant {
                message: "return count passed limits but cannot fit BluV1",
            })?;
        self.emit_close_bindings(0, statement.span())?;
        self.emit(Instruction::Return { first, count }, statement.span())
    }

    fn copy_return_values(
        &mut self,
        values: &[ExpressionId],
        registers: &[u16],
    ) -> Result<u16, OwnedCompileError> {
        let mut first = None;
        for (expression_id, source) in values.iter().copied().zip(registers.iter().copied()) {
            let destination = self.allocate_register()?;
            first.get_or_insert(destination);
            self.emit(
                Instruction::Move {
                    destination,
                    source,
                },
                self.expression(expression_id)?.span(),
            )?;
        }
        first.ok_or(OwnedCompileError::InternalInvariant {
            message: "non-contiguous empty return list reached copy lowering",
        })
    }

    fn lower_table_fields(
        &mut self,
        table: u16,
        constructor: TableConstructor,
    ) -> Result<(), OwnedCompileError> {
        let end = constructor
            .first_field()
            .checked_add(constructor.field_count())
            .ok_or(OwnedCompileError::InternalInvariant {
                message: "table constructor field range overflows",
            })?;
        if end > self.table_fields.len() {
            return Err(OwnedCompileError::InternalInvariant {
                message: "table constructor field range is out of bounds",
            });
        }
        let mut reverse_constant_hash_fields =
            self.profile != SemanticProfile::Luau && constructor.field_count() > 1;
        if reverse_constant_hash_fields {
            for index in constructor.first_field()..end {
                let TableField::Named { name, value } = self.table_fields[index] else {
                    reverse_constant_hash_fields = false;
                    break;
                };
                if !matches!(
                    self.expression(value)?.kind(),
                    ExpressionKind::Nil
                        | ExpressionKind::Boolean(_)
                        | ExpressionKind::DecimalInteger
                        | ExpressionKind::DecimalNumber
                        | ExpressionKind::HexInteger
                        | ExpressionKind::HexNumber
                        | ExpressionKind::BinaryInteger
                        | ExpressionKind::StringLiteral
                ) {
                    reverse_constant_hash_fields = false;
                    break;
                }
                let name = self.source.slice(name.span())?;
                if (constructor.first_field()..index).any(|previous| {
                    let TableField::Named {
                        name: previous_name,
                        ..
                    } = self.table_fields[previous]
                    else {
                        return false;
                    };
                    self.source
                        .slice(previous_name.span())
                        .is_ok_and(|previous_name| previous_name == name)
                }) {
                    reverse_constant_hash_fields = false;
                    break;
                }
            }
        }
        let mut field_indices = allocate_vec(constructor.field_count(), "table field indices")?;
        if reverse_constant_hash_fields {
            field_indices.extend((constructor.first_field()..end).rev());
        } else {
            field_indices.extend(constructor.first_field()..end);
        }
        let mut array_index = 1_i64;
        for index in field_indices {
            let field = self.table_fields[index];
            if index + 1 == end
                && let TableField::Array(value) = field
            {
                let expression = *self.expression(value)?;
                let call = match expression.kind() {
                    ExpressionKind::Call(call) => {
                        let argument_end =
                            self.call_argument_end(call.first_argument(), call.argument_count())?;
                        let expands_varargs = argument_end > call.first_argument()
                            && matches!(
                                self.expression(self.call_arguments[argument_end - 1])?
                                    .kind(),
                                ExpressionKind::Vararg
                            );
                        let expands_call = !expands_varargs
                            && argument_end > call.first_argument()
                            && matches!(
                                self.expression(self.call_arguments[argument_end - 1])?
                                    .kind(),
                                ExpressionKind::Call(_) | ExpressionKind::MethodCall(_)
                            );
                        if expands_varargs && !self.is_vararg {
                            return Err(OwnedCompileError::Diagnostic(
                                self.source_diagnostic(
                                    "BLU-COMPILE-0006",
                                    Phase::Lower,
                                    self.expression(self.call_arguments[argument_end - 1])?
                                        .span(),
                                    "vararg expression is outside a variadic function",
                                )?,
                            ));
                        }
                        let function = self.lower_expression(call.function())?;
                        let fixed_end = argument_end - usize::from(expands_varargs || expands_call);
                        let mut sources =
                            allocate_vec(call.argument_count(), "table call argument registers")?;
                        for argument in call.first_argument()..fixed_end {
                            sources.push(self.lower_expression(self.call_arguments[argument])?);
                        }
                        let dynamic_argument = if expands_call {
                            Some(self.call_arguments[argument_end - 1])
                        } else {
                            None
                        };
                        Some((
                            function,
                            sources,
                            expands_varargs,
                            dynamic_argument,
                            call.span(),
                        ))
                    }
                    ExpressionKind::MethodCall(call) => {
                        let argument_end =
                            self.call_argument_end(call.first_argument(), call.argument_count())?;
                        let expands_varargs = argument_end > call.first_argument()
                            && matches!(
                                self.expression(self.call_arguments[argument_end - 1])?
                                    .kind(),
                                ExpressionKind::Vararg
                            );
                        let expands_call = !expands_varargs
                            && argument_end > call.first_argument()
                            && matches!(
                                self.expression(self.call_arguments[argument_end - 1])?
                                    .kind(),
                                ExpressionKind::Call(_) | ExpressionKind::MethodCall(_)
                            );
                        if expands_varargs && !self.is_vararg {
                            return Err(OwnedCompileError::Diagnostic(
                                self.source_diagnostic(
                                    "BLU-COMPILE-0006",
                                    Phase::Lower,
                                    self.expression(self.call_arguments[argument_end - 1])?
                                        .span(),
                                    "vararg expression is outside a variadic function",
                                )?,
                            ));
                        }
                        let receiver = self.lower_expression(call.receiver())?;
                        let key = self.lower_owned_method_name(call.method().span())?;
                        let function = self.allocate_register()?;
                        self.emit(
                            Instruction::GetTable {
                                destination: function,
                                table: receiver,
                                key,
                            },
                            call.span(),
                        )?;
                        let fixed_end = argument_end - usize::from(expands_varargs || expands_call);
                        let mut sources = allocate_vec(
                            call.argument_count().saturating_add(1),
                            "table method argument registers",
                        )?;
                        sources.push(receiver);
                        for argument in call.first_argument()..fixed_end {
                            sources.push(self.lower_expression(self.call_arguments[argument])?);
                        }
                        let dynamic_argument = if expands_call {
                            Some(self.call_arguments[argument_end - 1])
                        } else {
                            None
                        };
                        Some((
                            function,
                            sources,
                            expands_varargs,
                            dynamic_argument,
                            call.span(),
                        ))
                    }
                    _ => None,
                };
                if let Some((function, sources, expands_varargs, dynamic_argument, span)) = call {
                    let start = u32::try_from(array_index).map_err(|_| {
                        OwnedCompileError::InternalInvariant {
                            message: "call table-list start passed limits but cannot fit BluV1",
                        }
                    })?;
                    let (arguments, argument_count) = self.copy_call_arguments(&sources, span)?;
                    if let Some(dynamic_argument) = dynamic_argument {
                        self.lower_all_call_results(dynamic_argument)?;
                    }
                    let instruction = if dynamic_argument.is_some() {
                        Instruction::SetListCallDynamic {
                            table,
                            start,
                            function,
                            arguments,
                            argument_count,
                        }
                    } else if expands_varargs {
                        Instruction::SetListCallVarargs {
                            table,
                            start,
                            function,
                            arguments,
                            argument_count,
                        }
                    } else {
                        Instruction::SetListCall {
                            table,
                            start,
                            function,
                            arguments,
                            argument_count,
                        }
                    };
                    self.emit(instruction, span)?;
                    continue;
                }
            }
            if index + 1 == end
                && matches!(
                    field,
                    TableField::Array(value)
                        if matches!(self.expression(value)?.kind(), ExpressionKind::Vararg)
                )
            {
                let TableField::Array(value) = field else {
                    unreachable!()
                };
                if !self.is_vararg {
                    return Err(OwnedCompileError::Diagnostic(self.source_diagnostic(
                        "BLU-COMPILE-0006",
                        Phase::Lower,
                        self.expression(value)?.span(),
                        "vararg expression is outside a variadic function",
                    )?));
                }
                let start = u32::try_from(array_index).map_err(|_| {
                    OwnedCompileError::InternalInvariant {
                        message: "vararg table-list start passed limits but cannot fit BluV1",
                    }
                })?;
                self.emit(
                    Instruction::SetListVarargs { table, start },
                    self.expression(value)?.span(),
                )?;
                continue;
            }
            let (key, value, span) = match field {
                TableField::Array(value) => {
                    let span = self.expression(value)?.span();
                    let constant = if matches!(
                        self.profile,
                        SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
                    ) {
                        Constant::Integer(array_index)
                    } else {
                        Constant::Number(array_index as f64)
                    };
                    array_index =
                        array_index
                            .checked_add(1)
                            .ok_or(OwnedCompileError::InternalInvariant {
                                message: "table array field index overflows i64",
                            })?;
                    (self.lower_constant(constant, span)?, value, span)
                }
                TableField::Named { name, value } => {
                    let span = self.expression(value)?.span();
                    let key = self.lower_constant(
                        Constant::String(copy_bytes(
                            self.source.slice(name.span())?,
                            "table field name",
                        )?),
                        name.span(),
                    )?;
                    (key, value, span)
                }
                TableField::Indexed { key, value } => {
                    let span = self.expression(value)?.span();
                    (self.lower_expression(key)?, value, span)
                }
            };
            let value = self.lower_expression(value)?;
            self.emit(Instruction::SetTable { table, key, value }, span)?;
        }
        Ok(())
    }

    fn lower_call(&mut self, call: CallExpression) -> Result<u16, OwnedCompileError> {
        let mut results = self.lower_call_results(call, 1)?;
        results.pop().ok_or(OwnedCompileError::InternalInvariant {
            message: "single-result call did not allocate a result register",
        })
    }

    fn lower_call_results(
        &mut self,
        call: CallExpression,
        result_count: usize,
    ) -> Result<Vec<u16>, OwnedCompileError> {
        let end = self.call_argument_end(call.first_argument(), call.argument_count())?;
        let expands_varargs = if let Some(argument) =
            self.call_arguments.get(end.saturating_sub(1)).copied()
            && end > call.first_argument()
            && matches!(self.expression(argument)?.kind(), ExpressionKind::Vararg)
        {
            true
        } else {
            false
        };
        if expands_varargs && !self.is_vararg {
            return Err(OwnedCompileError::Diagnostic(self.source_diagnostic(
                "BLU-COMPILE-0006",
                Phase::Lower,
                self.expression(self.call_arguments[end - 1])?.span(),
                "vararg expression is outside a variadic function",
            )?));
        }
        let expands_call = !expands_varargs
            && end > call.first_argument()
            && matches!(
                self.expression(self.call_arguments[end - 1])?.kind(),
                ExpressionKind::Call(_) | ExpressionKind::MethodCall(_)
            );
        let function = self.lower_expression(call.function())?;
        let mut sources = allocate_vec(call.argument_count(), "call argument registers")?;
        let fixed_end = end - usize::from(expands_varargs || expands_call);
        for index in call.first_argument()..fixed_end {
            sources.push(self.lower_expression(self.call_arguments[index])?);
        }
        if expands_call {
            let (arguments, argument_count) = self.copy_call_arguments(&sources, call.span())?;
            self.lower_all_call_results(self.call_arguments[end - 1])?;
            return self.emit_dynamic_call_results(
                function,
                arguments,
                argument_count,
                result_count,
                call.span(),
            );
        }
        self.emit_fixed_call_results(
            function,
            &sources,
            result_count,
            expands_varargs,
            call.span(),
        )
    }

    fn lower_method_call(&mut self, call: MethodCallExpression) -> Result<u16, OwnedCompileError> {
        let mut results = self.lower_method_call_results(call, 1)?;
        results.pop().ok_or(OwnedCompileError::InternalInvariant {
            message: "single-result method call did not allocate a result register",
        })
    }

    fn lower_owned_method_name(&mut self, span: ByteSpan) -> Result<u16, OwnedCompileError> {
        let method_name = self.source.slice(span)?;
        let method_name = if self.profile == SemanticProfile::Luau {
            let mut marked = allocate_vec(
                method_name.len().saturating_add(1),
                "Luau owned method name",
            )?;
            marked.push(LUAU_OWNED_NAMECALL_MARKER);
            marked.extend_from_slice(method_name);
            marked
        } else {
            copy_bytes(method_name, "method name")?
        };
        self.lower_constant(Constant::String(method_name), span)
    }

    fn lower_method_call_results(
        &mut self,
        call: MethodCallExpression,
        result_count: usize,
    ) -> Result<Vec<u16>, OwnedCompileError> {
        let end = self.call_argument_end(call.first_argument(), call.argument_count())?;
        let expands_varargs = if let Some(argument) =
            self.call_arguments.get(end.saturating_sub(1)).copied()
            && end > call.first_argument()
            && matches!(self.expression(argument)?.kind(), ExpressionKind::Vararg)
        {
            true
        } else {
            false
        };
        if expands_varargs && !self.is_vararg {
            return Err(OwnedCompileError::Diagnostic(self.source_diagnostic(
                "BLU-COMPILE-0006",
                Phase::Lower,
                self.expression(self.call_arguments[end - 1])?.span(),
                "vararg expression is outside a variadic function",
            )?));
        }
        let expands_call = !expands_varargs
            && end > call.first_argument()
            && matches!(
                self.expression(self.call_arguments[end - 1])?.kind(),
                ExpressionKind::Call(_) | ExpressionKind::MethodCall(_)
            );
        let receiver = self.lower_expression(call.receiver())?;
        let key = self.lower_owned_method_name(call.method().span())?;
        let function = self.allocate_register()?;
        self.emit(
            Instruction::GetTable {
                destination: function,
                table: receiver,
                key,
            },
            call.span(),
        )?;
        let source_count =
            call.argument_count()
                .checked_add(1)
                .ok_or(OwnedCompileError::InternalInvariant {
                    message: "method call argument count overflows",
                })?;
        let mut sources = allocate_vec(source_count, "method call argument registers")?;
        sources.push(receiver);
        let fixed_end = end - usize::from(expands_varargs || expands_call);
        for index in call.first_argument()..fixed_end {
            sources.push(self.lower_expression(self.call_arguments[index])?);
        }
        if expands_call {
            let (arguments, argument_count) = self.copy_call_arguments(&sources, call.span())?;
            self.lower_all_call_results(self.call_arguments[end - 1])?;
            return self.emit_dynamic_call_results(
                function,
                arguments,
                argument_count,
                result_count,
                call.span(),
            );
        }
        self.emit_fixed_call_results(
            function,
            &sources,
            result_count,
            expands_varargs,
            call.span(),
        )
    }

    fn lower_call_expression_results(
        &mut self,
        id: ExpressionId,
        result_count: usize,
    ) -> Result<Option<Vec<u16>>, OwnedCompileError> {
        let expression = *self.expression(id)?;
        match expression.kind() {
            ExpressionKind::Call(call) => self.lower_call_results(call, result_count).map(Some),
            ExpressionKind::MethodCall(call) => {
                self.lower_method_call_results(call, result_count).map(Some)
            }
            ExpressionKind::Vararg => {
                if !self.is_vararg {
                    return Err(OwnedCompileError::Diagnostic(self.source_diagnostic(
                        "BLU-COMPILE-0006",
                        Phase::Lower,
                        expression.span(),
                        "vararg expression is outside a variadic function",
                    )?));
                }
                let result_count = u16::try_from(result_count).map_err(|_| {
                    OwnedCompileError::InternalInvariant {
                        message: "vararg result count passed limits but cannot fit BluV1",
                    }
                })?;
                let mut results =
                    allocate_vec(usize::from(result_count), "vararg result registers")?;
                let destination = self.allocate_register()?;
                results.push(destination);
                for _ in 1..result_count {
                    results.push(self.allocate_register()?);
                }
                self.emit(
                    Instruction::Varargs {
                        destination,
                        count: result_count,
                    },
                    expression.span(),
                )?;
                Ok(Some(results))
            }
            _ => Ok(None),
        }
    }

    fn lower_all_call_results(&mut self, id: ExpressionId) -> Result<(), OwnedCompileError> {
        let expression = *self.expression(id)?;
        let (function, sources, expands_varargs, dynamic_argument, span) = match expression.kind() {
            ExpressionKind::Call(call) => {
                let end = self.call_argument_end(call.first_argument(), call.argument_count())?;
                let expands_varargs = end > call.first_argument()
                    && matches!(
                        self.expression(self.call_arguments[end - 1])?.kind(),
                        ExpressionKind::Vararg
                    );
                let expands_call = !expands_varargs
                    && end > call.first_argument()
                    && matches!(
                        self.expression(self.call_arguments[end - 1])?.kind(),
                        ExpressionKind::Call(_) | ExpressionKind::MethodCall(_)
                    );
                let function = self.lower_expression(call.function())?;
                let mut sources =
                    allocate_vec(call.argument_count(), "dynamic call argument registers")?;
                let fixed_end = end - usize::from(expands_varargs || expands_call);
                for index in call.first_argument()..fixed_end {
                    sources.push(self.lower_expression(self.call_arguments[index])?);
                }
                (
                    function,
                    sources,
                    expands_varargs,
                    if expands_call {
                        Some(self.call_arguments[end - 1])
                    } else {
                        None
                    },
                    call.span(),
                )
            }
            ExpressionKind::MethodCall(call) => {
                let end = self.call_argument_end(call.first_argument(), call.argument_count())?;
                let expands_varargs = end > call.first_argument()
                    && matches!(
                        self.expression(self.call_arguments[end - 1])?.kind(),
                        ExpressionKind::Vararg
                    );
                let expands_call = !expands_varargs
                    && end > call.first_argument()
                    && matches!(
                        self.expression(self.call_arguments[end - 1])?.kind(),
                        ExpressionKind::Call(_) | ExpressionKind::MethodCall(_)
                    );
                let receiver = self.lower_expression(call.receiver())?;
                let key = self.lower_owned_method_name(call.method().span())?;
                let function = self.allocate_register()?;
                self.emit(
                    Instruction::GetTable {
                        destination: function,
                        table: receiver,
                        key,
                    },
                    call.span(),
                )?;
                let source_count = call.argument_count().checked_add(1).ok_or(
                    OwnedCompileError::InternalInvariant {
                        message: "dynamic method argument count overflows",
                    },
                )?;
                let mut sources = allocate_vec(source_count, "dynamic method argument registers")?;
                sources.push(receiver);
                let fixed_end = end - usize::from(expands_varargs || expands_call);
                for index in call.first_argument()..fixed_end {
                    sources.push(self.lower_expression(self.call_arguments[index])?);
                }
                (
                    function,
                    sources,
                    expands_varargs,
                    if expands_call {
                        Some(self.call_arguments[end - 1])
                    } else {
                        None
                    },
                    call.span(),
                )
            }
            _ => {
                return Err(OwnedCompileError::InternalInvariant {
                    message: "dynamic result producer is not a call",
                });
            }
        };
        if expands_varargs && !self.is_vararg {
            return Err(OwnedCompileError::Diagnostic(self.source_diagnostic(
                "BLU-COMPILE-0006",
                Phase::Lower,
                expression.span(),
                "vararg expression is outside a variadic function",
            )?));
        }
        let (arguments, argument_count) = self.copy_call_arguments(&sources, span)?;
        if let Some(argument) = dynamic_argument {
            self.lower_all_call_results(argument)?;
        }
        self.emit(
            if dynamic_argument.is_some() {
                Instruction::CallDynamicAllResults {
                    function,
                    arguments,
                    argument_count,
                }
            } else if expands_varargs {
                Instruction::CallVarargsAllResults {
                    function,
                    arguments,
                    argument_count,
                }
            } else {
                Instruction::CallAllResults {
                    function,
                    arguments,
                    argument_count,
                }
            },
            span,
        )
    }

    fn emit_dynamic_call_results(
        &mut self,
        function: u16,
        arguments: u16,
        argument_count: u16,
        result_count: usize,
        span: ByteSpan,
    ) -> Result<Vec<u16>, OwnedCompileError> {
        let destination = self.allocate_register()?;
        let result_count =
            u16::try_from(result_count).map_err(|_| OwnedCompileError::InternalInvariant {
                message: "dynamic call result count passed limits but cannot fit BluV1",
            })?;
        let mut results = allocate_vec(usize::from(result_count), "dynamic call result registers")?;
        results.push(destination);
        for _ in 1..result_count {
            results.push(self.allocate_register()?);
        }
        self.emit(
            Instruction::CallDynamicResults {
                destination,
                function,
                arguments,
                argument_count,
                result_count,
            },
            span,
        )?;
        Ok(results)
    }

    fn lower_return_call_expression(
        &mut self,
        id: ExpressionId,
        prefix: Option<(u16, u16)>,
    ) -> Result<bool, OwnedCompileError> {
        let expression = *self.expression(id)?;
        let (function, sources, expands_varargs, dynamic_argument, span) = match expression.kind() {
            ExpressionKind::Call(call) => {
                let end = self.call_argument_end(call.first_argument(), call.argument_count())?;
                let expands_varargs = end > call.first_argument()
                    && matches!(
                        self.expression(self.call_arguments[end - 1])?.kind(),
                        ExpressionKind::Vararg
                    );
                if expands_varargs && !self.is_vararg {
                    return Err(OwnedCompileError::Diagnostic(self.source_diagnostic(
                        "BLU-COMPILE-0006",
                        Phase::Lower,
                        self.expression(self.call_arguments[end - 1])?.span(),
                        "vararg expression is outside a variadic function",
                    )?));
                }
                let expands_call = !expands_varargs
                    && end > call.first_argument()
                    && matches!(
                        self.expression(self.call_arguments[end - 1])?.kind(),
                        ExpressionKind::Call(_) | ExpressionKind::MethodCall(_)
                    );
                let function = self.lower_expression(call.function())?;
                let mut sources =
                    allocate_vec(call.argument_count(), "return call argument registers")?;
                let fixed_end = end - usize::from(expands_varargs || expands_call);
                for index in call.first_argument()..fixed_end {
                    sources.push(self.lower_expression(self.call_arguments[index])?);
                }
                (
                    function,
                    sources,
                    expands_varargs,
                    if expands_call {
                        Some(self.call_arguments[end - 1])
                    } else {
                        None
                    },
                    call.span(),
                )
            }
            ExpressionKind::MethodCall(call) => {
                let end = self.call_argument_end(call.first_argument(), call.argument_count())?;
                let expands_varargs = end > call.first_argument()
                    && matches!(
                        self.expression(self.call_arguments[end - 1])?.kind(),
                        ExpressionKind::Vararg
                    );
                if expands_varargs && !self.is_vararg {
                    return Err(OwnedCompileError::Diagnostic(self.source_diagnostic(
                        "BLU-COMPILE-0006",
                        Phase::Lower,
                        self.expression(self.call_arguments[end - 1])?.span(),
                        "vararg expression is outside a variadic function",
                    )?));
                }
                let expands_call = !expands_varargs
                    && end > call.first_argument()
                    && matches!(
                        self.expression(self.call_arguments[end - 1])?.kind(),
                        ExpressionKind::Call(_) | ExpressionKind::MethodCall(_)
                    );
                let receiver = self.lower_expression(call.receiver())?;
                let key = self.lower_owned_method_name(call.method().span())?;
                let function = self.allocate_register()?;
                self.emit(
                    Instruction::GetTable {
                        destination: function,
                        table: receiver,
                        key,
                    },
                    call.span(),
                )?;
                let source_count = call.argument_count().checked_add(1).ok_or(
                    OwnedCompileError::InternalInvariant {
                        message: "return method call argument count overflows",
                    },
                )?;
                let mut sources =
                    allocate_vec(source_count, "return method call argument registers")?;
                sources.push(receiver);
                let fixed_end = end - usize::from(expands_varargs || expands_call);
                for index in call.first_argument()..fixed_end {
                    sources.push(self.lower_expression(self.call_arguments[index])?);
                }
                (
                    function,
                    sources,
                    expands_varargs,
                    if expands_call {
                        Some(self.call_arguments[end - 1])
                    } else {
                        None
                    },
                    call.span(),
                )
            }
            _ => return Ok(false),
        };
        let (arguments, argument_count) = self.copy_call_arguments(&sources, span)?;
        if let Some(argument) = dynamic_argument {
            self.lower_all_call_results(argument)?;
        }
        let instruction = if dynamic_argument.is_some() {
            if let Some((first, count)) = prefix {
                Instruction::ReturnCallDynamicPrefix {
                    first,
                    count,
                    function,
                    arguments,
                    argument_count,
                }
            } else {
                Instruction::ReturnCallDynamic {
                    function,
                    arguments,
                    argument_count,
                }
            }
        } else if let (true, Some((first, count))) = (expands_varargs, prefix) {
            Instruction::ReturnCallVarargsPrefix {
                first,
                count,
                function,
                arguments,
                argument_count,
            }
        } else if expands_varargs {
            Instruction::ReturnCallVarargs {
                function,
                arguments,
                argument_count,
            }
        } else if let Some((first, count)) = prefix {
            Instruction::ReturnCallPrefix {
                first,
                count,
                function,
                arguments,
                argument_count,
            }
        } else {
            Instruction::ReturnCall {
                function,
                arguments,
                argument_count,
            }
        };
        self.emit(instruction, span)?;
        Ok(true)
    }

    fn call_argument_end(
        &self,
        first_argument: usize,
        argument_count: usize,
    ) -> Result<usize, OwnedCompileError> {
        let end = first_argument.checked_add(argument_count).ok_or(
            OwnedCompileError::InternalInvariant {
                message: "call argument range overflows",
            },
        )?;
        if end > self.call_arguments.len() {
            return Err(OwnedCompileError::InternalInvariant {
                message: "call argument range is out of bounds",
            });
        }
        Ok(end)
    }

    fn emit_fixed_call_results(
        &mut self,
        function: u16,
        sources: &[u16],
        result_count: usize,
        expands_varargs: bool,
        span: ByteSpan,
    ) -> Result<Vec<u16>, OwnedCompileError> {
        let limit = self
            .limits
            .artifact
            .max_registers_per_prototype
            .min(u16::MAX as usize);
        check_limit(OwnedCompileLimit::CallArguments, sources.len(), limit)?;
        check_limit(OwnedCompileLimit::ReturnValues, result_count, limit)?;
        let (arguments, argument_count) = self.copy_call_arguments(sources, span)?;
        let destination = self.allocate_register()?;
        let result_count =
            u16::try_from(result_count).map_err(|_| OwnedCompileError::InternalInvariant {
                message: "call result count passed limits but cannot fit BluV1",
            })?;
        let mut results = allocate_vec(usize::from(result_count), "call result registers")?;
        results.push(destination);
        for _ in 1..result_count {
            results.push(self.allocate_register()?);
        }
        if expands_varargs {
            self.emit(
                Instruction::CallVarargsResults {
                    destination,
                    function,
                    arguments,
                    argument_count,
                    result_count,
                },
                span,
            )?;
        } else if result_count == 1 {
            self.emit(
                Instruction::Call {
                    destination,
                    function,
                    arguments,
                    argument_count,
                },
                span,
            )?;
        } else {
            self.emit(
                Instruction::CallResults {
                    destination,
                    function,
                    arguments,
                    argument_count,
                    result_count,
                },
                span,
            )?;
        }
        Ok(results)
    }

    fn copy_call_arguments(
        &mut self,
        sources: &[u16],
        span: ByteSpan,
    ) -> Result<(u16, u16), OwnedCompileError> {
        let limit = self
            .limits
            .artifact
            .max_registers_per_prototype
            .min(u16::MAX as usize);
        check_limit(OwnedCompileLimit::CallArguments, sources.len(), limit)?;
        let arguments = if sources.is_empty() {
            0
        } else {
            let first = self.allocate_register()?;
            self.emit(
                Instruction::Move {
                    destination: first,
                    source: sources[0],
                },
                span,
            )?;
            for source in sources.iter().copied().skip(1) {
                let destination = self.allocate_register()?;
                self.emit(
                    Instruction::Move {
                        destination,
                        source,
                    },
                    span,
                )?;
            }
            first
        };
        let argument_count =
            u16::try_from(sources.len()).map_err(|_| OwnedCompileError::InternalInvariant {
                message: "call argument count passed limits but cannot fit BluV1",
            })?;
        Ok((arguments, argument_count))
    }

    fn lower_expression(&mut self, id: ExpressionId) -> Result<u16, OwnedCompileError> {
        let expression = *self.expression(id)?;
        match expression.kind() {
            ExpressionKind::Nil => self.lower_constant(Constant::Nil, expression.span()),
            ExpressionKind::Vararg => {
                let mut results = self.lower_call_expression_results(id, 1)?.ok_or(
                    OwnedCompileError::InternalInvariant {
                        message: "vararg expression did not lower as adjusted results",
                    },
                )?;
                results.pop().ok_or(OwnedCompileError::InternalInvariant {
                    message: "scalar vararg lowering returned no register",
                })
            }
            ExpressionKind::Boolean(value) => {
                self.lower_constant(Constant::Boolean(value), expression.span())
            }
            ExpressionKind::DecimalInteger => {
                let constant = self.decimal_constant(expression.span())?;
                let constant_index = self.push_constant(constant)?;
                let destination = self.allocate_register()?;
                self.emit(
                    Instruction::LoadConstant {
                        destination,
                        constant: constant_index,
                    },
                    expression.span(),
                )?;
                Ok(destination)
            }
            ExpressionKind::DecimalNumber => {
                let constant = self.decimal_number_constant(expression.span())?;
                let constant_index = self.push_constant(constant)?;
                let destination = self.allocate_register()?;
                self.emit(
                    Instruction::LoadConstant {
                        destination,
                        constant: constant_index,
                    },
                    expression.span(),
                )?;
                Ok(destination)
            }
            ExpressionKind::HexInteger => {
                let constant = self.hex_integer_constant(expression.span())?;
                let constant_index = self.push_constant(constant)?;
                let destination = self.allocate_register()?;
                self.emit(
                    Instruction::LoadConstant {
                        destination,
                        constant: constant_index,
                    },
                    expression.span(),
                )?;
                Ok(destination)
            }
            ExpressionKind::HexNumber => {
                let constant = self.hex_number_constant(expression.span())?;
                let constant_index = self.push_constant(constant)?;
                let destination = self.allocate_register()?;
                self.emit(
                    Instruction::LoadConstant {
                        destination,
                        constant: constant_index,
                    },
                    expression.span(),
                )?;
                Ok(destination)
            }
            ExpressionKind::BinaryInteger => {
                let constant = self.binary_integer_constant(expression.span())?;
                let constant_index = self.push_constant(constant)?;
                let destination = self.allocate_register()?;
                self.emit(
                    Instruction::LoadConstant {
                        destination,
                        constant: constant_index,
                    },
                    expression.span(),
                )?;
                Ok(destination)
            }
            ExpressionKind::StringLiteral => {
                let constant = self.string_constant(expression.span())?;
                self.lower_constant(constant, expression.span())
            }
            ExpressionKind::InterpolatedString(string) => {
                self.lower_interpolated_string(string, expression.span())
            }
            ExpressionKind::Table(constructor) => {
                let destination = self.allocate_register()?;
                self.emit(Instruction::NewTable { destination }, expression.span())?;
                self.lower_table_fields(destination, constructor)?;
                Ok(destination)
            }
            ExpressionKind::Identifier(identifier) => {
                if let Some(register) = self.resolve_local(identifier.span())? {
                    Ok(register)
                } else if let Some(upvalue) = self.resolve_upvalue(identifier.span())? {
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::GetUpvalue {
                            destination,
                            upvalue,
                        },
                        expression.span(),
                    )?;
                    Ok(destination)
                } else if self.global_status(identifier.span())? == Some(false) {
                    let message = self.undeclared_global_message(identifier.span())?;
                    Err(OwnedCompileError::Diagnostic(self.source_diagnostic(
                        "BLU-COMPILE-0012",
                        Phase::Lower,
                        identifier.span(),
                        &message,
                    )?))
                } else if let Some(environment) = self.resolve_environment(expression.span())? {
                    let key = self.lower_environment_key(identifier.span())?;
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::GetTable {
                            destination,
                            table: environment,
                            key,
                        },
                        expression.span(),
                    )?;
                    Ok(destination)
                } else {
                    let name = self.global_name_constant(identifier.span())?;
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::LoadGlobal { destination, name },
                        expression.span(),
                    )?;
                    Ok(destination)
                }
            }
            ExpressionKind::Group(inner) => self.lower_expression(inner),
            ExpressionKind::Index(index) => {
                let table = self.lower_expression(index.table())?;
                let key = self.lower_expression(index.key())?;
                let destination = self.allocate_register()?;
                self.emit(
                    Instruction::GetTable {
                        destination,
                        table,
                        key,
                    },
                    expression.span(),
                )?;
                Ok(destination)
            }
            ExpressionKind::Field(field) => {
                let table = self.lower_expression(field.table())?;
                let key = self.lower_constant(
                    Constant::String(copy_bytes(
                        self.source.slice(field.name().span())?,
                        "field name",
                    )?),
                    field.name().span(),
                )?;
                let destination = self.allocate_register()?;
                self.emit(
                    Instruction::GetTable {
                        destination,
                        table,
                        key,
                    },
                    expression.span(),
                )?;
                Ok(destination)
            }
            ExpressionKind::Call(call) => self.lower_call(call),
            ExpressionKind::MethodCall(call) => self.lower_method_call(call),
            ExpressionKind::Function(function) => {
                self.lower_function(function.function(), function.span(), false)
            }
            ExpressionKind::If(if_expression) => {
                let condition = self.lower_expression(if_expression.condition())?;
                let destination = self.allocate_register()?;
                let false_branch = self.code.len();
                self.emit(
                    Instruction::JumpIfFalsy {
                        condition,
                        target: 0,
                    },
                    expression.span(),
                )?;
                let then_value = self.lower_expression(if_expression.then_value())?;
                self.emit(
                    Instruction::Move {
                        destination,
                        source: then_value,
                    },
                    expression.span(),
                )?;
                let end_branch = self.code.len();
                self.emit(Instruction::Jump { target: 0 }, expression.span())?;
                self.patch_forward_branch(false_branch, self.code.len())?;
                let else_value = self.lower_expression(if_expression.else_value())?;
                self.emit(
                    Instruction::Move {
                        destination,
                        source: else_value,
                    },
                    expression.span(),
                )?;
                self.patch_forward_branch(end_branch, self.code.len())?;
                Ok(destination)
            }
            ExpressionKind::Unary(unary) => match unary.operator() {
                UnaryOperator::Not => {
                    let source = self.lower_expression(unary.operand())?;
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::Not {
                            destination,
                            source,
                        },
                        expression.span(),
                    )?;
                    Ok(destination)
                }
                UnaryOperator::Negate => {
                    if matches!(
                        self.expression(unary.operand())?.kind(),
                        ExpressionKind::DecimalInteger
                    ) && matches!(
                        self.profile,
                        SemanticProfile::Blu
                            | SemanticProfile::Lua53
                            | SemanticProfile::Lua54
                            | SemanticProfile::Lua55
                    ) && self.decimal_integer_is_i64_min_magnitude(
                        self.expression(unary.operand())?.span(),
                    )? {
                        return self.lower_constant(Constant::Integer(i64::MIN), expression.span());
                    }
                    let source = self.lower_expression(unary.operand())?;
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::Negate {
                            destination,
                            source,
                        },
                        expression.span(),
                    )?;
                    Ok(destination)
                }
                UnaryOperator::BitwiseNot => {
                    let source = self.lower_expression(unary.operand())?;
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::BitwiseNot {
                            destination,
                            source,
                        },
                        expression.span(),
                    )?;
                    Ok(destination)
                }
                UnaryOperator::Length => {
                    let source = self.lower_expression(unary.operand())?;
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::Length {
                            destination,
                            source,
                        },
                        expression.span(),
                    )?;
                    Ok(destination)
                }
            },
            ExpressionKind::Binary(binary) => match binary.operator() {
                BinaryOperator::And | BinaryOperator::Or => {
                    let left = self.lower_expression(binary.left())?;
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::Move {
                            destination,
                            source: left,
                        },
                        expression.span(),
                    )?;
                    let branch = self.code.len();
                    let instruction = if binary.operator() == BinaryOperator::And {
                        Instruction::JumpIfFalsy {
                            condition: left,
                            target: 0,
                        }
                    } else {
                        Instruction::JumpIfTruthy {
                            condition: left,
                            target: 0,
                        }
                    };
                    self.emit(instruction, expression.span())?;
                    let right = self.lower_expression(binary.right())?;
                    self.emit(
                        Instruction::Move {
                            destination,
                            source: right,
                        },
                        expression.span(),
                    )?;
                    self.patch_forward_branch(branch, self.code.len())?;
                    Ok(destination)
                }
                BinaryOperator::Add => {
                    let (left, right, debug_span) =
                        self.lower_binary_operands(binary, expression.span())?;
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::Add {
                            destination,
                            left,
                            right,
                        },
                        debug_span,
                    )?;
                    Ok(destination)
                }
                BinaryOperator::Subtract => {
                    let (left, right, debug_span) =
                        self.lower_binary_operands(binary, expression.span())?;
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::Subtract {
                            destination,
                            left,
                            right,
                        },
                        debug_span,
                    )?;
                    Ok(destination)
                }
                BinaryOperator::Multiply => {
                    let (left, right, debug_span) =
                        self.lower_binary_operands(binary, expression.span())?;
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::Multiply {
                            destination,
                            left,
                            right,
                        },
                        debug_span,
                    )?;
                    Ok(destination)
                }
                BinaryOperator::Divide => {
                    let (left, right, debug_span) =
                        self.lower_binary_operands(binary, expression.span())?;
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::Divide {
                            destination,
                            left,
                            right,
                        },
                        debug_span,
                    )?;
                    Ok(destination)
                }
                BinaryOperator::Modulo => {
                    let (left, right, debug_span) =
                        self.lower_binary_operands(binary, expression.span())?;
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::Modulo {
                            destination,
                            left,
                            right,
                        },
                        debug_span,
                    )?;
                    Ok(destination)
                }
                BinaryOperator::Power => {
                    let (left, right, debug_span) =
                        self.lower_binary_operands(binary, expression.span())?;
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::Power {
                            destination,
                            left,
                            right,
                        },
                        debug_span,
                    )?;
                    Ok(destination)
                }
                BinaryOperator::FloorDivide => {
                    let (left, right, debug_span) =
                        self.lower_binary_operands(binary, expression.span())?;
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::FloorDivide {
                            destination,
                            left,
                            right,
                        },
                        debug_span,
                    )?;
                    Ok(destination)
                }
                BinaryOperator::BitwiseAnd
                | BinaryOperator::BitwiseOr
                | BinaryOperator::BitwiseExclusiveOr
                | BinaryOperator::ShiftLeft
                | BinaryOperator::ShiftRight => {
                    let (left, right, debug_span) =
                        self.lower_binary_operands(binary, expression.span())?;
                    let destination = self.allocate_register()?;
                    let instruction = match binary.operator() {
                        BinaryOperator::BitwiseAnd => Instruction::BitwiseAnd {
                            destination,
                            left,
                            right,
                        },
                        BinaryOperator::BitwiseOr => Instruction::BitwiseOr {
                            destination,
                            left,
                            right,
                        },
                        BinaryOperator::BitwiseExclusiveOr => Instruction::BitwiseExclusiveOr {
                            destination,
                            left,
                            right,
                        },
                        BinaryOperator::ShiftLeft => Instruction::ShiftLeft {
                            destination,
                            left,
                            right,
                        },
                        BinaryOperator::ShiftRight => Instruction::ShiftRight {
                            destination,
                            left,
                            right,
                        },
                        _ => unreachable!("bitwise lowering arm filters the operator"),
                    };
                    self.emit(instruction, debug_span)?;
                    Ok(destination)
                }
                BinaryOperator::Concatenate => {
                    let (left, right, debug_span) =
                        self.lower_binary_operands(binary, expression.span())?;
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::Concatenate {
                            destination,
                            left,
                            right,
                        },
                        debug_span,
                    )?;
                    Ok(destination)
                }
                BinaryOperator::Equal | BinaryOperator::NotEqual => {
                    let (left, right, debug_span) =
                        self.lower_binary_operands(binary, expression.span())?;
                    let compared = self.allocate_register()?;
                    self.emit(
                        Instruction::Equal {
                            destination: compared,
                            left,
                            right,
                        },
                        debug_span,
                    )?;
                    if binary.operator() == BinaryOperator::NotEqual {
                        let destination = self.allocate_register()?;
                        self.emit(
                            Instruction::Not {
                                destination,
                                source: compared,
                            },
                            debug_span,
                        )?;
                        Ok(destination)
                    } else {
                        Ok(compared)
                    }
                }
                BinaryOperator::LessThan | BinaryOperator::GreaterThan => {
                    let (left, right, debug_span) =
                        self.lower_binary_operands(binary, expression.span())?;
                    let destination = self.allocate_register()?;
                    let (left, right) = if binary.operator() == BinaryOperator::GreaterThan {
                        (right, left)
                    } else {
                        (left, right)
                    };
                    self.emit(
                        Instruction::LessThan {
                            destination,
                            left,
                            right,
                        },
                        debug_span,
                    )?;
                    Ok(destination)
                }
                BinaryOperator::LessEqual | BinaryOperator::GreaterEqual => {
                    let (left, right, debug_span) =
                        self.lower_binary_operands(binary, expression.span())?;
                    let destination = self.allocate_register()?;
                    let (left, right) = if binary.operator() == BinaryOperator::GreaterEqual {
                        (right, left)
                    } else {
                        (left, right)
                    };
                    self.emit(
                        Instruction::LessEqual {
                            destination,
                            left,
                            right,
                        },
                        debug_span,
                    )?;
                    Ok(destination)
                }
            },
        }
    }

    fn expression(&self, id: ExpressionId) -> Result<&Expression, OwnedCompileError> {
        self.expressions
            .get(id.as_usize())
            .ok_or(OwnedCompileError::InternalInvariant {
                message: "AST expression index is out of bounds",
            })
    }

    fn lower_interpolated_string(
        &mut self,
        string: InterpolatedString,
        span: ByteSpan,
    ) -> Result<u16, OwnedCompileError> {
        let end = string.first_part().checked_add(string.part_count()).ok_or(
            OwnedCompileError::InternalInvariant {
                message: "interpolated string part range overflows",
            },
        )?;
        let parts = self
            .interpolated_parts
            .get(string.first_part()..end)
            .ok_or(OwnedCompileError::InternalInvariant {
                message: "interpolated string part range is out of bounds",
            })?;
        let mut result = None;
        for part in parts {
            let (value, part_span) = match *part {
                InterpolatedStringPart::Text(text) => (self.lower_interpolated_text(text)?, text),
                InterpolatedStringPart::Expression(expression) => {
                    let value = self.lower_expression(expression)?;
                    let function = self.lower_global_name(b"tostring", span)?;
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::Call {
                            destination,
                            function,
                            arguments: value,
                            argument_count: 1,
                        },
                        self.expression(expression)?.span(),
                    )?;
                    (destination, self.expression(expression)?.span())
                }
            };
            result = Some(match result {
                None => value,
                Some(left) => {
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::Concatenate {
                            destination,
                            left,
                            right: value,
                        },
                        part_span,
                    )?;
                    destination
                }
            });
        }
        result.ok_or(OwnedCompileError::InternalInvariant {
            message: "interpolated string contains no parts",
        })
    }

    fn lower_interpolated_text(&mut self, span: ByteSpan) -> Result<u16, OwnedCompileError> {
        let decoded = self.interpolated_string_constant(span)?;
        self.lower_constant(decoded, span)
    }

    fn interpolated_string_constant(
        &mut self,
        span: ByteSpan,
    ) -> Result<Constant, OwnedCompileError> {
        let bytes = self.source.slice(span)?;
        let decoded = decode_interpolated_string_text(bytes, self.profile)?;
        check_limit(
            OwnedCompileLimit::StringLiteralBytes,
            decoded.len(),
            self.limits.artifact.max_constant_bytes,
        )?;
        let total =
            self.constant_bytes
                .checked_add(decoded.len())
                .ok_or(OwnedCompileError::Limit {
                    kind: OwnedCompileLimit::TotalConstantBytes,
                    required: usize::MAX,
                    limit: self.limits.artifact.max_total_constant_bytes,
                })?;
        check_limit(
            OwnedCompileLimit::TotalConstantBytes,
            total,
            self.limits.artifact.max_total_constant_bytes,
        )?;
        self.constant_bytes = total;
        Ok(Constant::String(decoded))
    }

    fn lower_constant(
        &mut self,
        constant: Constant,
        span: ByteSpan,
    ) -> Result<u16, OwnedCompileError> {
        let constant = self.push_constant(constant)?;
        let destination = self.allocate_register()?;
        self.emit(
            Instruction::LoadConstant {
                destination,
                constant,
            },
            span,
        )?;
        Ok(destination)
    }

    fn decimal_constant(&self, span: ByteSpan) -> Result<Constant, OwnedCompileError> {
        let bytes = self.source.slice(span)?;
        check_limit(
            OwnedCompileLimit::IntegerLiteralBytes,
            bytes.len(),
            self.limits.max_integer_literal_bytes,
        )?;
        for byte in bytes {
            if !byte.is_ascii_digit() && *byte != b'_' {
                return Err(OwnedCompileError::InternalInvariant {
                    message: "decimal-integer AST contains a non-decimal byte",
                });
            }
        }

        if matches!(
            self.profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        ) {
            let mut integer = 0_i64;
            let mut fits_integer = true;
            for byte in bytes {
                if *byte == b'_' {
                    continue;
                }
                let digit = i64::from(*byte - b'0');
                let Some(next) = integer
                    .checked_mul(10)
                    .and_then(|current| current.checked_add(digit))
                else {
                    fits_integer = false;
                    break;
                };
                integer = next;
            }
            if fits_integer {
                return Ok(Constant::Integer(integer));
            }
        }

        let mut normalized = allocate_vec(bytes.len(), "normalized decimal integer")?;
        normalized.extend(bytes.iter().copied().filter(|byte| *byte != b'_'));
        let text = core::str::from_utf8(&normalized).map_err(|_| {
            OwnedCompileError::InternalInvariant {
                message: "decimal-integer AST is not ASCII",
            }
        })?;
        let number = text
            .parse::<f64>()
            .map_err(|_| OwnedCompileError::InternalInvariant {
                message: "validated decimal-integer text failed numeric parsing",
            })?;
        Ok(Constant::Number(number))
    }

    fn decimal_integer_is_i64_min_magnitude(
        &self,
        span: ByteSpan,
    ) -> Result<bool, OwnedCompileError> {
        let bytes = self.source.slice(span)?;
        let mut digits = allocate_vec(bytes.len(), "normalized decimal integer")?;
        digits.extend(bytes.iter().copied().filter(|byte| *byte != b'_'));
        Ok(digits.as_slice() == b"9223372036854775808")
    }

    fn decimal_number_constant(&self, span: ByteSpan) -> Result<Constant, OwnedCompileError> {
        let bytes = self.source.slice(span)?;
        check_limit(
            OwnedCompileLimit::NumberLiteralBytes,
            bytes.len(),
            self.limits.max_number_literal_bytes,
        )?;
        let mut normalized = allocate_vec(bytes.len(), "normalized decimal number")?;
        normalized.extend(bytes.iter().copied().filter(|byte| *byte != b'_'));
        let text = core::str::from_utf8(&normalized).map_err(|_| {
            OwnedCompileError::InternalInvariant {
                message: "decimal-number AST is not ASCII",
            }
        })?;
        let number = text
            .parse::<f64>()
            .map_err(|_| OwnedCompileError::InternalInvariant {
                message: "validated decimal-number text failed numeric parsing",
            })?;
        Ok(Constant::Number(number))
    }

    fn hex_integer_constant(&self, span: ByteSpan) -> Result<Constant, OwnedCompileError> {
        let bytes = self.source.slice(span)?;
        check_limit(
            OwnedCompileLimit::IntegerLiteralBytes,
            bytes.len(),
            self.limits.max_integer_literal_bytes,
        )?;
        let Some(digits) = bytes
            .strip_prefix(b"0x")
            .or_else(|| bytes.strip_prefix(b"0X"))
        else {
            return Err(OwnedCompileError::InternalInvariant {
                message: "hex-integer AST has no hexadecimal prefix",
            });
        };
        if digits.is_empty()
            || !digits
                .iter()
                .all(|byte| byte.is_ascii_hexdigit() || *byte == b'_')
            || !digits.iter().any(u8::is_ascii_hexdigit)
        {
            return Err(OwnedCompileError::InternalInvariant {
                message: "hex-integer AST contains an invalid digit",
            });
        }

        if matches!(
            self.profile,
            SemanticProfile::Blu
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        ) {
            let integer = digits
                .iter()
                .filter(|byte| **byte != b'_')
                .fold(0_u64, |value, byte| {
                    value
                        .wrapping_mul(16)
                        .wrapping_add(u64::from(hex_digit(*byte)))
                });
            return Ok(Constant::Integer(integer as i64));
        }

        let number = digits
            .iter()
            .filter(|byte| **byte != b'_')
            .fold(0.0_f64, |value, byte| {
                value.mul_add(16.0, f64::from(hex_digit(*byte)))
            });
        Ok(Constant::Number(number))
    }

    fn binary_integer_constant(&self, span: ByteSpan) -> Result<Constant, OwnedCompileError> {
        let bytes = self.source.slice(span)?;
        check_limit(
            OwnedCompileLimit::IntegerLiteralBytes,
            bytes.len(),
            self.limits.max_integer_literal_bytes,
        )?;
        let Some(digits) = bytes
            .strip_prefix(b"0b")
            .or_else(|| bytes.strip_prefix(b"0B"))
        else {
            return Err(OwnedCompileError::InternalInvariant {
                message: "binary-integer AST has no binary prefix",
            });
        };
        if digits.is_empty()
            || !digits.iter().all(|byte| matches!(byte, b'0' | b'1' | b'_'))
            || !digits.iter().any(|byte| matches!(byte, b'0' | b'1'))
        {
            return Err(OwnedCompileError::InternalInvariant {
                message: "binary-integer AST contains an invalid digit",
            });
        }
        if self.profile == SemanticProfile::Blu {
            let integer = digits
                .iter()
                .filter(|byte| **byte != b'_')
                .fold(0_u64, |value, byte| {
                    value.wrapping_mul(2).wrapping_add(u64::from(*byte - b'0'))
                });
            Ok(Constant::Integer(integer as i64))
        } else {
            let number = digits
                .iter()
                .filter(|byte| **byte != b'_')
                .fold(0.0_f64, |value, byte| {
                    value.mul_add(2.0, f64::from(*byte - b'0'))
                });
            Ok(Constant::Number(number))
        }
    }

    fn hex_number_constant(&self, span: ByteSpan) -> Result<Constant, OwnedCompileError> {
        let bytes = self.source.slice(span)?;
        check_limit(
            OwnedCompileLimit::NumberLiteralBytes,
            bytes.len(),
            self.limits.max_number_literal_bytes,
        )?;
        let Some(body) = bytes
            .strip_prefix(b"0x")
            .or_else(|| bytes.strip_prefix(b"0X"))
        else {
            return Err(OwnedCompileError::InternalInvariant {
                message: "hex-number AST has no hexadecimal prefix",
            });
        };
        let exponent_offset = body.iter().position(|byte| matches!(byte, b'p' | b'P'));
        let (mantissa, exponent) = match exponent_offset {
            Some(offset) => (&body[..offset], Some(&body[offset + 1..])),
            None => (body, None),
        };
        let dot = mantissa.iter().position(|byte| *byte == b'.');
        let mut value = 0.0_f64;
        let mut fractional_digits = 0_i64;
        let mut after_dot = false;
        for byte in mantissa {
            match byte {
                b'.' if !after_dot => after_dot = true,
                b'_' => {}
                byte if byte.is_ascii_hexdigit() => {
                    value = value.mul_add(16.0, f64::from(hex_digit(*byte)));
                    if after_dot {
                        fractional_digits = fractional_digits.saturating_add(1);
                    }
                }
                _ => {
                    return Err(OwnedCompileError::InternalInvariant {
                        message: "hex-number AST contains an invalid mantissa byte",
                    });
                }
            }
        }
        if dot.is_none() && exponent.is_none() {
            return Err(OwnedCompileError::InternalInvariant {
                message: "hex-number AST has neither a point nor an exponent",
            });
        }
        let exponent = exponent.map_or(Ok(0_i64), parse_signed_decimal_exponent)?;
        let binary_exponent = exponent.saturating_sub(fractional_digits.saturating_mul(4));
        let binary_exponent =
            i32::try_from(binary_exponent).unwrap_or(if binary_exponent.is_negative() {
                i32::MIN
            } else {
                i32::MAX
            });
        Ok(Constant::Number(value * 2.0_f64.powi(binary_exponent)))
    }

    fn string_constant(&mut self, span: ByteSpan) -> Result<Constant, OwnedCompileError> {
        let bytes = self.source.slice(span)?;
        let Some(&first) = bytes.first() else {
            return Err(OwnedCompileError::InternalInvariant {
                message: "string-literal AST is empty",
            });
        };
        let (value, is_long) = if matches!(first, b'\'' | b'"') {
            let Some((&closing, value)) = bytes[1..].split_last() else {
                return Err(OwnedCompileError::InternalInvariant {
                    message: "string-literal AST has no closing quote",
                });
            };
            if closing != first {
                return Err(OwnedCompileError::InternalInvariant {
                    message: "string-literal AST delimiters do not match",
                });
            }
            (value, false)
        } else {
            (long_string_payload(bytes)?, true)
        };
        let decoded_len = if is_long {
            decoded_long_string_len(value, self.profile)
        } else {
            decoded_string_len(value, self.profile)?
        };
        check_limit(
            OwnedCompileLimit::StringLiteralBytes,
            decoded_len,
            self.limits.artifact.max_constant_bytes,
        )?;
        let total =
            self.constant_bytes
                .checked_add(decoded_len)
                .ok_or(OwnedCompileError::Limit {
                    kind: OwnedCompileLimit::TotalConstantBytes,
                    required: usize::MAX,
                    limit: self.limits.artifact.max_total_constant_bytes,
                })?;
        check_limit(
            OwnedCompileLimit::TotalConstantBytes,
            total,
            self.limits.artifact.max_total_constant_bytes,
        )?;
        let mut decoded = allocate_vec(decoded_len, "string literal bytes")?;
        if is_long {
            decode_long_string(value, self.profile, &mut decoded)?;
        } else {
            let mut offset = 0;
            while offset < value.len() {
                let byte = value[offset];
                if byte == b'\\' {
                    let escape = decode_string_escape(value, offset, self.profile)?;
                    for decoded_byte in &escape.bytes[..escape.len] {
                        push_fallible(&mut decoded, *decoded_byte, "string literal bytes")?;
                    }
                    offset += escape.consumed;
                } else {
                    push_fallible(&mut decoded, byte, "string literal bytes")?;
                    offset += 1;
                }
            }
        }
        self.constant_bytes = total;
        Ok(Constant::String(decoded))
    }

    fn source_diagnostic(
        &self,
        code: &str,
        phase: Phase,
        span: ByteSpan,
        message: &str,
    ) -> Result<Diagnostic, OwnedCompileError> {
        Ok(Diagnostic::try_new(
            code,
            phase,
            self.profile,
            Severity::Error,
            span,
            message,
            self.limits.parse.lexer.diagnostic_limits,
        )?)
    }

    fn resolve_local(&self, name: ByteSpan) -> Result<Option<u16>, OwnedCompileError> {
        for binding in self.bindings.iter().rev() {
            match binding.name {
                BindingName::Global(global)
                    if self.source.slice(global)? == self.source.slice(name)? =>
                {
                    return Ok(None);
                }
                BindingName::GlobalWildcard => continue,
                name if name.is_global() => continue,
                _ => {}
            }
            if binding.name.matches(self.source, name)? {
                return Ok(Some(binding.register));
            }
        }
        Ok(None)
    }

    fn global_status(&self, name: ByteSpan) -> Result<Option<bool>, OwnedCompileError> {
        fn status<I>(
            bindings: I,
            source: &SourceFile,
            name: ByteSpan,
        ) -> Result<Option<bool>, OwnedCompileError>
        where
            I: Iterator<Item = BindingName>,
        {
            let mut explicit = false;
            for binding in bindings {
                match binding {
                    BindingName::Global(global) => {
                        explicit = true;
                        if source.slice(global)? == source.slice(name)? {
                            return Ok(Some(true));
                        }
                    }
                    BindingName::GlobalWildcard => return Ok(Some(true)),
                    BindingName::GlobalDefault if !explicit => return Ok(Some(true)),
                    _ => {}
                }
            }
            Ok(explicit.then_some(false))
        }
        if let Some(status) = status(
            self.bindings.iter().rev().map(|binding| binding.name),
            self.source,
            name,
        )? {
            return Ok(Some(status));
        }
        status(
            self.outer_bindings.iter().rev().map(|binding| binding.name),
            self.source,
            name,
        )
    }

    fn resolve_environment(&mut self, span: ByteSpan) -> Result<Option<u16>, OwnedCompileError> {
        if !matches!(
            self.profile,
            SemanticProfile::Lua52
                | SemanticProfile::Lua53
                | SemanticProfile::Lua54
                | SemanticProfile::Lua55
        ) {
            return Ok(None);
        }
        for binding in self.bindings.iter().rev() {
            if binding.name.is_global() {
                continue;
            }
            if binding.name.bytes(self.source)? == b"_ENV" {
                self.uses_environment = true;
                return Ok(Some(binding.register));
            }
        }
        for (index, binding) in self.upvalues.iter().enumerate().rev() {
            if binding.name.is_global() {
                continue;
            }
            if binding.name.bytes(self.source)? == b"_ENV" {
                let upvalue =
                    u16::try_from(index).map_err(|_| OwnedCompileError::InternalInvariant {
                        message: "upvalue count passed limits but cannot fit BluV1",
                    })?;
                let destination = self.allocate_register()?;
                self.emit(
                    Instruction::GetUpvalue {
                        destination,
                        upvalue,
                    },
                    span,
                )?;
                self.uses_environment = true;
                return Ok(Some(destination));
            }
        }
        let mut outer = None;
        for binding in self.outer_bindings.iter().rev() {
            if binding.name.is_global() {
                continue;
            }
            if binding.name.bytes(self.source)? == b"_ENV" {
                outer = Some(*binding);
                break;
            }
        }
        if let Some(binding) = outer {
            let upvalue = self.push_upvalue(binding)?;
            let destination = self.allocate_register()?;
            self.emit(
                Instruction::GetUpvalue {
                    destination,
                    upvalue,
                },
                span,
            )?;
            self.uses_environment = true;
            return Ok(Some(destination));
        }
        Ok(None)
    }

    fn ensure_writable(&self, name: ByteSpan) -> Result<(), OwnedCompileError> {
        for binding in self.bindings.iter().rev() {
            match binding.name {
                BindingName::Global(global)
                    if self.source.slice(global)? == self.source.slice(name)? =>
                {
                    if binding.constant {
                        return Err(OwnedCompileError::Diagnostic(
                            self.const_assignment_diagnostic("BLU-COMPILE-0011", name)?,
                        ));
                    }
                    return Ok(());
                }
                BindingName::GlobalWildcard => continue,
                name if name.is_global() => continue,
                _ => {}
            }
            if binding.name.matches(self.source, name)? {
                if binding.constant {
                    return Err(OwnedCompileError::Diagnostic(
                        self.const_assignment_diagnostic("BLU-COMPILE-0011", name)?,
                    ));
                }
                return Ok(());
            }
        }
        for binding in self.upvalues.iter().rev() {
            if binding.name.is_global() {
                continue;
            }
            if binding.name.matches(self.source, name)? {
                if binding.constant {
                    return Err(OwnedCompileError::Diagnostic(
                        self.const_assignment_diagnostic("BLU-COMPILE-0011", name)?,
                    ));
                }
                return Ok(());
            }
        }
        for binding in self.outer_bindings.iter().rev() {
            if binding.name.is_global() {
                continue;
            }
            if binding.name.matches(self.source, name)? {
                if binding.constant {
                    return Err(OwnedCompileError::Diagnostic(
                        self.const_assignment_diagnostic("BLU-COMPILE-0011", name)?,
                    ));
                }
                return Ok(());
            }
        }
        if self.global_is_constant(name)? {
            return Err(OwnedCompileError::Diagnostic(
                self.const_assignment_diagnostic("BLU-COMPILE-0013", name)?,
            ));
        }
        Ok(())
    }

    fn const_assignment_diagnostic(
        &self,
        code: &str,
        name: ByteSpan,
    ) -> Result<Diagnostic, OwnedCompileError> {
        let display_name = String::from_utf8_lossy(self.source.slice(name)?);
        let message = format!("attempt to assign to const variable '{display_name}'");
        self.source_diagnostic(code, Phase::Lower, name, &message)
    }

    fn global_is_constant(&self, name: ByteSpan) -> Result<bool, OwnedCompileError> {
        let check =
            |binding: BindingName, constant: bool| -> Result<Option<bool>, OwnedCompileError> {
                match binding {
                    BindingName::Global(global)
                        if self.source.slice(global)? == self.source.slice(name)? =>
                    {
                        Ok(Some(constant))
                    }
                    BindingName::GlobalWildcard => Ok(Some(constant)),
                    _ => Ok(None),
                }
            };
        for binding in self.bindings.iter().rev() {
            if let Some(constant) = check(binding.name, binding.constant)? {
                return Ok(constant);
            }
        }
        for binding in self.upvalues.iter().rev() {
            if let Some(constant) = check(binding.name, binding.constant)? {
                return Ok(constant);
            }
        }
        for binding in self.outer_bindings.iter().rev() {
            if let Some(constant) = check(binding.name, binding.constant)? {
                return Ok(constant);
            }
        }
        Ok(false)
    }

    fn resolve_upvalue(&mut self, name: ByteSpan) -> Result<Option<u16>, OwnedCompileError> {
        for (index, binding) in self.upvalues.iter().enumerate().rev() {
            if binding.name.is_global() {
                continue;
            }
            if binding.name.matches(self.source, name)? {
                return Ok(Some(u16::try_from(index).map_err(|_| {
                    OwnedCompileError::InternalInvariant {
                        message: "upvalue count passed limits but cannot fit BluV1",
                    }
                })?));
            }
        }
        let mut explicit_global = false;
        for binding in self.outer_bindings.iter().rev() {
            match binding.name {
                BindingName::Global(global) => {
                    explicit_global = true;
                    if self.source.slice(global)? == self.source.slice(name)? {
                        return Ok(None);
                    }
                    continue;
                }
                BindingName::GlobalWildcard => continue,
                BindingName::GlobalDefault => continue,
                _ => {}
            }
            if binding.name.matches(self.source, name)? {
                let binding = *binding;
                let index = self.push_upvalue(binding)?;
                return Ok(Some(index));
            }
            if explicit_global && self.source.slice(name)? != b"_ENV" {
                continue;
            }
        }
        Ok(None)
    }

    fn function_mentions_binding(
        &self,
        span: ByteSpan,
        binding: BindingName,
    ) -> Result<bool, OwnedCompileError> {
        let name = binding.bytes(self.source)?;
        if name.is_empty() {
            return Ok(false);
        }
        let body = self.source.slice(span)?;
        Ok(body
            .windows(name.len())
            .enumerate()
            .any(|(offset, candidate)| {
                if candidate != name {
                    return false;
                }
                let before = offset.checked_sub(1).and_then(|index| body.get(index));
                let after = body.get(offset.saturating_add(name.len()));
                !before.is_some_and(|byte| is_identifier_byte(*byte))
                    && !after.is_some_and(|byte| is_identifier_byte(*byte))
            }))
    }

    fn push_upvalue(&mut self, binding: OuterBinding) -> Result<u16, OwnedCompileError> {
        let index = self.upvalues.len();
        check_limit(
            OwnedCompileLimit::Upvalues,
            index.saturating_add(1),
            self.limits
                .artifact
                .max_upvalues_per_prototype
                .min(u16::MAX as usize),
        )?;
        push_fallible(&mut self.upvalues, binding, "prototype upvalues")?;
        u16::try_from(index).map_err(|_| OwnedCompileError::InternalInvariant {
            message: "upvalue count passed limits but cannot fit BluV1",
        })
    }

    fn ensure_upvalue(&mut self, binding: OuterBinding) -> Result<u16, OwnedCompileError> {
        if let Some(index) = self.upvalues.iter().position(|candidate| {
            candidate.name == binding.name && candidate.source == binding.source
        }) {
            return u16::try_from(index).map_err(|_| OwnedCompileError::InternalInvariant {
                message: "upvalue count passed limits but cannot fit BluV1",
            });
        }
        self.push_upvalue(binding)
    }

    fn global_name_constant(&mut self, name: ByteSpan) -> Result<u32, OwnedCompileError> {
        let bytes = copy_bytes(self.source.slice(name)?, "global name")?;
        self.push_constant(Constant::String(bytes))
    }

    fn allocate_register(&mut self) -> Result<u16, OwnedCompileError> {
        let required = self.register_count.saturating_add(1);
        let limit = self
            .limits
            .max_registers
            .min(self.limits.artifact.max_registers_per_prototype)
            .min(self.limits.artifact.max_total_registers)
            .min(u16::MAX as usize);
        check_limit(OwnedCompileLimit::Registers, required, limit)?;
        let register = u16::try_from(self.register_count).map_err(|_| {
            OwnedCompileError::InternalInvariant {
                message: "register index passed limits but cannot fit BluV1",
            }
        })?;
        self.register_count = required;
        Ok(register)
    }

    fn push_constant(&mut self, constant: Constant) -> Result<u32, OwnedCompileError> {
        let required = self.constants.len().saturating_add(1);
        let limit = self
            .limits
            .max_constants
            .min(self.limits.artifact.max_constants_per_prototype)
            .min(self.limits.artifact.max_total_constants)
            .min(u32::MAX as usize);
        check_limit(OwnedCompileLimit::Constants, required, limit)?;
        let index = u32::try_from(self.constants.len()).map_err(|_| {
            OwnedCompileError::InternalInvariant {
                message: "constant index passed limits but cannot fit BluV1",
            }
        })?;
        push_fallible(&mut self.constants, constant, "constants")?;
        Ok(index)
    }

    fn emit(&mut self, instruction: Instruction, span: ByteSpan) -> Result<(), OwnedCompileError> {
        let required = self.code.len().saturating_add(1);
        let limit = self
            .limits
            .max_instructions
            .min(self.limits.artifact.max_code_per_prototype)
            .min(self.limits.artifact.max_total_code)
            .min(self.limits.artifact.max_total_source_map_entries)
            .min(u32::MAX as usize);
        check_limit(OwnedCompileLimit::Instructions, required, limit)?;
        push_fallible(&mut self.code, instruction, "instructions")?;
        push_fallible(&mut self.source_map, span, "source map")
    }

    fn patch_forward_branch(
        &mut self,
        instruction: usize,
        target: usize,
    ) -> Result<(), OwnedCompileError> {
        let target = u32::try_from(target).map_err(|_| OwnedCompileError::InternalInvariant {
            message: "branch target passed limits but cannot fit BluV1",
        })?;
        let Some(slot) = self.code.get_mut(instruction) else {
            return Err(OwnedCompileError::InternalInvariant {
                message: "branch patch instruction is missing",
            });
        };
        match slot {
            Instruction::JumpIfTruthy {
                target: branch_target,
                ..
            }
            | Instruction::JumpIfFalsy {
                target: branch_target,
                ..
            }
            | Instruction::Jump {
                target: branch_target,
            } => *branch_target = target,
            _ => {
                return Err(OwnedCompileError::InternalInvariant {
                    message: "branch patch did not reference a branch",
                });
            }
        }
        Ok(())
    }

    fn debug_branch_span(
        &self,
        condition_span: ByteSpan,
        body: &[Statement],
    ) -> Result<ByteSpan, OwnedCompileError> {
        let Some(first_statement) = body.first() else {
            return Ok(condition_span);
        };
        let condition_end = condition_span.end().as_usize();
        let body_start = first_statement.span().start().as_usize();
        if condition_end >= body_start {
            return Ok(condition_span);
        }
        let between = &self.source.bytes()[condition_end..body_start];
        let Some(relative) = between
            .iter()
            .rposition(|byte| matches!(*byte, b'\n' | b'\r'))
        else {
            return Ok(condition_span);
        };
        let line_marker = condition_end.saturating_add(relative);
        self.source.span(line_marker, line_marker).map_err(|_| {
            OwnedCompileError::InternalInvariant {
                message: "if debug branch marker span became invalid",
            }
        })
    }

    fn debug_assignment_span(&self, span: ByteSpan) -> Result<ByteSpan, OwnedCompileError> {
        let start = span.start().as_usize();
        let end = span.end().as_usize();
        if end <= start {
            return Ok(span);
        }
        let start_line = owned_source_line(self.source, self.profile, start)?;
        let end_offset = end - 1;
        let end_line = owned_source_line(self.source, self.profile, end_offset)?;
        if start_line == end_line {
            return Ok(span);
        }
        self.source
            .span(end_offset, end)
            .map_err(|_| OwnedCompileError::InternalInvariant {
                message: "assignment debug span became invalid",
            })
    }

    fn lower_binary_operands(
        &mut self,
        binary: BinaryExpression,
        expression_span: ByteSpan,
    ) -> Result<(u16, u16, ByteSpan), OwnedCompileError> {
        let operator_span = binary.operator_span();
        let left_line = owned_source_line(
            self.source,
            self.profile,
            self.expression(binary.left())?.span().start().as_usize(),
        )?;
        let operator_line =
            owned_source_line(self.source, self.profile, operator_span.start().as_usize())?;
        let multiline = left_line != operator_line;
        let left_start = self.code.len();
        let left = self.lower_expression(binary.left())?;
        if multiline {
            for span in &mut self.source_map[left_start..] {
                *span = operator_span;
            }
        }
        let right = self.lower_expression(binary.right())?;
        Ok((
            left,
            right,
            if multiline {
                operator_span
            } else {
                expression_span
            },
        ))
    }
}

fn check_limit(
    kind: OwnedCompileLimit,
    required: usize,
    limit: usize,
) -> Result<(), OwnedCompileError> {
    if required > limit {
        Err(OwnedCompileError::Limit {
            kind,
            required,
            limit,
        })
    } else {
        Ok(())
    }
}

fn copy_string(value: &str, what: &'static str) -> Result<String, OwnedCompileError> {
    let mut copied = String::new();
    copied
        .try_reserve_exact(value.len())
        .map_err(|_| OwnedCompileError::Allocation {
            what,
            requested: value.len(),
        })?;
    copied.push_str(value);
    Ok(copied)
}

fn constant_sign(constant: Constant) -> i8 {
    match constant {
        Constant::Integer(value) => value.signum() as i8,
        Constant::Number(value) if value > 0.0 => 1,
        Constant::Number(value) if value < 0.0 => -1,
        Constant::Number(_) => 0,
        _ => 0,
    }
}

fn copy_bytes(bytes: &[u8], what: &'static str) -> Result<Vec<u8>, OwnedCompileError> {
    let mut copied = allocate_vec(bytes.len(), what)?;
    copied.extend_from_slice(bytes);
    Ok(copied)
}

fn owned_source_line(
    source: &SourceFile,
    _profile: SemanticProfile,
    offset: usize,
) -> Result<u32, OwnedCompileError> {
    Ok(source
        .position(offset)
        .map_err(|_| OwnedCompileError::InternalInvariant {
            message: "source span has no line position",
        })?
        .line)
}

const fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

const fn is_lua_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

fn decode_common_string_escape(escaped: u8) -> Option<u8> {
    Some(match escaped {
        b'\\' => b'\\',
        b'\'' => b'\'',
        b'"' => b'"',
        b'a' => 0x07,
        b'b' => 0x08,
        b'f' => 0x0c,
        b'n' => b'\n',
        b'r' => b'\r',
        b't' => b'\t',
        b'v' => 0x0b,
        _ => return None,
    })
}

fn decode_interpolated_string_text(
    value: &[u8],
    profile: SemanticProfile,
) -> Result<Vec<u8>, OwnedCompileError> {
    let mut decoded = allocate_vec(value.len(), "interpolated string bytes")?;
    let mut offset = 0;
    while offset < value.len() {
        if value[offset] != b'\\' {
            push_fallible(&mut decoded, value[offset], "interpolated string bytes")?;
            offset += 1;
            continue;
        }
        let escaped = *value
            .get(offset + 1)
            .ok_or(OwnedCompileError::InternalInvariant {
                message: "validated interpolated string ends in a backslash",
            })?;
        if matches!(escaped, b'{' | b'}' | b'`' | b'\\' | b' ') {
            push_fallible(&mut decoded, escaped, "interpolated string bytes")?;
            offset += 2;
            continue;
        }
        let escape = decode_string_escape(value, offset, profile)?;
        for byte in &escape.bytes[..escape.len] {
            push_fallible(&mut decoded, *byte, "interpolated string bytes")?;
        }
        offset += escape.consumed;
    }
    Ok(decoded)
}

struct DecodedEscape {
    bytes: [u8; 6],
    len: usize,
    consumed: usize,
}

impl DecodedEscape {
    const fn single(byte: u8, consumed: usize) -> Self {
        let mut bytes = [0; 6];
        bytes[0] = byte;
        Self {
            bytes,
            len: 1,
            consumed,
        }
    }

    const fn empty(consumed: usize) -> Self {
        Self {
            bytes: [0; 6],
            len: 0,
            consumed,
        }
    }
}

fn decode_string_escape(
    value: &[u8],
    offset: usize,
    profile: SemanticProfile,
) -> Result<DecodedEscape, OwnedCompileError> {
    let escaped = *value
        .get(offset + 1)
        .ok_or(OwnedCompileError::InternalInvariant {
            message: "validated string literal ends in a backslash",
        })?;
    if let Some(decoded) = decode_common_string_escape(escaped) {
        return Ok(DecodedEscape::single(decoded, 2));
    }
    if escaped == b'\n' {
        let consumed = if value.get(offset + 2) == Some(&b'\r') {
            3
        } else {
            2
        };
        return Ok(DecodedEscape::single(b'\n', consumed));
    }
    if escaped == b'\r' {
        let consumed = if value.get(offset + 2) == Some(&b'\n') {
            3
        } else {
            2
        };
        return Ok(DecodedEscape::single(b'\n', consumed));
    }
    if escaped.is_ascii_digit() {
        let mut cursor = offset + 1;
        let mut decoded = 0_u16;
        let mut digits = 0;
        while digits < 3 && value.get(cursor).is_some_and(u8::is_ascii_digit) {
            decoded = decoded * 10 + u16::from(value[cursor] - b'0');
            cursor += 1;
            digits += 1;
        }
        let decoded = u8::try_from(decoded).map_err(|_| OwnedCompileError::InternalInvariant {
            message: "validated decimal byte escape is out of range",
        })?;
        return Ok(DecodedEscape::single(decoded, cursor - offset));
    }
    if escaped == b'x' {
        let high = *value
            .get(offset + 2)
            .filter(|byte| byte.is_ascii_hexdigit())
            .ok_or(OwnedCompileError::InternalInvariant {
                message: "validated hexadecimal byte escape has no high digit",
            })?;
        let low = *value
            .get(offset + 3)
            .filter(|byte| byte.is_ascii_hexdigit())
            .ok_or(OwnedCompileError::InternalInvariant {
                message: "validated hexadecimal byte escape has no low digit",
            })?;
        return Ok(DecodedEscape::single(
            hex_digit(high) * 16 + hex_digit(low),
            4,
        ));
    }
    if escaped == b'z' {
        let mut cursor = offset + 2;
        while value
            .get(cursor)
            .is_some_and(|byte| is_lua_whitespace(*byte))
        {
            cursor += 1;
        }
        return Ok(DecodedEscape::empty(cursor - offset));
    }
    if escaped == b'u' {
        let mut cursor = offset + 2;
        if value.get(cursor) != Some(&b'{') {
            return Err(OwnedCompileError::InternalInvariant {
                message: "validated Unicode escape has no opening brace",
            });
        }
        cursor += 1;
        let mut codepoint = 0_u32;
        let mut digits = 0;
        while value.get(cursor).is_some_and(u8::is_ascii_hexdigit) {
            codepoint = codepoint
                .saturating_mul(16)
                .saturating_add(u32::from(hex_digit(value[cursor])));
            cursor += 1;
            digits += 1;
        }
        if digits == 0 || digits > 8 || value.get(cursor) != Some(&b'}') {
            return Err(OwnedCompileError::InternalInvariant {
                message: "validated Unicode escape is malformed",
            });
        }
        cursor += 1;
        let maximum = match profile {
            SemanticProfile::Blu | SemanticProfile::Lua54 | SemanticProfile::Lua55 => 0x7fff_ffff,
            SemanticProfile::Luau | SemanticProfile::Lua53 => 0x10_ffff,
            _ => {
                return Err(OwnedCompileError::InternalInvariant {
                    message: "Unicode escape reached an unsupported profile",
                });
            }
        };
        if codepoint > maximum {
            return Err(OwnedCompileError::InternalInvariant {
                message: "validated Unicode escape exceeds the profile maximum",
            });
        }
        let mut decoded = encode_extended_utf8(codepoint);
        decoded.consumed = cursor - offset;
        return Ok(decoded);
    }
    Err(OwnedCompileError::InternalInvariant {
        message: "validated string literal contains an unsupported escape",
    })
}

fn encode_extended_utf8(codepoint: u32) -> DecodedEscape {
    let len = match codepoint {
        0..=0x7f => 1,
        0x80..=0x7ff => 2,
        0x800..=0xffff => 3,
        0x1_0000..=0x1f_ffff => 4,
        0x20_0000..=0x3ff_ffff => 5,
        _ => 6,
    };
    if len == 1 {
        return DecodedEscape::single(codepoint as u8, 0);
    }
    let mut bytes = [0_u8; 6];
    let mut remaining = codepoint;
    for index in (1..len).rev() {
        bytes[index] = 0x80 | (remaining as u8 & 0x3f);
        remaining >>= 6;
    }
    let prefix = match len {
        2 => 0xc0,
        3 => 0xe0,
        4 => 0xf0,
        5 => 0xf8,
        6 => 0xfc,
        _ => 0,
    };
    bytes[0] = prefix | remaining as u8;
    DecodedEscape {
        bytes,
        len,
        consumed: 0,
    }
}

fn hex_digit(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => 0,
    }
}

fn parse_signed_decimal_exponent(bytes: &[u8]) -> Result<i64, OwnedCompileError> {
    let (negative, digits) = match bytes.first() {
        Some(b'+') => (false, &bytes[1..]),
        Some(b'-') => (true, &bytes[1..]),
        _ => (false, bytes),
    };
    if !digits.iter().any(u8::is_ascii_digit)
        || !digits
            .iter()
            .all(|byte| byte.is_ascii_digit() || *byte == b'_')
    {
        return Err(OwnedCompileError::InternalInvariant {
            message: "hex-number AST contains an invalid exponent",
        });
    }
    let magnitude = digits
        .iter()
        .filter(|byte| **byte != b'_')
        .fold(0_i64, |value, byte| {
            value
                .saturating_mul(10)
                .saturating_add(i64::from(*byte - b'0'))
        });
    Ok(if negative {
        magnitude.saturating_neg()
    } else {
        magnitude
    })
}

fn long_string_payload(bytes: &[u8]) -> Result<&[u8], OwnedCompileError> {
    if bytes.first() != Some(&b'[') {
        return Err(OwnedCompileError::InternalInvariant {
            message: "string-literal AST has an unsupported delimiter",
        });
    }
    let mut opener_end = 1;
    while bytes.get(opener_end) == Some(&b'=') {
        opener_end += 1;
    }
    if bytes.get(opener_end) != Some(&b'[') {
        return Err(OwnedCompileError::InternalInvariant {
            message: "long-string AST has a malformed opening delimiter",
        });
    }
    let equals = opener_end - 1;
    let closing_len = equals + 2;
    if bytes.len() < opener_end + 1 + closing_len {
        return Err(OwnedCompileError::InternalInvariant {
            message: "long-string AST has no closing delimiter",
        });
    }
    let closing = &bytes[bytes.len() - closing_len..];
    if closing.first() != Some(&b']')
        || closing.last() != Some(&b']')
        || !closing[1..closing.len() - 1]
            .iter()
            .all(|byte| *byte == b'=')
    {
        return Err(OwnedCompileError::InternalInvariant {
            message: "long-string AST delimiters do not match",
        });
    }
    Ok(&bytes[opener_end + 1..bytes.len() - closing_len])
}

fn long_string_content_start(value: &[u8], profile: SemanticProfile) -> usize {
    match value {
        [b'\r', b'\n', ..] => 2,
        [b'\n', b'\r', ..] if profile != SemanticProfile::Luau => 2,
        [b'\n', ..] => 1,
        [b'\r', ..] if profile != SemanticProfile::Luau => 1,
        _ => 0,
    }
}

fn decoded_long_string_len(value: &[u8], profile: SemanticProfile) -> usize {
    let value = &value[long_string_content_start(value, profile)..];
    let mut length = 0;
    let mut offset = 0;
    while offset < value.len() {
        let paired_line_end = (value[offset] == b'\r' && value.get(offset + 1) == Some(&b'\n'))
            || (profile != SemanticProfile::Luau
                && value[offset] == b'\n'
                && value.get(offset + 1) == Some(&b'\r'));
        if paired_line_end {
            length += 1;
            offset += 2;
        } else {
            length += 1;
            offset += 1;
        }
    }
    length
}

fn decode_long_string(
    value: &[u8],
    profile: SemanticProfile,
    decoded: &mut Vec<u8>,
) -> Result<(), OwnedCompileError> {
    let mut offset = long_string_content_start(value, profile);
    while offset < value.len() {
        let paired_line_end = (value[offset] == b'\r' && value.get(offset + 1) == Some(&b'\n'))
            || (profile != SemanticProfile::Luau
                && value[offset] == b'\n'
                && value.get(offset + 1) == Some(&b'\r'));
        if paired_line_end {
            push_fallible(decoded, b'\n', "string literal bytes")?;
            offset += 2;
        } else if value[offset] == b'\r' && profile != SemanticProfile::Luau {
            push_fallible(decoded, b'\n', "string literal bytes")?;
            offset += 1;
        } else {
            push_fallible(decoded, value[offset], "string literal bytes")?;
            offset += 1;
        }
    }
    Ok(())
}

fn decoded_string_len(value: &[u8], profile: SemanticProfile) -> Result<usize, OwnedCompileError> {
    let mut decoded_len = 0_usize;
    let mut offset = 0;
    while offset < value.len() {
        let added = if value[offset] == b'\\' {
            let decoded = decode_string_escape(value, offset, profile)?;
            offset += decoded.consumed;
            decoded.len
        } else {
            offset += 1;
            1
        };
        decoded_len =
            decoded_len
                .checked_add(added)
                .ok_or(OwnedCompileError::InternalInvariant {
                    message: "decoded string literal length overflowed",
                })?;
    }
    Ok(decoded_len)
}

fn allocate_vec<T>(capacity: usize, what: &'static str) -> Result<Vec<T>, OwnedCompileError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| OwnedCompileError::Allocation {
            what,
            requested: capacity,
        })?;
    Ok(values)
}

fn push_fallible<T>(
    values: &mut Vec<T>,
    value: T,
    what: &'static str,
) -> Result<(), OwnedCompileError> {
    if values.len() == values.capacity() {
        let requested = values.len().saturating_add(1);
        values
            .try_reserve(1)
            .map_err(|_| OwnedCompileError::Allocation { what, requested })?;
    }
    values.push(value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{OwnedCompileError, allocate_vec};

    #[test]
    fn compiler_allocation_failure_is_structured() {
        assert!(matches!(
            allocate_vec::<u8>(usize::MAX, "test entries"),
            Err(OwnedCompileError::Allocation {
                what: "test entries",
                requested: usize::MAX,
            })
        ));
    }
}
