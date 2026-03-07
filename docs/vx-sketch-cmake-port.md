# Vixen Sketches: Porting CMake Patterns

This document takes real CMake patterns and sketches how the equivalent build
logic would look in Vixen.

The goal is to pressure-test Vixen's surface against a build system that is
fundamentally imperative and script-shaped, where the "rules" are really
sequential commands that mutate global state.

## Assumed Domain Types

These sketches assume a C++ rule pack provides typed domain data, analogous to
how the Rust rule pack provides `CrateGraph` and `CrateNode`.

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
  visibility: Visibility,
}

enum Visibility {
  Public,
  Private,
  Interface,
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

struct CustomCommand {
  name: String,
  tool: String,
  args: [String],
  inputs: [SourceFile],
  outputs: [String],
}
```

## Example 1: Simple executable with library dependencies

### CMake

```cmake
add_executable(myapp
  src/main.cpp
  src/util.cpp
  src/parser.cpp
)

target_link_libraries(myapp PRIVATE fmt::fmt spdlog::spdlog)
target_compile_features(myapp PRIVATE cxx_std_20)
```

### Vixen

```text
fn plan_executable(target: CppTarget, config: BuildConfig) -> Unit {
  compile_ids = target.sources
    .map((src) => {
      id = format::compile_id(target.name, src.path)

      emit::node {
        id,
        kind: Action,
        label: `compile ${src.path}`,
        metadata: {
          target_name: target.name,
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
    },
  }

  compile_ids.each((cid) => emit::edge {
    from: cid,
    to: link_id,
    kind: Always,
  })

  target.deps
    .filter($.visibility != Interface)
    .each((dep) => emit::edge {
      from: format::link_id(dep.target_name),
      to: link_id,
      kind: IfChanged,
    })
}
```

What this shows:

- CMake's imperative "add target, then mutate its properties" becomes
  declarative planning over a pre-parsed target model
- the scattered `target_link_libraries` / `target_compile_features` calls
  collapse into fields on a typed `CppTarget` struct
- Vixen does not need to reproduce CMake's global mutable state

## Example 2: Platform-conditional sources

### CMake

```cmake
add_library(platform STATIC
  src/platform/common.cpp
)

if(WIN32)
  target_sources(platform PRIVATE src/platform/win32.cpp)
elseif(APPLE)
  target_sources(platform PRIVATE src/platform/macos.cpp)
else()
  target_sources(platform PRIVATE src/platform/linux.cpp)
endif()

target_link_libraries(platform
  PUBLIC common_base
)

if(APPLE)
  target_link_libraries(platform PRIVATE "-framework CoreFoundation")
endif()
```

### Vixen

```text
fn plan_platform_lib(target: CppTarget, config: BuildConfig) -> Unit {
  sources = platform_sources(target, config)

  sources.each((src) => {
    id = format::compile_id(target.name, src.path)

    emit::node {
      id,
      kind: Action,
      label: `compile ${src.path}`,
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
    label: `archive ${target.name}`,
    metadata: {
      target_name: target.name,
      output_type: "static_lib",
      extra_link_flags: platform_link_flags(config),
    },
  }
}

fn platform_sources(target: CppTarget, config: BuildConfig) -> [SourceFile] {
  common = target.sources

  platform_specific = match config.platform {
    Windows => [SourceFile { path: "src/platform/win32.cpp" }]
    MacOS   => [SourceFile { path: "src/platform/macos.cpp" }]
    Linux   => [SourceFile { path: "src/platform/linux.cpp" }]
    _       => []
  }

  common.concat(platform_specific)
}

fn platform_link_flags(config: BuildConfig) -> [String] {
  match config.platform {
    MacOS => ["-framework CoreFoundation"]
    _ => []
  }
}
```

What this shows:

- CMake's `if(WIN32) ... elseif(APPLE) ... endif()` becomes a `match` on a
  typed `Platform` enum
- platform logic lives in pure helper functions instead of mutating a global
  target
- the conditional source list is computed once and then planned, rather than
  patched incrementally with `target_sources`

## Example 3: Custom command for code generation

### CMake

```cmake
add_custom_command(
  OUTPUT ${CMAKE_BINARY_DIR}/generated/config.h
  COMMAND python3 ${CMAKE_SOURCE_DIR}/tools/gen_config.py
    --input ${CMAKE_SOURCE_DIR}/config.yaml
    --output ${CMAKE_BINARY_DIR}/generated/config.h
  DEPENDS tools/gen_config.py config.yaml
  COMMENT "Generating config.h"
)

add_custom_target(gen_config DEPENDS ${CMAKE_BINARY_DIR}/generated/config.h)

add_library(app_config INTERFACE)
target_include_directories(app_config
  INTERFACE ${CMAKE_BINARY_DIR}/generated
)
add_dependencies(app_config gen_config)
```

### Vixen

```text
fn plan_codegen(cmd: CustomCommand) -> Unit {
  gen_id = format::codegen_id(cmd.name)

  emit::node {
    id: gen_id,
    kind: Action,
    label: `generate ${cmd.outputs.first() or cmd.name}`,
    metadata: {
      tool: cmd.tool,
      args: cmd.args,
      outputs: cmd.outputs,
    },
  }

  cmd.inputs.each((input) => emit::edge {
    from: format::source_id(input.path),
    to: gen_id,
    kind: IfChanged,
  })

  cmd.outputs.each((output) => emit::node {
    id: format::artifact_id(output),
    kind: Artifact,
    label: output,
    metadata: { generated_by: cmd.name },
  })

  cmd.outputs.each((output) => emit::edge {
    from: gen_id,
    to: format::artifact_id(output),
    kind: Always,
  })
}
```

What this shows:

- CMake's three-step dance (`add_custom_command` + `add_custom_target` +
  `add_dependencies`) collapses into one function that emits the action node,
  its input edges, and its output artifacts
- the generated artifact is an explicit node in the graph, not an implicit file
  dependency
- no global target namespace to worry about

## Example 4: Transitive dependency propagation

### CMake

```cmake
add_library(base STATIC src/base.cpp)
target_include_directories(base PUBLIC include/)

add_library(core STATIC src/core.cpp)
target_link_libraries(core PUBLIC base)
target_include_directories(core PUBLIC include/)

add_executable(app src/main.cpp)
target_link_libraries(app PRIVATE core)
# app transitively gets base's include dirs through core
```

### Vixen

```text
fn plan_target(target: CppTarget, project: CppProject) -> Unit throws ResolveError {
  match target.kind {
    Executable => plan_executable(target, project)
    StaticLib  => plan_static_lib(target, project)
    SharedLib  => plan_shared_lib(target, project)
    HeaderOnly => ()
  }
}

fn plan_with_transitive_deps(
  target: CppTarget,
  project: CppProject,
) -> Unit throws ResolveError {
  plan_target(target, project)

  transitive = resolve_transitive_deps(target, project)

  transitive
    .filter($.visibility == Public)
    .each((dep) => {
      dep_target = project.lookup(dep.target_name)
        or fail ResolveError { message: `unknown dep ${dep.target_name}` }

      emit::edge {
        from: format::output_id(dep_target),
        to: format::compile_id_prefix(target.name),
        kind: IfChanged,
        metadata: { reason: "transitive_include" },
      }
    })
}

fn resolve_transitive_deps(
  target: CppTarget,
  project: CppProject,
) -> [DepRef] throws ResolveError {
  direct = target.deps

  indirect = direct
    .filter($.visibility == Public)
    .flat_map((dep) => {
      dep_target = project.lookup(dep.target_name)
        or fail ResolveError { message: `unknown dep ${dep.target_name}` }
      resolve_transitive_deps(dep_target, project)
    })

  direct.concat(indirect)
}
```

Wait: recursion is banned. That means `resolve_transitive_deps` cannot call
itself. The host would provide a pre-computed transitive closure instead:

```text
fn plan_with_transitive_deps(
  target: CppTarget,
  project: CppProject,
) -> Unit throws ResolveError {
  plan_target(target, project)

  cpp::transitive_public_deps(project, target)
    .each((dep) => {
      emit::edge {
        from: format::output_id(dep),
        to: format::compile_id_prefix(target.name),
        kind: IfChanged,
        metadata: { reason: "transitive_include" },
      }
    })
}
```

What this shows:

- CMake's implicit transitive propagation through `PUBLIC` link dependencies
  becomes explicit graph edges in Vixen
- the no-recursion constraint means the host must provide transitive closures
  as precomputed domain queries, which is the right design anyway
- dependency resolution is a host concern, not a rule-language concern

## What CMake Ports Reveal About Vixen

1. **CMake's biggest problem is global mutable state.** Targets are created and
   then patched by later commands (`target_sources`, `target_link_libraries`,
   `target_compile_features`). Vixen avoids this entirely — targets arrive as
   fully-formed typed data and rules plan from them.

2. **Platform conditionals are much cleaner.** CMake's `if(WIN32)` is stringly
   typed and relies on cached variables. Vixen's `match config.platform` is
   typed, exhaustive, and local.

3. **Custom commands are the worst case.** CMake's three-object dance
   (custom_command + custom_target + add_dependencies) is fundamentally about
   connecting file-based dependencies. Vixen's explicit graph emission handles
   this as ordinary nodes and edges.

4. **Transitive propagation exposes the recursion constraint.** This is where
   the host API matters. The rule language should not compute transitive
   closures — the host should provide them as precomputed domain queries.
