use crate::{
    Ast, BinaryExpression, BinaryOperator, DialectDirective, Expression, ExpressionId,
    ExpressionKind, Identifier, LexError, Lexed, LexerLimits, LocalStatement, ReturnStatement,
    Statement, Token, TokenKind, UnaryExpression, UnaryOperator, lex,
};
use blu_core::{
    ByteSpan, Diagnostic, DiagnosticError, DiagnosticLimits, Phase, SemanticProfile, Severity,
    SourceFile, SpanError,
};
use core::fmt;

/// Resource limits for one parser invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseLimits {
    pub lexer: LexerLimits,
    pub max_ast_nodes: usize,
    pub max_expression_depth: usize,
    /// Maximum number of lexical or parser diagnostics.
    pub max_diagnostics: usize,
}

impl Default for ParseLimits {
    fn default() -> Self {
        Self {
            lexer: LexerLimits::default(),
            max_ast_nodes: 1_000_000,
            max_expression_depth: 256,
            max_diagnostics: 10_000,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseLimit {
    AstNodes,
    ExpressionDepth,
    Diagnostics,
}

impl fmt::Display for ParseLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AstNodes => formatter.write_str("AST nodes"),
            Self::ExpressionDepth => formatter.write_str("expression depth"),
            Self::Diagnostics => formatter.write_str("parser diagnostics"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseError {
    Lex(LexError),
    Limit {
        kind: ParseLimit,
        required: usize,
        limit: usize,
    },
    AllocationFailed {
        what: &'static str,
        requested: usize,
    },
    Diagnostic(DiagnosticError),
    InternalInvariant {
        message: &'static str,
    },
    Span(SpanError),
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lex(error) => error.fmt(formatter),
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
            Self::InternalInvariant { message } => {
                write!(formatter, "parser internal invariant failed: {message}")
            }
            Self::Span(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Lex(error) => Some(error),
            Self::Diagnostic(error) => Some(error),
            Self::Span(error) => Some(error),
            Self::Limit { .. } | Self::AllocationFailed { .. } | Self::InternalInvariant { .. } => {
                None
            }
        }
    }
}

impl From<LexError> for ParseError {
    fn from(error: LexError) -> Self {
        Self::Lex(error)
    }
}

impl From<SpanError> for ParseError {
    fn from(error: SpanError) -> Self {
        Self::Span(error)
    }
}

impl From<DiagnosticError> for ParseError {
    fn from(error: DiagnosticError) -> Self {
        Self::Diagnostic(error)
    }
}

/// A successfully parsed, still trivia-preserving source.
#[derive(Debug, Eq, PartialEq)]
pub struct Parsed {
    lexed: Lexed,
    ast: Ast,
}

impl Parsed {
    #[must_use]
    pub const fn profile(&self) -> SemanticProfile {
        self.ast.profile()
    }

    #[must_use]
    pub const fn directive(&self) -> Option<DialectDirective> {
        self.lexed.directive()
    }

    #[must_use]
    pub fn tokens(&self) -> &[Token] {
        self.lexed.tokens()
    }

    #[must_use]
    pub const fn ast(&self) -> &Ast {
        &self.ast
    }

    #[must_use]
    pub const fn lexed(&self) -> &Lexed {
        &self.lexed
    }
}

/// A structurally rejected source. No partial AST is exposed.
#[derive(Debug, Eq, PartialEq)]
pub struct Rejected {
    lexed: Lexed,
    diagnostics: Vec<Diagnostic>,
}

impl Rejected {
    #[must_use]
    pub const fn profile(&self) -> SemanticProfile {
        self.lexed.profile()
    }

    #[must_use]
    pub const fn directive(&self) -> Option<DialectDirective> {
        self.lexed.directive()
    }

    #[must_use]
    pub fn tokens(&self) -> &[Token] {
        self.lexed.tokens()
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        if self.diagnostics.is_empty() {
            self.lexed.diagnostics()
        } else {
            &self.diagnostics
        }
    }

    #[must_use]
    pub const fn lexed(&self) -> &Lexed {
        &self.lexed
    }
}

/// Parsing either yields a complete AST or a diagnostic-only rejection.
#[derive(Debug, Eq, PartialEq)]
pub enum ParseOutcome {
    Accepted(Parsed),
    Rejected(Rejected),
}

impl ParseOutcome {
    #[must_use]
    pub const fn profile(&self) -> SemanticProfile {
        match self {
            Self::Accepted(parsed) => parsed.profile(),
            Self::Rejected(rejected) => rejected.profile(),
        }
    }

    #[must_use]
    pub const fn accepted(&self) -> Option<&Parsed> {
        match self {
            Self::Accepted(parsed) => Some(parsed),
            Self::Rejected(_) => None,
        }
    }

    #[must_use]
    pub const fn rejected(&self) -> Option<&Rejected> {
        match self {
            Self::Accepted(_) => None,
            Self::Rejected(rejected) => Some(rejected),
        }
    }
}

/// Parses the currently supported grammar under an explicit profile.
///
/// The grammar is limited to `local name = expression`, a final
/// `return expression (, expression)*`, nil, booleans, escape-free quoted byte
/// strings, decimal-integer and identifier
/// expressions, grouping parentheses, and left-associative `+` and `//`
/// (`//` binds more tightly).
/// Trivia remains available through [`Parsed::tokens`] or
/// [`Rejected::tokens`]. Lexical diagnostics, including profile gates, reject
/// before parsing. This function does not resolve names or integer values.
pub fn parse(
    source: &SourceFile,
    explicit_profile: SemanticProfile,
    limits: ParseLimits,
) -> Result<ParseOutcome, ParseError> {
    let lexer_limits = LexerLimits {
        max_diagnostics: limits.lexer.max_diagnostics.min(limits.max_diagnostics),
        ..limits.lexer
    };
    let lexed = lex(source, explicit_profile, lexer_limits)?;
    if !lexed.diagnostics.is_empty() {
        return Ok(ParseOutcome::Rejected(Rejected {
            lexed,
            diagnostics: Vec::new(),
        }));
    }

    let (ast, diagnostics) = Parser::new(source, &lexed, limits)?.run()?;
    if diagnostics.is_empty() {
        let Some(ast) = ast else {
            return Err(ParseError::InternalInvariant {
                message: "diagnostic-free parser run produced no AST",
            });
        };
        Ok(ParseOutcome::Accepted(Parsed { lexed, ast }))
    } else {
        Ok(ParseOutcome::Rejected(Rejected { lexed, diagnostics }))
    }
}

#[derive(Clone, Copy)]
struct BuiltExpression {
    id: ExpressionId,
    depth: usize,
}

struct Parser<'a> {
    source: &'a SourceFile,
    lexed: &'a Lexed,
    limits: ParseLimits,
    cursor: usize,
    statements: Vec<Statement>,
    expressions: Vec<Expression>,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Parser<'a> {
    fn new(
        source: &'a SourceFile,
        lexed: &'a Lexed,
        limits: ParseLimits,
    ) -> Result<Self, ParseError> {
        let ast_capacity = lexed.tokens().len().min(limits.max_ast_nodes).min(4_096);
        let diagnostic_capacity = lexed.tokens().len().min(limits.max_diagnostics).min(16);
        Ok(Self {
            source,
            lexed,
            limits,
            cursor: 0,
            statements: allocate_vec(ast_capacity, "AST statements")?,
            expressions: allocate_vec(ast_capacity, "AST expressions")?,
            diagnostics: allocate_vec(diagnostic_capacity, "parser diagnostics")?,
        })
    }

    fn run(mut self) -> Result<(Option<Ast>, Vec<Diagnostic>), ParseError> {
        while let Some(token) = self.current() {
            match token.kind() {
                TokenKind::Local => self.parse_local()?,
                TokenKind::Return => {
                    self.parse_return()?;
                    if self.current().is_some() {
                        self.report_current(
                            "BLU-PARSE-0005",
                            "unexpected token after return statement",
                            &["end of source"],
                        )?;
                        while self.bump().is_some() {}
                    }
                }
                _ => {
                    self.report_current(
                        "BLU-PARSE-0001",
                        "expected a supported statement",
                        &["local", "return"],
                    )?;
                    self.bump();
                }
            }
        }

        if !self.diagnostics.is_empty() {
            return Ok((None, self.diagnostics));
        }
        let span = match (self.statements.first(), self.statements.last()) {
            (Some(first), Some(last)) => first.span().merge(last.span())?,
            _ => self.source.span(0, 0)?,
        };
        let ast = Ast::new(
            self.lexed.profile(),
            span,
            self.statements,
            self.expressions,
        );
        Ok((Some(ast), self.diagnostics))
    }

    fn parse_local(&mut self) -> Result<(), ParseError> {
        let Some(keyword) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "local parser entered without a current token",
            });
        };
        let name = if self.at(TokenKind::Identifier) {
            let Some(identifier) = self.bump() else {
                return Err(ParseError::InternalInvariant {
                    message: "identifier check succeeded without a current token",
                });
            };
            Some(Identifier::new(identifier.span()))
        } else {
            self.report_current_or_eof("BLU-PARSE-0002", "expected a local name", &["identifier"])?;
            None
        };

