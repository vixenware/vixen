# `vx-daemon` Config Draft

This is draft design material for hosting and wiring services inside one
`vx-daemon` process.

The canonical location for this file is user-level node config, not
per-project config.

Current implemented shape:

- service blocks under `daemon.services` mean that service is hosted
- omitted dependency refs default to in-process when the required hosted service
  exists
- explicit remote deps use `@rpc{address "..."}`
- client commands may resolve the daemon address from:
  1. explicit `--daemon`
  2. explicit `--config path/to/file.styx`
  3. `VX_DAEMON_CONFIG`
  4. user config at `XDG_CONFIG_HOME/vx/daemon.styx` or `~/.config/vx/daemon.styx`
  5. fallback `local:///tmp/vixen.vox`

These examples are implementation-facing, not normative spec text.

## Local All-In-One

See [daemon-local-all-in-one.styx](/Users/amos/bearcove/vixen/docs/design/examples/daemon-local-all-in-one.styx).

This hosts:

- `Orchestrator`
- `Store`
- `Runner`
- `Vfsd`

and relies on default in-process wiring.

## Split Orchestrator / Store / Runner

See [daemon-orchestrator-remote-store-runner.styx](/Users/amos/bearcove/vixen/docs/design/examples/daemon-orchestrator-remote-store-runner.styx).

This shows:

- local `Orchestrator`
- remote `Store`
- remote `Runner`
- no local `Vfsd`

The important model point is that co-hosting does not erase service identity.
Even when services run in one process, they are still wired through explicit
service references.
