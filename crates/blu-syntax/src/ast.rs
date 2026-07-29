use blu_core::{ByteSpan, SemanticProfile};

/// Index of an expression in an [`Ast`]'s expression arena.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ExpressionId(usize);

impl ExpressionId {
    #[must_use]
    pub const fn as_usize(self) -> usize {
        self.0
    }

    pub(crate) const fn new(value: usize) -> Self {
        Self(value)
    }
}

/// A parsed identifier. Its spelling remains in the source bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Identifier {
    span: ByteSpan,
}

impl Identifier {
    pub(crate) const fn new(span: ByteSpan) -> Self {
        Self { span }
    }

    #[must_use]
    pub const fn span(self) -> ByteSpan {
        self.span
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum BinaryOperator {
    Add,
    FloorDivide,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum UnaryOperator {
    Not,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UnaryExpression {
    operator: UnaryOperator,
    operator_span: ByteSpan,
    operand: ExpressionId,
}

impl UnaryExpression {
    pub(crate) const fn new(
        operator: UnaryOperator,
        operator_span: ByteSpan,
        operand: ExpressionId,
    ) -> Self {
        Self {
            operator,
            operator_span,
            operand,
        }
    }

    #[must_use]
    pub const fn operator(self) -> UnaryOperator {
        self.operator
    }

    #[must_use]
    pub const fn operator_span(self) -> ByteSpan {
        self.operator_span
    }

    #[must_use]
    pub const fn operand(self) -> ExpressionId {
        self.operand
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BinaryExpression {
    left: ExpressionId,
    operator: BinaryOperator,
    operator_span: ByteSpan,
    right: ExpressionId,
}

impl BinaryExpression {
    pub(crate) const fn new(
        left: ExpressionId,
        operator: BinaryOperator,
        operator_span: ByteSpan,
        right: ExpressionId,
    ) -> Self {
        Self {
            left,
            operator,
            operator_span,
            right,
        }
    }

    #[must_use]
    pub const fn left(self) -> ExpressionId {
        self.left
    }

    #[must_use]
    pub const fn operator(self) -> BinaryOperator {
        self.operator
    }

    #[must_use]
    pub const fn operator_span(self) -> ByteSpan {
        self.operator_span
    }

    #[must_use]
    pub const fn right(self) -> ExpressionId {
        self.right
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ExpressionKind {
    Nil,
    Boolean(bool),
    DecimalInteger,
    StringLiteral,
    Identifier(Identifier),
    Group(ExpressionId),
    Unary(UnaryExpression),
    Binary(BinaryExpression),
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Expression {
    kind: ExpressionKind,
    span: ByteSpan,
}

impl Expression {
    pub(crate) const fn new(kind: ExpressionKind, span: ByteSpan) -> Self {
        Self { kind, span }
    }

    #[must_use]
    pub const fn kind(self) -> ExpressionKind {
        self.kind
    }

    #[must_use]
    pub const fn span(self) -> ByteSpan {
        self.span
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LocalStatement {
    name: Identifier,
    value: ExpressionId,
    span: ByteSpan,
}

impl LocalStatement {
    pub(crate) const fn new(name: Identifier, value: ExpressionId, span: ByteSpan) -> Self {
        Self { name, value, span }
    }

    #[must_use]
    pub const fn name(self) -> Identifier {
        self.name
    }

    #[must_use]
    pub const fn value(self) -> ExpressionId {
        self.value
    }

    #[must_use]
    pub const fn span(self) -> ByteSpan {
        self.span
    }
}

#[derive(Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReturnStatement {
    values: Vec<ExpressionId>,
    span: ByteSpan,
}

impl ReturnStatement {
    pub(crate) const fn new(values: Vec<ExpressionId>, span: ByteSpan) -> Self {
        Self { values, span }
    }

    #[must_use]
    pub fn values(&self) -> &[ExpressionId] {
        &self.values
    }

    #[must_use]
    pub const fn span(&self) -> ByteSpan {
        self.span
    }
}

#[derive(Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Statement {
    Local(LocalStatement),
    Return(ReturnStatement),
}

impl Statement {
    #[must_use]
    pub const fn span(&self) -> ByteSpan {
        match self {
            Self::Local(statement) => statement.span(),
            Self::Return(statement) => statement.span(),
        }
    }
}

/// Spanned AST for the currently supported parser slice.
///
/// Expression children are arena indices so construction does not depend on
/// infallible recursive heap allocation.
#[derive(Debug, Eq, PartialEq)]
pub struct Ast {
    profile: SemanticProfile,
    span: ByteSpan,
    statements: Vec<Statement>,
    expressions: Vec<Expression>,
}

impl Ast {
    pub(crate) const fn new(
        profile: SemanticProfile,
        span: ByteSpan,
        statements: Vec<Statement>,
        expressions: Vec<Expression>,
    ) -> Self {
        Self {
            profile,
            span,
            statements,
            expressions,
        }
    }

    #[must_use]
    pub const fn profile(&self) -> SemanticProfile {
        self.profile
    }

    #[must_use]
    pub const fn span(&self) -> ByteSpan {
        self.span
    }

    #[must_use]
    pub fn statements(&self) -> &[Statement] {
        &self.statements
    }

    #[must_use]
    pub fn expressions(&self) -> &[Expression] {
        &self.expressions
    }

    #[must_use]
    pub fn expression(&self, id: ExpressionId) -> Option<&Expression> {
        self.expressions.get(id.as_usize())
    }

    #[must_use]
    pub fn node_count(&self) -> usize {
        self.statements.len().saturating_add(self.expressions.len())
    }
}
