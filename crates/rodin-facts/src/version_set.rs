use std::cmp::Ordering;

use semver::{Comparator, Op, Prerelease, Version, VersionReq};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Interval {
    pub lower: Option<Version>,
    pub upper: Option<Version>,
}

impl Interval {
    #[must_use]
    pub fn new(lower: Option<Version>, upper: Option<Version>) -> Option<Self> {
        let interval = Self { lower, upper };
        (!interval.is_empty()).then_some(interval)
    }

    #[must_use]
    pub fn contains(&self, version: &Version) -> bool {
        self.lower.as_ref().is_none_or(|lower| version >= lower)
            && self.upper.as_ref().is_none_or(|upper| version < upper)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        match (&self.lower, &self.upper) {
            (Some(lower), Some(upper)) => lower >= upper,
            (None, _) | (_, None) => false,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VersionSet {
    intervals: Vec<Interval>,
}

impl VersionSet {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            intervals: Vec::new(),
        }
    }

    #[must_use]
    pub fn universe() -> Self {
        Self {
            intervals: vec![Interval {
                lower: None,
                upper: None,
            }],
        }
    }

    #[must_use]
    pub fn exact(version: Version) -> Self {
        Self::from_interval(Some(version.clone()), Some(next_patch(&version)))
    }

    #[must_use]
    pub fn from_interval(lower: Option<Version>, upper: Option<Version>) -> Self {
        Interval::new(lower, upper).map_or_else(Self::empty, |interval| Self {
            intervals: vec![interval],
        })
    }

    #[must_use]
    pub fn from_intervals(intervals: impl IntoIterator<Item = Interval>) -> Self {
        let mut intervals: Vec<_> = intervals
            .into_iter()
            .filter(|interval| !interval.is_empty())
            .collect();
        intervals.sort_by(compare_lower);

        let mut normalized: Vec<Interval> = Vec::new();
        for interval in intervals {
            if let Some(last) = normalized.last_mut()
                && intervals_touch_or_overlap(last, &interval)
            {
                last.upper = max_upper(last.upper.take(), interval.upper);
                continue;
            }
            normalized.push(interval);
        }
        Self {
            intervals: normalized,
        }
    }

    #[must_use]
    pub fn intervals(&self) -> &[Interval] {
        &self.intervals
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.intervals.is_empty()
    }

    #[must_use]
    pub fn contains(&self, version: &Version) -> bool {
        self.intervals
            .iter()
            .any(|interval| interval.contains(version))
    }

    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        Self::from_intervals(
            self.intervals
                .iter()
                .cloned()
                .chain(other.intervals.iter().cloned()),
        )
    }

    #[must_use]
    pub fn intersect(&self, other: &Self) -> Self {
        let mut out = Vec::new();
        for left in &self.intervals {
            for right in &other.intervals {
                let lower = max_lower(left.lower.clone(), right.lower.clone());
                let upper = min_upper(left.upper.clone(), right.upper.clone());
                if let Some(interval) = Interval::new(lower, upper) {
                    out.push(interval);
                }
            }
        }
        Self::from_intervals(out)
    }

    #[must_use]
    pub fn complement(&self, universe: &Self) -> Self {
        universe.intersect(&self.complement_unbounded())
    }

    #[must_use]
    pub fn complement_unbounded(&self) -> Self {
        if self.is_empty() {
            return Self::universe();
        }

        let mut out = Vec::new();
        let mut cursor: Option<Version> = None;
        for interval in &self.intervals {
            if cursor != interval.lower
                && let Some(gap) = Interval::new(cursor.clone(), interval.lower.clone())
            {
                out.push(gap);
            }
            cursor = interval.upper.clone();
            if cursor.is_none() {
                return Self::from_intervals(out);
            }
        }
        if let Some(gap) = Interval::new(cursor, None) {
            out.push(gap);
        }
        Self::from_intervals(out)
    }

    #[must_use]
    pub fn is_subset_of(&self, other: &Self) -> bool {
        self.intersect(other) == *self
    }

    /// Converts a Cargo semver requirement into interval algebra.
    ///
    /// The resulting set is exact for release-only package universes, which is
    /// what Rodin's package-version facts are expected to provide. Cargo's
    /// prerelease admission policy is not representable as a finite union of
    /// plain total-order half-open intervals because it creates infinitely many
    /// prerelease holes above otherwise admitted release ranges. `rodin.md`
    /// agenda B records prerelease admission as a v2 requirement, likely via a
    /// per-interval admission flag or an ordering trick; this function does not
    /// attempt that model.
    #[must_use]
    pub fn from_req(req: &VersionReq) -> Self {
        req.comparators.iter().fold(Self::universe(), |set, cmp| {
            set.intersect(&from_comparator(cmp))
        })
    }
}

