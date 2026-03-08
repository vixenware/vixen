# Vixen design TODO

## Error handling
What happens when an `exec` fails? When a file doesn't exist in a tree?
When deserialization fails? Is there a Result type? Do errors propagate
implicitly? Is there try/catch or something else?

## Closure serialization
Closures with captured values must be serialized to ship to remote executors.
What does the wire format look like? How are function references serialized?
What happens with closures that capture other closures?

## Early cutoff implementation
The revalidation model says "if the result is unchanged, early cutoff prevents
downstream recomputation." How does equality comparison work for large trees?
Is it hash-based? What about streams — can you early-cutoff a stream?

## std.rust.workspace internals
Feature resolution, build scripts, proc macros, cfg handling, edition
handling, target-specific dependencies. This is where the cargo-replacement
complexity actually lives.
