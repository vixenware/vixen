# Development Conventions

Avoid fully qualified paths (for example `std::process::Output` or `std::path::Path`)
when a local `use` is appropriate.

Instead of:

```rust
fn process_file(path: &std::path::Path) {
    // ...
}
```

Do:

```rust
use std::path::Path;

fn process_file(path: &Path) {
    // ...
}
```

Also, don't have imports inside functions/sub-scopes. Try to keep them all at
the top of the file.

Exception: platform-specific imports inside `#[cfg(...)]` branches are allowed
when they only apply to one platform path and improve clarity (for example
`use std::env::consts::OS;` inside a non-macOS branch).

## Module ordering

Keep module file layout in this order:
  1. `//!` module docs
  2. `use` directives
  3. `mod` child modules
  4. type definitions
  5. functions
  6. `#[cfg(test)] mod tests`
  
## Public interface

Do not make mods `pub` by default. Keep them private or `pub(crate)`, and re-export
important types/functions from the top-level.

Instead of:
```rust
pub mod config;
```

Do:
```rust
mod config;
pub use config::Config;
```

Think twice about exporting something from a crate. Do we truly need it?

When it comes to submodules, group `mod foo` with `pub use foo::blah`:

Instead of:
```rust
mod config;
mod types;

pub use config::Config;
pub use types::Error;
```

Do:
```rust
mod config;
pub use config::Config;

mod types;
pub use types::Error;
```

## Tests

Prefer unit tests (under `crate/src/`) than integration tests (under `crate/tests/`)
because 1 integration tests = 1 more binary to check/clippy/compile/run.

The right pattern is, in `crate/src/lib.rs`:

```rust
//! My module

// all the code

#[cfg(test)]
mod tests;
```

In `crate/src/tests.rs`:

```rust
#[test]
fn my_test() {
    // IAMA unit test AMA!
}
```

Rationale: it keeps `lib.rs` shorter, easier to edit. If `tests.rs` gets too long, we
can give it submodules as
 - `crate/src/tests/category1.rs`
 - `crate/src/tests/category2.rs`
..etc

## macOS App Workflow

`apps/mac` contains the `Vixen.app` FSKit host used by `vx-vfs`.

1. Install `xcodegen` (`brew install xcodegen`)
2. Generate project from `/Users/amos/.codex/worktrees/19d8/vixen/apps/mac/xcode` with `xcodegen generate`
3. Build the `Vixen` scheme in Xcode
4. Copy `/Applications/Vixen.app` and enable the extension in System Settings

## Bumping Roam

When updating Vixen to a specific Roam revision:

1. Ensure the Roam commit is pushed to GitHub first.
   If the SHA only exists locally, Swift package resolution will fail.
2. Get the Roam commit SHA (for example from your local checkout):
   `git -C /Users/amos/bearcove/roam rev-parse HEAD`
3. Run:
   `cargo xtask bump-roam <sha>`
4. This updates Swift-side pins only:
   - `apps/mac/Package.swift`
   - `apps/mac/xcode/project.yml`
5. Rebuild and reinstall the macOS app/extension:
   `just apps/mac/install`
6. If install fails with write/lock errors under `apps/mac/xcode/build/`
   (for example `.cargo-lock` permission issues), remove the build dir and retry:
   `rm -rf apps/mac/xcode/build/`
   `just apps/mac/install`
7. If FSKit appears stuck on stale state, restart FSKit daemons:
   `sudo killall fskitd fskit_agent`
   This requires elevated privileges; it must be user-ran.

Rust-side bumping is usually not needed in local development here because
`Cargo.toml` uses `[patch."https://github.com/bearcove/roam"]` path overrides
to the local Roam checkout.
