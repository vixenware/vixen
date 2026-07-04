//! Standalone fact layer for Rodin, the carving resolver.
//!
//! This crate deliberately stops at plain data, append-only stores, interval
//! algebra, and replayable derivation structure. Search, decision heuristics,
//! and propagation execution belong in Rodin proper.

use std::collections::BTreeSet;

pub use clause::{Atom, Clause, ClauseId, ClauseStore};
pub use derivation::{
    Derivation, DerivationId, DerivationStore, PinRef, Premise, ReplayError, ReplayReport,
    RuleKind, WidenCert,
};
pub use exclusion::{Exclusion, ExclusionId, ExclusionStore};
pub use interner::{FeatIx, Interner, PkgIx};
pub use region::{Assignment, Region};
pub use version_set::{Interval, VersionSet};

mod clause;
mod derivation;
mod exclusion;
mod interner;
mod region;
mod version_set;

pub(crate) type PackageFeature = (PkgIx, FeatIx);
pub(crate) type FeaturePolarity = (PkgIx, FeatIx, bool);

fn packages_in_atoms<'a>(atoms: impl IntoIterator<Item = &'a Atom>) -> BTreeSet<PkgIx> {
    atoms
        .into_iter()
        .map(|atom| match atom {
            Atom::InGraph(pkg) | Atom::Ver(pkg, _) | Atom::Feat(pkg, _) => *pkg,
        })
        .collect()
}

fn features_in_atoms<'a>(atoms: impl IntoIterator<Item = &'a Atom>) -> BTreeSet<PackageFeature> {
    atoms
        .into_iter()
        .filter_map(|atom| match atom {
            Atom::Feat(pkg, feat) => Some((*pkg, *feat)),
            Atom::InGraph(_) | Atom::Ver(_, _) => None,
        })
        .collect()
}
