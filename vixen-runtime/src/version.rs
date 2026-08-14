//! Toolchain versions, which are **not** package versions.
//!
//! `std/version.vix` is strict semver and stays that way: it serves the
//! dependency solver, and a crates.io version genuinely is semver. A toolchain
//! is not. `xcodebuild -version` says `15.2`, MSVC says `19.38.33130.0`, and
//! Quartus says `22.1std` — two components, four components, and a trailing
//! edition tag that is not a number at all. Holding those to semver rejects
//! every tool this vocabulary exists for while passing the one tool (rustc)
//! that needed it least.
//!
//! So there are two kinds of version here, and a pin says which kind it is
//! rather than guessing:
//!
//! - **Ordered** — a dot-separated run of numbers with an optional prerelease
//!   tag. Comparable, so a range means something.
//! - **Opaque** — anything else. `22.1std` is a real version of a real tool and
//!   the machine may state it, but nothing can put it on a number line, so the
//!   only honest question about it is "is it this exact one".
//!
//! The pin picks the question. `toolchain: "22.1std"` asks for that string and
//! parses neither side; `toolchain_range: ">=1.89, <1.90"` asks an ordering
//! question and requires both sides to answer it. A range held against an
//! opaque version is a refusal, not a false — nobody can order `22.1std`, and
//! saying "no" would imply somebody had.

use core::cmp::Ordering;

/// A toolchain version that can be placed on a number line: a dot-separated
/// run of numeric components, plus an optional prerelease tag.
///
/// Arity is free. `15.2` and `15.2.0` are the SAME version — a missing
/// component reads as zero — because Xcode saying `15.2` and a pin saying
/// `>=15.2.0` are talking about one thing, and a version type that disagreed
/// would be inventing a distinction the tool does not have.
/// Equality is defined by [`Ord`], not derived. `15.2` and `15.2.0` hold
/// different component vectors and are the same version, so a derived
/// `PartialEq` would disagree with `cmp` — an inconsistency that reads fine in
/// a comparison and corrupts a `BTreeMap`.
#[derive(Clone, Debug)]
pub struct OrderedVersion {
    components: Vec<u64>,
    /// The `-nightly` of `1.99.0-nightly`. Ordered BELOW the same release, per
    /// semver, because a prerelease precedes the thing it is a prerelease of.
    ///
    /// This reads any `-suffix` as a prerelease, which is the semver rule and
    /// what rustc needs. A tool that spells an INCREMENT that way — a service
    /// pack, `2023.09-SP1` — would sort below its own base version, which is
    /// backwards. Pin such a tool exactly rather than by range; that is what
    /// the exact pin is for.
    prerelease: Option<String>,
}

impl PartialEq for OrderedVersion {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for OrderedVersion {}

impl OrderedVersion {
    /// Read a stated version, or `None` when it is not orderable.
    ///
    /// Build metadata (`+abc`) is discarded rather than refused: semver says it
    /// does not participate in precedence, and a toolchain that appends a build
    /// hash is still the same version of the same tool.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.split_once('+').map_or(text, |(before, _)| before);
        let (core, prerelease) = match text.split_once('-') {
            Some((core, tag)) if !tag.is_empty() => (core, Some(tag.to_owned())),
            Some(_) => return None,
            None => (text, None),
        };
        if core.is_empty() {
            return None;
        }
        let components = core
            .split('.')
            .map(|part| part.parse::<u64>().ok())
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            components,
            prerelease,
        })
    }

    /// Component `index`, or zero past the end — the "missing reads as zero"
    /// rule that makes `15.2` and `15.2.0` one version.
    fn component(&self, index: usize) -> u64 {
        self.components.get(index).copied().unwrap_or(0)
    }

    /// Whether the numeric cores agree, ignoring prerelease tags. This is what
    /// the prerelease admission rule below is keyed on.
    fn same_release(&self, other: &Self) -> bool {
        let width = self.components.len().max(other.components.len());
        (0..width).all(|index| self.component(index) == other.component(index))
    }
}

impl PartialOrd for OrderedVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        let width = self.components.len().max(other.components.len());
        for index in 0..width {
            match self.component(index).cmp(&other.component(index)) {
                Ordering::Equal => {}
                unequal => return unequal,
            }
        }
        // Same numbers: a prerelease precedes its own release. Between two
        // prereleases this compares the tags as text, which is a deliberate
        // simplification — semver's dot-separated identifier rules exist to
        // order `alpha.2` under `alpha.10`, and no toolchain ships that.
        match (&self.prerelease, &other.prerelease) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(left), Some(right)) => left.cmp(right),
        }
    }
}

impl core::fmt::Display for OrderedVersion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let core = self
            .components
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(".");
        match &self.prerelease {
            Some(tag) => write!(f, "{core}-{tag}"),
            None => f.write_str(&core),
        }
    }
}

