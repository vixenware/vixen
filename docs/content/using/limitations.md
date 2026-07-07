+++
title = "Current limitations"
weight = 7
+++

This page is load-bearing: early access only works if you know what you're
early to. Everything here is stated as of its date; the
[Status](/status/) page moves faster.

**Scale honesty.** The end-to-end pipeline — manifest → resolve → fetch →
extract → build with real rustc → verified against Cargo — runs at *fixture
scale*: real crates, real compilers, small graphs. The ladder to real
workspaces (a trivial crate, then one with a build script, then one with
proc-macros and a real dependency tree) is the current milestone sequence.
What "works" means here is strict — differential verification against Cargo's
own unit graph — but the graphs are small.

**Resolver coverage.** rodin matches Cargo on the differential fixture suite
except weak-feature unification (`dep?/feature`), which is the known frontier
and next milestone. A deliberate long tail of Cargo behaviors (the exotic ~3%)
will be documented as unsupported policy rather than chased — that list gets
written as it's decided, here.

**Cargo's ecosystem is big.** Build scripts and proc-macros work in the tested
paths; the wilderness of what build scripts *do* (network access, host
introspection, writing outside OUT_DIR) is exactly what hermetic execution
exists to surface — expect early builds of messy crates to *fail loudly* where
Cargo silently tolerates, and expect that to be the feature it is.

**Sandbox coverage.** Confinement (undeclared reads fail) is real on macOS
today, Linux next; full *observation* of reads through the virtual filesystem
is staged behind it. Until observation lands, read-sets for external processes
are declared-and-enforced rather than observed — sound, but coarser than the
end state. Windows: deferred, honestly and indefinitely-for-now.

**Distribution.** Content addressing makes multi-machine builds and shared
caches structurally straightforward, and the service architecture is built
for them — but early access is single-machine first. The remote story ships
when the local story is boring.

**The language.** vix is young. It gains capabilities weekly (this week:
real loops, in-place updates, full semver); its standard library is small;
some ergonomics are rough (the honest list lives with the language docs).
It is, however, already load-bearing: everything above runs through it.
