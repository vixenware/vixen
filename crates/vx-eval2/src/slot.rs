//! Rewriteable slot storage and resolution primitives.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::Claim;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SlotId(Arc<str>);

impl SlotId {
    pub fn new(id: impl Into<Arc<str>>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for SlotId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for SlotId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<Arc<str>> for SlotId {
    fn from(value: Arc<str>) -> Self {
        Self::new(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotAction<Intent> {
    Current,
    Refresh(Intent),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotResolved<Value, VerifierPayload> {
    pub value: Value,
    pub claims: Vec<Claim>,
    pub verifier_payload: Option<VerifierPayload>,
}

impl<Value, VerifierPayload> SlotResolved<Value, VerifierPayload> {
    pub fn new(value: Value) -> Self {
        Self {
            value,
            claims: Vec::new(),
            verifier_payload: None,
        }
    }

    pub fn with_claims(mut self, claims: Vec<Claim>) -> Self {
        self.claims = claims;
        self
    }

    pub fn with_verifier_payload(mut self, verifier_payload: VerifierPayload) -> Self {
        self.verifier_payload = Some(verifier_payload);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotVerification {
    Valid,
    Invalid { reason: Arc<str> },
}

impl SlotVerification {
    pub fn invalid(reason: impl Into<Arc<str>>) -> Self {
        Self::Invalid {
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotStoreReason {
    Missing,
    WitnessChanged,
    ClaimsInvalid { reason: Arc<str> },
    Refresh,
}

impl SlotStoreReason {
    pub fn is_rewrite(&self) -> bool {
        !matches!(self, Self::Missing)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotRecordMeta {
    pub write_count: u64,
    pub rewrite_count: u64,
    pub last_store_reason: SlotStoreReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotRecord<Value, Witness, VerifierPayload> {
    pub slot_id: SlotId,
    pub value: Value,
    pub witness: Witness,
    pub claims: Vec<Claim>,
    pub verifier_payload: Option<VerifierPayload>,
    pub meta: SlotRecordMeta,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotResolveRequest<'a, Value, Witness, Intent, VerifierPayload> {
    pub witness: &'a Witness,
    pub current: Option<&'a SlotRecord<Value, Witness, VerifierPayload>>,
    pub action: &'a SlotAction<Intent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotResolutionKind {
    Reused,
    Stored { reason: SlotStoreReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotResolution<Value, Witness, VerifierPayload> {
    pub kind: SlotResolutionKind,
    pub record: SlotRecord<Value, Witness, VerifierPayload>,
}

#[derive(Debug, Clone)]
pub struct SlotStore<Value, Witness, VerifierPayload> {
    records: BTreeMap<SlotId, SlotRecord<Value, Witness, VerifierPayload>>,
}

impl<Value, Witness, VerifierPayload> Default for SlotStore<Value, Witness, VerifierPayload> {
    fn default() -> Self {
        Self {
            records: BTreeMap::new(),
        }
    }
}

impl<Value, Witness, VerifierPayload> SlotStore<Value, Witness, VerifierPayload>
where
    Value: Clone,
    Witness: Clone + PartialEq,
    VerifierPayload: Clone,
{
    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn get(&self, slot_id: &SlotId) -> Option<&SlotRecord<Value, Witness, VerifierPayload>> {
        self.records.get(slot_id)
    }

    pub fn resolve<Intent, Error, Verify, Resolve>(
        &mut self,
        slot_id: impl Into<SlotId>,
        witness: Witness,
        action: SlotAction<Intent>,
        verify: Verify,
        resolve: Resolve,
    ) -> Result<SlotResolution<Value, Witness, VerifierPayload>, Error>
    where
        Verify: FnOnce(&SlotRecord<Value, Witness, VerifierPayload>) -> SlotVerification,
        Resolve: FnOnce(
            SlotResolveRequest<'_, Value, Witness, Intent, VerifierPayload>,
        ) -> Result<SlotResolved<Value, VerifierPayload>, Error>,
    {
        let slot_id = slot_id.into();
        let current = self.records.get(&slot_id).cloned();
        let store_reason = match (&action, current.as_ref()) {
            (SlotAction::Refresh(_), _) => Some(SlotStoreReason::Refresh),
            (SlotAction::Current, None) => Some(SlotStoreReason::Missing),
            (SlotAction::Current, Some(current)) if current.witness != witness => {
                Some(SlotStoreReason::WitnessChanged)
            }
            (SlotAction::Current, Some(current)) => match verify(current) {
                SlotVerification::Valid => None,
                SlotVerification::Invalid { reason } => {
                    Some(SlotStoreReason::ClaimsInvalid { reason })
                }
            },
        };

        if let Some(reason) = store_reason {
            let resolved = resolve(SlotResolveRequest {
                witness: &witness,
                current: current.as_ref(),
                action: &action,
            })?;
            let previous_meta = current.as_ref().map(|current| &current.meta);
            let record = SlotRecord {
                slot_id: slot_id.clone(),
                value: resolved.value,
                witness,
                claims: resolved.claims,
                verifier_payload: resolved.verifier_payload,
                meta: SlotRecordMeta {
                    write_count: previous_meta.map_or(1, |meta| meta.write_count + 1),
                    rewrite_count: previous_meta.map_or(0, |meta| {
                        meta.rewrite_count + u64::from(reason.is_rewrite())
                    }),
                    last_store_reason: reason.clone(),
                },
            };
            self.records.insert(slot_id, record.clone());
            return Ok(SlotResolution {
                kind: SlotResolutionKind::Stored { reason },
                record,
            });
        }

        let record = current.expect("slot resolution should reuse an existing record");
        Ok(SlotResolution {
            kind: SlotResolutionKind::Reused,
            record,
        })
    }
}
