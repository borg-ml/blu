use crate::{
    AssignmentListStatement, AssignmentStatement, AssignmentTarget, Ast, BinaryExpression,
    BinaryOperator, Block, BreakStatement, CallExpression, CallStatement,
    CompoundAssignmentOperator, CompoundAssignmentStatement, ContinueStatement, DialectDirective,
    DoStatement, Expression, ExpressionId, ExpressionKind, FieldExpression, FunctionBody,
    FunctionExpression, FunctionId, FunctionStatement, GenericForStatement, GlobalStatement,
    GotoStatement, Identifier, IfClause, IfExpression, IfStatement, IndexExpression,
    InterpolatedString, InterpolatedStringPart, LabelStatement, LexError, Lexed, LexerLimits,
    LocalAttribute, LocalFunctionStatement, LocalListStatement, LocalStatement,
    MethodCallExpression, NumericForStatement, RepeatStatement, ReturnStatement, Statement,
    TableConstructor, TableField, Token, TokenKind, UnaryExpression, UnaryOperator, WhileStatement,
    lex, supports_global_declarations, supports_local_attributes, supports_named_vararg,
    supports_type_annotations,
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
    statement_count: usize,
    block_depth: usize,
    loop_depth: usize,
    expressions: Vec<Expression>,
    table_fields: Vec<TableField>,
    interpolated_parts: Vec<InterpolatedStringPart>,
    table_field_count: usize,
    call_arguments: Vec<ExpressionId>,
    call_argument_count: usize,
    functions: Vec<FunctionBody>,
    function_node_count: usize,
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
            statement_count: 0,
            block_depth: 0,
            loop_depth: 0,
            expressions: allocate_vec(ast_capacity, "AST expressions")?,
            table_fields: allocate_vec(ast_capacity, "AST table fields")?,
            interpolated_parts: allocate_vec(ast_capacity, "AST interpolated string parts")?,
            table_field_count: 0,
            call_arguments: allocate_vec(ast_capacity, "AST call arguments")?,
            call_argument_count: 0,
            functions: allocate_vec(ast_capacity.min(64), "AST functions")?,
            function_node_count: 0,
            diagnostics: allocate_vec(diagnostic_capacity, "parser diagnostics")?,
        })
    }

    fn run(mut self) -> Result<(Option<Ast>, Vec<Diagnostic>), ParseError> {
        self.parse_statements(&[])?;

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
            self.table_fields,
            self.interpolated_parts,
            self.call_arguments,
            self.functions,
        );
        Ok((Some(ast), self.diagnostics))
    }

    fn parse_statements(&mut self, terminators: &[TokenKind]) -> Result<(), ParseError> {
        while let Some(token) = self.current() {
            if terminators.contains(&token.kind()) {
                break;
            }
            match token.kind() {
                TokenKind::Semicolon => {
                    if self.lexed.profile() == SemanticProfile::Luau
                        && self
                            .previous_significant_kind()
                            .is_none_or(|kind| kind == TokenKind::Semicolon)
                    {
                        self.report_current(
                            "BLU-PARSE-0056",
                            "empty statements are unavailable in the Luau profile",
                            &["statement"],
                        )?;
                    }
                    self.bump();
                }
                TokenKind::Global
                    if self.lexed.profile() == SemanticProfile::Lua55
                        && !matches!(
                            self.significant_kind_after_cursor(1),
                            Some(
                                TokenKind::Function
                                    | TokenKind::LessThan
                                    | TokenKind::Star
                                    | TokenKind::Identifier
                            )
                        ) =>
                {
                    self.parse_expression_statement()?
                }
                TokenKind::Global => self.parse_global()?,
                TokenKind::Local => self.parse_local()?,
                TokenKind::Function => self.parse_function_statement()?,
                TokenKind::Identifier => self.parse_assignment()?,
                TokenKind::Nil
                | TokenKind::True
                | TokenKind::False
                | TokenKind::DecimalInteger
                | TokenKind::DecimalNumber
                | TokenKind::HexInteger
                | TokenKind::HexNumber
                | TokenKind::BinaryInteger
                | TokenKind::StringLiteral
                | TokenKind::Not
                | TokenKind::Minus
                | TokenKind::BitwiseExclusiveOr
                | TokenKind::Hash
                | TokenKind::LeftParenthesis
                | TokenKind::InterpolatedStringStart
                | TokenKind::LeftBrace => self.parse_expression_statement()?,
                TokenKind::If => self.parse_if()?,
                TokenKind::While => self.parse_while()?,
                TokenKind::Repeat => self.parse_repeat()?,
                TokenKind::Do => self.parse_do()?,
                TokenKind::For => self.parse_for()?,
                TokenKind::ColonColon => self.parse_label()?,
                TokenKind::Goto => self.parse_goto()?,
                TokenKind::Break | TokenKind::Continue => {
                    let Some(keyword) = self.bump() else {
                        return Err(ParseError::InternalInvariant {
                            message: "loop-control check succeeded without a current token",
                        });
                    };
                    if self.loop_depth == 0 {
                        let diagnostic = parser_diagnostic(
                            "BLU-PARSE-0022",
                            self.lexed.profile(),
                            keyword.span(),
                            "loop control is only valid inside a loop",
                            &["loop body"],
                            Some(self.source.slice(keyword.span())?),
                            self.limits.lexer.diagnostic_limits,
                        )?;
                        self.push_diagnostic(diagnostic)?;
                    } else if keyword.kind() == TokenKind::Break {
                        self.push_statement(Statement::Break(BreakStatement::new(keyword.span())))?;
                    } else {
                        self.push_statement(Statement::Continue(ContinueStatement::new(
                            keyword.span(),
                        )))?;
                    }
                    if self.at(TokenKind::Semicolon) {
                        self.bump();
                    }
                    if self.current().is_some_and(|token| {
                        !terminators.contains(&token.kind()) && token.kind() != TokenKind::Semicolon
                    }) {
                        self.report_current(
                            "BLU-PARSE-0023",
                            "unexpected token after loop-control statement",
                            &["end of block"],
                        )?;
                        while self
                            .current()
                            .is_some_and(|token| !terminators.contains(&token.kind()))
                        {
                            self.bump();
                        }
                    }
                }
                TokenKind::Return => {
                    self.parse_return()?;
                    if self.at(TokenKind::Semicolon) {
                        self.bump();
                    }
                    if self.at(TokenKind::Semicolon)
                        && matches!(
                            self.lexed.profile(),
                            SemanticProfile::Lua51
                                | SemanticProfile::Lua52
                                | SemanticProfile::Lua53
                                | SemanticProfile::Lua54
                                | SemanticProfile::Lua55
                        )
                    {
                        self.report_current(
                            "BLU-PARSE-0005",
                            "unexpected token after return statement",
                            &["end of source"],
                        )?;
                    }
                    if self.current().is_some_and(|token| {
                        !terminators.contains(&token.kind()) && token.kind() != TokenKind::Semicolon
                    }) {
                        self.report_current(
                            "BLU-PARSE-0005",
                            "unexpected token after return statement",
                            &["end of source"],
                        )?;
                        while self
                            .current()
                            .is_some_and(|token| !terminators.contains(&token.kind()))
                        {
                            self.bump();
                        }
                    }
                }
                _ => {
                    self.report_current(
                        "BLU-PARSE-0001",
                        "expected a supported statement",
                        &[
                            "local",
                            "function",
                            "assignment",
                            "if",
                            "while",
                            "repeat",
                            "do",
                            "for",
                            "break",
                            "continue",
                            "::label::",
                            "goto",
                            "return",
                        ],
                    )?;
                    self.bump();
                }
            }
        }
        Ok(())
    }

    fn parse_label(&mut self) -> Result<(), ParseError> {
        let Some(open) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "label parser entered without a current token",
            });
        };
        if self.lexed.profile() == SemanticProfile::Luau {
            self.report_current(
                "BLU-PARSE-0055",
                "labels are unavailable in the Luau profile; `::` is reserved for type assertions",
                &["type assertion"],
            )?;
            while self
                .current()
                .is_some_and(|token| token.kind() != TokenKind::ColonColon)
            {
                self.bump();
            }
            self.bump();
            return Ok(());
        }
        if !self.at(TokenKind::Identifier) {
            self.report_current_or_eof(
                "BLU-PARSE-0046",
                "expected label name after `::`",
                &["identifier"],
            )?;
            return Ok(());
        }
        let Some(name) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "label identifier check succeeded without a current token",
            });
        };
        if !self.at(TokenKind::ColonColon) {
            self.report_current_or_eof(
                "BLU-PARSE-0047",
                "expected `::` after label name",
                &["::"],
            )?;
            return Ok(());
        }
        let Some(close) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "label close check succeeded without a current token",
            });
        };
        self.push_statement(Statement::Label(LabelStatement::new(
            Identifier::new(name.span()),
            open.span().merge(close.span())?,
        )))
    }

    fn parse_goto(&mut self) -> Result<(), ParseError> {
        let Some(keyword) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "goto parser entered without a current token",
            });
        };
        if !self.at(TokenKind::Identifier) {
            self.report_current_or_eof(
                "BLU-PARSE-0048",
                "expected label name after `goto`",
                &["identifier"],
            )?;
            return Ok(());
        }
        let Some(name) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "goto identifier check succeeded without a current token",
            });
        };
        self.push_statement(Statement::Goto(GotoStatement::new(
            Identifier::new(name.span()),
            keyword.span().merge(name.span())?,
        )))
    }

    fn parse_if(&mut self) -> Result<(), ParseError> {
        let Some(keyword) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "if parser entered without a current token",
            });
        };
        let mut clauses = allocate_vec(1, "if clauses")?;
        let mut clause_keyword = keyword;
        loop {
            let Some(condition) = self.parse_expression(0)? else {
                return Ok(());
            };
            if !self.at(TokenKind::Then) {
                self.report_current_or_eof(
                    "BLU-PARSE-0018",
                    "expected `then` after if condition",
                    &["then"],
                )?;
                return Ok(());
            }
            let Some(then_token) = self.bump() else {
                return Err(ParseError::InternalInvariant {
                    message: "then check succeeded without a current token",
                });
            };
            let body =
                self.parse_nested_block(&[TokenKind::ElseIf, TokenKind::Else, TokenKind::End])?;
            let clause_end = body
                .statements()
                .last()
                .map_or(then_token.span(), Statement::span);
            push_fallible(
                &mut clauses,
                IfClause::new(condition.id, body, clause_keyword.span().merge(clause_end)?),
                "if clauses",
            )?;
            if !self.at(TokenKind::ElseIf) {
                break;
            }
            let Some(elseif) = self.bump() else {
                return Err(ParseError::InternalInvariant {
                    message: "elseif check succeeded without a current token",
                });
            };
            clause_keyword = elseif;
        }

        let else_body = if self.at(TokenKind::Else) {
            self.bump();
            Some(self.parse_nested_block(&[TokenKind::End])?)
        } else {
            None
        };
        if !self.at(TokenKind::End) {
            self.report_current_or_eof(
                "BLU-PARSE-0019",
                "expected `end` to close if statement",
                &["end"],
            )?;
            return Ok(());
        }
        let Some(end) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "end check succeeded without a current token",
            });
        };
        self.push_statement(Statement::If(IfStatement::new(
            clauses,
            else_body,
            keyword.span().merge(end.span())?,
        )))
    }

    fn parse_while(&mut self) -> Result<(), ParseError> {
        let Some(keyword) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "while parser entered without a current token",
            });
        };
        let Some(condition) = self.parse_expression(0)? else {
            return Ok(());
        };
        if !self.at(TokenKind::Do) {
            self.report_current_or_eof(
                "BLU-PARSE-0020",
                "expected `do` after while condition",
                &["do"],
            )?;
            return Ok(());
        }
        self.bump();
        self.loop_depth += 1;
        let parsed_body = self.parse_nested_block(&[TokenKind::End]);
        self.loop_depth -= 1;
        let body = parsed_body?;
        if !self.at(TokenKind::End) {
            self.report_current_or_eof(
                "BLU-PARSE-0021",
                "expected `end` to close while statement",
                &["end"],
            )?;
            return Ok(());
        }
        let Some(end) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "end check succeeded without a current token",
            });
        };
        self.push_statement(Statement::While(WhileStatement::new(
            condition.id,
            body,
            keyword.span().merge(end.span())?,
        )))
    }

    fn parse_repeat(&mut self) -> Result<(), ParseError> {
        let Some(keyword) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "repeat parser entered without a current token",
            });
        };
        self.loop_depth += 1;
        let parsed_body = self.parse_nested_block(&[TokenKind::Until]);
        self.loop_depth -= 1;
        let body = parsed_body?;
        if !self.at(TokenKind::Until) {
            self.report_current_or_eof(
                "BLU-PARSE-0024",
                "expected `until` to close repeat statement",
                &["until"],
            )?;
            return Ok(());
        }
        self.bump();
        let Some(condition) = self.parse_expression(0)? else {
            return Ok(());
        };
        let condition_span = self
            .expressions
            .get(condition.id.as_usize())
            .ok_or(ParseError::InternalInvariant {
                message: "repeat condition expression is missing",
            })?
            .span();
        self.push_statement(Statement::Repeat(RepeatStatement::new(
            body,
            condition.id,
            keyword.span().merge(condition_span)?,
        )))
    }

    fn parse_do(&mut self) -> Result<(), ParseError> {
        let Some(keyword) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "do parser entered without a current token",
            });
        };
        let body = self.parse_nested_block(&[TokenKind::End])?;
        if !self.at(TokenKind::End) {
            self.report_current_or_eof(
                "BLU-PARSE-0025",
                "expected `end` to close do statement",
                &["end"],
            )?;
            return Ok(());
        }
        let Some(end) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "end check succeeded without a current token",
            });
        };
        self.push_statement(Statement::Do(DoStatement::new(
            body,
            keyword.span().merge(end.span())?,
        )))
    }

    fn parse_for(&mut self) -> Result<(), ParseError> {
        let Some(keyword) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "for parser entered without a current token",
            });
        };
        if !self.at(TokenKind::Identifier) {
            self.report_current_or_eof(
                "BLU-PARSE-0026",
                "expected loop variable after `for`",
                &["identifier"],
            )?;
            return Ok(());
        }
        let Some(name) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "identifier check succeeded without a current token",
            });
        };
        let name = Identifier::new(name.span());
        if !self.at(TokenKind::Equal) {
            return self.parse_generic_for(keyword, name);
        }
        self.bump();
        let Some(initial) = self.parse_expression(0)? else {
            return Ok(());
        };
        if !self.at(TokenKind::Comma) {
            self.report_current_or_eof(
                "BLU-PARSE-0028",
                "expected `,` after numeric for initial value",
                &[","],
            )?;
            return Ok(());
        }
        self.bump();
        let Some(limit) = self.parse_expression(0)? else {
            return Ok(());
        };
        let step = if self.at(TokenKind::Comma) {
            self.bump();
            self.parse_expression(0)?.map(|expression| expression.id)
        } else {
            None
        };
        if !self.at(TokenKind::Do) {
            self.report_current_or_eof(
                "BLU-PARSE-0030",
                "expected `do` after numeric for controls",
                &["do"],
            )?;
            return Ok(());
        }
        self.bump();
        self.loop_depth += 1;
        let parsed_body = self.parse_nested_block(&[TokenKind::End]);
        self.loop_depth -= 1;
        let body = parsed_body?;
        if !self.at(TokenKind::End) {
            self.report_current_or_eof(
                "BLU-PARSE-0031",
                "expected `end` to close numeric for statement",
                &["end"],
            )?;
            return Ok(());
        }
        let Some(end) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "end check succeeded without a current token",
            });
        };
        self.push_statement(Statement::NumericFor(NumericForStatement::new(
            name,
            initial.id,
            limit.id,
            step,
            body,
            keyword.span().merge(end.span())?,
        )))
    }

    fn parse_generic_for(
        &mut self,
        keyword: Token,
        first_name: Identifier,
    ) -> Result<(), ParseError> {
        let mut names = allocate_vec(2, "generic for names")?;
        push_fallible(&mut names, first_name, "generic for names")?;
        while self.at(TokenKind::Comma) {
            self.bump();
            if !self.at(TokenKind::Identifier) {
                self.report_current_or_eof(
                    "BLU-PARSE-0042",
                    "expected loop variable after `,`",
                    &["identifier"],
                )?;
                return Ok(());
            }
            let Some(name) = self.bump() else {
                return Err(ParseError::InternalInvariant {
                    message: "identifier check succeeded without a current token",
                });
            };
            push_fallible(
                &mut names,
                Identifier::new(name.span()),
                "generic for names",
            )?;
        }
        if !self.at(TokenKind::In) {
            self.report_current_or_eof(
                "BLU-PARSE-0043",
                "expected `in` after generic for variables",
                &["in"],
            )?;
            return Ok(());
        }
        self.bump();
        let mut values = allocate_vec(2, "generic for values")?;
        let Some(first) = self.parse_expression(0)? else {
            return Ok(());
        };
        push_fallible(&mut values, first.id, "generic for values")?;
        while self.at(TokenKind::Comma) {
            self.bump();
            let Some(value) = self.parse_expression(0)? else {
                return Ok(());
            };
            push_fallible(&mut values, value.id, "generic for values")?;
        }
        if !self.at(TokenKind::Do) {
            self.report_current_or_eof(
                "BLU-PARSE-0044",
                "expected `do` after generic for values",
                &["do"],
            )?;
            return Ok(());
        }
        self.bump();
        self.loop_depth += 1;
        let parsed_body = self.parse_nested_block(&[TokenKind::End]);
        self.loop_depth -= 1;
        let body = parsed_body?;
        if !self.at(TokenKind::End) {
            self.report_current_or_eof(
                "BLU-PARSE-0045",
                "expected `end` to close generic for statement",
                &["end"],
            )?;
            return Ok(());
        }
        let Some(end) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "end check succeeded without a current token",
            });
        };
        self.push_statement(Statement::GenericFor(GenericForStatement::new(
            names,
            values,
            body,
            keyword.span().merge(end.span())?,
        )))
    }

    fn parse_nested_block(&mut self, terminators: &[TokenKind]) -> Result<Block, ParseError> {
        let depth = self.block_depth.saturating_add(1);
        if depth > self.limits.max_expression_depth {
            return Err(ParseError::Limit {
                kind: ParseLimit::ExpressionDepth,
                required: depth,
                limit: self.limits.max_expression_depth,
            });
        }
        let capacity = self
            .lexed
            .tokens()
            .len()
            .min(
                self.limits
                    .max_ast_nodes
                    .saturating_sub(self.statement_count),
            )
            .min(64);
        let nested = allocate_vec(capacity, "block statements")?;
        let outer = core::mem::replace(&mut self.statements, nested);
        self.block_depth = depth;
        let parsed = self.parse_statements(terminators);
        self.block_depth -= 1;
        let body = core::mem::replace(&mut self.statements, outer);
        parsed?;
        Ok(Block::new(body))
    }

    fn parse_function_body(&mut self, keyword: Token) -> Result<Option<FunctionId>, ParseError> {
        if !self.at(TokenKind::LeftParenthesis) {
            self.report_current_or_eof(
                "BLU-PARSE-0035",
                "expected `(` before function parameters",
                &["("],
            )?;
            return Ok(None);
        }
        self.bump();
        let mut parameters = allocate_vec(2, "function parameters")?;
        let mut is_vararg = false;
        let mut vararg_name = None;
        if !self.at(TokenKind::RightParenthesis) {
            loop {
                if self.at(TokenKind::Ellipsis) {
                    self.bump();
                    is_vararg = true;
                    if supports_named_vararg(self.lexed.profile()) && self.at(TokenKind::Identifier)
                    {
                        let Some(name) = self.bump() else {
                            return Err(ParseError::InternalInvariant {
                                message: "identifier check succeeded without a current token",
                            });
                        };
                        vararg_name = Some(Identifier::new(name.span()));
                    }
                    if supports_type_annotations(self.lexed.profile()) && self.at(TokenKind::Colon)
                    {
                        self.skip_type_annotation()?;
                    }
                    break;
                }
                if !self.at(TokenKind::Identifier) {
                    self.report_current_or_eof(
                        "BLU-PARSE-0036",
                        "expected a function parameter name",
                        &["identifier", "...", ")"],
                    )?;
                    return Ok(None);
                }
                let Some(parameter) = self.bump() else {
                    return Err(ParseError::InternalInvariant {
                        message: "identifier check succeeded without a current token",
                    });
                };
                if supports_type_annotations(self.lexed.profile()) && self.at(TokenKind::Colon) {
                    self.skip_type_annotation()?;
                }
                self.function_node_count = self.function_node_count.saturating_add(1);
                self.check_ast_limit()?;
                push_fallible(
                    &mut parameters,
                    Identifier::new(parameter.span()),
                    "function parameters",
                )?;
                if !self.at(TokenKind::Comma) {
                    break;
                }
                self.bump();
            }
        }
        if !self.at(TokenKind::RightParenthesis) {
            self.report_current_or_eof(
                "BLU-PARSE-0037",
                "expected `)` after function parameters",
                &[")"],
            )?;
            return Ok(None);
        }
        self.bump();
        if supports_type_annotations(self.lexed.profile()) && self.at(TokenKind::Colon) {
            self.skip_type_annotation()?;
        }
        let outer_loop_depth = core::mem::replace(&mut self.loop_depth, 0);
        let parsed_body = self.parse_nested_block(&[TokenKind::End]);
        self.loop_depth = outer_loop_depth;
        let body = parsed_body?;
        if !self.at(TokenKind::End) {
            self.report_current_or_eof(
                "BLU-PARSE-0038",
                "expected `end` to close function",
                &["end"],
            )?;
            return Ok(None);
        }
        let Some(end) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "end check succeeded without a current token",
            });
        };
        self.function_node_count = self.function_node_count.saturating_add(1);
        self.check_ast_limit()?;
        let id = FunctionId::new(self.functions.len());
        push_fallible(
            &mut self.functions,
            FunctionBody::new(
                parameters,
                is_vararg,
                vararg_name,
                body,
                keyword.span().merge(end.span())?,
            ),
            "AST functions",
        )?;
        Ok(Some(id))
    }

    fn skip_type_annotation(&mut self) -> Result<(), ParseError> {
        if !self.at(TokenKind::Colon) {
            return Err(ParseError::InternalInvariant {
                message: "type annotation parser entered without a colon",
            });
        }
        self.bump();
        self.skip_type_expression()
    }

    fn skip_type_assertion(&mut self) -> Result<(), ParseError> {
        if !self.at(TokenKind::ColonColon) {
            return Err(ParseError::InternalInvariant {
                message: "type assertion parser entered without `::`",
            });
        }
        self.bump();
        self.skip_type_expression()
    }

    fn skip_type_expression(&mut self) -> Result<(), ParseError> {
        self.skip_type_primary()?;

        if self.at(TokenKind::Question) {
            self.bump();
        }

        // The owned AST intentionally has no type information. Preserve the
        // useful Luau/Blu compatibility slice by consuming qualified and
        // union/intersection names while leaving all type-checking to a future
        // typed frontend.
        loop {
            if self.at(TokenKind::BitwiseOr) || self.at(TokenKind::BitwiseAnd) {
                self.bump();
                self.skip_type_primary()?;
                if self.at(TokenKind::Question) {
                    self.bump();
                }
                continue;
            }
            break;
        }
        Ok(())
    }

    fn skip_type_primary(&mut self) -> Result<(), ParseError> {
        if self.at_type_atom() {
            self.bump();
            while self.at(TokenKind::Dot) {
                self.bump();
                if !self.at_type_atom() {
                    self.report_current_or_eof(
                        "BLU-PARSE-0041",
                        "expected a type name after `.`",
                        &["identifier", "nil"],
                    )?;
                    return Ok(());
                }
                self.bump();
            }
            if self.at(TokenKind::LessThan) {
                self.skip_type_container(
                    TokenKind::LessThan,
                    TokenKind::GreaterThan,
                    "BLU-PARSE-0043",
                    ">",
                )?;
            }
            return Ok(());
        }
        if self.at(TokenKind::LeftBrace) {
            return self.skip_type_container(
                TokenKind::LeftBrace,
                TokenKind::RightBrace,
                "BLU-PARSE-0043",
                "}",
            );
        }
        if self.at(TokenKind::LeftParenthesis) {
            self.skip_type_container(
                TokenKind::LeftParenthesis,
                TokenKind::RightParenthesis,
                "BLU-PARSE-0043",
                ")",
            )?;
            if self.at(TokenKind::Minus) {
                self.bump();
                if self.at(TokenKind::GreaterThan) {
                    self.bump();
                    self.skip_type_expression()?;
                }
            }
            return Ok(());
        }
        self.report_current_or_eof(
            "BLU-PARSE-0040",
            "expected a type name after `:`",
            &["identifier", "nil", "{", "("],
        )?;
        Ok(())
    }

    fn skip_type_container(
        &mut self,
        open: TokenKind,
        close: TokenKind,
        code: &'static str,
        expected: &'static str,
    ) -> Result<(), ParseError> {
        if !self.at(open) {
            return Err(ParseError::InternalInvariant {
                message: "type container parser entered without its opener",
            });
        }
        self.bump();
        let mut depth = 1usize;
        while let Some(token) = self.current() {
            if token.kind() == open {
                depth = depth.saturating_add(1);
            } else if token.kind() == close {
                depth -= 1;
                self.bump();
                if depth == 0 {
                    return Ok(());
                }
                continue;
            }
            self.bump();
        }
        self.report_current_or_eof(code, "unterminated type annotation", &[expected])?;
        Ok(())
    }

    fn at_type_atom(&self) -> bool {
        self.at(TokenKind::Identifier) || self.at(TokenKind::Nil)
    }

    fn parse_function_statement(&mut self) -> Result<(), ParseError> {
        self.parse_function_statement_with_global(false)
    }

    fn parse_function_statement_with_global(&mut self, is_global: bool) -> Result<(), ParseError> {
        let Some(keyword) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "function parser entered without a current token",
            });
        };
        if !self.at(TokenKind::Identifier) {
            self.report_current_or_eof(
                "BLU-PARSE-0039",
                "expected a function name",
                &["identifier"],
            )?;
            return Ok(());
        }
        let Some(name) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "identifier check succeeded without a current token",
            });
        };
        let mut names = allocate_vec(2, "function name path")?;
        push_fallible(
            &mut names,
            Identifier::new(name.span()),
            "function name path",
        )?;
        while self.at(TokenKind::Dot) {
            self.bump();
            if !self.at(TokenKind::Identifier) {
                self.report_current_or_eof(
                    "BLU-PARSE-0040",
                    "expected a name after `.` in function declaration",
                    &["identifier"],
                )?;
                return Ok(());
            }
            let Some(field) = self.bump() else {
                return Err(ParseError::InternalInvariant {
                    message: "identifier check succeeded without a current token",
                });
            };
            self.function_node_count = self.function_node_count.saturating_add(1);
            self.check_ast_limit()?;
            push_fallible(
                &mut names,
                Identifier::new(field.span()),
                "function name path",
            )?;
        }
        let is_method = if self.at(TokenKind::Colon) {
            self.bump();
            if !self.at(TokenKind::Identifier) {
                self.report_current_or_eof(
                    "BLU-PARSE-0041",
                    "expected a method name after `:`",
                    &["identifier"],
                )?;
                return Ok(());
            }
            let Some(method) = self.bump() else {
                return Err(ParseError::InternalInvariant {
                    message: "identifier check succeeded without a current token",
                });
            };
            self.function_node_count = self.function_node_count.saturating_add(1);
            self.check_ast_limit()?;
            push_fallible(
                &mut names,
                Identifier::new(method.span()),
                "function name path",
            )?;
            true
        } else {
            false
        };
        let Some(function) = self.parse_function_body(keyword)? else {
            return Ok(());
        };
        let Some(body) = self.functions.get(function.as_usize()) else {
            return Err(ParseError::InternalInvariant {
                message: "new function body is out of bounds",
            });
        };
        let span = keyword.span().merge(body.span())?;
        let statement = if is_global {
            if is_method || names.len() != 1 {
                self.report_current(
                    "BLU-PARSE-0054",
                    "global function declarations require one plain name",
                    &["plain function name"],
                )?;
            }
            FunctionStatement::new_global(names, function, span)
        } else {
            FunctionStatement::new(names, function, is_method, span)
        };
        self.push_statement(Statement::Function(statement))
    }

    fn parse_local(&mut self) -> Result<(), ParseError> {
        let Some(keyword) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "local parser entered without a current token",
            });
        };
        if self.at(TokenKind::Function) {
            let Some(function_keyword) = self.bump() else {
                return Err(ParseError::InternalInvariant {
                    message: "function check succeeded without a current token",
                });
            };
            if !self.at(TokenKind::Identifier) {
                self.report_current_or_eof(
                    "BLU-PARSE-0034",
                    "expected a local function name",
                    &["identifier"],
                )?;
                return Ok(());
            }
            let Some(name) = self.bump() else {
                return Err(ParseError::InternalInvariant {
                    message: "identifier check succeeded without a current token",
                });
            };
            let Some(function) = self.parse_function_body(function_keyword)? else {
                return Ok(());
            };
            let Some(body) = self.functions.get(function.as_usize()) else {
                return Err(ParseError::InternalInvariant {
                    message: "new function body is out of bounds",
                });
            };
            return self.push_statement(Statement::LocalFunction(LocalFunctionStatement::new(
                Identifier::new(name.span()),
                function,
                keyword.span().merge(body.span())?,
            )));
        }
        let default_attribute = self.parse_local_attribute(LocalAttribute::Regular)?;
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

        let mut names = allocate_vec(1, "local name list")?;
        let mut attributes = allocate_vec(1, "local attribute list")?;
        if let Some(name) = name {
            names.push(name);
            if supports_type_annotations(self.lexed.profile()) && self.at(TokenKind::Colon) {
                self.skip_type_annotation()?;
            }
            attributes.push(self.parse_local_attribute(default_attribute)?);
        }
        while self.at(TokenKind::Comma) {
            self.bump();
            if !self.at(TokenKind::Identifier) {
                self.report_current_or_eof(
                    "BLU-PARSE-0002",
                    "expected a local name",
                    &["identifier"],
                )?;
                break;
            }
            let Some(identifier) = self.bump() else {
                return Err(ParseError::InternalInvariant {
                    message: "identifier check succeeded without a current token",
                });
            };
            if supports_type_annotations(self.lexed.profile()) && self.at(TokenKind::Colon) {
                self.skip_type_annotation()?;
            }
            push_fallible(
                &mut names,
                Identifier::new(identifier.span()),
                "local name list",
            )?;
            push_fallible(
                &mut attributes,
                self.parse_local_attribute(default_attribute)?,
                "local attribute list",
            )?;
        }

        let mut values = allocate_vec(1, "local value list")?;
        if self.at(TokenKind::Equal) {
            self.bump();
            if let Some(value) = self.parse_expression(0)? {
                values.push(value.id);
                while self.at(TokenKind::Comma) {
                    self.bump();
                    let Some(value) = self.parse_expression(0)? else {
                        break;
                    };
                    push_fallible(&mut values, value.id, "local value list")?;
                }
            }
        }
        if names.is_empty() {
            return Ok(());
        }
        let end = if let Some(value) = values.last() {
            self.expression(*value)?.span()
        } else {
            let Some(name) = names.last() else {
                return Err(ParseError::InternalInvariant {
                    message: "non-empty local name list became empty",
                });
            };
            name.span()
        };
        let span = keyword.span().merge(end)?;
        if names.len() == 1 && values.len() <= 1 {
            let Some(name) = names.first().copied() else {
                return Err(ParseError::InternalInvariant {
                    message: "single local name list became empty",
                });
            };
            self.push_statement(Statement::Local(LocalStatement::new(
                name,
                values.first().copied(),
                attributes
                    .first()
                    .copied()
                    .unwrap_or(LocalAttribute::Regular),
                span,
            )))?;
        } else {
            self.push_statement(Statement::LocalList(LocalListStatement::new(
                names, values, attributes, span,
            )))?;
        }
        Ok(())
    }

    fn parse_global(&mut self) -> Result<(), ParseError> {
        let Some(keyword) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "global parser entered without a current token",
            });
        };
        if !supports_global_declarations(self.lexed.profile()) {
            self.report_current(
                "BLU-PARSE-0051",
                "global declarations are unavailable in this profile",
                &["identifier"],
            )?;
            return Ok(());
        }
        if self.at(TokenKind::Function) {
            return self.parse_function_statement_with_global(true);
        }

        let leading_attribute = self.parse_local_attribute(LocalAttribute::Regular)?;

        let mut names = allocate_vec(1, "global name list")?;
        let mut attributes = allocate_vec(1, "global name attributes")?;
        let wildcard = if self.at(TokenKind::Star) {
            self.bump();
            push_fallible(&mut attributes, leading_attribute, "global name attributes")?;
            true
        } else {
            loop {
                if !self.at(TokenKind::Identifier) {
                    self.report_current_or_eof(
                        "BLU-PARSE-0052",
                        "expected a global name or `*`",
                        &["identifier", "*"],
                    )?;
                    break;
                }
                let Some(identifier) = self.bump() else {
                    return Err(ParseError::InternalInvariant {
                        message: "identifier check succeeded without a current token",
                    });
                };
                let inline_attribute = self.parse_local_attribute(LocalAttribute::Regular)?;
                push_fallible(
                    &mut names,
                    Identifier::new(identifier.span()),
                    "global name list",
                )?;
                push_fallible(
                    &mut attributes,
                    if inline_attribute == LocalAttribute::Regular {
                        leading_attribute
                    } else {
                        inline_attribute
                    },
                    "global name attributes",
                )?;
                if !self.at(TokenKind::Comma) {
                    break;
                }
                self.bump();
            }
            false
        };

        let mut values = allocate_vec(1, "global value list")?;
        if self.at(TokenKind::Equal) {
            self.bump();
            if wildcard {
                self.report_current(
                    "BLU-PARSE-0053",
                    "`global *` cannot have initial values",
                    &["end of declaration"],
                )?;
            } else if let Some(value) = self.parse_expression(0)? {
                values.push(value.id);
                while self.at(TokenKind::Comma) {
                    self.bump();
                    let Some(value) = self.parse_expression(0)? else {
                        break;
                    };
                    push_fallible(&mut values, value.id, "global value list")?;
                }
            }
        }
        let end = if let Some(value) = values.last() {
            self.expression(*value)?.span()
        } else if let Some(name) = names.last() {
            name.span()
        } else {
            keyword.span()
        };
        self.push_statement(Statement::Global(GlobalStatement::new(
            names,
            values,
            wildcard,
            attributes,
            keyword.span().merge(end)?,
        )))
    }

    fn parse_local_attribute(
        &mut self,
        default: LocalAttribute,
    ) -> Result<LocalAttribute, ParseError> {
        if !self.at(TokenKind::LessThan) {
            return Ok(default);
        }
        let Some(open) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "local attribute opener check succeeded without a current token",
            });
        };
        let Some(name) = self.current() else {
            self.report_current_or_eof(
                "BLU-PARSE-0050",
                "expected `const` or `close` inside local attribute",
                &["const", "close"],
            )?;
            return Ok(default);
        };
        if name.kind() != TokenKind::Identifier {
            self.report_current_or_eof(
                "BLU-PARSE-0050",
                "expected `const` or `close` inside local attribute",
                &["const", "close"],
            )?;
            return Ok(default);
        }
        let Some(name) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "local attribute name check succeeded without a current token",
            });
        };
        if !self.at(TokenKind::GreaterThan) {
            self.report_current_or_eof(
                "BLU-PARSE-0051",
                "expected `>` after local attribute",
                &[">"],
            )?;
        } else {
            self.bump();
        }
        let attribute = match self.source.slice(name.span())? {
            b"const" => LocalAttribute::Const,
            b"close" => LocalAttribute::Close,
            _ => {
                let diagnostic = parser_diagnostic(
                    "BLU-PARSE-0052",
                    self.lexed.profile(),
                    open.span().merge(name.span())?,
                    "local attribute must be `const` or `close`",
                    &["const", "close"],
                    Some(self.source.slice(name.span())?),
                    self.limits.lexer.diagnostic_limits,
                )?;
                self.push_diagnostic(diagnostic)?;
                return Ok(default);
            }
        };
        if !supports_local_attributes(self.lexed.profile()) {
            let diagnostic = parser_diagnostic(
                "BLU-PARSE-0053",
                self.lexed.profile(),
                open.span().merge(name.span())?,
                "local attributes are unavailable in this profile",
                &["Lua 5.4", "Lua 5.5"],
                Some(self.source.slice(name.span())?),
                self.limits.lexer.diagnostic_limits,
            )?;
            self.push_diagnostic(diagnostic)?;
        }
        Ok(attribute)
    }

    fn parse_return(&mut self) -> Result<(), ParseError> {
        let Some(keyword) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "return parser entered without a current token",
            });
        };
        let mut values = allocate_vec(1, "return expression list")?;
        if self.current().is_none()
            || self.at(TokenKind::Semicolon)
            || self.current().is_some_and(|token| {
                matches!(
                    token.kind(),
                    TokenKind::End | TokenKind::Else | TokenKind::ElseIf | TokenKind::Until
                )
            })
        {
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

    fn parse_assignment(&mut self) -> Result<(), ParseError> {
        let Some(target) = self.bump() else {
            return Err(ParseError::InternalInvariant {
                message: "assignment parser entered without a current token",
            });
        };
        let mut targets = allocate_vec(1, "assignment target list")?;
        let identifier = Identifier::new(target.span());
        let target_expression = self.push_expression(
            Expression::new(ExpressionKind::Identifier(identifier), target.span()),
            1,
        )?;
        let target_expression = self.parse_postfix(target_expression)?;
        if matches!(
            self.expression(target_expression.id)?.kind(),
            ExpressionKind::Call(_) | ExpressionKind::MethodCall(_)
        ) && !self.at(TokenKind::Equal)
            && !self.at(TokenKind::Comma)
        {
            let span = self.expression(target_expression.id)?.span();
            return self.push_statement(Statement::Call(CallStatement::new(
                target_expression.id,
                span,
            )));
        }
        let first_target = match self.expression(target_expression.id)?.kind() {
            ExpressionKind::Identifier(identifier) => AssignmentTarget::Identifier(identifier),
            ExpressionKind::Index(index) => AssignmentTarget::Index(index),
            ExpressionKind::Field(field) => AssignmentTarget::Field(field),
            _ => {
                return Err(ParseError::InternalInvariant {
                    message: "assignment target parser produced a non-target expression",
                });
            }
        };
        if let Some(operator) = compound_assignment_operator(self.current().map(Token::kind)) {
            self.bump();
            let Some(value) = self.parse_expression(0)? else {
                return Ok(());
            };
            let span = target.span().merge(self.expression(value.id)?.span())?;
            return self.push_statement(Statement::CompoundAssignment(
                CompoundAssignmentStatement::new(first_target, operator, value.id, span),
            ));
        }
        targets.push(first_target);
        while self.at(TokenKind::Comma) {
            self.bump();
            if !self.at(TokenKind::Identifier) {
                self.report_current_or_eof(
                    "BLU-PARSE-0007",
                    "expected an assignment target",
                    &["identifier"],
                )?;
                return Ok(());
            }
            let Some(target) = self.bump() else {
                return Err(ParseError::InternalInvariant {
                    message: "assignment target check succeeded without a current token",
                });
            };
            let identifier = Identifier::new(target.span());
            let expression = self.push_expression(
                Expression::new(ExpressionKind::Identifier(identifier), target.span()),
                1,
            )?;
            let expression = self.parse_postfix(expression)?;
            let target = match self.expression(expression.id)?.kind() {
                ExpressionKind::Identifier(identifier) => AssignmentTarget::Identifier(identifier),
                ExpressionKind::Index(index) => AssignmentTarget::Index(index),
                ExpressionKind::Field(field) => AssignmentTarget::Field(field),
                _ => {
                    self.report_current_or_eof(
                        "BLU-PARSE-0046",
                        "expected an assignable identifier, index, or field",
                        &["identifier", "index", "field"],
                    )?;
                    return Ok(());
                }
            };
            push_fallible(&mut targets, target, "assignment target list")?;
        }
        if !self.at(TokenKind::Equal) {
            self.report_current_or_eof(
                "BLU-PARSE-0006",
                "expected `=` after assignment target",
                &["="],
            )?;
            return Ok(());
        }
        self.bump();
        let Some(first_value) = self.parse_expression(0)? else {
            return Ok(());
        };
        let mut values = allocate_vec(1, "assignment value list")?;
        values.push(first_value.id);
        while self.at(TokenKind::Comma) {
            self.bump();
            let Some(value) = self.parse_expression(0)? else {
                return Ok(());
            };
            push_fallible(&mut values, value.id, "assignment value list")?;
        }
        let Some(last_value) = values.last().copied() else {
            return Err(ParseError::InternalInvariant {
                message: "assignment value list became empty",
            });
        };
        let span = target.span().merge(self.expression(last_value)?.span())?;
        if targets.len() == 1 && values.len() == 1 {
            let Some(target) = targets.first().copied() else {
                return Err(ParseError::InternalInvariant {
                    message: "single assignment target list became empty",
                });
            };
            self.push_statement(Statement::Assignment(AssignmentStatement::new(
                target,
                first_value.id,
                span,
            )))
        } else {
            self.push_statement(Statement::AssignmentList(AssignmentListStatement::new(
                targets, values, span,
            )))
        }
    }

    fn parse_expression_statement(&mut self) -> Result<(), ParseError> {
        let Some(expression) = self.parse_expression(0)? else {
            return Ok(());
        };
        let kind = self.expression(expression.id)?.kind();
        if self.at(TokenKind::Equal)
            || self.at(TokenKind::Comma)
            || compound_assignment_operator(self.current().map(Token::kind)).is_some()
        {
            let first_target = match kind {
                ExpressionKind::Identifier(identifier) => AssignmentTarget::Identifier(identifier),
                ExpressionKind::Index(index) => AssignmentTarget::Index(index),
                ExpressionKind::Field(field) => AssignmentTarget::Field(field),
                _ => {
                    self.report_current_or_eof(
                        "BLU-PARSE-0007",
                        "expected an assignable identifier, index, or field",
                        &["identifier", "index", "field"],
                    )?;
                    return Ok(());
                }
            };
            if let Some(operator) = compound_assignment_operator(self.current().map(Token::kind)) {
                self.bump();
                let Some(value) = self.parse_expression(0)? else {
                    return Ok(());
                };
                let span = self
                    .expression(expression.id)?
                    .span()
                    .merge(self.expression(value.id)?.span())?;
                return self.push_statement(Statement::CompoundAssignment(
                    CompoundAssignmentStatement::new(first_target, operator, value.id, span),
                ));
            }
            let mut targets = allocate_vec(1, "assignment target list")?;
            targets.push(first_target);
            while self.at(TokenKind::Comma) {
                self.bump();
                let Some(target_expression) = self.parse_expression(0)? else {
                    return Ok(());
                };
                let target = match self.expression(target_expression.id)?.kind() {
                    ExpressionKind::Identifier(identifier) => {
                        AssignmentTarget::Identifier(identifier)
                    }
                    ExpressionKind::Index(index) => AssignmentTarget::Index(index),
                    ExpressionKind::Field(field) => AssignmentTarget::Field(field),
                    _ => {
                        self.report_current_or_eof(
                            "BLU-PARSE-0046",
                            "expected an assignable identifier, index, or field",
                            &["identifier", "index", "field"],
                        )?;
                        return Ok(());
                    }
                };
                push_fallible(&mut targets, target, "assignment target list")?;
            }
            if !self.at(TokenKind::Equal) {
                self.report_current_or_eof(
                    "BLU-PARSE-0006",
                    "expected `=` after assignment target",
                    &["="],
                )?;
                return Ok(());
            }
            self.bump();
            let Some(first_value) = self.parse_expression(0)? else {
                return Ok(());
            };
            let mut values = allocate_vec(1, "assignment value list")?;
            values.push(first_value.id);
            while self.at(TokenKind::Comma) {
                self.bump();
                let Some(value) = self.parse_expression(0)? else {
                    return Ok(());
                };
                push_fallible(&mut values, value.id, "assignment value list")?;
            }
            let Some(last_value) = values.last().copied() else {
                return Err(ParseError::InternalInvariant {
                    message: "assignment value list became empty",
                });
            };
            let span = self
                .expression(expression.id)?
                .span()
                .merge(self.expression(last_value)?.span())?;
            if targets.len() == 1 && values.len() == 1 {
                let Some(target) = targets.first().copied() else {
                    return Err(ParseError::InternalInvariant {
                        message: "single assignment target list became empty",
                    });
                };
                return self.push_statement(Statement::Assignment(AssignmentStatement::new(
                    target,
                    first_value.id,
                    span,
                )));
            }
            return self.push_statement(Statement::AssignmentList(AssignmentListStatement::new(
                targets, values, span,
            )));
        }
        if matches!(
            kind,
            ExpressionKind::Call(_) | ExpressionKind::MethodCall(_)
        ) {
            return self.push_statement(Statement::Call(CallStatement::new(
                expression.id,
                self.expression(expression.id)?.span(),
            )));
        }
        let span = self.expression(expression.id)?.span();
        let diagnostic = parser_diagnostic(
            "BLU-PARSE-0049",
            self.lexed.profile(),
            span,
            "expected a function or method call statement",
            &["function call"],
            Some(self.source.slice(span)?),
            self.limits.lexer.diagnostic_limits,
        )?;
        self.push_diagnostic(diagnostic)?;
        if self.current().is_some() && !self.at(TokenKind::Semicolon) {
            self.report_current(
                "BLU-PARSE-0001",
                "unexpected token after expression statement",
                &["semicolon", "end of block"],
            )?;
            self.bump();
        }
        Ok(())
    }

    fn parse_expression(
        &mut self,
        minimum_precedence: u8,
    ) -> Result<Option<BuiltExpression>, ParseError> {
        let Some(mut left) = self.parse_prefix()? else {
            return Ok(None);
        };

        while let Some(operator_token) = self.current() {
            let Some((operator, precedence, right_precedence)) =
                binary_operator(operator_token.kind())
            else {
                break;
            };
            if precedence < minimum_precedence {
                break;
            }
            self.bump();
            let Some(right) = self.parse_expression(right_precedence)? else {
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
        let Some(operator) = self.current() else {
            return self.parse_primary();
        };
        let unary_operator = match operator.kind() {
            TokenKind::Not => UnaryOperator::Not,
            TokenKind::Minus => UnaryOperator::Negate,
            TokenKind::BitwiseExclusiveOr => UnaryOperator::BitwiseNot,
            TokenKind::Hash => UnaryOperator::Length,
            _ => return self.parse_primary(),
        };
        self.bump();
        let Some(operand) = self.parse_expression(11)? else {
            return Ok(None);
        };
        let span = operator.span().merge(self.expression(operand.id)?.span())?;
        self.push_expression(
            Expression::new(
                ExpressionKind::Unary(UnaryExpression::new(
                    unary_operator,
                    operator.span(),
                    operand.id,
                )),
                span,
            ),
            operand.depth.saturating_add(1),
        )
        .map(Some)
    }

    fn parse_interpolated_string(
        &mut self,
        start: Token,
    ) -> Result<Option<BuiltExpression>, ParseError> {
        self.bump();
        let mut parts = allocate_vec(2, "interpolated string parts")?;
        let mut depth = 1;
        loop {
            let Some(text) = self
                .current()
                .filter(|token| token.kind() == TokenKind::InterpolatedStringText)
            else {
                self.report_current_or_eof(
                    "BLU-PARSE-0057",
                    "expected interpolated string text",
                    &["interpolated string text"],
                )?;
                return Ok(None);
            };
            self.bump();
            push_fallible(
                &mut parts,
                InterpolatedStringPart::Text(text.span()),
                "interpolated string parts",
            )?;
            if self.at(TokenKind::InterpolatedStringEnd) {
                let Some(end) = self.bump() else {
                    return Err(ParseError::InternalInvariant {
                        message: "interpolated string end check succeeded without a token",
                    });
                };
                let first_part = self.interpolated_parts.len();
                let part_count = parts.len();
                for part in parts {
                    push_fallible(
                        &mut self.interpolated_parts,
                        part,
                        "AST interpolated string parts",
                    )?;
                }
                let span = start.span().merge(end.span())?;
                return self
                    .push_expression(
                        Expression::new(
                            ExpressionKind::InterpolatedString(InterpolatedString::new(
                                first_part, part_count, span,
                            )),
                            span,
                        ),
                        depth,
                    )
                    .map(Some);
            }
            if !self.at(TokenKind::InterpolationOpen) {
                self.report_current_or_eof(
                    "BLU-PARSE-0058",
                    "expected `{` in interpolated string",
                    &["{"],
                )?;
                return Ok(None);
            }
            self.bump();
            let Some(expression) = self.parse_expression(0)? else {
                return Ok(None);
            };
            depth = depth.max(expression.depth.saturating_add(1));
            if !self.at(TokenKind::InterpolationClose) {
                self.report_current_or_eof(
                    "BLU-PARSE-0059",
                    "expected `}` after interpolated expression",
                    &["}"],
                )?;
                return Ok(None);
            }
            self.bump();
            push_fallible(
                &mut parts,
                InterpolatedStringPart::Expression(expression.id),
                "interpolated string parts",
            )?;
        }
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
                    "decimal number",
                    "hexadecimal integer",
                    "hexadecimal number",
                    "binary integer",
                    "identifier",
                    "not",
                    "-",
                    "#",
                    "(",
                ],
            )?;
            return Ok(None);
        };
        let kind = match token.kind() {
            TokenKind::Nil => ExpressionKind::Nil,
            TokenKind::Ellipsis => ExpressionKind::Vararg,
            TokenKind::True => ExpressionKind::Boolean(true),
            TokenKind::False => ExpressionKind::Boolean(false),
            TokenKind::DecimalInteger => ExpressionKind::DecimalInteger,
            TokenKind::DecimalNumber => ExpressionKind::DecimalNumber,
            TokenKind::HexInteger => ExpressionKind::HexInteger,
            TokenKind::HexNumber => ExpressionKind::HexNumber,
            TokenKind::BinaryInteger => ExpressionKind::BinaryInteger,
            TokenKind::StringLiteral => ExpressionKind::StringLiteral,
            TokenKind::InterpolatedStringStart => {
                return self.parse_interpolated_string(token);
            }
            TokenKind::Identifier => ExpressionKind::Identifier(Identifier::new(token.span())),
            TokenKind::Global if self.lexed.profile() == SemanticProfile::Lua55 => {
                ExpressionKind::Identifier(Identifier::new(token.span()))
            }
            TokenKind::If => {
                if !matches!(
                    self.lexed.profile(),
                    SemanticProfile::Blu | SemanticProfile::Luau
                ) {
                    self.report_current(
                        "BLU-PARSE-0046",
                        "if-expressions are available only in Blu and Luau",
                        &["expression"],
                    )?;
                    self.bump();
                    return Ok(None);
                }
                self.bump();
                let Some(expression) = self.parse_if_expression(token)? else {
                    return Ok(None);
                };
                return self.parse_postfix(expression).map(Some);
            }
            TokenKind::Function => {
                self.bump();
                let Some(function) = self.parse_function_body(token)? else {
                    return Ok(None);
                };
                let Some(body) = self.functions.get(function.as_usize()) else {
                    return Err(ParseError::InternalInvariant {
                        message: "new function body is out of bounds",
                    });
                };
                let expression = self.push_expression(
                    Expression::new(
                        ExpressionKind::Function(FunctionExpression::new(function, body.span())),
                        body.span(),
                    ),
                    1,
                )?;
                return self.parse_postfix(expression).map(Some);
            }
            TokenKind::LeftBrace => {
                let table = self.parse_table_constructor(token)?;
                return self.parse_postfix(table).map(Some);
            }
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
                let group = self.push_expression(
                    Expression::new(ExpressionKind::Group(inner.id), span),
                    inner.depth.saturating_add(1),
                )?;
                return self.parse_postfix(group).map(Some);
            }
            _ => {
                self.report_current(
                    "BLU-PARSE-0004",
                    "expected an expression",
                    &[
                        "nil",
                        "boolean",
                        "quoted string",
                        "decimal number",
                        "identifier",
                        "not",
                        "(",
                    ],
                )?;
                return Ok(None);
            }
        };
        self.bump();
        let primary = self.push_expression(Expression::new(kind, token.span()), 1)?;
        self.parse_postfix(primary).map(Some)
    }

    fn parse_if_expression(
        &mut self,
        keyword: Token,
    ) -> Result<Option<BuiltExpression>, ParseError> {
        let Some(condition) = self.parse_expression(0)? else {
            return Ok(None);
        };
        if !self.at(TokenKind::Then) {
            self.report_current_or_eof(
                "BLU-PARSE-0047",
                "expected `then` after if-expression condition",
                &["then"],
            )?;
            return Ok(None);
        }
        self.bump();
        let Some(then_value) = self.parse_expression(0)? else {
            return Ok(None);
        };
        let else_value = if self.at(TokenKind::ElseIf) {
            let Some(elseif) = self.bump() else {
                return Err(ParseError::InternalInvariant {
                    message: "elseif check succeeded without a current token",
                });
            };
            let Some(value) = self.parse_if_expression(elseif)? else {
                return Ok(None);
            };
            value
        } else {
            if !self.at(TokenKind::Else) {
                self.report_current_or_eof(
                    "BLU-PARSE-0048",
                    "expected `else` in if-expression",
                    &["elseif", "else"],
                )?;
                return Ok(None);
            }
            self.bump();
            let Some(value) = self.parse_expression(0)? else {
                return Ok(None);
            };
            value
        };
        let depth = condition
            .depth
            .max(then_value.depth)
            .max(else_value.depth)
            .saturating_add(1);
        let span = keyword
            .span()
            .merge(self.expression(else_value.id)?.span())?;
        self.push_expression(
            Expression::new(
                ExpressionKind::If(IfExpression::new(
                    condition.id,
                    then_value.id,
                    else_value.id,
                )),
                span,
            ),
            depth,
        )
        .map(Some)
    }

    fn parse_postfix(
        &mut self,
        mut expression: BuiltExpression,
    ) -> Result<BuiltExpression, ParseError> {
        loop {
            let kind = self.expression(expression.id)?.kind();
            if supports_type_annotations(self.lexed.profile())
                && self.at(TokenKind::ColonColon)
                && !self.at_label_token_sequence()
            {
                // Luau's runtime type assertions are erased by the owned
                // frontend. Consume the assertion while retaining the value
                // expression and continue parsing any following postfix.
                self.skip_type_assertion()?;
                continue;
            }
            if self.at(TokenKind::LeftBracket) {
                if !is_postfix_expression(kind) {
                    break;
                }
                self.bump();
                let Some(key) = self.parse_expression(0)? else {
                    return Ok(expression);
                };
                let Some(close) = self
                    .current()
                    .filter(|token| token.kind() == TokenKind::RightBracket)
                else {
                    self.report_current_or_eof(
                        "BLU-PARSE-0024",
                        "expected `]` after table key",
                        &["]"],
                    )?;
                    return Ok(expression);
                };
                self.bump();
                let depth = expression.depth.max(key.depth).saturating_add(1);
                let span = self.expression(expression.id)?.span().merge(close.span())?;
                expression = self.push_expression(
                    Expression::new(
                        ExpressionKind::Index(IndexExpression::new(expression.id, key.id, span)),
                        span,
                    ),
                    depth,
                )?;
            } else if self.at(TokenKind::Dot) {
                if !is_postfix_expression(kind) {
                    break;
                }
                self.bump();
                let Some(name) = self
                    .current()
                    .filter(|token| token.kind() == TokenKind::Identifier)
                else {
                    self.report_current_or_eof(
                        "BLU-PARSE-0027",
                        "expected a field name after `.`",
                        &["identifier"],
                    )?;
                    return Ok(expression);
                };
                self.bump();
                let span = self.expression(expression.id)?.span().merge(name.span())?;
                expression = self.push_expression(
                    Expression::new(
                        ExpressionKind::Field(FieldExpression::new(
                            expression.id,
                            Identifier::new(name.span()),
                            span,
                        )),
                        span,
                    ),
                    expression.depth.saturating_add(1),
                )?;
            } else if self.at(TokenKind::Colon) {
                if !is_postfix_expression(kind) {
                    break;
                }
                self.bump();
                let Some(name) = self
                    .current()
                    .filter(|token| token.kind() == TokenKind::Identifier)
                else {
                    self.report_current_or_eof(
                        "BLU-PARSE-0032",
                        "expected a method name after `:`",
                        &["identifier"],
                    )?;
                    return Ok(expression);
                };
                self.bump();
                let mut arguments = allocate_vec(2, "method call arguments")?;
                let mut depth = expression.depth;
                let end_span = if self.at(TokenKind::LeftParenthesis) {
                    self.bump();
                    if !self.at(TokenKind::RightParenthesis) {
                        loop {
                            let Some(argument) = self.parse_expression(0)? else {
                                return Ok(expression);
                            };
                            self.check_ast_limit()?;
                            self.call_argument_count = self.call_argument_count.saturating_add(1);
                            push_fallible(&mut arguments, argument.id, "method call arguments")?;
                            depth = depth.max(argument.depth);
                            if !self.at(TokenKind::Comma) {
                                break;
                            }
                            self.bump();
                        }
                    }
                    let Some(close) = self
                        .current()
                        .filter(|token| token.kind() == TokenKind::RightParenthesis)
                    else {
                        self.report_current_or_eof(
                            "BLU-PARSE-0031",
                            "expected `)` after call arguments",
                            &[")"],
                        )?;
                        return Ok(expression);
                    };
                    self.bump();
                    close.span()
                } else if self.at(TokenKind::LeftBrace) {
                    let Some(open) = self.current() else {
                        return Ok(expression);
                    };
                    let argument = self.parse_table_constructor(open)?;
                    self.check_ast_limit()?;
                    self.call_argument_count = self.call_argument_count.saturating_add(1);
                    push_fallible(&mut arguments, argument.id, "method call arguments")?;
                    depth = depth.max(argument.depth);
                    self.expression(argument.id)?.span()
                } else if self.at(TokenKind::StringLiteral) {
                    let Some(token) = self.current() else {
                        return Ok(expression);
                    };
                    self.bump();
                    let argument = self.push_expression(
                        Expression::new(ExpressionKind::StringLiteral, token.span()),
                        1,
                    )?;
                    self.check_ast_limit()?;
                    self.call_argument_count = self.call_argument_count.saturating_add(1);
                    push_fallible(&mut arguments, argument.id, "method call arguments")?;
                    depth = depth.max(argument.depth);
                    self.expression(argument.id)?.span()
                } else {
                    self.report_current_or_eof(
                        "BLU-PARSE-0033",
                        "expected `(`, table, or string argument after method name",
                        &["(", "table", "string literal"],
                    )?;
                    return Ok(expression);
                };
                let first_argument = self.call_arguments.len();
                let argument_count = arguments.len();
                for argument in arguments {
                    push_fallible(&mut self.call_arguments, argument, "AST call arguments")?;
                }
                let span = self.expression(expression.id)?.span().merge(end_span)?;
                expression = self.push_expression(
                    Expression::new(
                        ExpressionKind::MethodCall(MethodCallExpression::new(
                            expression.id,
                            Identifier::new(name.span()),
                            first_argument,
                            argument_count,
                            span,
                        )),
                        span,
                    ),
                    depth.saturating_add(1),
                )?;
            } else if self.at(TokenKind::LeftParenthesis) {
                if !is_postfix_expression(kind) {
                    break;
                }
                if self.lexed.profile() == SemanticProfile::Luau && self.current_has_line_break() {
                    self.report_current(
                        "BLU-PARSE-0057",
                        "a line break before call arguments is unavailable in the Luau profile",
                        &["call on the same line"],
                    )?;
                    return Ok(expression);
                }
                if matches!(kind, ExpressionKind::Table(_))
                    && self.parenthesized_prefix_starts_assignment()
                {
                    // A newline after a table constructor starts a new
                    // Luau prefix-assignment statement in constructs such as
                    // `local t = {}\n(t)[key] = value`, rather than a call on
                    // the constructor itself.
                    break;
                }
                self.bump();
                let mut arguments = allocate_vec(2, "call arguments")?;
                let mut depth = expression.depth;
                if !self.at(TokenKind::RightParenthesis) {
                    loop {
                        let Some(argument) = self.parse_expression(0)? else {
                            return Ok(expression);
                        };
                        self.check_ast_limit()?;
                        self.call_argument_count = self.call_argument_count.saturating_add(1);
                        push_fallible(&mut arguments, argument.id, "call arguments")?;
                        depth = depth.max(argument.depth);
                        if !self.at(TokenKind::Comma) {
                            break;
                        }
                        self.bump();
                    }
                }
                let Some(close) = self
                    .current()
                    .filter(|token| token.kind() == TokenKind::RightParenthesis)
                else {
                    self.report_current_or_eof(
                        "BLU-PARSE-0031",
                        "expected `)` after call arguments",
                        &[")"],
                    )?;
                    return Ok(expression);
                };
                self.bump();
                let first_argument = self.call_arguments.len();
                let argument_count = arguments.len();
                for argument in arguments {
                    push_fallible(&mut self.call_arguments, argument, "AST call arguments")?;
                }
                let span = self.expression(expression.id)?.span().merge(close.span())?;
                expression = self.push_expression(
                    Expression::new(
                        ExpressionKind::Call(CallExpression::new(
                            expression.id,
                            first_argument,
                            argument_count,
                            span,
                        )),
                        span,
                    ),
                    depth.saturating_add(1),
                )?;
            } else if self.at(TokenKind::LeftBrace) {
                if !is_postfix_expression(kind) {
                    break;
                }
                let Some(open) = self.current() else {
                    return Ok(expression);
                };
                let argument = self.parse_table_constructor(open)?;
                self.check_ast_limit()?;
                self.call_argument_count = self.call_argument_count.saturating_add(1);
                let first_argument = self.call_arguments.len();
                push_fallible(&mut self.call_arguments, argument.id, "call arguments")?;
                let span = self
                    .expression(expression.id)?
                    .span()
                    .merge(self.expression(argument.id)?.span())?;
                expression = self.push_expression(
                    Expression::new(
                        ExpressionKind::Call(CallExpression::new(
                            expression.id,
                            first_argument,
                            1,
                            span,
                        )),
                        span,
                    ),
                    expression.depth.max(argument.depth).saturating_add(1),
                )?;
            } else if self.at(TokenKind::StringLiteral) {
                if !is_postfix_expression(kind) {
                    break;
                }
                let Some(token) = self.current() else {
                    return Ok(expression);
                };
                self.bump();
                let argument = self.push_expression(
                    Expression::new(ExpressionKind::StringLiteral, token.span()),
                    1,
                )?;
                self.check_ast_limit()?;
                self.call_argument_count = self.call_argument_count.saturating_add(1);
                let first_argument = self.call_arguments.len();
                push_fallible(&mut self.call_arguments, argument.id, "call arguments")?;
                let span = self
                    .expression(expression.id)?
                    .span()
                    .merge(self.expression(argument.id)?.span())?;
                expression = self.push_expression(
                    Expression::new(
                        ExpressionKind::Call(CallExpression::new(
                            expression.id,
                            first_argument,
                            1,
                            span,
                        )),
                        span,
                    ),
                    expression.depth.max(argument.depth).saturating_add(1),
                )?;
            } else {
                break;
            }
        }
        Ok(expression)
    }

    fn at_label_token_sequence(&self) -> bool {
        self.significant_kind_after_cursor(0) == Some(TokenKind::ColonColon)
            && self.significant_kind_after_cursor(1) == Some(TokenKind::Identifier)
            && self.significant_kind_after_cursor(2) == Some(TokenKind::ColonColon)
    }

    fn significant_kind_after_cursor(&self, wanted: usize) -> Option<TokenKind> {
        let mut seen = 0;
        for token in self.lexed.tokens().get(self.cursor..)?.iter().copied() {
            if is_trivia(token.kind()) {
                continue;
            }
            if seen == wanted {
                return Some(token.kind());
            }
            seen += 1;
        }
        None
    }

    fn parenthesized_prefix_starts_assignment(&self) -> bool {
        let tokens = self.lexed.tokens();
        let mut index = self.cursor;
        let mut line_break = false;
        while tokens
            .get(index)
            .is_some_and(|token| is_trivia(token.kind()))
        {
            if let Some(token) = tokens.get(index) {
                line_break |= self
                    .source
                    .slice(token.span())
                    .ok()
                    .is_some_and(|bytes| bytes.contains(&b'\n') || bytes.contains(&b'\r'));
            }
            index += 1;
        }
        if !line_break {
            return false;
        }
        if tokens.get(index).map(|token| token.kind()) != Some(TokenKind::LeftParenthesis) {
            return false;
        }
        let mut depth = 0usize;
        while let Some(token) = tokens.get(index) {
            match token.kind() {
                TokenKind::LeftParenthesis => depth = depth.saturating_add(1),
                TokenKind::RightParenthesis => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        index += 1;
                        while tokens.get(index).is_some_and(|next| is_trivia(next.kind())) {
                            index += 1;
                        }
                        if tokens
                            .get(index)
                            .is_some_and(|next| next.kind() == TokenKind::LeftBracket)
                        {
                            let mut bracket_depth = 0usize;
                            while let Some(next) = tokens.get(index) {
                                match next.kind() {
                                    TokenKind::LeftBracket => {
                                        bracket_depth = bracket_depth.saturating_add(1)
                                    }
                                    TokenKind::RightBracket => {
                                        bracket_depth = bracket_depth.saturating_sub(1);
                                        if bracket_depth == 0 {
                                            index += 1;
                                            break;
                                        }
                                    }
                                    _ => {}
                                }
                                index += 1;
                            }
                        } else if tokens
                            .get(index)
                            .is_some_and(|next| next.kind() == TokenKind::Dot)
                        {
                            index = index.saturating_add(2);
                        }
                        while tokens.get(index).is_some_and(|next| is_trivia(next.kind())) {
                            index += 1;
                        }
                        return tokens.get(index).is_some_and(|next| {
                            next.kind() == TokenKind::Equal
                                || compound_assignment_operator(Some(next.kind())).is_some()
                        });
                    }
                }
                _ => {}
            }
            index += 1;
        }
        false
    }

    fn parse_table_constructor(&mut self, open: Token) -> Result<BuiltExpression, ParseError> {
        self.bump();
        let mut fields = allocate_vec(4, "table constructor fields")?;
        let mut depth = 0usize;
        while !self.at(TokenKind::RightBrace) {
            if self.current().is_none() {
                self.report_current_or_eof(
                    "BLU-PARSE-0025",
                    "expected `}` after table constructor",
                    &["}"],
                )?;
                break;
            }
            let (field, field_depth) = if self.at(TokenKind::LeftBracket) {
                self.bump();
                let Some(key) = self.parse_expression(0)? else {
                    break;
                };
                if !self.at(TokenKind::RightBracket) {
                    self.report_current_or_eof(
                        "BLU-PARSE-0024",
                        "expected `]` after table key",
                        &["]"],
                    )?;
                    break;
                }
                self.bump();
                if !self.at(TokenKind::Equal) {
                    self.report_current_or_eof(
                        "BLU-PARSE-0028",
                        "expected `=` after indexed table field",
                        &["="],
                    )?;
                    break;
                }
                self.bump();
                let Some(value) = self.parse_expression(0)? else {
                    break;
                };
                (
                    TableField::Indexed {
                        key: key.id,
                        value: value.id,
                    },
                    key.depth.max(value.depth),
                )
            } else {
                let Some(value_or_name) = self.parse_expression(0)? else {
                    break;
                };
                if self.at(TokenKind::Equal) {
                    let ExpressionKind::Identifier(name) =
                        self.expression(value_or_name.id)?.kind()
                    else {
                        self.report_current(
                            "BLU-PARSE-0029",
                            "named table field must be an identifier",
                            &["identifier"],
                        )?;
                        break;
                    };
                    self.bump();
                    let Some(value) = self.parse_expression(0)? else {
                        break;
                    };
                    (
                        TableField::Named {
                            name,
                            value: value.id,
                        },
                        value_or_name.depth.max(value.depth),
                    )
                } else {
                    (TableField::Array(value_or_name.id), value_or_name.depth)
                }
            };
            self.check_ast_limit()?;
            self.table_field_count = self.table_field_count.saturating_add(1);
            push_fallible(&mut fields, field, "table constructor fields")?;
            depth = depth.max(field_depth);
            if self.at(TokenKind::Comma) || self.at(TokenKind::Semicolon) {
                self.bump();
            } else if !self.at(TokenKind::RightBrace) {
                self.report_current_or_eof(
                    "BLU-PARSE-0030",
                    "expected a table field separator",
                    &[",", ";", "}"],
                )?;
                break;
            }
        }
        let Some(close) = self
            .current()
            .filter(|token| token.kind() == TokenKind::RightBrace)
        else {
            return self.push_expression(
                Expression::new(
                    ExpressionKind::Table(TableConstructor::new(self.table_fields.len(), 0)),
                    open.span(),
                ),
                1,
            );
        };
        self.bump();
        let first_field = self.table_fields.len();
        let field_count = fields.len();
        for field in fields {
            push_fallible(&mut self.table_fields, field, "AST table fields")?;
        }
        let span = open.span().merge(close.span())?;
        self.push_expression(
            Expression::new(
                ExpressionKind::Table(TableConstructor::new(first_field, field_count)),
                span,
            ),
            depth.saturating_add(1),
        )
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
        push_fallible(&mut self.statements, statement, "AST statements")?;
        self.statement_count += 1;
        Ok(())
    }

    fn check_ast_limit(&self) -> Result<(), ParseError> {
        let required = self
            .statement_count
            .saturating_add(self.expressions.len())
            .saturating_add(self.table_field_count)
            .saturating_add(self.interpolated_parts.len())
            .saturating_add(self.call_argument_count)
            .saturating_add(self.function_node_count)
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

    fn previous_significant_kind(&self) -> Option<TokenKind> {
        self.lexed
            .tokens()
            .get(..self.cursor)?
            .iter()
            .rev()
            .copied()
            .find(|token| !is_trivia(token.kind()))
            .map(Token::kind)
    }

    fn current_has_line_break(&self) -> bool {
        self.lexed
            .tokens()
            .get(self.cursor..)
            .unwrap_or_default()
            .iter()
            .take_while(|token| is_trivia(token.kind()))
            .filter_map(|token| self.source.slice(token.span()).ok())
            .any(|bytes| bytes.contains(&b'\n') || bytes.contains(&b'\r'))
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

fn binary_operator(kind: TokenKind) -> Option<(BinaryOperator, u8, u8)> {
    match kind {
        TokenKind::Or => Some((BinaryOperator::Or, 1, 2)),
        TokenKind::And => Some((BinaryOperator::And, 2, 3)),
        TokenKind::EqualEqual => Some((BinaryOperator::Equal, 3, 4)),
        TokenKind::NotEqual => Some((BinaryOperator::NotEqual, 3, 4)),
        TokenKind::LessThan => Some((BinaryOperator::LessThan, 3, 4)),
        TokenKind::LessEqual => Some((BinaryOperator::LessEqual, 3, 4)),
        TokenKind::GreaterThan => Some((BinaryOperator::GreaterThan, 3, 4)),
        TokenKind::GreaterEqual => Some((BinaryOperator::GreaterEqual, 3, 4)),
        TokenKind::BitwiseOr => Some((BinaryOperator::BitwiseOr, 4, 5)),
        TokenKind::BitwiseExclusiveOr => Some((BinaryOperator::BitwiseExclusiveOr, 5, 6)),
        TokenKind::BitwiseAnd => Some((BinaryOperator::BitwiseAnd, 6, 7)),
        TokenKind::ShiftLeft => Some((BinaryOperator::ShiftLeft, 7, 8)),
        TokenKind::ShiftRight => Some((BinaryOperator::ShiftRight, 7, 8)),
        TokenKind::Concatenate => Some((BinaryOperator::Concatenate, 8, 8)),
        TokenKind::Plus => Some((BinaryOperator::Add, 9, 10)),
        TokenKind::Minus => Some((BinaryOperator::Subtract, 9, 10)),
        TokenKind::Star => Some((BinaryOperator::Multiply, 10, 11)),
        TokenKind::Slash => Some((BinaryOperator::Divide, 10, 11)),
        TokenKind::Percent => Some((BinaryOperator::Modulo, 10, 11)),
        TokenKind::FloorDivide => Some((BinaryOperator::FloorDivide, 10, 11)),
        TokenKind::Caret => Some((BinaryOperator::Power, 12, 12)),
        _ => None,
    }
}

fn compound_assignment_operator(kind: Option<TokenKind>) -> Option<CompoundAssignmentOperator> {
    match kind? {
        TokenKind::PlusEqual => Some(CompoundAssignmentOperator::Add),
        TokenKind::MinusEqual => Some(CompoundAssignmentOperator::Subtract),
        TokenKind::StarEqual => Some(CompoundAssignmentOperator::Multiply),
        TokenKind::SlashEqual => Some(CompoundAssignmentOperator::Divide),
        TokenKind::FloorDivideEqual => Some(CompoundAssignmentOperator::FloorDivide),
        TokenKind::PercentEqual => Some(CompoundAssignmentOperator::Modulo),
        TokenKind::CaretEqual => Some(CompoundAssignmentOperator::Power),
        TokenKind::ConcatenateEqual => Some(CompoundAssignmentOperator::Concatenate),
        _ => None,
    }
}

fn is_trivia(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Whitespace | TokenKind::Comment | TokenKind::DialectDirective
    )
}

fn is_postfix_expression(kind: ExpressionKind) -> bool {
    matches!(
        kind,
        ExpressionKind::Identifier(_)
            | ExpressionKind::Group(_)
            | ExpressionKind::Index(_)
            | ExpressionKind::Field(_)
            | ExpressionKind::Call(_)
            | ExpressionKind::MethodCall(_)
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
