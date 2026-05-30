# Package resolution (compiler-side)

## What this is, and what it is not

Vixen's package *distribution* model is designed in
[Packages and publishing](/packages-and-publishing/): scoped names (`@scope/name`),
OIDC publishing, an index mapping `(name, version) → Store handle`, PubGrub
version solving, lockfiles. This document designs the **other half**: how the
**compiler** turns an `import "…"` in a module it is compiling into the imported
package's *source*, so the package's exports become usable — name-resolved,
type-checked, and (for grammars) readable as data.

Today the compiler bundles only stdlib and treats `import` as a no-op
(`compile.rs`: user source is `SourceId(0)`, `stdlib_sources()` get `1..`,
`build_program(&sources)`; `resolve.rs:172` ignores `Import`; `ImportExpr`'s
`resolved_source` is never filled). So `import "@vixen/grammar-cc"` resolves to
nothing, and a `@cc[…]` CLI literal can't be checked because the `Grammar` value
behind `cc` is unreachable. **This is the wall, and it is the next task.**

It is deliberately **orthogonal to the runtime rebuild.** Resolution is
front-end + source-graph work: it produces more source units and cross-module
name bindings. It never touches the demand evaluator. *Evaluating* an imported
value at runtime is the runtime's concern; the front-end (and the grammar
loader, which reads a `Grammar { … }` literal straight from the AST) need only
the source and its definitions. That is why it can be built now while the
evaluator is in flux.

**v1 scope (simple + honest):** local resolution + a real module graph + real
cross-module exports — enough that `import "@vixen/grammar-cc"` works and the
grammar check runs in the compiler. **Non-goals for v1:** the network index,
Store-backed fetch, PubGrub solving, publishing. Those slot in behind the
resolver seam later, unchanged above it.

## Import path scheme

