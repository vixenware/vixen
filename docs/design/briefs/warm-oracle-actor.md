# Brief: warm-oracle actor thread in vix-daemon

*Self-contained delegation prompt. Repo: ~/oss/facet-cc (or a worktree),
branch snark-playground-rebased. Crates: vix (evaluator), vix-daemon (vox
service). Run tests with `cargo nextest run -p vix-daemon -p vix`; keep
clippy at zero warnings; never use serde or println (tracing only); commit
early, never revert; if something doesn't work, commit and report the
negative result.*

## The problem

`vix-daemon` (src/lib.rs) evaluates each request with a FRESH
`vix::oracle::Oracle` inside `tokio::task::spawn_blocking`. So a second
"demand" click re-runs the whole build cold. The oracle is deliberately
`!Send`/`!Sync` (RefCell interior); it cannot be stored in the async
service. The fix is an ACTOR: one dedicated `std::thread` OWNS one
long-lived Oracle; the service talks to it over channels.

## Why this gives the demo we want (the anti-Nix demo)

The oracle's memo keys are `(fn_canon_hash, args_hash)` where fn_canon_hash
is the AST-modulo-spans hash (comments/whitespace don't change it). With a
warm oracle, editing a comment and demanding again produces exactly one
`Hit { func: "lua", .. }` event — the whole build is one memo hit, live in
the browser IDE. Nix rebuilds on that edit; vix shrugs.

## Design (decided — do not simplify away)

1. `Oracle::reload(&mut self, source: &str) -> Result<(), String>` in
   vix/src/oracle.rs: re-parse `source` (use the shared process-wide parser,
   see `Oracle::load`) and replace `fns`, `fn_hashes`, `enums`, `structs` —
   PRESERVING `memo`, `journal`, `exec_cache`, and the run bookkeeping.
   Memo keys are canon-hash-based, so entries stay valid across reloads:
   an untouched fn keeps hitting; an edited fn misses naturally because its
   canon hash changed. Reset `events` (`sink` is per-eval, see below) and
   `fn_stack`; keep `epoch` (or reset it — timestamps are eval-relative
   either way, pick one and document it in the code).
2. In vix-daemon: an `OracleActor` struct — `std::thread::spawn` at
   `DaemonService::new()`, owning `Option<Oracle>` (created on first
   request). Request channel: `std::sync::mpsc::Sender<EvalJob>` where
   `EvalJob { source: String, entry: String, sink: vix::oracle::EventSink,
   reply: std::sync::mpsc::Sender<Result<String, String>> }`.
   The actor loop: reload (or first-load) the source, `with_sink`-style
   install the job's sink (add an `Oracle::set_sink(&mut self, ...)` if the
   builder-style `with_sink` doesn't fit reuse), run
   `target_args_for`/`call` exactly as `eval()` does today, send the
   Debug-formatted result back.
3. `DaemonService::eval` keeps its ENTIRE existing bridge (gating,
   permits, step credits, terminal events, post-terminal channel handling)
   — only the `spawn_blocking` block is replaced by: build the same sink
   closure, send an EvalJob, `spawn_blocking` on `reply.recv()` for the
   final result. The sink closure is already `Send`; it moves to the actor
   thread instead of the spawn_blocking thread.
4. One actor per DaemonService (shared across connections — that's the
   point: warmth is daemon-global). Concurrent evals serialize through the
   actor queue; that is FINE for v1 and must be documented in a comment.

## Acceptance (all must hold)

- New test in vix-daemon/tests/eval.rs: `warm_daemon_whitespace_edit_is_one_hit`
  — eval lua.vix (run mode) once; eval again with a comment inserted into
  the source (e.g. prepend `// warm\n`); assert the second eval's events
  are EXACTLY one `Hit` for "lua" followed by `Done` (no Miss, no
  Created/Scheduled/Finished, no cold Observations).
- New test: `warm_daemon_edited_fn_reruns` — change something semantic
  (e.g. add a define to the `defines` array) and assert the second eval
  contains at least one `Miss` and at least one `Finished` with
  `Serving::Ran` (tier-2 may absorb some — do not over-assert).
- All existing vix-daemon + vix tests stay green (`--profile fast` for the
  daemon tests; default profile for the two slow vix::oracle tests).
- Step mode still gates correctly (existing test covers it).
- clippy zero warnings on vix and vix-daemon.

## Traps known in advance

- Do NOT make Oracle Send/Sync or wrap it in Mutex — the actor thread is
  the design, the !Send-ness is load-bearing.
- The sink must be installed per-eval and REMOVED after (or replaced next
  eval); events from eval N must never reach client N-1's channels.
- The journal preserves fetch/acquire pins across evals — that's wanted
  (replayed Observations on warm evals). The whitespace test asserts NO
  observations at all on the warm eval because the `lua` hit short-circuits
  above them.
- vixd (src/bin/vixd.rs) needs no changes; the service constructor change
  must keep `DaemonService: Clone` working (share the actor Sender via Arc
  or clone the mpsc Sender directly).

## Report back

Commit hashes, test output, and: does `Oracle::reload` invalidation-by-
canon-hash feel sufficient, or did you find a staleness hazard (e.g. enums/
structs changing shape while memo entries reference old variants)? Do not
fix such a hazard silently — report it with a failing test if you can
construct one.
