//! `vx-runtime` — the demand-driven Vixen runtime.
//!
//! A lazy **demand sea** with eager pure **kernel islands** (see
//! `docs/design/kernel-abi.md`). This crate owns the runtime value model, the
//! demand graph it evaluates, the kernel interpreter, and — later — the
//! copy-and-patch JIT. Per `i[impl.exec.shared-evaluator]` it is linked by
//! **both** the orchestrator and the executor, so a shipped closure evaluates
//! the same on either side.
//!
//! It **replaces** the eager evaluator (`vx-daemon`'s `runtime_eval` core, the
//! `vx-ir` back-end `KernelOp`/`lower`/`demand`, and the `vx-rt` crate). Those
//! are removed at the cutover phase, not before.
//!
//! **Status: Phase 0** of `docs/design/runtime-rebuild-plan.md` — the *spine*:
//! the value model ([`value`]) and (next) the demand-graph type. No engine yet.
//!
//! Before changing anything here, read `AGENTS.md` and
//! `docs/design/runtime-semantics.md`. The non-negotiables (no `Pending(String)`,
//! no flat ISA, pending-is-a-resolver, bomb-detonates-on-force) are load-bearing.

pub mod analyze;
pub mod eval;
pub mod graph;
pub mod kernel;
pub mod lower;
pub mod remote;
pub mod strand;
pub mod value;

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
