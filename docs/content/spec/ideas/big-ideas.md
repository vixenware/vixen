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

### Baby steps

The first step to doing better is to do worse, and so I'm going to show you a
Vixen program that is intentionally very bad, mirroring the shell script we saw
above:

```vixen
fn build(params: BuildParams) -> Tree (
  exec ["clang", "main.c", "-o", "main"]
)
```

We're defining a function called build that takes build params; that's the entry
point to your vixen program. It returns a Tree, which for now you can think of
as a directory. 

If you try to run this in Vixen, you will immediately encounter an issue:
`clang` is not found.

And that's because every exec in Vixen, be it local or remote, runs inside of a
sandbox. If you want to run Clang, you first have to discover information about it:

It might look something like this:

```vixen
fn find_clang(class: ExecutorClass) -> Map[String, String] {
  discover(class, () => (
    let clang = host.which "clang";
    let clang_hash = host.blake3 clang;
    %{ clang, clang_hash }
  ))
}
```

Which you could then use like so:

```vixen
fn build(params: BuildParams) -> Tree (
  let toolchain = find_clang(default_exec_class());
  exec [toolchain.clang, "main.c", "-o", "main"]
)
```

The problem here: hashing the clang executable is actually not enough. System
headers and libraries can change. From underneath it: we might be over-caching,
which would produce incorrect builds.

On the other hand, it is often impractical to hash the entire toolchain for
performance reasons, and it is impractical to provide the entire toolchain, for
legal reasons. That's why code like this is better left to packages that would
use strategies like looking at the operating system version and the SDK version,
etc.

However, that code still doesn't actually run: we now have access to the entire
toolchain inside of the sandbox (TODO: why? How is it magically mounted? which
paths, and through which mechanisms?), but not our input files.

That's where `BuildParams` comes in: it gives us a `Path` corresponding to the
root of our workspace:

```
workspace/
  build.vx
  src/
    main.c
    util.h
```

```vixen
fn build(params: BuildParams) -> Tree (
  let toolchain = find_clang(default_exec_class());
  let src = @({params.workspace}/src);
  exec [toolchain.clang, @src/main.c, "-o", "main"]
)
```

However, this still doesn't work because main.c actually includes util.h!

No worries, we can mount the entire src directory:

```vixen
fn build(params: BuildParams) -> Tree (
  let toolchain = find_clang(default_exec_class());
  let src = @({params.workspace}/src);
  exec [toolchain.clang, @src/main.c, "-o", "main"] where {
    mounts: %{ src } // sugar for `@src => src`
  }
)
```

Next problem: the returned tree is actually empty (.vx/out is empty after `vx
build`). That's because by default, only the contents of the out folder are
returned.

Easy fix:

```vixen
fn build(params: BuildParams) -> Tree (
  let toolchain = find_clang(default_exec_class());
  let src = @({params.workspace}/src);
  exec [toolchain.clang, @src/main.c, "-o", @out/main] where {
    mounts: %{ src }
  }
)
```

### Local is just another remote

Now that we have declared all our inputs (`@src`), all our outputs (`@out`), and
which toolchain to use to run the compilation, that compilation can run anywhere.

Right now, because we used `default_exec_class()`, it's running... wherever
the default is, which for `vx build`, would probably be "the local machine": in
my case, a macOS laptop.

But `vx build` could accept a `--prefer-remote` knob, changing the default
executor, and then, it would connect to a `vxd` running on that machine, transfer
the `discover` closure so we can gather information about clang, and then run
the `exec` on it — mounting a VFS with the inputs (streamed from the local host)
and collecting the outputs on completion.

Remote execution isn't special in Vixen. All execution is remote, with "local"
being the closest remote (and benefitting from a few performance improvemenst).

### This isn't imperative code

Looking at the build function, it would be natural to try and read it from top
to bottom and imagine in which order things are being executed. 

If that were true, then this code would fail:

```vixen
fn bomb() -> Tree {
  fail "should not run"
}

fn build(params: BuildParams) -> Tree (
  let toolchain = find_clang(default_exec_class());
  let src = params.workspace / @src;
  
  // 💣
  let unused = bomb();
  
  exec [toolchain.clang, @src/main.c, "-o", @out/main] where {
    mounts: %{ src }
  }
)
```

Because upon reaching the call to bomb, fail would be evaluated, which would
poison the entire computation, and then we would never get to the end result. 

But because it's never actually used to produce the end result, it's never
actually evaluated. You can think of it as dead code elimination, but kind of
hardcore. 

