+++
title = "Diagnostics Rendering Specification"
insert_anchor_links = "heading"
weight = 40
+++

# Diagnostics Rendering Specification

This chapter specifies the terminal diagnostics renderer we want for Vixen.
It is deliberately renderer-centric rather than language-centric: the goal is
to define a reusable diagnostics engine with clear behavior, not a pile of
formatting heuristics.

The design is portable enough to serve Vixen and other consumers without
changing its core model.

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
> The diagnostics system is introduced as two crates from the beginning:
>
> 1. `margin`
> 2. `margin-term`
>
> `margin` is the neutral reusable core crate name and is part of the intended
> product shape: it should be portable outside Vixen and not tied to one
> specific caller such as `vx`, `vxd`, or a compiler frontend.

> d[package.no-vixen-prefix]
>
> The reusable renderer crates do not use a `vx-` prefix. Vixen-specific
> adapters, conversion glue, or integration helpers live in Vixen crates,
> but the renderer is named and structured as a standalone library boundary.

> d[package.primary-split]
>
> The diagnostics system has a required two-crate split:
>
> 1. `margin` owns the diagnostic model, layout engine, shared rendering data
>    structures, and fixture tests for layout behavior.
> 2. `margin-term` owns terminal-specific emission, glyph selection, color
>    output, and terminal-facing renderer options.

> d[package.additional-renderers]
>
> The package topology supports additional renderer frontends as sibling crates
> without changing the split between core and terminal:
>
> 1. `margin` for the core model and layout engine
> 2. `margin-term` for terminal rendering
> 3. additional renderer crates such as `margin-html`
>
> The split between core and terminal remains stable as more frontends are
> added.

## Public API Shape

> d[api.core-model]
>
> `margin` exposes a first-class diagnostic model as a library API rather than
> only a "render this error" convenience function. Callers can construct
> reports, windows, labels, overlays, notes, and themes as structured values.

> d[api.layout-render-separation]
>
> The crate topology and code structure keep a clear split between:
>
> 1. input model construction
> 2. layout planning
> 3. final terminal emission
>
> `margin` owns the first two concerns. `margin-term` owns the third. The
> implementation must not collapse all three concerns into a single ad hoc
> write loop.

> d[api.renderer-options]
>
> Width, glyph mode, theme, and other presentation choices are explicit
> renderer options. Terminal-facing options belong in `margin-term`; layout
> inputs that affect planning belong in `margin`. Callers do not discover these
> behaviors indirectly through ambient terminal state alone.

> d[api.stable-fixtures]
>
> The public or crate-visible testing surface supports fixture-driven rendering
> from structured inputs so callers can lock down output behavior precisely.

> d[api.explicit-layout-artifact]
>
> `margin` exposes an explicit layout artifact or layout plan between input
> diagnostics and final terminal output. `margin-term` consumes that artifact
> instead of rediscovering geometry opportunistically while emitting text.

## Core Model

> d[model.report]
>
> The core model represents a diagnostic report as structured data with, at
> minimum:
>
> 1. severity
> 2. headline or title text
> 3. zero or more source-backed annotations
> 4. zero or more notes or help entries
> 5. zero or more subordinate sections or related reports

> d[model.source]
>
> The core model distinguishes source identity from source content. A source has
> stable identity, display name, and text content; multiple reports may refer to
> the same source without duplicating source bookkeeping.

> d[model.annotation]
>
> The core model distinguishes annotations from prose. An annotation binds a
> span to semantics such as primary label, secondary label, related span,
> syntax-token overlay, or emphasis; prose such as notes and help is modeled
> separately.

> d[model.window]
>
> Source windows are first-class values. A window is an explicit visible slice
> of one source with line boundaries, omitted regions, and the annotations
> assigned to it.

> d[model.role-stable]
>
> Annotation and text roles are semantic and stable across renderers. The model
> names roles such as primary diagnostic label, secondary label, syntax token
> class, note heading, and connector style independently of any one renderer's
> internal widget vocabulary.

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

> d[input.span-bounds]
>
> Every span has inclusive start and exclusive end semantics. Zero-width spans
> are representable and have intentional rendering behavior rather than being
> treated as malformed input.

