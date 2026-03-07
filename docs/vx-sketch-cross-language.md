# Vixen Sketches: Cross-Language Builds

This document sketches what happens when a single build graph spans multiple
languages and rule packs. This is a core Vixen requirement — the self-hosting
goal requires building LLVM (C++), linking it into a Rust binary, and
potentially generating bindings across the boundary.

The interesting pressure is not "can we plan C++ and Rust separately?" — we
already know that works. The interesting pressure is what happens at the seams.

## Assumed Domain Types

These sketches assume both a C++ rule pack and a Rust rule pack, plus a
cross-language binding layer.

```text
// From the C++ rule pack
struct CppTarget {
  name: String,
  kind: CppTargetKind,
  sources: [SourceFile],
  headers: [SourceFile],
  deps: [DepRef],
  compile_flags: [String],
  include_dirs: [String],
}

enum CppTargetKind {
  StaticLib,
  SharedLib,
}

// From the Rust rule pack
struct RustTarget {
  name: String,
  kind: RustTargetKind,
  sources: [SourceFile],
  deps: [RustDepRef],
  edition: String,
  features: [String],
  native_deps: [NativeDepRef],
}

enum RustTargetKind {
  Binary,
  Library,
  ProcMacro,
}

// Cross-language binding
struct NativeDepRef {
  target_name: String,
  link_type: NativeLinkType,
}

enum NativeLinkType {
  Static,
  Dynamic,
}

// Binding generation
struct BindgenTarget {
  name: String,
  header: SourceFile,
  include_dirs: [String],
  allowlist_functions: [String],
  allowlist_types: [String],
  language_from: BindgenLanguage,
  language_to: BindgenLanguage,
}

enum BindgenLanguage {
  C,
  Cpp,
  Rust,
}

// Unified project view
struct MixedProject {
  cpp_targets: [CppTarget],
  rust_targets: [RustTarget],
  bindgen_targets: [BindgenTarget],
  config: BuildConfig,
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

struct SourceFile {
  path: String,
}

struct DepRef {
  target_name: String,
}

struct RustDepRef {
  crate_name: String,
  target_name: String,
}
```

## Example 1: Rust binary linking a C static library

The simplest cross-language case: a Rust binary that depends on a C library
through FFI.

```text
fn plan_rust_with_native_dep(
  rust: RustTarget,
  config: BuildConfig,
  project: MixedProject,
) -> Unit throws PlanError {
  // Plan the Rust compilation
  rustc_id = format::rustc_id(rust.name)

  emit::node {
    id: rustc_id,
    kind: Action,
    label: `rustc ${rust.name}`,
    metadata: {
      crate_name: rust.name,
      crate_type: rust_target_type(rust.kind),
      edition: rust.edition,
    },
  }

  rust.sources.each((src) => emit::edge {
    from: format::source_id(src.path),
    to: rustc_id,
    kind: IfChanged,
  })

  // Rust-to-Rust deps
  rust.deps.each((dep) => emit::edge {
    from: format::rustc_id(dep.target_name),
    to: rustc_id,
    kind: IfChanged,
  })

  // Cross-language: link against native libraries
  rust.native_deps.each((native) => {
    cpp_target = project.cpp_targets.find($.name == native.target_name)
      or fail PlanError { message: `native dep ${native.target_name} not found` }

    // The Rust compile needs the C headers (for bindgen/FFI)
    cpp_target.headers.each((hdr) => emit::edge {
      from: format::source_id(hdr.path),
      to: rustc_id,
      kind: IfChanged,
    })

    // The Rust link needs the C archive
    link_id = format::link_id(rust.name)

    emit::edge {
      from: format::archive_id(native.target_name),
      to: link_id,
      kind: Always,
      metadata: {
        link_type: match native.link_type {
          Static  => "static"
          Dynamic => "dynamic"
        },
      },
    }
  })

  // Link node for the Rust binary
  if rust.kind == Binary {
    link_id = format::link_id(rust.name)

    emit::node {
      id: link_id,
      kind: Action,
      label: `link ${rust.name}`,
      metadata: {
        target_name: rust.name,
        output_type: "executable",
      },
    }

    emit::edge {
      from: rustc_id,
      to: link_id,
      kind: Always,
    }
  }
}

fn rust_target_type(kind: RustTargetKind) -> String {
  match kind {
    Binary   => "bin"
    Library  => "lib"
    ProcMacro => "proc-macro"
  }
}
```

What this shows:

- cross-language dependencies are explicit edges between nodes from different
  rule packs
- the seam is where Rust's `native_deps` field references C++ targets by name,
  and the planning function resolves them against the project model
- header dependencies flow as IfChanged edges from C headers to the Rust
  compile node — if a header changes, Rust recompiles
- link dependencies flow as Always edges from the C archive to the Rust link

## Example 2: Bindgen — generating Rust bindings from C headers

```text
fn plan_bindgen(
  target: BindgenTarget,
  project: MixedProject,
) -> Unit throws PlanError {
  gen_id = format::bindgen_id(target.name)
  output_path = `generated/${target.name}_bindings.rs`

  emit::node {
    id: gen_id,
    kind: Action,
    label: `bindgen ${target.name}`,
    metadata: {
      header: target.header.path,
      include_dirs: target.include_dirs,
      allowlist_functions: target.allowlist_functions,
      allowlist_types: target.allowlist_types,
      output: output_path,
    },
  }

  // Bindgen depends on the input header
  emit::edge {
    from: format::source_id(target.header.path),
    to: gen_id,
    kind: IfChanged,
  }

  // Bindgen also depends on all transitively included headers
  // (the host provides this — rule code cannot chase #include graphs)
  cpp::transitive_headers(project, target.header)
    .each((hdr) => emit::edge {
      from: format::source_id(hdr.path),
      to: gen_id,
      kind: IfChanged,
    })

  // Output artifact
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
```

