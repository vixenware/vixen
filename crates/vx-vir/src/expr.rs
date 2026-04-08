//! Expression nodes.
//!
//! Every expression in a Vixen program becomes an [`Expr`] with a unique
//! [`ExprId`](crate::ExprId). The [`ExprKind`] enum has 28 variants (27
//! language constructs + Error). Parenthesized expressions are eliminated
//! during the CST-to-VIR walk — the inner expression is used directly.

use crate::{BodyId, ExprId, ItemId, PatternId, ScopeId, StmtId, TypeRefId};
use vx_resolve::SourceSpan;
use vx_types::Ty;

/// An expression node in the VIR.
#[derive(Debug, Clone)]
pub struct Expr {
    /// The source location of this expression in the original source file.
    /// Populated during: CST walk.
    pub span: SourceSpan,

    /// The body this expression belongs to.
    /// Populated during: CST walk.
    pub body: BodyId,

    /// The lexical scope containing this expression — the scope in which names
    /// are resolved for this node. For scope-introducing nodes (blocks, match arms),
    /// this is the *parent* scope, not the scope they introduce.
    /// Populated during: CST walk.
    pub scope: ScopeId,

    /// The syntactic form of this expression.
    /// Populated during: CST walk.
    pub kind: ExprKind,

    /// The inferred type of this expression.
    /// Populated during: type checking pass. `None` before that.
    pub ty: Option<Ty>,
}

// ---------------------------------------------------------------------------
// ExprKind — 27 variants
// ---------------------------------------------------------------------------

/// Discriminant for all expression forms in Vixen.
///
/// Paren expressions are NOT represented — they are unwrapped during the
/// CST-to-VIR walk so that the inner expression is used directly.
#[derive(Debug, Clone)]
pub enum ExprKind {
    /// A literal value: `42`, `3.14`, `"hello"`, `true`, `false`.
    Literal(LiteralExpr),

    /// A path literal: `p"some/path"` or `p"some/{interpolated}/path"`.
    PathLiteral(PathLiteralExpr),

    /// A reference to a name in scope: `x`, `foo`, `MyType`.
    /// After name resolution, `resolved` points to the definition.
    NameRef(NameRefExpr),

    /// A block expression: `{ stmts...; tail_expr }`.
    /// The value of a block is its tail expression, or unit if absent.
    Block(BlockExpr),

    /// A closure expression: `(x, y) => body`.
    /// Creates a new [`Body`](crate::Body) for capture analysis.
    Closure(ClosureExpr),

    /// A tuple constructor: `(a, b, c)`.
    /// Zero elements = unit `()`, two or more = tuple.
    Tuple(TupleExpr),

    /// An array literal: `[a, b, ..c]`.
    Array(ArrayExpr),

    /// A record literal (anonymous struct): `{ name: value, ... }`.
    /// Used for keyword arguments in `where` expressions.
    Record(RecordExpr),

    /// A map literal: `%{ key: value, ... }`.
    Map(MapExpr),

    /// A named struct literal: `Point { x: 1, y: 2 }`.
    StructLiteral(StructLiteralExpr),

    /// A function call: `f(a, b)`.
    Call(CallExpr),

    /// A keyword-argument call: `f(a) where { key: val }`.
    Where(WhereExpr),

    /// A bare invocation (no-arg call): `f!`.
    Invoke(InvokeExpr),

    /// A partial application: `partial f(a, _)`.
    PartialApp(PartialAppExpr),

    /// An index operation: `a[b]`.
    Index(IndexExpr),

    /// A field access: `a.field` or `a.0`.
    Field(FieldExpr),

    /// An if expression: `if cond ( then ) else ( otherwise )`.
    If(IfExpr),

    /// A match expression: `match scrutinee { pat => body, ... }`.
    Match(MatchExpr),

    /// A binary operation: `a + b`, `a |> b`, `a && b`, etc.
    Binary(BinaryExpr),

    /// A prefix (unary) operation: `!a`, `-a`.
    Prefix(PrefixExpr),

    /// A range expression: `a..b` or `a..=b`.
    Range(RangeExpr),

    /// A type operation: `x as T`, `x is T`, `x :: T`.
    TypeOp(TypeOpExpr),

    /// A functional update: `value with { field: new_val }`.
    With(WithExpr),

    /// A fallible expression: `expr?`.
    Fallible(FallibleExpr),

    /// An interpolated string: `` `hello {name}, you are {age}` ``.
    InterpolatedString(InterpolatedStringExpr),

    /// An import expression: `import "path/to/file"`.
    Import(ImportExpr),

    /// A fail expression: `fail "message"` or `fail expr`.
    Fail(FailExpr),

