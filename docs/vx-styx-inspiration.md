# Styx Inspiration for Vixen Syntax

This note records what Styx actually teaches us about syntax design, parser
architecture, and where Vixen may want to simplify its surface.

Relevant Styx references read for this note:

- `/Users/amos/bearcove/styx/README.md`
- `/Users/amos/bearcove/styx/docs/content/guide/primer.md`
- `/Users/amos/bearcove/styx/docs/content/reference/spec/parser.md`
- `/Users/amos/bearcove/styx/docs/grammar.ebnf`
- `/Users/amos/bearcove/styx/examples/showcase.styx`
- `/Users/amos/bearcove/styx/compliance/format.md`
- `/Users/amos/bearcove/styx/crates/README.md`
- `/Users/amos/bearcove/styx/crates/styx-cst/README.md`
- `/Users/amos/bearcove/styx/crates/styx-cst/src/parser.rs`
- `/Users/amos/bearcove/styx/crates/styx-cst/src/validation.rs`

## The Important Styx Moves

The strongest moves in Styx are not specific token choices. They are these:

- parse structure first, assign richer meaning later
- keep scalars as opaque text at parse time
- remove punctuation that duplicates obvious structure
- make CST concerns and semantic concerns separate
- let comments and formatting survive parsing cleanly
- parse a little more than the style guide blesses, then validate/style later

That is the "spirit" worth borrowing.

## What Styx Gets Rid Of

Styx aggressively avoids committing syntax too early:

- scalars are not typed at parse time
- quotes are only needed when content forces them
- key/value pairs do not need `:`
- sequences do not need commas
- documents are implicit objects
- a lot of syntax is inferred from position and delimiters rather than marked
  redundantly

This is appealing because the language stops asking the user to restate what is
already obvious from context.

## What Vixen Can Borrow

### 1. Delay meaning until later passes

The most important transferable idea is:

- the parser should commit to syntax, spans, comments, and structure
- the type checker / later semantic layers should decide what things mean

For Vixen this does **not** mean "everything is an untyped string literal". It
does mean:

- avoid baking too much semantic distinction into the parser
- do not force syntax to spell out information the type/context already knows

### 2. Separate parse permissiveness from style policy

One of the strongest concrete lessons in Styx is that parsing and style are not
the same concern.

For example:

- the parser accepts mixed separators in objects
- the CST preserves the exact separator choice
- validation warns about mixed separators
- formatting can normalize style later

That is a strong model for Vixen too:

- the parser should accept structurally unambiguous forms
- style rules should live in validation/formatting
- the CST should preserve what the user actually wrote

This matters if we experiment with optional commas, optional semicolons, or
shorter typed forms. We do not need to force every style preference into the
grammar itself.

### 3. Minimize punctuation where it is redundant

This is the biggest place to experiment.

Questions Styx pushes us to ask:

- do we really need commas in every multiline structure?
- do we really need semicolons in every obvious statement boundary?
- do we really need fully explicit `Type::Variant` spelling in every typed
  context?

That does **not** mean the answer must be "no". It means these should be
conscious decisions, not default cargo-culting from Rust.

### 4. Prefer one clear syntax per job

Styx is disciplined about syntax having a reason.

Vixen should aim for the same:

- `.` for fields and methods
- `::` for namespaces
- backticks for interpolation
- avoid extra punctuation forms that compete for the same role

### 5. Preserve source exactly in the front-end

The Styx split is very strong:

- tokenizer
- CST
- semantic/event parser
- formatter and LSP on CST

Vixen should borrow that architecture directly.

In particular, Styx makes a clean distinction between:

- lossless syntax preservation
- semantic validation
- semantic tree construction

That is a much better fit for Vixen than trying to make one parser data
structure serve every job at once.

## What Vixen Should Probably Not Copy

### 1. Fully opaque scalars everywhere

Styx can do this because it is a configuration language.

Vixen is code. It needs:

- identifiers
- operators
- literals
- control flow
- callable expressions

So Vixen probably should not go all the way to "everything is just an atom until
later". That would fight the language more than it would help.

### 2. Completely punctuation-light structure

For configuration, whitespace-driven structure is excellent.

For code with:

- `if`
- `match`
- `for`
- function calls
- nested expressions

