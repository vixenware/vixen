# Query & projection surface (draft)

Draft design material, **not normative spec**. It came out of working the rustc
`exec` observer — filtering a message stream, projecting fields, reading the
write-set — and trying to make that read left-to-right and DRY. Several pieces
here are settled in discussion; several are open. The one piece already promoted
to the spec is the `?` fallible-variant suffix (`r[error.option]`); everything
else waits here until it survives more stress-testing.

## The core rule: `.` vs `[ ]`

- **`.name`** — `name` is a member the value's *own type declares* (a struct
  field), statically known. `point.x`, `artifact.emit`, `proc.exit_code` (a
  field, possibly pending — `exit_code` is unavailable until exit, still a field
  not a method).
- **`[selector]`** — a *computed selection from a container*. The left operand is
  always the container; the selector's kind decides the operation.

`[ ]` is **not symmetric**: `tree[path]` (tree selects the entry at path) is
valid; `path[tree]` ("path selects by tree") is ill-typed and does not exist.

## `[ ]` selector dispatch (unproven — the thing most in need of review)

| selector | on a collection | on a scalar |
|---|---|---|
| `Int` / key | element / lookup | element / lookup |
| `UpperType` | filter + narrow (`xs[Artifact]`) | — |
| `UpperType where pred` | filter (`xs[Artifact where emit == "metadata"]`) | — |
| function value | **map** (`lines[from_json]`, `opt[tree]`) | ill-typed — use `\|>` |
| `.field` | lifted field projection (`xs[.artifact]`) | (just `.field`) |

The case convention (`UpperCamel` = type, lowercase = value) makes type-vs-value
selectors lexically distinct. A function selector means *apply*: over a
collection (incl. `Option`, a 0-or-1 functor) it maps; over a scalar it's
ill-typed, and free-function application to a scalar uses a separate operator
(`x |> f`, character negotiable) — a genuinely different operation, hence
different syntax.

Whether this whole grid stays unambiguous and readable is the open question.

## `?` — fallible-variant suffix (in the spec)

`f` poisons (assertive); `f?` is the auto-derived `Option`-returning variant
(recoverable). `tree.file` / `tree.file?`, `.only` / `.only?`, `parse_u64` /
`parse_u64?`, `as` / `as?`. Not a blanket expression poison-catch. See
`r[error.option]`.

## Implicit `$.` inside bracket predicates

Inside `[Type where (…)]` the candidate type is named, so a bare field reads as
`$.field` (`[Artifact where emit == "metadata"]`). This is the scoped comeback
of the implicit-`$.` we removed from *bare* `Type where (…)`; it's safe here
because the type is explicit in the bracket.

## Stream → Array is an implicit coercion

No `.to_array()`. A `Stream[T]` used where `Array[T]` is wanted (a return type, a
field type) coerces — and that coercion is the deterministic collection point
(sorts by the universal order).

## `from_json` and type-associated functions

`Type.from_json` is an **auto-derived universal** — the inverse of the universal
*serialise* property, built from the type's shape plus its `#[rename]` / `#[tag]`
annotations. Nobody writes it, the same way nobody writes `hash` / `eq` /
`serialise` / `to_string`. Type-associated functions a user *does* write (a
custom constructor, `SemVer`'s `^=`, `MacToolchain.probe`) go in the
**`namespace Type (…)`** block already in the language. There are **no extension
traits** — `Type.foo` is either a derived universal or a `namespace` declaration;
nothing bolts a method onto a foreign type.

## Observers return independently-timed records

The load-bearing shape: an observer returns a **record whose fields each resolve
at their own time** — never a sum decided at exit. Branching the whole result on
`proc.exit_code` would force a join at exit and destroy early availability
(pipelining). `rmeta` must resolve off the message stream alone — `Some` the
instant rustc emits the metadata artifact, `None` only when stderr closes without
it — independent of exit.

## Worked example

```vixen
#[tag = "$message_type"]
enum RustcMessage {
    #[rename = "artifact"]   struct Artifact   { artifact: Path, emit: String }
    #[rename = "diagnostic"] struct Diagnostic { message: String, level: String, rendered: Option[String] }
}

struct Unit { rmeta: Option[Blob], rlib: Option[Blob], diagnostics: Array[Diagnostic], exit: U64 }

fn build(proc: Process, tree: Tree) -> Unit (
    let msgs = proc.stderr.lines()[RustcMessage.from_json]               // Stream[RustcMessage]
    Unit {
        rmeta:       msgs[Artifact where emit == "metadata"][.artifact].only?[tree],  // Option[Blob], early
        rlib:        msgs[Artifact where emit == "link"][.artifact].only?[tree],      // Option[Blob], late
        diagnostics: msgs[Diagnostic],                                                // Array[Diagnostic]
        exit:        proc.exit_code,                                                  // U64
    }
)
```

Step types for the `rmeta` line: `Stream[RustcMessage]` →`[Artifact where …]`→
`Stream[Artifact]` →`[.artifact]`→ `Stream[Path]` →`.only?`→ `Option[Path]`
→`[tree]`→ `Option[Blob]`. The final `[tree]` reads left-to-right because
`.only?` yields an `Option` (a collection) and `[tree]` maps the callable `tree`
over it — the tree-read that was awkward for a bare scalar `Path` dissolves once
the recoverable form makes it an `Option`.

## Open questions for the review

- The `[ ]` selector-dispatch grid (above) — prove every selector-kind ×
  subject-kind combination is unambiguous and reads well.
- `[fn]` map-on-collection vs ill-typed-on-scalar, and the scalar free-function
  apply operator (`|>`? other?).
- Does `.field` ever auto-lift over a collection, or is `[.field]` mandatory for
  lifted projection? (Currently: hold the line, `[.field]`.)
- **Shared / multicast stream consumption** — `msgs` is consumed by several
  downstream filters, so the underlying stderr must be read once and fanned. Real
  runtime requirement.
- Juxtaposition-as-application weirdness budget (`g()` = apply-to-unit) — flagged,
  unresolved; interacts with how much `[ ]` can carry.
- Flow-narrowing through `[Type]` and `where ($ is T)`.
