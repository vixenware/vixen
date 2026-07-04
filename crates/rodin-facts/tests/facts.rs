use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;
use rodin_facts::{
    Assignment, Atom, Clause, ClauseId, ClauseStore, Derivation, DerivationId, DerivationStore,
    Exclusion, ExclusionId, ExclusionStore, FeatIx, Interval, PackageInterner, PinRef, PkgId,
    PkgIx, Premise, Region, ReplayError, RuleKind, SourceInterner, SourceIx, VersionSet, WidenCert,
};
use semver::{Version, VersionReq};

fn v(major: u64, minor: u64, patch: u64) -> Version {
    Version::new(major, minor, patch)
}

fn range(lower: (u64, u64, u64), upper: (u64, u64, u64)) -> VersionSet {
    VersionSet::from_interval(
        Some(v(lower.0, lower.1, lower.2)),
        Some(v(upper.0, upper.1, upper.2)),
    )
}

fn finite_versions() -> Vec<Version> {
    (0..=5).map(|major| v(major, 0, 0)).collect()
}

fn set_strategy() -> impl Strategy<Value = VersionSet> {
    prop::collection::vec((0_u64..6, 0_u64..6), 0..8).prop_map(|pairs| {
        VersionSet::from_intervals(pairs.into_iter().filter_map(|(a, b)| {
            let lower = a.min(b);
            let upper = a.max(b);
            (lower != upper).then(|| Interval {
                lower: Some(v(lower, 0, 0)),
                upper: Some(v(upper, 0, 0)),
            })
        }))
    })
}

fn universe() -> VersionSet {
    range((0, 0, 0), (6, 0, 0))
}

proptest! {
    #[test]
    fn version_set_operations_match_pointwise_membership(a in set_strategy(), b in set_strategy()) {
        let union = a.union(&b);
        let intersection = a.intersect(&b);
        let a_complement = a.complement(&universe());

        for version in finite_versions() {
            prop_assert_eq!(union.contains(&version), a.contains(&version) || b.contains(&version));
            prop_assert_eq!(intersection.contains(&version), a.contains(&version) && b.contains(&version));
            prop_assert_eq!(a_complement.contains(&version), universe().contains(&version) && !a.contains(&version));
        }
    }

    #[test]
    fn version_set_de_morgan_laws_hold_in_universe(a in set_strategy(), b in set_strategy()) {
        let universe = universe();
        let left = a.union(&b).complement(&universe);
        let right = a.complement(&universe).intersect(&b.complement(&universe));
        prop_assert_eq!(left, right);

        let left = a.intersect(&b).complement(&universe);
        let right = a.complement(&universe).union(&b.complement(&universe)).intersect(&universe);
        prop_assert_eq!(left, right);
    }

    #[test]
    fn version_set_normalization_is_sorted_disjoint_and_non_touching(set in set_strategy()) {
        for pair in set.intervals().windows(2) {
            let left = &pair[0];
            let right = &pair[1];
            prop_assert!(left.upper <= right.lower);
            prop_assert_ne!(&left.upper, &right.lower);
        }
    }

    #[test]
    fn complement_round_trips_inside_universe(set in set_strategy()) {
        let universe = universe();
        let round_trip = set.complement(&universe).complement(&universe);
        prop_assert_eq!(set.intersect(&universe), round_trip);
    }
}

#[test]
fn version_req_conversion_matches_semver_for_release_versions() {
    let requirements = ["*", "^1.2.3", "~1.2", ">=1.0.0, <3.0.0", "=2.0.0", "<=1.2"];
    let versions = [
        v(0, 9, 0),
        v(1, 2, 2),
        v(1, 2, 3),
        v(1, 9, 0),
        v(2, 0, 0),
        v(3, 0, 0),
    ];

    for req in requirements {
        let req = req.parse::<VersionReq>().unwrap();
        let set = VersionSet::from_req(&req);
        for version in &versions {
            assert_eq!(
                set.contains(version),
                req.matches(version),
                "{req} disagreeing on {version}"
            );
        }
    }
}

#[test]
fn package_interner_distinguishes_same_name_across_sources() {
    let mut sources = SourceInterner::new();
    let crates_io = sources.intern("crates.io");
    let private = sources.intern("private");
    assert_ne!(crates_io, private);
    assert_eq!(sources.resolve_str(crates_io), Some("crates.io"));

    let mut packages = PackageInterner::new();
    let left = packages.intern(PkgId::new(crates_io, "derive"));
    let right = packages.intern(PkgId::new(private, "derive"));
    let default = packages.intern_default("derive");

    assert_ne!(left, right);
    assert_ne!(right, default);
    assert_eq!(
        packages.resolve(left),
        Some(&PkgId::new(crates_io, "derive"))
    );
    assert_eq!(
        packages.resolve(default),
        Some(&PkgId::new(SourceIx::DEFAULT, "derive"))
    );
}

#[test]
fn clause_store_indexes_by_mentioned_package_and_feature() {
    let pkg = PkgIx(1);
    let dep = PkgIx(2);
    let feat = FeatIx(7);
    let dep_feat = FeatIx(9);
    let mut store = ClauseStore::new();

    let weak = store.append(Clause {
        antecedents: vec![Atom::InGraph(dep), Atom::Feat(pkg, feat)],
        consequent: Some(Atom::Feat(dep, dep_feat)),
    });
    let incompat = store.append(Clause {
        antecedents: vec![Atom::Feat(pkg, feat), Atom::Ver(pkg, VersionSet::empty())],
        consequent: None,
    });

    assert_eq!(
        store.mentioned_package(pkg).collect::<Vec<_>>(),
        vec![weak, incompat]
    );
    assert_eq!(
        store.mentioned_feature(pkg, feat).collect::<Vec<_>>(),
        vec![weak, incompat]
    );
    assert_eq!(
        store.mentioned_feature(dep, dep_feat).collect::<Vec<_>>(),
        vec![weak]
    );
}

