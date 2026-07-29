#![forbid(unsafe_code)]
//! Byte-oriented lexical analysis for the first Blu-owned frontend slice.
//!
//! This crate does not parse or compile source. It retains whitespace and
//! comments, reconciles an optional initial dialect directive with an explicit
//! semantic profile, and recognizes only the tokens represented by
//! [`TokenKind`]. All source inspection is performed on bytes.

use blu_core::{
    ByteSpan, Diagnostic, DiagnosticCode, Label, Phase, SemanticProfile, Severity, SourceFile,
    SpanError,
};
use core::fmt;

const DIRECTIVE_PREFIX: &[u8] = b"--!dialect";
const FOUND_BYTE_LIMIT: usize = 32;

/// Resource limits applied while lexing one already-bounded [`SourceFile`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LexerLimits {
    pub max_tokens: usize,
    pub max_diagnostics: usize,
}

impl Default for LexerLimits {
    fn default() -> Self {
        Self {
            max_tokens: 1_000_000,
            max_diagnostics: 10_000,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LexerLimit {
    Tokens,
    Diagnostics,
}

impl fmt::Display for LexerLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tokens => formatter.write_str("lexer tokens"),
            Self::Diagnostics => formatter.write_str("lexer diagnostics"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LexError {
    Limit {
        kind: LexerLimit,
        required: usize,
        limit: usize,
    },
    AllocationFailed {
        what: &'static str,
        requested: usize,
    },
    Span(SpanError),
}

impl fmt::Display for LexError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Limit {
                kind,
                required,
                limit,
            } => write!(
                formatter,
                "{kind} require {required}, exceeding limit {limit}"
            ),
            Self::AllocationFailed { what, requested } => {
                write!(
                    formatter,
                    "failed to allocate {what} for {requested} entries"
                )
            }
            Self::Span(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for LexError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Span(error) => Some(error),
            Self::Limit { .. } | Self::AllocationFailed { .. } => None,
        }
    }
}

impl From<SpanError> for LexError {
    fn from(error: SpanError) -> Self {
        Self::Span(error)
    }
}

/// Token kinds supported by this dependency-gated lexer slice.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum TokenKind {
    Whitespace,
    Comment,
    DialectDirective,
    Local,
    Return,
    Identifier,
    DecimalInteger,
    Equal,
    Plus,
    FloorDivide,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Token {
    kind: TokenKind,
    span: ByteSpan,
}

impl Token {
    #[must_use]
    pub const fn new(kind: TokenKind, span: ByteSpan) -> Self {
        Self { kind, span }
    }

    #[must_use]
    pub const fn kind(self) -> TokenKind {
        self.kind
    }

    #[must_use]
    pub const fn span(self) -> ByteSpan {
        self.span
    }
}

/// A valid initial source directive. Its span excludes the line terminator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DialectDirective {
    profile: SemanticProfile,
    span: ByteSpan,
    value_span: ByteSpan,
}

impl DialectDirective {
    #[must_use]
    pub const fn profile(self) -> SemanticProfile {
        self.profile
    }

    #[must_use]
    pub const fn span(self) -> ByteSpan {
        self.span
    }

