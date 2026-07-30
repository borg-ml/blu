#![forbid(unsafe_code)]
//! Byte-oriented syntax analysis for the first Blu-owned frontend slice.
//!
//! The lexer retains whitespace and comments, reconciles an optional initial
//! dialect directive with an explicit semantic profile, and recognizes only
//! the tokens represented by [`TokenKind`]. The bounded parser accepts only
//! the small grammar documented by [`parse`]. This crate does not resolve,
//! lower, compile, or execute source. All source inspection is performed on
//! bytes.

mod ast;
mod parser;

pub use ast::{
    AssignmentListStatement, AssignmentStatement, AssignmentTarget, Ast, BinaryExpression,
    BinaryOperator, Block, BreakStatement, CallExpression, CallStatement, ContinueStatement,
    DoStatement, Expression, ExpressionId, ExpressionKind, FieldExpression, FunctionBody,
    FunctionExpression, FunctionId, FunctionStatement, GenericForStatement, Identifier, IfClause,
    IfStatement, IndexExpression, LocalFunctionStatement, LocalListStatement, LocalStatement,
    MethodCallExpression, NumericForStatement, RepeatStatement, ReturnStatement, Statement,
    TableConstructor, TableField, UnaryExpression, UnaryOperator, WhileStatement,
};
pub use parser::{ParseError, ParseLimit, ParseLimits, ParseOutcome, Parsed, Rejected, parse};

use blu_core::{
    ByteSpan, Diagnostic, DiagnosticError, DiagnosticLimits, Phase, SemanticProfile, Severity,
    SourceFile, SpanError,
};
use core::fmt;

const DIRECTIVE_PREFIX: &[u8] = b"--!dialect";

/// Resource limits applied while lexing one already-bounded [`SourceFile`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LexerLimits {
    pub max_tokens: usize,
    pub max_diagnostics: usize,
    /// Per-diagnostic owned-value limits used by the lexer and parser.
    pub diagnostic_limits: DiagnosticLimits,
}

