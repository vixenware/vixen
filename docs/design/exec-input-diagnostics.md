# Exec Input Diagnostics

This document describes the intended diagnostic pipeline for `build.vx` exec
failures where a command cannot see an input file that the build author
expected to exist.

The motivating example is:

- source code mounts `@src: src_tree`
- source code passes `@(src/{file})` to `cc`
- mounted tree already contains a top-level `src/`
- process tries `open("src/lua.c")`
- actual available sandbox path is `src/src/lua.c`

The current CLI can show that the build failed and where evaluation reached the
failing exec. It does not yet explain the sandbox path mismatch clearly enough.

## Product Goal

When a build command fails because an input path is missing, the user should be
able to answer all of these questions from one diagnostic:

1. Which command failed?
2. Which path did the process actually try to open inside the sandbox?
3. Which expression in my source code produced that path?
4. Was there a similar path available in the mounted inputs?
5. What is the most likely fix?

The diagnostic should feel like a build-aware source diagnostic, not a post-hoc
stderr dump.

## Desired Output Shape

The preferred default output has four layers of evidence.

1. Primary source label
   Highlight the source expression that produced the missing sandbox path, for
   example `@(src/{file})`.

2. Process fact
   Show the compiler/tool stderr summary, for example:
   `clang: error: no such file or directory: 'src/lua.c'`

3. Sandbox fact
   Show the failed VFS access, for example:
   `open("src/lua.c") -> ENOENT`

4. Build-model explanation
   Show how that sandbox path maps back to mounted inputs and source, for
   example:
   - `this exec argument expanded to src/lua.c`
   - `mount @src => /.../zoo/lua-src`
   - `mounted tree already contains top-level src/`
   - `similar available path: src/src/lua.c`
   - `did you mean src_tree.subtree(@src)?`

## Default Stack vs Full Stack

Default CLI output should prioritize the actionable error.

Default:
1. one primary error report
2. one dependency frame if it adds information
3. one build-entry frame if it adds information
4. structured notes and hints

Optional full stack:
1. the entire demand chain rendered as advice reports
2. all intermediate source-backed frames
3. all notes

CLI surface:
- `vx build`: trimmed by default
- `vx build --full-stack`: render every available frame

## Required Structured Inputs

To reach the desired output quality, the diagnostic layer needs structured data
 from three different subsystems.

### 1. Exec Source Model

Captured before dispatch to the runner:

1. program
2. argv entries
3. cwd
4. env entries that contain path-like values
5. mount table
6. writable roots
7. source provenance for each path-bearing argv/env entry

This is the bridge between source code and sandbox-visible paths.

### 2. Runner Process Result

Captured after process exit:

1. exit code
2. stdout
3. stderr
4. observer result
5. verification result

This already exists in rough form today.

### 3. Failed Sandbox Access History

Captured inside the VFS/runner:

1. a small ring buffer of recent failed ops for the action
2. operation kind (`open`, `stat`, `readlink`, etc.)
3. sandbox path
4. errno / failure kind
5. timestamp or sequence number

The intended default is the last 5 failed ops.

This is the most trustworthy source for “what path was actually missing inside
the sandbox”.

## Diagnostic Construction Pipeline

The intended pipeline is:

1. detect an exec/process failure
2. extract the most relevant missing sandbox path
3. match that path against the exec request model
4. locate the source expression that produced it
5. search mounted trees for similar available paths
6. emit one primary diagnostic with notes and conditional hints

### Step 1: Detect an Input-Missing Failure

The preferred detection order is:

1. failed VFS ops showing `ENOENT`
2. structured tool result kinds if introduced later
3. stderr parsing as fallback only

We should not rely on stderr parsing once VFS evidence is available.

### Step 2: Pick the Relevant Missing Path

Given several failed ops, prefer:

1. `open`/`stat` on relative input paths
2. paths that match a known exec argument or env path
3. the last repeated `ENOENT` if the same path failed multiple times

If no path can be identified, fall back to the generic process failure path.

### Step 3: Map Sandbox Path Back to Source

For each path-bearing exec input, preserve:

1. final sandbox-relative path
2. originating source span
3. source expression kind

Examples:
- direct relative path argument
- interpolated path template
- tree/blob auto-mount expansion
- `ExecPath`-derived path

If the missing sandbox path matches one of those final paths exactly, the
primary label should be placed on that originating source span.

If no exact match exists, keep the current fallback:
- label the enclosing exec call or dependency expression

## Similar-Path Analysis

This is the “did you mean …” layer.

Inputs:
1. missing sandbox path
2. exec mount table
3. mounted tree contents, or a bounded candidate listing derived from them

The analysis should look for:

1. duplicated leading segment patterns
   - missing `src/lua.c`
   - available `src/src/lua.c`

2. mount-point off-by-one patterns
   - mount target `src`
   - mounted tree root already contains `src/...`

3. basename matches under nearby prefixes
   - missing `include/lua.h`
   - available `src/include/lua.h`

4. exact suffix matches with one extra leading segment

Hints should only be emitted when the confidence is high enough that the result
is likely actionable rather than noisy.

## Proposed Hint Wording

Preferred wording:

1. `this exec input expanded to src/lua.c, but that path does not exist in the sandbox`
2. `recent failed sandbox access: open("src/lua.c") -> ENOENT`
3. `similar available path: src/src/lua.c`
4. `mount @src points at a tree that already contains a top-level src/`
5. `did you mean src_tree.subtree(@src)?`

Avoid wording that blames the observer when the process itself failed first.

Bad:
- `failed to evaluate shipped observer`
- `this expression explicitly failed`

Better:
- `build command input not found`
- `this command depends on an input file that was not present in the sandbox`

## Incremental Implementation Plan

### Phase 1: Immediate Improvements Without VFS History

Ship now:
1. stop leading with observer-blame wording when the process already exited
2. detect missing process inputs from stderr as a fallback
3. render those failures as input-missing diagnostics
4. trim default stack output and add `--full-stack`

This phase is imperfect but materially better than the current output.

### Phase 2: VFS-Backed Missing-Path Facts

Add:
1. last failed VFS ops to `RunResult`
2. propagation through `vxd`
3. rendering as structured notes

This removes the need to trust compiler stderr as the primary source of truth.

### Phase 3: Exec Input Provenance

Add:
1. provenance per path-bearing exec arg/env entry
2. final sandbox path per entry
3. matching logic from failed VFS path back to source

This enables precise source labels on `@(src/{file})` instead of only the
enclosing exec call.

### Phase 4: Similar-Path / Mount Analysis

Add:
1. mounted-tree similarity search
2. duplicated-prefix detection
3. targeted hints

This is where the “did you mean `src_tree.subtree(@src)`?” quality bar becomes
possible.

## Non-Goals

1. Do not parse arbitrary compiler stderr forever if the runner can provide
   direct filesystem facts.
2. Do not print the full demand chain by default.
3. Do not emit speculative hints without a strong mount/path basis.
4. Do not conflate process failure with observer failure.

## Acceptance Standard

For the motivating `src/src/...` case, the finished system should make the root
cause obvious in one screen, without requiring the user to manually inspect the
workspace tree after the fact.
