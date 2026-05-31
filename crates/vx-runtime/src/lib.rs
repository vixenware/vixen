//! `vx-runtime` — the demand-driven Vixen runtime.
//!
//! A lazy **demand sea** with eager pure **kernel islands** (see
//! `docs/design/kernel-abi.md`). Per `i[impl.exec.shared-evaluator]` it is linked
//! by **both** the orchestrator and the executor, so a shipped closure evaluates
//! the same on either side.
//!
//! It **replaces** the eager evaluator (`vx-daemon`'s `runtime_eval` core, the
//! `vx-ir` back-end `KernelOp`/`lower`/`demand`, and the `vx-rt` crate). Those
//! are removed at the cutover phase, not before.
//!
//! The pieces, demand-in to value-out:
//! - [`analyze`] — VIR → the [`graph`] demand graph (with the expr/binding indices
//!   [`dispatch`] needs).
//! - [`graph`] — the demand graph: lazy nodes, spans, bombs.
//! - [`lower`] — a scalar region of the graph → [`kernel`] IR (`NotLowerable` when
//!   it isn't pure-scalar).
//! - [`kernel`] — the kernel IR + its interpreter, which is the **oracle** the JIT
//!   is checked against.
//! - [`jit`] — the native copy-and-patch JIT (native target only; the interpreter
//!   is the path elsewhere).
//! - [`dispatch`] — decides whether a (sub-)expression runs as a kernel and builds
//!   the plan; [`eval`] is the tree-walker that consults it.
//! - [`strand`] — the demand-driven scheduler (our own, **not** tokio tasks; the
//!   I1–I7 contract in `runtime-semantics.md` §2).
//! - [`remote`] — cross-executor values as progressive per-part handles over vox.
//!
//! Before changing anything here, read `AGENTS.md` and
//! `docs/design/runtime-semantics.md`. The non-negotiables (no `Pending(String)`,
//! no flat ISA, pending-is-a-resolver, bomb-detonates-on-force) are load-bearing.

pub mod analyze;
pub mod dispatch;
pub mod eval;
pub mod graph;
pub mod kernel;
pub mod lower;
pub mod remote;
pub mod strand;

/// Stencil machine code extracted from rustc's object output at build time
/// (`build.rs`). Only on the native target; the interpreter is the path elsewhere.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod stencils {
    include!(concat!(env!("OUT_DIR"), "/stencils.rs"));
}

/// The native copy-and-patch JIT: run the kernel stencils as machine code from
/// `MAP_JIT` memory. `kernel.rs` is its oracle.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub mod jit;
