# Vixen Sketches: Hermetic Toolchain Acquisition

This document sketches how Vixen plans the acquisition and setup of hermetic
toolchains as part of the build graph itself.

This is one of Vixen's core axioms: no system tools in the build path. The Rust
compiler, the linker (from Zig), and any other tools must be downloaded,
verified, and unpacked by Vixen before any compilation can happen. That means
toolchain acquisition is not a precondition — it is part of the build graph.

## Why This Matters for the Rule Language

Most build systems treat toolchains as ambient: they assume `rustc` is on
`$PATH` or configured in a settings file. The toolchain is not a graph node —
it is an invisible input to every compilation.

Vixen's hermetic model means:

- toolchain downloads are content-addressed artifacts
- toolchain unpacking is an action node
- every compile/link node depends on the toolchain node
- if the toolchain version changes, everything downstream rebuilds
- toolchains can be shared across machines via the shared CAS

This creates a specific kind of pressure on the rule language: the rules must
be able to express "before you compile anything, make sure the toolchain exists
and is the right version."

## Assumed Domain Types

```text
struct Toolchain {
  name: String,
  version: String,
  kind: ToolchainKind,
  platform: Platform,
  download_url: String,
  sha256: String,
  components: [ToolchainComponent],
}

enum ToolchainKind {
  Rust,
  Zig,
  Llvm,
}

struct ToolchainComponent {
  name: String,
  relative_path: String,
  executable: Bool,
}

enum Platform {
  Linux,
  MacOS,
  Windows,
}

struct ToolchainSet {
  rust: Toolchain,
  zig: Toolchain,
  platform: Platform,
}

struct BuildConfig {
  platform: Platform,
  profile: String,
  target_triple: String,
  toolchains: ToolchainSet,
}

struct RustTarget {
  name: String,
  kind: RustTargetKind,
  sources: [SourceFile],
  deps: [RustDepRef],
  edition: String,
}

enum RustTargetKind {
  Binary,
  Library,
}

struct SourceFile {
  path: String,
}

struct RustDepRef {
  crate_name: String,
  target_name: String,
}
```

## Example 1: Planning a single toolchain acquisition

```text
fn plan_toolchain(tc: Toolchain) -> Unit throws PlanError {
  download_id = format::download_id(tc.name, tc.version)
  unpack_id = format::unpack_id(tc.name, tc.version)
  ready_id = format::toolchain_ready_id(tc.name, tc.version)

  // Download the archive
  emit::node {
    id: download_id,
    kind: Action,
    label: `download ${tc.name} ${tc.version}`,
    metadata: {
      url: tc.download_url,
      expected_sha256: tc.sha256,
      category: "toolchain_download",
    },
  }

  // Download produces an archive artifact
  archive_id = format::artifact_id(`toolchains/${tc.name}-${tc.version}.tar.xz`)

  emit::node {
    id: archive_id,
    kind: Artifact,
    label: `${tc.name}-${tc.version}.tar.xz`,
    metadata: { sha256: tc.sha256 },
  }

  emit::edge {
    from: download_id,
    to: archive_id,
    kind: Always,
  }

  // Unpack the archive
  emit::node {
    id: unpack_id,
    kind: Action,
    label: `unpack ${tc.name} ${tc.version}`,
    metadata: {
      archive: archive_id,
      category: "toolchain_unpack",
    },
  }

  emit::edge {
    from: archive_id,
    to: unpack_id,
    kind: Always,
  }

  // Each component becomes an artifact
  tc.components.each((comp) => {
    comp_id = format::artifact_id(
      `toolchains/${tc.name}-${tc.version}/${comp.relative_path}`
    )

    emit::node {
      id: comp_id,
      kind: Artifact,
      label: comp.relative_path,
      metadata: {
        toolchain: tc.name,
        version: tc.version,
        executable: comp.executable,
      },
    }

    emit::edge {
      from: unpack_id,
      to: comp_id,
      kind: Always,
    }
  })

  // A sentinel "ready" fact so downstream rules can depend on the whole
  // toolchain being available without depending on each component
  emit::node {
    id: ready_id,
    kind: Fact,
    label: `${tc.name} ${tc.version} ready`,
    metadata: {
      toolchain: tc.name,
      version: tc.version,
    },
  }

  emit::edge {
    from: unpack_id,
    to: ready_id,
    kind: Always,
  }
}
```

What this shows:

- toolchain acquisition is three graph nodes: download → archive → unpack →
  components + ready sentinel
- the download is content-addressed by `expected_sha256` — if the archive is
  already in the CAS, the download is a no-op
- individual components (rustc, cargo, etc.) are artifact nodes that specific
  compile actions can depend on
- the "ready" sentinel lets rules say "I need the Rust toolchain" without
  listing every component

## Example 2: Wiring toolchains into compilation

```text
fn plan_rust_compile_with_toolchain(
  target: RustTarget,
  config: BuildConfig,
) -> Unit {
  tc = config.toolchains.rust
  rustc_ready = format::toolchain_ready_id(tc.name, tc.version)
  rustc_bin = format::artifact_id(
    `toolchains/${tc.name}-${tc.version}/bin/rustc`
  )

  compile_id = format::rustc_id(target.name)

  emit::node {
    id: compile_id,
    kind: Action,
    label: `rustc ${target.name}`,
    metadata: {
      crate_name: target.name,
      crate_type: match target.kind {
        Binary  => "bin"
        Library => "lib"
      },
      edition: target.edition,
      rustc_path: `toolchains/${tc.name}-${tc.version}/bin/rustc`,
      toolchain_version: tc.version,
    },
  }

  // Depend on the toolchain being ready
  emit::edge {
    from: rustc_ready,
    to: compile_id,
    kind: IfChanged,
  }

  // Depend on the rustc binary specifically
  emit::edge {
    from: rustc_bin,
    to: compile_id,
    kind: IfChanged,
  }

  // Source dependencies
  target.sources.each((src) => emit::edge {
    from: format::source_id(src.path),
    to: compile_id,
    kind: IfChanged,
  })

  // Crate dependencies
  target.deps.each((dep) => emit::edge {
    from: format::rustc_id(dep.target_name),
    to: compile_id,
    kind: IfChanged,
  })
}
```

