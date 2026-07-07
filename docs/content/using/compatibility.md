+++
title = "Will this work with my Cargo setup?"
weight = 2
+++

The question that decides whether you keep reading. The short answer: **vixen
reads your existing `Cargo.toml` files and changes nothing about them.** There
is no migration, no new manifest format, no `BUILD` files to write. Your
workspace stays a Cargo workspace; Cargo keeps working next to vixen the whole
time — falling back costs nothing.

What that means concretely:

**Manifests — read as-is.** Workspace members, dependencies (versions, paths,
workspace inheritance, features, optionals), lib/bin targets, editions. The
manifest-reading logic is itself verified against `cargo metadata` output.

**Resolution — Cargo-compatible, measured.** vixen's resolver selects the
same versions Cargo selects; this is enforced by differential tests against
real `cargo` on materialized workspaces. Current score and the named gaps
live on [Status](/status/) — as of today the known frontier is weak-feature
unification (`dep?/feature`), in progress.

**Build scripts and proc-macros — supported, hermetically.** Both work in the
verified paths (a build script writing `OUT_DIR` consumed by downstream
compilation; proc-macro crates compiled and loaded). The difference from
Cargo: they run *sandboxed*. A build script that reads undeclared files —
network calls, `~/.config` peeks, host introspection — will **fail loudly**
instead of succeeding unreproducibly. For messy crates, expect vixen to
surface what Cargo silently tolerates. That's the feature, but it's honest to
call it the main source of early friction.

**The registry — real crates.io.** Fetching uses the sparse index and
checksum-verified downloads; a pinned index snapshot makes builds
reproducible by default.

**What doesn't work:** the honest, dated list is
[Limitations](/using/limitations/) — headline items: real-workspace scale is
the current milestone ladder (fixture-scale is verified today), a deliberate
~3% tail of exotic Cargo behaviors will be documented as unsupported policy,
and Windows is deferred. If your setup leans on any of that,
[When not to use vixen](/using/when-not/) is the franker page.