        let has_equal = if self.at(TokenKind::Equal) {
            self.bump();
            true
        } else {
            self.report_current_or_eof("BLU-PARSE-0003", "expected `=` after local name", &["="])?;
            false
        };
        let value = self.parse_expression(0)?;

        if let (Some(name), true, Some(value)) = (name, has_equal, value) {
            let span = keyword.span().merge(self.expression(value.id)?.span())?;
            self.push_statement(Statement::Local(LocalStatement::new(name, value.id, span)))?;
        }
        Ok(())
    }

    fn parse_return(&mut self) -> Result<(), ParseError> {
        let Some(keyword) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "return parser entered without a current token",
            });
        };
        let mut values = allocate_vec(1, "return expression list")?;
        if self.current().is_none() {
            return self.push_statement(Statement::Return(ReturnStatement::new(
                values,
                keyword.span(),
            )));
        }
        let Some(first) = self.parse_expression(0)? else {
            return Ok(());
        };
        push_fallible(&mut values, first.id, "return expression list")?;
        let mut end = self.expression(first.id)?.span();

        while self.at(TokenKind::Comma) {
            self.bump();
            let Some(value) = self.parse_expression(0)? else {
                break;
            };
            end = self.expression(value.id)?.span();
            push_fallible(&mut values, value.id, "return expression list")?;
        }

        let span = keyword.span().merge(end)?;
        self.push_statement(Statement::Return(ReturnStatement::new(values, span)))
    }

    fn parse_expression(
        &mut self,
        minimum_precedence: u8,
    ) -> Result<Option<BuiltExpression>, ParseError> {
        let Some(mut left) = self.parse_prefix()? else {
            return Ok(None);
        };

        while let Some(operator_token) = self.current() {
            let Some((operator, precedence)) = binary_operator(operator_token.kind()) else {
                break;
            };
            if precedence < minimum_precedence {
                break;
            }
            self.bump();
            let Some(right) = self.parse_expression(precedence.saturating_add(1))? else {
                break;
            };
            let depth = left.depth.max(right.depth).saturating_add(1);
            let span = self
                .expression(left.id)?
                .span()
                .merge(self.expression(right.id)?.span())?;
            left = self.push_expression(
                Expression::new(
                    ExpressionKind::Binary(BinaryExpression::new(
                        left.id,
                        operator,
                        operator_token.span(),
                        right.id,
                    )),
                    span,
                ),
                depth,
            )?;
        }
        Ok(Some(left))
    }

    fn parse_prefix(&mut self) -> Result<Option<BuiltExpression>, ParseError> {
        let Some(operator) = self
            .current()
            .filter(|token| token.kind() == TokenKind::Not)
        else {
            return self.parse_primary();
        };
        self.bump();
        let Some(operand) = self.parse_expression(3)? else {
            return Ok(None);
        };
        let span = operator.span().merge(self.expression(operand.id)?.span())?;
        self.push_expression(
            Expression::new(
                ExpressionKind::Unary(UnaryExpression::new(
                    UnaryOperator::Not,
                    operator.span(),
                    operand.id,
                )),
                span,
            ),
            operand.depth.saturating_add(1),
        )
        .map(Some)
    }

    fn parse_primary(&mut self) -> Result<Option<BuiltExpression>, ParseError> {
        let Some(token) = self.current() else {
            self.report_current_or_eof(
                "BLU-PARSE-0004",
                "expected an expression",
                &[
                    "nil",
                    "boolean",
                    "quoted string",
                    "decimal integer",
                    "identifier",
                    "not",
                    "(",
                ],
            )?;
            return Ok(None);
        };
        let kind = match token.kind() {
            TokenKind::Nil => ExpressionKind::Nil,
            TokenKind::True => ExpressionKind::Boolean(true),
            TokenKind::False => ExpressionKind::Boolean(false),
            TokenKind::DecimalInteger => ExpressionKind::DecimalInteger,
            TokenKind::StringLiteral => ExpressionKind::StringLiteral,
            TokenKind::Identifier => ExpressionKind::Identifier(Identifier::new(token.span())),
            TokenKind::LeftParenthesis => {
                self.bump();
                let Some(inner) = self.parse_expression(0)? else {
                    return Ok(None);
                };
                let Some(close) = self
                    .current()
                    .filter(|token| token.kind() == TokenKind::RightParenthesis)
                else {
                    self.report_current_or_eof(
                        "BLU-PARSE-0006",
                        "expected `)` after grouped expression",
                        &[")"],
                    )?;
                    return Ok(Some(inner));
                };
                self.bump();
                let span = token.span().merge(close.span())?;
                return self
                    .push_expression(
                        Expression::new(ExpressionKind::Group(inner.id), span),
                        inner.depth.saturating_add(1),
                    )
                    .map(Some);
            }
            _ => {
                self.report_current(
                    "BLU-PARSE-0004",
                    "expected an expression",
                    &[
                        "nil",
                        "boolean",
                        "quoted string",
                        "decimal integer",
                        "identifier",
                        "not",
                        "(",
                    ],
                )?;
                return Ok(None);
            }
        };
        self.bump();
        self.push_expression(Expression::new(kind, token.span()), 1)
            .map(Some)
    }

    fn push_expression(
        &mut self,
        expression: Expression,
        depth: usize,
    ) -> Result<BuiltExpression, ParseError> {
        if depth > self.limits.max_expression_depth {
            return Err(ParseError::Limit {
                kind: ParseLimit::ExpressionDepth,
                required: depth,
                limit: self.limits.max_expression_depth,
            });
        }
        self.check_ast_limit()?;
        let id = ExpressionId::new(self.expressions.len());
        push_fallible(&mut self.expressions, expression, "AST expressions")?;
        Ok(BuiltExpression { id, depth })
    }

    fn push_statement(&mut self, statement: Statement) -> Result<(), ParseError> {
        self.check_ast_limit()?;
        push_fallible(&mut self.statements, statement, "AST statements")
    }

    fn check_ast_limit(&self) -> Result<(), ParseError> {
        let required = self
            .statements
            .len()
            .saturating_add(self.expressions.len())
            .saturating_add(1);
        if required > self.limits.max_ast_nodes {
            Err(ParseError::Limit {
                kind: ParseLimit::AstNodes,
                required,
                limit: self.limits.max_ast_nodes,
            })
        } else {
            Ok(())
        }
    }

    fn expression(&self, id: ExpressionId) -> Result<Expression, ParseError> {
        self.expressions
            .get(id.as_usize())
            .copied()
            .ok_or(ParseError::InternalInvariant {
                message: "expression arena index is out of bounds",
            })
    }

    fn current(&self) -> Option<Token> {
        self.lexed
            .tokens()
            .get(self.cursor..)?
            .iter()
            .copied()
            .find(|token| !is_trivia(token.kind()))
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.current().is_some_and(|token| token.kind() == kind)
    }

    fn bump(&mut self) -> Option<Token> {
        while let Some(token) = self.lexed.tokens().get(self.cursor).copied() {
            self.cursor += 1;
            if !is_trivia(token.kind()) {
                return Some(token);
            }
        }
        None
    }

    fn report_current(
        &mut self,
        code: &'static str,
        message: &'static str,
        expected: &[&'static str],
    ) -> Result<(), ParseError> {
        let Some(token) = self.current() else {
            return Err(ParseError::InternalInvariant {
                message: "current-token diagnostic requested at end of source",
            });
        };
        let found = self.source.slice(token.span())?;
        let diagnostic = parser_diagnostic(
            code,
            self.lexed.profile(),
            token.span(),
            message,
            expected,
            Some(found),
            self.limits.lexer.diagnostic_limits,
        )?;
        self.push_diagnostic(diagnostic)
    }

    fn report_current_or_eof(
        &mut self,
        code: &'static str,
        message: &'static str,
        expected: &[&'static str],
    ) -> Result<(), ParseError> {
        if self.current().is_some() {
            self.report_current(code, message, expected)
        } else {
            let span = self.source.span(self.source.len(), self.source.len())?;
            let diagnostic = parser_diagnostic(
                code,
                self.lexed.profile(),
                span,
                message,
                expected,
                None,
                self.limits.lexer.diagnostic_limits,
            )?;
            self.push_diagnostic(diagnostic)
        }
    }

    fn push_diagnostic(&mut self, diagnostic: Diagnostic) -> Result<(), ParseError> {
        let required = self.diagnostics.len().saturating_add(1);
        if required > self.limits.max_diagnostics {
            return Err(ParseError::Limit {
                kind: ParseLimit::Diagnostics,
                required,
                limit: self.limits.max_diagnostics,
            });
        }
        push_fallible(&mut self.diagnostics, diagnostic, "parser diagnostics")
    }
}

fn binary_operator(kind: TokenKind) -> Option<(BinaryOperator, u8)> {
    match kind {
        TokenKind::Plus => Some((BinaryOperator::Add, 1)),
        TokenKind::FloorDivide => Some((BinaryOperator::FloorDivide, 2)),
        _ => None,
    }
}

fn is_trivia(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Whitespace | TokenKind::Comment | TokenKind::DialectDirective
    )
}