> d[input.multiple-sources]
>
> One report may refer to multiple sources. Cross-file diagnostics are modeled
> directly rather than flattened into unrelated standalone reports.

> d[input.overlay-kinds]
>
> Overlay inputs are typed. Syntax overlays, search highlights, selections, and
> diagnostic labels are represented as distinct overlay kinds even when they
> share span-like geometry.

## Span Semantics

> d[span.unicode-correct]
>
> Span mapping is Unicode-correct. The implementation defines and documents the
> relationship between byte offsets, scalar-value indices, grapheme boundaries,
> and terminal cell widths so callers know what is canonical and what is
> derived.

> d[span.authoritative-source]
>
> One coordinate space is authoritative inside `margin`; all other derived
> indices are computed from it. The implementation does not bounce spans
> between partially-specified coordinate systems.

> d[span.zero-width]
>
> Zero-width spans render intentionally, for example as insertion markers or
> carets, and participate in overlap and placement rules without breaking the
> rest of the layout engine.

> d[span.line-breaks]
>
> Spans that cross line breaks are supported as first-class inputs. The layout
> engine defines how multiline spans are segmented for underlines, connectors,
> and inline messages.

## Layout Pipeline

> d[layout.pipeline]
>
> Layout is a defined pipeline rather than ad hoc mutation during terminal
> emission. At minimum the pipeline includes:
>
> 1. source normalization and span resolution
> 2. clustering and coalescing
> 3. source-window selection
> 4. lane or connector assignment
> 5. message placement and wrapping
> 6. final terminal-cell emission

> d[layout.pure-planning]
>
> Layout planning is deterministic from structured inputs and renderer options.
> Reading ambient terminal state or measuring partially-emitted output does not
> change geometry decisions.

> d[layout.cell-grid]
>
> Layout planning targets terminal cell positions rather than byte counts. All
> width-sensitive placement uses display-cell measurements.

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

> d[layout.placement-modes]
>
> The renderer supports multiple annotation-message placement modes. The layout
> engine may choose among at least:
>
> 1. side placement near the span
> 2. below-span placement
> 3. stacked compact placement for crowded regions

> d[layout.crowding-policy]
>
> The layout engine has an explicit crowding policy. When several annotations
> compete for the same horizontal space, the engine resolves that competition
> predictably instead of degrading into arbitrary far-right placement.

> d[layout.gutter-geometry]
>
> Gutter width, line-number width, separator columns, and connector columns are
> explicit geometry decisions in the layout plan rather than side effects of
> string formatting.

> d[layout.ellipsis]
>
> Omitted source regions render with explicit omission markers. A window never
> silently skips lines without a visible indicator.

> d[layout.long-lines]
>
> Long source lines are handled intentionally. The renderer defines whether a
> line is clipped, horizontally windowed, wrapped, or segmented, and preserves
> annotation alignment under that policy.

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

> d[window.context-policy]
>
> Context selection is policy-driven. The renderer defines how many surrounding
> lines a primary, secondary, or related span receives and how those windows
> merge.

> d[window.cross-file]
>
> Reports that touch multiple sources render as multiple explicit source
> sections. Each section has its own heading, local window selection, and local
> annotations.

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

> d[label.priority-lattice]
>
> Overlay precedence is explicit and stable. The system defines a coherent order
> between syntax overlays, passive highlights, related spans, warnings, errors,
> selections, and any other supported overlay classes.

> d[label.multiline-message-alignment]
>
> When a label message wraps or spans multiple lines, each continuation line
> remains visually associated with the same anchor annotation rather than
> becoming an unrelated paragraph.

> d[label.multispan]
>
> One logical label may cover multiple disjoint spans when that yields a
> clearer explanation than emitting duplicated messages on each span
> separately.

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

> d[syntax.non-owning]
>
> Syntax overlays do not own source-window selection, message placement, or
> report grouping. They are compositional inputs to layout, not the primary
> driver of report shape.

> d[syntax.overlay-priority]
>
> Syntax overlays participate in the same explicit precedence system as other
> overlays and never override higher-priority diagnostic semantics.

## Coalescing and Deduplication

> d[coalesce.identical]
>
> Identical annotations are deduplicated before layout. Two annotations with
> the same source, span, role, and message do not produce duplicated visible
> output.

