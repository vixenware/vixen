# vixen/wiki

Durable home for multi-thread architectural work on Vixen. Substrate is dodeca; substance is human-authored. Flat namespace, explicit slug links.

## Serve

```
ddc serve wiki
```

From `/Users/amos/vixenware`, use `ddc serve vixen/wiki`.

Default port range is 4000–4019. Live reload enabled.

## Layout

```
wiki/
├── .config/dodeca.styx     # source schema + site output path
├── content/                # every page sits here directly (no subdirs)
│   ├── _index.md
│   ├── style.css
│   └── *.md                # one file per wiki page
├── templates/              # Tera HTML templates (index, section, page)
└── starters/               # blank starters for each page type — copy, rename, fill
```

## Page Types

Pages have no per-type directory — the page type lives in the frontmatter, not the path. This keeps cross-references stable when a page changes type and keeps URLs namespace-free.

The live schema defines two page types:

| Type   | Purpose |
| ------ | ------- |
| `Doc`  | Synthesized explanation, organized for reading. |
| `Task` | Work item with status, priority, optional blockers, and tier tags. |

Task statuses are `todo`, `in-progress`, `blocked`, `done`, `superseded`, and
`abandoned`.

`superseded` is a first-class lifecycle state, not a validation failure. A
superseded page should be a short tombstone that points to the current model; it
must not keep a long contradictory body under the tombstone.

## Frontmatter

TOML (`+++ ... +++`). Standard fields (`title`, …) at the top level; typed fields go inside `[extra]` with a `type` discriminant matching one of the page types declared in `.config/dodeca.styx`. Enum fields (e.g. `status = "todo"`) work directly — dodeca's loader lifts the TOML string to the styx tag before validation.

## Linking

Inside markdown bodies, link to other pages with explicit site-root URLs:

```
See [CCC (Cargo Cloud Cache)](/ccc/) for the lowest-touch deployment.
```

Resolution is by **route slug**. Use the filename-derived route with leading and trailing slashes, e.g. `content/cache-and-trust.md` becomes `/cache-and-trust/`.

For typed cross-references in the frontmatter (e.g. `supersedes`, `resolved_by`), use the **slug** (filename without `.md`), e.g. `resolved_by = "cache-key-tightening-is-monotonic"`. Schemas declare these with `@link(@PageType)` and dodeca enforces both existence and type.

## Authorship discipline

- **Agents write scaffolding** (this README, schemas, templates, starters, validators).
- **Agents faithfully transcribe** stated vision when asked.
- **Agents do not author ideas.** Visions, decisions, milestone narratives — substance is human.
