//! Workspace-scoped feature resolution.
//!
//! Cargo throws its feature resolution away every build and never records it in
//! `Cargo.lock`, so the classic phantom-feature-edge footgun stays invisible: a
//! member compiles in the workspace only because a *sibling* enabled some feature
//! of a shared dependency, yet the member's own manifest never declared it. Build
//! the member alone (or publish it) and it breaks.
//!
//! Here the two resolutions we need are just two root sets over one shared
//! [`CompiledIndex`]:
//!
//! * **unified** — every member is a root; feature unification across the whole
//!   workspace. This is what actually gets built (hakari-style default).
//! * **isolated** — one member is the sole root, with versions pinned to the
//!   unified lock via [`Decree`]s so we compare like for like (same dependency
//!   version, different feature set).
//!
//! The per-member delta `unified \ isolated` is the *candidate* phantom edge set:
//! a feature the workspace grants a dependency that the member alone does not,
//! for a dependency the member actually builds against. It is a cheap candidate
//! filter (pure resolution, no compilation) — the zero-false-positive version
//! reads the member's source to confirm the symbol is reachable only through the
//! phantom feature, and lands downstream of this machinery.

use std::collections::BTreeMap;

use crate::{CompiledIndex, Decree, Problem, RootReq, SolveError, SolveResult};

/// A dependency feature a workspace member builds against but does not justify on
/// its own — present only because a sibling member enabled it.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PhantomEdge {
    /// The leeching member (the root whose isolated resolution lacks the feature).
    pub member: String,
    /// The dependency key as it appears in [`SolveResult::enabled_features`].
    pub dep: String,
    /// The feature the workspace enabled on `dep` that `member` alone did not.
    pub feature: String,
}

/// The result of resolving a workspace both ways.
#[derive(Clone, Debug)]
pub struct WorkspaceResolution {
    /// Unified (workspace-scope) resolution — the one that drives the build.
    pub unified: SolveResult,
    /// Per-member isolated feature resolution over the unified lock.
    pub isolated: BTreeMap<String, SolveResult>,
    /// Candidate phantom feature edges, sorted.
    pub phantom_edges: Vec<PhantomEdge>,
}

/// Resolve `members` as a workspace: one unified solve, then per-member isolated
/// feature resolution over the unified lock, then the phantom-edge delta.
pub fn resolve_workspace(
    index: &CompiledIndex,
    members: &[RootReq],
) -> Result<WorkspaceResolution, Box<SolveError>> {
    // 1. Unified: every member is a root — feature unification across the workspace.
    let unified = solve_workspace(index, Problem::new(members.to_vec()))?;

    // 2. Pin every selected package to its unified-lock version so isolated feature
    //    resolution compares the *same* dependency versions, only different features.
    let decrees: Vec<Decree> = unified
        .selected
        .iter()
        .map(|(name, version)| Decree::new(name.clone(), format!("={version}")))
        .collect();

    // 3. Isolated: each member alone, over the locked versions.
    let mut isolated = BTreeMap::new();
    for member in members {
        let mut problem = Problem::new(vec![member.clone()]);
        problem.decrees = decrees.clone();
        isolated.insert(member.name.clone(), solve_workspace(index, problem)?);
    }

    // 4. Delta: a feature the workspace granted a dependency that the member alone
    //    did not, for a dependency the member actually builds against.
    let phantom_edges = phantom_edges(&unified, &isolated);

    Ok(WorkspaceResolution {
        unified,
        isolated,
        phantom_edges,
    })
}

fn solve_workspace(
    index: &CompiledIndex,
    problem: Problem,
) -> Result<SolveResult, Box<SolveError>> {
    crate::solve(index, problem)
}

fn phantom_edges(
    unified: &SolveResult,
    isolated: &BTreeMap<String, SolveResult>,
) -> Vec<PhantomEdge> {
    let mut out = Vec::new();
    for (member, member_result) in isolated {
        for (dep, unified_feats) in &unified.enabled_features {
            // The member only leeches on `dep` if it actually builds against it.
            let member_feats = match member_result.enabled_features.get(dep) {
                Some(feats) => feats,
                None => continue,
            };
            for feature in unified_feats.difference(member_feats) {
                out.push(PhantomEdge {
                    member: member.clone(),
                    dep: dep.clone(),
                    feature: feature.clone(),
                });
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use eyre::Result;

    use super::*;
    use crate::{CompiledIndex, RootReq, SyntheticDep, v};

    #[test]
    fn detects_phantom_feature_edge_leeched_from_sibling() -> Result<()> {
        // Shared dependency with an optional-ish feature `json`.
        let mut index = CompiledIndex::empty();
        index.add_package_version(
            "logger",
            v(1, 0, 0),
            Vec::new(),
            BTreeMap::from([("json".to_owned(), Vec::new())]),
        );
        // alpha legitimately enables logger/json via its dependency edge.
        index.add_package_version(
            "alpha",
            v(1, 0, 0),
            vec![SyntheticDep::normal("logger", "=1.0.0").feature("json")],
            BTreeMap::new(),
        );
        // beta depends on logger but does NOT enable json — in real life its source
        // would use a json-gated symbol; here it just fails to declare the feature.
        index.add_package_version(
            "beta",
            v(1, 0, 0),
            vec![SyntheticDep::normal("logger", "=1.0.0")],
            BTreeMap::new(),
        );
        index.finish_synthetic()?;

        let members = vec![RootReq::new("alpha", "*"), RootReq::new("beta", "*")];
        let resolution = resolve_workspace(&index, &members)
            .map_err(|err| eyre::eyre!("{err:?}"))?;

        // Under unification logger gets json (from alpha).
        assert!(
            resolution.unified.enabled_features["logger@1"].contains("json"),
            "unified resolution should enable logger/json",
        );
        // beta isolated does not enable json on its own.
        assert!(
            !resolution.isolated["beta"]
                .enabled_features
                .get("logger@1")
                .is_some_and(|feats| feats.contains("json")),
            "beta alone should not enable logger/json",
        );
        // The phantom edge is (beta, logger@1, json) — and nothing for alpha.
        assert_eq!(
            resolution.phantom_edges,
            vec![PhantomEdge {
                member: "beta".to_owned(),
                dep: "logger@1".to_owned(),
                feature: "json".to_owned(),
            }],
        );
        Ok(())
    }
}
