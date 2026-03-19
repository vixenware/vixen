//! Experimental evaluator v2 core model.
//!
//! This crate is intentionally narrow. It is not a second production
//! evaluator yet. It exists to let us settle the core semantic data model
//! without inheriting `vx-eval`'s current layering problems.
//!
//! The key differences from v1 are:
//! - payload and metadata are separate (`ValueRep` vs `ValueMeta`)
//! - readiness is not a value (`Resolution` is separate)
//! - host observations live next to any value, not in a wrapper enum case
//! - provenance is always available, even on successful scalar values
//! - boundary mobility is explicit metadata, not an implicit side effect

mod meta;
pub use meta::{BoundaryConversion, Observation, PinnedReason, Portability, ValueMeta};

mod engine;
pub use engine::eval_source_entry;

mod provenance;
pub use provenance::{ProjectionStep, Provenance, ProvenanceRoot, RuntimeInputKind};

mod result;
pub use result::{DemandFrame, EvalError, EvalErrorKind, PendingRef, Resolution};

mod value;
pub use value::{BlobRep, FunctionRef, HostRef, MapValue, StructValue, TreeRep, Value, ValueRep};

#[cfg(test)]
mod tests;
