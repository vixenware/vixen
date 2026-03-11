+++
title = "Diagnostics Rendering Specification"
insert_anchor_links = "heading"
+++

# Diagnostics Rendering Specification

This chapter specifies the terminal diagnostics renderer we want for Vixen.
It is deliberately renderer-centric rather than language-centric: the goal is
to define a reusable diagnostics engine with clear behavior, not a pile of
formatting heuristics.

The intended first consumer is `vx` diagnostics, but the design should be
portable enough to escape the Vixen repository later.

## Scope

This specification covers:

1. structured diagnostic input
2. terminal rendering and layout
3. source window selection
4. label overlap and priority behavior
5. syntax token overlays
6. theming and glyph fallback

It does not specify:

1. LSP transport
2. HTML rendering
3. evaluator provenance collection
4. exact public crate names or module layout

## Packaging

> d[package.name]
>
> The diagnostics renderer lives in a new crate named `margin`. The crate name
> is part of the intended product shape: it should be neutral, reusable outside
> Vixen, and not tied to one specific caller such as `vx`, `vxd`, or a compiler
> frontend.

> d[package.no-vixen-prefix]
>
> The reusable renderer crate does not use a `vx-` prefix. Vixen-specific
> adapters, conversion glue, or integration helpers may live in Vixen crates,
> but the renderer itself is named and structured as a standalone library.

> d[package.primary-split]
>
> The initial deliverable is one primary crate, `margin`, that owns the
> diagnostic model, layout engine, terminal rendering, and fixture tests needed
> for immediate adoption in Vixen. The first release does not require an
> up-front split into multiple crates.

> d[package.future-split]
>
> If the implementation later needs clearer separation, the package topology
> may split along renderer boundaries:
>
> 1. `margin` for the core model and layout engine
> 2. `margin-term` for terminal rendering
> 3. optional future crates such as `margin-html`
>
> This split is optional and should follow real boundary pressure, not happen
> pre-emptively.

## Public API Shape

> d[api.core-model]
>
> `margin` exposes a first-class diagnostic model as a library API rather than
> only a "render this error" convenience function. Callers can construct
> reports, windows, labels, overlays, notes, and themes as structured values.

> d[api.layout-render-separation]
>
> The crate keeps a clear conceptual split between:
>
> 1. input model construction
> 2. layout planning
> 3. final terminal emission
>
> The implementation may realize this split with modules, types, or internal
> phases, but the codebase should not collapse all three concerns into a single
> ad hoc write loop.

> d[api.renderer-options]
>
> Width, glyph mode, theme, and other presentation choices are explicit renderer
> options. Callers do not discover these behaviors indirectly through ambient
> terminal state alone.

> d[api.stable-fixtures]
>
> The public or crate-visible testing surface supports fixture-driven rendering
> from structured inputs so Vixen can lock down output behavior before migration.

## Input Model

> d[input.structured]
>
> The renderer accepts a structured diagnostic model rather than raw formatted
> strings. At minimum, the model distinguishes reports, source windows, labels,
> notes, help text, syntax token overlays, severity, and styling roles.

> d[input.unified-spans]
>
> All span-bearing inputs use a single explicit coordinate system per source,
> and the renderer defines one authoritative conversion path when display
> layout requires character or grapheme indices. Callers must not need
> renderer-specific span glue logic.

> d[input.source-identity]
>
> Each source-backed item carries stable source identity sufficient to map
> spans, source windows, and syntax overlays to the same underlying file or
> buffer.

## Layout

> d[layout.width-aware]
>
> Terminal rendering is width-aware. The renderer accepts an explicit target
> width and uses it when placing gutters, source text, connectors, side
> messages, wrapped notes, and continuation lines.

> d[layout.no-terminal-wrap]
>
> The renderer does not rely on arbitrary terminal wrapping for diagnostic
> correctness. When text would exceed the available width, the renderer
> reflows or repositions content intentionally.

> d[layout.message-placement]
>
> Message placement prefers keeping a label's explanation near its anchor span.
> If the preferred side placement is crowded or too wide, the renderer may
> choose an alternate supported placement, such as below-span or stacked
> annotation mode, instead of pushing the message arbitrarily far right.

> d[layout.multiline-labels]
>
> Label messages may contain soft wraps or hard line breaks. Continuation lines
> remain attached to the same diagnostic layout: aligned with the gutter and
> connector geometry rather than emitted as raw terminal lines.

> d[layout.notes-wrap]
>
> Notes, help text, and other report body prose wrap to the active width while
> preserving diagnostic indentation and visual grouping.

## Source Windows

> d[window.primary-context]
>
> Source rendering is windowed. A report renders the primary span plus a bounded
> amount of surrounding source context rather than the entire file by default.