What this shows:

- binding generation is a node in the build graph like any other action
- transitive header resolution is a host query (`cpp::transitive_headers`),
  not rule-language recursion — the host has already parsed includes
- the generated `.rs` file is an artifact node that downstream Rust compiles
  can depend on

## Example 3: Full pipeline — C library + bindings + Rust binary

This is the composition case: multiple planning functions wired together.

```text
fn plan_mixed_project(project: MixedProject) -> Unit throws PlanError {
  // Phase 1: plan all C++ targets
  project.cpp_targets.each((target) => plan_cpp_target(target))

  // Phase 2: plan all binding generators
  project.bindgen_targets.each((target) => plan_bindgen(target, project))

  // Phase 3: plan all Rust targets (which may depend on C++ and bindings)
  project.rust_targets.each((target) =>
    plan_rust_with_native_dep(target, project.config, project)
  )

  // Phase 4: wire bindgen outputs into Rust compiles
  project.bindgen_targets.each((bg) => {
    output_path = `generated/${bg.name}_bindings.rs`

    // Find Rust targets that depend on this bindgen output
    project.rust_targets
      .filter((rt) => rt.sources.any($.path == output_path))
      .each((rt) => emit::edge {
        from: format::artifact_id(output_path),
        to: format::rustc_id(rt.name),
        kind: IfChanged,
      })
  })
}

fn plan_cpp_target(target: CppTarget) -> Unit {
  target.sources.each((src) => {
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

    emit::edge {
      from: format::source_id(src.path),
      to: id,
      kind: IfChanged,
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
}
```

What this shows:

- the full cross-language pipeline is: C sources → C compile → C archive →
  bindgen → Rust bindings → Rust compile → Rust link (with C archive)
- each phase is a separate planning function, composed in `plan_mixed_project`
- the cross-language wiring (phase 4) is explicit graph edges from bindgen
  artifacts to Rust compile nodes
- ordering between planning phases does not matter — it is the graph edges
  that determine execution order, not the order of `emit` calls

## Example 4: Codegen that produces both `.rs` and `.h`

Some generators (protobuf, flatbuffers, capnproto) produce outputs for
multiple languages from a single schema.

```text
fn plan_multi_language_codegen(
  name: String,
  schema: SourceFile,
  generator: String,
  languages: [BindgenLanguage],
) -> Unit {
  gen_id = format::codegen_id(name)

  emit::node {
    id: gen_id,
    kind: Action,
    label: `codegen ${name}`,
    metadata: {
      generator,
      schema: schema.path,
      languages: languages.map(language_flag($)),
    },
  }

  emit::edge {
    from: format::source_id(schema.path),
    to: gen_id,
    kind: IfChanged,
  }

  // Each target language gets its own output artifact
  languages.each((lang) => {
    output_path = codegen_output(name, lang)
    output_id = format::artifact_id(output_path)

    emit::node {
      id: output_id,
      kind: Artifact,
      label: output_path,
      metadata: {
        generated_by: name,
        language: language_flag(lang),
      },
    }

    emit::edge {
      from: gen_id,
      to: output_id,
      kind: Always,
    }
  })
}

fn codegen_output(name: String, lang: BindgenLanguage) -> String {
  match lang {
    C    => `generated/${name}.h`
    Cpp  => `generated/${name}.h`
    Rust => `generated/${name}.rs`
  }
}

fn language_flag(lang: BindgenLanguage) -> String {
  match lang {
    C    => "c"
    Cpp  => "cpp"
    Rust => "rust"
  }
}
```

What this shows:

- one action node, multiple output artifacts, one per language
- downstream C++ and Rust compiles independently depend on their respective
  output artifacts
- the codegen action reruns if the schema changes, and only the affected
  language outputs trigger downstream rebuilds (via IfChanged edges)

## What Cross-Language Builds Reveal About Vixen

1. **The seam is data, not mechanism.** Cross-language dependencies work
   because both rule packs emit into the same graph. The connection is an
   ordinary edge from a C++ archive node to a Rust link node. No special
   cross-language machinery is needed in the rule language.

2. **Host queries matter at boundaries.** Transitive header resolution
   (`cpp::transitive_headers`) is a host concern. The rule language cannot
   chase `#include` graphs — that requires parsing C/C++ which is firmly
   outside the rule language's scope.

3. **Artifact nodes are the lingua franca.** When a codegen step produces
   `.rs` and `.h` files, both are artifact nodes. Downstream rules from any
   language can depend on them by adding edges. The artifact node type is
   language-agnostic.

4. **Planning order does not matter.** Phase 1/2/3/4 in `plan_mixed_project`
   is a convenience for reading the code. The graph does not care what order
   nodes and edges were emitted — it is the edge structure that determines
   execution order.

5. **The `MixedProject` type is the real design question.** How do C++ targets
   and Rust targets end up in the same project model? That is a manifest
   parsing and project discovery question, not a rule-language question. The
   rule language receives the answer; it does not compute it.
