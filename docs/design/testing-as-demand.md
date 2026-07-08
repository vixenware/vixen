# Testing as demand (Amos, 2026-07-08: vixen takes over testing)

Not an integration with the compiler's view of tests — a takeover of testing itself.

## Why compiler-graph selection is blind by construction
rustc's dependency graph ends where Rust ends. It cannot see: native libraries a test
links (a bumped .a/.so is invisible), runtime test I/O (fixtures, config, env, network,
dlopen), build-script effects on test behavior. Any selection scheme keyed on compiler
dependencies is wrong precisely on the inputs that break tests in practice, and cannot be
fixed from inside the compiler — the compiler is the wrong observer.

## The two right observers (both already landed)
1. **Artifacts**: reloc-walk reachability over compiled binaries (native sections
   included), panic-Location masking, conservative Unknown→run. FOLDED.
2. **Runtime truth**: THE TEST RUN IS AN EXEC NODE. Executed under the VFS, every test's
   actual reads land in an observed read-set — selection keyed on what tests demonstrably
   consumed. A test that reads a fixture is invalidated by that fixture. No guessing.

## What "taking over testing" means
Tests are units: scheduled, memoized, receipted, selected like compiles — one machine.
- SELECTION: invalidation of test-units by observed inputs (artifact reach ∪ runtime reads).
- SCHEDULING over time and across runners: the same demand machinery + capability
  placement; compilation and testing interleave by demand, not by phase (a test runs the
  moment its unit's inputs exist — rmeta-pipelining logic applies to tests too).
- MEMOIZATION: an unchanged test-unit with unchanged observed inputs is a cache hit — "we
  already know this passes" with a receipt, not a skip heuristic. Audit mode = re-run
  memoized tests statistically and verify.
- FLAKINESS becomes measurable: nondeterministic read-sets / outcomes under identical
  inputs are detected by the machine, not folklore.
- Per-test resource watchdogs (memcap precedent) and progressive results (publish/notify
  as standing demands) are the same machinery.
Runner half-solutions (sharding, retries, JUnit exports) are scheduling patches on a
pipeline that cannot see; with observation, most of them dissolve.