What's happening instead is starting from the the command line invocation
demands the return value of build, therefore we need the tree produced by exec.
Therefore we need to evaluate exec. Therefore we need to evaluate all its
arguments. Therefore we need to evaluate src, which and we need to evaluate
toolchain.clang, which requires toolchain, which requires find clang, which
requires default exec class, etc., etc. And we have this tree of things that we
need to evaluate, and `unused` is in nowhere in that tree, so it never gets
evaluated.

Simple. 

### Implicit concurrency

This same demand-driven model is where Vixen's concurrency comes from.

There is no `parallel` keyword here:

```vixen
fn build(params: BuildParams) -> Tree (
  let toolchain = find_clang(default_exec_class());
  let src = params.workspace / @src;

  let main_o =
    exec [toolchain.clang, "-c", @src/main.c, "-o", @out/main.o] where {
      mounts: %{ src }
    };

  let util_o =
    exec [toolchain.clang, "-c", @src/util.c, "-o", @out/util.o] where {
      mounts: %{ src }
    };

  let log_o =
    exec [toolchain.clang, "-c", @src/log.c, "-o", @out/log.o] where {
      mounts: %{ src }
    };

  exec [
    toolchain.clang,
    main_o,
    util_o,
    log_o,
    "-o",
    @out/main,
  ]
)
```

But the three compilation steps are independent from one another. The final
link step needs all three object files, so demand reaches all three of them.
Once that happens, the runtime is free to progress them concurrently, and only
then proceed to the final link.

This scales naturally:

```vixen
fn build(params: BuildParams) -> Tree (
  let toolchain = find_clang(default_exec_class());
  let src = params.workspace / @src;

  fn compile_one(name: String) -> Tree (
    exec [
      toolchain.clang,
      "-c",
      @(src/{name}.c),
      "-o",
      @(out/{name}.o),
    ] where {
      mounts: %{ src }
    }
  );

  let objects =
    ["main", "util", "log"]
      .map(compile_one);

  exec [toolchain.clang, ..objects, "-o", @out/main]
)
```

This is why Vixen's parallelism is not just "run a hand-written build graph
with a thread pool". It falls out of the evaluation model itself:

  * dead work is never entered
  * shared work only needs to be entered once
  * independent demanded work can progress concurrently

Source order does not force execution order. Demand does.

### What gets reused

At this point, we have seen that Vixen does not evaluate everything eagerly,
and that independent demanded work can progress concurrently.

The next question is: if a computation is needed more than once, do we do it
more than once?

The answer is no.

Within a single evaluation, Vixen reuses computations that have already been
entered instead of re-entering them from scratch.

That means if two different parts of the build depend on the same subtree, the
same toolchain discovery, or the same generated file, the runtime does not need
to duplicate that work just because it was demanded from two places.

That is local reuse inside one build.

```vixen
fn object_file(class: ExecutorClass, src: Path, name: String) -> Tree (
  let toolchain = find_clang(class);

  exec [toolchain.clang, "-c", @(src/{name}.c), "-o", @(out/{name}.o)] where {
    mounts: %{ src }
  }
)

fn build(params: BuildParams) -> Tree (
  let class = default_exec_class();
  let src = params.workspace / @src;

  let main_o = object_file(class, src, "main");
  let util_o = object_file(class, src, "util");

  exec [find_clang(class).clang, main_o, util_o, "-o", @out/main]
)
```

Here, `find_clang(class)` is reached from three different places: once for each
compile step, and once for the final link step. But because all three are
asking for the same computation with the same input, the runtime can share it
instead of rediscovering Clang each time.

But Vixen also wants reuse across different builds.

If you build the same thing tomorrow, on the same code, with the same inputs,
with the same discovered toolchain facts, then ideally Vixen should not repeat
all the work. It should reuse the results of computations that are still
justified.

That includes obvious things like compiler invocations, but it also includes
more than that:

  * discovered facts about toolchains and executors
  * generated trees
  * package-resolution subcomputations
  * any other pure or boundary-crossing computation whose inputs are the same

So the guiding idea is not "cache processes". It is "reuse computations".

That is why Vixen is demand-driven in the first place. The runtime is not just
trying to run actions. It is trying to identify computations, understand what
they depend on, and avoid repeating work whose justification has not changed.

### When does work have to run again?

A computation has to run again when something it depends on has changed.

For a pure local computation, that means its arguments changed, or the code of
the computation changed.

For an `exec`, that means things like:

  * the executable path changed
  * the command line changed
  * the mounted inputs changed
  * the environment changed
  * the discovered toolchain facts it depends on no longer validate

