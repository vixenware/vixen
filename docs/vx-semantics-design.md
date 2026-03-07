# Vixen Semantics Design

This document is the working source of truth for Vixen's new semantic
direction.

It supersedes the earlier planner-centric language notes. Vixen is no longer
centered on authoring an explicit build graph in source. The semantic center is
now a typed, tracked, demand-driven runtime.

## Status

This note is intentionally being written incrementally during design
discussion. It records decisions as they are made and will grow section by
section as the model sharpens.

## Core Model

Vixen source defines:

- types
- values
- functions
- modules

The primary authoring model is **typed tracked values**.

A value such as `RustLibrary`, `Executable`, or `OciImage` is:

- a typed value in the source language
- a latent computation in the runtime
- something that can become a realized product when demanded externally

These are not separate source-level categories.

## Runtime Architecture

The language semantics and runtime architecture are co-designed.

Current architecture:

- an orchestrator evaluates Vixen near the source tree and decides what work is
  needed
- a content-addressed store is the source of truth for artifacts, toolchains,
  and intermediate values
- executors run sandboxed `exec` computations, potentially far from the source
  tree or store
- a virtual filesystem mounts store-backed content into sandboxes on demand

The intended data flow is:

1. the orchestrator evaluates Vixen definitions for a demanded value
2. required tracked computations are discovered through value flow
3. `exec` work is dispatched to an executor when needed
4. executor sandboxes mount inputs from the store through the VFS
5. declared outputs are captured back into the store
6. downstream computations consume those outputs by value, not by ad hoc path
   conventions

This architecture is why several surface decisions are load-bearing:

- file values are pathless content values
- trees are first-class tracked values
- `exec` takes structured inputs and declared structured outputs
- `write_file` creates store-backed output values, not immediate disk effects

## Demand

Demand is entirely external to the language.

Vixen source has:

- no `force`
- no root declaration
- no default target
- no side-effecting top-level statements

A Vixen file is a module. It exports values, functions, and types. The CLI or
other host/runtime entry point chooses what to demand.

Examples of demand live outside the language:

- "build `my_binary`"
- "realize `app_image`"
- "run `unit_tests`"

The runtime resolves the requested top-level name, traces its dependency
subgraph, and realizes exactly what is needed.

## Discoverability

There is no `pub`/`private` visibility system in v0.

Discoverability should come from the shape of top-level definitions, not from
an explicit export marker.

Current direction:

- top-level concrete bindings are the natural namespace of demandable names
- functions with parameters are helpers, not direct demand roots
- tooling should further distinguish ordinary values from realizable build
  products by **type**, not just arity

That means a top-level `Executable`, `RustLibrary`, `OciImage`, or
`TestResult` is naturally demandable, while a top-level `String` or
`[String]` is not the primary CLI surface even if it is inspectable.

## Surface Type Model

The language surface does **not** expose a type-level distinction between pure
values and tracked host values.

Authors write:

- `RustLibrary`
- `Executable`
- `OciImage`
- `String`
- `[T]`

They do **not** write things like:

- `Tracked<RustLibrary>`
- `Future<Executable>`
- `HostValue<T>`

The runtime internally knows whether a value arises from pure computation or a
tracked host computation. That distinction is runtime metadata, not a
user-facing type.

This avoids a viral coloring problem in the type system.

## Host Primitive Boundary

The primitive builtin set should stay small and focused on operations that
cannot or should not be expressed in Vixen source itself.

Current primitive direction:

- `exec`
- `acquire`
- `download`
- file access primitives
- format parsing/emission primitives
- hashing primitives
- `write_file` for directly producing output content

Everything else should, as much as practical, be written in Vixen in the
standard library:

- Rust compilation rules
- C/C++ compilation rules
- toolchain acquisition
- OCI assembly

This keeps the interesting build logic visible, typed, and editable in Vixen
rather than burying it in host code.

`acquire` is not just "download, then unpack later".

Current direction:

- `acquire` is a single tracked primitive for archive acquisition
- it downloads, verifies, and streams extracted contents into the store as one
  operation
- the hash/checksum is mandatory
- unverified acquisition has no place in a hermetic build system
- its primitive result is `Stream<TreeEntry>`, not a fully collected `Tree`

`download` remains for single-file fetching where archive extraction is not the
goal.

`write_file` should not be read as "mutate the local filesystem now".