fn from_comparator(cmp: &Comparator) -> VersionSet {
    match cmp.op {
        Op::Exact | Op::Wildcard => exact_or_wildcard(cmp),
        Op::Greater => VersionSet::from_interval(Some(greater_lower(cmp)), None),
        Op::GreaterEq => VersionSet::from_interval(Some(base_version(cmp)), None),
        Op::Less => VersionSet::from_interval(None, Some(base_version(cmp))),
        Op::LessEq => VersionSet::from_interval(None, Some(less_eq_upper(cmp))),
        Op::Tilde => VersionSet::from_interval(Some(base_version(cmp)), Some(tilde_upper(cmp))),
        Op::Caret => VersionSet::from_interval(Some(base_version(cmp)), Some(caret_upper(cmp))),
        _ => VersionSet::empty(),
    }
}

fn exact_or_wildcard(cmp: &Comparator) -> VersionSet {
    let lower = base_version(cmp);
    let upper = match (cmp.minor, cmp.patch) {
        (None, _) => Version::new(cmp.major + 1, 0, 0),
        (Some(minor), None) => Version::new(cmp.major, minor + 1, 0),
        (Some(_), Some(_)) => next_patch(&lower),
    };
    VersionSet::from_interval(Some(lower), Some(upper))
}

fn base_version(cmp: &Comparator) -> Version {
    Version {
        major: cmp.major,
        minor: cmp.minor.unwrap_or(0),
        patch: cmp.patch.unwrap_or(0),
        pre: if cmp.patch.is_some() {
            cmp.pre.clone()
        } else {
            Prerelease::EMPTY
        },
        build: semver::BuildMetadata::EMPTY,
    }
}

fn greater_lower(cmp: &Comparator) -> Version {
    match (cmp.minor, cmp.patch) {
        (None, _) => Version::new(cmp.major + 1, 0, 0),
        (Some(minor), None) => Version::new(cmp.major, minor + 1, 0),
        (Some(_), Some(_)) => next_patch(&base_version(cmp)),
    }
}

fn less_eq_upper(cmp: &Comparator) -> Version {
    match (cmp.minor, cmp.patch) {
        (None, _) => Version::new(cmp.major + 1, 0, 0),
        (Some(minor), None) => Version::new(cmp.major, minor + 1, 0),
        (Some(_), Some(_)) => next_patch(&base_version(cmp)),
    }
}

fn tilde_upper(cmp: &Comparator) -> Version {
    match cmp.minor {
        None => Version::new(cmp.major + 1, 0, 0),
        Some(minor) => Version::new(cmp.major, minor + 1, 0),
    }
}

fn caret_upper(cmp: &Comparator) -> Version {
    let minor = cmp.minor.unwrap_or(0);
    let patch = cmp.patch.unwrap_or(0);
    if cmp.major > 0 {
        Version::new(cmp.major + 1, 0, 0)
    } else if minor > 0 {
        Version::new(0, minor + 1, 0)
    } else if cmp.minor.is_some() && patch > 0 {
        Version::new(0, 0, patch + 1)
    } else if cmp.minor.is_some() {
        Version::new(0, 1, 0)
    } else {
        Version::new(1, 0, 0)
    }
}

fn next_patch(version: &Version) -> Version {
    Version::new(version.major, version.minor, version.patch + 1)
}

fn compare_lower(left: &Interval, right: &Interval) -> Ordering {
    match (&left.lower, &right.lower) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(left), Some(right)) => left.cmp(right),
    }
}

fn intervals_touch_or_overlap(left: &Interval, right: &Interval) -> bool {
    match (&left.upper, &right.lower) {
        (None, _) | (_, None) => true,
        (Some(upper), Some(lower)) => upper >= lower,
    }
}

fn max_lower(left: Option<Version>, right: Option<Version>) -> Option<Version> {
    match (left, right) {
        (None, other) | (other, None) => other,
        (Some(left), Some(right)) => Some(left.max(right)),
    }
}

fn min_upper(left: Option<Version>, right: Option<Version>) -> Option<Version> {
    match (left, right) {
        (None, other) | (other, None) => other,
        (Some(left), Some(right)) => Some(left.min(right)),
    }
}

fn max_upper(left: Option<Version>, right: Option<Version>) -> Option<Version> {
    match (left, right) {
        (None, _) | (_, None) => None,
        (Some(left), Some(right)) => Some(left.max(right)),
    }
}
