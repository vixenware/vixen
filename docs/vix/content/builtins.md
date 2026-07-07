+++
title = "Builtins"
weight = 10

[extra]
updated = "2026-07-08"
+++

Some capabilities are provided by the machine rather than written in vix.
They fall into three honest categories — and knowing which is which tells
you where the language is going, not just what it can do today:

**Effects — permanent primitives.** Interactions with the world outside the
machine, which must be mediated so their observations are tracked:

- `fetch(url:, sha256:)` — retrieve external content, verified against an
  expected hash: the result is *content*, not "whatever the network said
  today." A checksum-bearing `fetch` is pure; a checksum-less fetch is a
  different, deliberately not-directly-exposed thing (an observation that
  pins what it saw, not a pure function).
- `exec` (via command templates like `rustc!{...}`, `build_script!{...}`) —
  run a real process, hermetically: declared inputs staged, sandbox
  enforced, outputs harvested as a `Tree`. The workhorse of building. See
  [Exec nodes](/vix/exec/).
- `glob`, `target()` — working-copy queries and host/platform facts, as
  tracked observations rather than ambient environment reads.

**Parsers — provisional primitives.** `toml()`, `json()`, `cfg(...)`, archive
extraction (`crate_archive`), format probes (ELF inspection). These are
machine-provided *today* and scheduled to become ordinary library code as
[snark](/dev/constellation/snark/) grammars — `cfg(...)` has already made
this move; JSON's grammar already exists and is next. The call sites you
write won't change when a parser makes this move; what's behind them will.
See [Document parsing](/vix/parsing/) for where each parser currently stands.

**Accessors — dissolving.** A historical layer of per-shape operations that
generic, schema-driven machinery is replacing from below. You mostly can't
tell from vix source that this is happening, which is the point: a `Doc`
field access or an array projection reads the same before and after its
implementation stops being a bespoke per-shape function and becomes generic
dispatch over a declared schema.

## The design pressure

Every builtin is a place where the machine, not the language, defines
behavior — so the project treats the builtin list as *debt to be paid down*,
not surface to be grown. The direction of travel is consistent: effects stay
(mediating the outside world really is the machine's job), everything else
migrates into vix or snark, where it becomes inspectable, incremental, and
yours to read instead of a black box you call into. See [Honest
gaps](/vix/gaps/) for what's migrated already and what's still in flight.
