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
    Instruction, LocalDebug, Prototype, SourceRecord, ValidatedArtifact, ValidationError,
    decode_validated, encode,
};
use blu_core::{
    ByteSpan, CompilerIdentity, Diagnostic, DiagnosticError, IdentityError, Phase, SemanticProfile,
    Severity, SourceFile, SourceIdentity, SpanError,
};
use blu_syntax::{
    AssignmentListStatement, AssignmentStatement, Ast, BinaryOperator, Expression, ExpressionId,
    ExpressionKind, LocalListStatement, LocalStatement, ParseError, ParseLimits, ParseOutcome,
    Rejected, ReturnStatement, Statement, UnaryOperator, parse,
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
    IntegerLiteralBytes,
    StringLiteralBytes,
    TotalConstantBytes,
    SourceNameBytes,
    DebugNameBytes,
    TotalDebugBytes,
}

impl fmt::Display for OwnedCompileLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bindings => formatter.write_str("local bindings"),
            Self::Registers => formatter.write_str("registers"),
            Self::Constants => formatter.write_str("constants"),
            Self::Instructions => formatter.write_str("instructions"),
            Self::ReturnValues => formatter.write_str("return values"),
            Self::IntegerLiteralBytes => formatter.write_str("integer literal bytes"),
            Self::StringLiteralBytes => formatter.write_str("string literal bytes"),
            Self::TotalConstantBytes => formatter.write_str("total constant bytes"),
            Self::SourceNameBytes => formatter.write_str("source identity name bytes"),
            Self::DebugNameBytes => formatter.write_str("local debug name bytes"),
            Self::TotalDebugBytes => formatter.write_str("total local debug name bytes"),
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
        let prototype = Lowerer::new(source, parsed.ast(), self.limits)?.run(parsed.ast())?;
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
        let mut prototypes = allocate_vec(1, "artifact prototypes")?;
        prototypes.push(prototype);
        let artifact = Artifact {
            format: BytecodeFormat::BluV1,
            compiler: compiler_identity,
            sources,
            prototypes,
            main: 0,
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
}

struct Lowerer<'a> {
    source: &'a SourceFile,
    profile: SemanticProfile,
    expressions: &'a [Expression],
    limits: OwnedCompileLimits,
    bindings: Vec<Binding>,
    register_count: usize,
    constants: Vec<Constant>,
    constant_bytes: usize,
    code: Vec<Instruction>,
    source_map: Vec<ByteSpan>,
}

impl<'a> Lowerer<'a> {
    fn new(
        source: &'a SourceFile,
        ast: &'a Ast,
        limits: OwnedCompileLimits,
    ) -> Result<Self, OwnedCompileError> {
        let capacity = ast.node_count().min(4_096);
        Ok(Self {
            source,
            profile: ast.profile(),
            expressions: ast.expressions(),
            limits,
            bindings: allocate_vec(capacity.min(limits.max_bindings), "local bindings")?,
            register_count: 0,
            constants: allocate_vec(capacity.min(limits.max_constants), "constants")?,
            constant_bytes: 0,
            code: allocate_vec(capacity.min(limits.max_instructions), "instructions")?,
            source_map: allocate_vec(capacity.min(limits.max_instructions), "source map")?,
        })
    }

