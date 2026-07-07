+++
title = "Builtins"
+++

Some capabilities are provided by the machine rather than written in vix.
They fall into three honest categories — and knowing which is which tells you
where the language is going:

**Effects — permanent primitives.** Interactions with the world outside the
machine, which must be mediated so their observations are tracked:

- `fetch(url:, sha256:)` — retrieve external content, verified against an
  expected hash: the result is *content*, not "whatever the network said."
- `exec` (via command templates like `rustc!{...}`) — run a real process,
  hermetically: declared inputs staged, sandbox enforced, outputs harvested as
  a `Tree`. The workhorse of building.
- `glob`, `target()` — working-copy queries and host/platform facts, as
  tracked observations.

**Parsers — provisional primitives.** `toml()`, `json()`, archive extraction
(`crate_archive`), format probes (ELF inspection). These are machine-provided
*today* and scheduled to become ordinary library code as
[snark](@/constellation/snark.md) grammars (JSON first — its grammar already
exists) or registered capabilities. The call sites you write won't change;
what's behind them will.

**Accessors — dissolving.** A historical layer of per-shape operations that
generic machinery is replacing from below. You mostly can't tell from vix
source, which is the point.

## The design pressure

Every builtin is a place where the machine, not the language, defines
behavior — so the project treats the builtin list as *debt to be paid down*,
not surface to be grown. The direction of travel is consistent: effects stay
(they're the machine's actual job), everything else migrates into vix or
snark where it becomes inspectable, incremental, and yours to read. The
[status page](@/status/_index.md) tracks migrations as they land.