And because Vixen runs processes against a virtual file system, we also get
some things that are unusually hard in other build systems almost for free.

```vixen
fn build(params: BuildParams) -> Tree (
  let src = Tree(params.workspace / @src);

  let has_config =
    match src.file(@config.h)? (
      Some { value: _ } => "yes\n",
      None => "no\n",
    );

  let plugins = src.glob(@plugins/*.c);

  Tree %{
    @has-config.txt => Blob(has_config),
    @plugins => plugins,
  }
)
```

If a computation tried to read a file and that file was absent, that absence is
itself part of what was observed. If that file appears before the next build,
then the computation is no longer justified and has to run again.

Similarly, if a computation globbed for `*.c` in some directory, then the set of
matching files is part of what it depended on. Adding or removing a matching
file invalidates that computation too.

So the question is not "did we call this function before?"

The question is "is this still the same computation, justified by the same
inputs and the same discovered facts?"

If yes, Vixen should reuse it.

If no, Vixen should run it again.

### Why Vixen needs analysis

At this point, Vixen is very far from "read the program top to bottom and do
what it says".

The source code is not the execution plan.

Instead, Vixen has to analyze the program as a graph of computations.

That analysis answers questions like:

  * which computations are actually demanded by the final result
  * which computations are shared
  * which computations are independent
  * which computations cross explicit boundaries like `exec` or `discover`
  * which observed facts justify reusing a previous result

Without that analysis, Vixen would collapse back into a line-by-line
interpreter, which is the wrong execution model for this language.

This is also how Vixen can be efficient.

```vixen
fn build(params: BuildParams) -> Tree (
  let toolchain = find_clang(default_exec_class());
  let src = params.workspace / @src;

  let input = @src/main.c;
  let output = @out/main.o;
  let args = [
    toolchain.clang,
    "-c",
    input,
    "-o",
    output,
  ];

  exec args where {
    mounts: %{ src }
  }
)
```

Some parts of a program are just ordinary local computation: building argument
lists, selecting flags, constructing paths, combining arrays, choosing between
branches, and so on.

Other parts are real boundaries: discovering host facts, reading the
filesystem, running a process in a sandbox, validating previously discovered
assumptions, and returning results across an executor boundary.

The local parts can be analyzed and compiled into efficient straight-line
kernels.

The boundary-crossing parts must remain explicit runtime-managed nodes, because
they are exactly where Vixen's correctness story lives.

In the example above, everything up to `exec args` is ordinary local
computation. Constructing `input`, `output`, and `args` should not require
handing control back to the runtime after every tiny step. The `exec` itself,
however, is a real boundary, so that is where the runtime has to take over
again.

So analysis is not an implementation detail bolted on afterward. It is the
thing that lets Vixen preserve its semantics while still being practical:

  * precise reuse
  * implicit concurrency
  * explicit boundaries
  * efficient execution

### Kernels and boundaries

At that point, the runtime should not have to take control back after every
tiny local step.

```vixen
fn build(params: BuildParams) -> Tree (
  let toolchain = find_clang(default_exec_class());
  let src = params.workspace / @src;

  let flags =
    if toolchain.clang.contains("clang") (
      ["-Wall", "-O2"]
    ) else (
      ["-Wall", "-O2", "-fPIC"]
    );

  let args = [
    toolchain.clang,
    "-c",
    ..flags,
    @src/main.c,
    "-o",
    @out/main.o,
  ];

  exec args where {
    mounts: %{ src }
  }
)
```

Everything up to `exec args` is ordinary local computation.

It should be possible to run that whole region in one shot, producing the final
command line and mount table, instead of bouncing through the runtime after each
`let`, each branch, and each array construction.

That is what kernels are for.

A kernel is an analyzed region of local Vixen computation that can be executed
straight-line.

What matters here is the semantic split, not the exact backend:

  * local computation can run in one shot
  * boundaries like `exec` and `discover` stay explicit
  * the runtime only has to take control back at those boundaries

An implementation might lower those kernels first to a compact bytecode, and
later to machine code. That choice is important for engineering, but it does
not change the basic shape of the system.

The important part is that Vixen does not need to choose between "fully
interpreted" and "lie about the semantics". It can keep the demand-driven,
boundary-aware model, while still compiling ordinary local regions into
efficient kernels.

### There's more

Things we haven't talked about yet. We haven't talked about, well, remote
execution in much detail. Of course, we haven't talked about like laziness in
general, like demand-driven evaluation. We specifically haven't talked about
join and key, and I need to fucking look up how it actually works. Oh, we
haven't talked about deserialization.