impl Default for LexerLimits {
    fn default() -> Self {
        Self {
            max_tokens: 1_000_000,
            max_diagnostics: 10_000,
            diagnostic_limits: DiagnosticLimits::default(),
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
    Diagnostic(DiagnosticError),
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
            Self::Diagnostic(error) => error.fmt(formatter),
            Self::Span(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for LexError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Diagnostic(error) => Some(error),
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

impl From<DiagnosticError> for LexError {
    fn from(error: DiagnosticError) -> Self {
        Self::Diagnostic(error)
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
    Function,
    Return,
    Not,
    And,
    Or,
    If,
    Then,
    ElseIf,
    Else,
    End,
    While,
    Do,
    Repeat,
    Until,
    For,
    In,
    Break,
    Continue,
    Nil,
    True,
    False,
    Identifier,
    DecimalInteger,
    DecimalNumber,
    HexInteger,
    HexNumber,
    BinaryInteger,
    StringLiteral,
    Equal,
    EqualEqual,
    NotEqual,
    LessThan,
    LessEqual,
    GreaterThan,
    GreaterEqual,
    Comma,
    Semicolon,
    Colon,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Caret,
    Hash,
    Concatenate,
    Dot,
    FloorDivide,
    LeftParenthesis,
    RightParenthesis,
    LeftBrace,
    RightBrace,
    LeftBracket,
    RightBracket,
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

#[derive(Debug, Eq, PartialEq)]
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
            limits.diagnostic_limits,
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
                            limits.diagnostic_limits,
                        )?
                        .try_with_expected("long-comment closing delimiter")?;
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
                        limits.diagnostic_limits,
                    )?
                    .try_with_found(&bytes[start..offset])?
                    .try_with_note_parts(&["selected profile: ", explicit_profile.as_str()])?;
                    push_diagnostic(&mut diagnostics, diagnostic, limits.max_diagnostics)?;
                }
                TokenKind::FloorDivide
            }
            b'/' => {
                offset += 1;
                TokenKind::Slash
            }
            b'=' if bytes.get(offset + 1) == Some(&b'=') => {
                offset += 2;
                TokenKind::EqualEqual
            }
            b'=' => {
                offset += 1;
                TokenKind::Equal
            }
            b'~' if bytes.get(offset + 1) == Some(&b'=') => {
                offset += 2;
                TokenKind::NotEqual
            }
            b'<' if bytes.get(offset + 1) == Some(&b'=') => {
                offset += 2;
                TokenKind::LessEqual
            }
            b'<' => {
                offset += 1;
                TokenKind::LessThan
            }
            b'>' if bytes.get(offset + 1) == Some(&b'=') => {
                offset += 2;
                TokenKind::GreaterEqual
            }
            b'>' => {
                offset += 1;
                TokenKind::GreaterThan
            }
            b',' => {
                offset += 1;
                TokenKind::Comma
            }
            b';' => {
                offset += 1;
                TokenKind::Semicolon
            }
            b':' => {
                offset += 1;
                TokenKind::Colon
            }
            b'+' => {
                offset += 1;
                TokenKind::Plus
            }
            b'-' => {
                offset += 1;
                TokenKind::Minus
            }
            b'*' => {
                offset += 1;
                TokenKind::Star
            }
            b'%' => {
                offset += 1;
                TokenKind::Percent
            }
            b'^' => {
                offset += 1;
                TokenKind::Caret
            }
            b'#' => {
                offset += 1;
                TokenKind::Hash
            }
            b'{' => {
                offset += 1;
                TokenKind::LeftBrace
            }
            b'}' => {
                offset += 1;
                TokenKind::RightBrace
            }
            b']' => {
                offset += 1;
                TokenKind::RightBracket
            }
            b'.' if bytes.get(offset + 1) == Some(&b'.') => {
                offset += 2;
                TokenKind::Concatenate
            }
            b'[' if long_comment_opener(bytes, offset).is_some() => {
                let (delimiter_len, equals) =
                    long_comment_opener(bytes, offset).expect("guard checked long delimiter");
                let opener_end = offset + delimiter_len;
                if let Some(end) = long_comment_end(bytes, opener_end, equals) {
                    offset = end;
                } else {
                    offset = bytes.len();
                    let opener_span = source.span(start, opener_end)?;
                    let diagnostic = diagnostic(
                        "BLU-LEX-0019",
                        explicit_profile,
                        opener_span,
                        "unterminated long string",
                        limits.diagnostic_limits,
                    )?
                    .try_with_expected("long-string closing delimiter")?;
                    push_diagnostic(&mut diagnostics, diagnostic, limits.max_diagnostics)?;
                }
                TokenKind::StringLiteral
            }
            b'[' => {
                offset += 1;
                TokenKind::LeftBracket
            }
            quote @ (b'\'' | b'"') => {
                offset += 1;
                let mut unsupported_escape = None;
                let mut malformed_escape = None;
                let mut malformed_unicode = None;
                while offset < bytes.len()
                    && bytes[offset] != quote
                    && !matches!(bytes[offset], b'\r' | b'\n')
                {
                    if bytes[offset] == b'\\' {
                        let Some(escaped) = bytes.get(offset + 1).copied() else {
                            unsupported_escape.get_or_insert(offset);
                            offset += 1;
                            continue;
                        };
                        if escaped == b'\n' {
                            offset += 2;
                            continue;
                        }
                        if escaped == b'\r' {
                            offset += if bytes.get(offset + 2) == Some(&b'\n') {
                                3
                            } else {
                                2
                            };
                            continue;
                        }
                        if matches!(
                            escaped,
                            b'\\' | b'\'' | b'"' | b'a' | b'b' | b'f' | b'n' | b'r' | b't' | b'v'
                        ) {
                            offset += 2;
                            continue;
                        }
                        if escaped.is_ascii_digit() {
                            let escape = offset;
                            offset += 1;
                            let mut value = 0_u16;
                            let mut digits = 0;
                            while digits < 3 && bytes.get(offset).is_some_and(u8::is_ascii_digit) {
                                value = value * 10 + u16::from(bytes[offset] - b'0');
                                offset += 1;
                                digits += 1;
                            }
                            if value > u16::from(u8::MAX) {
                                malformed_escape.get_or_insert((escape, offset));
                            }
                            continue;
                        }
                        if escaped == b'x' {
                            let escape = offset;
                            offset += 2;
                            let digits = offset;
                            while offset < bytes.len()
                                && offset < digits + 2
                                && bytes[offset].is_ascii_hexdigit()
                            {
                                offset += 1;
                            }
                            if !supports_hex_byte_escape(explicit_profile) {
                                unsupported_escape.get_or_insert(escape);
                            } else if offset != digits + 2 {
                                malformed_escape.get_or_insert((escape, offset));
                            }
                            continue;
                        }
                        if escaped == b'z' && supports_whitespace_escape(explicit_profile) {
                            offset += 2;
                            while offset < bytes.len() && bytes[offset].is_ascii_whitespace() {
                                offset += 1;
                            }
                            continue;
                        }
                        if escaped == b'u' {
                            let escape = offset;
                            let mut cursor = offset + 2;
                            let mut value = 0_u32;
                            let mut digits = 0_usize;
                            let mut closed = false;
                            if bytes.get(cursor) == Some(&b'{') {
                                cursor += 1;
                                while bytes.get(cursor).is_some_and(u8::is_ascii_hexdigit) {
                                    value = value
                                        .saturating_mul(16)
                                        .saturating_add(u32::from(hex_value(bytes[cursor])));
                                    cursor += 1;
                                    digits += 1;
                                }
                                if bytes.get(cursor) == Some(&b'}') {
                                    cursor += 1;
                                    closed = true;
                                }
                            }
                            offset = cursor;
                            let Some(maximum) = unicode_escape_maximum(explicit_profile) else {
                                unsupported_escape.get_or_insert(escape);
                                continue;
                            };
                            if !closed || digits == 0 || digits > 8 || value > maximum {
                                malformed_unicode.get_or_insert((escape, offset));
                            }
                            continue;
                        }
                        unsupported_escape.get_or_insert(offset);
                    }
                    offset += 1;
                }
                if offset < bytes.len() && bytes[offset] == quote {
                    offset += 1;
                } else {
                    let span = source.span(start, offset)?;
                    let diagnostic = diagnostic(
                        "BLU-LEX-0008",
                        explicit_profile,
                        span,
                        "unterminated quoted string",
                        limits.diagnostic_limits,
                    )?
                    .try_with_found(&bytes[start..offset])?;
                    push_diagnostic(&mut diagnostics, diagnostic, limits.max_diagnostics)?;
                }
                if let Some(escape) = unsupported_escape {
                    let span = source.span(escape, escape + 1)?;
                    let diagnostic = diagnostic(
                        "BLU-LEX-0007",
                        explicit_profile,
                        span,
                        "unsupported string escape for this profile",
                        limits.diagnostic_limits,
                    )?
                    .try_with_found(&bytes[escape..escape + 1])?
                    .try_with_note_parts(&["selected profile: ", explicit_profile.as_str()])?;
                    push_diagnostic(&mut diagnostics, diagnostic, limits.max_diagnostics)?;
                }
                if let Some((escape, end)) = malformed_escape {
                    let span = source.span(escape, end)?;
                    let diagnostic = diagnostic(
                        "BLU-LEX-0017",
                        explicit_profile,
                        span,
                        "malformed byte escape in quoted string",
                        limits.diagnostic_limits,
                    )?
                    .try_with_found(&bytes[escape..end])?
                    .try_with_expected("a byte value from 0 through 255")?;
                    push_diagnostic(&mut diagnostics, diagnostic, limits.max_diagnostics)?;
                }
                if let Some((escape, end)) = malformed_unicode {
                    let span = source.span(escape, end)?;
                    let diagnostic = diagnostic(
                        "BLU-LEX-0018",
                        explicit_profile,
                        span,
                        "malformed Unicode escape for this profile",
                        limits.diagnostic_limits,
                    )?
                    .try_with_found(&bytes[escape..end])?
                    .try_with_note_parts(&["selected profile: ", explicit_profile.as_str()])?;
                    push_diagnostic(&mut diagnostics, diagnostic, limits.max_diagnostics)?;
                }
                TokenKind::StringLiteral
            }
            b'(' => {
                offset += 1;
                TokenKind::LeftParenthesis
            }
            b')' => {
                offset += 1;
                TokenKind::RightParenthesis
            }
            b'0'..=b'9' => {
                if bytes[start] == b'0'
                    && bytes
                        .get(start + 1)
                        .is_some_and(|byte| matches!(byte, b'b' | b'B'))
                {
                    offset += 2;
                    let mut has_digit = false;
                    while offset < bytes.len()
                        && (matches!(bytes[offset], b'0' | b'1') || bytes[offset] == b'_')
                    {
                        has_digit |= matches!(bytes[offset], b'0' | b'1');
                        offset += 1;
                    }
                    if !has_digit {
                        let span = source.span(start, offset)?;
                        let diagnostic = diagnostic(
                            "BLU-LEX-0013",
                            explicit_profile,
                            span,
                            "binary integer requires at least one digit",
                            limits.diagnostic_limits,
                        )?
                        .try_with_found(&bytes[start..offset])?
                        .try_with_expected("binary digit")?;
                        push_diagnostic(&mut diagnostics, diagnostic, limits.max_diagnostics)?;
                    }
                    if !supports_binary_integers(explicit_profile) {
                        let span = source.span(start, offset)?;
                        let diagnostic = diagnostic(
                            "BLU-LEX-0014",
                            explicit_profile,
                            span,
                            "binary integers are not supported by this profile",
                            limits.diagnostic_limits,
                        )?
                        .try_with_found(&bytes[start..offset])?
                        .try_with_note_parts(&["selected profile: ", explicit_profile.as_str()])?;
                        push_diagnostic(&mut diagnostics, diagnostic, limits.max_diagnostics)?;
                    }
                    TokenKind::BinaryInteger
                } else if bytes[start] == b'0'
                    && bytes
                        .get(start + 1)
                        .is_some_and(|byte| matches!(byte, b'x' | b'X'))
                {
                    offset += 2;
                    let mut has_digit = false;
                    let mut has_separator = false;
                    while offset < bytes.len()
                        && (bytes[offset].is_ascii_hexdigit() || bytes[offset] == b'_')
                    {
                        has_digit |= bytes[offset].is_ascii_hexdigit();
                        has_separator |= bytes[offset] == b'_';
                        offset += 1;
                    }
                    let has_dot =
                        bytes.get(offset) == Some(&b'.') && bytes.get(offset + 1) != Some(&b'.');
                    if has_dot {
                        offset += 1;
                        while offset < bytes.len()
                            && (bytes[offset].is_ascii_hexdigit() || bytes[offset] == b'_')
                        {
                            has_digit |= bytes[offset].is_ascii_hexdigit();
                            has_separator |= bytes[offset] == b'_';
                            offset += 1;
                        }
                    }
                    let mut has_exponent = false;
                    let mut malformed_exponent = None;
                    if matches!(bytes.get(offset), Some(b'p' | b'P')) {
                        has_exponent = true;
                        let exponent = offset;
                        offset += 1;
                        if matches!(bytes.get(offset), Some(b'+' | b'-')) {
                            offset += 1;
                        }
                        let mut has_exponent_digit = false;
                        while offset < bytes.len()
                            && (bytes[offset].is_ascii_digit() || bytes[offset] == b'_')
                        {
                            has_exponent_digit |= bytes[offset].is_ascii_digit();
                            has_separator |= bytes[offset] == b'_';
                            offset += 1;
                        }
                        if !has_exponent_digit {
                            malformed_exponent = Some(exponent);
                        }
                    }
                    let is_number = has_dot || has_exponent;
                    if !has_digit {
                        let span = source.span(start, offset)?;
                        let diagnostic = diagnostic(
                            "BLU-LEX-0011",
                            explicit_profile,
                            span,
                            "hexadecimal integer requires at least one digit",
                            limits.diagnostic_limits,
                        )?
                        .try_with_found(&bytes[start..offset])?
                        .try_with_expected("hexadecimal digit")?;
                        push_diagnostic(&mut diagnostics, diagnostic, limits.max_diagnostics)?;
                    }
                    if let Some(exponent) = malformed_exponent {
                        let span = source.span(exponent, offset)?;
                        let diagnostic = diagnostic(
                            "BLU-LEX-0015",
                            explicit_profile,
                            span,
                            "hexadecimal exponent requires at least one decimal digit",
                            limits.diagnostic_limits,
                        )?
                        .try_with_found(&bytes[exponent..offset])?
                        .try_with_expected("decimal exponent digit")?;
                        push_diagnostic(&mut diagnostics, diagnostic, limits.max_diagnostics)?;
                    }
                    if is_number && !supports_hex_number(explicit_profile, has_dot, has_exponent) {
                        let span = source.span(start, offset)?;
                        let diagnostic = diagnostic(
                            "BLU-LEX-0016",
                            explicit_profile,
                            span,
                            "this hexadecimal number form is not supported by the selected profile",
                            limits.diagnostic_limits,
                        )?
                        .try_with_found(&bytes[start..offset])?
                        .try_with_note_parts(&["selected profile: ", explicit_profile.as_str()])?;
                        push_diagnostic(&mut diagnostics, diagnostic, limits.max_diagnostics)?;
                    }
                    if has_separator && !supports_numeric_separators(explicit_profile) {
                        push_numeric_separator_diagnostic(
                            source,
                            explicit_profile,
                            start,
                            offset,
                            &mut diagnostics,
                            limits,
                        )?;
                    }
                    if is_number {
                        TokenKind::HexNumber
                    } else {
                        TokenKind::HexInteger
                    }
                } else {
                    let scan = scan_decimal(bytes, start);
                    offset = scan.end;
                    if let Some(exponent) = scan.malformed_exponent {
                        let span = source.span(exponent, offset)?;
                        let diagnostic = diagnostic(
                            "BLU-LEX-0009",
                            explicit_profile,
                            span,
                            "decimal exponent requires at least one digit",
                            limits.diagnostic_limits,
                        )?
                        .try_with_found(&bytes[exponent..offset])?
                        .try_with_expected("decimal exponent digit")?;
                        push_diagnostic(&mut diagnostics, diagnostic, limits.max_diagnostics)?;
                    }
                    if scan.has_separator && !supports_numeric_separators(explicit_profile) {
                        push_numeric_separator_diagnostic(
                            source,
                            explicit_profile,
                            start,
                            offset,
                            &mut diagnostics,
                            limits,
                        )?;
                    }
                    if scan.is_integer {
                        TokenKind::DecimalInteger
                    } else {
                        TokenKind::DecimalNumber
                    }
                }
            }
            b'.' if bytes.get(offset + 1).is_some_and(u8::is_ascii_digit) => {
                let scan = scan_decimal(bytes, start);
                offset = scan.end;
                if let Some(exponent) = scan.malformed_exponent {
                    let span = source.span(exponent, offset)?;
                    let diagnostic = diagnostic(
                        "BLU-LEX-0009",
                        explicit_profile,
                        span,
                        "decimal exponent requires at least one digit",
                        limits.diagnostic_limits,
                    )?
                    .try_with_found(&bytes[exponent..offset])?
                    .try_with_expected("decimal exponent digit")?;
                    push_diagnostic(&mut diagnostics, diagnostic, limits.max_diagnostics)?;
                }
                if scan.has_separator && !supports_numeric_separators(explicit_profile) {
                    push_numeric_separator_diagnostic(
                        source,
                        explicit_profile,
                        start,
                        offset,
                        &mut diagnostics,
                        limits,
                    )?;
                }
                TokenKind::DecimalNumber
            }
            b'.' => {
                offset += 1;
                TokenKind::Dot
            }
            byte if is_identifier_start(byte) => {
                offset += 1;
                while offset < bytes.len() && is_identifier_continue(bytes[offset]) {
                    offset += 1;
                }
                match &bytes[start..offset] {
                    b"local" => TokenKind::Local,
                    b"function" => TokenKind::Function,
                    b"return" => TokenKind::Return,
                    b"not" => TokenKind::Not,
                    b"and" => TokenKind::And,
                    b"or" => TokenKind::Or,
                    b"if" => TokenKind::If,
                    b"then" => TokenKind::Then,
                    b"elseif" => TokenKind::ElseIf,
                    b"else" => TokenKind::Else,
                    b"end" => TokenKind::End,
                    b"while" => TokenKind::While,
                    b"do" => TokenKind::Do,
                    b"repeat" => TokenKind::Repeat,
                    b"until" => TokenKind::Until,
                    b"for" => TokenKind::For,
                    b"in" => TokenKind::In,
                    b"break" => TokenKind::Break,
                    b"continue" => {
                        if !supports_continue(explicit_profile) {
                            let span = source.span(start, offset)?;
                            let diagnostic = diagnostic(
                                "BLU-LEX-0018",
                                explicit_profile,
                                span,
                                "`continue` is unavailable in this profile",
                                limits.diagnostic_limits,
                            )?
                            .try_with_found(&bytes[start..offset])?
                            .try_with_note_parts(&[
                                "selected profile: ",
                                explicit_profile.as_str(),
                            ])?;
                            push_diagnostic(&mut diagnostics, diagnostic, limits.max_diagnostics)?;
                        }
                        TokenKind::Continue
                    }
                    b"nil" => TokenKind::Nil,
                    b"true" => TokenKind::True,
                    b"false" => TokenKind::False,
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
                    limits.diagnostic_limits,
                )?
                .try_with_found(&bytes[start..offset])?;
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

#[derive(Clone, Copy)]
struct DecimalScan {
    end: usize,
    is_integer: bool,
    malformed_exponent: Option<usize>,
    has_separator: bool,
}

fn scan_decimal(bytes: &[u8], start: usize) -> DecimalScan {
    let mut offset = start;
    let mut is_integer = true;
    let mut has_separator = false;
    let leading_dot = bytes[offset] == b'.';
    if leading_dot {
        is_integer = false;
        offset += 1;
    }
    while offset < bytes.len() && (bytes[offset].is_ascii_digit() || bytes[offset] == b'_') {
        has_separator |= bytes[offset] == b'_';
        offset += 1;
    }
    if !leading_dot && bytes.get(offset) == Some(&b'.') {
        if bytes.get(offset + 1) == Some(&b'.') {
            return DecimalScan {
                end: offset,
                is_integer,
                malformed_exponent: None,
                has_separator,
            };
        } else {
            is_integer = false;
            offset += 1;
        }
        while offset < bytes.len() && (bytes[offset].is_ascii_digit() || bytes[offset] == b'_') {
            has_separator |= bytes[offset] == b'_';
            offset += 1;
        }
    }
    let mut malformed_exponent = None;
    if matches!(bytes.get(offset), Some(b'e' | b'E')) {
        is_integer = false;
        let exponent = offset;
        offset += 1;
        if matches!(bytes.get(offset), Some(b'+' | b'-')) {
            offset += 1;
        }
        let mut has_exponent_digit = false;
        while offset < bytes.len() && (bytes[offset].is_ascii_digit() || bytes[offset] == b'_') {
            has_exponent_digit |= bytes[offset].is_ascii_digit();
            has_separator |= bytes[offset] == b'_';
            offset += 1;
        }
        if !has_exponent_digit {
            malformed_exponent = Some(exponent);
        }
    }
    DecimalScan {
        end: offset,
        is_integer,
        malformed_exponent,
        has_separator,
    }
}

fn push_numeric_separator_diagnostic(
    source: &SourceFile,
    profile: SemanticProfile,
    start: usize,
    end: usize,
    diagnostics: &mut Vec<Diagnostic>,
    limits: LexerLimits,
) -> Result<(), LexError> {
    let span = source.span(start, end)?;
    let diagnostic = diagnostic(
        "BLU-LEX-0012",
        profile,
        span,
        "numeric separators are not supported by this profile",
        limits.diagnostic_limits,
    )?
    .try_with_found(source.slice(span)?)?
    .try_with_note_parts(&["selected profile: ", profile.as_str()])?;
    push_diagnostic(diagnostics, diagnostic, limits.max_diagnostics)
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
    diagnostic_limits: DiagnosticLimits,
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
            diagnostic_limits,
        )?)?;
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
            diagnostic_limits,
        )?
        .try_with_found(&bytes[trailing..content_end])?
        .try_with_expected("end of directive")?;
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
                diagnostic_limits,
            )?
            .try_with_found(&bytes[value_start..value_end])?,
        )?;
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
            diagnostic_limits,
        )?
        .try_with_found(&bytes[value_start..value_end])?
        .try_with_note_parts(&["explicit profile: ", explicit_profile.as_str()])?
        .try_with_expected(explicit_profile.as_str())?;
        push_diagnostic(diagnostics, diagnostic, diagnostic_limit)?;
    }
    Ok(Some(directive))
}

