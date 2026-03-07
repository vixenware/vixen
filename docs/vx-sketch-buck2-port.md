# Vixen Sketches: Porting Buck2 Patterns

This document takes real Buck2 patterns and sketches how the equivalent build
logic would look in Vixen.

Buck2 is the closest existing system to Vixen's design goals: content-addressed
storage, remote execution, incremental computation, and a non-Turing-complete
rule language (Starlark). The comparison is less about "why Vixen is better"
and more about where the surfaces differ and what Vixen gains or loses.

## Assumed Domain Types

These sketches reuse the C++ types from the earlier ports and add Buck2-specific
concepts like dynamic dependencies and anonymous targets.

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

// Rust-specific domain types for the Rust rule pack
struct RustTarget {
  name: String,
  kind: RustTargetKind,
  sources: [SourceFile],
  deps: [DepRef],
  edition: String,
  features: [String],
}

enum RustTargetKind {
  Binary,
  Library,
  ProcMacro,
  Test,
}

// Code generation input
struct CodegenTarget {
  name: String,
  schema: SourceFile,
  generator: String,
  language: ProtoLanguage,
}

enum ProtoLanguage {
  Rust,
  Cpp,
  Python,
}
```

## Example 1: cxx_binary with platform deps

### Buck2

```python
cxx_binary(
    name = "server",
    srcs = ["main.cpp", "server.cpp"],
    deps = [
        ":core",
        "//third-party:boost",
    ] + select({
        "config//os:linux": ["//platform:epoll_reactor"],
        "config//os:macos": ["//platform:kqueue_reactor"],
    }),
    compiler_flags = ["-std=c++20", "-O2"],
    linker_flags = select({
        "config//os:linux": ["-lpthread"],
        "DEFAULT": [],
    }),
)
```

### Vixen

```text
fn plan_cxx_binary(target: CppTarget, config: BuildConfig) -> Unit {
  all_deps = target.deps.concat(platform_deps(config))
  all_link_flags = platform_link_flags(config)

  object_ids = target.sources.map((src) => {
    id = format::compile_id(target.name, src.path)

    emit::node {
      id,
      kind: Action,
      label: `c++ ${src.path}`,
      metadata: {
        source: src.path,
        flags: target.compile_flags,
      },
    }

    id
  })

  link_id = format::link_id(target.name)

  emit::node {
    id: link_id,
    kind: Action,
    label: `link ${target.name}`,
    metadata: {
      target_name: target.name,
      output_type: "executable",
      link_flags: all_link_flags,
    },
  }

  object_ids.each((oid) => emit::edge {
    from: oid,
    to: link_id,
    kind: Always,
  })

  all_deps.each((dep) => emit::edge {
    from: format::output_id_for(dep.target_name),
    to: link_id,
    kind: IfChanged,
  })
}

fn platform_deps(config: BuildConfig) -> [DepRef] {
  match config.platform {
    Linux => [DepRef { target_name: "//platform:epoll_reactor" }]
    MacOS => [DepRef { target_name: "//platform:kqueue_reactor" }]
    _     => []
  }
}

