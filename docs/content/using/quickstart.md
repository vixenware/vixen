+++
title = "Try it on your repo"
weight = 1
+++

> **Early access:** exact commands ship with your access wave. The flow below
> is what those commands do — so the first run is recognition, not surprise.

The trial is deliberately zero-risk: **vixen reads your Cargo workspace and
writes nothing into it.** No files change, no `Cargo.toml` edits, no lockfile
rewrites. Cargo continues to work; your fallback is "stop typing vixen."

**1. Point it at your workspace.** vixen parses your manifests (verified
against `cargo metadata`), resolves versions (verified against Cargo), and
plans the build. If something in your setup isn't supported yet, this is
where it says so — by name, before any work happens. See
[Compatibility](/using/compatibility/).

**2. First build.** Real `rustc`, hermetically: each invocation sees exactly
the declared inputs. Two things may happen that Cargo never does: a messy
build script *fails loudly* for reading something undeclared (the fix is
declaring it — annoying once, sound forever), and everything the build
produces lands in a content-addressed store with a
[receipt](/using/receipts/).

**3. The interesting part — run it again.** Warm second build: near-instant,
and the receipt shows *why* — every step a cache hit, none of it trust-based.

**4. Now edit something.** Change a comment: rebuild does nothing, provably.
Change a function body: watch the wave stop at the first unchanged interface.
Run the tests: only the ones your change can reach execute — the rest are
skipped *with the reasoning attached*.

Step 4 is the product. Steps 1–3 exist so step 4 is trustworthy.

When something surprises you — a rebuild you didn't expect, a loud sandbox
failure — the debugging move is always the same:
[read the receipt](/using/receipts/). And when you hit the edge of what early
access supports, [Limitations](/using/limitations/) should already name it;
if it doesn't, that's a bug report we want.
