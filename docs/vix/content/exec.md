+++
title = "Exec nodes"
weight = 6

[extra]
updated = "2026-07-08"
+++

`exec` is a permanent primitive — one of the few places the machine, not the
language, defines behavior, because running a real external process is
exactly the kind of effect that has to be mediated for its observations to be
tracked at all. Command templates (`rustc!{...}`, `build_script!{...}`,
`cc!{...}`) are vix's surface over it: they read like invoking a tool, and
lower to an `EXEC_HOST` graph node that's demand-spawned like anything else —
constructing one computes nothing until its result is demanded.

## The shape

```vix
pub fn argv_only(target: Target) -> [Arg] {
    let rustc = rustc_for(target);
    [
        Arg::Str("--extern"),
        Arg::Str("dep="),
        Arg::Interpolation { tree: dep(rustc), subpath: p"libdep.rmeta" },
    ]
}

pub fn build_stdout(target: Target) -> String {
    let rustc = rustc_for(target);
    let build_script_binary = rustc! {
        --crate-name build_script_build
        --crate-type bin
        --emit=link={p"build_script"}
    };
    let run = build_script! {
        --executable {build_script_binary / p"build_script"}
        --stdout {p"build.stdout"}
        --out-dir {p"out"}
    };
    run.text(p"build.stdout")
}
```
— `vix/tests/argv_arg.rs`. `rustc!{...}` builds a binary; the result is a
`Tree` (its output directory), projected with `/` to reach a specific file;
`build_script!{...}` runs that binary hermetically and captures its outputs
as another `Tree`; `.text(path)` reads a single file leaf out of a tree as a
`String`.

## Argv elements are an enum, not strings

Every argument in an exec's command line is one of three shapes:

- **`Arg::Str(s)`** — a literal token, known up front.
- **`Arg::Path(p)`** — a path, subject to the same join-only capability rules
  as any other `Path` (see [Values and types](/vix/values/)).
- **`Arg::Interpolation { tree, subpath }`** — a reference to a *subfile* of
  another build step's output tree. This is the important one:
  **an `--extern` argument is a graph edge wearing an argv costume.**
  Constructing an `Arg::Interpolation` records nothing by itself — building
  the argv list forces nothing. Only when the exec's own result is demanded
  does the interpolation get walked: at that point it creates the dependency
  edge to the producing node, declares the referenced subfile as an input,
  and grants the sandbox a mount for exactly that subpath. Lazy all the way
  down, exactly like everything else in the language.

The payoff isn't just avoiding eager evaluation — it's **granularity**. A
consumer whose argv only interpolates a producer's `.rmeta` subfile reads
only that subfile's content identity, not the whole output tree's. If the
producer's other outputs change but the `.rmeta` doesn't, the consumer's
read-set is untouched and it stays a memo hit — early cutoff at the subfile
level, falling directly out of the argv grammar rather than requiring a
separate pipelining feature. Variable-length computed argv (an arbitrary
number of dependencies producing an arbitrary number of `--extern` flags) is
expressed as an ordinary `[Arg]` built with `.map()`/`.push()` — no special
syntax for "n things need n flags."

## Hermetic by construction

Every exec declares its inputs (the argv's paths and interpolations) up
front; the sandbox stages exactly those, nothing else. An undeclared read —
touching a file the exec didn't declare — fails loudly rather than silently
poisoning the cache. This is what makes a cache hit a proof rather than a
hope: the machine isn't trusting that a build step's declared inputs are
complete, it's enforcing that they are.

## Where this sits among the builtins

`exec` (via these command templates) is squarely in the **permanent
primitive** category — see [Builtins](/vix/builtins/) for the full breakdown
of what's staying a machine primitive forever versus what's migrating into
ordinary vix or [snark](/dev/constellation/snark/) grammars.