    fn run(mut self, ast: &Ast) -> Result<Prototype, OwnedCompileError> {
        let mut saw_return = false;
        for (index, statement) in ast.statements().iter().enumerate() {
            match statement {
                Statement::Local(local) => self.lower_local(*local)?,
                Statement::LocalList(local) => self.lower_local_list(local)?,
                Statement::Assignment(assignment) => self.lower_assignment(*assignment)?,
                Statement::AssignmentList(assignment) => self.lower_assignment_list(assignment)?,
                Statement::Return(return_statement) => {
                    if index + 1 != ast.statements().len() {
                        return Err(OwnedCompileError::InternalInvariant {
                            message: "parser exposed a non-final return statement",
                        });
                    }
                    self.lower_return(return_statement)?;
                    saw_return = true;
                }
            }
        }
        if !saw_return {
            let eof = self.source.span(self.source.len(), self.source.len())?;
            self.emit(Instruction::Return { first: 0, count: 0 }, eof)?;
        }

        let end_pc =
            u32::try_from(self.code.len()).map_err(|_| OwnedCompileError::InternalInvariant {
                message: "instruction count passed limits but cannot fit a debug PC",
            })?;
        let mut debug_bytes = 0_usize;
        for binding in &self.bindings {
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
        let mut locals = allocate_vec(self.bindings.len(), "local debug entries")?;
        for binding in self.bindings {
            let name = copy_bytes(self.source.slice(binding.name)?, "local debug name")?;
            locals.push(LocalDebug {
                name,
                register: binding.register,
                start_pc: binding.start_pc,
                end_pc,
            });
        }

        let register_count = u16::try_from(self.register_count).map_err(|_| {
            OwnedCompileError::InternalInvariant {
                message: "register count passed limits but cannot fit BluV1",
            }
        })?;
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
        Ok(Prototype {
            profile: ast.profile(),
            source: self.source.identity().id(),
            register_count,
            parameter_count: 0,
            is_vararg: false,
            required_features,
            constants: self.constants,
            upvalues: Vec::new(),
            children: Vec::new(),
            code: self.code,
            source_map: self.source_map,
            locals,
            upvalue_debug: Vec::new(),
        })
    }

    fn lower_local(&mut self, statement: LocalStatement) -> Result<(), OwnedCompileError> {
        let register = match statement.value() {
            Some(value) => self.lower_expression(value)?,
            None => self.lower_constant(Constant::Nil, statement.span())?,
        };
        let limit = self
            .limits
            .max_bindings
            .min(self.limits.artifact.max_debug_entries_per_prototype)
            .min(self.limits.artifact.max_total_debug_entries);
        check_limit(
            OwnedCompileLimit::Bindings,
            self.bindings.len().saturating_add(1),
            limit,
        )?;
        let start_pc =
            u32::try_from(self.code.len()).map_err(|_| OwnedCompileError::InternalInvariant {
                message: "instruction count passed limits but cannot fit a debug PC",
            })?;
        push_fallible(
            &mut self.bindings,
            Binding {
                name: statement.name().span(),
                register,
                start_pc,
            },
            "local bindings",
        )
    }

    fn lower_assignment(
        &mut self,
        statement: AssignmentStatement,
    ) -> Result<(), OwnedCompileError> {
        let destination = self.resolve(statement.target().span())?;
        let source = self.lower_expression(statement.value())?;
        self.emit(
            Instruction::Move {
                destination,
                source,
            },
            statement.span(),
        )
    }

    fn lower_local_list(
        &mut self,
        statement: &LocalListStatement,
    ) -> Result<(), OwnedCompileError> {
        let required = self.bindings.len().saturating_add(statement.names().len());
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
            ExpressionKind::StringLiteral => {
                let constant = self.string_constant(expression.span())?;
                self.lower_constant(constant, expression.span())
            }
            ExpressionKind::Identifier(identifier) => self.resolve(identifier.span()),
            ExpressionKind::Group(inner) => self.lower_expression(inner),
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
            },
            ExpressionKind::Binary(binary) => match binary.operator() {
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
            if !byte.is_ascii_digit() {
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

        let text =
            core::str::from_utf8(bytes).map_err(|_| OwnedCompileError::InternalInvariant {
                message: "decimal-integer AST is not ASCII",
            })?;
        let number = text
            .parse::<f64>()
            .map_err(|_| OwnedCompileError::InternalInvariant {
                message: "validated decimal-integer text failed numeric parsing",
            })?;
        Ok(Constant::Number(number))
    }

    fn string_constant(&mut self, span: ByteSpan) -> Result<Constant, OwnedCompileError> {
        let bytes = self.source.slice(span)?;
        let Some((&quote, rest)) = bytes.split_first() else {
            return Err(OwnedCompileError::InternalInvariant {
                message: "string-literal AST is empty",
            });
        };
        let Some((&closing, value)) = rest.split_last() else {
            return Err(OwnedCompileError::InternalInvariant {
                message: "string-literal AST has no closing quote",
            });
        };
        if !matches!(quote, b'\'' | b'"') || closing != quote {
            return Err(OwnedCompileError::InternalInvariant {
                message: "string-literal AST delimiters do not match",
            });
        }
        let decoded_len = decoded_string_len(value)?;
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
        let mut offset = 0;
        while offset < value.len() {
            let byte = value[offset];
            if byte == b'\\' {
                let escaped =
                    *value
                        .get(offset + 1)
                        .ok_or(OwnedCompileError::InternalInvariant {
                            message: "validated string literal ends in a backslash",
                        })?;
                push_fallible(
                    &mut decoded,
                    decode_string_escape(escaped).ok_or(OwnedCompileError::InternalInvariant {
                        message: "validated string literal contains an unsupported escape",
                    })?,
                    "string literal bytes",
                )?;
                offset += 2;
            } else {
                push_fallible(&mut decoded, byte, "string literal bytes")?;
                offset += 1;
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
        let bytes = self.source.slice(name)?;
        for binding in self.bindings.iter().rev() {
            if self.source.slice(binding.name)? == bytes {
                return Ok(binding.register);
            }
        }
        Err(OwnedCompileError::Diagnostic(self.source_diagnostic(
            "BLU-RESOLVE-0001",
            Phase::Resolve,
            name,
            "local name is unresolved",
        )?))
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

fn copy_bytes(bytes: &[u8], what: &'static str) -> Result<Vec<u8>, OwnedCompileError> {
    let mut copied = allocate_vec(bytes.len(), what)?;
    copied.extend_from_slice(bytes);
    Ok(copied)
}

fn decode_string_escape(escaped: u8) -> Option<u8> {
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

fn decoded_string_len(value: &[u8]) -> Result<usize, OwnedCompileError> {
    let mut decoded_len = 0_usize;
    let mut offset = 0;
    while offset < value.len() {
        if value[offset] == b'\\' {
            let escaped = *value
                .get(offset + 1)
                .ok_or(OwnedCompileError::InternalInvariant {
                    message: "validated string literal ends in a backslash",
                })?;
            if decode_string_escape(escaped).is_none() {
                return Err(OwnedCompileError::InternalInvariant {
                    message: "validated string literal contains an unsupported escape",
                });
            }
            offset += 2;
        } else {
            offset += 1;
        }
        decoded_len = decoded_len
            .checked_add(1)
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
