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
    Instruction, LocalDebug, Prototype, SourceRecord, Upvalue, ValidatedArtifact, ValidationError,
    decode_validated, encode,
};
use blu_core::{
    ByteSpan, CompilerIdentity, Diagnostic, DiagnosticError, IdentityError, Phase, SemanticProfile,
    Severity, SourceFile, SourceIdentity, SpanError,
};
use blu_syntax::{
    AssignmentListStatement, AssignmentStatement, AssignmentTarget, Ast, BinaryOperator,
    CallExpression, Expression, ExpressionId, ExpressionKind, FunctionId, Identifier, IfStatement,
    LocalListStatement, LocalStatement, MethodCallExpression, NumericForStatement, ParseError,
    ParseLimits, ParseOutcome, Rejected, RepeatStatement, ReturnStatement, Statement,
    TableConstructor, TableField, UnaryOperator, WhileStatement, parse,
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
        let prototype = Lowerer::new(source, parsed.ast(), self.limits, &mut prototypes, &[], &[])?
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
            format: BytecodeFormat::BluV1,
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
            | Self::InternalInvariant { .. } => None,
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

#[derive(Clone, Copy)]
struct Binding {
    name: ByteSpan,
    register: u16,
    start_pc: u32,
    end_pc: Option<u32>,
}

#[derive(Clone, Copy)]
struct OuterBinding {
    name: ByteSpan,
    source: Upvalue,
}

struct Lowerer<'a, 'prototypes> {
    source: &'a SourceFile,
    ast: &'a Ast,
    profile: SemanticProfile,
    expressions: &'a [Expression],
    table_fields: &'a [TableField],
    call_arguments: &'a [ExpressionId],
    limits: OwnedCompileLimits,
    prototypes: &'prototypes mut Vec<Prototype>,
    outer_bindings: Vec<OuterBinding>,
    upvalues: Vec<OuterBinding>,
    parameter_count: usize,
    bindings: Vec<Binding>,
    closed_bindings: Vec<Binding>,
    loop_breaks: Vec<Vec<usize>>,
    loop_continues: Vec<Vec<usize>>,
    register_count: usize,
    constants: Vec<Constant>,
    constant_bytes: usize,
    code: Vec<Instruction>,
    source_map: Vec<ByteSpan>,
    children: Vec<u32>,
}

impl<'a, 'prototypes> Lowerer<'a, 'prototypes> {
    fn new(
        source: &'a SourceFile,
        ast: &'a Ast,
        limits: OwnedCompileLimits,
        prototypes: &'prototypes mut Vec<Prototype>,
        parameters: &[Identifier],
        outer_bindings: &[OuterBinding],
    ) -> Result<Self, OwnedCompileError> {
        let capacity = ast.node_count().min(4_096);
        let mut copied_outer_bindings =
            allocate_vec(outer_bindings.len(), "outer lexical bindings")?;
        copied_outer_bindings.extend_from_slice(outer_bindings);
        let mut lowerer = Self {
            source,
            ast,
            profile: ast.profile(),
            expressions: ast.expressions(),
            table_fields: ast.table_field_arena(),
            call_arguments: ast.call_argument_arena(),
            limits,
            prototypes,
            outer_bindings: copied_outer_bindings,
            upvalues: allocate_vec(4, "prototype upvalues")?,
            parameter_count: parameters.len(),
            bindings: allocate_vec(capacity.min(limits.max_bindings), "local bindings")?,
            closed_bindings: allocate_vec(
                capacity.min(limits.max_bindings),
                "closed local bindings",
            )?,
            loop_breaks: allocate_vec(8, "loop control stack")?,
            loop_continues: allocate_vec(8, "loop continue stack")?,
            register_count: 0,
            constants: allocate_vec(capacity.min(limits.max_constants), "constants")?,
            constant_bytes: 0,
            code: allocate_vec(capacity.min(limits.max_instructions), "instructions")?,
            source_map: allocate_vec(capacity.min(limits.max_instructions), "source map")?,
            children: allocate_vec(4, "prototype children")?,
        };
        for parameter in parameters {
            let register = lowerer.allocate_register()?;
            lowerer.push_binding(parameter.span(), register, 0)?;
        }
        Ok(lowerer)
    }

    fn run(mut self, statements: &[Statement]) -> Result<Prototype, OwnedCompileError> {
        if !self.lower_statements(statements)? {
            let eof = self.source.span(self.source.len(), self.source.len())?;
            self.emit(Instruction::Return { first: 0, count: 0 }, eof)?;
        }

        let end_pc =
            u32::try_from(self.code.len()).map_err(|_| OwnedCompileError::InternalInvariant {
                message: "instruction count passed limits but cannot fit a debug PC",
            })?;
        let mut debug_bytes = 0_usize;
        for binding in self.closed_bindings.iter().chain(&self.bindings) {
            let name_len = self.source.slice(binding.name)?.len();
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
            .len()
            .saturating_add(self.bindings.len());
        let mut locals = allocate_vec(binding_count, "local debug entries")?;
        for binding in self.closed_bindings.into_iter().chain(self.bindings) {
            let name = copy_bytes(self.source.slice(binding.name)?, "local debug name")?;
            locals.push(LocalDebug {
                name,
                register: binding.register,
                start_pc: binding.start_pc,
                end_pc: binding.end_pc.unwrap_or(end_pc),
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
        if self.code.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::JumpIfTruthy { .. } | Instruction::JumpIfFalsy { .. }
            )
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
        }) {
            required_features = required_features | FeatureBits::GLOBALS;
        }
        if self.code.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::NewTable { .. }
                    | Instruction::GetTable { .. }
                    | Instruction::SetTable { .. }
            )
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
                Instruction::NewClosure { .. }
                    | Instruction::GetUpvalue { .. }
                    | Instruction::SetUpvalue { .. }
            )
        }) {
            required_features = required_features | FeatureBits::CLOSURES;
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
            is_vararg: false,
            required_features,
            constants: self.constants,
            upvalues,
            children: self.children,
            code: self.code,
            source_map: self.source_map,
            locals,
            upvalue_debug: Vec::new(),
        })
    }

    fn lower_statements(&mut self, statements: &[Statement]) -> Result<bool, OwnedCompileError> {
        for statement in statements {
            let terminated = match statement {
                Statement::Local(local) => {
                    self.lower_local(*local)?;
                    false
                }
                Statement::LocalFunction(function) => {
                    let destination = self.allocate_register()?;
                    let nil = self.push_constant(Constant::Nil)?;
                    self.emit(
                        Instruction::LoadConstant {
                            destination,
                            constant: nil,
                        },
                        function.name().span(),
                    )?;
                    let start_pc = u32::try_from(self.code.len()).map_err(|_| {
                        OwnedCompileError::InternalInvariant {
                            message: "instruction count passed limits but cannot fit a debug PC",
                        }
                    })?;
                    self.push_binding(function.name().span(), destination, start_pc)?;
                    let closure = self.lower_function(function.function(), function.span())?;
                    self.emit(
                        Instruction::Move {
                            destination,
                            source: closure,
                        },
                        function.span(),
                    )?;
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
                    let terminated = self.lower_statements(statement.body().statements())?;
                    self.close_scope(scope)?;
                    terminated
                }
                Statement::NumericFor(statement) => {
                    self.lower_numeric_for(statement)?;
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
                Statement::Return(return_statement) => {
                    self.lower_return(return_statement)?;
                    true
                }
            };
            if terminated {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn lower_if(&mut self, statement: &IfStatement) -> Result<bool, OwnedCompileError> {
        let mut end_jumps = allocate_vec(statement.clauses().len(), "if end branches")?;
        let mut all_clauses_terminate = true;
        for clause in statement.clauses() {
            let condition = self.lower_expression(clause.condition())?;
            let false_branch = self.code.len();
            self.emit(
                Instruction::JumpIfFalsy {
                    condition,
                    target: 0,
                },
                clause.span(),
            )?;
            let scope = self.bindings.len();
            let terminated = self.lower_statements(clause.body().statements())?;
            self.close_scope(scope)?;
            all_clauses_terminate &= terminated;
            if !terminated {
                let branch = self.code.len();
                self.emit(Instruction::Jump { target: 0 }, clause.span())?;
                push_fallible(&mut end_jumps, branch, "if end branches")?;
            }
            self.patch_forward_branch(false_branch, self.code.len())?;
        }

        let else_terminates = if let Some(body) = statement.else_body() {
            let scope = self.bindings.len();
            let terminated = self.lower_statements(body.statements())?;
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
        let lowered = self.lower_statements(statement.body().statements());
        let continues = self
            .loop_continues
            .pop()
            .ok_or(OwnedCompileError::InternalInvariant {
                message: "loop continue stack became empty during lowering",
            })?;
        for branch in continues {
            self.patch_forward_branch(branch, start)?;
        }
        let breaks = self
            .loop_breaks
            .pop()
            .ok_or(OwnedCompileError::InternalInvariant {
                message: "loop control stack became empty during lowering",
            })?;
        let terminated = lowered?;
        self.close_scope(scope)?;
        if !terminated {
            let target =
                u32::try_from(start).map_err(|_| OwnedCompileError::InternalInvariant {
                    message: "loop target passed limits but cannot fit BluV1",
                })?;
            self.emit(Instruction::Jump { target }, statement.span())?;
        }
        let end = self.code.len();
        self.patch_forward_branch(exit, end)?;
        for branch in breaks {
            self.patch_forward_branch(branch, end)?;
        }
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
        let lowered = self.lower_statements(statement.body().statements());
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
        lowered?;
        let condition_start = self.code.len();
        for branch in continues {
            self.patch_forward_branch(branch, condition_start)?;
        }
        let condition = self.lower_expression(statement.condition())?;
        let exit = self.code.len();
        self.emit(
            Instruction::JumpIfTruthy {
                condition,
                target: 0,
            },
            statement.span(),
        )?;
        let target = u32::try_from(start).map_err(|_| OwnedCompileError::InternalInvariant {
            message: "loop target passed limits but cannot fit BluV1",
        })?;
        self.emit(Instruction::Jump { target }, statement.span())?;
        self.close_scope(scope)?;
        let end = self.code.len();
        self.patch_forward_branch(exit, end)?;
        for branch in breaks {
            self.patch_forward_branch(branch, end)?;
        }
        Ok(())
    }

    fn lower_numeric_for(
        &mut self,
        statement: &NumericForStatement,
    ) -> Result<(), OwnedCompileError> {
        let initial_source = self.lower_expression(statement.initial())?;
        let limit_source = self.lower_expression(statement.limit())?;
        let initial = self.allocate_register()?;
        self.emit(
            Instruction::Move {
                destination: initial,
                source: initial_source,
            },
            statement.span(),
        )?;
        let limit = self.allocate_register()?;
        self.emit(
            Instruction::Move {
                destination: limit,
                source: limit_source,
            },
            statement.span(),
        )?;
        let index = self.allocate_register()?;
        self.emit(
            Instruction::Move {
                destination: index,
                source: initial,
            },
            statement.span(),
        )?;
        let (step, ascending) = if let Some(step_expression) = statement.step() {
            let Some(sign) = self.numeric_for_step_sign(step_expression, false)? else {
                return Err(OwnedCompileError::Diagnostic(self.source_diagnostic(
                    "BLU-COMPILE-0003",
                    Phase::Lower,
                    self.expression(step_expression)?.span(),
                    "numeric for step must currently be a nonzero numeric literal",
                )?));
            };
            if sign == 0 {
                return Err(OwnedCompileError::Diagnostic(self.source_diagnostic(
                    "BLU-COMPILE-0004",
                    Phase::Lower,
                    self.expression(step_expression)?.span(),
                    "zero numeric for step is profile-specific and not yet executable",
                )?));
            }
            let source = self.lower_expression(step_expression)?;
            let snapshot = self.allocate_register()?;
            self.emit(
                Instruction::Move {
                    destination: snapshot,
                    source,
                },
                statement.span(),
            )?;
            (snapshot, sign > 0)
        } else {
            let step_constant = if matches!(
                self.profile,
                SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
            ) {
                Constant::Integer(1)
            } else {
                Constant::Number(1.0)
            };
            (self.lower_constant(step_constant, statement.span())?, true)
        };

        let loop_scope = self.bindings.len();
        let binding_limit = self
            .limits
            .max_bindings
            .min(self.limits.artifact.max_debug_entries_per_prototype)
            .min(self.limits.artifact.max_total_debug_entries);
        check_limit(
            OwnedCompileLimit::Bindings,
            self.closed_bindings
                .len()
                .saturating_add(self.bindings.len())
                .saturating_add(1),
            binding_limit,
        )?;
        let start_pc =
            u32::try_from(self.code.len()).map_err(|_| OwnedCompileError::InternalInvariant {
                message: "instruction count passed limits but cannot fit a debug PC",
            })?;
        push_fallible(
            &mut self.bindings,
            Binding {
                name: statement.name().span(),
                register: index,
                start_pc,
                end_pc: None,
            },
            "local bindings",
        )?;

        let start = self.code.len();
        let condition = self.allocate_register()?;
        self.emit(
            Instruction::LessEqual {
                destination: condition,
                left: if ascending { index } else { limit },
                right: if ascending { limit } else { index },
            },
            statement.span(),
        )?;
        let exit = self.code.len();
        self.emit(
            Instruction::JumpIfFalsy {
                condition,
                target: 0,
            },
            statement.span(),
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
        let lowered = self.lower_statements(statement.body().statements());
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
        lowered?;
        self.close_scope(body_scope)?;
        let increment = self.code.len();
        for branch in continues {
            self.patch_forward_branch(branch, increment)?;
        }
        self.emit(
            Instruction::Add {
                destination: index,
                left: index,
                right: step,
            },
            statement.span(),
        )?;
        let target = u32::try_from(start).map_err(|_| OwnedCompileError::InternalInvariant {
            message: "loop target passed limits but cannot fit BluV1",
        })?;
        self.emit(Instruction::Jump { target }, statement.span())?;
        let end = self.code.len();
        self.patch_forward_branch(exit, end)?;
        for branch in breaks {
            self.patch_forward_branch(branch, end)?;
        }
        self.close_scope(loop_scope)
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

    fn close_scope(&mut self, start: usize) -> Result<(), OwnedCompileError> {
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

    fn lower_local(&mut self, statement: LocalStatement) -> Result<(), OwnedCompileError> {
        let register = match statement.value() {
            Some(value) => self.lower_expression(value)?,
            None => self.lower_constant(Constant::Nil, statement.span())?,
        };
        let start_pc =
            u32::try_from(self.code.len()).map_err(|_| OwnedCompileError::InternalInvariant {
                message: "instruction count passed limits but cannot fit a debug PC",
            })?;
        self.push_binding(statement.name().span(), register, start_pc)
    }

    fn push_binding(
        &mut self,
        name: ByteSpan,
        register: u16,
        start_pc: u32,
    ) -> Result<(), OwnedCompileError> {
        let limit = self
            .limits
            .max_bindings
            .min(self.limits.artifact.max_debug_entries_per_prototype)
            .min(self.limits.artifact.max_total_debug_entries);
        check_limit(
            OwnedCompileLimit::Bindings,
            self.closed_bindings
                .len()
                .saturating_add(self.bindings.len())
                .saturating_add(1),
            limit,
        )?;
        push_fallible(
            &mut self.bindings,
            Binding {
                name,
                register,
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
    ) -> Result<u16, OwnedCompileError> {
        let body = self
            .ast
            .function(function)
            .ok_or(OwnedCompileError::InternalInvariant {
                message: "function expression references an absent AST body",
            })?;
        let inherited = self.outer_bindings.len();
        for index in 0..inherited {
            let binding = self.outer_bindings[index];
            self.ensure_upvalue(binding)?;
        }
        let visible_count = self.upvalues.len().saturating_add(self.bindings.len());
        let mut outer_bindings = allocate_vec(visible_count, "child lexical bindings")?;
        for (upvalue, binding) in self.upvalues.iter().enumerate() {
            outer_bindings.push(OuterBinding {
                name: binding.name,
                source: Upvalue::ParentUpvalue(u16::try_from(upvalue).map_err(|_| {
                    OwnedCompileError::InternalInvariant {
                        message: "upvalue count passed limits but cannot fit BluV1",
                    }
                })?),
            });
        }
        outer_bindings.extend(self.bindings.iter().map(|binding| OuterBinding {
            name: binding.name,
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
            &outer_bindings,
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
        self.emit(
            Instruction::NewClosure {
                destination,
                child: u16::try_from(child_slot).map_err(|_| {
                    OwnedCompileError::InternalInvariant {
                        message: "child count passed limits but cannot fit BluV1",
                    }
                })?,
            },
            span,
        )?;
        Ok(destination)
    }

    fn lower_assignment(
        &mut self,
        statement: AssignmentStatement,
    ) -> Result<(), OwnedCompileError> {
        match statement.target() {
            AssignmentTarget::Identifier(identifier) => {
                let source = self.lower_expression(statement.value())?;
                if let Some(destination) = self.resolve_local(identifier.span())? {
                    self.emit(
                        Instruction::Move {
                            destination,
                            source,
                        },
                        statement.span(),
                    )
                } else if let Some(upvalue) = self.resolve_upvalue(identifier.span())? {
                    self.emit(
                        Instruction::SetUpvalue { upvalue, source },
                        statement.span(),
                    )
                } else {
                    let name = self.global_name_constant(identifier.span())?;
                    self.emit(Instruction::StoreGlobal { name, source }, statement.span())
                }
            }
            AssignmentTarget::Index(index) => {
                let table = self.lower_expression(index.table())?;
                let key = self.lower_expression(index.key())?;
                let value = self.lower_expression(statement.value())?;
                self.emit(
                    Instruction::SetTable { table, key, value },
                    statement.span(),
                )
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
                self.emit(
                    Instruction::SetTable { table, key, value },
                    statement.span(),
                )
            }
        }
    }

    fn lower_local_list(
        &mut self,
        statement: &LocalListStatement,
    ) -> Result<(), OwnedCompileError> {
        let required = self
            .closed_bindings
            .len()
            .saturating_add(self.bindings.len())
            .saturating_add(statement.names().len());
        let limit = self
            .limits
            .max_bindings
            .min(self.limits.artifact.max_debug_entries_per_prototype)
            .min(self.limits.artifact.max_total_debug_entries);
        check_limit(OwnedCompileLimit::Bindings, required, limit)?;

        let capacity = statement.names().len().max(statement.values().len());
        let mut registers = allocate_vec(capacity, "local declaration registers")?;
        for value in statement.values().iter().copied() {
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
        let start_pc =
            u32::try_from(self.code.len()).map_err(|_| OwnedCompileError::InternalInvariant {
                message: "instruction count passed limits but cannot fit a debug PC",
            })?;
        for (name, register) in statement
            .names()
            .iter()
            .copied()
            .zip(registers.iter().copied())
        {
            push_fallible(
                &mut self.bindings,
                Binding {
                    name: name.span(),
                    register,
                    start_pc,
                    end_pc: None,
                },
                "local bindings",
            )?;
        }
        Ok(())
    }

    fn lower_assignment_list(
        &mut self,
        statement: &AssignmentListStatement,
    ) -> Result<(), OwnedCompileError> {
        let mut destinations = allocate_vec(
            statement.targets().len(),
            "assignment destination registers",
        )?;
        for target in statement.targets() {
            let AssignmentTarget::Identifier(target) = target else {
                return Err(OwnedCompileError::InternalInvariant {
                    message: "indexed assignment reached list lowering",
                });
            };
            destinations.push(self.resolve(target.span())?);
        }
        let capacity = statement.targets().len().max(statement.values().len());
        let mut sources = allocate_vec(capacity, "assignment source registers")?;
        for (index, value) in statement.values().iter().copied().enumerate() {
            let source = self.lower_expression(value)?;
            if index < destinations.len() {
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
        for (destination, source) in destinations.into_iter().zip(sources) {
            self.emit(
                Instruction::Move {
                    destination,
                    source,
                },
                statement.span(),
            )?;
        }
        Ok(())
    }

    fn lower_return(&mut self, statement: &ReturnStatement) -> Result<(), OwnedCompileError> {
        let values = statement.values();
        let limit = self.limits.max_return_values.min(u16::MAX as usize);
        check_limit(OwnedCompileLimit::ReturnValues, values.len(), limit)?;
        if values.is_empty() {
            return self.emit(Instruction::Return { first: 0, count: 0 }, statement.span());
        }
        let mut registers = allocate_vec(values.len(), "return registers")?;
        for expression_id in values.iter().copied() {
            registers.push(self.lower_expression(expression_id)?);
        }
        let contiguous = registers
            .windows(2)
            .all(|pair| pair[0].checked_add(1) == Some(pair[1]));
        let first = if contiguous {
            registers[0]
        } else {
            self.copy_return_values(values, &registers)?
        };
        let count =
            u16::try_from(values.len()).map_err(|_| OwnedCompileError::InternalInvariant {
                message: "return count passed limits but cannot fit BluV1",
            })?;
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
        let mut array_index = 1_i64;
        for index in constructor.first_field()..end {
            let field = self.table_fields[index];
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
        let end = self.call_argument_end(call.first_argument(), call.argument_count())?;
        let function = self.lower_expression(call.function())?;
        let mut sources = allocate_vec(call.argument_count(), "call argument registers")?;
        for index in call.first_argument()..end {
            sources.push(self.lower_expression(self.call_arguments[index])?);
        }
        self.emit_fixed_call(function, &sources, call.span())
    }

    fn lower_method_call(&mut self, call: MethodCallExpression) -> Result<u16, OwnedCompileError> {
        let end = self.call_argument_end(call.first_argument(), call.argument_count())?;
        let receiver = self.lower_expression(call.receiver())?;
        let key = self.lower_constant(
            Constant::String(copy_bytes(
                self.source.slice(call.method().span())?,
                "method name",
            )?),
            call.method().span(),
        )?;
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
        for index in call.first_argument()..end {
            sources.push(self.lower_expression(self.call_arguments[index])?);
        }
        self.emit_fixed_call(function, &sources, call.span())
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

    fn emit_fixed_call(
        &mut self,
        function: u16,
        sources: &[u16],
        span: ByteSpan,
    ) -> Result<u16, OwnedCompileError> {
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
        let destination = self.allocate_register()?;
        let argument_count =
            u16::try_from(sources.len()).map_err(|_| OwnedCompileError::InternalInvariant {
                message: "call argument count passed limits but cannot fit BluV1",
            })?;
        self.emit(
            Instruction::Call {
                destination,
                function,
                arguments,
                argument_count,
            },
            span,
        )?;
        Ok(destination)
    }

    fn lower_expression(&mut self, id: ExpressionId) -> Result<u16, OwnedCompileError> {
        let expression = *self.expression(id)?;
        match expression.kind() {
            ExpressionKind::Nil => self.lower_constant(Constant::Nil, expression.span()),
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
                self.lower_function(function.function(), function.span())
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
                    let left = self.lower_expression(binary.left())?;
                    let right = self.lower_expression(binary.right())?;
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::Add {
                            destination,
                            left,
                            right,
                        },
                        expression.span(),
                    )?;
                    Ok(destination)
                }
                BinaryOperator::Subtract => {
                    let left = self.lower_expression(binary.left())?;
                    let right = self.lower_expression(binary.right())?;
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::Subtract {
                            destination,
                            left,
                            right,
                        },
                        expression.span(),
                    )?;
                    Ok(destination)
                }
                BinaryOperator::Multiply => {
                    let left = self.lower_expression(binary.left())?;
                    let right = self.lower_expression(binary.right())?;
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::Multiply {
                            destination,
                            left,
                            right,
                        },
                        expression.span(),
                    )?;
                    Ok(destination)
                }
                BinaryOperator::Divide => {
                    let left = self.lower_expression(binary.left())?;
                    let right = self.lower_expression(binary.right())?;
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::Divide {
                            destination,
                            left,
                            right,
                        },
                        expression.span(),
                    )?;
                    Ok(destination)
                }
                BinaryOperator::Modulo => {
                    let left = self.lower_expression(binary.left())?;
                    let right = self.lower_expression(binary.right())?;
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::Modulo {
                            destination,
                            left,
                            right,
                        },
                        expression.span(),
                    )?;
                    Ok(destination)
                }
                BinaryOperator::Power => {
                    let left = self.lower_expression(binary.left())?;
                    let right = self.lower_expression(binary.right())?;
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::Power {
                            destination,
                            left,
                            right,
                        },
                        expression.span(),
                    )?;
                    Ok(destination)
                }
                BinaryOperator::FloorDivide => {
                    if self.profile == SemanticProfile::Blu {
                        return Err(OwnedCompileError::Diagnostic(self.source_diagnostic(
                            "BLU-LOWER-0001",
                            Phase::Lower,
                            binary.operator_span(),
                            "Blu floor-division semantics are not assigned",
                        )?));
                    }
                    let left = self.lower_expression(binary.left())?;
                    let right = self.lower_expression(binary.right())?;
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::FloorDivide {
                            destination,
                            left,
                            right,
                        },
                        expression.span(),
                    )?;
                    Ok(destination)
                }
                BinaryOperator::Concatenate => {
                    let left = self.lower_expression(binary.left())?;
                    let right = self.lower_expression(binary.right())?;
                    let destination = self.allocate_register()?;
                    self.emit(
                        Instruction::Concatenate {
                            destination,
                            left,
                            right,
                        },
                        expression.span(),
                    )?;
                    Ok(destination)
                }
                BinaryOperator::Equal | BinaryOperator::NotEqual => {
                    let left = self.lower_expression(binary.left())?;
                    let right = self.lower_expression(binary.right())?;
                    let compared = self.allocate_register()?;
                    self.emit(
                        Instruction::Equal {
                            destination: compared,
                            left,
                            right,
                        },
                        expression.span(),
                    )?;
                    if binary.operator() == BinaryOperator::NotEqual {
                        let destination = self.allocate_register()?;
                        self.emit(
                            Instruction::Not {
                                destination,
                                source: compared,
                            },
                            expression.span(),
                        )?;
                        Ok(destination)
                    } else {
                        Ok(compared)
                    }
                }
                BinaryOperator::LessThan | BinaryOperator::GreaterThan => {
                    let left = self.lower_expression(binary.left())?;
                    let right = self.lower_expression(binary.right())?;
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
                        expression.span(),
                    )?;
                    Ok(destination)
                }
                BinaryOperator::LessEqual | BinaryOperator::GreaterEqual => {
                    let left = self.lower_expression(binary.left())?;
                    let right = self.lower_expression(binary.right())?;
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
                        expression.span(),
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
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
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
            SemanticProfile::Lua53 | SemanticProfile::Lua54 | SemanticProfile::Lua55
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
        let number = digits
            .iter()
            .filter(|byte| **byte != b'_')
            .fold(0.0_f64, |value, byte| {
                value.mul_add(2.0, f64::from(*byte - b'0'))
            });
        Ok(Constant::Number(number))
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

    fn resolve(&self, name: ByteSpan) -> Result<u16, OwnedCompileError> {
        if let Some(register) = self.resolve_local(name)? {
            Ok(register)
        } else {
            Err(OwnedCompileError::Diagnostic(self.source_diagnostic(
                "BLU-RESOLVE-0001",
                Phase::Resolve,
                name,
                "local name is unresolved",
            )?))
        }
    }

    fn resolve_local(&self, name: ByteSpan) -> Result<Option<u16>, OwnedCompileError> {
        let bytes = self.source.slice(name)?;
        for binding in self.bindings.iter().rev() {
            if self.source.slice(binding.name)? == bytes {
                return Ok(Some(binding.register));
            }
        }
        Ok(None)
    }

    fn resolve_upvalue(&mut self, name: ByteSpan) -> Result<Option<u16>, OwnedCompileError> {
        let bytes = self.source.slice(name)?;
        for (index, binding) in self.upvalues.iter().enumerate().rev() {
            if self.source.slice(binding.name)? == bytes {
                return Ok(Some(u16::try_from(index).map_err(|_| {
                    OwnedCompileError::InternalInvariant {
                        message: "upvalue count passed limits but cannot fit BluV1",
                    }
                })?));
            }
        }
        for binding in self.outer_bindings.iter().rev() {
            if self.source.slice(binding.name)? == bytes {
                let binding = *binding;
                let index = self.push_upvalue(binding)?;
                return Ok(Some(index));
            }
        }
        Ok(None)
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
        return Ok(DecodedEscape::single(b'\n', 2));
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
        while value.get(cursor).is_some_and(u8::is_ascii_whitespace) {
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
        [b'\n', ..] => 1,
        [b'\r', ..] if profile != SemanticProfile::Luau => 1,
        _ => 0,
    }
}

fn decoded_long_string_len(value: &[u8], profile: SemanticProfile) -> usize {
    let value = &value[long_string_content_start(value, profile)..];
    value.len() - value.windows(2).filter(|window| *window == b"\r\n").count()
}

fn decode_long_string(
    value: &[u8],
    profile: SemanticProfile,
    decoded: &mut Vec<u8>,
) -> Result<(), OwnedCompileError> {
    let mut offset = long_string_content_start(value, profile);
    while offset < value.len() {
        if value[offset] == b'\r' && value.get(offset + 1) == Some(&b'\n') {
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