> d[window.merge-nearby]
>
> Nearby spans in the same source are merged into a single rendered window when
> that produces a clearer diagnostic than separate repeated sections.

> d[window.stable-selection]
>
> Source window selection is deterministic for the same diagnostic input and
> width. Repeated renders do not oscillate between different context cuts.

## Labels and Overlays

> d[label.priority]
>
> Overlapping annotations obey explicit priority. Higher-priority labels or
> overlays override lower-priority ones for visible styling in overlapping
> ranges.

> d[label.semantic-over-syntax]
>
> Diagnostic labels override passive syntax-highlighting overlays on overlap.
> Syntax coloring provides context; diagnostic emphasis is the primary signal.

> d[label.clustered]
>
> The renderer supports clustered reports: nearby or equivalent spans may be
> represented as one visible explanatory cluster instead of duplicated repeated
> blocks.

> d[label.single-message-bearing]
>
> Within one visible cluster, the renderer can prefer one primary
> message-bearing label while retaining additional unlabeled or compactly
> rendered spans for context.

## Syntax Highlighting

> d[syntax.overlay-model]
>
> Syntax highlighting enters the renderer as structured span overlays, not as
> pre-colored source text. The renderer composes token overlays with diagnostic
> labels in one pass.

> d[syntax.window-bounded]
>
> Syntax highlighting is bounded by the rendered source windows. Highlighting a
> source file does not, by itself, cause the renderer to expand the visible
> source region to the whole file.

## Themes and Glyphs

> d[theme.roles]
>
> Styling is role-based rather than hard-coded by widget. Themes can style at
> least severity, primary label, secondary label, syntax token kinds, notes,
> help text, gutter, connector, and emphasis roles independently.

> d[glyph.unicode-ascii]
>
> The renderer supports both Unicode and ASCII presentation. Layout remains
> correct when box-drawing or arrow glyphs are replaced with ASCII fallbacks.

## Determinism and Testing

> d[test.fixture-first]
>
> The renderer is testable from structured fixtures. Given the same diagnostic
> model, theme, glyph mode, and width, it produces stable output suitable for
> snapshot-style tests.

> d[test.width-matrix]
>
> Width-sensitive behavior is covered by tests at multiple widths so wrapping,
> placement, and connector routing are validated intentionally rather than by
> accident.

> d[test.unicode]
>
> The test suite covers non-ASCII source text and label messages so span
> mapping, wrapping, and connector placement remain correct for Unicode input.

> d[test.migration-fixtures]
>
> The test suite includes acceptance fixtures derived from real Vixen build
> diagnostics, including:
>
> 1. primary build-input failures
> 2. demand-chain advice frames
> 3. overlapping syntax and error labels
> 4. narrow-width terminal layouts
> 5. duplicate-span clustering cases

## Vixen Integration

> d[integration.vx-adapter]
>
> Vixen-specific conversion from evaluator or daemon diagnostics into `margin`
> input types remains outside `margin` itself. `margin` consumes structured
> diagnostics; it does not know about Vixen evaluator internals, `PoisonError`,
> or daemon RPC types.

> d[integration.migration-target]
>
> `vx` terminal diagnostics become a `margin` consumer once the crate can render
> current structured build failures with parity or better behavior on:
>
> 1. source windowing
> 2. syntax-overlay composition
> 3. diagnostic-label priority
> 4. multiline and width-aware text layout

> d[integration.parity-before-removal]
>
> Existing Ariadne-based rendering is not removed until `margin` demonstrates
> parity or improvement on the Vixen fixture set for the currently supported
> build-diagnostics flows.

> d[integration.no-format-lockin]
>
> Vixen integration does not depend on Ariadne-specific concepts or naming. The
> adapter layer targets `margin`'s model directly, even if temporary bridges are
> used during migration.

## Migration Plan

> d[migration.stage-one]
>
> Stage one delivers `margin` as a new crate in the Vixen repository with no
> immediate requirement to replace all existing rendering paths. The first goal
> is a stable crate boundary and acceptance-test coverage.

> d[migration.stage-two]
>
> Stage two migrates `vx` build diagnostics to `margin`, using the existing
> structured build-failure model as the source input and retaining current user
> functionality while improving layout quality.

> d[migration.stage-three]
>
> Stage three removes obsolete renderer-specific glue once `margin` is the
> production path for the targeted Vixen diagnostics flows.

> d[migration.merge-safety]
>
> Deliverables merged into Vixen `main` must keep the migration path explicit:
> the crate boundary, adapter boundary, and remaining parity gaps are documented
> so follow-up integration work does not rely on implicit assumptions from the
> original author.

## Product Standard

> d[product.compiler-grade]
>
> The finished renderer should feel like a compiler-grade diagnostic system:
> source-first, compact, intentional, and readable at ordinary terminal widths.
> It must not feel like a terminal dump whose final layout is left to the shell.
