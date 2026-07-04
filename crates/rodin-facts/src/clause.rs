use std::collections::{BTreeMap, BTreeSet};

use crate::{FeatIx, PackageFeature, PkgIx, VersionSet, features_in_atoms, packages_in_atoms};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Atom {
    InGraph(PkgIx),
    Ver(PkgIx, VersionSet),
    Feat(PkgIx, FeatIx),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Clause {
    pub antecedents: Vec<Atom>,
    pub consequent: Option<Atom>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ClauseId(pub u32);

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ClauseStore {
    clauses: Vec<Clause>,
    by_package: BTreeMap<PkgIx, BTreeSet<ClauseId>>,
    by_feature: BTreeMap<PackageFeature, BTreeSet<ClauseId>>,
}

impl ClauseStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn append(&mut self, clause: Clause) -> ClauseId {
        let id = ClauseId(u32::try_from(self.clauses.len()).expect("too many clauses"));
        let atoms = clause.antecedents.iter().chain(clause.consequent.iter());
        for pkg in packages_in_atoms(atoms.clone()) {
            self.by_package.entry(pkg).or_default().insert(id);
        }
        for feature in features_in_atoms(atoms) {
            self.by_feature.entry(feature).or_default().insert(id);
        }
        self.clauses.push(clause);
        id
    }

    #[must_use]
    pub fn get(&self, id: ClauseId) -> Option<&Clause> {
        self.clauses.get(id.0 as usize)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.clauses.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clauses.is_empty()
    }

    pub fn mentioned_package(&self, pkg: PkgIx) -> impl Iterator<Item = ClauseId> + '_ {
        self.by_package
            .get(&pkg)
            .into_iter()
            .flat_map(|ids| ids.iter().copied())
    }

    pub fn mentioned_feature(
        &self,
        pkg: PkgIx,
        feat: FeatIx,
    ) -> impl Iterator<Item = ClauseId> + '_ {
        self.by_feature
            .get(&(pkg, feat))
            .into_iter()
            .flat_map(|ids| ids.iter().copied())
    }
}
