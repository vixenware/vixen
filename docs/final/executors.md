# Executors and ambient tools

This document records design decisions and open questions about where `exec`
runs and how ambient tools (Xcode, MSVC, system frameworks) are handled.

## Core model

Every `exec` runs in a sandbox. The sandbox blocks everything by default.
Inputs must be explicitly declared. There are two kinds:

  * **Content-addressed inputs**: source trees, downloaded toolchains,
    dependencies. Mounted via VFS. Hashed (BLAKE3). Fully hermetic. Can be
    shared across executors via the CAS.

  * **Ambient tools**: Xcode, MSVC, system SDKs. Already installed on the
    executor. Too large or legally restricted to redistribute. Declared by
    name, versioned, and fingerprinted. Exposed in the sandbox by a module
    that knows how to find, version, and hash them.

Both kinds go into the cache key. The sandbox enforces that nothing undeclared
leaks in. If a build tries to read something that isn't declared, it fails.

## Ambient tool modules

Each ambient tool has a module built into Vixen (or provided via plugin) that
knows:

  * How to find the tool on an executor (e.g. `xcode-select -p`)
  * How to extract its version
  * What paths to expose in the sandbox
  * How to hash/fingerprint it for cache key soundness
  * How to produce useful error messages when versions don't match

You can't just say "give me access to `/opt/random/thing`". If Vixen doesn't
have a module for it, you can't use it. This is how reproducibility is
enforced for ambient tools.

## Three layers

1. **Constraint**: what the build declares. "I need Xcode >= 26.0". Named,
   version-ranged, human-readable.

2. **Resolution**: what the executor actually has. "Xcode 26.3". The module
   verifies the constraint is satisfied.

3. **Content hash**: the cache key component. The module hashes the relevant
   parts of the installation. This is what makes the cache sound.

## Executor lock file

Analogous to Cargo.lock but for executor profiles. Records:

  * The constraint (Xcode >= 26.0)
  * The resolved version (Xcode 26.3)
  * The content hash (abc123...)

This is novel. No other build system locks executor configurations. But it's
necessary if you want reproducible builds across machines with different
ambient tool versions.

## Cache key implications

If Xcode 26.0 and 26.3 produce different binaries, they have different cache
keys. This is correctness, not a limitation.

Switching profiles (local vs CI vs cloud) means different cache keys for exec
nodes that touch ambient tools. But pure content-addressed parts of the build
graph (source trees, downloaded crates, parsed manifests) share cache keys
across all profiles. Only the exec boundary diverges.

## No "capability" concept

Early designs considered a special "ambient capability" category separate from
content-addressed inputs. Rejected. Everything is declared and goes into the
sandbox. The difference is only provenance:

  * Downloaded by Vixen: transferable, shared CAS
  * Already on executor: non-transferable (stays in local store), but same
    cache key math

The ambient tool module is just the logic that bridges "already installed"
into "content-addressed in the local store".

## Open questions

  * What does the lock file look like concretely? One lock file per project,
    or per profile?
  * How do profiles work at the CLI level? How do you say "build with this
    executor configuration"?
  * How granular is the fingerprint? All of Xcode (20+ GB) or just version
    number + key binaries?
  * Can third parties define new ambient tool modules, or is the set built
    into Vixen?
  * When the locked version isn't available on the executor, what's the
    default behavior? Fail? Re-resolve? User chooses?
  * How do multiple profiles interact with the lock file? Can one lock file
    record several known-good configurations?
