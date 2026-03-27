use std::collections::{BTreeMap, BTreeSet};

use crate::{
    AnalysisResult, AnalyzedBody, BodyId, CaptureSlot, ClosureEnvLayout, ExprId, ExtractedNode,
    ExtractedNodeReason, FieldKey, KernelEntrypoint, KernelId, KernelParam, KernelRegion, LayoutId,
    Observation, RuntimeBoundaryKind, RuntimeNode, RuntimeNodeId, SourceExpr, UseSiteId,
};

pub fn canonical_program_a_fixture() -> AnalysisResult {
    let main_body = BodyId(0);
    let pair_expr = ExprId(2);
    let projection_expr = ExprId(3);

    let mut pair_obs = BTreeMap::new();
    pair_obs.insert(FieldKey::Index(0), Observation::NeedValue);

    AnalysisResult {
        bodies: vec![AnalyzedBody {
            id: main_body,
            result_observation: Observation::NeedValue,
            use_observations: BTreeMap::from([
                (UseSiteId(0), Observation::NeedProduct(pair_obs)),
                (UseSiteId(1), Observation::NeedValue),
            ]),
            callee_targets: BTreeMap::new(),
            callee_local_origins: BTreeMap::new(),
            extracted_nodes: vec![ExtractedNode {
                id: RuntimeNodeId(0),
                source: SourceExpr {
                    body: main_body,
                    expr: pair_expr,
                },
                reason: ExtractedNodeReason::LazyProductField {
                    field: FieldKey::Index(1),
                },
            }],
            kernel_entries: vec![KernelEntrypoint {
                body: main_body,
                source: SourceExpr {
                    body: main_body,
                    expr: projection_expr,
                },
                kernel: KernelId(0),
                demanded_result: Observation::NeedValue,
            }],
        }],
        kernels: vec![KernelRegion {
            id: KernelId(0),
            body: main_body,
            exprs: BTreeSet::from([ExprId(0), ExprId(1), projection_expr]),
            params: vec![],
            result_obs: Observation::NeedValue,
        }],
        runtime_nodes: vec![RuntimeNode::Lazy {
            id: RuntimeNodeId(0),
            source: SourceExpr {
                body: main_body,
                expr: pair_expr,
            },
        }],
        closure_layouts: vec![],
        frame_layouts: vec![],
    }
}

pub fn canonical_program_b_fixture() -> AnalysisResult {
    let main_body = BodyId(1);
    let add_n_expr = ExprId(11);

    AnalysisResult {
        bodies: vec![AnalyzedBody {
            id: main_body,
            result_observation: Observation::NeedValue,
            use_observations: BTreeMap::from([
                (UseSiteId(10), Observation::NeedCallable),
                (UseSiteId(11), Observation::NeedValue),
                (UseSiteId(12), Observation::NeedValue),
            ]),
            callee_targets: BTreeMap::new(),
            callee_local_origins: BTreeMap::new(),
            extracted_nodes: vec![
                ExtractedNode {
                    id: RuntimeNodeId(10),
                    source: SourceExpr {
                        body: main_body,
                        expr: add_n_expr,
                    },
                    reason: ExtractedNodeReason::CapturedByClosureEnv {
                        layout: LayoutId(10),
                    },
                },
                ExtractedNode {
                    id: RuntimeNodeId(11),
                    source: SourceExpr {
                        body: main_body,
                        expr: ExprId(14),
                    },
                    reason: ExtractedNodeReason::LazyProductField {
                        field: FieldKey::Index(0),
                    },
                },
            ],
            kernel_entries: vec![KernelEntrypoint {
                body: main_body,
                source: SourceExpr {
                    body: main_body,
                    expr: ExprId(15),
                },
                kernel: KernelId(10),
                demanded_result: Observation::NeedValue,
            }],
        }],
        kernels: vec![KernelRegion {
            id: KernelId(10),
            body: main_body,
            exprs: BTreeSet::from([ExprId(12), ExprId(15)]),
            params: vec![KernelParam {
                name: "n".to_owned(),
                source: Some(SourceExpr {
                    body: main_body,
                    expr: ExprId(9),
                }),
            }],
            result_obs: Observation::NeedValue,
        }],
        runtime_nodes: vec![
            RuntimeNode::Lazy {
                id: RuntimeNodeId(10),
                source: SourceExpr {
                    body: main_body,
                    expr: add_n_expr,
                },
            },
            RuntimeNode::Lazy {
                id: RuntimeNodeId(11),
                source: SourceExpr {
                    body: main_body,
                    expr: ExprId(14),
                },
            },
        ],
        closure_layouts: vec![ClosureEnvLayout {
            id: LayoutId(10),
            captures: vec![CaptureSlot {
                name: "n".to_owned(),
                source: SourceExpr {
                    body: main_body,
                    expr: ExprId(9),
                },
            }],
        }],
        frame_layouts: vec![],
    }
}