/// One comparison in a range.
///
/// Caret and tilde are deliberately absent. `^1.2.3` means "below 2.0.0"
/// because semver fixes which position is major; with free arity nobody can say
/// whether `^15.2` stops at `16` or at `15.3`, so the operator would mean
/// whatever this file decided and read like it meant what Cargo decided. A
/// toolchain pin is nearly always "at least this" or "exactly this" anyway.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Comparison {
    Greater,
    GreaterEq,
    Less,
    LessEq,
    Exact,
}

impl Comparison {
    fn parse(text: &str) -> Option<(Self, &str)> {
        for (token, comparison) in [
            (">=", Self::GreaterEq),
            ("<=", Self::LessEq),
            (">", Self::Greater),
            ("<", Self::Less),
            ("=", Self::Exact),
        ] {
            if let Some(rest) = text.strip_prefix(token) {
                return Some((comparison, rest.trim()));
            }
        }
        None
    }

    fn admits(self, ordering: Ordering) -> bool {
        match self {
            Self::Greater => ordering == Ordering::Greater,
            Self::GreaterEq => ordering != Ordering::Less,
            Self::Less => ordering == Ordering::Less,
            Self::LessEq => ordering != Ordering::Greater,
            Self::Exact => ordering == Ordering::Equal,
        }
    }
}

/// One bound of a range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bound {
    comparison: Comparison,
    version: OrderedVersion,
}

/// A conjunction of bounds — `>=1.89, <1.90` is both, and a version satisfies
/// the range only by satisfying every one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionRange {
    bounds: Vec<Bound>,
}

/// Why a range could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RangeParseError {
    /// Nothing between the commas.
    Empty,
    /// A bound with no comparator. Bare `1.56` is refused rather than being
    /// read as caret (Cargo's default) or as equality: those two readings
    /// disagree about whether `1.57` satisfies it, and the spelling that means
    /// one of them should say so.
    MissingComparison { bound: String },
    /// A bound whose version is not orderable — `>=22.1std` asks an ordering
    /// question about something that has no order.
    UnorderableBound { bound: String },
}

impl core::fmt::Display for RangeParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Empty => f.write_str("a version range needs at least one bound"),
            Self::MissingComparison { bound } => write!(
                f,
                "`{bound}` has no comparison; write `>={bound}` or `={bound}` \
                 (a bare version is not read as either)"
            ),
            Self::UnorderableBound { bound } => write!(
                f,
                "`{bound}` is not an orderable version, so it cannot bound a \
                 range; pin it exactly with `toolchain` instead"
            ),
        }
    }
}

impl VersionRange {
    /// Read a comma-separated conjunction of bounds.
    pub fn parse(text: &str) -> Result<Self, RangeParseError> {
        let mut bounds = Vec::new();
        for piece in text.split(',') {
            let piece = piece.trim();
            if piece.is_empty() {
                continue;
            }
            let Some((comparison, rest)) = Comparison::parse(piece) else {
                return Err(RangeParseError::MissingComparison {
                    bound: piece.to_owned(),
                });
            };
            let Some(version) = OrderedVersion::parse(rest) else {
                return Err(RangeParseError::UnorderableBound {
                    bound: piece.to_owned(),
                });
            };
            bounds.push(Bound {
                comparison,
                version,
            });
        }
        if bounds.is_empty() {
            return Err(RangeParseError::Empty);
        }
        Ok(Self { bounds })
    }

    /// Whether a stated version satisfies every bound.
    ///
    /// A prerelease is admitted only by a bound that names a prerelease of the
    /// SAME numeric core — Cargo's rule, kept on purpose. `1.99.0-nightly` does
    /// not satisfy `>=1.89`: a nightly is a materially different tool from the
    /// stable release whose number it carries, and admitting it silently is the
    /// pretending this whole vocabulary exists to refuse. Say you mean it
    /// (`>=1.99.0-nightly`) and it is admitted.
    #[must_use]
    pub fn matches(&self, version: &OrderedVersion) -> bool {
        if version.prerelease.is_some()
            && !self
                .bounds
                .iter()
                .any(|bound| bound.version.prerelease.is_some() && bound.version.same_release(version))
        {
            return false;
        }
        self.bounds
            .iter()
            .all(|bound| bound.comparison.admits(version.cmp(&bound.version)))
    }
}

impl core::fmt::Display for VersionRange {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let rendered = self
            .bounds
            .iter()
            .map(|bound| {
                let comparison = match bound.comparison {
                    Comparison::Greater => ">",
                    Comparison::GreaterEq => ">=",
                    Comparison::Less => "<",
                    Comparison::LessEq => "<=",
                    Comparison::Exact => "=",
                };
                format!("{comparison}{}", bound.version)
            })
            .collect::<Vec<_>>()
            .join(", ");
        f.write_str(&rendered)
    }
}

/// The characters a range bound can start with.
///
/// Used to catch `toolchain: ">=1.89"` — an exact pin whose value is plainly a
/// range. Without this the rename from #17's single `toolchain` key breaks
/// quietly: the string compares unequal to every stated version, forever, and
/// the refusal blames the machine for a source bug.
const COMPARISON_LEADS: [char; 5] = ['>', '<', '=', '^', '~'];

