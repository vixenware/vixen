# VFS Corruption Investigation

## Symptom
- `json_output_has_action_events` test fails
- Build gets to `compile hello` step, rustc is exec'd from VFS via landlock-exec
- rustc exits with code 2 (rustc's own error exit code)
- `vx: connection closed` in stderr — connection drops after rustc fails
- VFS mount point: `/tmp/.vx-test/nextest/home/vixen/runner`
- rustc path: `/tmp/.vx-test/nextest/home/vixen/runner/tc-07d390be26e7f2d7/bin/rustc`

## Hypothesis
Something wrong with the VFS is causing rustc to malfunction — either:
- Binary served from VFS is corrupted/truncated
- Shared libraries or sysroot files are missing/wrong
- File permissions or metadata are wrong

## Investigation Log