    /// An error recovery node. Produced when the CST contains a broken or
    /// unparseable expression. Carries any child expressions that were
    /// successfully parsed, so the type checker can still process them.
    /// The type of an error expression is always `Ty::Poison`.
    Error,
}

// ---------------------------------------------------------------------------
// Helper types for each variant
// ---------------------------------------------------------------------------

/// The kind of literal value.
#[derive(Debug, Clone)]
pub enum LitKind {
    /// An integer literal (e.g. `42`, `0xFF`). Raw token text, not parsed.
    Int { raw: String },
    /// A floating-point literal (e.g. `3.14`). Raw token text, not parsed.
    Float { raw: String },
    /// A string literal. Content with surrounding quotes stripped, but not
    /// escape-processed. (Not truly raw token text — quotes are removed.)
    String { raw: String },
    /// A boolean literal: `true` or `false`.
    Bool(bool),
}

/// A literal value expression.
#[derive(Debug, Clone)]
pub struct LiteralExpr {
    /// The literal value.
    pub value: LitKind,
}

/// A path literal expression: `p"some/path"`.
#[derive(Debug, Clone)]
pub struct PathLiteralExpr {
    /// The parts of the path literal. A plain path has one Text part;
    /// an interpolated path has alternating Text and Expr parts.
    pub parts: Vec<StringPart>,
}

/// A reference to a name in scope.
#[derive(Debug, Clone)]
pub struct NameRefExpr {
    /// The name as written in source.
    pub name: String,

    /// What this name resolves to.
    /// Populated during: name resolution pass. `None` means unresolved
    /// (either an error or not yet resolved).
    pub resolved: Option<crate::scope::DefTarget>,
}

/// A block expression: `{ stmts...; tail_expr }`.
#[derive(Debug, Clone)]
pub struct BlockExpr {
    /// The scope introduced by this block.
    /// Populated during: CST walk.
    pub scope: ScopeId,

    /// The statements in the block, in source order.
    pub stmts: Vec<StmtId>,

    /// The optional tail expression (the block's value).
    /// `None` means the block evaluates to unit.
    pub tail: Option<ExprId>,

    /// Whether this block was synthesized by the builder (e.g. top-level body wrapper).
    /// Synthetic blocks are not user-written code and should not generate diagnostics.
    pub synthetic: bool,
}

/// A closure expression: `(x, y) => body`.
#[derive(Debug, Clone)]
pub struct ClosureExpr {
    /// The body of the closure. The body holds parameters and root expression.
    pub body: BodyId,
}

/// A tuple constructor: `(a, b, c)`.
#[derive(Debug, Clone)]
pub struct TupleExpr {
    /// The elements of the tuple. Empty = unit `()`.
    pub elements: Vec<ExprId>,
}

/// An element in an array literal.
#[derive(Debug, Clone)]
pub enum ArrayElement {
    /// A plain element: `expr`.
    Expr(ExprId),
    /// A spread element: `..expr`.
    Spread(ExprId),
}

/// An array literal: `[a, b, ..c]`.
#[derive(Debug, Clone)]
pub struct ArrayExpr {
    /// The elements, which may include spreads.
    pub elements: Vec<ArrayElement>,
}

/// A field in a record literal: `name: value`.
#[derive(Debug, Clone)]
pub struct RecordField {
    /// The field name.
    pub name: String,
    /// Span of the field name token, for diagnostics.
    pub name_span: SourceSpan,
    /// The value expression.
    pub value: ExprId,
}

/// A record literal: `{ name: value, ... }`.
#[derive(Debug, Clone)]
pub struct RecordExpr {
    /// The fields of the record.
    pub fields: Vec<RecordField>,
}

/// A key in a map literal entry.
#[derive(Debug, Clone)]
pub enum MapKey {
    /// A computed key expression: `[expr]: value` or a bare expression key.
    Expr(ExprId),
    /// A name used as a string key: `name: value` (shorthand — key is `"name"`).
    Name {
        /// The name text.
        name: String,
        /// Span of the name token.
        span: SourceSpan,
    },
}

/// A key-value entry in a map literal.
#[derive(Debug, Clone)]
pub struct MapEntry {
    /// The key.
    pub key: MapKey,
    /// The value expression.
    pub value: ExprId,
    /// Span of the entire entry.
    pub span: SourceSpan,
}

/// An element in a map literal.
#[derive(Debug, Clone)]
pub enum MapElement {
    /// A key-value entry.
    Entry(MapEntry),
    /// A spread: `..expr`.
    Spread(ExprId),
}

/// A map literal: `%{ key: value, ... }`.
#[derive(Debug, Clone)]
pub struct MapExpr {
    /// The entries, which may include spreads.
    pub entries: Vec<MapElement>,
}

