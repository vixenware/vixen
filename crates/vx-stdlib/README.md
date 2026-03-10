This directory holds declaration-only standard library headers for Vixen.

These files are the intended source of truth for the builtin language
surface that cannot be implemented as ordinary user code:

- opaque runtime-backed types
- builtin namespaces and methods
- capability-granting intrinsics like `exec`, `discover`, and `stabilize`

They are ordinary `.vx` source files on disk so tooling can eventually
jump to real definitions instead of synthetic builtin tables.

This first cut is intentionally narrow and declaration-only. Runtime
behavior still lives in Rust.