Current direction:

- `write_file` produces a tracked file output value
- that value can be passed to later computations, including `exec`
- realization does not imply immediate local-disk materialization
- the runtime may keep it in memory, in a virtual filesystem, in local CAS, or
  in remote storage as needed
- final materialization is a runtime concern driven by demand, not a direct
  source-level side effect

`exec` is a single primitive, not a family of special builtins like `compile`
or `link`.

It should be structured, not shell-shaped:

- argv-based
- explicit environment
- explicit structured inputs
- explicit structured outputs

Nonzero exit is a failure, not a return value.

Captured stdout, when requested, is `Bytes`. Text decoding is a separate
tracked step and may fail.

Stderr belongs to runtime diagnostics, not the typed success value.

Timings and similar process metadata are runtime telemetry, not language-level
results.

Higher-level build operations should be written as standard-library Vixen code
over `exec` and the other primitives.

## File and Tree Values

Pathless file content values are the primitive unit of file-like data.

Current direction:

- `write_file(content)` produces a pathless file content value
- content identity is content-addressed, not path-addressed
- the same content may be placed at multiple paths without becoming a distinct
  primitive value

Placement is contextual:

- an `exec` input mapping
- a tree entry
- final materialization for a demanded output

Trees are also first-class typed values.

A tree is **not** just `Map<Path, FileBlob>`.

A tree carries structural semantics the runtime must track directly:

- membership
- placement
- subtree identity
- merge/filter/subtree operations
- use as a natural sandbox input/output unit

The runtime should therefore track tree identity and tree-level invalidation as
first-class concerns, not as incidental map behavior.

## Lists and Streams

Vixen needs both `List<T>` and `Stream<T>`.

They are both first-class typed values, but they are not the same thing.

Current direction:

- `List<T>` has a known spine immediately
- `Stream<T>` produces elements progressively from a source
- both may contain unrealized tracked values as elements
- bindings name these values as graph nodes, not eagerly realized products

`Stream<T>` should be reusable, not linear.

That means:

- a stream may have multiple downstream consumers
- the producer runs once
- produced prefix/elements are cached by the runtime
- the language does not expose a mutable cursor or one-shot consumption model

`Stream<T>` is unordered as a language concept.

A producer may happen to discover or emit elements in some runtime order, but
that order is not preserved, exposed, or relied on by the language semantics.

Stream operations such as `map` and `filter` should preserve the progressive
model where possible.

Stream elements should be typed values, not anonymous tuples.

For archive acquisition, the element shape is currently understood as something
like `TreeEntry`, carrying at least:

- `path`
- `content`
- `kind`

where `kind` distinguishes file-like and directory-like entries.

Current direction:

- stream elements arrive progressively
- transformed elements do not carry a language-level source-order contract
- runtime scheduling should remain free to surface completed work as it becomes
  available
- determinism should come from collection points and output structure, not from
  producer order or completion order

This means collection points are responsible for imposing deterministic
ordering when they produce ordered results.

Some downstream operations will instead demand a complete collection and
therefore collapse a stream into a fully known result.

Examples include:

- linking over the full set of object files
- counting all elements
- collecting archive entries into a `Tree`

Examples of deterministic collection behavior:

- `collect_tree()` canonicalizes by path and tree structure
- `collect_list()` requires an explicit ordering policy

There should be no implicit default for list collection.

If a caller wants lexicographic order by some key, that intent must be
explicit. There is no "use producer order" option and no "use completion
order" option in the language semantics.

Because streams are unordered, positional operations like `zip` do not belong
on `Stream<T>`.

`zip` only makes sense on ordered collections such as `List<T>`.

If a program needs to correlate progressive elements from multiple streams, it
should do so by key or after explicit collection into an ordered form.

For the same reason, `fold`/`reduce` should not exist on `Stream<T>` in v0.

A fold over a stream is a collection boundary in disguise: it forces the full
stream and produces one accumulated value.

In v0, that boundary should stay explicit:

- collect first
- then fold the resulting `List<T>`

Keyed stream operations such as `group_by` or `join` are also deferred.

They may become important later, but they should not be added until real build
rules force them.

## File and Tree Observations

Path construction is pure.

Examples:

- `workspace / "src" / "lib.rs"`
- `toolchain_root / "bin" / "rustc"`

These expressions build location values only. They do not observe the outside
world yet.

