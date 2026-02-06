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
