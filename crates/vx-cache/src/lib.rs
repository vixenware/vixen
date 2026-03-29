//! Cache key computation for Vixen exec requests.
//!
//! The daemon and runner share this crate so both sides agree on how a
//! sandboxed `run` request should be keyed.

use vx_runner_proto::RunRequest;
use vx_store_proto::Blake3Hash;

/// Current cache key schema version for generic tool execution.
/// Version 10: tracked mounts contribute only structural identity; opaque mounts
/// keep whole-bundle identity in the request key.
pub const RUN_CACHE_KEY_VERSION: u32 = 10;

/// Compute a cache key for a generic `run` request.
pub fn run_cache_key(_request: &RunRequest) -> Blake3Hash {
    todo!("re-implement")
}
