//! Lowered instruction representation.
//!
//! This is the Send-safe, owned tree that the evaluator operates on.
//! It's produced by the lowering pass from the rowan CST (which is !Send).
//! Comments, whitespace, and parsing artifacts are stripped — this is
//! pure evaluation structure.

use std::sync::Arc;

/// A lowered module — all top-level declarations from a source file.
#[derive(Debug, Clone)]
pub struct Module {
    pub functions: Vec<Arc<FnDef>>,
}

/// A lowered function definition.
#[derive(Debug, Clone)]
pub struct FnDef {
    pub name: String,
    pub params: Vec<Param>,
    pub body: Block,
}

/// A function parameter.
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
}

/// A block: a sequence of statements. The last statement's value is the
/// block's value. An empty block or a block ending with a non-expression
/// statement evaluates to Unit.
#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
}

/// A statement in a block.
#[derive(Debug, Clone)]
pub enum Stmt {
    /// `let name = expr`
    Let { name: String, value: Expr },
    /// `name = expr` (reassignment / shadowing)
    Assign { name: String, value: Expr },
    /// A bare expression (the last one in a block is the block's value)
    Expr(Expr),
    /// `return expr`
    Return(Expr),
    /// A local function declaration
    FnDecl(Arc<FnDef>),
}

/// An expression — the core of the instruction tree.
#[derive(Debug, Clone)]
pub enum Expr {
    /// Integer literal (default U64)
    LitInt(u64),
    /// Float literal (default F64)
    LitFloat(f64),
    /// Boolean literal
    LitBool(bool),
    /// String literal
    LitString(String),
    /// Unit `()`
    Unit,

    /// Variable reference (resolved name)
    Var(String),

    /// Binary operation
    Binary {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },

    /// Unary operation
    Unary { op: UnaryOp, operand: Box<Expr> },

    /// Block expression
    Block(Block),

    /// `if cond { then } else { else_ }`
    If {
        cond: Box<Expr>,
        then_branch: Block,
        else_branch: Block,
    },

    /// Function call: `f(args...; kw: value, ...)`
    Call {
        func: Box<Expr>,
        args: Vec<Expr>,
        kwargs: Vec<(String, Expr)>,
    },

    /// Struct literal: `TypeName { field: value, ... }`
    StructLit {
        name: String,
        fields: Vec<(String, Expr)>,
    },

    /// Array literal: `[a, b, c]`
    Array(Vec<Expr>),

    /// Map literal: `%{ key => value, ... }`
    Map(Vec<(Expr, Expr)>),

    /// Field access: `expr.field`
    Field { receiver: Box<Expr>, field: String },

    /// `fail expr`
    Fail(Box<Expr>),

    /// Tuple literal: `(a, b, c)`
    Tuple(Vec<Expr>),
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
}
