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

The "hermeticity" of Buck2 is also, in practice, only true when builds are executed
remotely. Local execution (on a macOS machine for example) is not, actually, hermetic.

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
