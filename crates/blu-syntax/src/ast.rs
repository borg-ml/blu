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
    And,
    Or,
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
    Power,
    FloorDivide,
    Concatenate,
    Equal,
    NotEqual,
    LessThan,
    LessEqual,
    GreaterThan,
    GreaterEqual,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum UnaryOperator {
    Not,
    Negate,
    Length,
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
    DecimalNumber,
    HexInteger,
    HexNumber,
    BinaryInteger,
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
    value: Option<ExpressionId>,
    span: ByteSpan,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AssignmentStatement {
    target: Identifier,
    value: ExpressionId,
    span: ByteSpan,
}

impl AssignmentStatement {
    pub(crate) const fn new(target: Identifier, value: ExpressionId, span: ByteSpan) -> Self {
        Self {
            target,
            value,
            span,
        }
    }

    #[must_use]
    pub const fn target(self) -> Identifier {
        self.target
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
pub struct AssignmentListStatement {
    targets: Vec<Identifier>,
    values: Vec<ExpressionId>,
    span: ByteSpan,
}

impl AssignmentListStatement {
    pub(crate) const fn new(
        targets: Vec<Identifier>,
        values: Vec<ExpressionId>,
        span: ByteSpan,
    ) -> Self {
        Self {
            targets,
            values,
            span,
        }
    }

    #[must_use]
    pub fn targets(&self) -> &[Identifier] {
        &self.targets
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

impl LocalStatement {
    pub(crate) const fn new(name: Identifier, value: Option<ExpressionId>, span: ByteSpan) -> Self {
        Self { name, value, span }
    }

    #[must_use]
    pub const fn name(self) -> Identifier {
        self.name
    }

    #[must_use]
    pub const fn value(self) -> Option<ExpressionId> {
        self.value
    }

    #[must_use]
    pub const fn span(self) -> ByteSpan {
        self.span
    }
}

#[derive(Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LocalListStatement {
    names: Vec<Identifier>,
    values: Vec<ExpressionId>,
    span: ByteSpan,
}

impl LocalListStatement {
    pub(crate) const fn new(
        names: Vec<Identifier>,
        values: Vec<ExpressionId>,
        span: ByteSpan,
    ) -> Self {
        Self {
            names,
            values,
            span,
        }
    }

    #[must_use]
    pub fn names(&self) -> &[Identifier] {
        &self.names
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
pub struct IfClause {
    condition: ExpressionId,
    body: Block,
    span: ByteSpan,
}

impl IfClause {
    pub(crate) const fn new(condition: ExpressionId, body: Block, span: ByteSpan) -> Self {
        Self {
            condition,
            body,
            span,
        }
    }

    #[must_use]
    pub const fn condition(&self) -> ExpressionId {
        self.condition
    }

    #[must_use]
    pub const fn body(&self) -> &Block {
        &self.body
    }

    #[must_use]
    pub const fn span(&self) -> ByteSpan {
        self.span
    }
}

#[derive(Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct IfStatement {
    clauses: Vec<IfClause>,
    else_body: Option<Block>,
    span: ByteSpan,
}

impl IfStatement {
    pub(crate) const fn new(
        clauses: Vec<IfClause>,
        else_body: Option<Block>,
        span: ByteSpan,
    ) -> Self {
        Self {
            clauses,
            else_body,
            span,
        }
    }

    #[must_use]
    pub fn clauses(&self) -> &[IfClause] {
        &self.clauses
    }

    #[must_use]
    pub const fn else_body(&self) -> Option<&Block> {
        self.else_body.as_ref()
    }

    #[must_use]
    pub const fn span(&self) -> ByteSpan {
        self.span
    }
}

#[derive(Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WhileStatement {
    condition: ExpressionId,
    body: Block,
    span: ByteSpan,
}

impl WhileStatement {
    pub(crate) const fn new(condition: ExpressionId, body: Block, span: ByteSpan) -> Self {
        Self {
            condition,
            body,
            span,
        }
    }

    #[must_use]
    pub const fn condition(&self) -> ExpressionId {
        self.condition
    }

    #[must_use]
    pub const fn body(&self) -> &Block {
        &self.body
    }

    #[must_use]
    pub const fn span(&self) -> ByteSpan {
        self.span
    }
}

#[derive(Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RepeatStatement {
    body: Block,
    condition: ExpressionId,
    span: ByteSpan,
}

impl RepeatStatement {
    pub(crate) const fn new(body: Block, condition: ExpressionId, span: ByteSpan) -> Self {
        Self {
            body,
            condition,
            span,
        }
    }

    #[must_use]
    pub const fn body(&self) -> &Block {
        &self.body
    }

    #[must_use]
    pub const fn condition(&self) -> ExpressionId {
        self.condition
    }

    #[must_use]
    pub const fn span(&self) -> ByteSpan {
        self.span
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BreakStatement {
    span: ByteSpan,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContinueStatement {
    span: ByteSpan,
}

impl ContinueStatement {
    pub(crate) const fn new(span: ByteSpan) -> Self {
        Self { span }
    }

    #[must_use]
    pub const fn span(self) -> ByteSpan {
        self.span
    }
}

impl BreakStatement {
    pub(crate) const fn new(span: ByteSpan) -> Self {
        Self { span }
    }

    #[must_use]
    pub const fn span(self) -> ByteSpan {
        self.span
    }
}

#[derive(Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Statement {
    Local(LocalStatement),
    LocalList(LocalListStatement),
    Assignment(AssignmentStatement),
    AssignmentList(AssignmentListStatement),
    If(IfStatement),
    While(WhileStatement),
    Repeat(RepeatStatement),
    Break(BreakStatement),
    Continue(ContinueStatement),
    Return(ReturnStatement),
}

impl Statement {
    #[must_use]
    pub const fn span(&self) -> ByteSpan {
        match self {
            Self::Local(statement) => statement.span(),
            Self::LocalList(statement) => statement.span(),
            Self::Assignment(statement) => statement.span(),
            Self::AssignmentList(statement) => statement.span(),
            Self::If(statement) => statement.span(),
            Self::While(statement) => statement.span(),
            Self::Repeat(statement) => statement.span(),
            Self::Break(statement) => statement.span(),
            Self::Continue(statement) => statement.span(),
            Self::Return(statement) => statement.span(),
        }
    }
}

/// An owned lexical statement block.
///
/// Blocks are explicit AST nodes so structured statements can own nested
/// bodies without flattening scope or control-flow boundaries.
#[derive(Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Block {
    statements: Vec<Statement>,
}

impl Block {
    pub(crate) const fn new(statements: Vec<Statement>) -> Self {
        Self { statements }
    }

    #[must_use]
    pub fn statements(&self) -> &[Statement] {
        &self.statements
    }

    #[must_use]
    pub fn node_count(&self) -> usize {
        self.statements.iter().fold(0_usize, |count, statement| {
            let nested = match statement {
                Statement::If(statement) => {
                    let clauses = statement.clauses().iter().fold(0_usize, |count, clause| {
                        count.saturating_add(clause.body().node_count())
                    });
                    clauses
                        .saturating_add(statement.else_body().map_or(0, |block| block.node_count()))
                }
                Statement::While(statement) => statement.body().node_count(),
                Statement::Repeat(statement) => statement.body().node_count(),
                _ => 0,
            };
            count.saturating_add(1).saturating_add(nested)
        })
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
    block: Block,
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
            block: Block::new(statements),
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
        self.block.statements()
    }

    #[must_use]
    pub const fn block(&self) -> &Block {
        &self.block
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
        self.block
            .node_count()
            .saturating_add(self.expressions.len())
    }
}
