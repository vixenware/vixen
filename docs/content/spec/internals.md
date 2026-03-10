+++
title = "Internals Specification"
insert_anchor_links = "heading"
+++

# Vixen Internals Specification

This chapter is implementation-facing. User-visible type semantics and
surface APIs live in the language specification; internals only
specifies runtime representation strategies and boundary mechanics.

## Persistent data structures

> i[impl.persistent]
>
> The runtime represents compound values (arrays, maps, structs, trees)
> as persistent (structurally-shared) data structures, reference-counted
> at the node level.

### Array

> i[impl.array]
>
> An `Array` is backed by a balanced tree (e.g., RRB tree or similar).
> Cloning is O(1) — it increments a reference count on the root.
> Appending or replacing an element is O(log n) — it creates new nodes
> along the path from root to the modified position, sharing the rest.

### Map

> i[impl.map]
>
> A `Map` is backed by a hash-array mapped trie (HAMT) or similar
> persistent hash map. Same properties: O(1) clone, O(log n)
> insert/update.

### Struct

> i[impl.struct]
>
> A `struct` is a fixed-size product type. Cloning is shallow — each
> field is a reference-counted handle. Updating one field creates a new
> struct node with the new field and shared references to the others.

### Tree

> i[impl.tree]
>
> A `Tree` (store type) is already structurally shared by nature — it's a
> merkle tree in the content-addressable store. Cloning is O(1) (copy the
> root hash). Subtree operations are O(1) (slice the tree at a path).

### Scalars

> i[impl.scalar]
>
> Scalars (`U64`, `I64`, `F64`, `Bool`) are unboxed values. They are
> copied directly — no reference counting needed.

### Strings

> i[impl.string]
>
> Strings are reference-counted byte buffers. Small strings may be
> inlined (small-string optimization). String concatenation produces a
> new buffer.

### Closures

> i[impl.closure]
>
> Closures are a function pointer plus a tuple of captured values. The
> captured values are reference-counted handles, same as struct fields.
> Cloning a closure is shallow.

## Operational notes

> i[impl.persistent.ops]
>
> Operations like `append` on arrays and `with` on structs are O(log n),
> not O(n). They create a new path in the persistent structure, sharing
> the unchanged parts. This makes idiomatic Vixen code (chains of `with`
> updates, combinator pipelines) efficient by default.

## Executor-side evaluation

> i[impl.exec.shipped-closures]
>
> Observers and verifiers passed to `exec` are not host callbacks. The
> runtime lowers them into transportable evaluator payloads: code
> identity, lowered function body, and resolved captures.

> i[impl.exec.shared-evaluator]
>
> The orchestrator and executor share the same evaluator semantics. In a
> split-process implementation, that usually means both sides link the
> same evaluator crate. The executor reconstructs the shipped closure,
> binds it to executor-local values like `Process`, `Tree`, and `Host`,
> and evaluates it there.

> i[impl.exec.materialization]
>
> When executor-local `Blob` or `Tree` values cross back to the
> orchestrator, the runtime first commits them into the
> content-addressable store. The boundary carries store-backed content,
> not raw executor filesystem handles.

## Cycle-free by design

> i[impl.cycle-free]
>
> Because recursive types are disallowed, the value graph is always a
> DAG. Reference counting cannot leak — there are no cycles to break. No
> garbage collector is needed.
