# Runtime semantics — moved to the wiki

This design moved to the **served wiki** (`ddc serve vixen/wiki`), split in two so
each page is focused:

- **The demand-driven runtime** — `wiki/content/runtime-semantics.md`: the
  evaluation model, node-reference values, kernels, the cost model, `Tree`, remote
  handles, observability.
- **The strand scheduler** — `wiki/content/strand-scheduler.md`: the
  demand-driven concurrency contract (I1–I7), on stackful coroutines, the
  permit/park model, the remote-part bridge.

The `exec` synthesis moved to `wiki/content/exec.md`.

(Full prior content is in git history; this stub exists only so older links don't
dangle. New design content goes in the wiki or the Tracey spec, not here.)
