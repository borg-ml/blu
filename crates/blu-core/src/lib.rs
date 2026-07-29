#![forbid(unsafe_code)]
//! Dependency-free core types shared by future Blu frontend stages.
//!
//! This crate deliberately does not migrate existing compiler, package, or
//! runtime consumers. It establishes bounded byte-source, identity, span,
//! semantic-profile, and bounded, fallibly allocated diagnostic contracts for
//! that later work. These object limits do not constitute process-wide memory
//! accounting.

mod diagnostic;
mod identity;
mod profile;
mod source;
mod span;

pub use diagnostic::{
    Diagnostic, DiagnosticCode, DiagnosticCodeError, DiagnosticError, DiagnosticLimit,
    DiagnosticLimits, Label, Phase, Severity,
};
pub use identity::{
    CompilerId, CompilerIdentity, IdentityError, IdentityLimits, SourceId, SourceIdentity,
};
pub use profile::{ParseSemanticProfileError, SemanticProfile};
pub use source::{
    LineIndex, LineIndexError, LinePosition, SourceError, SourceFile, SourceLimit, SourceLimits,
};
pub use span::{ByteOffset, ByteSpan, SpanError};