> d[coalesce.nearby]
>
> Nearby equivalent annotations may be coalesced into one visible cluster when
> that preserves meaning and improves readability.

> d[coalesce.message-bearing]
>
> Coalescing distinguishes geometric spans from message-bearing explanations. A
> cluster may retain multiple geometric anchors while choosing one primary
> visible explanation.

> d[coalesce.report-sections]
>
> Repeated source sections caused solely by layout fragmentation are merged into
> one report section when they refer to the same source region and diagnostic
> meaning.

> d[coalesce.deterministic]
>
> Coalescing decisions are deterministic. Equivalent input orderings do not
> lead to visibly different output merely because annotations arrived in a
> different sequence.

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

> d[theme.capability-fallback]
>
> The renderer degrades intentionally across terminal capabilities. Lack of
> color, lack of truecolor, or forced ASCII mode changes presentation while
> preserving structure and meaning.

## Unicode and Width

> d[unicode.display-width]
>
> Display width is computed with a Unicode-aware width algorithm equivalent in
> behavior to `unicode-width` or a stricter implementation. Combining marks,
> zero-width joiners, double-width characters, and other non-ASCII cases are
> not treated as one byte equals one column.

> d[unicode.grapheme-safety]
>
> Wrapping, clipping, and visible slicing do not split grapheme clusters in a
> way that produces broken display text or misaligned annotations.

> d[unicode.tab-policy]
>
> Tab handling is explicit. The renderer defines tab expansion before geometry
> planning and uses that policy consistently for source text, connectors, and
> caret placement.

> d[unicode.normalization-stability]
>
> The renderer does not silently normalize source text in ways that would shift
> span identity. Any normalization used for width calculations preserves source
> indexing semantics.

## Terminal Capabilities

> d[term.explicit-capabilities]
>
> `margin-term` renders against explicit terminal capability settings such as:
>
> 1. width
> 2. color level
> 3. glyph mode
> 4. hyperlink support if used
>
> The renderer does not hide these assumptions behind ambient process state
> alone.

> d[term.ansi-discipline]
>
> Terminal styling output is structurally correct. ANSI sequences, if enabled,
> are emitted in a balanced and composable way so wrapped or truncated output
> does not corrupt subsequent terminal text.

> d[term.plaintext-mode]
>
> `margin-term` supports plaintext rendering with no ANSI styling while
> preserving layout, grouping, and annotation meaning.

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

> d[test.vixen-acceptance-fixtures]
>
> The test suite includes acceptance fixtures derived from real Vixen build
> diagnostics, including:
>
> 1. primary build-input failures
> 2. demand-chain advice frames
> 3. overlapping syntax and error labels
> 4. narrow-width terminal layouts
> 5. duplicate-span clustering cases

> d[test.capability-matrix]
>
> The test suite covers terminal capability combinations, including:
>
> 1. Unicode plus ANSI color
> 2. ASCII plus ANSI color
> 3. Unicode plaintext
> 4. ASCII plaintext

> d[test.coalescing-fixtures]
>
> The test suite includes fixtures that differ only in annotation order so
> coalescing and placement stability are verified against ordering artifacts.

## Vixen Integration

> d[integration.vx-adapter]
>
> Vixen-specific conversion from evaluator or daemon diagnostics into `margin`
> input types remains outside `margin` itself. `margin` consumes structured
> diagnostics; it does not know about Vixen evaluator internals, `PoisonError`,
> or daemon RPC types.

> d[integration.vx-consumer]
>
> `vx` terminal diagnostics consume `margin` + `margin-term` through a Vixen
> adapter layer. That integration preserves the structured build-failure input
> model and supports at least:
>
> 1. source windowing
> 2. syntax-overlay composition
> 3. diagnostic-label priority
> 4. multiline and width-aware text layout

> d[integration.no-format-lockin]
>
> Vixen integration does not depend on Ariadne-specific concepts or naming. The
> adapter layer targets `margin`'s model directly rather than Ariadne-specific
> report structures or naming.

## Product Standard

> d[product.compiler-grade]
>
> The finished renderer should feel like a compiler-grade diagnostic system:
> source-first, compact, intentional, and readable at ordinary terminal widths.
> It must not feel like a terminal dump whose final layout is left to the shell.