too little punctuation can become muddy very quickly.

So the lesson is "strip redundant punctuation", not "strip punctuation
everywhere".

There is a specific version of this problem that matters a lot for Vixen:

- if `{}` is used for both blocks and object/record literals
- and commas/colons/semicolons are all weakened

then the language either needs meaningful whitespace, or another strong visual
disambiguator. Parser context alone is not enough; the surface has to stay easy
for humans to scan.

### 3. Styx's exact value model

Styx has tags, payloads, implicit unit, and schema-driven interpretation.

Those ideas are useful to study, but Vixen should not contort itself into the
same value model just because Styx has it.

## Concrete Vixen Experiments Worth Trying

These are the places where borrowing the Styx spirit might actually simplify the
language.

### Experiment A: commas optional in multiline forms

Instead of requiring commas everywhere, allow newline-delimited multiline forms:

```rust
Node {
    id: format::compile_node_id(krate.crate_name, krate.crate_type)
    kind: NodeKind::Action
    label: `compile ${krate.crate_name}`
}
```

while still allowing inline/comma-separated forms:

```rust
Node { id: x, kind: k, label: l }
```

This would be a direct "Styx spirit" experiment.

### Experiment B: semicolons only where needed

Current Vixen bias is Rust-style semicolons.

A Styx-inspired question is whether newline-separated statements should be the
default, with semicolons only when disambiguation is required.

That may be too far. But it is worth testing with real rule sketches rather than
deciding abstractly.

### Experiment C: lighter enum/value spelling in typed contexts

If context already says a field is `NodeKind`, maybe:

```rust
kind: Action
```

could be enough instead of:

```rust
kind: NodeKind::Action
```

This is potentially nice, but dangerous if it makes code too magical. It should
be tested against real examples before adoption.

### Experiment D: fewer explicit constructors when the field type is known

If a field already expects `NodeId`, can we sometimes write:

```rust
id: format::compile_node_id(...)
```

instead of:

```rust
id: NodeId { value: format::compile_node_id(...) }
```

Again: maybe yes, maybe no. But this is exactly the kind of redundant syntax
Styx encourages us to notice.

### Experiment E: relaxed multiline call and literal layout

Styx gets a lot of readability by letting vertical structure carry meaning.
For Vixen, that does not mean whitespace-only syntax, but it may mean allowing
cleaner multiline forms without mandatory punctuation at every line break.

For example, pressure-test whether this:

```rust
emit::node(Node {
    id: format::compile_node_id(krate.crate_name, krate.crate_type),
    kind: NodeKind::Action,
    label: `compile ${krate.crate_name}`,
    metadata: metadata,
});
```

really needs to stay that explicit, or whether a multiline style like this:

```rust
emit::node(Node {
    id: format::compile_node_id(krate.crate_name, krate.crate_type)
    kind: NodeKind::Action
    label: `compile ${krate.crate_name}`
    metadata: metadata
})
```

is still readable enough to be worth it.

This is exactly the kind of thing Styx encourages us to test empirically
instead of deciding from tradition.

## Where Styx Pushes Vixen Most

The biggest pressure Styx applies is not "be more like a config language".
It is this:

- every delimiter and separator should justify itself
- parsing should stay simpler than the eventual semantics
- syntax policy and style policy should not be conflated
- the language should exploit typed context where that removes noise without
  creating magic

That is the right lens for re-evaluating:

- semicolons
- commas
- constructor noise
- enum qualification noise
- how much structure must be spelled explicitly in multiline code

## Current Read

The best Styx-inspired principle for Vixen is:

- make the parser structurally rich but semantically humble

And the best syntax-design questions Styx forces are:

- which syntax in Vixen is carrying real information, and which syntax is just
  making the user restate what context already knows?
- which style constraints belong in the formatter/linter instead of the grammar?
- which syntactic distinctions need to exist in the CST, even if later passes
  erase them semantically?

That is the right pressure to apply now.

## Suggested Next Step

Do not immediately rewrite `vx-syntax-design.md` around Styx.

Instead:

1. keep the current Vixen syntax note as the conservative baseline
2. use this note to pick 2-3 concrete simplification experiments
3. rewrite a few real rule sketches in both styles
4. compare readability, ambiguity, and formatter burden before changing the
   baseline
