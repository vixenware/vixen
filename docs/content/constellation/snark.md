+++
title = "snark — declarative parsing"
+++

snark is a parser system in the tree-sitter tradition — grammars as declarative
descriptions, parse tables generated from them, incremental-friendly concrete
syntax trees — rebuilt from scratch so the whole pipeline lives inside the
constellation rather than behind a C toolchain.

A language on snark is two files: a grammar (tree-sitter-style) and a small
annotation file naming the structures. From those, snark generates the typed
AST and the lowering that produces it — nobody hand-writes tree-walking code.
vix itself parses this way; so do the other languages in the workshop. The same
grammar drives editor tooling: syntax highlighting and structural queries run
off the grammar's query engine, not off a second implementation.

## Why rebuild tree-sitter?

Three reasons, in increasing order of ambition:

1. **No C dependency.** Highlighting a code block shouldn't require a C
   compiler at build time. snark is pure Rust, and its parsers lower to
   [weavy](@/constellation/weavy.md) like everything else.
2. **One substrate.** A parser that compiles to the same execution substrate as
   the rest of the system inherits the machinery: observable execution, the
   JIT, and — the real prize — the demand machine's memoization. Incremental
   reparsing stops being a bespoke algorithm and becomes *the build system
   applied to text*: unchanged spans are cache hits, and everything computed
   *from* unchanged syntax (queries, diagnostics, downstream builds) cuts off
   early. That convergence is in progress; the direction is settled.
3. **Binary formats.** Textual grammars are tree-sitter-shaped, but build
   systems also read *artifacts* — object files, archives, indexes. The plan
   is a second declarative dialect for binary formats (structured reads with
   computed offsets — think Kaitai, not lex/yacc) sharing snark's toolchain:
   declare, generate, observe. Today those readers are Rust capabilities; the
   dialect replaces them piecemeal.

## Where it stands

Textual snark is real and load-bearing: vix's own grammar, its typed AST, and
its editor tooling all ship on it. Binary snark is design-stage. The honest
boundary: anything you can describe as a grammar belongs here eventually;
entropy coding and cryptography never will (a gzip stream is not a syntax
tree, and pretending otherwise helps nobody).