fn parser_diagnostic(
    code: &'static str,
    profile: SemanticProfile,
    span: ByteSpan,
    message: &'static str,
    expected: &[&'static str],
    found: Option<&[u8]>,
    limits: DiagnosticLimits,
) -> Result<Diagnostic, ParseError> {
    let mut diagnostic = Diagnostic::try_new(
        code,
        Phase::Parse,
        profile,
        Severity::Error,
        span,
        message,
        limits,
    )?;
    for value in expected {
        diagnostic = diagnostic.try_with_expected(value)?;
    }
    if let Some(found) = found {
        diagnostic = diagnostic.try_with_found(found)?;
    }
    Ok(diagnostic)
}

fn allocate_vec<T>(capacity: usize, what: &'static str) -> Result<Vec<T>, ParseError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| ParseError::AllocationFailed {
            what,
            requested: capacity,
        })?;
    Ok(values)
}

fn push_fallible<T>(values: &mut Vec<T>, value: T, what: &'static str) -> Result<(), ParseError> {
    if values.len() == values.capacity() {
        let requested = values.len().saturating_add(1);
        values
            .try_reserve(1)
            .map_err(|_| ParseError::AllocationFailed { what, requested })?;
    }
    values.push(value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ParseError, allocate_vec};

    #[test]
    fn parser_allocation_failure_is_structured() {
        assert_eq!(
            allocate_vec::<u8>(usize::MAX, "test entries"),
            Err(ParseError::AllocationFailed {
                what: "test entries",
                requested: usize::MAX,
            })
        );
    }
}
