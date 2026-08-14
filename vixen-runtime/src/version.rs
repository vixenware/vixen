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

/// Hashed over the SIGNIFICANT components, so two equal versions hash alike.
///
/// Written rather than derived for the same reason [`PartialEq`] is: `15.2` and
/// `15.2.0` are one version holding different component vectors. A derived
/// `Hash` would give them different hashes while `eq` called them equal, which
/// is the `Hash`/`Eq` contract broken — and a `HashMap` that loses entries
/// depending on how a manifest happened to spell a version.
impl core::hash::Hash for OrderedVersion {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        let significant = self
            .components
            .iter()
            .rposition(|component| *component != 0)
            .map_or(0, |last| last + 1);
        self.components[..significant].hash(state);
        self.prerelease.hash(state);
    }
}

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

// The guard that catches `toolchain: ">=1.89"` — an exact pin whose value is
// plainly a range — lives in `compiler::declare_capability_constraints`, not
// here. It has to: the refusal is a compile error, and `vix-core` cannot depend
// on this crate. A mirror of it here would be a second copy of one rule in a
// second crate, tested while the copy that actually runs was not. Its
// acceptance test is `machine_manifest::
// an_exact_pin_that_is_plainly_a_range_is_refused_at_compile_time`.


#[cfg(test)]
mod tests {
    use super::*;

    fn ordered(text: &str) -> OrderedVersion {
        OrderedVersion::parse(text).expect("orderable")
    }

    fn range(text: &str) -> VersionRange {
        VersionRange::parse(text).expect("a readable range")
    }

    /// `range` admits `stated`, asked the way the binder asks it.
    fn admits(range_text: &str, stated: &str) -> bool {
        range(range_text).matches(&ordered(stated))
    }

    // ---- parsing ----------------------------------------------------------

    #[test]
    fn the_four_real_toolchain_formats_all_read() {
        // The exact strings that strict semver rejected, which is why this
        // type exists. Two components, three, four, and a prerelease.
        assert_eq!(ordered("15.2").to_string(), "15.2"); // xcodebuild -version
        assert_eq!(ordered("19.38.33130.0").to_string(), "19.38.33130.0"); // MSVC cl
        assert_eq!(ordered("22.1").to_string(), "22.1"); // quartus, numeric edition
        assert_eq!(ordered("1.96.0").to_string(), "1.96.0"); // rustc
        assert_eq!(ordered("1.99.0-nightly").to_string(), "1.99.0-nightly");
    }

    #[test]
    fn a_single_component_is_a_version() {
        assert_eq!(ordered("22"), ordered("22.0.0"));
        assert!(ordered("22") < ordered("23"));
    }

    #[test]
    fn a_non_numeric_component_is_not_orderable() {
        // Quartus's real spelling, and the shapes near it.
        assert_eq!(OrderedVersion::parse("22.1std"), None);
        assert_eq!(OrderedVersion::parse("Pro 23.4"), None);
        assert_eq!(OrderedVersion::parse("v22.1"), None);
        assert_eq!(OrderedVersion::parse("2023R2"), None);
        assert_eq!(OrderedVersion::parse("15.2 beta 2"), None);
    }

    #[test]
    fn degenerate_texts_are_not_orderable() {
        assert_eq!(OrderedVersion::parse(""), None);
        assert_eq!(OrderedVersion::parse("."), None);
        assert_eq!(OrderedVersion::parse("1..2"), None);
        assert_eq!(OrderedVersion::parse("1.2."), None);
        // A dash with nothing after it is not a prerelease.
        assert_eq!(OrderedVersion::parse("1.0.0-"), None);
        // …and nothing before it is not a version.
        assert_eq!(OrderedVersion::parse("-1.0.0"), None);
        // Wider than u64.
        assert_eq!(OrderedVersion::parse("18446744073709551616"), None);
    }

    #[test]
    fn leading_zeros_are_just_zeros() {
        // `1.09` and `1.9` are the same number, however a tool spells it.
        assert_eq!(ordered("1.09"), ordered("1.9"));
        assert_eq!(ordered("01.02.03"), ordered("1.2.3"));
    }

