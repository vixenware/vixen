use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PkgIx(pub u32);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FeatIx(pub u32);

pub trait InternIx: Copy + Ord {
    fn from_usize(value: usize) -> Self;
    fn to_usize(self) -> usize;
}

impl InternIx for PkgIx {
    fn from_usize(value: usize) -> Self {
        Self(u32::try_from(value).expect("too many interned packages"))
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
pub struct Interner<Ix> {
    names: Vec<String>,
    by_name: BTreeMap<String, Ix>,
}

impl<Ix: InternIx> Interner<Ix> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            names: Vec::new(),
            by_name: BTreeMap::new(),
        }
    }

    pub fn intern(&mut self, name: impl Into<String>) -> Ix {
        let name = name.into();
        if let Some(ix) = self.by_name.get(&name) {
            return *ix;
        }
        let ix = Ix::from_usize(self.names.len());
        self.names.push(name.clone());
        self.by_name.insert(name, ix);
        ix
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<Ix> {
        self.by_name.get(name).copied()
    }

    #[must_use]
    pub fn resolve(&self, ix: Ix) -> Option<&str> {
        self.names.get(ix.to_usize()).map(String::as_str)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.names.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}
