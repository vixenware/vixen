//! `vx-runtime` — the demand-driven Vixen runtime.
//!
//! A lazy **demand sea** with eager pure **kernel islands** (see
//! `docs/design/kernel-abi.md`). Per `i[impl.exec.shared-evaluator]` it is linked
//! by **both** the orchestrator and the executor, so a shipped closure evaluates
//! the same on either side.
//!
//! It **replaces** the eager evaluator. The standalone `vx-rt` crate is now
//! removed; `vx-daemon`'s `runtime_eval` core and the `vx-ir` back-end
//! (`KernelOp`/`lower`/`demand`) remain until the daemon cuts over.
//!
//! The pieces, demand-in to value-out:
//! - [`analyze`] — VIR → the [`graph`] demand graph.
//! - [`graph`] — the demand graph: lazy nodes, spans, bombs.
//! - `graph_exec` — source entry → graph driver. This is the new public source
//!   execution path while the rest of the runtime is being cut over.
//! - [`lower`] — a scalar region of the graph → [`kernel`] IR (`NotLowerable` when
//!   it isn't pure-scalar).
//! - [`kernel`] — the kernel IR + its interpreter, used by the JIT path.
//! - [`jit`] — the native copy-and-patch JIT (native target only; the interpreter
//!   is the path elsewhere).
//! - [`strand`] — the demand-driven scheduler (our own, **not** tokio tasks; the
//!   I1–I7 contract on the strand-scheduler wiki page).
//! - [`remote`] — cross-executor values as progressive per-part handles over vox.
//!
//! Before changing anything here, read `AGENTS.md` and the wiki's
//! demand-driven-runtime + strand-scheduler pages. The non-negotiables (no `Pending(String)`,
//! no flat ISA, pending-is-a-resolver, bomb-detonates-on-force) are load-bearing.

pub mod ability;
pub mod analyze;
pub mod command;
pub mod compile;
pub mod error_union;
pub mod eval;
pub mod graph;
mod graph_exec;
pub mod host;
pub mod kernel;
pub mod lower;
pub mod observe;
pub mod remote;
pub mod run_value;
pub mod strand;
pub mod type_check;

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