/// A field in a struct literal: `name: value`.
#[derive(Debug, Clone)]
pub struct StructLiteralField {
    /// The field name.
    pub name: String,
    /// Span of the field name token.
    pub name_span: SourceSpan,
    /// The value expression. For shorthand `Point { x }`, this is a
    /// NameRef expression for `x`.
    pub value: ExprId,
}

/// A named struct literal: `Point { x: 1, y: 2 }`.
#[derive(Debug, Clone)]
pub struct StructLiteralExpr {
    /// The struct name as written (e.g. `"Point"`).
    pub name: String,
    /// Span of the struct name, for diagnostics.
    pub name_span: SourceSpan,
    /// The resolved target of the struct name.
    /// Populated during: name resolution pass.
    pub resolved: Option<ItemId>,
    /// The field initializers.
    pub fields: Vec<StructLiteralField>,
}

/// A function call: `f(a, b)` or `f[T](a, b)`.
#[derive(Debug, Clone)]
pub struct CallExpr {
    /// The callee expression (the function being called).
    pub callee: ExprId,
    /// Positional arguments, in order.
    pub args: Vec<Arg>,
    /// Explicit type arguments from `f[T, U](...)` syntax, if any.
    pub type_args: Vec<TypeRefId>,
}

/// A positional argument in a function call.
#[derive(Debug, Clone)]
pub struct Arg {
    /// The argument expression.
    pub expr: ExprId,
    /// Span of the argument (may differ from expr span due to surrounding syntax).
    pub span: SourceSpan,
}

/// A keyword-argument call: `callee where { key: val }`.
#[derive(Debug, Clone)]
pub struct WhereExpr {
    /// The callee expression (may be a Call or a NameRef).
    pub callee: ExprId,
    /// The keyword record expression after `where`.
    pub keywords: ExprId,
}

/// A bare invocation (no-arg call): `callee!`.
#[derive(Debug, Clone)]
pub struct InvokeExpr {
    /// The callee expression being invoked with no arguments.
    pub callee: ExprId,
}

/// A partial application: `partial f(a, _)`.
#[derive(Debug, Clone)]
pub struct PartialAppExpr {
    /// The callee expression being partially applied.
    pub callee: ExprId,
    /// The arguments. Placeholder positions (`_`) are represented as `None`.
    pub args: Vec<PartialArg>,
}

/// An argument in a partial application.
#[derive(Debug, Clone)]
pub enum PartialArg {
    /// A concrete argument value.
    Expr(ExprId),
    /// A placeholder (`_`) — this position is left unapplied.
    Placeholder {
        /// Span of the `_` token.
        span: SourceSpan,
    },
}

/// An index operation: `a[b]`.
#[derive(Debug, Clone)]
pub struct IndexExpr {
    /// The expression being indexed.
    pub receiver: ExprId,
    /// The index expression.
    pub index: ExprId,
}

/// A key identifying a field on a product type.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FieldKey {
    /// A named field: `.name`.
    Name(String),
    /// A positional (tuple) index: `.0`, `.1`, etc.
    Index(u32),
}

/// A field access: `a.field` or `a.0`.
#[derive(Debug, Clone)]
pub struct FieldExpr {
    /// The expression whose field is being accessed.
    pub receiver: ExprId,
    /// The field being accessed.
    pub field: FieldKey,
    /// Span of the field name/index token.
    pub field_span: SourceSpan,
}

/// An if expression: `if cond ( then ) else ( otherwise )`.
#[derive(Debug, Clone)]
pub struct IfExpr {
    /// The condition expression.
    pub condition: ExprId,
    /// The then-branch expression.
    pub then_branch: ExprId,
    /// The else-branch expression. `None` if there is no else clause.
    pub else_branch: Option<ExprId>,
}

/// A match arm: `pattern => body`.
#[derive(Debug, Clone)]
pub struct MatchArm {
    /// The pattern to match against.
    pub pattern: PatternId,
    /// The body expression evaluated when this arm matches.
    pub body: ExprId,
    /// The scope for bindings introduced by this arm's pattern.
    pub scope: ScopeId,
    /// Span of the entire arm (pattern + `=>` + body).
    pub span: SourceSpan,
}

/// A match expression: `match scrutinee { arms... }`.
#[derive(Debug, Clone)]
pub struct MatchExpr {
    /// The expression being matched on.
    pub scrutinee: ExprId,
    /// The match arms, in source order.
    pub arms: Vec<MatchArm>,
}