    #[test]
    fn build_metadata_is_discarded_wherever_it_appears() {
        // Semver says build metadata does not participate in precedence, and a
        // toolchain that appends a hash is the same version of the same tool.
        assert_eq!(ordered("1.96.0+abc123"), ordered("1.96.0"));
        assert_eq!(ordered("1.96.0-nightly+abc123"), ordered("1.96.0-nightly"));
        // The `-` inside metadata is not a prerelease marker.
        assert_eq!(ordered("1.96.0+a-b"), ordered("1.96.0"));
    }

    #[test]
    fn a_prerelease_tag_keeps_its_own_dashes_and_dots() {
        assert_eq!(ordered("1.96.0-beta.2").to_string(), "1.96.0-beta.2");
        assert_eq!(ordered("1.0.0-alpha-1").to_string(), "1.0.0-alpha-1");
    }

    // ---- ordering ---------------------------------------------------------

    #[test]
    fn a_missing_component_reads_as_zero() {
        assert_eq!(ordered("15.2"), ordered("15.2.0"));
        assert_eq!(ordered("15.2"), ordered("15.2.0.0.0"));
        assert!(ordered("15.2") < ordered("15.2.1"));
        assert!(ordered("15.2.1") > ordered("15.2"));
    }

    #[test]
    fn components_compare_as_numbers_not_text() {
        // The bug string comparison would introduce: "9" > "10" lexically.
        assert!(ordered("15.9") < ordered("15.10"));
        assert!(ordered("1.9.0") < ordered("1.89.0"));
        assert!(ordered("19.37.99999.0") < ordered("19.38.0.0"));
    }

    #[test]
    fn equality_agrees_with_the_ordering() {
        // Derived `PartialEq` would call these unequal (different component
        // vectors) while `cmp` calls them equal — an inconsistency that reads
        // fine in a comparison and corrupts a `BTreeMap`.
        let short = ordered("15.2");
        let long = ordered("15.2.0");
        assert_eq!(short, long);
        assert_eq!(short.cmp(&long), Ordering::Equal);
        let mut set = std::collections::BTreeSet::new();
        set.insert(short);
        set.insert(long);
        assert_eq!(set.len(), 1, "one version, however it was spelled");
    }

    #[test]
    fn the_ordering_is_total_and_sorts() {
        let mut versions = [
            ordered("1.99.0"),
            ordered("15.2"),
            ordered("1.99.0-nightly"),
            ordered("1.0"),
            ordered("19.38.33130.0"),
            ordered("1.98.0"),
        ];
        versions.sort();
        let rendered: Vec<String> = versions.iter().map(ToString::to_string).collect();
        assert_eq!(
            rendered,
            vec![
                "1.0",
                "1.98.0",
                "1.99.0-nightly",
                "1.99.0",
                "15.2",
                "19.38.33130.0",
            ]
        );
    }

    // ---- prereleases ------------------------------------------------------

    #[test]
    fn a_prerelease_precedes_its_own_release() {
        assert!(ordered("1.99.0-nightly") < ordered("1.99.0"));
        assert!(ordered("1.96.0-beta.2") < ordered("1.96.0"));
    }

    #[test]
    fn a_prerelease_still_outranks_an_earlier_release() {
        assert!(ordered("1.99.0-nightly") > ordered("1.98.0"));
        assert!(ordered("1.99.0-nightly") > ordered("1.98.9"));
    }

    #[test]
    fn prereleases_of_one_release_order_among_themselves() {
        assert!(ordered("1.0.0-alpha") < ordered("1.0.0-beta"));
        assert!(ordered("1.0.0-beta") < ordered("1.0.0-nightly"));
        assert_eq!(ordered("1.0.0-beta"), ordered("1.0.0-beta"));
    }

    #[test]
    fn a_suffix_reads_as_a_prerelease_even_when_it_meant_an_increment() {
        // Pinned so the trap is visible rather than discovered. Semver says a
        // `-suffix` precedes its release, which is right for `-nightly` and
        // backwards for a service pack — `2023.9-SP1` is an increment ON
        // `2023.9`, not a preview of it. Nothing here can tell them apart, so
        // a tool that versions that way wants an exact pin.
        assert!(ordered("2023.9-SP1") < ordered("2023.9"));
    }

    #[test]
    fn a_nightly_does_not_satisfy_a_stable_range() {
        // Cargo's rule, kept deliberately: a nightly is a materially different
        // tool from the stable release whose number it carries, and admitting
        // it silently is the pretending this vocabulary exists to refuse.
        assert!(!admits(">=1.89", "1.99.0-nightly"));
        assert!(!admits(">=1.89, <2", "1.99.0-nightly"));
        assert!(!admits("<2", "1.99.0-nightly"));
        assert!(!admits(">=1.0", "1.99.0-nightly"));
        // Even when the numbers alone would sit comfortably inside.
        assert!(!admits(">=1.98, <2.0", "1.99.0-nightly"));
    }

