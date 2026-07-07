# Parked designs: the vix playground + dodeca-on-snark

Status: Amos, 2026-07-08 — "don't do it now but let's think about it." Thought about;
recorded; NOT fired.

## Interactive exercises in the vix docs (editor + machine-in-WASM)

Feasibility inventory (all existing):
- Interpreter lane is pure Rust; JIT/stencils stay native — lanes() split was built for this.
- wasm precedent in-repo: snark-wasm, styx-wasm, vox wasm-browser-tests.
- **The fake exec backend is the perfect browser citizen**: whole fake-rustc builds run with
  no OS/network; trees are store values. Exercises therefore teach THE MACHINE, not syntax:
  "make this build hit the cache", "why did this recompute — fix it", "prove the argv is
  lazy", with the demand trace rendered live. Unique pedagogy: nobody else's build system
  runs in a browser tab.
- Secretly the sales demo: /proof becomes runnable.
- Editor: CodeMirror; highlighter GENERATED from the snark grammar of vix (grammar exists;
  derived tooling stays in sync by construction). vix-lsp exists for a later rung.
- Risks: wasm size (interp-only, likely fine); host fns needing OS paths (fake backend
  avoids); tokio → wasm-bindgen-futures/single-thread; tracing → console sink.
- Integration: dodeca embeds a playground component (vite bundle + wasm as static siblings).

## Dodeca → snark

- Visible part: markdown as a snark text-dialect grammar. OWN the dialect
  (dodeca-markdown ⊂ CommonMark; the 3% rule — full CommonMark not owed).
- Real payoffs: (1) Amos's inline annotations become AST NODES, not comment-regex — the
  annotation workflow stops being fragile to wholesale edits; (2) incremental reparse falls
  out of the tree-sitter-compatible model (live editing); (3) one parsing substrate
  constellation-wide; (4) frontmatter via the same doc-parse choke point.
- ENDGAME (the reason to do it): dodeca becomes a vixen APPLICATION — pages as derived
  artifacts, site rebuild as memoized demand, live-reload as invalidation. The docs site
  built by the build system it documents.

## Sequencing instinct
Playground after /vix namespace has content worth exercising (docs mission feeding it now).
Dodeca-snark when a driver beyond aesthetics exists — annotation robustness or the
playground's literate pages. Both are constellation-priced (cheap where incumbents find
them expensive): grammar-derived tooling, fake-backend-in-wasm, demand-driven rebuilds.