/// Binary operators available in Vixen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    /// `+` addition.
    Add,
    /// `++` string/array concatenation.
    Concat,
    /// `-` subtraction.
    Sub,
    /// `*` multiplication.
    Mul,
    /// `/` division.
    Div,
    /// `%` remainder.
    Rem,
    /// `==` equality.
    Eq,
    /// `!=` inequality.
    Neq,
    /// `<` less than.
    Lt,
    /// `<=` less than or equal.
    LtEq,
    /// `>` greater than.
    Gt,
    /// `>=` greater than or equal.
    GtEq,
    /// `&&` logical and (short-circuiting).
    And,
    /// `||` logical or (short-circuiting).
    Or,
    /// `|>` pipe operator.
    Pipe,
}

/// A binary expression: `lhs op rhs`.
#[derive(Debug, Clone)]
pub struct BinaryExpr {
    /// The left-hand operand.
    pub lhs: ExprId,
    /// The operator.
    pub op: BinOp,
    /// Span of the operator token, for diagnostics.
    pub op_span: SourceSpan,
    /// The right-hand operand.
    pub rhs: ExprId,
}

/// Prefix (unary) operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefixOp {
    /// `!` logical negation.
    Not,
    /// `-` arithmetic negation.
    Neg,
}

/// A prefix expression: `!a` or `-a`.
#[derive(Debug, Clone)]
pub struct PrefixExpr {
    /// The operator.
    pub op: PrefixOp,
    /// Span of the operator token.
    pub op_span: SourceSpan,
    /// The operand expression.
    pub operand: ExprId,
}

/// A range expression: `a..b`, `a..=b`, `a..`, `..b`, `..=b`, or `..`.
#[derive(Debug, Clone)]
pub struct RangeExpr {
    /// The start of the range. `None` for `..b` or `..`.
    pub start: Option<ExprId>,
    /// The end of the range. `None` for `a..` or `..`.
    pub end: Option<ExprId>,
    /// Whether the range is inclusive (`..=`) or exclusive (`..`).
    pub inclusive: bool,
}

/// The kind of type operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeOpKind {
    /// `expr as Type` — type cast/coercion.
    As,
    /// `expr is Type` — type test (returns bool).
    Is,
    /// `expr :: Type` — type ascription.
    Ascription,
}

/// A type operation: `expr as T`, `expr is T`, `expr :: T`.
#[derive(Debug, Clone)]
pub struct TypeOpExpr {
    /// The expression being operated on.
    pub expr: ExprId,
    /// The kind of type operation.
    pub op: TypeOpKind,
    /// The type being tested/cast to.
    pub ty: TypeRefId,
}

/// A field update within a `with` expression or compound update statement.
#[derive(Debug, Clone)]
pub enum UpdateField {
    /// `field_path: new_value` — set a field to a new value.
    Set {
        /// The field path (may be nested via dot notation, e.g. `["a", "b"]`).
        path: Vec<String>,
        /// Span of the field path.
        path_span: SourceSpan,
        /// The new value expression.
        value: ExprId,
    },
    /// `:field_path with { ... }` — recursive update of a nested field.
    Nested {
        /// The field path to the nested struct.
        path: Vec<String>,
        /// Span of the field path.
        path_span: SourceSpan,
        /// The nested field updates.
        updates: Vec<UpdateField>,
    },
}

/// A functional update: `value with { updates... }`.
#[derive(Debug, Clone)]
pub struct WithExpr {
    /// The base expression being updated.
    pub receiver: ExprId,
    /// The field updates to apply.
    pub updates: Vec<UpdateField>,
}

/// A fallible expression: `expr?`.
#[derive(Debug, Clone)]
pub struct FallibleExpr {
    /// The expression that may fail.
    pub expr: ExprId,
}

/// A part of an interpolated string or path literal.
#[derive(Debug, Clone)]
pub enum StringPart {
    /// A literal text segment.
    Text(String),
    /// An interpolated expression: `{expr}`.
    Expr(ExprId),
}

/// An interpolated string: `` `hello {name}` ``.
#[derive(Debug, Clone)]
pub struct InterpolatedStringExpr {
    /// The parts of the interpolated string, alternating text and expressions.
    pub parts: Vec<StringPart>,
}

/// An import expression: `import "path/to/file"`.
#[derive(Debug, Clone)]
pub struct ImportExpr {
    /// The import path as written in source (string literal content).
    pub path: String,
    /// The resolved source ID of the imported file.
    /// Populated during: name resolution pass. `None` before that.
    pub resolved_source: Option<vx_resolve::SourceId>,
}

/// A fail expression: `fail "message"` or `fail expr`.
#[derive(Debug, Clone)]
pub struct FailExpr {
    /// The message expression. `None` for bare `fail`.
    pub message: Option<ExprId>,
}