/// Whether this exact-pin text looks like somebody meant a range.
#[must_use]
pub fn looks_like_a_range(pin: &str) -> bool {
    pin.trim().starts_with(COMPARISON_LEADS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ordered(text: &str) -> OrderedVersion {
        OrderedVersion::parse(text).expect("orderable")
    }

    #[test]
    fn real_toolchain_versions_are_orderable_whatever_their_arity() {
        // The formats #17's strict semver rejected outright.
        assert_eq!(ordered("15.2").to_string(), "15.2"); // xcodebuild
        assert_eq!(ordered("19.38.33130.0").to_string(), "19.38.33130.0"); // MSVC
        assert_eq!(ordered("22.1").to_string(), "22.1"); // quartus, numeric edition
        assert_eq!(ordered("1.96.0").to_string(), "1.96.0"); // rustc
    }

    #[test]
    fn a_missing_component_reads_as_zero() {
        assert_eq!(ordered("15.2"), ordered("15.2.0"));
        assert_eq!(ordered("15.2").cmp(&ordered("15.2.0")), Ordering::Equal);
        assert!(ordered("15.2") < ordered("15.2.1"));
        assert!(ordered("15.10") > ordered("15.9"));
    }

    #[test]
    fn a_non_numeric_edition_is_not_orderable() {
        // Quartus's actual spelling. Refused rather than coerced — see the
        // module docs. `toolchain: "22.1std"` is how you ask for it.
        assert_eq!(OrderedVersion::parse("22.1std"), None);
        assert_eq!(OrderedVersion::parse("Pro 23.4"), None);
        assert_eq!(OrderedVersion::parse("v22.1"), None);
        assert_eq!(OrderedVersion::parse(""), None);
    }

    #[test]
    fn a_suffix_reads_as_a_prerelease_even_when_it_meant_an_increment() {
        // Pinned so the trap is visible rather than discovered: semver says a
        // `-suffix` precedes its release, so a service pack sorts BELOW the
        // version it patches. Correct for `1.99.0-nightly`, backwards for
        // `2023.09-SP1` — which is why such a tool wants an exact pin.
        assert!(ordered("2023.9-SP1") < ordered("2023.9"));
    }

    #[test]
    fn build_metadata_does_not_participate() {
        assert_eq!(ordered("1.96.0+abc123"), ordered("1.96.0"));
    }

    #[test]
    fn a_prerelease_precedes_its_release() {
        assert!(ordered("1.99.0-nightly") < ordered("1.99.0"));
        assert!(ordered("1.99.0-nightly") > ordered("1.98.0"));
    }

    #[test]
    fn ranges_admit_and_refuse_by_comparison() {
        let range = VersionRange::parse(">=1.56, <2").expect("range");
        assert!(range.matches(&ordered("1.96.0")));
        assert!(range.matches(&ordered("1.56")));
        assert!(!range.matches(&ordered("1.55.9")));
        assert!(!range.matches(&ordered("2.0.0")));
    }

    #[test]
    fn a_two_component_range_bounds_a_four_component_version() {
        // The MSVC shape: the machine states four components, the pin says two.
        let range = VersionRange::parse(">=19.38").expect("range");
        assert!(range.matches(&ordered("19.38.33130.0")));
        assert!(!range.matches(&ordered("19.37.99999.0")));
    }

    #[test]
    fn a_nightly_needs_naming_and_then_is_admitted() {
        let stable = VersionRange::parse(">=1.56, <2").expect("range");
        assert!(!stable.matches(&ordered("1.99.0-nightly")));
        let named = VersionRange::parse(">=1.99.0-nightly").expect("range");
        assert!(named.matches(&ordered("1.99.0-nightly")));
    }

    #[test]
    fn a_bare_bound_is_refused_rather_than_guessed() {
        assert!(matches!(
            VersionRange::parse("1.56"),
            Err(RangeParseError::MissingComparison { .. })
        ));
    }

    #[test]
    fn an_unorderable_bound_is_refused_with_the_exact_pin_as_the_way_out() {
        let error = VersionRange::parse(">=22.1std").expect_err("unorderable");
        assert!(matches!(error, RangeParseError::UnorderableBound { .. }));
        assert!(error.to_string().contains("pin it exactly"));
    }

    #[test]
    fn an_exact_pin_that_is_plainly_a_range_is_recognizable() {
        // The guard that keeps #17's `toolchain: ">=1.56, <2"` from silently
        // becoming a string nothing equals.
        assert!(looks_like_a_range(">=1.56, <2"));
        assert!(looks_like_a_range("^1.2"));
        assert!(looks_like_a_range(" <2"));
        assert!(!looks_like_a_range("22.1std"));
        assert!(!looks_like_a_range("15.2"));
    }
}