Tracked observations begin when the program asks the runtime to inspect a tree
or file.

Current semantic split:

- exact-path observation:
  - `read_text(path)`
  - `read_bytes(path)`
  - `exists(path)`
  - `stat(path)`
- structural observation:
  - `children(dir)`
  - `glob(root, pattern)`

Important consequences:

- reading a file depends on that file's observed content or metadata
- listing a directory depends on membership, not just content of existing files
- `glob` is a first-class primitive because it must work over abstract trees,
  including non-local or non-filesystem-backed roots
- structural observations should return deterministic, stably ordered results

In terms of collection shape:

- exact-path observations return ordinary values
- structural observations such as `children(...)` and `glob(...)` return
  `Stream<TreeEntry>`

The same distinction applies outside the local filesystem:

- roots may come from the workspace
- fetched artifacts may unpack into trees
- generated results may become trees
- remote or store-backed trees should still support the same observation model

This is why `glob` is a primitive rather than just library code over
`children()`.

## `exec` Inputs and Outputs

`exec` should accept both:

- whole trees
- individually placed files

The runtime may normalize these internally, but the surface should support the
natural shape of the task.

Examples:

- compile with a source tree plus a generated config file
- link with a set of individual object files
- assemble an image from a tree of assets

`exec` outputs should be declared structurally.

That means the caller declares whether a named output is:

- a file at a relative path
- a tree rooted at a relative path

The success result of `exec` is then a typed record derived from that declared
output schema, not an untyped bag of strings.

This gives the runtime a real contract:

- output shape mismatches are errors
- outputs can be captured back into the store with the right structure
- downstream computations receive typed file/tree values directly

## Value-Level Invalidation

Invalidation should propagate by value, not only by file hash.

Example shape:

1. a file read reruns because file bytes changed
2. parsing reruns because its input changed
3. if the parsed typed value is unchanged, invalidation stops there

This is a core reason to keep file observation and format parsing as separate
tracked computation boundaries.

## Recursion

Recursion is allowed.

There is currently no planned `let rec`, structural-decrease checker, or other
syntactic restriction on recursion in source.

Instead:

- the runtime memoizes calls
- recursive evaluation over finite DAG-like domains is expected and natural
- cycles are detected at runtime in the realized computation graph

Cycle errors should be surfaced as ordinary diagnostics with a useful cycle
trace.

## Functions and Effects

There is currently no separate declaration marker for "pure functions" versus
"host-touching functions".

Any function may call host primitives.

This is intentionally **not** treated like `throws`.

Reasoning:

- host-touching markers would create a viral coloring problem
- the runtime already knows when a host primitive is invoked
- purity is discovered by evaluation and dependency recording
- callers should not have to care whether a returned `RustLibrary` came from
  pure computation, cached work, or a newly realized host computation

Errors are different because they affect control flow at the call site.
Host-tracked computation does not need to color the surface in the same way.

## Failure Semantics

Failures are part of tracked evaluation and need deterministic runtime
semantics.

### Primary failure

When multiple independent computations fail while realizing one demand root,
the runtime should expose one primary failure without waiting for slower
siblings to also fail.

Current direction:

- reporting is fail-fast
- the runtime does not delay reporting just to preserve source-order priority
- if multiple failures are already available at the moment reporting happens,
  tie-breaking should still be deterministic

Stable source/dependency order is therefore a tie-breaker among failures
already observed, not a reason to block on unfinished work.

### Additional failures

The runtime may also surface additional failures discovered while other
independent work was already in flight.

That means:

- build-oriented UX can behave fail-fast in presentation
- test-oriented UX can aggregate more failures
- the semantic contract remains deterministic because tie-breaking among
  already-observed failures is stable

The language semantics do not need to require waiting for every independent
branch to either succeed or fail before reporting.

### Failure caching

Failure caching is primitive-specific, not a single global rule.

Current direction:

- hermetic, input-determined failures are cacheable
- transient failures are not necessarily cacheable

Examples of cacheable failures:

- parse failures from fixed bytes
- validation failures from fixed typed values
- `exec` failures with hermetic inputs, toolchain, and sandbox

Examples of non-final or retryable failures:

- network failure from `download`

This means the runtime should not treat "failure" as one undifferentiated
thing for memoization. Builtins need explicit retry/cache semantics where it
matters.
