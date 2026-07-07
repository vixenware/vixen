+++
title = "Worked examples"
weight = 12
type = "reference"
updated = "2026-07-08"
+++

Three fragments from the programs vixen actually runs, read closely enough
to see the language's rules doing real work — not toy snippets written for
this page.

## A resolver fragment: feature activation

From [`rodin/rodin.vix`](https://code.vixen.rs/vixen/facet/src/branch/main/rodin/rodin.vix),
the loop that enables a batch of named features on a package and reports
whether anything actually changed:

```vix
fn enable_named_features(state: State, features: [Int], changed: Bool) -> Step {
    match features.len() == 0 {
        true => Step::Pass { state: state, changed: changed },
        false => enable_named_feature_tuple(state, features.pop(), changed),
    }
}

fn enable_named_feature_tuple(state: State, popped: (Int, [Int]), changed: Bool) -> Step {
    enable_named_feature_after_pop(state, popped.0, popped.1, changed)
}

fn enable_named_feature_after_pop(state: State, feature: Int, rest: [Int], changed: Bool) -> Step {
    match enable_feature(state, feature) {
        Step::Conflict(conflict) => Step::Conflict(conflict),
        Step::Pass { state: next, changed: feature_changed } =>
            enable_named_features(next, rest, bool_or(changed, feature_changed)),
    }
}
```

Read as a loop: pop one feature index off the list, enable it, and — if that
didn't conflict — recurse on the rest with the accumulated `changed` flag
folded in. `Step::{Pass, Conflict}` carries either the next state or a
conflict to propagate upward; see [Pattern matching](/vix/patterns/) for the
enum-payload shape and [Values and types](/vix/values/) for why `.pop()`
returns `(Int, [Int])` rather than an `Option`.

It's also an honest illustration of a real limitation: `enable_named_features`
doesn't call *itself* in tail position — it calls `enable_named_feature_tuple`,
which calls `enable_named_feature_after_pop`, which calls back into
`enable_named_features`. Three distinct functions in a cycle, not one
function recursing on itself. [Loops](/vix/loops/) only lowers **literal
self**-tail-calls to a backward jump today; this three-function cycle is
exactly the mutual-tail-recursion case that isn't covered yet, so this
production loop currently pays a full memo-boundary INVOKE per feature
rather than running as a jump. See [Honest gaps](/vix/gaps/).

## A unit-derivation fragment: `--extern` from a resolved graph

From the build-orchestration samples
(`playgrounds/snark/src/bundled/vix/samples/crate.vix`), turning a package's
resolved dependency indices into `--extern` argv entries:

```vix
fn resolved_unit(graph: ResolvedGraph, ix: Int) -> ResolvedUnit {
    graph.units.get(ix).unwrap()
}

fn direct_extern_args(graph: ResolvedGraph, deps: [Int], mode: String, out: [String]) -> [String] {
    match deps.len() == 0 {
        true => out,
        false => direct_extern_args_tuple(graph, deps.pop(), mode, out),
    }
}

fn direct_extern_args_tuple(graph: ResolvedGraph, popped: (Int, [Int]), mode: String, out: [String]) -> [String] {
    direct_extern_args_one(graph, popped.0, popped.1, mode, out)
}

fn direct_extern_args_one(graph: ResolvedGraph, dep: Int, rest: [Int], mode: String, out: [String]) -> [String] {
    let unit = resolved_unit(graph, dep);
    let arg = unit.name + "=/m/0/" + extern_file(unit, mode);
    direct_extern_args(graph, rest, mode, out.push("--extern").push(arg))
}
```

Same pop-and-recurse shape as the resolver fragment above, walking a list of
dependency indices into `graph.units` (itself an ordinary field projection —
`ResolvedGraph` and `ResolvedUnit` are plain vix structs) and building up a
flat `[String]` of alternating `"--extern"` / `"name=path"` pairs. This is
the string-argv precursor to the typed `Arg::{Str, Path, Interpolation}`
model described in [Exec nodes](/vix/exec/) — the same "one flag per
dependency" shape, expressed here as plain strings rather than lazy
interpolations.

## A directive parse: build-script `cargo:` output

From the same samples, turning a build script's captured stdout into
structured directives — the vix-side replacement for Cargo's own
`cargo:rustc-cfg=...` / `cargo:rerun-if-changed=...` line protocol:

```vix
fn cargo_directive_payload(line: String) -> String {
    let double_colon = line.strip_prefix("cargo::");
    match double_colon == line {
        true => match line.strip_prefix("cargo:") {
            single_colon => match single_colon == line {
                true => malformed_build_script_directive(line),
                false => single_colon,
            },
        },
        false => double_colon,
    }
}

fn build_script_directive_payload(payload: String, directives: BuildScriptDirectives) -> BuildScriptDirectives {
    let key = payload.before("=");
    let value = payload.after("=");
    match key == "rustc-cfg" {
        true => BuildScriptDirectives { rustc_cfg: directives.rustc_cfg.push(value), ..directives },
        false => match key == "rustc-check-cfg" {
            true => BuildScriptDirectives { rustc_check_cfg: directives.rustc_check_cfg.push(value), ..directives },
            // ... one arm per directive kind (rustc-env, rustc-link-lib,
            //     rustc-link-search, rerun-if-changed, rerun-if-env-changed,
            //     warning), falling through to a generic key=value metadata
            //     entry for anything else.
            false => BuildScriptDirectives { metadata: directives.metadata.push(metadata_directive(key, value)), ..directives },
        },
    }
}

fn parse_build_script_directive_lines(stdout: String, directives: BuildScriptDirectives) -> BuildScriptDirectives {
    match stdout.contains("\n") {
        true => parse_build_script_directive_lines(stdout.after("\n"), build_script_directive_line(stdout.before("\n"), directives)),
        false => build_script_directive_line(stdout, directives),
    }
}
```

`build_script_directive_payload` is the struct-update idiom from
[Molten values](/vix/molten/) in its natural habitat: `BuildScriptDirectives
{ rustc_cfg: directives.rustc_cfg.push(value), ..directives }` builds a new
struct value that's identical to `directives` except one field, and — unlike
the `let x = x.push(v)` rebind idiom — this one *isn't* a consuming rebind
(`directives` is a function parameter read once here, not rebound), so each
line's update goes through the ordinary construction path. `..directives` is
struct spread: every field not named explicitly is copied from `directives`.
`parse_build_script_directive_lines` is a real self-tail-call — one function
calling itself, in tail position — so, unlike the resolver fragment above,
this one *does* get the jump treatment from [Loops](/vix/loops/), and a
multi-hundred-line build script's stdout parses as one spawn, not one per
line.

One more thing worth noting honestly: `malformed_build_script_directive`
reports a malformed line by looking it up in a `Map` literal it just
constructed empty — `let missing: Map<String, String> = {}; missing.get(line).unwrap()`
— which always resolves to `None.unwrap()`. That's this program's way of
producing an attributable failure value (see
[Diagnostics](/vix/diagnostics/) — runtime failures are values in the trace,
attributed to the exact demand that produced them) without a dedicated
"raise an error" primitive in the surface language today.