    #[test]
    fn a_nightly_named_by_the_bound_is_admitted() {
        assert!(admits(">=1.99.0-nightly", "1.99.0-nightly"));
        assert!(admits("=1.99.0-nightly", "1.99.0-nightly"));
        assert!(admits(">=1.99.0-nightly, <2", "1.99.0-nightly"));
    }

    #[test]
    fn naming_a_prerelease_of_a_different_release_does_not_admit_this_one() {
        // The bound must name a prerelease of the SAME numeric core. Otherwise
        // `>=1.98.0-nightly` would quietly open the door to every later
        // nightly, which is the blanket admission the rule exists to prevent.
        assert!(!admits(">=1.98.0-nightly", "1.99.0-nightly"));
        assert!(!admits(">=1.98.0-nightly, <2", "1.99.0-nightly"));
    }

    #[test]
    fn one_bound_naming_the_prerelease_is_enough_to_consider_it() {
        // The gate asks whether ANY bound names this release's prerelease;
        // the bounds themselves are still all enforced afterwards.
        assert!(admits(">=1.99.0-nightly, <2.0", "1.99.0-nightly"));
        // Considered, then refused on the numbers: below the lower bound.
        assert!(!admits(">=1.99.1-nightly, <2.0", "1.99.1-alpha"));
    }

    #[test]
    fn a_prerelease_bound_does_not_disturb_stable_versions() {
        // The gate only applies to a prerelease STATED version. A stable
        // toolchain compares against a prerelease bound on the numbers, where
        // a release outranks its own prerelease.
        assert!(admits(">=1.99.0-nightly", "1.99.0"));
        assert!(admits(">=1.99.0-nightly", "2.0.0"));
        assert!(!admits(">=1.99.0-nightly", "1.98.0"));
    }

    #[test]
    fn a_named_nightly_still_answers_to_the_bounds() {
        // `<1.0.0` does NOT exclude it: a prerelease precedes its release, so
        // `1.0.0-nightly` really is below `1.0.0`. Naming it admits it and the
        // numbers then agree.
        assert!(admits(">=1.0.0-nightly, <1.0.0", "1.0.0-nightly"));
        // An upper bound below the prerelease does exclude it, though — the
        // gate decides whether it is CONSIDERED, never whether it passes.
        assert!(!admits(">=0.9, <1.0.0-alpha", "1.0.0-nightly"));
    }

    #[test]
    fn rustc_channel_shapes_round_trip() {
        // The three things `rustc --version` actually reports.
        assert!(admits(">=1.56, <2", "1.96.0"));
        assert!(!admits(">=1.56, <2", "1.96.0-nightly"));
        assert!(!admits(">=1.56, <2", "1.96.0-beta.2"));
        assert!(admits(">=1.96.0-beta.2", "1.96.0-beta.2"));
    }

    // ---- ranges -----------------------------------------------------------

    #[test]
    fn every_comparison_admits_what_it_says() {
        assert!(admits(">1.0", "1.0.1"));
        assert!(!admits(">1.0", "1.0"));
        assert!(admits(">=1.0", "1.0"));
        assert!(!admits(">=1.0", "0.9"));
        assert!(admits("<1.0", "0.9"));
        assert!(!admits("<1.0", "1.0"));
        assert!(admits("<=1.0", "1.0"));
        assert!(!admits("<=1.0", "1.0.1"));
        assert!(admits("=1.0", "1.0.0"));
        assert!(!admits("=1.0", "1.0.1"));
    }

    #[test]
    fn bounds_conjoin() {
        assert!(admits(">=1.56, <2", "1.96.0"));
        assert!(!admits(">=1.56, <2", "1.55.0"));
        assert!(!admits(">=1.56, <2", "2.0.0"));
        // Three bounds, all enforced.
        assert!(admits(">=1.0, <2.0, =1.5", "1.5"));
        assert!(!admits(">=1.0, <2.0, =1.5", "1.6"));
    }

