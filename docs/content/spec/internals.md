+++
title = "Internals Specification"
insert_anchor_links = "heading"
+++

# Vixen Internals Specification

How Vixen implements value semantics without copying everything.

## Persistent data structures

> r[impl.persistent]
>
> The runtime represents compound values (arrays, maps, structs, trees)
> as persistent (structurally-shared) data structures, reference-counted
> at the node level.

### Array

> r[impl.array]
>
> An `Array` is backed by a balanced tree (e.g., RRB tree or similar).
> Cloning is O(1) — it increments a reference count on the root.
> Appending or replacing an element is O(log n) — it creates new nodes
> along the path from root to the modified position, sharing the rest.

### Map

> r[impl.map]
>
> A `Map` is backed by a hash-array mapped trie (HAMT) or similar
> persistent hash map. Same properties: O(1) clone, O(log n)
> insert/update.

### Struct

> r[impl.struct]
>
> A `struct` is a fixed-size product type. Cloning is shallow — each
> field is a reference-counted handle. Updating one field creates a new
> struct node with the new field and shared references to the others.

### Tree

> r[impl.tree]
>
> A `Tree` (store type) is already structurally shared by nature — it's a
> merkle tree in the content-addressable store. Cloning is O(1) (copy the
> root hash). Subtree operations are O(1) (slice the tree at a path).

### Scalars

> r[impl.scalar]
>
> Scalars (`U64`, `I64`, `F64`, `Bool`) are unboxed values. They are
> copied directly — no reference counting needed.

### Strings

> r[impl.string]
>
> Strings are reference-counted byte buffers. Small strings may be
> inlined (small-string optimization). String concatenation produces a
> new buffer.

### Closures

> r[impl.closure]
>
> Closures are a function pointer plus a tuple of captured values. The
> captured values are reference-counted handles, same as struct fields.
> Cloning a closure is shallow.

## Rebinding performance

> r[impl.rebind-perf]
>
> `items.append!("new")` is O(log n), not O(n). It creates a new path in
> the persistent array, sharing the unchanged parts. The rebinding
> replaces the root reference. This makes idiomatic Vixen code (chains of
> `!` calls) efficient by default.

## Cycle-free by design

> r[impl.cycle-free]
>
> Because recursive types are disallowed, the value graph is always a
> DAG. Reference counting cannot leak — there are no cycles to break. No
> garbage collector is needed.
