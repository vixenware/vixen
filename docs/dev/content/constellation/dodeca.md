+++
title = "dodeca — docs"
weight = 10
+++

dodeca is the documentation engine — the thing serving the page you're reading.
Content lives as Markdown with typed frontmatter, templates render it, search
is built in, and the whole site regenerates live as files change.

Two features shape how this knowledge base is actually made:

**Annotations.** In editing mode, a reader can select any passage and attach a
note — a question, a correction, a "this is wrong because…" — which lands as a
structured comment *in the source file*, surviving edits around it. This turns
documentation review into a conversation held in the margins of the real text.
This site is being written that way: drafted, annotated, revised, in a loop.

**Typed pages.** Frontmatter is schema-checked — pages declare their type and
metadata against a config, so "every page states its kind, tier, and freshness
date" is enforced, not aspired to. For an early-access knowledge base, the
tier field is the access-control seam: pages tagged for a later wave simply
aren't in an earlier wave's build.

dodeca is also a member of the family it documents: its content pipeline is
the kind of workload vixen exists for (parse → transform → render, with exact
invalidation), and the arrows point in both directions — the site is a
consumer of the constellation's parsing and templating
([snark](/constellation/snark/) highlighting,
[gingembre](/constellation/gingembre/) templating) and a proving ground
for the incremental machinery underneath.
