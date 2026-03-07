# Vixen Sketches: Porting Bazel Patterns

This document takes real Bazel patterns and sketches how the equivalent build
logic would look in Vixen.

Bazel is the most interesting comparison because it already has a non-Turing-
complete rule language (Starlark), a graph-based execution model, and typed
provider-based information flow between rules. Vixen shares several of these
goals but with a very different surface.

## Assumed Domain Types

These sketches extend the C++ types from the CMake port and add Bazel-specific
concepts like providers and configurable attributes.

```text
struct CppProject {
  targets: [CppTarget],
  config: BuildConfig,
}

struct CppTarget {
  name: String,
  kind: TargetKind,
  sources: [SourceFile],
  headers: [SourceFile],
  deps: [DepRef],
  compile_flags: [String],
  include_dirs: [String],
}

enum TargetKind {
  Executable,
  StaticLib,
  SharedLib,
  HeaderOnly,
}

struct SourceFile {
  path: String,
}

struct DepRef {
  target_name: String,
}

struct BuildConfig {
  platform: Platform,
  profile: String,
  target_triple: String,
}

enum Platform {
  Linux,
  MacOS,
  Windows,
}

// Bazel-style proto compilation input
struct ProtoTarget {
  name: String,
  src: SourceFile,
  deps: [DepRef],
  language: ProtoLanguage,
}

enum ProtoLanguage {
  Cpp,
  Python,
  Go,
}

// Bazel-style test target
struct TestTarget {
  name: String,
  sources: [SourceFile],
  deps: [DepRef],
  size: TestSize,
  timeout: Option<U32>,
  data: [SourceFile],
}

enum TestSize {
  Small,
  Medium,
  Large,
  Enormous,
}
```

## Example 1: cc_library with deps

### Bazel

```python
cc_library(
    name = "logging",
    srcs = [
        "logging.cc",
        "log_sink.cc",
    ],
    hdrs = [
        "logging.h",
        "log_sink.h",
    ],
    deps = [
        "//third_party/abseil-cpp/absl/strings",
        "//third_party/abseil-cpp/absl/status",
        "//third_party/abseil-cpp/absl/time",
    ],
    copts = ["-std=c++20"],
    visibility = ["//visibility:public"],
)
```

### Vixen

```text
fn plan_cc_library(target: CppTarget) -> Unit {
  object_ids = target.sources.map((src) => {
    id = format::compile_id(target.name, src.path)

    emit::node {
      id,
      kind: Action,
      label: `cc ${src.path}`,
      metadata: {
        target_name: target.name,
        source: src.path,
        flags: target.compile_flags,
        include_dirs: target.include_dirs,
      },
    }

    id
  })

  archive_id = format::archive_id(target.name)

  emit::node {
    id: archive_id,
    kind: Action,
    label: `ar ${target.name}`,
    metadata: {
      target_name: target.name,
      output_type: "static_lib",
    },
  }

  object_ids.each((oid) => emit::edge {
    from: oid,
    to: archive_id,
    kind: Always,
  })

  target.deps.each((dep) => emit::edge {
    from: format::archive_id(dep.target_name),
    to: archive_id,
    kind: IfChanged,
  })
}
```

What this shows:

- Bazel's declarative `cc_library(...)` call is data, not logic. In Vixen, the
  equivalent data arrives as a typed `CppTarget`. The rule function is the
  planning logic that Bazel's built-in `cc_library` rule implementation does
  internally.
- Bazel hides the compile-then-archive pipeline inside its rule implementation.
  Vixen makes it explicit as graph nodes and edges.

## Example 2: Configurable attributes with select()

### Bazel

```python
cc_library(
    name = "platform",
    srcs = ["common.cc"] + select({
        "@platforms//os:linux": ["linux.cc"],
        "@platforms//os:macos": ["macos.cc"],
        "@platforms//os:windows": ["win32.cc"],
    }),
    hdrs = glob(["include/**/*.h"]),
    deps = select({
        "@platforms//os:linux": ["//third_party:epoll"],
        "@platforms//os:macos": ["//third_party:kqueue"],
        "@platforms//os:windows": ["//third_party:iocp"],
        "//conditions:default": [],
    }),
)
```

### Vixen

