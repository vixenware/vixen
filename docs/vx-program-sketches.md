# Vixen Program Sketches

This document is for pressure-testing the language surface against the kinds of
programs Vixen actually needs to express.

The point is not to defend a specific syntax feature in isolation. The point is
to sketch realistic rule logic and see what feels natural, awkward, or missing.

Related experiments:

- [vx-program-sketches-styx.md](/Users/amos/bearcove/vixen/docs/vx-program-sketches-styx.md)
  is the deliberately aggressive Styx-heavy pressure test.
- [vx-program-sketches-do-end.md](/Users/amos/bearcove/vixen/docs/vx-program-sketches-do-end.md)
  isolates `do/end` block syntax without bundling in every other stylistic
  decision.
- [vx-program-sketches-minimal-forms.md](/Users/amos/bearcove/vixen/docs/vx-program-sketches-minimal-forms.md)
  explores a more holistic minimal grammar with `do/end`, `:=`, and
  payload-shaped calls and headers.

Neither note is the current baseline.

Current constraints for these sketches:

- no Kajit-backed semantics yet
- no new interpreter design yet
- method syntax allowed
- no user-defined methods assumed
- no references assumed
- no closures
- no recursion
- Rust-like syntax where possible

That means these sketches intentionally lean on:

- plain functions
- value-receiver methods
- `let` / `mut`
- `if`
- `for .. in ..`
- `match`
- explicit namespace-qualified operations
- explicit output emission

## Sketch 1: Plan compile nodes for binary crates

This is the baseline planning shape.

```rust
fn plan_crates(graph: CrateGraph) -> Unit {
    for krate in graph.crates {
        if krate.crate_type == CrateType::Bin {
            let node_id = format::compile_node_id(krate.crate_name, krate.crate_type);
            let metadata = {}
                .insert("crate_name", krate.crate_name)
                .insert("crate_type", "bin");

            emit::node(Node {
                id: NodeId { value: node_id },
                kind: NodeKind::Action,
                label: `compile ${krate.crate_name}`,
                metadata: metadata,
            });
        }
    }
}
```

What this suggests:

- value-receiver methods help most with collection and record-adjacent code
- explicit namespace functions are still fine for host/domain operations
- ambient output emission is more natural than rebuilding a `RulePlan` value on
  every call

## Sketch 2: Plan compile edges from dependency closure

This tests multi-step graph logic.

```rust
fn plan_edges(graph: CrateGraph) -> Unit {
    for krate in graph.crates {
        let compile_id = format::compile_node_id(krate.crate_name, krate.crate_type);

        for dep in krate.deps {
            let dep_krate = graph.lookup_crate(dep.crate_id)?;
            let dep_id = format::compile_node_id(dep_krate.crate_name, dep_krate.crate_type);

            emit::edge(Edge {
                from: NodeId { value: dep_id },
                to: NodeId { value: compile_id },
                kind: EdgeKind::Always,
                metadata: {},
            });
        }
    }
}
```

What this suggests:

- exact-type `?` is useful even without traits
- lookup-style domain methods or builtins will matter
- nested `for` plus helper calls is readable with value-receiver methods and no
  references

## Sketch 3: Validate manifest-derived input

This tests domain diagnostics instead of only graph emission.

```rust
fn validate_registry_dependencies(graph: CrateGraph) -> Result<Unit, InputError> {
    for krate in graph.crates {
        match krate.source {
            CrateSource::Registry { name, version, checksum } => {
                let exists = cargo::registry_package_exists(name, version)?;
                if !exists {
                    return Err(InputError::MissingRegistryCrate {
                        name: name,
                        version: version,
                    });
                }

                let valid = cargo::registry_checksum_matches(name, version, checksum)?;
                if !valid {
                    return Err(InputError::ChecksumMismatch {
                        name: name,
                        version: version,
                    });
                }
            }
            CrateSource::Path { .. } => {}
        }
    }

    Ok(())
}
```

What this suggests:

- Vixen rule code needs a clean way to distinguish input/domain diagnostics from
  internal failures
- `Result` plus `return Err(...)` can already express a lot
- exceptions are not obviously required for this class of logic

## Sketch 4: Build a filtered list without closures

This tests whether the lack of closures is survivable.

```rust
fn binary_crates(graph: CrateGraph) -> List<CrateNode> {
    let mut out = [];

    for krate in graph.crates {
        if krate.crate_type == CrateType::Bin {
            out = out.push(krate);
        }
    }

    out
}
```

What this suggests:

- value-receiver methods make local collection building much less ugly
- reassignment is still explicit, which preserves the value-semantics story
- the remaining open question is whether explicit reassignment is pleasant
  enough, or whether some additional syntax is warranted later

## Sketch 5: Construct and update a local record

This tests whether field assignment feels okay without methods.

```rust
fn make_compile_node(krate: CrateNode) -> Node {
    let metadata = {}
        .insert("crate_name", krate.crate_name)
        .insert("crate_type", "bin");

    Node {
        id: NodeId {
            value: format::compile_node_id(krate.crate_name, krate.crate_type),
        },
        kind: NodeKind::Action,
        label: `compile ${krate.crate_name}`,
        metadata: metadata,
    }
}
```

What this suggests:

- builder-style value code often reads better than incremental field mutation
- chained map updates are much nicer with value-receiver methods
- field assignment is still useful, but it no longer feels like the only escape
  hatch

## Immediate Questions Raised by These Sketches

1. Should local collection mutation be direct syntax, or purely reassignment via
   value-returning functions?
2. Is ambient `emit::*` the right model for output, or should planning functions
   explicitly return plan fragments?
3. Are chained value-receiver methods enough for maps and lists, or do we still
   want additional syntax for local updates?
4. Is exact-type `?` sufficient for rule code, or do these examples still feel
   too noisy?
5. Which builtins/helpers are fundamental enough to count as part of the core
   language surface?

## Current Read

The strongest thing these sketches show is:

- value-receiver methods improve the feel of the language substantially
- references are not necessary to express planning logic
- the real pressure point is now less about methods and more about where we
  still want explicit reassignment, output emission, and typed diagnostics

That suggests the next syntax discussion should focus less on `self` and more
on:

- local mutation model
- output-emission model
- error/reporting model
