# Vixen design TODO

## Discovered inputs and locking
The distinction between declared inputs (known before evaluation) and
discovered inputs (emerge during evaluation). Examples: "what's Rust stable
right now?", "what Xcode does this executor have?", crates.io index queries.
All affect cache keys. All need to be lockable for reproducibility. How does
the language represent these? How are they recorded? How do they interact with
the cache?

Subsumes the old "network / downloads" and "toolchain acquisition" questions —
downloading a hashed artifact is straightforward (goes to the store, cacheable
forever), but resolving *which* artifact to download is the hard part.

## Deserialization annotations
Parsers are built-in and type-driven (resolved). Open question: what syntax
for annotations like rename (`dev-dependencies` → `dev_dependencies`),
defaults, flattening, etc.?