What this shows:

- every compilation node depends on the toolchain ready sentinel AND the
  specific compiler binary
- the `IfChanged` edge from the toolchain means: if the toolchain version
  changes, all downstream compilations rerun
- the compiler path is metadata on the compile node — the executor uses it to
  find the right binary, not `$PATH`
- this is the hermetic guarantee: there is an explicit, content-addressed
  dependency chain from "download rustc" to "compile this crate"

## Example 3: Zig as the linker

Vixen uses Zig's bundled lld as the linker. This means the link step depends
on the Zig toolchain, not the Rust toolchain.

```text
fn plan_link_with_zig(
  target: RustTarget,
  config: BuildConfig,
) -> Unit {
  zig_tc = config.toolchains.zig
  zig_ready = format::toolchain_ready_id(zig_tc.name, zig_tc.version)
  zig_bin = format::artifact_id(
    `toolchains/${zig_tc.name}-${zig_tc.version}/zig`
  )

  link_id = format::link_id(target.name)

  emit::node {
    id: link_id,
    kind: Action,
    label: `link ${target.name}`,
    metadata: {
      target_name: target.name,
      linker: `toolchains/${zig_tc.name}-${zig_tc.version}/zig`,
      linker_mode: "cc",
      target_triple: config.target_triple,
    },
  }

  // Link depends on the Zig toolchain
  emit::edge {
    from: zig_ready,
    to: link_id,
    kind: IfChanged,
  }

  emit::edge {
    from: zig_bin,
    to: link_id,
    kind: IfChanged,
  }

  // Link depends on the compiled output
  emit::edge {
    from: format::rustc_id(target.name),
    to: link_id,
    kind: Always,
  }
}
```

What this shows:

- the compile step depends on the Rust toolchain, the link step depends on the
  Zig toolchain — different tools, different dependency chains
- if Zig is updated but Rust is not, only link steps rerun (not compiles)
- the linker path is in the metadata, not discovered from the environment

## Example 4: Full planning with toolchain setup

```text
fn plan_full_build(
  targets: [RustTarget],
  config: BuildConfig,
) -> Unit throws PlanError {
  // First: plan toolchain acquisition
  plan_toolchain(config.toolchains.rust)
  plan_toolchain(config.toolchains.zig)

  // Then: plan compilation and linking
  targets.each((target) => {
    plan_rust_compile_with_toolchain(target, config)

    if target.kind == Binary {
      plan_link_with_zig(target, config)
    }
  })
}
```

What this shows:

- toolchain planning and compilation planning are composed in one function
- the graph edges ensure correct ordering: downloads happen before compiles,
  regardless of the order `plan_toolchain` and `plan_rust_compile` are called
- the entire build, from toolchain acquisition to final binary, is one graph

## Example 5: Toolchain version pinning and verification

```text
fn plan_verified_toolchain(tc: Toolchain) -> Unit throws PlanError {
  // Verify the URL matches the expected pattern for this toolchain
  expected_prefix = match tc.kind {
    Rust => "https://static.rust-lang.org/dist/"
    Zig  => "https://ziglang.org/download/"
    Llvm => "https://github.com/llvm/llvm-project/releases/"
  }

  if !tc.download_url.starts_with(expected_prefix) {
    fail PlanError {
      message: `suspicious download URL for ${tc.name}: ${tc.download_url}`,
    }
  }

  // Verify the checksum is a valid sha256 hex string
  if tc.sha256.len() != 64 {
    fail PlanError {
      message: `invalid sha256 length for ${tc.name}: expected 64, got ${tc.sha256.len()}`,
    }
  }

  plan_toolchain(tc)
}
```

What this shows:

- rule code can validate toolchain metadata before planning the download
- URL prefix checking is a basic supply-chain safety measure
- the `fail` path produces a typed diagnostic, not a silent fallback
- this validation runs at planning time, before any download is attempted

## What Toolchain Acquisition Reveals About Vixen

1. **Toolchains are graph nodes, not ambient assumptions.** This is the core
   hermetic property. Every compile/link action has an explicit, content-
   addressed dependency on the toolchain that produced it. If the toolchain
   is already in the CAS, the download is skipped. If the toolchain changes,
   everything downstream rebuilds.

2. **The CAS is invisible to rule code.** Rule code emits `expected_sha256`
   as metadata on the download node. The executor and CAS handle deduplication,
   caching, and sharing. The rule language describes intent ("download this,
   verify this hash"), not mechanism ("check the cache, then fetch if missing").

3. **Compile and link can depend on different toolchains.** Rust compilation
   uses rustc, linking uses Zig's lld. These are independent dependency chains
   in the graph. Updating one does not invalidate work done by the other.

4. **Planning-time validation is natural.** Checking URLs, checksums, and
   version constraints is ordinary rule code with `if` / `fail`. No special
   validation framework is needed.

5. **The "ready" sentinel pattern is useful.** Instead of making every compile
   node depend on every toolchain component, a single Fact node serves as a
   "toolchain is fully unpacked" signal. This keeps the graph manageable
   without losing correctness.
