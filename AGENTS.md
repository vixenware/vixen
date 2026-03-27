# Agent Notes

All operational/development workflow instructions live in `DEVELOP.md`.

Agents should follow `DEVELOP.md` and not duplicate process instructions here.

## Documentation Authority

For language semantics, execution model, runtime model, compiler architecture,
and eval-related planning, the source of truth is:

- `docs/content/spec/`

Use this authority split:

- `docs/content/spec/` = normative/current spec
- `docs/design/` = working notes and draft design material
- `notes/archive/` = historical material, not authority

If `docs/content/spec/` and `docs/design/` disagree, `docs/content/spec/` wins.

### Required Reading Order For Eval/Architecture Work

Before making changes or recommendations about eval, runtime, lowering, kernels,
laziness, demand, closures, host boundaries, or related documentation, agents
must read these first:

1. `docs/content/spec/language/core.md`
2. `docs/content/spec/language/build.md`
3. `docs/content/spec/internals.md`

Read `docs/content/spec/language/guide.md` only as a guide-level aid. If it
disagrees with the normative spec, the normative spec wins.

Only after reading the normative spec should agents consult relevant files under
`docs/design/`.

### Planning Discipline

Agents must not:

- treat draft design notes as the primary source of truth without first reading
  the normative spec
- create parallel architecture tracks that ignore existing spec material
- recommend doc restructuring before checking the current `docs/content/spec/`
  layout and contents

When touching eval or architecture docs, prefer:

- promoting accepted truth into `docs/content/spec/`
- keeping `docs/design/` for drafts/open questions
- moving retired material to `notes/archive/`
