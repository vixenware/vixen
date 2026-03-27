+++
title = "Big Ideas"
insert_anchor_links = "heading"
weight = 10
+++

# Vixen Big Ideas

This page is an overview, not a normative rulebook. The goal is to explain
what Vixen is for and why the rest of the specification looks the way it does.

> v[ideas.overview]
>
> Vixen is a lazy, demand-driven language for build and execution
> orchestration. Its design centers on explicit values, explicit boundaries,
> deterministic results, and a runtime that can eventually analyze authored
> Vixen and compile straight-line kernels without changing language semantics.

## Build systems

### A shell script

A shell script lets you run a command that will compile some file, some C file, into an executable:

```shell
#!/bin/bash -eux
clang main.c -o main
```

This is not a very good build system. It runs no matter whether main.c changed or not.

### Make

Systems like make let you define rules that explain how to produce an output:

```make
main:
  clang main.c -o main
```

But oops, we forgot to specify that main depended on main.c:

```make
main: main.c
  clang main.c -o main
```

And oops! Actually, it doesn't just depend on main.c — it also depends on many
other inputs that are easy to leave undeclared, like headers, compiler flags,
generated files, environment variables, link inputs, and the compiler itself.

Another oops: we can change the contents of main.c, as long as the timestamp is
the same, make is just not gonna notice any difference or rerun the compiler.

In general, systems like make give you local incremental rebuild based mostly
on declared edges and file timestamps. And even then, it's very frequent to
just clean everything because the build was corrupted in some way, and you hope
to God that you detected the failure.

### CMake + Ninja

CMake is one of the many attempts at solving the problem of oh, actually, we
might not be using clang to build C. The platform we're on might have an
executable suffix like .exe. It might name its libraries funny. The compiler
flags are not at all the same. The way to find if you have a library or to link
against the library are completely different.

Someone has to take care of this, and CMake is one of those someones. CMake
projects often use a configure phase to probe the environment, make
project-specific decisions, and generate a build graph using backends like
Ninja.

Ninja is the other half of that split. It is not trying to be a full build
language. It is trying to be a low-level explicit build graph format and fast
executor, with support for features such as discovered dependencies, multiple
outputs per edge, and rebuilds when the generating command line changes.

So the responsibilities split here is:

  * Phase 1: a CMake configure step probes the environment, determines what
    features are going to be enabled or disabled with which compiler against
    which dependencies, etc.
  * Phase 2: Ninja runs the compiler in parallel as fast as possible, giving an
    incorrect build if the dependency edges were underspecified, and completely
    unaware of a whole category of changes that would require reconfiguring
    (like a new source file, turning a feature on/off, etc.)
  
### Buck2

Buck2 brings several improvements on the CMake + Ninja combo: it lets you define
rules in the Starlark language, it lets you execute them in a hermetic environment
(quote, "Missing dependencies are errors"), and it caches build results.

The speed of Buck2 comes from different sources. One of them is that it has a
persistent daemon running. That daemon is able to react to file system change
events, which means it doesn't have to rescan the disk for changed mtimes or
file hashes.

However, even though Buck2 has [dynamic
dependencies](https://buck2.build/docs/rule_authors/dynamic_dependencies/),
building a Rust project with regular crates.io dependencies is impractical without
the use of a separate tool (reindeer) run ahead of time, which invokes cargo
and translates its lockfiles to BUCK files.

Buck2's hermeticity story is, in practice, a remote-execution story. Local
execution, on a macOS machine for example, is not actually hermetic.

### Cargo

Cargo is a mix of a package manager and a build tool.

Its power is its convenience and ubiquity, but it is not a principled build
system. It is not hermetic. It does not do precise caching. Its support for
cross compilation is minimal.

One of the main day-to-day annoyances of using Cargo is under-caching: turning a
cargo feature on or off will routinely overwrite parts of the cache, instead of
keeping it to the side for when you do that same build again. 

(Switching back and forth from workspace builds to `-p pkg` builds makes this
problem painfully obvious).

Cargo's solution for cross-language builds is `build.rs`, where arbitrary Rust
code is compiled and executed, and has full access to the entire environment,
the entire host, and can do anything unchecked.

If a build script forgets to declare even one of its dependencies, which is
extremely easy to do, given that they inherit from the ambient environment and
can call arbitrary processes, usually provided by the system, then cargo
exhibits problems of over-caching: re-using the previous build, even though it
should have actually rebuilt it.

### Doing better

The tools we have seen so far cover many different jobs.

Configuration, which involves discovering facts about hosts and targets,
selecting features and options, and determining which tools and
libraries are available.

Resolution, which involves starting from the package requests in a Cargo
manifest and recursively selecting concrete package versions from a
registry, using algorithms such as PubGrub to satisfy all version
constraints.

Scheduling, which decides in which order and where to run the many different
steps that a build is made up of. 

Execution, which involves actually running processes (compilers, linkers,
various binutils, etc.) in an executor with the correct inputs, ideally denying
access to undeclared inputs, and retrieving the outputs.

Caching, which tries to record decisions made or the results of
actions, keyed on everything that went into those decisions or actions, so
that they may be reused in subsequent builds, whether on the same host or a
different one.

### Doing worse

The first step to doing better is to do worse, and so I'm going to show you a
Vixen program that is intentionally very bad:

```vixen
exec ["clang", "main.c", "-o", "main"]
```

(This is a function application. We're applying an array to a function called exec.)

This is problematic in a number of ways. For example, where does clang come
from? Is it from the `PATH` env variable? How do we know to rerun compilation
(and change the cache key) when clang changes?

Same questions for `main.c`: is it a local file? If yes, say it's local to my
macOS machine, I want to build on a remote Linux server, how is `main.c` sent over
there? How is `main` sent back to me?

And if it's on a remote machine, how do we know that they have clang? How do we
make sure we don't makes things that run here and things that run there?

Vixen has answers for all of these.

First of, a `build.vx` file is not a naked expression. It exports a build function,
which takes build parameters.

```vixen
fn build(params: BuildParams) (
  // body goes here
)
```

Those include:

  * Command-line arguments, environment variables, and a path to the workspace.
  
So if we have:

```
my-project/
  build.vx
  src/
    main.c
    main.h
```

Then `vx build` will have `params.workspace` set to `my-project/` and we can
build a path to `main.c` from it:

```vixen
fn build(params: BuildParams) (
  let main_c = @({params.workspace}/src/main.c); // path literal, with interpolation
  exec ["clang", main_c, "-o", "main"]
)
```

However, this will fail because `main.c` depends on `main.h`, and thus, we actually want to
mount all of `src`

```vixen
fn build(params: BuildParams) (
  exec ["clang", @(src/main.c), "-o", "main"] where {
    mounts: %{ @(src) => @({params.workspace}/src) }
  }
)
```

Regarding the output,