    #[must_use]
    pub const fn value_span(self) -> ByteSpan {
        self.value_span
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Lexed {
    profile: SemanticProfile,
    directive: Option<DialectDirective>,
    tokens: Vec<Token>,
    diagnostics: Vec<Diagnostic>,
}

impl Lexed {
    /// The explicit caller-selected profile. A directive can confirm it but
    /// cannot replace it.
    #[must_use]
    pub const fn profile(&self) -> SemanticProfile {
        self.profile
    }

    #[must_use]
    pub const fn directive(&self) -> Option<DialectDirective> {
        self.directive
    }

    #[must_use]
    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    #[must_use]
    pub fn has_errors(&self) -> bool {
        !self.diagnostics.is_empty()
    }
}

/// Lexes the owned bytes in `source` under an explicit semantic profile.
///
/// An initial `--!dialect <profile>` directive is recognized only at byte
/// offset zero. A valid directive must agree with `explicit_profile`; a
/// conflict is diagnosed on the directive value and the explicit profile
/// remains authoritative.
pub fn lex(
    source: &SourceFile,
    explicit_profile: SemanticProfile,
    limits: LexerLimits,
) -> Result<Lexed, LexError> {
    let bytes = source.bytes();
    let token_capacity = bytes.len().min(limits.max_tokens).min(4_096);
    let diagnostic_capacity = bytes.len().min(limits.max_diagnostics).min(16);
    let mut tokens = allocate_vec(token_capacity, "lexer tokens")?;
    let mut diagnostics = allocate_vec(diagnostic_capacity, "lexer diagnostics")?;
    let mut directive = None;
    let mut offset = 0;

    if is_initial_directive(bytes) {
        let line_end = line_end(bytes, 0);
        let directive_end = if line_end > 0 && bytes[line_end - 1] == b'\r' {
            line_end - 1
        } else {
            line_end
        };
        let span = source.span(0, directive_end)?;
        push_token(
            &mut tokens,
            Token::new(TokenKind::DialectDirective, span),
            limits.max_tokens,
        )?;
        directive = parse_directive(
            source,
            explicit_profile,
            line_end,
            span,
            &mut diagnostics,
            limits.max_diagnostics,
        )?;
        offset = directive_end;
    }

    while offset < bytes.len() {
        let start = offset;
        let kind = match bytes[offset] {
            byte if is_whitespace(byte) => {
                offset += 1;
                while offset < bytes.len() && is_whitespace(bytes[offset]) {
                    offset += 1;
                }
                TokenKind::Whitespace
            }
            b'-' if bytes.get(offset + 1) == Some(&b'-') => {
                if let Some((delimiter_len, equals)) = long_comment_opener(bytes, offset + 2) {
                    let opener_end = offset + 2 + delimiter_len;
                    if let Some(end) = long_comment_end(bytes, opener_end, equals) {
                        offset = end;
                    } else {
                        offset = bytes.len();
                        let opener_span = source.span(offset_of_opener(start), opener_end)?;
                        let diagnostic = diagnostic(
                            "BLU-LEX-0006",
                            explicit_profile,
                            opener_span,
                            "unterminated long comment",
                        )
                        .with_expected("long-comment closing delimiter");
                        push_diagnostic(&mut diagnostics, diagnostic, limits.max_diagnostics)?;
                    }
                } else {
                    offset = line_content_end(bytes, offset);
                }
                TokenKind::Comment
            }
            b'/' if bytes.get(offset + 1) == Some(&b'/') => {
                offset += 2;
                if !supports_floor_division(explicit_profile) {
                    let span = source.span(start, offset)?;
                    let diagnostic = diagnostic(
                        "BLU-LEX-0002",
                        explicit_profile,
                        span,
                        "floor-division syntax is unavailable in this profile",
                    )
                    .with_found(copy_found(&bytes[start..offset])?)
                    .with_note(format!("selected profile: {explicit_profile}"));
                    push_diagnostic(&mut diagnostics, diagnostic, limits.max_diagnostics)?;
                }
                TokenKind::FloorDivide
            }
            b'=' => {
                offset += 1;
                TokenKind::Equal
            }
            b'+' => {
                offset += 1;
                TokenKind::Plus
            }
            b'0'..=b'9' => {
                offset += 1;
                while offset < bytes.len() && bytes[offset].is_ascii_digit() {
                    offset += 1;
                }
                TokenKind::DecimalInteger
            }
            byte if is_identifier_start(byte) => {
                offset += 1;
                while offset < bytes.len() && is_identifier_continue(bytes[offset]) {
                    offset += 1;
                }
                match &bytes[start..offset] {
                    b"local" => TokenKind::Local,
                    b"return" => TokenKind::Return,
                    _ => TokenKind::Identifier,
                }
            }
            _ => {
                offset += 1;
                let span = source.span(start, offset)?;
                let diagnostic = diagnostic(
                    "BLU-LEX-0001",
                    explicit_profile,
                    span,
                    "unrecognized source byte",
                )
                .with_found(copy_found(&bytes[start..offset])?);
                push_diagnostic(&mut diagnostics, diagnostic, limits.max_diagnostics)?;
                TokenKind::Unknown
            }
        };

        let span = source.span(start, offset)?;
        push_token(&mut tokens, Token::new(kind, span), limits.max_tokens)?;
    }

    Ok(Lexed {
        profile: explicit_profile,
        directive,
        tokens,
        diagnostics,
    })
}

fn is_initial_directive(bytes: &[u8]) -> bool {
    let Some(rest) = bytes.strip_prefix(DIRECTIVE_PREFIX) else {
        return false;
    };
    rest.first()
        .is_none_or(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
}

fn parse_directive(
    source: &SourceFile,
    explicit_profile: SemanticProfile,
    line_end: usize,
    directive_span: ByteSpan,
    diagnostics: &mut Vec<Diagnostic>,
    diagnostic_limit: usize,
) -> Result<Option<DialectDirective>, LexError> {
    let bytes = source.bytes();
    let content_end = if line_end > 0 && bytes[line_end - 1] == b'\r' {
        line_end - 1
    } else {
        line_end
    };
    let mut value_start = DIRECTIVE_PREFIX.len();
    while value_start < content_end && matches!(bytes[value_start], b' ' | b'\t') {
        value_start += 1;
    }
    let mut value_end = value_start;
    while value_end < content_end && !matches!(bytes[value_end], b' ' | b'\t') {
        value_end += 1;
    }
    let mut trailing = value_end;
    while trailing < content_end && matches!(bytes[trailing], b' ' | b'\t') {
        trailing += 1;
    }

    if value_start == value_end {
        let span = source.span(value_start, value_end)?;
        let diagnostic = add_profile_expectations(diagnostic(
            "BLU-LEX-0003",
            explicit_profile,
            span,
            "dialect directive is missing a profile",
        ));
        push_diagnostic(diagnostics, diagnostic, diagnostic_limit)?;
        return Ok(None);
    }

    if trailing != content_end {
        let span = source.span(trailing, content_end)?;
        let diagnostic = diagnostic(
            "BLU-LEX-0003",
            explicit_profile,
            span,
            "unexpected bytes after dialect profile",
        )
        .with_found(copy_found(&bytes[trailing..content_end])?)
        .with_expected("end of directive");
        push_diagnostic(diagnostics, diagnostic, diagnostic_limit)?;
        return Ok(None);
    }

    let value_span = source.span(value_start, value_end)?;
    let Some(profile) = profile_from_bytes(&bytes[value_start..value_end]) else {
        let diagnostic = add_profile_expectations(
            diagnostic(
                "BLU-LEX-0004",
                explicit_profile,
                value_span,
                "unknown dialect profile",
            )
            .with_found(copy_found(&bytes[value_start..value_end])?),
        );
        push_diagnostic(diagnostics, diagnostic, diagnostic_limit)?;
        return Ok(None);
    };

    let directive = DialectDirective {
        profile,
        span: directive_span,
        value_span,
    };
    if profile != explicit_profile {
        let diagnostic = diagnostic(
            "BLU-LEX-0005",
            explicit_profile,
            value_span,
            "source directive conflicts with the explicit profile",
        )
        .with_found(copy_found(&bytes[value_start..value_end])?)
        .with_note(format!("explicit profile: {explicit_profile}"))
        .with_expected(explicit_profile.as_str());
        push_diagnostic(diagnostics, diagnostic, diagnostic_limit)?;
    }
    Ok(Some(directive))
}

fn add_profile_expectations(mut diagnostic: Diagnostic) -> Diagnostic {
    for profile in SemanticProfile::ALL {
        diagnostic = diagnostic.with_expected(profile.as_str());
    }
    diagnostic
}

fn profile_from_bytes(bytes: &[u8]) -> Option<SemanticProfile> {
    match bytes {
        b"blu" => Some(SemanticProfile::Blu),
        b"luau" => Some(SemanticProfile::Luau),
        b"lua51" => Some(SemanticProfile::Lua51),
        b"lua52" => Some(SemanticProfile::Lua52),
        b"lua53" => Some(SemanticProfile::Lua53),
        b"lua54" => Some(SemanticProfile::Lua54),
        b"lua55" => Some(SemanticProfile::Lua55),
        _ => None,
    }
}

fn supports_floor_division(profile: SemanticProfile) -> bool {
    match profile {
        SemanticProfile::Blu
        | SemanticProfile::Luau
        | SemanticProfile::Lua53
        | SemanticProfile::Lua54
        | SemanticProfile::Lua55 => true,
        SemanticProfile::Lua51 | SemanticProfile::Lua52 => false,
        _ => false,
    }
}

fn long_comment_opener(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
    if bytes.get(start) != Some(&b'[') {
        return None;
    }
    let mut cursor = start + 1;
    while bytes.get(cursor) == Some(&b'=') {
        cursor += 1;
    }
    (bytes.get(cursor) == Some(&b'[')).then_some((cursor - start + 1, cursor - start - 1))
}

fn long_comment_end(bytes: &[u8], mut cursor: usize, equals: usize) -> Option<usize> {
    while cursor < bytes.len() {
        if bytes[cursor] == b']' {
            let equals_end = cursor.checked_add(1 + equals)?;
            if equals_end < bytes.len()
                && bytes[cursor + 1..equals_end]
                    .iter()
                    .all(|byte| *byte == b'=')
                && bytes[equals_end] == b']'
            {
                return Some(equals_end + 1);
            }
        }
        cursor += 1;
    }
    None
}

const fn offset_of_opener(comment_start: usize) -> usize {
    comment_start + 2
}

fn line_end(bytes: &[u8], start: usize) -> usize {
    bytes[start..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(bytes.len(), |relative| start + relative)
}

fn line_content_end(bytes: &[u8], start: usize) -> usize {
    let end = line_end(bytes, start);
    if end > start && bytes[end - 1] == b'\r' {
        end - 1
    } else {
        end
    }
}

const fn is_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

const fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

const fn is_identifier_continue(byte: u8) -> bool {
    is_identifier_start(byte) || byte.is_ascii_digit()
}

fn diagnostic(
    code: &'static str,
    profile: SemanticProfile,
    span: ByteSpan,
    message: &'static str,
) -> Diagnostic {
    Diagnostic::new(
        DiagnosticCode::new(code).expect("fixed lexer diagnostic code must be valid"),
        Phase::Lex,
        profile,
        Severity::Error,
        Label::new(span, message),
    )
}

fn copy_found(bytes: &[u8]) -> Result<Vec<u8>, LexError> {
    let copied_len = bytes.len().min(FOUND_BYTE_LIMIT);
    let mut copied = allocate_vec(copied_len, "diagnostic found bytes")?;
    copied.extend_from_slice(&bytes[..copied_len]);
    Ok(copied)
}

fn allocate_vec<T>(capacity: usize, what: &'static str) -> Result<Vec<T>, LexError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| LexError::AllocationFailed {
            what,
            requested: capacity,
        })?;
    Ok(values)
}

fn push_token(tokens: &mut Vec<Token>, token: Token, limit: usize) -> Result<(), LexError> {
    push_limited(tokens, token, LexerLimit::Tokens, limit, "lexer tokens")
}

fn push_diagnostic(
    diagnostics: &mut Vec<Diagnostic>,
    diagnostic: Diagnostic,
    limit: usize,
) -> Result<(), LexError> {
    push_limited(
        diagnostics,
        diagnostic,
        LexerLimit::Diagnostics,
        limit,
        "lexer diagnostics",
    )
}

fn push_limited<T>(
    values: &mut Vec<T>,
    value: T,
    kind: LexerLimit,
    limit: usize,
    what: &'static str,
) -> Result<(), LexError> {
    let required = values.len().saturating_add(1);
    if required > limit {
        return Err(LexError::Limit {
            kind,
            required,
            limit,
        });
    }
    if values.len() == values.capacity() {
        values
            .try_reserve(1)
            .map_err(|_| LexError::AllocationFailed {
                what,
                requested: required,
            })?;
    }
    values.push(value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{LexError, allocate_vec};

    #[test]
    fn source_sized_vector_allocation_failure_is_structured() {
        assert_eq!(
            allocate_vec::<u8>(usize::MAX, "test entries"),
            Err(LexError::AllocationFailed {
                what: "test entries",
                requested: usize::MAX,
            })
        );
    }
}
