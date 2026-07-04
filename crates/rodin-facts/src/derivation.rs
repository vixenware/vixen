use std::collections::{BTreeMap, BTreeSet};

use crate::{ClauseId, ExclusionId, VersionSet};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DerivationId(pub u32);

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PinRef(pub String);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WidenCert {
    pub package: crate::PkgIx,
    pub checked: VersionSet,
    pub manifest_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Premise {
    Clause(ClauseId),
    Exclusion(ExclusionId),
    Pin(PinRef),
    Hypothesis(u32),
    Derived(DerivationId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuleKind {
    Conflict,
    Resolve,
    Widen { certificate: WidenCert },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Derivation {
    pub premises: Vec<Premise>,
    pub rule: RuleKind,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DerivationStore {
    derivations: Vec<Derivation>,
    clauses: BTreeSet<ClauseId>,
    exclusions: BTreeMap<ExclusionId, DerivationId>,
    pins: BTreeSet<PinRef>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayReport {
    pub visited: BTreeSet<DerivationId>,
    pub hypotheses: BTreeSet<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplayError {
    MissingDerivation(DerivationId),
    MissingClause(ClauseId),
    MissingExclusion(ExclusionId),
    MissingPin(PinRef),
    Cycle(Vec<DerivationId>),
    HypothesisOutsideConflict(u32),
}

impl DerivationStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn append(&mut self, derivation: Derivation) -> DerivationId {
        let id = DerivationId(u32::try_from(self.derivations.len()).expect("too many derivations"));
        self.derivations.push(derivation);
        id
    }

    pub fn note_clause(&mut self, id: ClauseId) {
        self.clauses.insert(id);
    }

    pub fn note_exclusion(&mut self, id: ExclusionId, derivation: DerivationId) {
        self.exclusions.insert(id, derivation);
    }

    #[must_use]
    pub fn exclusion_derivation(&self, id: ExclusionId) -> Option<DerivationId> {
        self.exclusions.get(&id).copied()
    }

    pub fn note_pin(&mut self, pin: PinRef) {
        self.pins.insert(pin);
    }

    #[must_use]
    pub fn get(&self, id: DerivationId) -> Option<&Derivation> {
        self.derivations.get(id.0 as usize)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.derivations.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.derivations.is_empty()
    }

    pub fn replay(&self, id: DerivationId) -> Result<ReplayReport, ReplayError> {
        let mut report = ReplayReport {
            visited: BTreeSet::new(),
            hypotheses: BTreeSet::new(),
        };
        let mut stack = Vec::new();
        self.replay_from(id, false, &mut stack, &mut report)?;
        Ok(report)
    }

    fn replay_from(
        &self,
        id: DerivationId,
        hypothesis_allowed: bool,
        stack: &mut Vec<DerivationId>,
        report: &mut ReplayReport,
    ) -> Result<(), ReplayError> {
        if let Some(position) = stack.iter().position(|seen| *seen == id) {
            let mut cycle = stack[position..].to_vec();
            cycle.push(id);
            return Err(ReplayError::Cycle(cycle));
        }
        if report.visited.contains(&id) {
            return Ok(());
        }
        let derivation = self.get(id).ok_or(ReplayError::MissingDerivation(id))?;
        stack.push(id);
        let premise_hypothesis_allowed =
            hypothesis_allowed || matches!(derivation.rule, RuleKind::Conflict);
        for premise in &derivation.premises {
            self.check_premise(premise_hypothesis_allowed, premise, stack, report)?;
        }
        stack.pop();
        report.visited.insert(id);
        Ok(())
    }

    fn check_premise(
        &self,
        hypothesis_allowed: bool,
        premise: &Premise,
        stack: &mut Vec<DerivationId>,
        report: &mut ReplayReport,
    ) -> Result<(), ReplayError> {
        match premise {
            Premise::Clause(id) => {
                if self.clauses.contains(id) {
                    Ok(())
                } else {
                    Err(ReplayError::MissingClause(*id))
                }
            }
            Premise::Exclusion(id) => {
                let derivation = *self
                    .exclusions
                    .get(id)
                    .ok_or(ReplayError::MissingExclusion(*id))?;
                self.replay_from(derivation, hypothesis_allowed, stack, report)
            }
            Premise::Pin(pin) => {
                if self.pins.contains(pin) {
                    Ok(())
                } else {
                    Err(ReplayError::MissingPin(pin.clone()))
                }
            }
            Premise::Hypothesis(id) => {
                if hypothesis_allowed {
                    report.hypotheses.insert(*id);
                    Ok(())
                } else {
                    Err(ReplayError::HypothesisOutsideConflict(*id))
                }
            }
            Premise::Derived(id) => self.replay_from(*id, hypothesis_allowed, stack, report),
        }
    }
}
