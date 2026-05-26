+++
title = "Does cargo know enough to pre-materialise rustc's read set?"

[extra]
type = "Question"
status = "open"
created = "2026-05-26"
tags = ["ccc", "cargo", "rustc", "fork-scope"]
+++

[[Subgraph elision]] requires that when CCC restores an intermediate artifact from the cache (an `.rlib`/`.rmeta`/etc.), every downstream rustc invocation that depends on it can find what it needs *somewhere*. Two paths:

1. **Cargo pre-materialises** the cached intermediate to the on-disk location an unmodified rustc would have read it from. CCC stays a cargo-only fork.
2. **Rustc itself reads from the cache.** Requires forking rustc too — earlier than we planned.

The deciding question is whether cargo, *today*, has full visibility into the file set rustc will read for a given invocation. If cargo knows every `.rlib`, every `.rmeta`, every dep-info file, every codegen-backend artifact, every proc-macro `.so/.dylib/.dll` that rustc will open — then (1) works. If cargo only knows the immediate `--extern` set and rustc resolves the rest internally via sysroot probes, sibling-file conventions, etc., then we hit (2).

## Disposition

Forking rustc is on the eventual Vixen roadmap regardless — it unlocks several things CCC alone can't do (custom dep-graph emission, finer-grained cache key derivation, possibly distributed remote-exec hooks). The question is whether it has to happen *now*, as part of Tier 1, or whether Tier 1 ships with cargo-only and rustc-fork lands later.

## What unblocks resolution

A code survey of cargo's rustc-invocation code path (`cargo::core::compiler::CompileMode`, the `RUSTC_WRAPPER` plumbing, dep-info handling) plus a side-by-side with rustc's input-file resolution. Concretely: enumerate everything rustc opens for a non-trivial invocation (with `strace`/`dtruss`/`ETW`), then check which of those paths cargo knows about explicitly.

## Related

- [[Subgraph elision]]
- [[CCC is a cargo fork, not a rustc wrapper]]
- [[Tier 1 — CCC]]
