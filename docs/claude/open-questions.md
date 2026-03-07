# Open Questions

Design decisions that are not yet settled.

## Named struct fields vs stream filtering for exec results — RESOLVED

**Resolution**: both. exec returns `Stream<E>`. The caller constructs a
regular struct whose fields are lazy projections over that stream.

```
fn build_crate(src: Tree, tc: Toolchain) -> BuiltCrate = {
  let events = rustc_events(tc, src, externs, cargo)

  {
    name        = cargo.package.name,
    rmeta       = events.find($ as RMeta).file,
    rlib        = events.find($ as RLib).file,
    diagnostics = events.filter($ as Diagnostic).map($.diag),
  }
}
```

The struct is ordinary. Its fields are demand-driven computations. No promises,
no progressive structs, no new language primitive. `built.rmeta` blocks until
the `find` completes, which blocks until the monitor yields `RMeta`.

This gives both ergonomic access (`built.rmeta`) and stream flexibility (the
underlying stream is still available for custom projections). No mapping from
events to fields is needed — the user writes the mapping as regular code.

## mut keyword

Do we need an explicit `mut` keyword for rebindable variables?

Arguments for: makes intent clear, catches accidental rebinding.
Arguments against: Vixen doesn't have Rust's ownership model, so `mut` is just
ceremony. Every rebinding is just a new binding anyway.

Currently: leaning toward no `mut`. Rebinding is always allowed.

## Promise<T> in the language

We explicitly decided against `Promise<T>` as a language-level type. The
reasoning:

- Promise has reference semantics (shared handle). Everything else has value
  semantics. Having both creates a Java-like split.
- "Only the producer can resolve" requires capability discipline that's hard to
  enforce.
- Making promises general (user-constructible) turns Vixen into a concurrent
  language, which is a much bigger commitment than intended.

Instead, progressive availability comes from streams. `exec` with a monitor
yields events as they become available. Consumers block on `find` until the
element arrives. No promises needed.

The one place this was tempting: exec results where `rmeta` and `rlib` are
separate fields that resolve at different times. Streams solve this: `rmeta`
is a stream event that arrives before `rlib`. See "Named struct fields vs
stream filtering" above.

## Stream ordering

Streams are ordered (deterministic element order for a given input). This is a
departure from the earlier design which said streams were unordered.

The change was motivated by concrete examples where ordering matters:
- Archive extraction: entries have a deterministic order
- Rustc JSON output: events arrive in a deterministic order
- Test discovery: tests are discovered in a deterministic order

Ordering is part of the stream's identity. Two streams with the same elements
in different orders are different streams.

This does NOT mean consumers should rely on ordering for correctness. `find`
and `filter` are still the right way to extract specific elements. But the
runtime can and does guarantee deterministic replay.

## Streams: what's in v0

Streams are in v0. They are not a future feature.

The core stream operations for v0:
- `find`, `filter`, `map` — progressive, don't require completion
- `collect_tree()`, `collect_list(order: ...)` — blocking, require completion
- Reusability (multiple consumers, single producer, cached elements)

Not in v0:
- `zip`, `fold`, `reduce`, `group_by`, `join`
- If needed, collect first, then operate on the list

## Function identity stability

How exactly are function identities computed? The function body gets a semantic
hash, but:

- Does reformatting the source change the hash? (Probably not — hash the
  AST/HIR, not the text.)
- How are closures identified? (Deferred — memoize named functions only in v0.)
- How do standard library updates propagate? (Stdlib functions have their own
  hashes. Changing a stdlib function invalidates all callers.)

This is likely the biggest implementation risk. If function identity is
unstable, the cache is useless.

## Value equality for cached types

Early cutoff requires semantic equality. Which types have it?

- Blobs: BLAKE3 hash equality. Trivial.
- Trees: merkle root equality. Trivial.
- Strings, ints, bools: structural equality. Trivial.
- Records: fieldwise structural equality.
- Streams: sequence of element identities.
- User-defined types: structural equality by default?

The risk is types that don't have a meaningful equality. If a type can't be
compared, it can't participate in early cutoff, which makes it a bad cache
boundary.

## exec without a monitor

If `exec` has no monitor, what does it return? Options:

1. A stream with a single completion event containing the declared outputs.
2. A record of declared outputs (blocking until process exits).

Currently: option 1 (for consistency — exec always returns a stream). But
option 2 might be more ergonomic for simple cases where you don't need
pipelining.

## Diagnostics aggregation

Diagnostics are stream events. But how does the CLI collect and display them?

- Should there be a built-in concept of "diagnostic" that the CLI knows how
  to render?
- Or is rendering entirely user-defined (the CLI just shows whatever events
  the user's code surfaces)?
- How are diagnostics from cached builds replayed? (They're stored with the
  stream elements, so replaying the stream replays the diagnostics.)

## Store sharing across machines

Content-addressed storage is designed for sharing. But the protocol for
sharing (push vs pull, authentication, trust boundaries) is not yet designed.

## How does the monitor know when a file is "ready"?

The monitor runs on the executor and parses process stdout. When it sees
"rmeta is ready," it calls `store.commit(path)` and yields the event.

But what does `store.commit(path)` actually do? It needs to:
1. Read the file from the executor's filesystem
2. Compute its BLAKE3 hash
3. Store it in the executor's local CAS
4. Make it available for transfer to the orchestrator/shared store

The file must be complete on disk before `store.commit` reads it. The monitor
relies on the structured output (rustc's JSON) to know when the file is ready,
not on filesystem events.
