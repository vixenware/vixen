+++
title = "Values and schemas"
+++

Every vix value is **bytes described by a schema**. There is no universal
"Value" object that everything boxes into — a design refusal that matters:
god-objects make every operation a switch over every possible shape, and they
make "what is this?" a runtime question. In vix, what a value *is* is static;
where its bytes live is the machine's business.

The shapes:

- **Primitives** — `Int`, `Float`, `Bool`, `String`, and domain primitives like
  `Path`. Versions are a struct in the standard library, not a primitive —
  see [std](@/vix/std.md).
- **Structs** — named fields, declared in vix:

  ```
  pub struct Version {
      major: Int,
      minor: Int,
      patch: Int,
      prerelease: Option<String>,
      build: Option<String>,
  }
  ```

- **Enums** — variants with payloads, matched with patterns.
- **Containers** — `[T]` arrays, `Map<K, V>`, `Option<T>`, tuples. Functional
  update methods (`push`, `set`, `insert`, `pop`) return the new value — and
  run in place when that's invisible; see [molten](@/vix/molten.md).
- **Machine values** — `Tree` (a directory-shaped artifact, e.g. an extracted
  archive or a build's outputs), `Blob` (raw bytes), `Doc` (dynamic structured
  data from parsers like `toml()`/`json()`, navigated by field access and
  coerced into typed values at the boundary).

Two properties come with every value, without opt-in:

**Content identity.** Any value can be hashed by what it contains, which is
what makes it memoizable, storable, and comparable across processes. You never
implement hashing, equality, or serialization for cache purposes — being a vix
value *is* the implementation.

**Schema-checked construction.** Values are built through their schema —
there's no reinterpreting bytes. The checker verifies field access and
pattern-matching against declarations; `Doc` (deliberately dynamic) is the
one escape hatch, and coercing a `Doc` into a typed value is an explicit,
checkable step.