pub fn canonical_program_c_fixture() -> AnalysisResult {
    let main_body = BodyId(2);
    let plus_one_call = ExprId(20);

    AnalysisResult {
        bodies: vec![AnalyzedBody {
            id: main_body,
            result_observation: Observation::NeedValue,
            use_observations: BTreeMap::from([
                (UseSiteId(20), Observation::NeedValue),
                (UseSiteId(21), Observation::NeedValue),
            ]),
            callee_targets: BTreeMap::new(),
            callee_local_origins: BTreeMap::new(),
            extracted_nodes: vec![ExtractedNode {
                id: RuntimeNodeId(20),
                source: SourceExpr {
                    body: main_body,
                    expr: plus_one_call,
                },
                reason: ExtractedNodeReason::SharedAcrossUses,
            }],
            kernel_entries: vec![KernelEntrypoint {
                body: main_body,
                source: SourceExpr {
                    body: main_body,
                    expr: ExprId(23),
                },
                kernel: KernelId(20),
                demanded_result: Observation::NeedValue,
            }],
        }],
        kernels: vec![KernelRegion {
            id: KernelId(20),
            body: main_body,
            exprs: BTreeSet::from([ExprId(21), ExprId(22), ExprId(23)]),
            params: vec![],
            result_obs: Observation::NeedValue,
        }],
        runtime_nodes: vec![RuntimeNode::Lazy {
            id: RuntimeNodeId(20),
            source: SourceExpr {
                body: main_body,
                expr: plus_one_call,
            },
        }],
        closure_layouts: vec![],
        frame_layouts: vec![],
    }
}

pub fn canonical_program_d_fixture() -> AnalysisResult {
    let apply_body = BodyId(3);
    let main_body = BodyId(4);

    AnalysisResult {
        bodies: vec![
            AnalyzedBody {
                id: apply_body,
                result_observation: Observation::NeedValue,
                use_observations: BTreeMap::from([
                    (UseSiteId(30), Observation::NeedCallable),
                    (UseSiteId(31), Observation::NeedValue),
                ]),
                callee_targets: BTreeMap::new(),
                callee_local_origins: BTreeMap::new(),
                extracted_nodes: vec![],
                kernel_entries: vec![KernelEntrypoint {
                    body: apply_body,
                    source: SourceExpr {
                        body: apply_body,
                        expr: ExprId(31),
                    },
                    kernel: KernelId(30),
                    demanded_result: Observation::NeedValue,
                }],
            },
            AnalyzedBody {
                id: main_body,
                result_observation: Observation::NeedValue,
                use_observations: BTreeMap::from([(UseSiteId(40), Observation::NeedCallable)]),
                callee_targets: BTreeMap::new(),
                callee_local_origins: BTreeMap::new(),
                extracted_nodes: vec![ExtractedNode {
                    id: RuntimeNodeId(30),
                    source: SourceExpr {
                        body: main_body,
                        expr: ExprId(40),
                    },
                    reason: ExtractedNodeReason::CapturedByClosureEnv {
                        layout: LayoutId(30),
                    },
                }],
                kernel_entries: vec![KernelEntrypoint {
                    body: main_body,
                    source: SourceExpr {
                        body: main_body,
                        expr: ExprId(42),
                    },
                    kernel: KernelId(31),
                    demanded_result: Observation::NeedValue,
                }],
            },
        ],
        kernels: vec![
            KernelRegion {
                id: KernelId(30),
                body: apply_body,
                exprs: BTreeSet::from([ExprId(31)]),
                params: vec![
                    KernelParam {
                        name: "f".to_owned(),
                        source: Some(SourceExpr {
                            body: apply_body,
                            expr: ExprId(30),
                        }),
                    },
                    KernelParam {
                        name: "x".to_owned(),
                        source: Some(SourceExpr {
                            body: apply_body,
                            expr: ExprId(32),
                        }),
                    },
                ],
                result_obs: Observation::NeedValue,
            },
            KernelRegion {
                id: KernelId(31),
                body: main_body,
                exprs: BTreeSet::from([ExprId(41), ExprId(42)]),
                params: vec![KernelParam {
                    name: "n".to_owned(),
                    source: Some(SourceExpr {
                        body: main_body,
                        expr: ExprId(39),
                    }),
                }],
                result_obs: Observation::NeedValue,
            },
        ],
        runtime_nodes: vec![RuntimeNode::Lazy {
            id: RuntimeNodeId(30),
            source: SourceExpr {
                body: main_body,
                expr: ExprId(40),
            },
        }],
        closure_layouts: vec![ClosureEnvLayout {
            id: LayoutId(30),
            captures: vec![CaptureSlot {
                name: "n".to_owned(),
                source: SourceExpr {
                    body: main_body,
                    expr: ExprId(39),
                },
            }],
        }],
        frame_layouts: vec![],
    }
}

pub fn build_shaped_exec_fixture() -> AnalysisResult {
    let build_body = BodyId(5);
    let exec_call = ExprId(55);

    AnalysisResult {
        bodies: vec![AnalyzedBody {
            id: build_body,
            result_observation: Observation::NeedValue,
            use_observations: BTreeMap::from([
                (UseSiteId(50), Observation::NeedValue),
                (UseSiteId(51), Observation::NeedValue),
            ]),
            callee_targets: BTreeMap::new(),
            callee_local_origins: BTreeMap::new(),
            extracted_nodes: vec![ExtractedNode {
                id: RuntimeNodeId(50),
                source: SourceExpr {
                    body: build_body,
                    expr: exec_call,
                },
                reason: ExtractedNodeReason::RuntimeBoundary {
                    kind: RuntimeBoundaryKind::Exec,
                },
            }],
            kernel_entries: vec![KernelEntrypoint {
                body: build_body,
                source: SourceExpr {
                    body: build_body,
                    expr: ExprId(54),
                },
                kernel: KernelId(50),
                demanded_result: Observation::NeedValue,
            }],
        }],
        kernels: vec![KernelRegion {
            id: KernelId(50),
            body: build_body,
            exprs: BTreeSet::from([ExprId(52), ExprId(53), ExprId(54)]),
            params: vec![],
            result_obs: Observation::NeedValue,
        }],
        runtime_nodes: vec![RuntimeNode::Exec {
            id: RuntimeNodeId(50),
            source: SourceExpr {
                body: build_body,
                expr: exec_call,
            },
        }],
        closure_layouts: vec![],
        frame_layouts: vec![],
    }
}
