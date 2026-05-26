# vixen/wiki

Durable home for multi-thread architectural work on Vixen. Substrate is dodeca; substance is human-authored. Flat namespace, wiki-style links.

## Serve

```
ddc serve vixen/wiki
```

Default port range is 4000–4019. Live reload enabled.

## Layout

```
wiki/
├── .config/dodeca.styx     # page-types schemas + content/output paths
├── content/                # every page sits here directly (no subdirs)
│   ├── _index.md
│   ├── style.css
│   └── *.md                # one file per wiki page
├── templates/              # Tera HTML templates (index, section, page)
└── starters/               # blank starters for each page type — copy, rename, fill
```

## Page types

Pages have no per-type directory — the page type lives in the frontmatter, not the path. This keeps cross-references stable when a page changes type and keeps `[[Wiki links]]` namespace-free.

| Type               | Purpose                                              |
| ------------------ | ---------------------------------------------------- |
| `Vision`           | Long-form aspirational direction.                    |
| `Milestone`        | Concrete goal with enumerated success criteria.      |
| `Decision`         | ADR-style record of an architectural choice.         |
| `Question`         | Open architectural question.                         |
| `Experiment`       | Hypothesis + result.                                 |
| `ArchitectureNote` | How something works *today*.                         |
| `Glossary`         | One term, two or three sentences.                    |

## Frontmatter

TOML (`+++ ... +++`). Standard fields (`title`, …) at the top level; typed fields go inside `[extra]` with a `type` discriminant matching one of the page types declared in `.config/dodeca.styx`. Enum fields (e.g. `status = "living"`) work directly — dodeca's loader lifts the TOML string to the styx tag before validation.

## Linking

Inside markdown bodies, link to other pages with **wiki-style** syntax:

```
See [[Tier 1 — CCC]] for the lowest-touch deployment.
```

Resolution is by **page title**. Dodeca turns `[[Page Title]]` into a link to that page's permalink at render time. Use the canonical title verbatim (case + punctuation).

For typed cross-references in the frontmatter (e.g. `supersedes`, `resolved_by`), use the **slug** (filename without `.md`), e.g. `resolved_by = "cache-key-tightening-is-monotonic"`. Schemas declare these with `@link(@PageType)` and dodeca enforces both existence and type.

## Authorship discipline

- **Agents write scaffolding** (this README, schemas, templates, starters, validators).
- **Agents faithfully transcribe** stated vision when asked.
- **Agents do not author ideas.** Visions, decisions, milestone narratives — substance is human.
