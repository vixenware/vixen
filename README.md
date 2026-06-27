# vixen

![Vixen fox logo](docs/static/img/fox.svg)

An experimental demand-driven build system.

Vixen evaluates lazily, runs boundary work on executors, and treats content
identity/cache keys as part of the semantics rather than as an afterthought.

## Orientation

Read [`AGENTS.md`](AGENTS.md) before changing runtime, evaluator, build
semantics, or the macOS FSKit path. The runtime is mid-rebuild, and the current
checkpoint is the graph-driven execution split:

- [`docs/design/runtime-graph-execution/README.md`](docs/design/runtime-graph-execution/README.md)
  is the current execution roadmap and anti-fallback checklist.
- [`wiki/content/runtime-semantics.md`](wiki/content/runtime-semantics.md),
  [`wiki/content/strand-scheduler.md`](wiki/content/strand-scheduler.md), and
  [`wiki/content/exec.md`](wiki/content/exec.md) carry the demand-runtime model.
- Normative specs live under [`docs/spec`](docs/spec).

## Local entry points

- `cargo run --bin vixen -- --help` shows the management CLI.
- `cargo run --bin vixen -- health` checks or starts the daemon target.
- `cargo run --bin vx -- <command>` is the package-command shortcut path.
- `cargo nextest run` is the test runner for Rust tests.

Tracey is configured in [`.config/tracey/config.styx`](.config/tracey/config.styx).
Use `tracey_config` or `tracey_status` against this repo root to discover the
active spec prefixes and coverage.

The architectural wiki is served with `ddc serve wiki` from this repo root, or
`ddc serve vixen/wiki` from `/Users/amos/vixenware`.

The macOS app and FSKit host are documented in [`apps/mac`](apps/mac).

## Co-developed with LLMs

This project was co-developed with LLMs such as Claude, Codex, etc. Human
maintainers are responsible for their contributions regardless of machine help.

## License

Vixen is licensed under [AGPL-3.0-or-later](LICENSE).

For commercial licensing inquiries, contact: amos@bearcove.eu
