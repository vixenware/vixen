//! Vixen strict IR types consumed by the Cranelift backend.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictType {
    U64,
    Bool,
    String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringOwnership {
    /// Pointer into static/leaked data. Never freed.
    Static,
    /// Slice of another string (e.g. result of StringSlice). Never freed.
    Borrowed,
    /// Heap-allocated by ConcatStrings. Caller must free after use.
    Owned,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalInfo {
    pub id: LocalId,
    pub ty: StrictType,
    pub ownership: StringOwnership,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrictLiteral {
    U64(u64),
    Bool(bool),
    /// Static string data; ownership = Static.
    String(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictBinOp {
    Add,
    Sub,
    Eq,
    Ne,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrictExpr {
    Literal(StrictLiteral),
    Local(LocalId),
    Binary {
        op: StrictBinOp,
        lhs: Box<StrictExpr>,
        rhs: Box<StrictExpr>,
    },
    StringLen {
        value: Box<StrictExpr>,
    },
    StringSlice {
        value: Box<StrictExpr>,
        start: Box<StrictExpr>,
        end: Box<StrictExpr>,
    },
    ConcatStrings {
        parts: Vec<StrictExpr>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrictStmt {
    Let {
        local: LocalId,
        value: StrictExpr,
    },
    If {
        condition: StrictExpr,
        then_body: Vec<StrictStmt>,
        else_body: Vec<StrictStmt>,
    },
    Return(Option<StrictExpr>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictParam {
    pub local: LocalId,
    pub name: String,
    pub ty: StrictType,
    // String params are always Borrowed — no ownership field needed.
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictFunction {
    pub name: String,
    pub params: Vec<StrictParam>,
    pub locals: Vec<LocalInfo>,
    pub return_type: StrictType,
    /// Meaningful only when return_type == String.
    pub return_ownership: StringOwnership,
    pub body: Vec<StrictStmt>,
}
