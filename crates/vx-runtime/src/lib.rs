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

pub mod value;