fn platform_link_flags(config: BuildConfig) -> [String] {
  match config.platform {
    Linux => ["-lpthread"]
    _     => []
  }
}
```

What this shows:

- Buck2's `select()` behaves identically to Bazel's. Vixen replaces it with
  the same `match` pattern.
- Buck2 and Bazel both bundle compile + link into one target declaration. Vixen
  decomposes this into explicit graph nodes, which is what both systems do
  internally in their rule implementations.

## Example 2: rust_library with features and edition

### Buck2

```python
rust_library(
    name = "parser",
    srcs = glob(["src/**/*.rs"]),
    deps = [
        "//third-party:serde",
        "//third-party:nom",
    ],
    features = ["json", "streaming"],
    edition = "2021",
    env = {
        "PARSER_VERSION": "2.1.0",
    },
)
```

### Vixen

```text
fn plan_rust_library(target: RustTarget) -> Unit {
  compile_id = format::rustc_id(target.name)

  emit::node {
    id: compile_id,
    kind: Action,
    label: `rustc ${target.name}`,
    metadata: {
      crate_name: target.name,
      crate_type: "lib",
      edition: target.edition,
      features: target.features,
      source_count: target.sources.len(),
    },
  }

  target.sources.each((src) => emit::edge {
    from: format::source_id(src.path),
    to: compile_id,
    kind: IfChanged,
  })

  target.deps.each((dep) => emit::edge {
    from: format::rustc_id(dep.target_name),
    to: compile_id,
    kind: IfChanged,
  })

  rlib_id = format::artifact_id(`${target.name}.rlib`)

  emit::node {
    id: rlib_id,
    kind: Artifact,
    label: `${target.name}.rlib`,
    metadata: { crate_name: target.name },
  }

  emit::edge {
    from: compile_id,
    to: rlib_id,
    kind: Always,
  }
}
```

What this shows:

- Buck2's `rust_library` declaration is data. Vixen plans the compile action,
  source edges, dependency edges, and output artifact explicitly.
- features are data on the target, passed through as metadata on the compile
  node. The rule language does not need to understand what features do — the
  executor reads the metadata.
- `glob(["src/**/*.rs"])` in Buck2 is evaluated at parse time to produce a
  source list. In Vixen, the source list arrives pre-resolved in the typed
  domain data.

## Example 3: Custom rule with actions and providers

### Buck2

```python
def _codegen_impl(ctx: AnalysisContext) -> list[Provider]:
    output = ctx.actions.declare_output("generated.rs")

    ctx.actions.run(
        cmd_args(
            ctx.attrs.generator[RunInfo],
            "--input", ctx.attrs.schema,
            "--output", output.as_output(),
            "--lang", ctx.attrs.language,
        ),
        category = "codegen",
    )

    return [DefaultInfo(default_output = output)]

codegen = rule(
    impl = _codegen_impl,
    attrs = {
        "schema": attrs.source(),
        "generator": attrs.exec_dep(providers = [RunInfo]),
        "language": attrs.string(default = "rust"),
    },
)
```

### Vixen

```text
fn plan_codegen(target: CodegenTarget) -> Unit {
  gen_id = format::codegen_id(target.name)
  output_path = codegen_output_path(target)

  emit::node {
    id: gen_id,
    kind: Action,
    label: `codegen ${target.name}`,
    metadata: {
      generator: target.generator,
      schema: target.schema.path,
      language: codegen_language_flag(target.language),
      output: output_path,
      category: "codegen",
    },
  }

  emit::edge {
    from: format::source_id(target.schema.path),
    to: gen_id,
    kind: IfChanged,
  }

  emit::edge {
    from: format::tool_id(target.generator),
    to: gen_id,
    kind: IfChanged,
  }

  output_id = format::artifact_id(output_path)

  emit::node {
    id: output_id,
    kind: Artifact,
    label: output_path,
    metadata: { generated_by: target.name },
  }

  emit::edge {
    from: gen_id,
    to: output_id,
    kind: Always,
  }
}

fn codegen_output_path(target: CodegenTarget) -> String {
  match target.language {
    Rust   => `generated/${target.name}.rs`
    Cpp    => `generated/${target.name}.h`
    Python => `generated/${target.name}.py`
  }
}

