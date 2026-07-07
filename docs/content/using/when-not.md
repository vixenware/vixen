+++
title = "When not to use vixen"
weight = 8
+++

A tool that only tells you where it wins isn't credible. Today, do **not**
reach for vixen if:

**You need Windows.** Deferred, deliberately and indefinitely-for-now — the
filesystem-virtualization story there needs its own design, and we'd rather
ship two platforms honestly. (macOS and Linux are the supported pair.)

**You need multi-machine builds or shared remote caches *today*.** The
architecture is built for them — content addressing makes distribution
arithmetic rather than engineering — but early access is single-machine
first, on purpose. If remote execution is your blocking requirement this
quarter, Bazel/Buck2 remain the grown-ups.

**Your build leans on the exotic tail of Cargo.** A deliberate ~3% of Cargo's
behavior surface won't be chased; it gets documented as unsupported policy as
we hit it. If you're deep into unusual registry setups, exotic target
configurations, or build scripts that fundamentally depend on reading the
undeclared world, expect friction — loud, explained friction, but friction.

**You need battle-years.** Cargo has them; Bazel has them; vixen has
measured correctness on a growing fixture ladder and a public gap list.
If your risk budget is zero, wait a few waves — the
[status page](/status/) is dated precisely so you can watch it mature from a
distance.

**Your project isn't Rust-shaped.** General-purpose is the trajectory
(the machinery doesn't care what it builds); Rust-with-Cargo is what's real
today.

None of these are forever. All of them are true today, which is the only day
this page is about.
