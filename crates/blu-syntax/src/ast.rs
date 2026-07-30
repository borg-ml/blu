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
pub struct IndexExpression {
    table: ExpressionId,
    key: ExpressionId,
    span: ByteSpan,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FieldExpression {
    table: ExpressionId,
    name: Identifier,
    span: ByteSpan,
}

impl FieldExpression {
    pub(crate) const fn new(table: ExpressionId, name: Identifier, span: ByteSpan) -> Self {
        Self { table, name, span }
    }

    #[must_use]
    pub const fn table(self) -> ExpressionId {
        self.table
    }

    #[must_use]
    pub const fn name(self) -> Identifier {
        self.name
    }

    #[must_use]
    pub const fn span(self) -> ByteSpan {
        self.span
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TableConstructor {
    first_field: usize,
    field_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CallExpression {
    function: ExpressionId,
    first_argument: usize,
    argument_count: usize,
    span: ByteSpan,
}

impl CallExpression {
    pub(crate) const fn new(
        function: ExpressionId,
        first_argument: usize,
        argument_count: usize,
        span: ByteSpan,
    ) -> Self {
        Self {
            function,
            first_argument,
            argument_count,
            span,
        }
    }

    #[must_use]
    pub const fn function(self) -> ExpressionId {
        self.function
    }

    #[must_use]
    pub const fn first_argument(self) -> usize {
        self.first_argument
    }

    #[must_use]
    pub const fn argument_count(self) -> usize {
        self.argument_count
    }

    #[must_use]
    pub const fn span(self) -> ByteSpan {
        self.span
    }
}

impl TableConstructor {
    pub(crate) const fn new(first_field: usize, field_count: usize) -> Self {
        Self {
            first_field,
            field_count,
        }
    }

    #[must_use]
    pub const fn first_field(self) -> usize {
        self.first_field
    }

    #[must_use]
    pub const fn field_count(self) -> usize {
        self.field_count
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TableField {
    Array(ExpressionId),
    Named {
        name: Identifier,
        value: ExpressionId,
    },
    Indexed {
        key: ExpressionId,
        value: ExpressionId,
    },
}

impl IndexExpression {
    pub(crate) const fn new(table: ExpressionId, key: ExpressionId, span: ByteSpan) -> Self {
        Self { table, key, span }
    }

    #[must_use]
    pub const fn table(self) -> ExpressionId {
        self.table
    }

    #[must_use]
    pub const fn key(self) -> ExpressionId {
        self.key
    }

    #[must_use]
    pub const fn span(self) -> ByteSpan {
        self.span
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
    Table(TableConstructor),
    Identifier(Identifier),
    Group(ExpressionId),
    Index(IndexExpression),
    Field(FieldExpression),
    Call(CallExpression),
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
    target: AssignmentTarget,
    value: ExpressionId,
    span: ByteSpan,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CallStatement {
    call: ExpressionId,
    span: ByteSpan,
}

impl CallStatement {
    pub(crate) const fn new(call: ExpressionId, span: ByteSpan) -> Self {
        Self { call, span }
    }

    #[must_use]
    pub const fn call(self) -> ExpressionId {
        self.call
    }

    #[must_use]
    pub const fn span(self) -> ByteSpan {
        self.span
    }
}

impl AssignmentStatement {
    pub(crate) const fn new(target: AssignmentTarget, value: ExpressionId, span: ByteSpan) -> Self {
        Self {
            target,
            value,
            span,
        }
    }

    #[must_use]
    pub const fn target(self) -> AssignmentTarget {
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

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AssignmentTarget {
    Identifier(Identifier),
    Index(IndexExpression),
    Field(FieldExpression),
}

impl AssignmentTarget {
    #[must_use]
    pub const fn span(self) -> ByteSpan {
        match self {
            Self::Identifier(identifier) => identifier.span(),
            Self::Index(index) => index.span(),
            Self::Field(field) => field.span(),
        }
    }
}

#[derive(Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AssignmentListStatement {
    targets: Vec<AssignmentTarget>,
    values: Vec<ExpressionId>,
    span: ByteSpan,
}

impl AssignmentListStatement {
    pub(crate) const fn new(
        targets: Vec<AssignmentTarget>,
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
    pub fn targets(&self) -> &[AssignmentTarget] {
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

#[derive(Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DoStatement {
    body: Block,
    span: ByteSpan,
}

#[derive(Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NumericForStatement {
    name: Identifier,
    initial: ExpressionId,
    limit: ExpressionId,
    step: Option<ExpressionId>,
    body: Block,
    span: ByteSpan,
}

impl NumericForStatement {
    pub(crate) const fn new(
        name: Identifier,
        initial: ExpressionId,
        limit: ExpressionId,
        step: Option<ExpressionId>,
        body: Block,
        span: ByteSpan,
    ) -> Self {
        Self {
            name,
            initial,
            limit,
            step,
            body,
            span,
        }
    }

    #[must_use]
    pub const fn name(&self) -> Identifier {
        self.name
    }

    #[must_use]
    pub const fn initial(&self) -> ExpressionId {
        self.initial
    }

    #[must_use]
    pub const fn limit(&self) -> ExpressionId {
        self.limit
    }

    #[must_use]
    pub const fn step(&self) -> Option<ExpressionId> {
        self.step
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

impl DoStatement {
    pub(crate) const fn new(body: Block, span: ByteSpan) -> Self {
        Self { body, span }
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
    Call(CallStatement),
    If(IfStatement),
    While(WhileStatement),
    Repeat(RepeatStatement),
    Do(DoStatement),
    NumericFor(NumericForStatement),
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
            Self::Call(statement) => statement.span(),
            Self::If(statement) => statement.span(),
            Self::While(statement) => statement.span(),
            Self::Repeat(statement) => statement.span(),
            Self::Do(statement) => statement.span(),
            Self::NumericFor(statement) => statement.span(),
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
                Statement::Do(statement) => statement.body().node_count(),
                Statement::NumericFor(statement) => statement.body().node_count(),
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
    table_fields: Vec<TableField>,
    call_arguments: Vec<ExpressionId>,
}

impl Ast {
    pub(crate) const fn new(
        profile: SemanticProfile,
        span: ByteSpan,
        statements: Vec<Statement>,
        expressions: Vec<Expression>,
        table_fields: Vec<TableField>,
        call_arguments: Vec<ExpressionId>,
    ) -> Self {
        Self {
            profile,
            span,
            block: Block::new(statements),
            expressions,
            table_fields,
            call_arguments,
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
    pub fn table_fields(&self, constructor: TableConstructor) -> Option<&[TableField]> {
        let end = constructor
            .first_field()
            .checked_add(constructor.field_count())?;
        self.table_fields.get(constructor.first_field()..end)
    }

    #[must_use]
    pub fn table_field_arena(&self) -> &[TableField] {
        &self.table_fields
    }

    #[must_use]
    pub fn call_arguments(&self, call: CallExpression) -> Option<&[ExpressionId]> {
        let end = call.first_argument().checked_add(call.argument_count())?;
        self.call_arguments.get(call.first_argument()..end)
    }

    #[must_use]
    pub fn call_argument_arena(&self) -> &[ExpressionId] {
        &self.call_arguments
    }

    #[must_use]
    pub fn node_count(&self) -> usize {
        self.block
            .node_count()
            .saturating_add(self.expressions.len())
            .saturating_add(self.table_fields.len())
            .saturating_add(self.call_arguments.len())
    }
}