```text
fn plan_platform_lib(target: CppTarget, config: BuildConfig) -> Unit {
  sources = target.sources.concat(platform_sources(config))
  deps = target.deps.concat(platform_deps(config))

  sources.each((src) => {
    id = format::compile_id(target.name, src.path)

    emit::node {
      id,
      kind: Action,
      label: `cc ${src.path}`,
      metadata: {
        target_name: target.name,
        source: src.path,
      },
    }

    emit::edge {
      from: id,
      to: format::archive_id(target.name),
      kind: Always,
    }
  })

  emit::node {
    id: format::archive_id(target.name),
    kind: Action,
    label: `ar ${target.name}`,
    metadata: { target_name: target.name },
  }

  deps.each((dep) => emit::edge {
    from: format::archive_id(dep.target_name),
    to: format::archive_id(target.name),
    kind: IfChanged,
  })
}

fn platform_sources(config: BuildConfig) -> [SourceFile] {
  match config.platform {
    Linux   => [SourceFile { path: "linux.cc" }]
    MacOS   => [SourceFile { path: "macos.cc" }]
    Windows => [SourceFile { path: "win32.cc" }]
  }
}

fn platform_deps(config: BuildConfig) -> [DepRef] {
  match config.platform {
    Linux   => [DepRef { target_name: "//third_party:epoll" }]
    MacOS   => [DepRef { target_name: "//third_party:kqueue" }]
    Windows => [DepRef { target_name: "//third_party:iocp" }]
  }
}
```

What this shows:

- Bazel's `select()` is a configuration-time switch embedded inside attribute
  values. In Vixen, the equivalent is just `match` over a typed platform enum.
- Bazel `select()` operates at the attribute level (spliced into a list). Vixen
  computes the full source/dep list in a pure function and passes it to
  planning.
- Vixen's version is more explicit but also more testable: `platform_sources`
  is a pure function you can call in isolation.

## Example 3: genrule

### Bazel

```python
genrule(
    name = "gen_config",
    srcs = ["config.yaml"],
    outs = ["config.h"],
    cmd = "$(location //tools:gen_config) --input $(SRCS) --output $@",
    tools = ["//tools:gen_config"],
)

cc_library(
    name = "config",
    hdrs = [":gen_config"],
    includes = ["."],
)
```

### Vixen

```text
fn plan_genrule(
  name: String,
  tool: String,
  args: [String],
  inputs: [SourceFile],
  outputs: [String],
) -> Unit {
  action_id = format::genrule_id(name)

  emit::node {
    id: action_id,
    kind: Action,
    label: `gen ${name}`,
    metadata: {
      tool,
      args,
      outputs,
    },
  }

  inputs.each((input) => emit::edge {
    from: format::source_id(input.path),
    to: action_id,
    kind: IfChanged,
  })

  emit::edge {
    from: format::source_id(tool),
    to: action_id,
    kind: IfChanged,
  }

  outputs.each((output) => {
    artifact_id = format::artifact_id(output)

    emit::node {
      id: artifact_id,
      kind: Artifact,
      label: output,
      metadata: { generated_by: name },
    }

    emit::edge {
      from: action_id,
      to: artifact_id,
      kind: Always,
    }
  })
}
```

What this shows:

- Bazel's `genrule` is a macro that hides the action-artifact-dependency
  structure. Vixen makes all three explicit.
- Bazel's `$(location ...)` and `$(SRCS)` are string-level macro expansions
  inside a shell command. Vixen passes tool and args as typed data.
- the dependency on the tool itself (`//tools:gen_config`) is an explicit edge,
  not a side-effect of the `tools` attribute.

## Example 4: Custom rule with providers

### Bazel

```python
CompileInfo = provider(fields = ["objects", "include_dirs"])

def _compile_lib_impl(ctx):
    objects = []
    for src in ctx.files.srcs:
        obj = ctx.actions.declare_file(src.basename + ".o")
        ctx.actions.run(
            inputs = [src] + ctx.files.hdrs,
            outputs = [obj],
            executable = ctx.toolchains["//toolchain:cc"].compiler,
            arguments = [
                "-c", src.path,
                "-o", obj.path,
            ] + ["-I" + d for d in ctx.attr.includes],
        )
        objects.append(obj)

    archive = ctx.actions.declare_file("lib" + ctx.label.name + ".a")
    ctx.actions.run(
        inputs = objects,
        outputs = [archive],
        executable = "ar",
        arguments = ["rcs", archive.path] + [o.path for o in objects],
    )

    return [
        DefaultInfo(files = depset([archive])),
        CompileInfo(
            objects = depset(objects),
            include_dirs = depset(ctx.attr.includes),
        ),
    ]

compile_lib = rule(
    implementation = _compile_lib_impl,
    attrs = {
        "srcs": attr.label_list(allow_files = [".cc", ".cpp"]),
        "hdrs": attr.label_list(allow_files = [".h"]),
        "includes": attr.string_list(),
    },
    toolchains = ["//toolchain:cc"],
)
```

### Vixen

```text
fn plan_compile_lib(target: CppTarget) -> Unit {
  object_ids = target.sources.map((src) => {
    id = format::object_id(target.name, src.path)

    emit::node {
      id,
      kind: Action,
      label: `cc ${src.path}`,
      metadata: {
        source: src.path,
        include_dirs: target.include_dirs,
      },
    }

    target.headers.each((hdr) => emit::edge {
      from: format::source_id(hdr.path),
      to: id,
      kind: IfChanged,
    })

    id
  })

  archive_id = format::archive_id(target.name)

  emit::node {
    id: archive_id,
    kind: Action,
    label: `ar ${target.name}`,
    metadata: { target_name: target.name },
  }

  object_ids.each((oid) => emit::edge {
    from: oid,
    to: archive_id,
    kind: Always,
  })

  emit::fact {
    id: format::fact_id(target.name, "include_dirs"),
    node_id: archive_id,
    value: target.include_dirs.join(":"),
    metadata: {},
  }
}
```

