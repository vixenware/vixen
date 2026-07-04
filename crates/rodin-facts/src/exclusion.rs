use crate::{DerivationId, Region};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ExclusionId(pub u32);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Exclusion {
    pub region: Region,
    pub derivation: DerivationId,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExclusionStore {
    exclusions: Vec<Exclusion>,
}

impl ExclusionStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn append(&mut self, exclusion: Exclusion) -> ExclusionId {
        let id = ExclusionId(u32::try_from(self.exclusions.len()).expect("too many exclusions"));
        self.exclusions.push(exclusion);
        id
    }

    #[must_use]
    pub fn get(&self, id: ExclusionId) -> Option<&Exclusion> {
        self.exclusions.get(id.0 as usize)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.exclusions.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.exclusions.is_empty()
    }

    #[must_use]
    pub fn excluded_superset_of(&self, region: &Region) -> Option<ExclusionId> {
        self.exclusions
            .iter()
            .position(|exclusion| exclusion.region.contains_region(region))
            .map(|index| ExclusionId(index as u32))
    }

    pub fn overlapping<'a>(&'a self, region: &'a Region) -> impl Iterator<Item = ExclusionId> + 'a {
        self.exclusions
            .iter()
            .enumerate()
            .filter(|(_, exclusion)| exclusion.region.overlaps(region))
            .map(|(index, _)| ExclusionId(index as u32))
    }
}
