# VFS Corruption Investigation

## Symptom
- `json_output_has_action_events` test fails (and other json_output tests, and multi_crate tests)
- Build gets to `compile hello` step, rustc is exec'd from VFS via landlock-exec
- rustc exits with code 2 (rustc's own error exit code)
- `vx: connection closed` in stderr — connection drops after rustc fails
- NOT JSON-specific: non-JSON tests (cache_basics, multi_crate) also fail
- VFS mount point: `/tmp/.vx-test/nextest/home/vixen/runner`
- rustc path: `/tmp/.vx-test/nextest/home/vixen/runner/tc-07d390be26e7f2d7/bin/rustc`

## Hypothesis
Something wrong with the VFS is causing rustc to malfunction — either:
- Binary served from VFS is corrupted/truncated
- Shared libraries or sysroot files are missing/wrong
- File permissions or metadata are wrong

## Changes made so far

### 1. spawn_binary: inherit only stderr, not stdout
- `crates/vx-io/src/spawn.rs`: in VX_TEST mode, stdout→null, stderr→inherited
- Prevents spawned services from corrupting `--json` stdout
- Previously both stdout and stderr were inherited

### 2. Removed VX_TEST removal in JSON mode
- `crates/vx/src/main.rs`: removed the `unsafe { std::env::remove_var("VX_TEST") }` block
- JSON mode no longer clears VX_TEST before spawning daemons
- This means JSON and non-JSON tests can share the same daemon processes

### 3. init_service now writes to stderr
- `crates/vx-tracing/src/lib.rs`: added `.with_writer(std::io::stderr)` to `init_service`
- Previously defaulted to stdout (tracing_subscriber default)

### 4. Setup script: redirect daemon stderr to log file + tail
- `tests/start-vxd-for-tests.sh`: `vx start 2>/tmp/vx-test.log` then `tail -f &`
- Goal: see runner traces during test execution

## Blocking issue: can't see rustc's actual error
- Runner captures rustc stdout/stderr via pipes into `combined_output`
- But we never see what rustc actually complained about
- Runner's own tracing goes to stderr, but:
  - Runner is a child of vxd, which is a child of `vx start`
  - `vx start` exits after ping → children die (ur_taking_me_with_you)
  - Wait: this is how it always worked. Setup script ran `vx start` foreground too.
- Runner log file `/tmp/.vx-test/nextest/home/runner.log` has traces up to rustc spawn but no rustc output
- The runner doesn't log `combined_output` content at trace level when rustc fails

## Filed issues
- https://github.com/bearcove/vixen/issues/246 — hermetic_env should be platform-aware and action-aware

## Investigation Log
- RUST_LOG=trace not showing runner traces during test execution
- Need to figure out: where does rustc's actual error go?
- Next step: need to actually see what rustc prints when it fails