What this shows:

- Bazel's provider system (`CompileInfo` with `objects` and `include_dirs`) is
  how rules pass structured information to downstream consumers. In Vixen, the
  equivalent is graph facts and metadata on nodes.
- Bazel's `ctx.actions.run(...)` declares a single action. Vixen's
  `emit::node` does the same, but the dependency wiring is explicit edges
  rather than implicit from the `inputs` list.
- Bazel's `depset` is a specialized deduplicated collection for transitive
  dependency aggregation. Vixen does not need this in the rule language because
  the host handles transitive closure computation.
- The provider return value in Bazel (`return [DefaultInfo(...), CompileInfo(...)]`)
  is replaced by explicit fact emission in Vixen. Facts are first-class graph
  data, not invisible inter-rule channels.

## Example 5: Test rule with size constraints

### Bazel

```python
cc_test(
    name = "parser_test",
    srcs = ["parser_test.cc"],
    deps = [
        ":parser",
        "//third_party:googletest",
    ],
    size = "small",
    timeout = "short",
    data = ["testdata/input.txt"],
)
```

### Vixen

```text
fn plan_test(test: TestTarget, config: BuildConfig) -> Unit {
  compile_id = format::compile_id(test.name, test.sources.first()
    or fail PlanError { message: `test ${test.name} has no sources` })

  test.sources.each((src) => {
    id = format::compile_id(test.name, src.path)

    emit::node {
      id,
      kind: Action,
      label: `cc ${src.path}`,
      metadata: {
        target_name: test.name,
        source: src.path,
      },
    }

    emit::edge {
      from: id,
      to: format::link_id(test.name),
      kind: Always,
    }
  })

  link_id = format::link_id(test.name)

  emit::node {
    id: link_id,
    kind: Action,
    label: `link ${test.name}`,
    metadata: { target_name: test.name },
  }

  test.deps.each((dep) => emit::edge {
    from: format::archive_id(dep.target_name),
    to: link_id,
    kind: IfChanged,
  })

  run_id = format::test_run_id(test.name)

  emit::node {
    id: run_id,
    kind: Action,
    label: `test ${test.name}`,
    metadata: {
      test_name: test.name,
      size: test_size_label(test.size),
      timeout: test.timeout or default_timeout(test.size),
    },
  }

  emit::edge {
    from: link_id,
    to: run_id,
    kind: Always,
  }

  test.data.each((d) => emit::edge {
    from: format::source_id(d.path),
    to: run_id,
    kind: IfChanged,
  })
}

fn test_size_label(size: TestSize) -> String {
  match size {
    Small    => "small"
    Medium   => "medium"
    Large    => "large"
    Enormous => "enormous"
  }
}

fn default_timeout(size: TestSize) -> U32 {
  match size {
    Small    => 60
    Medium   => 300
    Large    => 900
    Enormous => 3600
  }
}
```

What this shows:

- Bazel's test rules bundle compile + link + run into one declaration. Vixen
  makes the three phases explicit as separate graph nodes.
- test metadata (size, timeout) becomes node metadata rather than a separate
  attribute namespace
- data dependencies are explicit edges, not implicit runtime inputs

## What Bazel Ports Reveal About Vixen

1. **Bazel's `select()` is solved by `match`.** Configurable attributes in
   Bazel are a DSL-within-a-DSL for platform switching. Vixen just uses
   ordinary pattern matching on typed config data. This is simpler and more
   testable.

2. **Providers map to facts and metadata.** Bazel's provider system is how
   rules communicate structured data to downstream rules. Vixen's graph facts
   serve the same purpose but are visible in the build graph rather than being
   opaque inter-rule channels.

3. **`depset` is a host concern.** Bazel's `depset` is a specialized data
   structure for transitive dependency aggregation. In Vixen, transitive
   closures are computed by the host and presented as domain queries, not
   assembled in rule code. This follows from the no-recursion constraint.

4. **Bazel's `ctx.actions.run()` is `emit::node` + `emit::edge`.** Bazel
   bundles action declaration, input specification, and output declaration into
   one call. Vixen separates these concerns — the action is a node, the inputs
   are edges, and the outputs are artifact nodes. This is more verbose but
   produces a fully inspectable graph.

5. **Bazel's rule schema (`attrs = {...}`) is replaced by Vixen's type
   system.** Bazel rules declare their attribute schema in Python dicts with
   `attr.label()`, `attr.string_list()`, etc. Vixen rules receive typed domain
   structs. Schema validation happens at the type-checking boundary, not at
   runtime.
