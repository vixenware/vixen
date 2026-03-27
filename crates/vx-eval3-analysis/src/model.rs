use std::collections::{BTreeMap, BTreeSet};

use vx_resolve::DefId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BodyId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExprId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UseSiteId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KernelId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeNodeId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LayoutId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FieldKey {
    Index(u32),
    Name(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    NoObs,
    NeedValue,
    NeedCallable,
    NeedProduct(BTreeMap<FieldKey, Observation>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeBoundaryKind {
    Exec,
    Discover,
    SharedKeyed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractedNodeReason {
    SharedAcrossUses,
    EscapesAsLazyResult,
    CapturedByClosureEnv { layout: LayoutId },
    LazyProductField { field: FieldKey },
    RuntimeBoundary { kind: RuntimeBoundaryKind },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceExpr {
    pub body: BodyId,
    pub expr: ExprId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedNode {
    pub id: RuntimeNodeId,
    pub source: SourceExpr,
    pub reason: ExtractedNodeReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelParam {
    pub name: String,
    pub source: Option<SourceExpr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelRegion {
    pub id: KernelId,
    pub body: BodyId,
    pub exprs: BTreeSet<ExprId>,
    pub params: Vec<KernelParam>,
    pub result_obs: Observation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelEntrypoint {
    pub body: BodyId,
    pub source: SourceExpr,
    pub kernel: KernelId,
    pub demanded_result: Observation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSlot {
    pub name: String,
    pub source: SourceExpr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosureEnvLayout {
    pub id: LayoutId,
    pub captures: Vec<CaptureSlot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameLayout {
    pub id: LayoutId,
    pub slots: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedKeyedNode {
    pub key_expr: SourceExpr,
    pub request_expr: SourceExpr,
    pub join_expr: SourceExpr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeNode {
    Lazy {
        id: RuntimeNodeId,
        source: SourceExpr,
    },
    Exec {
        id: RuntimeNodeId,
        source: SourceExpr,
    },
    Discover {
        id: RuntimeNodeId,
        source: SourceExpr,
    },
    SharedKeyed {
        id: RuntimeNodeId,
        source: SourceExpr,
        keyed: SharedKeyedNode,
    },
}

impl RuntimeNode {
    pub fn id(&self) -> RuntimeNodeId {
        match self {
            Self::Lazy { id, .. } => *id,
            Self::Exec { id, .. } => *id,
            Self::Discover { id, .. } => *id,
            Self::SharedKeyed { id, .. } => *id,
        }
    }

    pub fn boundary_kind(&self) -> Option<RuntimeBoundaryKind> {
        match self {
            Self::Lazy { .. } => None,
            Self::Exec { .. } => Some(RuntimeBoundaryKind::Exec),
            Self::Discover { .. } => Some(RuntimeBoundaryKind::Discover),
            Self::SharedKeyed { .. } => Some(RuntimeBoundaryKind::SharedKeyed),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyzedBody {
    pub id: BodyId,
    pub result_observation: Observation,
    pub use_observations: BTreeMap<UseSiteId, Observation>,
    pub callee_targets: BTreeMap<ExprId, DefId>,
    pub callee_local_origins: BTreeMap<ExprId, SourceLocalCalleeOrigin>,
    pub source_local_call_edges: Vec<SourceLocalCallEdge>,
    pub extracted_nodes: Vec<ExtractedNode>,
    pub kernel_entries: Vec<KernelEntrypoint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceLocalCalleeOrigin {
    TopLevelBody(BodyId),
    LocalClosure(SourceExpr),
    LocalDef(DefId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLocalCallEdge {
    pub call_expr: ExprId,
    pub callee: SourceLocalCalleeOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AnalysisResult {
    pub bodies: Vec<AnalyzedBody>,
    pub kernels: Vec<KernelRegion>,
    pub runtime_nodes: Vec<RuntimeNode>,
    pub closure_layouts: Vec<ClosureEnvLayout>,
    pub frame_layouts: Vec<FrameLayout>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnalysisError {
    DuplicateBody(BodyId),
    DuplicateKernel(KernelId),
    DuplicateRuntimeNode(RuntimeNodeId),
    MissingKernel {
        body: BodyId,
        kernel: KernelId,
    },
    MissingRuntimeNode(RuntimeNodeId),
    BoundaryKindMismatch {
        node: RuntimeNodeId,
        reason: RuntimeBoundaryKind,
        runtime: Option<RuntimeBoundaryKind>,
    },
}

impl AnalysisResult {
    /// Validate core analysis invariants.
    ///
    /// This keeps the "intended invariants" explicit at the data-model level,
    /// without coupling to any old eval3 lowering/runtime implementation.
    pub fn validate(&self) -> Result<(), AnalysisError> {
        let mut seen_bodies = BTreeSet::new();
        for body in &self.bodies {
            if !seen_bodies.insert(body.id) {
                return Err(AnalysisError::DuplicateBody(body.id));
            }
        }

        let mut kernels = BTreeMap::new();
        for kernel in &self.kernels {
            if kernels.insert(kernel.id, kernel).is_some() {
                return Err(AnalysisError::DuplicateKernel(kernel.id));
            }
        }

        let mut runtime_nodes = BTreeMap::new();
        for node in &self.runtime_nodes {
            let id = node.id();
            if runtime_nodes.insert(id, node).is_some() {
                return Err(AnalysisError::DuplicateRuntimeNode(id));
            }
        }

        for body in &self.bodies {
            for entry in &body.kernel_entries {
                if !kernels.contains_key(&entry.kernel) {
                    return Err(AnalysisError::MissingKernel {
                        body: body.id,
                        kernel: entry.kernel,
                    });
                }
            }

            for extracted in &body.extracted_nodes {
                let Some(runtime) = runtime_nodes.get(&extracted.id) else {
                    return Err(AnalysisError::MissingRuntimeNode(extracted.id));
                };

                if let ExtractedNodeReason::RuntimeBoundary { kind } = extracted.reason {
                    let runtime_kind = runtime.boundary_kind();
                    if runtime_kind != Some(kind) {
                        return Err(AnalysisError::BoundaryKindMismatch {
                            node: extracted.id,
                            reason: kind,
                            runtime: runtime_kind,
                        });
                    }
                }
            }
        }

        Ok(())
    }
}