fn codegen_language_flag(lang: ProtoLanguage) -> String {
  match lang {
    Rust   => "rust"
    Cpp    => "cpp"
    Python => "python"
  }
}
```

What this shows:

- Buck2's `ctx.actions.run(cmd_args(...))` builds a command line from typed
  pieces. Vixen emits a node with structured metadata — the executor
  reconstructs the command from that metadata.
- Buck2's provider return (`[DefaultInfo(default_output = output)]`) declares
  what the rule produces for downstream consumers. In Vixen, the output
  artifact is an explicit node in the graph. Downstream rules connect to it via
  edges.
- Buck2's `attrs.exec_dep(providers = [RunInfo])` is a typed dependency
  declaration with provider constraints. Vixen's domain types serve the same
  role: `target.generator` is already a string identifying the tool, and the
  tool's availability is an edge in the graph.

## Example 4: Multi-target planning (the "macro" pattern)

### Buck2

```python
def my_service(name, proto_srcs, deps = []):
    """Macro that creates a proto library, a server binary, and tests."""

    codegen(
        name = name + "_proto",
        schema = proto_srcs[0],
        generator = "//tools:protoc",
        language = "rust",
    )

    rust_library(
        name = name + "_lib",
        srcs = glob(["src/**/*.rs"]),
        deps = deps + [":" + name + "_proto"],
    )

    rust_binary(
        name = name,
        srcs = ["src/main.rs"],
        deps = [":" + name + "_lib"],
    )

    rust_test(
        name = name + "_test",
        srcs = glob(["tests/**/*.rs"]),
        deps = [":" + name + "_lib", "//third-party:tokio-test"],
    )
```

### Vixen

```text
fn plan_service(
  name: String,
  codegen_target: CodegenTarget,
  lib_target: RustTarget,
  bin_target: RustTarget,
  test_target: RustTarget,
) -> Unit throws PlanError {
  plan_codegen(codegen_target)
  plan_rust_library(lib_target)
  plan_rust_binary(bin_target)
  plan_rust_test(test_target)

  emit::edge {
    from: format::artifact_id(codegen_output_path(codegen_target)),
    to: format::rustc_id(lib_target.name),
    kind: IfChanged,
  }
}

fn plan_rust_binary(target: RustTarget) -> Unit {
  compile_id = format::rustc_id(target.name)

  emit::node {
    id: compile_id,
    kind: Action,
    label: `rustc ${target.name}`,
    metadata: {
      crate_name: target.name,
      crate_type: "bin",
      edition: target.edition,
    },
  }

  target.sources.each((src) => emit::edge {
    from: format::source_id(src.path),
    to: compile_id,
    kind: IfChanged,
  })

  target.deps.each((dep) => emit::edge {
    from: format::rustc_id(dep.target_name),
    to: compile_id,
    kind: IfChanged,
  })
}

fn plan_rust_test(target: RustTarget) -> Unit {
  compile_id = format::rustc_id(target.name)

  emit::node {
    id: compile_id,
    kind: Action,
    label: `rustc ${target.name} (test)`,
    metadata: {
      crate_name: target.name,
      crate_type: "test",
      edition: target.edition,
    },
  }

  target.sources.each((src) => emit::edge {
    from: format::source_id(src.path),
    to: compile_id,
    kind: IfChanged,
  })

  target.deps.each((dep) => emit::edge {
    from: format::rustc_id(dep.target_name),
    to: compile_id,
    kind: IfChanged,
  })

  run_id = format::test_run_id(target.name)

  emit::node {
    id: run_id,
    kind: Action,
    label: `test ${target.name}`,
    metadata: { test_binary: target.name },
  }

  emit::edge {
    from: compile_id,
    to: run_id,
    kind: Always,
  }
}
```

What this shows:

- Buck2 macros compose targets by string-concatenating names and wiring
  dependencies through naming conventions. Vixen composes planning functions
  that each emit their own subgraph, and wires them with explicit cross-edges.
- Buck2 macros are imperative Starlark that calls rule functions. Vixen macros
  are ordinary functions that call other planning functions. Same idea, typed.
- the cross-cutting edge from codegen output to the library compile is explicit
  in Vixen. In Buck2, it is implicit through the `deps = [":" + name + "_proto"]`
  naming convention.

## Example 5: Dynamic dependencies (Buck2-specific)

### Buck2

Buck2 has a unique `dynamic_output` feature for cases where the dependency
graph cannot be fully determined until an action runs:

```python
def _expand_impl(ctx: AnalysisContext) -> list[Provider]:
    manifest = ctx.actions.declare_output("manifest.json")

    # First action: generate a manifest of what to build
    ctx.actions.run(
        cmd_args(ctx.attrs.scanner[RunInfo], "--output", manifest.as_output()),
        category = "scan",
    )

    # Second action: depends on manifest contents (dynamic)
    def _dynamic(ctx, artifacts, outputs):
        manifest_content = artifacts[manifest].read_json()
        for entry in manifest_content["files"]:
            out = outputs[entry["name"]]
            ctx.actions.run(
                cmd_args("process", entry["path"], out.as_output()),
                category = "process",
            )

    outputs = {
        f["name"]: ctx.actions.declare_output(f["name"])
        for f in expected_files
    }
    ctx.actions.dynamic_output(
        dynamic = [manifest],
        inputs = [],
        outputs = outputs.values(),
        f = _dynamic,
    )

    return [DefaultInfo(default_outputs = outputs.values())]
