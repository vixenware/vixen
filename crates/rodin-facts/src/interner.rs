use std::{borrow::Borrow, collections::BTreeMap, fmt};

use semver::Version;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PkgIx(pub u32);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceIx(pub u32);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FeatIx(pub u32);

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PkgId {
    pub source: SourceIx,
    pub name: String,
    pub compat: Option<CompatClass>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CompatClass {
    pub major: u64,
    pub minor: Option<u64>,
    pub patch: Option<u64>,
}

pub type SourceInterner = Interner<SourceIx>;
pub type FeatureInterner = Interner<FeatIx>;
pub type PackageInterner = Interner<PkgIx, PkgId>;

impl SourceIx {
    pub const DEFAULT: Self = Self(0);
}

impl PkgId {
    #[must_use]
    pub fn new(source: SourceIx, name: impl Into<String>) -> Self {
        Self {
            source,
            name: name.into(),
            compat: None,
        }
    }

    #[must_use]
    pub fn default_source(name: impl Into<String>) -> Self {
        Self::new(SourceIx::DEFAULT, name)
    }

    #[must_use]
    pub fn domain(source: SourceIx, name: impl Into<String>, compat: CompatClass) -> Self {
        Self {
            source,
            name: name.into(),
            compat: Some(compat),
        }
    }
}

impl CompatClass {
    #[must_use]
    pub fn from_version(version: &Version) -> Self {
        if version.major != 0 {
            Self {
                major: version.major,
                minor: None,
                patch: None,
            }
        } else if version.minor != 0 {
            Self {
                major: 0,
                minor: Some(version.minor),
                patch: None,
            }
        } else {
            Self {
                major: 0,
                minor: Some(0),
                patch: Some(version.patch),
            }
        }
    }
}

impl fmt::Display for CompatClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.minor, self.patch) {
            (Some(minor), Some(patch)) => write!(f, "{}.{minor}.{patch}", self.major),
            (Some(minor), None) => write!(f, "{}.{minor}", self.major),
            (None, None) => write!(f, "{}", self.major),
            (None, Some(_)) => unreachable!("patch class requires minor class"),
        }
    }
}

pub trait InternIx: Copy + Ord {
    fn from_usize(value: usize) -> Self;
    fn to_usize(self) -> usize;
}

pub trait InternerKey: Clone + Ord {
    type Resolved: ?Sized;

    fn resolved(&self) -> &Self::Resolved;
}

impl InternerKey for String {
    type Resolved = str;

    fn resolved(&self) -> &Self::Resolved {
        self
    }
}

impl InternerKey for PkgId {
    type Resolved = PkgId;

    fn resolved(&self) -> &Self::Resolved {
        self
    }
}

impl InternIx for PkgIx {
    fn from_usize(value: usize) -> Self {
        Self(u32::try_from(value).expect("too many interned packages"))
    }

    fn to_usize(self) -> usize {
        self.0 as usize
    }
}

impl InternIx for SourceIx {
    fn from_usize(value: usize) -> Self {
        Self(u32::try_from(value).expect("too many interned sources"))
    }

    fn to_usize(self) -> usize {
        self.0 as usize
    }
}

impl InternIx for FeatIx {
    fn from_usize(value: usize) -> Self {
        Self(u32::try_from(value).expect("too many interned features"))
    }

    fn to_usize(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Interner<Ix, Key = String> {
    values: Vec<Key>,
    by_value: BTreeMap<Key, Ix>,
}

impl<Ix: InternIx, Key: InternerKey> Interner<Ix, Key> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            values: Vec::new(),
            by_value: BTreeMap::new(),
        }
    }

    pub fn intern(&mut self, value: impl Into<Key>) -> Ix {
        let value = value.into();
        if let Some(ix) = self.by_value.get(&value) {
            return *ix;
        }
        let ix = Ix::from_usize(self.values.len());
        self.values.push(value.clone());
        self.by_value.insert(value, ix);
        ix
    }

    #[must_use]
    pub fn get<Q>(&self, value: &Q) -> Option<Ix>
    where
        Key: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.by_value.get(value).copied()
    }

    #[must_use]
    pub fn resolve(&self, ix: Ix) -> Option<&Key::Resolved> {
        self.values.get(ix.to_usize()).map(InternerKey::resolved)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

impl<Ix: InternIx> Interner<Ix, String> {
    #[must_use]
    pub fn get_str(&self, name: &str) -> Option<Ix> {
        self.get(name)
    }

    #[must_use]
    pub fn resolve_str(&self, ix: Ix) -> Option<&str> {
        self.resolve(ix)
    }
}

impl PackageInterner {
    pub fn intern_pkg(&mut self, source: SourceIx, name: impl Into<String>) -> PkgIx {
        self.intern(PkgId::new(source, name))
    }

    pub fn intern_domain(
        &mut self,
        source: SourceIx,
        name: impl Into<String>,
        compat: CompatClass,
    ) -> PkgIx {
        self.intern(PkgId::domain(source, name, compat))
    }

    pub fn intern_default(&mut self, name: impl Into<String>) -> PkgIx {
        self.intern(PkgId::default_source(name))
    }
}
