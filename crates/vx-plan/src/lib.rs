use camino::Utf8PathBuf;
use facet::Facet;
use std::collections::{BTreeMap, HashMap, HashSet};
use strid::braid;

pub mod runtime;
pub mod vx_lang;
pub use runtime::*;

#[braid]
pub struct RulePlanNodeId {
    value: String,
}

#[braid]
pub struct RulePlanFactId {
    value: String,
}

#[braid]
pub struct RulePlanSourceId {
    value: String,
}

#[braid]
pub struct RuleId {
    value: String,
}

#[braid]
pub struct RulePlanValue {
    value: String,
}

/// Canonical typed intermediate representation between rule evaluation and execution.
///
/// This schema is designed for deterministic explain/diff/caching workflows.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct RulePlan {
    /// Interned source table shared by all provenance references.
    #[facet(default)]
    pub sources: Vec<RulePlanSource>,
    /// Typed graph nodes.
    pub nodes: Vec<RulePlanNode>,
    /// Typed graph edges.
    pub edges: Vec<RulePlanEdge>,
    /// Emitted facts with values.
    pub facts: Vec<RulePlanFact>,
}

impl RulePlan {
    /// Serialize this plan to canonical deterministic JSON.
    pub fn to_canonical_json(&self) -> Result<String, String> {
        let canonical = self.canonicalized();
        facet_json::to_string(&canonical).map_err(|e| e.to_string())
    }

    /// Deserialize a plan from JSON.
    pub fn from_json(json: &str) -> Result<Self, String> {
        facet_json::from_str(json).map_err(|e| e.to_string())
    }

    /// Validate structural invariants and type constraints.
    pub fn validate(&self) -> Result<(), RulePlanValidationError> {
        let mut diagnostics = Vec::new();
        let mut node_ids = HashSet::new();
        let mut node_kind_by_id = HashMap::new();
        let mut source_ids = HashSet::new();

        for source in &self.sources {
            if source.id.as_str().trim().is_empty() {
                diagnostics.push("source has empty id".to_string());
            }
            if !source_ids.insert(source.id.clone()) {
                diagnostics.push(format!("duplicate source id: {}", source.id));
            }
            if source.path.as_str().trim().is_empty() {
                diagnostics.push(format!("source {} has empty path", source.id));
            }
        }

        for node in &self.nodes {
            if node.id.as_str().trim().is_empty() {
                diagnostics.push("node has empty id".to_string());
            }
            if !node_ids.insert(node.id.clone()) {
                diagnostics.push(format!("duplicate node id: {}", node.id));
            }
            if let Some(issue) =
                validate_provenance(&node.provenance, "node", node.id.as_str(), &source_ids)
            {
                diagnostics.push(issue);
            }
            node_kind_by_id.insert(node.id.clone(), node.kind);
        }

        let mut fact_ids = HashSet::new();
        for fact in &self.facts {
            if fact.id.as_str().trim().is_empty() {
                diagnostics.push("fact has empty id".to_string());
            }
            if !fact_ids.insert(fact.id.clone()) {
                diagnostics.push(format!("duplicate fact id: {}", fact.id));
            }
            if fact.node_id.as_str().trim().is_empty() {
                diagnostics.push(format!("fact {} has empty node_id", fact.id));
            }
            match node_kind_by_id.get(&fact.node_id) {
                Some(RulePlanNodeKind::Fact) => {}
                Some(kind) => diagnostics.push(format!(
                    "fact {} references node {} with incompatible kind {:?}",
                    fact.id, fact.node_id, kind
                )),
                None => diagnostics.push(format!(
                    "fact {} references unknown node_id {}",
                    fact.id, fact.node_id
                )),
            }
            if let Some(issue) =
                validate_provenance(&fact.provenance, "fact", fact.id.as_str(), &source_ids)
            {
                diagnostics.push(issue);
            }
        }

        for edge in &self.edges {
            if edge.from.as_str().trim().is_empty() || edge.to.as_str().trim().is_empty() {
                diagnostics.push(format!(
                    "edge {} -> {} has empty endpoint id",
                    edge.from, edge.to
                ));
                continue;
            }

            let from_kind = node_kind_by_id.get(&edge.from);
            let to_kind = node_kind_by_id.get(&edge.to);

            if from_kind.is_none() {
                diagnostics.push(format!(
                    "edge {} -> {} references unknown from node {}",
                    edge.from, edge.to, edge.from
                ));
            }
            if to_kind.is_none() {
                diagnostics.push(format!(
                    "edge {} -> {} references unknown to node {}",
                    edge.from, edge.to, edge.to
                ));
            }

            if let (Some(from_kind), Some(to_kind)) = (from_kind, to_kind)
                && !edge_kind_allowed(edge.kind, *from_kind, *to_kind)
            {
                diagnostics.push(format!(
                    "edge {} -> {} with kind {:?} is invalid between {:?} and {:?}",
                    edge.from, edge.to, edge.kind, from_kind, to_kind
                ));
            }

            if let Some(issue) =
                validate_provenance(&edge.provenance, "edge", edge.from.as_str(), &source_ids)
            {
                diagnostics.push(issue);
            }
        }

        if diagnostics.is_empty() {
            Ok(())
        } else {
            Err(RulePlanValidationError { diagnostics })
        }
    }

