# grammars — the authoring side

CLI tool grammars, authored in **Vixen** against `@vixen/command-grammar`. This
is the form grammars actually ship in (through the package manager); the Rust
`Grammar` consts in `src/lib.rs` are the engine's hand-coded equivalents until
the lowering bridge lands.

- `command-grammar.vx` — the type library (`Grammar`, `Flag`, `Spelling`,
  `Policy`, `Kind`, `ValueDomain`, `Positional`, `EnvDecl`, `Arity`).
- `cc.vx` — the cc/clang grammar (plus `ld`, referenced by `-Wl,` **by name**),
  with the docs and value-domains that the LSP and `explain` surface.

## Notes

- **Sub-grammars are referenced by name** (`Passthrough { grammar: "ld" }`),
  not by value: Vixen forbids recursive types, and by-name resolution is what we
  want for cross-package references anyway. The engine resolves names at load.
- **Struct field defaults** keep `Flag` terse — only `canonical`, `spelling`,
  `policy`, `kind` are required; `aliases`, `value`, `doc` default.
- These `.vx` files use the current Vixen design and **do not run on today's
  runtime** (which is mid-migration). They are the authoring target; next is the
  engine side — an owned `Grammar` + a facet (de)serialization bridge that loads
  a lowered grammar value, evaluated by the daemon (never `vx-rt`).
