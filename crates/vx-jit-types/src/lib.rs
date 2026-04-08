//! Vixen-side typed AST used by the Kajit lowering bridge.
//!
//! These types are owned by Vixen because they describe the strict fragment
//! frontend model produced by Vixen analysis and consumed by Kajit lowering.

use kajit_reprs::hir::{BinaryOp, Literal, LocalId, Type, TypeDefId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VixenCoreTypes {
    pub string: Type,
    pub node: Type,
    pub edge: Type,
    pub fact: Type,
    pub crate_graph: Type,
    pub crate_node: Type,
    pub crate_id: Type,
    pub crate_type: Type,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VixenBuiltin {
    EmitNode,
    EmitEdge,
    EmitFact,
    RustCrateGraph,
    RustRoot,
    GraphLookupCrate,
    CargoRegistryPackageExists,
}

impl VixenBuiltin {
    pub const fn callable_name(self) -> &'static str {
        match self {
            Self::EmitNode => "emit.node",
            Self::EmitEdge => "emit.edge",
            Self::EmitFact => "emit.fact",
            Self::RustCrateGraph => "rust.crate_graph",
            Self::RustRoot => "rust.root",
            Self::GraphLookupCrate => "graph.lookup_crate",
            Self::CargoRegistryPackageExists => "cargo.registry_package_exists",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VixenCallableRef {
    Builtin(VixenBuiltin),
    Named(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VixenTypedExpr {
    Literal(Literal),
    TypedLiteral {
        literal: Literal,
        ty: Type,
    },
    Local(LocalId),
    Field {
        base: Box<VixenTypedExpr>,
        field: String,
    },
    Struct {
        def: TypeDefId,
        fields: Vec<(String, VixenTypedExpr)>,
    },
    Variant {
        def: TypeDefId,
        variant: String,
        fields: Vec<(String, VixenTypedExpr)>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<VixenTypedExpr>,
        rhs: Box<VixenTypedExpr>,
    },
    Call {
        callee: VixenCallableRef,
        args: Vec<VixenTypedExpr>,
    },
    MethodCall {
        receiver: Box<VixenTypedExpr>,
        method: String,
        args: Vec<VixenTypedExpr>,
    },
    If {
        condition: Box<VixenTypedExpr>,
        then_expr: Box<VixenTypedExpr>,
        else_expr: Box<VixenTypedExpr>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VixenTypedParam {
    pub local: LocalId,
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VixenTypedLocal {
    pub local: LocalId,
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VixenTypedStmt {
    Let {
        local: LocalId,
        value: VixenTypedExpr,
    },
    Expr(VixenTypedExpr),
    If {
        condition: VixenTypedExpr,
        then_body: Vec<VixenTypedStmt>,
        else_body: Vec<VixenTypedStmt>,
    },
    Return(Option<VixenTypedExpr>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VixenTypedFunction {
    pub name: String,
    pub params: Vec<VixenTypedParam>,
    pub locals: Vec<VixenTypedLocal>,
    pub return_type: Type,
    pub body: Vec<VixenTypedStmt>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VixenLoweringError {
    MissingCallable {
        builtin: VixenBuiltin,
    },
    MissingNamedCallable {
        name: String,
    },
    MissingMethod {
        method: String,
        receiver_ty: Type,
    },
    AmbiguousMethod {
        method: String,
        receiver_ty: Type,
        candidates: Vec<String>,
    },
    UnknownLocalType {
        local: LocalId,
    },
    UnknownFieldType {
        field: String,
        base: Type,
    },
    CannotInferExprType {
        expr: &'static str,
    },
    IfBranchTypeMismatch {
        then_ty: Type,
        else_ty: Type,
    },
    NonStatementIfExpr,
}