    fn canonicalized(&self) -> Self {
        let mut canonical = self.clone();
        canonical.sources.sort_by(|a, b| {
            a.id.as_str()
                .cmp(b.id.as_str())
                .then_with(|| a.path.as_str().cmp(b.path.as_str()))
        });
        canonical.nodes.sort_by(|a, b| {
            a.id.as_str()
                .cmp(b.id.as_str())
                .then_with(|| a.kind.canonical_name().cmp(b.kind.canonical_name()))
                .then_with(|| a.label.cmp(&b.label))
        });
        canonical.edges.sort_by(|a, b| {
            a.from
                .as_str()
                .cmp(b.from.as_str())
                .then_with(|| a.to.as_str().cmp(b.to.as_str()))
                .then_with(|| a.kind.canonical_name().cmp(b.kind.canonical_name()))
        });
        canonical.facts.sort_by(|a, b| {
            a.id.as_str()
                .cmp(b.id.as_str())
                .then_with(|| a.node_id.as_str().cmp(b.node_id.as_str()))
        });
        canonical
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulePlanValidationError {
    pub diagnostics: Vec<String>,
}

impl std::fmt::Display for RulePlanValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "RulePlan validation failed: {}",
            self.diagnostics.join("; ")
        )
    }
}

impl std::error::Error for RulePlanValidationError {}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct RulePlanSource {
    pub id: RulePlanSourceId,
    pub path: Utf8PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct RulePlanNode {
    pub id: RulePlanNodeId,
    pub kind: RulePlanNodeKind,
    pub label: String,
    pub provenance: RulePlanProvenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(u8)]
#[facet(rename_all = "lowercase")]
pub enum RulePlanNodeKind {
    Action,
    Artifact,
    Fact,
    Group,
}

impl RulePlanNodeKind {
    fn canonical_name(self) -> &'static str {
        match self {
            Self::Action => "action",
            Self::Artifact => "artifact",
            Self::Fact => "fact",
            Self::Group => "group",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct RulePlanEdge {
    pub from: RulePlanNodeId,
    pub to: RulePlanNodeId,
    pub kind: RulePlanEdgeKind,
    pub provenance: RulePlanProvenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(u8)]
#[facet(rename_all = "lowercase")]
pub enum RulePlanEdgeKind {
    IfChanged,
    IfWritten,
    IfCreate,
    IfDeleted,
    Always,
}

impl RulePlanEdgeKind {
    fn canonical_name(self) -> &'static str {
        match self {
            Self::IfChanged => "ifchanged",
            Self::IfWritten => "ifwritten",
            Self::IfCreate => "ifcreate",
            Self::IfDeleted => "ifdeleted",
            Self::Always => "always",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct RulePlanFact {
    pub id: RulePlanFactId,
    pub node_id: RulePlanNodeId,
    pub value: RulePlanValue,
    pub provenance: RulePlanProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct RulePlanProvenance {
    pub rule_id: RuleId,
    pub source_id: RulePlanSourceId,
    pub span: RulePlanSpan,
    #[facet(default)]
    pub comment: Option<String>,
    #[facet(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct RulePlanSpan {
    pub line: u32,
    pub column: u32,
    #[facet(default)]
    pub end_line: u32,
    #[facet(default)]
    pub end_column: u32,
}

fn validate_provenance(
    provenance: &RulePlanProvenance,
    owner_kind: &str,
    owner_id: &str,
    source_ids: &HashSet<RulePlanSourceId>,
) -> Option<String> {
    if provenance.rule_id.as_str().trim().is_empty() {
        return Some(format!(
            "{} {} has empty provenance.rule_id",
            owner_kind, owner_id
        ));
    }
    if provenance.source_id.as_str().trim().is_empty() {
        return Some(format!(
            "{} {} has empty provenance.source_id",
            owner_kind, owner_id
        ));
    }
    if !source_ids.contains(&provenance.source_id) {
        return Some(format!(
            "{} {} references unknown provenance.source_id {}",
            owner_kind, owner_id, provenance.source_id
        ));
    }
    if provenance.span.line == 0 || provenance.span.column == 0 {
        return Some(format!(
            "{} {} has invalid provenance source position {}:{}",
            owner_kind, owner_id, provenance.span.line, provenance.span.column
        ));
    }
    None
}

fn edge_kind_allowed(
    edge_kind: RulePlanEdgeKind,
    from_kind: RulePlanNodeKind,
    to_kind: RulePlanNodeKind,
) -> bool {
    match edge_kind {
        RulePlanEdgeKind::IfWritten | RulePlanEdgeKind::IfCreate | RulePlanEdgeKind::IfDeleted => {
            from_kind == RulePlanNodeKind::Action && to_kind == RulePlanNodeKind::Artifact
        }
        RulePlanEdgeKind::IfChanged => {
            matches!(
                from_kind,
                RulePlanNodeKind::Artifact | RulePlanNodeKind::Fact
            ) && matches!(to_kind, RulePlanNodeKind::Action | RulePlanNodeKind::Group)
        }
        RulePlanEdgeKind::Always => {
            matches!(
                from_kind,
                RulePlanNodeKind::Action | RulePlanNodeKind::Group
            ) && matches!(to_kind, RulePlanNodeKind::Action | RulePlanNodeKind::Group)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_sources() -> Vec<RulePlanSource> {
        vec![RulePlanSource {
            id: RulePlanSourceId::from_static("src_0").to_owned(),
            path: Utf8PathBuf::from("fixtures/rules/sample.edn"),
        }]
    }

    fn sample_provenance(rule_id: &'static str, line: u32) -> RulePlanProvenance {
        RulePlanProvenance {
            rule_id: RuleId::from_static(rule_id).to_owned(),
            source_id: RulePlanSourceId::from_static("src_0").to_owned(),
            span: RulePlanSpan {
                line,
                column: 1,
                end_line: line,
                end_column: 10,
            },
            comment: Some("fixture".to_string()),
            metadata: BTreeMap::from([
                ("phase".to_string(), "planner".to_string()),
                ("fixture".to_string(), "true".to_string()),
            ]),
        }
    }

    fn sample_rule_plan() -> RulePlan {
        RulePlan {
            sources: sample_sources(),
            nodes: vec![
                RulePlanNode {
                    id: RulePlanNodeId::from_static("n_group_build").to_owned(),
                    kind: RulePlanNodeKind::Group,
                    label: "group build".to_string(),
                    provenance: sample_provenance("rule.group.build", 1),
                },
                RulePlanNode {
                    id: RulePlanNodeId::from_static("n_action_compile").to_owned(),
                    kind: RulePlanNodeKind::Action,
                    label: "compile app".to_string(),
                    provenance: sample_provenance("rule.compile.app", 5),
                },
                RulePlanNode {
                    id: RulePlanNodeId::from_static("n_artifact_bin").to_owned(),
                    kind: RulePlanNodeKind::Artifact,
                    label: "target/app".to_string(),
                    provenance: sample_provenance("rule.compile.app", 8),
                },
                RulePlanNode {
                    id: RulePlanNodeId::from_static("n_fact_src_changed").to_owned(),
                    kind: RulePlanNodeKind::Fact,
                    label: "src_changed".to_string(),
                    provenance: sample_provenance("rule.detect.src", 12),
                },
            ],
            edges: vec![
                RulePlanEdge {
                    from: RulePlanNodeId::from_static("n_group_build").to_owned(),
                    to: RulePlanNodeId::from_static("n_action_compile").to_owned(),
                    kind: RulePlanEdgeKind::Always,
                    provenance: sample_provenance("rule.group.build", 2),
                },
                RulePlanEdge {
                    from: RulePlanNodeId::from_static("n_action_compile").to_owned(),
                    to: RulePlanNodeId::from_static("n_artifact_bin").to_owned(),
                    kind: RulePlanEdgeKind::IfWritten,
                    provenance: sample_provenance("rule.compile.app", 9),
                },
                RulePlanEdge {
                    from: RulePlanNodeId::from_static("n_fact_src_changed").to_owned(),
                    to: RulePlanNodeId::from_static("n_action_compile").to_owned(),
                    kind: RulePlanEdgeKind::IfChanged,
                    provenance: sample_provenance("rule.detect.src", 13),
                },
            ],
            facts: vec![RulePlanFact {
                id: RulePlanFactId::from_static("fact.src_changed").to_owned(),
                node_id: RulePlanNodeId::from_static("n_fact_src_changed").to_owned(),
                value: RulePlanValue::from_static("true").to_owned(),
                provenance: sample_provenance("rule.detect.src", 14),
            }],
        }
    }

    #[test]
    fn test_rule_plan_roundtrip_canonical_json_stable() {
        let plan = sample_rule_plan();
        plan.validate().expect("sample plan should validate");

        let canonical_1 = plan.to_canonical_json().expect("canonical json");
        let parsed = RulePlan::from_json(&canonical_1).expect("parse canonical json");
        let canonical_2 = parsed.to_canonical_json().expect("canonical json");

        assert_eq!(canonical_1, canonical_2);
    }

    #[test]
    fn test_rule_plan_validation_reports_actionable_diagnostics() {
        let mut invalid = sample_rule_plan();
        invalid.nodes.push(RulePlanNode {
            id: RulePlanNodeId::from_static("n_action_compile").to_owned(),
            kind: RulePlanNodeKind::Action,
            label: "duplicate".to_string(),
            provenance: sample_provenance("rule.duplicate", 20),
        });
        invalid.edges.push(RulePlanEdge {
            from: RulePlanNodeId::from_static("n_artifact_bin").to_owned(),
            to: RulePlanNodeId::from_static("n_action_compile").to_owned(),
            kind: RulePlanEdgeKind::IfWritten,
            provenance: sample_provenance("rule.invalid.edge", 21),
        });
        invalid.facts.push(RulePlanFact {
            id: RulePlanFactId::from_static("fact.bad_ref").to_owned(),
            node_id: RulePlanNodeId::from_static("missing_node").to_owned(),
            value: RulePlanValue::from_static("x").to_owned(),
            provenance: sample_provenance("rule.bad.fact", 22),
        });

        let err = invalid.validate().expect_err("invalid plan should fail");

        assert!(
            err.diagnostics
                .iter()
                .any(|d| d.contains("duplicate node id"))
        );
        assert!(err.diagnostics.iter().any(|d| {
            d.contains("IfWritten") && d.contains("invalid between Artifact and Action")
        }));
        assert!(
            err.diagnostics
                .iter()
                .any(|d| d.contains("references unknown node_id"))
        );
    }

    #[test]
    fn test_rule_plan_fixture_provenance_is_populated() {
        let plan = sample_rule_plan();

        assert_eq!(plan.sources.len(), 1);
        assert!(!plan.sources[0].id.as_str().is_empty());
        assert!(!plan.sources[0].path.as_str().is_empty());

        for node in &plan.nodes {
            assert!(!node.provenance.rule_id.as_str().is_empty());
            assert!(!node.provenance.source_id.as_str().is_empty());
            assert!(node.provenance.span.line > 0);
            assert!(node.provenance.span.column > 0);
        }
        for edge in &plan.edges {
            assert!(!edge.provenance.rule_id.as_str().is_empty());
            assert!(!edge.provenance.source_id.as_str().is_empty());
            assert!(edge.provenance.span.line > 0);
            assert!(edge.provenance.span.column > 0);
        }
        for fact in &plan.facts {
            assert!(!fact.provenance.rule_id.as_str().is_empty());
            assert!(!fact.provenance.source_id.as_str().is_empty());
            assert!(fact.provenance.span.line > 0);
            assert!(fact.provenance.span.column > 0);
        }
    }
}