fn add_profile_expectations(mut diagnostic: Diagnostic) -> Result<Diagnostic, LexError> {
    for profile in SemanticProfile::ALL {
        diagnostic = diagnostic.try_with_expected(profile.as_str())?;
    }
    Ok(diagnostic)
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

const fn supports_continue(profile: SemanticProfile) -> bool {
    matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau)
}

const fn supports_numeric_separators(profile: SemanticProfile) -> bool {
    matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau)
}

const fn supports_binary_integers(profile: SemanticProfile) -> bool {
    matches!(profile, SemanticProfile::Blu | SemanticProfile::Luau)
}

const fn supports_hex_number(profile: SemanticProfile, has_dot: bool, has_exponent: bool) -> bool {
    match profile {
        SemanticProfile::Blu
        | SemanticProfile::Lua52
        | SemanticProfile::Lua53
        | SemanticProfile::Lua54
        | SemanticProfile::Lua55 => true,
        SemanticProfile::Lua51 => has_exponent && !has_dot,
        SemanticProfile::Luau => false,
        _ => false,
    }
}

const fn supports_hex_byte_escape(profile: SemanticProfile) -> bool {
    match profile {
        SemanticProfile::Blu
        | SemanticProfile::Luau
        | SemanticProfile::Lua52
        | SemanticProfile::Lua53
        | SemanticProfile::Lua54
        | SemanticProfile::Lua55 => true,
        SemanticProfile::Lua51 => false,
        _ => false,
    }
}

const fn supports_whitespace_escape(profile: SemanticProfile) -> bool {
    supports_hex_byte_escape(profile)
}

const fn unicode_escape_maximum(profile: SemanticProfile) -> Option<u32> {
    match profile {
        SemanticProfile::Blu | SemanticProfile::Lua54 | SemanticProfile::Lua55 => Some(0x7fff_ffff),
        SemanticProfile::Luau | SemanticProfile::Lua53 => Some(0x10_ffff),
        SemanticProfile::Lua51 | SemanticProfile::Lua52 => None,
        _ => None,
    }
}

const fn hex_value(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => 0,
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
    limits: DiagnosticLimits,
) -> Result<Diagnostic, LexError> {
    Diagnostic::try_new(
        code,
        Phase::Lex,
        profile,
        Severity::Error,
        span,
        message,
        limits,
    )
    .map_err(LexError::Diagnostic)
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
