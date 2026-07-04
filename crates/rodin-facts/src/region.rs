use std::collections::{BTreeMap, BTreeSet};

use semver::Version;

use crate::{FeatIx, FeaturePolarity, PkgIx, VersionSet};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Assignment {
    pub versions: BTreeMap<PkgIx, Version>,
    pub features: BTreeSet<(PkgIx, FeatIx)>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Region {
    pub versions: BTreeMap<PkgIx, VersionSet>,
    pub features: BTreeSet<FeaturePolarity>,
}

impl Region {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_versions(mut self, pkg: PkgIx, versions: VersionSet) -> Self {
        self.versions.insert(pkg, versions);
        self
    }

    #[must_use]
    pub fn with_feature(mut self, pkg: PkgIx, feat: FeatIx, enabled: bool) -> Self {
        self.features.insert((pkg, feat, enabled));
        self
    }

    #[must_use]
    pub fn intersect(&self, other: &Self) -> Option<Self> {
        let mut out = Self {
            versions: self.versions.clone(),
            features: self.features.clone(),
        };

        for (pkg, versions) in &other.versions {
            out.versions
                .entry(*pkg)
                .and_modify(|left| *left = left.intersect(versions))
                .or_insert_with(|| versions.clone());
            if out.versions.get(pkg).is_some_and(VersionSet::is_empty) {
                return None;
            }
        }

        for &(pkg, feat, enabled) in &other.features {
            if out.features.contains(&(pkg, feat, !enabled)) {
                return None;
            }
            out.features.insert((pkg, feat, enabled));
        }

        Some(out)
    }

    #[must_use]
    pub fn contains_assignment(&self, assignment: &Assignment) -> bool {
        self.versions.iter().all(|(pkg, versions)| {
            assignment
                .versions
                .get(pkg)
                .is_some_and(|version| versions.contains(version))
        }) && self
            .features
            .iter()
            .all(|(pkg, feat, enabled)| assignment.features.contains(&(*pkg, *feat)) == *enabled)
    }

    #[must_use]
    pub fn contains_region(&self, other: &Self) -> bool {
        let versions_contained = self.versions.iter().all(|(pkg, versions)| {
            other
                .versions
                .get(pkg)
                .is_some_and(|other_versions| other_versions.is_subset_of(versions))
        });
        versions_contained
            && self
                .features
                .iter()
                .all(|feature| other.features.contains(feature))
    }

    /// Returns a finite disjoint cover for `self - other`.
    ///
    /// The number of fragments can grow with every constrained dimension in
    /// `other`. This is intentionally correct algebra rather than a compact
    /// frontier representation.
    #[must_use]
    pub fn subtract(&self, other: &Self) -> Vec<Self> {
        if self.intersect(other).is_none() {
            return vec![self.clone()];
        }

        let mut out = Vec::new();
        let mut matched_prefix = self.clone();

        for (pkg, other_versions) in &other.versions {
            let current = matched_prefix
                .versions
                .get(pkg)
                .cloned()
                .unwrap_or_else(VersionSet::universe);
            let inside = current.intersect(other_versions);
            let outside = current.intersect(&other_versions.complement(&current));

            for interval in outside.intervals() {
                let mut fragment = matched_prefix.clone();
                fragment
                    .versions
                    .insert(*pkg, VersionSet::from_intervals([interval.clone()]));
                out.push(fragment);
            }

            if inside.is_empty() {
                return out;
            }
            matched_prefix.versions.insert(*pkg, inside);
        }

        for &(pkg, feat, enabled) in &other.features {
            if matched_prefix.features.contains(&(pkg, feat, !enabled)) {
                return out;
            }
            if !matched_prefix.features.contains(&(pkg, feat, enabled)) {
                let mut fragment = matched_prefix.clone();
                fragment.features.insert((pkg, feat, !enabled));
                out.push(fragment);
                matched_prefix.features.insert((pkg, feat, enabled));
            }
        }

        out
    }

    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        self.intersect(other).is_some()
    }
}
