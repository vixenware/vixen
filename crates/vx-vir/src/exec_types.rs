//! Executable program types — the output of VIR lowering.
//!
//! These types represent the bytecode-level program consumed by the runtime
//! evaluator (vx-rt).

use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// IDs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BodyId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExprId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KernelId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeNodeId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SlotId(pub u32);

/// Bridge-era binding ID, retained for runtime boundary input specs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BridgeLocalBindingId(pub u32);

// ---------------------------------------------------------------------------
// Source tracking
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceExpr {
    pub body: BodyId,
    pub expr: ExprId,
}

// ---------------------------------------------------------------------------
// Field keys
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FieldKey {
    Index(u32),
    Name(String),
}

// ---------------------------------------------------------------------------
// Runtime boundary kinds
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeBoundaryKind {
    Exec,
    Discover,
    SharedKeyed,
}

// ---------------------------------------------------------------------------
// Program structure
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutableProgram {
    pub entry: BodyId,
    pub bodies: BTreeMap<BodyId, ExecutableBody>,
    pub runtime_nodes: BTreeMap<RuntimeNodeId, ExecutableRuntimeNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum KernelKind {
    #[default]
    Interpreted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutableBody {
    pub id: BodyId,
    pub kernel: KernelId,
    pub kernel_kind: KernelKind,
    pub params: Vec<SlotId>,
    pub ops: Vec<KernelOp>,
    pub result_source: ResultSource,
    pub runtime_boundary_extractions: Vec<RuntimeNodeId>,
    pub local_call_dependencies: Vec<BodyId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutableClosure {
    pub params: Vec<SlotId>,
    pub ops: Vec<KernelOp>,
    pub result_source: ResultSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutableRuntimeNode {
    pub id: RuntimeNodeId,
    pub kind: RuntimeBoundaryKind,
    pub source: SourceExpr,
    pub payload: RuntimeBoundaryPayload,
}

// ---------------------------------------------------------------------------
// Result source
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultSource {
    Slot(SlotId),
    PendingRuntimeBoundary,
}

// ---------------------------------------------------------------------------
// Operands
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operand {
    Slot(SlotId),
    Int(i64),
    String(String),
}

// ---------------------------------------------------------------------------
// Runtime boundary types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeBoundaryPayload {
    Exec(ExecBoundaryPayload),
    Discover(DiscoverBoundaryPayload),
    SharedKeyed(SharedKeyedBoundaryPayload),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecBoundaryPayload {
    pub positional_arg_count: usize,
    pub has_where_clause: bool,
    pub has_mounts: bool,
    pub has_writable: bool,
    pub has_create_dirs: bool,
    pub has_coarse_mounts: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoverBoundaryPayload {
    pub positional_arg_count: usize,
    pub has_closure_arg: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedKeyedBoundaryPayload {
    pub positional_arg_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeField {
    ExitCode,
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeMemberOp {
    Collect,
    Blob,
    Which,
    ExecSubpath,
    ReadSubpath,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TreeFilePath(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeBoundaryInputSpec {
    Binding {
        binding: BridgeLocalBindingId,
        slot: SlotId,
    },
    UnresolvedBinding {
        binding: BridgeLocalBindingId,
    },
    IntLiteral(i64),
    StringLiteral(String),
    PathLiteral(TreeFilePath),
    Closure {
        source: SourceExpr,
        closure: ExecutableClosure,
    },
    Array(Vec<RuntimeBoundaryInputSpec>),
    OpaqueExpr {
        source: SourceExpr,
    },
}

// ---------------------------------------------------------------------------
// KernelOp — the bytecode
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KernelOp {
    AssignHostLocal {
        dst: SlotId,
        source: SourceExpr,
    },
    AssignSandboxTreeRoot {
        dst: SlotId,
        source: SourceExpr,
        node: RuntimeNodeId,
    },
    AssignConstInt {
        dst: SlotId,
        source: SourceExpr,
        value: i64,
    },
    AssignConstString {
        dst: SlotId,
        source: SourceExpr,
        value: String,
    },
    MakeBlob {
        dst: SlotId,
        source: SourceExpr,
        value: Operand,
    },
    AssignAlias {
        dst: SlotId,
        source: SourceExpr,
        value: Operand,
    },
    AssignLazyPoison {
        dst: SlotId,
        source: SourceExpr,
        label: String,
    },
    AllocThunk {
        dst: SlotId,
        source: SourceExpr,
        thunk_body: BodyId,
        captures: Vec<Operand>,
    },
    AllocClosure {
        dst: SlotId,
        source: SourceExpr,
        body: BodyId,
        captures: Vec<Operand>,
        param_count: usize,
    },
    AssignCallableAddConst {
        dst: SlotId,
        source: SourceExpr,
        addend: Operand,
    },
    ApplyCallable {
        dst: SlotId,
        source: SourceExpr,
        callable: Operand,
        arg: Operand,
    },
    ApplyClosure {
        dst: SlotId,
        source: SourceExpr,
        closure: Operand,
        args: Vec<Operand>,
    },
    MakePair {
        dst: SlotId,
        source: SourceExpr,
        first: Operand,
        second: Operand,
    },
    MakeStruct {
        dst: SlotId,
        source: SourceExpr,
        fields: Vec<(String, Operand)>,
    },
    ProjectIndex {
        dst: SlotId,
        source: SourceExpr,
        pair: Operand,
        index: usize,
    },
    ProjectNamedField {
        dst: SlotId,
        source: SourceExpr,
        value: Operand,
        field: String,
    },
    Add {
        dst: SlotId,
        source: SourceExpr,
        lhs: Operand,
        rhs: Operand,
    },
    AssignConstBool {
        dst: SlotId,
        source: SourceExpr,
        value: bool,
    },
    Equal {
        dst: SlotId,
        source: SourceExpr,
        lhs: Operand,
        rhs: Operand,
    },
    NotEqual {
        dst: SlotId,
        source: SourceExpr,
        lhs: Operand,
        rhs: Operand,
    },
    Sub {
        dst: SlotId,
        source: SourceExpr,
        lhs: Operand,
        rhs: Operand,
    },
    ProjectField {
        dst: SlotId,
        source: SourceExpr,
        value: Operand,
        field: RuntimeField,
    },
    CallMember {
        dst: SlotId,
        source: SourceExpr,
        receiver: Operand,
        method: RuntimeMemberOp,
        args: Vec<Operand>,
    },
    BuildTree {
        dst: SlotId,
        source: SourceExpr,
        entries: Vec<(TreeFilePath, Operand)>,
    },
    MakeArray {
        dst: SlotId,
        source: SourceExpr,
        items: Vec<Operand>,
    },
    ConcatStrings {
        dst: SlotId,
        source: SourceExpr,
        parts: Vec<Operand>,
    },
    StringLen {
        dst: SlotId,
        source: SourceExpr,
        value: Operand,
    },
    StringSlice {
        dst: SlotId,
        source: SourceExpr,
        value: Operand,
        start: Operand,
        end: Operand,
    },
    Select {
        dst: SlotId,
        source: SourceExpr,
        condition: Operand,
        then_value: Operand,
        else_value: Operand,
    },
    AssignFailure {
        dst: SlotId,
        source: SourceExpr,
        message: Operand,
    },
    CallBody {
        dst: SlotId,
        source: SourceExpr,
        callee: BodyId,
        args: Vec<Operand>,
    },
    AwaitRuntime {
        dst: SlotId,
        source: SourceExpr,
        node: RuntimeNodeId,
        positional_inputs: Vec<RuntimeBoundaryInputSpec>,
        mounts: Vec<(TreeFilePath, RuntimeBoundaryInputSpec)>,
        writable_roots: Vec<TreeFilePath>,
        create_dirs: Vec<TreeFilePath>,
        coarse_mounts: Vec<RuntimeBoundaryInputSpec>,
    },
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecSliceError {
    UnsupportedShape(String),
    MissingBody(BodyId),
    MissingSlot(SlotId),
    MissingExpr(SourceExpr),
    MissingBindingForUse(SourceExpr),
    MissingBindingInitializer(BridgeLocalBindingId),
    TypeMismatch(String),
    VerificationFailure(String),
    InvalidPairIndex(usize),
    InvalidProgram(String),
}