Reconciling [Vixen language → Imports](/vixen-language/#imports) and the package
model, an import string is one of:

- **`vixen:<name>`** — first-party built-ins shipped *with the runtime*: the
  implicit prelude (`vixen:core`) plus the small embedded set (`vixen:build`,
  and the grammar-authoring vocabulary `vixen:command-grammar`). Resolved from
  the bundled set, no fetch.
- **`@scope/name`** — a published package (`@vixen/grammar-cc`, `@bearcove/foo`).
  Resolved through the resolver (local in v1; index+Store later).
- **bare `name`** *(deferred past v1)* — a workspace-local sibling module or a
  manifest dependency alias. Not needed to unblock grammars; left out of v1 to
  keep the scheme small.

## Packages and the manifest

A package is the shape already specified: a directory with `main.vx` whose
**frontmatter** (a ```` ```styx ```` fence) is the manifest. v1 reads:

- `id` (`@scope/name`) — identity, deduped on.
- `dependencies` — `id → version-constraint`. v1 ignores the constraint (local,
  single version) but uses the *set* to scope what an import may name.

The manifest is parsed with **`SourceFile::frontmatter()` → styx**, via `facet`
(no hand-rolled parsing). A package's **exports** are the top-level definitions
of `main.vx`: both items (`fn`/`struct`/`enum`/…) **and** top-level `let`
bindings (e.g. `let cc = Grammar { … }`). This is the key extension — today only
items are cross-visible; exports must include value bindings too.

## The resolver seam

One trait isolates "where source comes from" from everything above it:

```rust
/// Resolves an import string to a package's source (its main.vx + the manifest).
trait PackageResolver {
    fn resolve(&self, spec: &str) -> Result<ResolvedPackage, ResolveError>;
}

struct ResolvedPackage {
    id: String,            // canonical @scope/name (or vixen:<name>)
    main: String,          // main.vx source text
    // siblings deferred: v1 packages are single-file main.vx
}
```

v1 implementations:

- **`BundledResolver`** — `vixen:*` and the first-party grammar packages, embedded
  via `include_str!` (the way `stdlib_sources()` already works). This alone
  unblocks `@vixen/grammar-cc`.
- **`FsResolver`** — `@scope/name` → a directory under a configured packages root
  (`<root>/@scope/name/main.vx`). For local/workspace packages.

Later: **`RegistryResolver`** — lockfile handle → Store bytes, per
[Packages and publishing](/packages-and-publishing/). The compiler holds a
`dyn PackageResolver`; nothing above the seam changes when it swaps.

## The import closure (compile driver)

In `compile_source_observed`, *before* `build_program`, compute the closure:

1. Start from the user `SourceFile` (`SourceId(0)`) and its manifest, if any.
2. Collect its `import "…"` specs (walk the CST for `ImportExpr`s + manifest
   `dependencies`).
3. For each spec, `resolver.resolve(spec)` → parse the package `main.vx` (the
   parser already handles frontmatter), assign the next `SourceId`, recurse over
   *its* imports. Dedup by package `id` (one source unit per package).
4. The prelude (`vixen:core`) and embedded set are always included (as today).

The output is the existing `Vec<(SourceId, SourceFile)>` that `build_program`
already takes, plus a side table `SourceId → package id` for diagnostics and
export scoping. **Cycles** (a imports b imports a) are allowed for *type/name*
resolution (modules are records); only a *value* cycle through eager evaluation
would be an error, and that's the runtime's check, not ours.

## Cross-module name resolution

Replace the flat "inject every stdlib item into the user scope" bridge
(`inject_implicit_stdlib_imports`) with proper module scoping:

- Each source unit gets a **module export scope** holding its top-level
  definitions — items **and** top-level `let` bindings (new: bindings become
  referenceable across units).
- **The prelude** (`vixen:core`) is still injected implicitly into every module
  (that's what "prelude" means) — the one privileged, un-imported module.
- **`import "…"`** resolves its path → the target unit's export scope. The
  `ImportExpr.resolved_source` finally gets filled.
- **`let { a, b } = import "…"`** binds `a`, `b` in the importing scope to the
  target's exports `a`, `b` (error if the target doesn't export them). This is
  the form `cc.vx` and user grammar code use.
- **`let m = import "…"`** binds `m` to the module as a record; `m.x` reaches
  export `x`. (Dotted-module access; v1 can support it or defer to the
  destructuring form — destructuring is what grammars need.)

`resolve.rs` is where this lands: stop no-oping `Import`; resolve the path via
the closure's `SourceId → unit` map; for a destructuring `let`, look the names up
in the target unit's export scope.

## The payoff: grammars check, end to end, front-end only

With the above, the `@cc[…]` check becomes real with **zero evaluation**:

1. User module: `let { cc } = import "@vixen/grammar-cc"`. The closure resolves
   the grammar package; `cc` binds to its top-level `let cc = Grammar { … }`.
2. `@cc[ … ]`'s grammar name `cc` resolves (already a binding) to that export.
3. In `type_check`, follow the binding to its initializer — the `Grammar { … }`
   **struct-literal AST** — and lower it to a `command_model::Grammar` with the
   existing **`command-grammar-vx` loader** (AST → grammar; no evaluator).
4. Run **`command-grammar-vx::check`** against it: unknown flags, value domains,
   and the typed hole expectations — now emitted as real `vx-ir` diagnostics.

So this task is the keystone: it connects the parser (CLI literal), the loader
(AST → grammar), and the checker (grammar → diagnostics) into the real compiler.

## What v1 ships

1. **Manifest**: parse `main.vx` frontmatter (styx, via facet) into a `Manifest`
   (`id`, `dependencies`).
2. **Resolver**: the `PackageResolver` trait + `BundledResolver` (stdlib + the
   first-party grammar packages) and a minimal `FsResolver`.
3. **Import closure**: assemble the source-unit graph in the compile driver.
4. **Module exports + cross-module resolution**: export scopes (items + top-level
   lets), prelude-as-implicit, `import`/destructuring binding in `resolve.rs`.
5. **Grammar check wiring**: in `type_check`, lower a resolved grammar binding's
   `Grammar` literal via the loader and run the check.

Each step is independently testable (a resolver test, a closure test, a
cross-module-resolution test, and the grammar-check test that's been waiting).

## Orthogonality, restated

Nothing here is a demand-graph, a kernel, or an evaluator. It is: parse more
files, build a module graph, resolve names across it. The one place it *touches*
values is reading a grammar's struct-literal AST as data — which the loader
already does without evaluation. When the runtime rebuild lands, evaluating
imported *values* plugs in beneath this unchanged.

## Open questions (for approval)

1. **Path scheme**: `vixen:<name>` for built-ins vs `@vixen/...` for published —
   is `command-grammar` a `vixen:` built-in (authoring vocabulary shipped with
   the runtime) while concrete grammars are `@vixen/grammar-*` packages? (That's
   the split I assumed.)
2. **Where the local packages root lives**: a workspace config key, a
   conventional dir, or just the `BundledResolver` for v1 (no FS at all until a
   real local-package need appears)?
3. **Whole-module import** (`let m = import …`, dotted access) in v1, or only
   destructuring (`let { … } = import …`) — which grammars need?
4. **Lockfile in v1**: I propose *none* (local resolution, single version);
   lockfiles arrive with the `RegistryResolver`. Agree?
