# vixen

![Vixen fox logo](art/fox.svg)

A Rust build engine with correct caching. The CLI is `vx`.

## Getting Started

```bash
cargo xtask install
```

This installs `vx` and `vxd`. On Linux, you also need `libfuse3` and `fusermount`.

Then try it on your project:

```bash
vx build
```

## Brand

- Primary brand color: `#E88602`
- Brand guide: [`BRAND.md`](BRAND.md)

## What it does today

Builds single-crate Rust projects with no dependencies:

```bash
vx build           # debug build
vx build --release # release build
vx clean           # remove .vx/ directory
```

Outputs go to `.vx/build/` (not `target/`).

## Caching

vixen uses content-addressed storage and picante (incremental queries) to cache builds correctly:

- Second build with unchanged inputs = instant (zero rustc invocations)
- Change source, edition, or profile = rebuild
- Different checkout path = still a cache hit (`--remap-path-prefix`)
- Cache persists across sessions

Global cache lives at `~/.vx/` (or `$VX_HOME`).

## macOS FSKit host

The FSKit extension host lives in `apps/mac` as `Vixen.app`.

- App bundle id: `eu.bearcove.vixen`
- Extension bundle id: `eu.bearcove.vixenfs`
- Mount helper expected by `vx-vfs`: `/Applications/Vixen.app/Contents/Helpers/vixenfs-mount`

## Limitations (v0)

vixen explicitly rejects (with clear errors):

- Workspaces
- Dependencies
- Features
- Build scripts (`build.rs`)
- Proc macros
- Tests / benches / examples
- Multiple binary targets

## Debug logging

```bash
RUST_LOG=vx_daemon=debug vx build
```

## Tests

Ordinary Rust tests still run in nextest:

```bash
cargo nextest run
```

Full-stack daemon/VFS scenarios run through `xtask` instead of nextest:

```bash
cargo run -p xtask -- integration list
cargo run -p xtask -- integration run
cargo run -p xtask -- integration run cache-basics
cargo run -p xtask -- integration run json-output --jobs 8
cargo run -p xtask -- integration run lua --no-capture
cargo run -p xtask -- integration run json-output --keep-temp
```

The `xtask integration run` suite owns the migrated `vx build` end-to-end
scenarios that need explicit fixture setup, daemon lifecycle, and retained
artifacts. It bootstraps one shared daemon session, runs an explicit warmup
build before parallel scenarios, supports bounded parallelism with fail-fast
or `--no-fail-fast`, and reports live pass/fail/running counts.
On failure, the runner prints merged output plus retained temp/artifact
directories so logs can be inspected after teardown. `--no-capture` switches
to serial execution and streams scenario output directly for debugging, and
`--keep-temp` keeps scenario temp dirs even on success. `Ctrl-C` cancels the
run and still tears the shared daemon down.

## Co-developed with LLMs

This project was co-developed with LLMs such as Claude, Codex, etc. Human
maintainers are responsible for their contributions regardless of machine help.

## License

Vixen is licensed under [AGPL-3.0-or-later](LICENSE).

For commercial licensing inquiries, contact: amos@bearcove.eu