    #[test]
    fn a_range_and_a_version_may_differ_in_arity_either_way() {
        // MSVC: four stated components, a two-component bound.
        assert!(admits(">=19.38", "19.38.33130.0"));
        assert!(!admits(">=19.38", "19.37.99999.0"));
        // And the other direction: two stated, four in the bound.
        assert!(admits(">=15.2.0.0", "15.2"));
        assert!(!admits(">15.2.0.0", "15.2"));
    }

    #[test]
    fn whitespace_around_bounds_is_not_significant() {
        assert!(admits("  >=1.89 ,  <1.90  ", "1.89.5"));
        assert!(admits(">= 1.89", "1.89.5"));
    }

    #[test]
    fn a_trailing_comma_is_not_an_empty_bound() {
        assert!(admits(">=1.89,", "1.96.0"));
    }

    #[test]
    fn a_range_with_no_bounds_is_refused() {
        assert!(matches!(VersionRange::parse(""), Err(RangeParseError::Empty)));
        assert!(matches!(
            VersionRange::parse("   "),
            Err(RangeParseError::Empty)
        ));
        assert!(matches!(
            VersionRange::parse(",,,"),
            Err(RangeParseError::Empty)
        ));
    }

    #[test]
    fn a_bare_bound_is_refused_rather_than_guessed() {
        // `1.56` reads as caret to Cargo and as equality to everyone else, and
        // those two disagree about whether `1.57` satisfies it. The spelling
        // that means one of them should say which.
        let error = VersionRange::parse("1.56").expect_err("bare");
        assert!(matches!(error, RangeParseError::MissingComparison { .. }));
        assert!(error.to_string().contains(">=1.56"));
        assert!(matches!(
            VersionRange::parse(">=1.0, 2.0"),
            Err(RangeParseError::MissingComparison { .. })
        ));
    }

    #[test]
    fn caret_and_tilde_are_not_comparisons_here() {
        // Absent on purpose: `^1.2.3` bounds at `2.0.0` because semver fixes
        // which position is major, and with free arity nobody can say whether
        // `^15.2` stops at `16` or at `15.3`.
        assert!(matches!(
            VersionRange::parse("^1.2"),
            Err(RangeParseError::MissingComparison { .. })
        ));
        assert!(matches!(
            VersionRange::parse("~1.2"),
            Err(RangeParseError::MissingComparison { .. })
        ));
    }

    #[test]
    fn an_unorderable_bound_is_refused_and_names_the_way_out() {
        let error = VersionRange::parse(">=22.1std").expect_err("unorderable");
        assert!(matches!(error, RangeParseError::UnorderableBound { .. }));
        assert!(
            error.to_string().contains("pin it exactly"),
            "the message points at the exact pin: {error}"
        );
        assert!(matches!(
            VersionRange::parse(">=1.0, <22.1std"),
            Err(RangeParseError::UnorderableBound { .. })
        ));
    }

    #[test]
    fn a_comparison_with_no_version_is_refused() {
        assert!(matches!(
            VersionRange::parse(">="),
            Err(RangeParseError::UnorderableBound { .. })
        ));
    }

    #[test]
    fn a_range_renders_back_to_its_meaning() {
        assert_eq!(range(">=1.89, <1.90").to_string(), ">=1.89, <1.90");
        assert_eq!(range("  >= 1.89  ").to_string(), ">=1.89");
    }

    // ---- the exact-pin guard ---------------------------------------------

    #[test]
    fn equal_versions_hash_alike() {
        use std::collections::HashSet;

        // The `Hash`/`Eq` contract, which a derived `Hash` would break: these
        // are one version spelled two ways, and a `HashMap` keyed on them must
        // not care which spelling a manifest used.
        let mut seen = HashSet::new();
        seen.insert(ordered("15.2"));
        seen.insert(ordered("15.2.0"));
        seen.insert(ordered("15.2.0.0"));
        assert_eq!(seen.len(), 1, "one version, however it was spelled");
        assert!(seen.contains(&ordered("15.2.0")));

        // Zero is the boundary case: every component insignificant.
        let mut zeros = HashSet::new();
        zeros.insert(ordered("0"));
        zeros.insert(ordered("0.0.0"));
        assert_eq!(zeros.len(), 1);

        // A prerelease is significant and does not collide with its release.
        let mut channels = HashSet::new();
        channels.insert(ordered("1.99.0"));
        channels.insert(ordered("1.99.0-nightly"));
        assert_eq!(channels.len(), 2);
    }
}
