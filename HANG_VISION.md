# Hang Debugger Vision

Single command that collects all diagnostic state from the vixen process tree and presents it as a live web page.

## Collection

`vx debug` (or `vx hang`) does:

1. Sends SIGUSR1 to vxd, vx-store, vx-vfsd
2. Collects their stderr diagnostic dumps (roam-session, roam-shm, vx-locks)
3. Reads `.vixenfs/stats.json` from the VFS mount
4. Parses everything into a structured model

## Web Dashboard

Served on a local port. Shows:

### Process Graph

Visual graph of the process tree with connections:

```
┌──────────┐  roam-shm   ┌──────────┐  roam-stream  ┌──────────┐
│  FSKit   │◄────────────►│ vx-vfsd  │◄─────────────►│ vx-store │
│ (Swift)  │              │          │                │          │
└──────────┘              └──────────┘                └────┬─────┘
                                                          │
                          ┌──────────┐  roam-stream       │
                          │   vxd    │◄───────────────────┘
                          │          │
                          └────┬─────┘
                               │ roam-stream
                          ┌────▼─────┐
                          │ vx-runner│
                          └──────────┘
```

Each connection edge shows:
- Peer names (from Hello V6 metadata)
- Transport type (stream/shm)
- In-flight count / max_concurrent
- Total completed
- Age

### In-Flight Requests

Table of all stuck requests across all processes, sorted by duration:

| Duration | From → To | Method | Args | Backtrace |
|----------|-----------|--------|------|-----------|
| 2.8s | vx-vfsd → vx-store | Store.put_chunk | hash=abc123... | vfs.rs:142 → ... |
| 3.0s | vxd → vx-runner | Runner.compile_rust | crate=hello | executor.rs:89 → ... |

### Flow Control

Per-connection detail:
- Negotiated params (max_concurrent, initial_credit, max_payload)
- Client semaphore permits (available/total)
- Server in-flight count
- Channel credit state (per-channel tx/rx credit remaining)

### VFS Stats

From `.vixenfs/stats.json`:
- Read count, error count
- Byte throughput (requested vs returned)
- Latency distribution

### vx-locks State

All active locks, who holds them, who's waiting.

### SHM State

Per-channel ring buffer utilization, doorbell activity.

## Output Formats

- `vx debug` → opens browser to dashboard
- `vx debug --dump` → prints everything to stderr (current SIGUSR1 behavior, but unified)
- `vx debug --json` → structured JSON for tooling