```

### Vixen

Vixen's graph-emission model does not have dynamic dependencies in the rule
language itself. The equivalent would be:

```text
fn plan_scan_and_process(name: String, scanner: String) -> Unit {
  scan_id = format::scan_id(name)

  emit::node {
    id: scan_id,
    kind: Action,
    label: `scan ${name}`,
    metadata: {
      tool: scanner,
      category: "scan",
      produces_manifest: true,
    },
  }

  emit::node {
    id: format::dynamic_expand_id(name),
    kind: Group,
    label: `expand ${name}`,
    metadata: {
      expander: "scan_and_process",
      manifest_source: scan_id,
    },
  }

  emit::edge {
    from: scan_id,
    to: format::dynamic_expand_id(name),
    kind: Always,
  }
}
```

The actual expansion — reading the manifest and creating process actions —
would happen at the executor level, not in the rule language. The rule emits a
`Group` node that the executor knows how to expand dynamically.

What this shows:

- Buck2 handles dynamic dependencies inside the rule language with
  `dynamic_output` callbacks. Vixen pushes this to the executor layer.
- This is a deliberate design boundary: the rule language is for static
  planning over known inputs. Dynamic expansion is an executor concern.
- The rule still declares the *shape* of the dynamic expansion (a scan
  followed by a group expansion), even if it cannot enumerate the individual
  process actions.

## What Buck2 Ports Reveal About Vixen

1. **Buck2's Starlark and Vixen's rule language solve the same problem.** Both
   are non-Turing-complete, both produce build graphs, both separate rule logic
   from execution. The main difference is surface: Starlark is Python-shaped,
   Vixen is Rust-shaped with functional transforms.

2. **Buck2's provider system maps to Vixen's graph facts.** Both are typed
   inter-rule communication channels. Buck2 providers are returned from rule
   implementations. Vixen facts are emitted into the graph. The information
   flow is the same.

3. **Buck2's `select()` and Vixen's `match` are equivalent.** Both are
   configuration-time switches. Vixen's version is typed and checked by the
   compiler.

4. **Buck2 macros and Vixen planning functions compose the same way.** Both
   call sub-rules and wire dependencies between them. Buck2 uses string-based
   target names. Vixen uses typed domain data and explicit edges.

5. **Dynamic dependencies are the sharpest divergence.** Buck2 handles dynamic
   deps in the rule language with `dynamic_output`. Vixen draws a hard line:
   static planning in the rule language, dynamic expansion in the executor.
   This simplifies the rule language at the cost of requiring executor-level
   support for dynamic patterns.

6. **Content-addressed storage is invisible in both.** Neither Buck2's
   Starlark rules nor Vixen's rule functions mention content hashing. Both
   systems handle CAS at the execution layer. The rule language describes
   *what* to build, not *how* to cache it.
