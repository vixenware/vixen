# Vixen design TODO

## Closure serialization / executor programs
Two options for how observer closures run on executors:

**Option A: Serialize closures.** No runtime imports — the whole program is
statically known. The closure + captured values are serialized, shipped to the
executor, deserialized, and evaluated. Captures must all be resolved (not
pending) before serialization.

**Option B: Send programs, not closures.** Instead of serializing a closure,
send a self-contained Vixen program to the executor. The program is loaded and
evaluated, receiving inputs from the runtime. This means the observer can't
capture anything — it's a separate program. But it can import shared type
definitions from the same codebase, so types used by the orchestrator and
executor stay in sync.

Option A is simpler. Option B is more explicit about the boundary but
complicates exec's generic type parameter.