#[test]
fn region_subtract_can_make_version_holes() {
    let pkg = PkgIx(1);
    let source = Region::new().with_versions(pkg, range((1, 0, 0), (5, 0, 0)));
    let hole = Region::new().with_versions(pkg, range((2, 0, 0), (3, 0, 0)));

    let mut fragments = source.subtract(&hole);
    fragments.sort_by_key(|region| region.versions[&pkg].intervals()[0].lower.clone().unwrap());

    assert_eq!(fragments.len(), 2);
    assert!(fragments[0].versions[&pkg].contains(&v(1, 5, 0)));
    assert!(!fragments[0].versions[&pkg].contains(&v(2, 0, 0)));
    assert!(fragments[1].versions[&pkg].contains(&v(3, 0, 0)));
    assert!(!fragments[1].versions[&pkg].contains(&v(5, 0, 0)));
}

#[test]
fn region_subtract_partitions_feature_constraints() {
    let pkg = PkgIx(1);
    let feat = FeatIx(1);
    let source = Region::new().with_versions(pkg, range((1, 0, 0), (2, 0, 0)));
    let enabled = Region::new().with_feature(pkg, feat, true);

    let fragments = source.subtract(&enabled);
    assert_eq!(
        fragments,
        vec![
            Region::new()
                .with_versions(pkg, range((1, 0, 0), (2, 0, 0)))
                .with_feature(pkg, feat, false)
        ]
    );
}

#[test]
fn region_contains_assignment_respects_versions_and_feature_polarity() {
    let pkg = PkgIx(1);
    let feat = FeatIx(1);
    let region = Region::new()
        .with_versions(pkg, range((1, 0, 0), (2, 0, 0)))
        .with_feature(pkg, feat, true);
    let assignment = Assignment {
        versions: BTreeMap::from([(pkg, v(1, 4, 0))]),
        features: BTreeSet::from([(pkg, feat)]),
    };
    assert!(region.contains_assignment(&assignment));

    let assignment = Assignment {
        features: BTreeSet::new(),
        ..assignment
    };
    assert!(!region.contains_assignment(&assignment));
}

#[test]
fn exclusion_store_finds_superset_and_overlap() {
    let pkg = PkgIx(1);
    let mut store = ExclusionStore::new();
    let broad = store.append(Exclusion {
        region: Region::new().with_versions(pkg, range((1, 0, 0), (5, 0, 0))),
        derivation: DerivationId(0),
    });
    let disjoint = store.append(Exclusion {
        region: Region::new().with_versions(pkg, range((7, 0, 0), (8, 0, 0))),
        derivation: DerivationId(1),
    });

    let query = Region::new().with_versions(pkg, range((2, 0, 0), (3, 0, 0)));
    assert_eq!(store.excluded_superset_of(&query), Some(broad));
    assert_eq!(store.overlapping(&query).collect::<Vec<_>>(), vec![broad]);

    let query = Region::new().with_versions(pkg, range((6, 0, 0), (9, 0, 0)));
    assert_eq!(store.excluded_superset_of(&query), None);
    assert_eq!(
        store.overlapping(&query).collect::<Vec<_>>(),
        vec![disjoint]
    );
}

#[test]
fn derivation_replay_accepts_well_formed_dag() {
    let mut store = DerivationStore::new();
    let clause = ClauseId(0);
    let pin = PinRef("index-a".to_owned());
    store.note_clause(clause);
    store.note_pin(pin.clone());

    let conflict = store.append(Derivation {
        premises: vec![
            Premise::Clause(clause),
            Premise::Pin(pin),
            Premise::Hypothesis(42),
        ],
        rule: RuleKind::Conflict,
    });
    let exclusion = ExclusionId(0);
    store.note_exclusion(exclusion, conflict);
    let widen = store.append(Derivation {
        premises: vec![Premise::Exclusion(exclusion)],
        rule: RuleKind::Widen {
            certificate: WidenCert {
                package: PkgIx(0),
                checked: range((1, 0, 0), (2, 0, 0)),
                manifest_hash: "abc".to_owned(),
            },
        },
    });

    let report = store.replay(widen).unwrap();
    assert_eq!(report.visited, BTreeSet::from([conflict, widen]));
    assert_eq!(report.hypotheses, BTreeSet::from([42]));
}

#[test]
fn derivation_replay_rejects_dangling_premises() {
    let mut store = DerivationStore::new();
    let id = store.append(Derivation {
        premises: vec![Premise::Clause(ClauseId(99))],
        rule: RuleKind::Resolve,
    });

    assert_eq!(
        store.replay(id),
        Err(ReplayError::MissingClause(ClauseId(99)))
    );
}

#[test]
fn derivation_replay_rejects_cycles_through_exclusions() {
    let mut store = DerivationStore::new();
    let exclusion = ExclusionId(0);
    let id = store.append(Derivation {
        premises: vec![Premise::Exclusion(exclusion)],
        rule: RuleKind::Resolve,
    });
    store.note_exclusion(exclusion, id);

    assert_eq!(store.replay(id), Err(ReplayError::Cycle(vec![id, id])));
}

#[test]
fn derivation_replay_rejects_hypothesis_outside_conflict_path() {
    let mut store = DerivationStore::new();
    let id = store.append(Derivation {
        premises: vec![Premise::Hypothesis(1)],
        rule: RuleKind::Resolve,
    });

    assert_eq!(
        store.replay(id),
        Err(ReplayError::HypothesisOutsideConflict(1))
    );
}
