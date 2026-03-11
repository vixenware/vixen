//! Transportable evaluator payloads for executor-side evaluation.

use std::sync::Arc;

use facet::Facet;
use vx_store_proto::{BlobRef, NodeHash};

use crate::instr;
use crate::value::{
    BlobValue, BuiltinFunction, BuiltinMethod, BuiltinNamespace, BuiltinValue, DiscoveredValue,
    FunctionValue, HostObservation, MapValue, PathValue, StructValue, TreeValue, Value,
};

#[derive(Debug, Clone, PartialEq, Facet)]
pub struct TransportModule {
    pub functions: Vec<TransportFnDef>,
    pub top_level: TransportBlock,
}

#[derive(Debug, Clone, PartialEq, Facet)]
pub struct TransportFnDef {
    pub name: String,
    pub params: Vec<TransportParam>,
    pub body: TransportBlock,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TransportParam {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TransportSourceSpan {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TransportSourceProvenance {
    pub source_id: String,
    pub span: TransportSourceSpan,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Facet)]
pub struct TransportBlock {
    pub stmts: Vec<TransportStmt>,
}

#[derive(Debug, Clone, PartialEq, Facet)]
#[repr(u8)]
pub enum TransportArrayItem {
    Expr(TransportExpr),
    Spread(TransportExpr),
}

#[derive(Debug, Clone, PartialEq, Facet)]
#[repr(u8)]
pub enum TransportPattern {
    Binding(String),
    Struct(Vec<(String, Option<TransportPattern>)>),
}

#[derive(Debug, Clone, PartialEq, Facet)]
#[repr(u8)]
pub enum TransportRecordItem {
    Field { name: String, value: TransportExpr },
    Spread(TransportExpr),
}

#[derive(Debug, Clone, PartialEq, Facet)]
#[repr(u8)]
pub enum TransportStmt {
    Let {
        name: String,
        value: TransportExpr,
    },
    LetPattern {
        pattern: TransportPattern,
        value: TransportExpr,
    },
    Assign {
        name: String,
        value: TransportExpr,
    },
    Expr(TransportExpr),
    Return(TransportExpr),
    FnDecl(TransportFnDef),
}

#[derive(Debug, Clone, PartialEq, Facet)]
#[repr(u8)]
pub enum TransportTemplatePart {
    Text(String),
    Expr(Box<TransportExpr>),
}

#[derive(Debug, Clone, PartialEq, Facet)]
#[repr(u8)]
pub enum TransportExpr {
    LitInt(u64),
    LitFloat(f64),
    LitBool(bool),
    LitString(String),
    StringTemplate {
        parts: Vec<TransportTemplatePart>,
        provenance: Option<TransportSourceProvenance>,
    },
    PathTemplate {
        parts: Vec<TransportTemplatePart>,
        provenance: Option<TransportSourceProvenance>,
    },
    Unit,
    Var(String),
    Binary {
        op: TransportBinOp,
        left: Box<TransportExpr>,
        right: Box<TransportExpr>,
        provenance: Option<TransportSourceProvenance>,
    },
    Unary {
        op: TransportUnaryOp,
        operand: Box<TransportExpr>,
        provenance: Option<TransportSourceProvenance>,
    },
    Block(TransportBlock),
    If {
        cond: Box<TransportExpr>,
        then_branch: TransportBlock,
        else_branch: TransportBlock,
        provenance: Option<TransportSourceProvenance>,
    },
    Closure {
        closure_id: usize,
        def: TransportFnDef,
    },
    Call {
        func: Box<TransportExpr>,
        args: Vec<TransportExpr>,
        kwargs: Vec<(String, TransportExpr)>,
        kwargs_expr: Option<Box<TransportExpr>>,
        provenance: Option<TransportSourceProvenance>,
    },
    PartialCall {
        func: Box<TransportExpr>,
        args: Vec<TransportExpr>,
        kwargs: Vec<(String, TransportExpr)>,
        kwargs_expr: Option<Box<TransportExpr>>,
        provenance: Option<TransportSourceProvenance>,
    },
    StructLit {
        name: String,
        fields: Vec<(String, TransportExpr)>,
    },
    Record(Vec<TransportRecordItem>),
    Array(Vec<TransportArrayItem>),
    Map(Vec<(TransportExpr, TransportExpr)>),
    Field {
        receiver: Box<TransportExpr>,
        field: String,
        provenance: Option<TransportSourceProvenance>,
    },
    Fail {
        value: Box<TransportExpr>,
        provenance: Option<TransportSourceProvenance>,
    },
    Fallible(Box<TransportExpr>),
    Tuple(Vec<TransportExpr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum TransportBinOp {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum TransportUnaryOp {
    Neg,
    Not,
}

#[derive(Debug, Clone, PartialEq, Facet)]
pub struct ShippedFunction {
    pub code_identity: String,
    pub def: TransportFnDef,
    pub captures: Vec<(String, TransportValue)>,
}

#[derive(Debug, Clone, PartialEq, Facet)]
#[repr(u8)]
pub enum TransportValue {
    U64(u64),
    I64(i64),
    F64(f64),
    Bool(bool),
    String(String),
    Path(String),
    Blob(TransportBlob),
    Bytes(Vec<u8>),
    Tree(TransportTree),
    ExecutorClass(String),
    Discovered {
        value: Box<TransportValue>,
        obligations: Vec<HostObservation>,
    },
    Unit,
    Tuple(Vec<TransportValue>),
    Array(Vec<TransportValue>),
    Map(Vec<(TransportValue, TransportValue)>),
    Struct(TransportStruct),
    Function(ShippedFunction),
    Builtin(TransportBuiltinValue),
}

#[derive(Debug, Clone, Facet)]
#[repr(u8)]
pub enum TransportBlob {
    Inline { bytes: Vec<u8>, executable: bool },
    Store { blob_ref: BlobRef, executable: bool },
}

#[derive(Debug, Clone, PartialEq, Facet)]
#[repr(u8)]
pub enum TransportTree {
    Inline(Vec<(String, TransportValue)>),
    Store(NodeHash),
}

#[derive(Debug, Clone, PartialEq, Facet)]
pub struct TransportStruct {
    pub type_name: String,
    pub fields: Vec<(String, TransportValue)>,
}

#[derive(Debug, Clone, PartialEq, Facet)]
#[repr(u8)]
pub enum TransportBuiltinValue {
    BlobNamespace,
    BytesNamespace,
    TreeNamespace,
    ExecFunction,
    DiscoverFunction,
    ExecutorClassFunction,
    StructConstructor {
        type_name: String,
        fields: Vec<String>,
    },
    BlobFromString,
    BytesFromString,
    TreeOf,
    TreeEmpty,
    TreeFromPath,
    BlobToString {
        receiver: Box<TransportValue>,
    },
    BlobExecutable {
        receiver: Box<TransportValue>,
    },
    BlobUtf8 {
        receiver: Box<TransportValue>,
    },
    BlobUtf8Lossy {
        receiver: Box<TransportValue>,
    },
    BytesUtf8 {
        receiver: Box<TransportValue>,
    },
    BytesUtf8Lossy {
        receiver: Box<TransportValue>,
    },
    BytesBlob {
        receiver: Box<TransportValue>,
    },
    StreamCollect {
        receiver: Box<TransportValue>,
    },
    TreeFile {
        receiver: Box<TransportValue>,
    },
    TreeMerge {
        receiver: Box<TransportValue>,
    },
    TreeSubtree {
        receiver: Box<TransportValue>,
    },
    FallibleCall {
        receiver: Box<TransportValue>,
    },
}

impl From<&instr::Module> for TransportModule {
    fn from(module: &instr::Module) -> Self {
        Self {
            functions: module
                .functions
                .iter()
                .map(|function| TransportFnDef::from(function.as_ref()))
                .collect(),
            top_level: TransportBlock::from(&module.top_level),
        }
    }
}

impl From<TransportModule> for instr::Module {
    fn from(module: TransportModule) -> Self {
        Self {
            functions: module
                .functions
                .into_iter()
                .map(|function| Arc::new(function.into()))
                .collect(),
            top_level: module.top_level.into(),
        }
    }
}

impl From<&instr::FnDef> for TransportFnDef {
    fn from(def: &instr::FnDef) -> Self {
        Self {
            name: def.name.clone(),
            params: def.params.iter().map(TransportParam::from).collect(),
            body: TransportBlock::from(&def.body),
        }
    }
}

impl From<TransportFnDef> for instr::FnDef {
    fn from(def: TransportFnDef) -> Self {
        Self {
            name: def.name,
            params: def.params.into_iter().map(Into::into).collect(),
            body: def.body.into(),
        }
    }
}

impl From<&instr::Param> for TransportParam {
    fn from(param: &instr::Param) -> Self {
        Self {
            name: param.name.clone(),
        }
    }
}

impl From<&instr::SourceSpan> for TransportSourceSpan {
    fn from(span: &instr::SourceSpan) -> Self {
        Self {
            start: span.start,
            end: span.end,
        }
    }
}

impl From<TransportSourceSpan> for instr::SourceSpan {
    fn from(span: TransportSourceSpan) -> Self {
        Self {
            start: span.start,
            end: span.end,
        }
    }
}

impl From<&instr::SourceProvenance> for TransportSourceProvenance {
    fn from(provenance: &instr::SourceProvenance) -> Self {
        Self {
            source_id: provenance.source_id.to_string(),
            span: TransportSourceSpan::from(&provenance.span),
            label: provenance.label.clone(),
        }
    }
}

impl From<TransportSourceProvenance> for instr::SourceProvenance {
    fn from(provenance: TransportSourceProvenance) -> Self {
        Self {
            source_id: Arc::from(provenance.source_id),
            span: provenance.span.into(),
            label: provenance.label,
        }
    }
}

impl From<TransportParam> for instr::Param {
    fn from(param: TransportParam) -> Self {
        Self { name: param.name }
    }
}

impl From<&instr::Block> for TransportBlock {
    fn from(block: &instr::Block) -> Self {
        Self {
            stmts: block.stmts.iter().map(TransportStmt::from).collect(),
        }
    }
}

impl From<TransportBlock> for instr::Block {
    fn from(block: TransportBlock) -> Self {
        Self {
            stmts: block.stmts.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<&instr::Stmt> for TransportStmt {
    fn from(stmt: &instr::Stmt) -> Self {
        match stmt {
            instr::Stmt::Let { name, value } => Self::Let {
                name: name.clone(),
                value: TransportExpr::from(value),
            },
            instr::Stmt::LetPattern { pattern, value } => Self::LetPattern {
                pattern: TransportPattern::from(pattern),
                value: TransportExpr::from(value),
            },
            instr::Stmt::Assign { name, value } => Self::Assign {
                name: name.clone(),
                value: TransportExpr::from(value),
            },
            instr::Stmt::Expr(expr) => Self::Expr(TransportExpr::from(expr)),
            instr::Stmt::Return(expr) => Self::Return(TransportExpr::from(expr)),
            instr::Stmt::FnDecl(def) => Self::FnDecl(TransportFnDef::from(def.as_ref())),
            instr::Stmt::StructDecl(_) => Self::Expr(TransportExpr::Unit),
        }
    }
}

impl From<TransportStmt> for instr::Stmt {
    fn from(stmt: TransportStmt) -> Self {
        match stmt {
            TransportStmt::Let { name, value } => Self::Let {
                name,
                value: value.into(),
            },
            TransportStmt::LetPattern { pattern, value } => Self::LetPattern {
                pattern: pattern.into(),
                value: value.into(),
            },
            TransportStmt::Assign { name, value } => Self::Assign {
                name,
                value: value.into(),
            },
            TransportStmt::Expr(expr) => Self::Expr(expr.into()),
            TransportStmt::Return(expr) => Self::Return(expr.into()),
            TransportStmt::FnDecl(def) => Self::FnDecl(Arc::new(def.into())),
        }
    }
}

impl From<&instr::Pattern> for TransportPattern {
    fn from(pattern: &instr::Pattern) -> Self {
        match pattern {
            instr::Pattern::Binding(name) => Self::Binding(name.clone()),
            instr::Pattern::Struct(fields) => Self::Struct(
                fields
                    .iter()
                    .map(|(name, pattern)| {
                        (name.clone(), pattern.as_ref().map(TransportPattern::from))
                    })
                    .collect(),
            ),
        }
    }
}

impl From<TransportPattern> for instr::Pattern {
    fn from(pattern: TransportPattern) -> Self {
        match pattern {
            TransportPattern::Binding(name) => Self::Binding(name),
            TransportPattern::Struct(fields) => Self::Struct(
                fields
                    .into_iter()
                    .map(|(name, pattern)| (name, pattern.map(Into::into)))
                    .collect(),
            ),
        }
    }
}

impl From<&instr::RecordItem> for TransportRecordItem {
    fn from(item: &instr::RecordItem) -> Self {
        match item {
            instr::RecordItem::Field { name, value } => Self::Field {
                name: name.clone(),
                value: value.into(),
            },
            instr::RecordItem::Spread(value) => Self::Spread(value.into()),
        }
    }
}

impl From<TransportRecordItem> for instr::RecordItem {
    fn from(item: TransportRecordItem) -> Self {
        match item {
            TransportRecordItem::Field { name, value } => Self::Field {
                name,
                value: value.into(),
            },
            TransportRecordItem::Spread(value) => Self::Spread(value.into()),
        }
    }
}

impl From<&instr::TemplatePart> for TransportTemplatePart {
    fn from(part: &instr::TemplatePart) -> Self {
        match part {
            instr::TemplatePart::Text(text) => Self::Text(text.clone()),
            instr::TemplatePart::Expr(expr) => Self::Expr(Box::new(TransportExpr::from(expr))),
        }
    }
}

impl From<TransportTemplatePart> for instr::TemplatePart {
    fn from(part: TransportTemplatePart) -> Self {
        match part {
            TransportTemplatePart::Text(text) => Self::Text(text),
            TransportTemplatePart::Expr(expr) => Self::Expr((*expr).into()),
        }
    }
}

impl From<&instr::Expr> for TransportExpr {
    fn from(expr: &instr::Expr) -> Self {
        match expr {
            instr::Expr::LitInt(value) => Self::LitInt(*value),
            instr::Expr::LitFloat(value) => Self::LitFloat(*value),
            instr::Expr::LitBool(value) => Self::LitBool(*value),
            instr::Expr::LitString(value) => Self::LitString(value.clone()),
            instr::Expr::StringTemplate { parts, provenance } => Self::StringTemplate {
                parts: parts.iter().map(TransportTemplatePart::from).collect(),
                provenance: provenance.as_ref().map(TransportSourceProvenance::from),
            },
            instr::Expr::PathTemplate { parts, provenance } => Self::PathTemplate {
                parts: parts.iter().map(TransportTemplatePart::from).collect(),
                provenance: provenance.as_ref().map(TransportSourceProvenance::from),
            },
            instr::Expr::Unit => Self::Unit,
            instr::Expr::Var(name) => Self::Var(name.clone()),
            instr::Expr::Binary {
                op,
                left,
                right,
                provenance,
            } => Self::Binary {
                op: TransportBinOp::from(*op),
                left: Box::new(TransportExpr::from(left.as_ref())),
                right: Box::new(TransportExpr::from(right.as_ref())),
                provenance: provenance.as_ref().map(TransportSourceProvenance::from),
            },
            instr::Expr::Unary {
                op,
                operand,
                provenance,
            } => Self::Unary {
                op: TransportUnaryOp::from(*op),
                operand: Box::new(TransportExpr::from(operand.as_ref())),
                provenance: provenance.as_ref().map(TransportSourceProvenance::from),
            },
            instr::Expr::Block(block) => Self::Block(TransportBlock::from(block)),
            instr::Expr::If {
                cond,
                then_branch,
                else_branch,
                provenance,
            } => Self::If {
                cond: Box::new(TransportExpr::from(cond.as_ref())),
                then_branch: TransportBlock::from(then_branch),
                else_branch: TransportBlock::from(else_branch),
                provenance: provenance.as_ref().map(TransportSourceProvenance::from),
            },
            instr::Expr::Closure { closure_id, def } => Self::Closure {
                closure_id: *closure_id,
                def: TransportFnDef::from(def.as_ref()),
            },
            instr::Expr::Call {
                func,
                args,
                kwargs,
                kwargs_expr,
                provenance,
            } => Self::Call {
                func: Box::new(TransportExpr::from(func.as_ref())),
                args: args.iter().map(TransportExpr::from).collect(),
                kwargs: kwargs
                    .iter()
                    .map(|(name, value)| (name.clone(), TransportExpr::from(value)))
                    .collect(),
                kwargs_expr: kwargs_expr
                    .as_ref()
                    .map(|expr| Box::new(TransportExpr::from(expr.as_ref()))),
                provenance: provenance.as_ref().map(TransportSourceProvenance::from),
            },
            instr::Expr::PartialCall {
                func,
                args,
                kwargs,
                kwargs_expr,
                provenance,
            } => Self::PartialCall {
                func: Box::new(TransportExpr::from(func.as_ref())),
                args: args.iter().map(TransportExpr::from).collect(),
                kwargs: kwargs
                    .iter()
                    .map(|(name, value)| (name.clone(), TransportExpr::from(value)))
                    .collect(),
                kwargs_expr: kwargs_expr
                    .as_ref()
                    .map(|expr| Box::new(TransportExpr::from(expr.as_ref()))),
                provenance: provenance.as_ref().map(TransportSourceProvenance::from),
            },
            instr::Expr::StructLit { name, fields } => Self::StructLit {
                name: name.clone(),
                fields: fields
                    .iter()
                    .map(|(name, value)| (name.clone(), TransportExpr::from(value)))
                    .collect(),
            },
            instr::Expr::Record(items) => {
                Self::Record(items.iter().map(TransportRecordItem::from).collect())
            }
            instr::Expr::Array(values) => Self::Array(
                values
                    .iter()
                    .map(|value| match value {
                        instr::ArrayItem::Expr(expr) => TransportArrayItem::Expr(expr.into()),
                        instr::ArrayItem::Spread(expr) => TransportArrayItem::Spread(expr.into()),
                    })
                    .collect(),
            ),
            instr::Expr::Map(entries) => Self::Map(
                entries
                    .iter()
                    .map(|(key, value)| (TransportExpr::from(key), TransportExpr::from(value)))
                    .collect(),
            ),
            instr::Expr::Field {
                receiver,
                field,
                provenance,
            } => Self::Field {
                receiver: Box::new(TransportExpr::from(receiver.as_ref())),
                field: field.clone(),
                provenance: provenance.as_ref().map(TransportSourceProvenance::from),
            },
            instr::Expr::Fail {
                message,
                provenance,
            } => Self::Fail {
                value: Box::new(TransportExpr::from(message.as_ref())),
                provenance: provenance.as_ref().map(TransportSourceProvenance::from),
            },
            instr::Expr::Fallible(value) => {
                Self::Fallible(Box::new(TransportExpr::from(value.as_ref())))
            }
            instr::Expr::Tuple(values) => {
                Self::Tuple(values.iter().map(TransportExpr::from).collect())
            }
        }
    }
}

impl From<TransportExpr> for instr::Expr {
    fn from(expr: TransportExpr) -> Self {
        match expr {
            TransportExpr::LitInt(value) => Self::LitInt(value),
            TransportExpr::LitFloat(value) => Self::LitFloat(value),
            TransportExpr::LitBool(value) => Self::LitBool(value),
            TransportExpr::LitString(value) => Self::LitString(value),
            TransportExpr::StringTemplate { parts, provenance } => Self::StringTemplate {
                parts: parts.into_iter().map(Into::into).collect(),
                provenance: provenance.map(Into::into),
            },
            TransportExpr::PathTemplate { parts, provenance } => Self::PathTemplate {
                parts: parts.into_iter().map(Into::into).collect(),
                provenance: provenance.map(Into::into),
            },
            TransportExpr::Unit => Self::Unit,
            TransportExpr::Var(name) => Self::Var(name),
            TransportExpr::Binary {
                op,
                left,
                right,
                provenance,
            } => Self::Binary {
                op: op.into(),
                left: Box::new((*left).into()),
                right: Box::new((*right).into()),
                provenance: provenance.map(Into::into),
            },
            TransportExpr::Unary {
                op,
                operand,
                provenance,
            } => Self::Unary {
                op: op.into(),
                operand: Box::new((*operand).into()),
                provenance: provenance.map(Into::into),
            },
            TransportExpr::Block(block) => Self::Block(block.into()),
            TransportExpr::If {
                cond,
                then_branch,
                else_branch,
                provenance,
            } => Self::If {
                cond: Box::new((*cond).into()),
                then_branch: then_branch.into(),
                else_branch: else_branch.into(),
                provenance: provenance.map(Into::into),
            },
            TransportExpr::Closure { closure_id, def } => Self::Closure {
                closure_id,
                def: Arc::new(def.into()),
            },
            TransportExpr::Call {
                func,
                args,
                kwargs,
                kwargs_expr,
                provenance,
            } => Self::Call {
                func: Box::new((*func).into()),
                args: args.into_iter().map(Into::into).collect(),
                kwargs: kwargs
                    .into_iter()
                    .map(|(name, value)| (name, value.into()))
                    .collect(),
                kwargs_expr: kwargs_expr.map(|expr| Box::new((*expr).into())),
                provenance: provenance.map(Into::into),
            },
            TransportExpr::PartialCall {
                func,
                args,
                kwargs,
                kwargs_expr,
                provenance,
            } => Self::PartialCall {
                func: Box::new((*func).into()),
                args: args.into_iter().map(Into::into).collect(),
                kwargs: kwargs
                    .into_iter()
                    .map(|(name, value)| (name, value.into()))
                    .collect(),
                kwargs_expr: kwargs_expr.map(|expr| Box::new((*expr).into())),
                provenance: provenance.map(Into::into),
            },
            TransportExpr::StructLit { name, fields } => Self::StructLit {
                name,
                fields: fields
                    .into_iter()
                    .map(|(field, value)| (field, value.into()))
                    .collect(),
            },
            TransportExpr::Record(items) => {
                Self::Record(items.into_iter().map(Into::into).collect())
            }
            TransportExpr::Array(values) => Self::Array(
                values
                    .into_iter()
                    .map(|value| match value {
                        TransportArrayItem::Expr(expr) => instr::ArrayItem::Expr(expr.into()),
                        TransportArrayItem::Spread(expr) => instr::ArrayItem::Spread(expr.into()),
                    })
                    .collect(),
            ),
            TransportExpr::Map(entries) => Self::Map(
                entries
                    .into_iter()
                    .map(|(key, value)| (key.into(), value.into()))
                    .collect(),
            ),
            TransportExpr::Field {
                receiver,
                field,
                provenance,
            } => Self::Field {
                receiver: Box::new((*receiver).into()),
                field,
                provenance: provenance.map(Into::into),
            },
            TransportExpr::Fail { value, provenance } => Self::Fail {
                message: Box::new((*value).into()),
                provenance: provenance.map(Into::into),
            },
            TransportExpr::Fallible(value) => Self::Fallible(Box::new((*value).into())),
            TransportExpr::Tuple(values) => {
                Self::Tuple(values.into_iter().map(Into::into).collect())
            }
        }
    }
}

impl From<instr::BinOp> for TransportBinOp {
    fn from(op: instr::BinOp) -> Self {
        match op {
            instr::BinOp::Add => Self::Add,
            instr::BinOp::Sub => Self::Sub,
            instr::BinOp::Mul => Self::Mul,
            instr::BinOp::Div => Self::Div,
            instr::BinOp::Mod => Self::Mod,
            instr::BinOp::Eq => Self::Eq,
            instr::BinOp::Ne => Self::Ne,
            instr::BinOp::Lt => Self::Lt,
            instr::BinOp::Le => Self::Le,
            instr::BinOp::Gt => Self::Gt,
            instr::BinOp::Ge => Self::Ge,
            instr::BinOp::And => Self::And,
            instr::BinOp::Or => Self::Or,
        }
    }
}

impl From<TransportBinOp> for instr::BinOp {
    fn from(op: TransportBinOp) -> Self {
        match op {
            TransportBinOp::Add => Self::Add,
            TransportBinOp::Sub => Self::Sub,
            TransportBinOp::Mul => Self::Mul,
            TransportBinOp::Div => Self::Div,
            TransportBinOp::Mod => Self::Mod,
            TransportBinOp::Eq => Self::Eq,
            TransportBinOp::Ne => Self::Ne,
            TransportBinOp::Lt => Self::Lt,
            TransportBinOp::Le => Self::Le,
            TransportBinOp::Gt => Self::Gt,
            TransportBinOp::Ge => Self::Ge,
            TransportBinOp::And => Self::And,
            TransportBinOp::Or => Self::Or,
        }
    }
}

impl From<instr::UnaryOp> for TransportUnaryOp {
    fn from(op: instr::UnaryOp) -> Self {
        match op {
            instr::UnaryOp::Neg => Self::Neg,
            instr::UnaryOp::Not => Self::Not,
        }
    }
}

impl From<TransportUnaryOp> for instr::UnaryOp {
    fn from(op: TransportUnaryOp) -> Self {
        match op {
            TransportUnaryOp::Neg => Self::Neg,
            TransportUnaryOp::Not => Self::Not,
        }
    }
}

impl TryFrom<&FunctionValue> for ShippedFunction {
    type Error = String;

    fn try_from(function: &FunctionValue) -> Result<Self, Self::Error> {
        let captures = function
            .captures
            .iter()
            .map(|(name, value)| Ok((name.to_string(), TransportValue::try_from(value)?)))
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Self {
            code_identity: function.code_identity.to_string(),
            def: TransportFnDef::from(function.def.as_ref()),
            captures,
        })
    }
}

impl ShippedFunction {
    pub(crate) fn to_runtime(&self) -> Result<FunctionValue, String> {
        let captures = self
            .captures
            .iter()
            .map(|(name, value)| {
                Ok((
                    Arc::<str>::from(name.as_str()),
                    Value::try_from(value.clone())?,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(FunctionValue {
            def: Arc::new(self.def.clone().into()),
            code_identity: Arc::from(self.code_identity.as_str()),
            captures: Arc::from(captures),
        })
    }
}

impl TryFrom<&Value> for TransportValue {
    type Error = String;

    fn try_from(value: &Value) -> Result<Self, Self::Error> {
        match value {
            Value::Pending(_) => Err("cannot transport pending value".into()),
            Value::U64(value) => Ok(Self::U64(*value)),
            Value::I64(value) => Ok(Self::I64(*value)),
            Value::F64(value) => Ok(Self::F64(*value)),
            Value::Bool(value) => Ok(Self::Bool(*value)),
            Value::String(value) => Ok(Self::String(value.to_string())),
            Value::Path(path) => Ok(Self::Path(path.as_str().to_string())),
            Value::ExecPath(_) => Err("cannot transport ExecPath values".into()),
            Value::Blob(blob) => Ok(Self::Blob(TransportBlob::try_from(blob)?)),
            Value::Bytes(bytes) => Ok(Self::Bytes(bytes.as_ref().to_vec())),
            Value::Tree(tree) => Ok(Self::Tree(TransportTree::try_from(tree)?)),
            Value::ExecutorClass(label) => Ok(Self::ExecutorClass(label.to_string())),
            Value::Host(_) => Err("cannot transport Host values".into()),
            Value::Discovered(discovered) => Ok(Self::Discovered {
                value: Box::new(TransportValue::try_from(discovered.value.as_ref())?),
                obligations: discovered.obligations.iter().cloned().collect(),
            }),
            Value::Unit => Ok(Self::Unit),
            Value::Tuple(values) => Ok(Self::Tuple(
                values
                    .iter()
                    .map(TransportValue::try_from)
                    .collect::<Result<Vec<_>, String>>()?,
            )),
            Value::Array(values) => Ok(Self::Array(
                values
                    .iter()
                    .map(TransportValue::try_from)
                    .collect::<Result<Vec<_>, String>>()?,
            )),
            Value::Map(map) => Ok(Self::Map(
                map.entries
                    .iter()
                    .map(|(key, value)| {
                        Ok((
                            TransportValue::try_from(key)?,
                            TransportValue::try_from(value)?,
                        ))
                    })
                    .collect::<Result<Vec<_>, String>>()?,
            )),
            Value::Struct(value) => Ok(Self::Struct(TransportStruct::try_from(value)?)),
            Value::Function(function) => Ok(Self::Function(ShippedFunction::try_from(function)?)),
            Value::Partial(_) => Err("cannot transport partially applied values".into()),
            Value::Builtin(value) => Ok(Self::Builtin(TransportBuiltinValue::try_from(value)?)),
        }
    }
}

impl TryFrom<TransportValue> for Value {
    type Error = String;

    fn try_from(value: TransportValue) -> Result<Self, Self::Error> {
        match value {
            TransportValue::U64(value) => Ok(Self::U64(value)),
            TransportValue::I64(value) => Ok(Self::I64(value)),
            TransportValue::F64(value) => Ok(Self::F64(value)),
            TransportValue::Bool(value) => Ok(Self::Bool(value)),
            TransportValue::String(value) => Ok(Self::String(Arc::from(value))),
            TransportValue::Path(path) => Ok(Self::Path(PathValue::new(path))),
            TransportValue::Blob(blob) => Ok(Self::Blob(blob.into())),
            TransportValue::Bytes(bytes) => Ok(Self::Bytes(Arc::from(bytes))),
            TransportValue::Tree(tree) => Ok(Self::Tree(tree.try_into()?)),
            TransportValue::ExecutorClass(label) => Ok(Self::ExecutorClass(Arc::from(label))),
            TransportValue::Discovered { value, obligations } => {
                Ok(Self::Discovered(DiscoveredValue {
                    value: Box::new(Value::try_from(*value)?),
                    obligations: Arc::from(obligations),
                }))
            }
            TransportValue::Unit => Ok(Self::Unit),
            TransportValue::Tuple(values) => Ok(Self::Tuple(Arc::from(
                values
                    .into_iter()
                    .map(Value::try_from)
                    .collect::<Result<Vec<_>, String>>()?,
            ))),
            TransportValue::Array(values) => Ok(Self::Array(Arc::from(
                values
                    .into_iter()
                    .map(Value::try_from)
                    .collect::<Result<Vec<_>, String>>()?,
            ))),
            TransportValue::Map(entries) => Ok(Self::Map(MapValue {
                entries: Arc::from(
                    entries
                        .into_iter()
                        .map(|(key, value)| Ok((Value::try_from(key)?, Value::try_from(value)?)))
                        .collect::<Result<Vec<_>, String>>()?,
                ),
            })),
            TransportValue::Struct(value) => Ok(Self::Struct(value.try_into()?)),
            TransportValue::Function(function) => Ok(Self::Function(function.to_runtime()?)),
            TransportValue::Builtin(value) => Ok(Self::Builtin(value.try_into()?)),
        }
    }
}

impl TryFrom<&BlobValue> for TransportBlob {
    type Error = String;

    fn try_from(blob: &BlobValue) -> Result<Self, Self::Error> {
        match blob {
            BlobValue::Inline { bytes, executable } => Ok(Self::Inline {
                bytes: bytes.to_vec(),
                executable: *executable,
            }),
            BlobValue::FsFile { path, .. } => Err(format!(
                "cannot transport filesystem-backed blob {} yet",
                path.as_str()
            )),
            BlobValue::Store {
                blob_ref,
                executable,
            } => Ok(Self::Store {
                blob_ref: blob_ref.clone(),
                executable: *executable,
            }),
        }
    }
}

impl From<TransportBlob> for BlobValue {
    fn from(blob: TransportBlob) -> Self {
        match blob {
            TransportBlob::Inline { bytes, executable } => Self::Inline {
                bytes: Arc::from(bytes),
                executable,
            },
            TransportBlob::Store {
                blob_ref,
                executable,
            } => Self::Store {
                blob_ref,
                executable,
            },
        }
    }
}

impl PartialEq for TransportBlob {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                TransportBlob::Inline {
                    bytes: a_bytes,
                    executable: a_exec,
                },
                TransportBlob::Inline {
                    bytes: b_bytes,
                    executable: b_exec,
                },
            ) => a_exec == b_exec && a_bytes == b_bytes,
            (
                TransportBlob::Store {
                    blob_ref: a_ref,
                    executable: a_exec,
                },
                TransportBlob::Store {
                    blob_ref: b_ref,
                    executable: b_exec,
                },
            ) => a_exec == b_exec && blob_ref_eq(a_ref, b_ref),
            _ => false,
        }
    }
}

impl Eq for TransportBlob {}

impl TryFrom<&TreeValue> for TransportTree {
    type Error = String;

    fn try_from(tree: &TreeValue) -> Result<Self, Self::Error> {
        match tree {
            TreeValue::Inline(entries) => Ok(Self::Inline(
                entries
                    .iter()
                    .map(|(name, value)| Ok((name.to_string(), TransportValue::try_from(value)?)))
                    .collect::<Result<Vec<_>, String>>()?,
            )),
            TreeValue::FsDir(path) => Err(format!(
                "cannot transport filesystem-backed tree {} yet",
                path.as_str()
            )),
            TreeValue::Store(root) => Ok(Self::Store(*root)),
        }
    }
}

impl TryFrom<TransportTree> for TreeValue {
    type Error = String;

    fn try_from(tree: TransportTree) -> Result<Self, Self::Error> {
        match tree {
            TransportTree::Inline(entries) => Ok(Self::Inline(Arc::from(
                entries
                    .into_iter()
                    .map(|(name, value)| Ok((Arc::<str>::from(name), Value::try_from(value)?)))
                    .collect::<Result<Vec<_>, String>>()?,
            ))),
            TransportTree::Store(root) => Ok(Self::Store(root)),
        }
    }
}

fn blob_ref_eq(left: &BlobRef, right: &BlobRef) -> bool {
    match (left, right) {
        (
            BlobRef::Small {
                chunk: left_chunk,
                size: left_size,
            },
            BlobRef::Small {
                chunk: right_chunk,
                size: right_size,
            },
        ) => left_chunk == right_chunk && left_size == right_size,
        (
            BlobRef::Large {
                blob_node: left_blob_node,
                content_hash: left_content_hash,
                size: left_size,
            },
            BlobRef::Large {
                blob_node: right_blob_node,
                content_hash: right_content_hash,
                size: right_size,
            },
        ) => {
            left_blob_node == right_blob_node
                && left_content_hash == right_content_hash
                && left_size == right_size
        }
        _ => false,
    }
}

impl TryFrom<&StructValue> for TransportStruct {
    type Error = String;

    fn try_from(value: &StructValue) -> Result<Self, Self::Error> {
        Ok(Self {
            type_name: value.type_name.to_string(),
            fields: value
                .fields
                .iter()
                .map(|(name, value)| Ok((name.to_string(), TransportValue::try_from(value)?)))
                .collect::<Result<Vec<_>, String>>()?,
        })
    }
}

impl TryFrom<TransportStruct> for StructValue {
    type Error = String;

    fn try_from(value: TransportStruct) -> Result<Self, Self::Error> {
        Ok(Self {
            type_name: Arc::from(value.type_name),
            fields: Arc::from(
                value
                    .fields
                    .into_iter()
                    .map(|(name, value)| Ok((Arc::<str>::from(name), Value::try_from(value)?)))
                    .collect::<Result<Vec<_>, String>>()?,
            ),
        })
    }
}

impl TryFrom<&BuiltinValue> for TransportBuiltinValue {
    type Error = String;

    fn try_from(value: &BuiltinValue) -> Result<Self, Self::Error> {
        match value {
            BuiltinValue::Namespace(BuiltinNamespace::Blob) => Ok(Self::BlobNamespace),
            BuiltinValue::Namespace(BuiltinNamespace::Bytes) => Ok(Self::BytesNamespace),
            BuiltinValue::Namespace(BuiltinNamespace::Tree) => Ok(Self::TreeNamespace),
            BuiltinValue::Function(BuiltinFunction::Exec) => Ok(Self::ExecFunction),
            BuiltinValue::Function(BuiltinFunction::Discover) => Ok(Self::DiscoverFunction),
            BuiltinValue::Function(BuiltinFunction::ExecutorClass) => {
                Ok(Self::ExecutorClassFunction)
            }
            BuiltinValue::Function(BuiltinFunction::StructConstructor { type_name, fields }) => {
                Ok(Self::StructConstructor {
                    type_name: type_name.to_string(),
                    fields: fields.iter().map(|field| field.to_string()).collect(),
                })
            }
            BuiltinValue::Method(BuiltinMethod::BlobFromString) => Ok(Self::BlobFromString),
            BuiltinValue::Method(BuiltinMethod::BytesFromString) => Ok(Self::BytesFromString),
            BuiltinValue::Method(BuiltinMethod::TreeOf) => Ok(Self::TreeOf),
            BuiltinValue::Method(BuiltinMethod::TreeEmpty) => Ok(Self::TreeEmpty),
            BuiltinValue::Method(BuiltinMethod::TreeFromPath) => Ok(Self::TreeFromPath),
            BuiltinValue::Method(BuiltinMethod::BlobToString { receiver }) => {
                Ok(Self::BlobToString {
                    receiver: Box::new(TransportValue::try_from(receiver.as_ref())?),
                })
            }
            BuiltinValue::Method(BuiltinMethod::BlobExecutable { receiver }) => {
                Ok(Self::BlobExecutable {
                    receiver: Box::new(TransportValue::try_from(receiver.as_ref())?),
                })
            }
            BuiltinValue::Method(BuiltinMethod::BlobUtf8 { receiver }) => Ok(Self::BlobUtf8 {
                receiver: Box::new(TransportValue::try_from(receiver.as_ref())?),
            }),
            BuiltinValue::Method(BuiltinMethod::BlobUtf8Lossy { receiver }) => {
                Ok(Self::BlobUtf8Lossy {
                    receiver: Box::new(TransportValue::try_from(receiver.as_ref())?),
                })
            }
            BuiltinValue::Method(BuiltinMethod::BytesUtf8 { receiver }) => Ok(Self::BytesUtf8 {
                receiver: Box::new(TransportValue::try_from(receiver.as_ref())?),
            }),
            BuiltinValue::Method(BuiltinMethod::BytesUtf8Lossy { receiver }) => {
                Ok(Self::BytesUtf8Lossy {
                    receiver: Box::new(TransportValue::try_from(receiver.as_ref())?),
                })
            }
            BuiltinValue::Method(BuiltinMethod::BytesBlob { receiver }) => Ok(Self::BytesBlob {
                receiver: Box::new(TransportValue::try_from(receiver.as_ref())?),
            }),
            BuiltinValue::Method(BuiltinMethod::StreamCollect { receiver }) => {
                Ok(Self::StreamCollect {
                    receiver: Box::new(TransportValue::try_from(receiver.as_ref())?),
                })
            }
            BuiltinValue::Method(BuiltinMethod::TreeFile { receiver }) => Ok(Self::TreeFile {
                receiver: Box::new(TransportValue::try_from(receiver.as_ref())?),
            }),
            BuiltinValue::Method(BuiltinMethod::TreeMerge { receiver }) => Ok(Self::TreeMerge {
                receiver: Box::new(TransportValue::try_from(receiver.as_ref())?),
            }),
            BuiltinValue::Method(BuiltinMethod::TreeSubtree { receiver }) => {
                Ok(Self::TreeSubtree {
                    receiver: Box::new(TransportValue::try_from(receiver.as_ref())?),
                })
            }
            BuiltinValue::Method(BuiltinMethod::FallibleCall { receiver }) => {
                Ok(Self::FallibleCall {
                    receiver: Box::new(TransportValue::try_from(receiver.as_ref())?),
                })
            }
            BuiltinValue::Method(BuiltinMethod::ArrayMap { .. })
            | BuiltinValue::Method(BuiltinMethod::ArrayToMap { .. })
            | BuiltinValue::Method(BuiltinMethod::ArrayFlatten { .. })
            | BuiltinValue::Method(BuiltinMethod::ArrayMerge { .. })
            | BuiltinValue::Method(BuiltinMethod::MapMapValues { .. })
            | BuiltinValue::Method(BuiltinMethod::OptionUnwrapOr { .. })
            | BuiltinValue::Method(BuiltinMethod::HostRead { .. })
            | BuiltinValue::Method(BuiltinMethod::HostHash { .. })
            | BuiltinValue::Method(BuiltinMethod::HostWhich { .. }) => {
                Err("cannot transport host builtin methods".into())
            }
        }
    }
}

impl TryFrom<TransportBuiltinValue> for BuiltinValue {
    type Error = String;

    fn try_from(value: TransportBuiltinValue) -> Result<Self, Self::Error> {
        match value {
            TransportBuiltinValue::BlobNamespace => Ok(Self::Namespace(BuiltinNamespace::Blob)),
            TransportBuiltinValue::BytesNamespace => Ok(Self::Namespace(BuiltinNamespace::Bytes)),
            TransportBuiltinValue::TreeNamespace => Ok(Self::Namespace(BuiltinNamespace::Tree)),
            TransportBuiltinValue::ExecFunction => Ok(Self::Function(BuiltinFunction::Exec)),
            TransportBuiltinValue::DiscoverFunction => {
                Ok(Self::Function(BuiltinFunction::Discover))
            }
            TransportBuiltinValue::ExecutorClassFunction => {
                Ok(Self::Function(BuiltinFunction::ExecutorClass))
            }
            TransportBuiltinValue::StructConstructor { type_name, fields } => {
                Ok(Self::Function(BuiltinFunction::StructConstructor {
                    type_name: Arc::from(type_name),
                    fields: Arc::from(
                        fields
                            .into_iter()
                            .map(|field| Arc::<str>::from(field))
                            .collect::<Vec<_>>(),
                    ),
                }))
            }
            TransportBuiltinValue::BlobFromString => {
                Ok(Self::Method(BuiltinMethod::BlobFromString))
            }
            TransportBuiltinValue::BytesFromString => {
                Ok(Self::Method(BuiltinMethod::BytesFromString))
            }
            TransportBuiltinValue::TreeOf => Ok(Self::Method(BuiltinMethod::TreeOf)),
            TransportBuiltinValue::TreeEmpty => Ok(Self::Method(BuiltinMethod::TreeEmpty)),
            TransportBuiltinValue::TreeFromPath => Ok(Self::Method(BuiltinMethod::TreeFromPath)),
            TransportBuiltinValue::BlobToString { receiver } => {
                Ok(Self::Method(BuiltinMethod::BlobToString {
                    receiver: Box::new(Value::try_from(*receiver)?),
                }))
            }
            TransportBuiltinValue::BlobExecutable { receiver } => {
                Ok(Self::Method(BuiltinMethod::BlobExecutable {
                    receiver: Box::new(Value::try_from(*receiver)?),
                }))
            }
            TransportBuiltinValue::BlobUtf8 { receiver } => {
                Ok(Self::Method(BuiltinMethod::BlobUtf8 {
                    receiver: Box::new(Value::try_from(*receiver)?),
                }))
            }
            TransportBuiltinValue::BlobUtf8Lossy { receiver } => {
                Ok(Self::Method(BuiltinMethod::BlobUtf8Lossy {
                    receiver: Box::new(Value::try_from(*receiver)?),
                }))
            }
            TransportBuiltinValue::BytesUtf8 { receiver } => {
                Ok(Self::Method(BuiltinMethod::BytesUtf8 {
                    receiver: Box::new(Value::try_from(*receiver)?),
                }))
            }
            TransportBuiltinValue::BytesUtf8Lossy { receiver } => {
                Ok(Self::Method(BuiltinMethod::BytesUtf8Lossy {
                    receiver: Box::new(Value::try_from(*receiver)?),
                }))
            }
            TransportBuiltinValue::BytesBlob { receiver } => {
                Ok(Self::Method(BuiltinMethod::BytesBlob {
                    receiver: Box::new(Value::try_from(*receiver)?),
                }))
            }
            TransportBuiltinValue::StreamCollect { receiver } => {
                Ok(Self::Method(BuiltinMethod::StreamCollect {
                    receiver: Box::new(Value::try_from(*receiver)?),
                }))
            }
            TransportBuiltinValue::TreeFile { receiver } => {
                Ok(Self::Method(BuiltinMethod::TreeFile {
                    receiver: Box::new(Value::try_from(*receiver)?),
                }))
            }
            TransportBuiltinValue::TreeMerge { receiver } => {
                Ok(Self::Method(BuiltinMethod::TreeMerge {
                    receiver: Box::new(Value::try_from(*receiver)?),
                }))
            }
            TransportBuiltinValue::TreeSubtree { receiver } => {
                Ok(Self::Method(BuiltinMethod::TreeSubtree {
                    receiver: Box::new(Value::try_from(*receiver)?),
                }))
            }
            TransportBuiltinValue::FallibleCall { receiver } => {
                Ok(Self::Method(BuiltinMethod::FallibleCall {
                    receiver: Box::new(Value::try_from(*receiver)?),
                }))
            }
        }
    }
}
