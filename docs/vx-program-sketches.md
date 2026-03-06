# Vixen Program Sketches

This document is for pressure-testing the language surface against the kinds of
programs Vixen actually needs to express.

The point is not to defend a specific syntax feature in isolation. The point is
to sketch realistic rule logic and see what feels natural, awkward, or missing.

Current constraints for these sketches:

- no Kajit-backed semantics yet
- no new interpreter design yet
- no methods assumed
- no references assumed
- no closures
- no recursion
- Rust-like syntax where possible

That means these sketches intentionally lean on:

- plain functions
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
            let node_id = format.compile_node_id(krate.crate_name, krate.crate_type);

            emit.node(Node {
                id: NodeId { value: node_id },
                kind: NodeKind::Action,
                label: format.concat(["compile ", krate.crate_name]),
                metadata: {
                    "crate_name": krate.crate_name,
                    "crate_type": "bin",
                },
            });
        }
    }
}
```

What this suggests:

- method syntax is not necessary for basic planning code
- explicit namespace functions are readable enough
- ambient output emission is more natural than rebuilding a `RulePlan` value on
  every call

## Sketch 2: Plan compile edges from dependency closure

This tests multi-step graph logic.

```rust
fn plan_edges(graph: CrateGraph) -> Unit {
    for krate in graph.crates {
        let compile_id = format.compile_node_id(krate.crate_name, krate.crate_type);

        for dep in krate.deps {
            let dep_krate = rust.lookup_crate(graph, dep.crate_id)?;
            let dep_id = format.compile_node_id(dep_krate.crate_name, dep_krate.crate_type);

            emit.edge(Edge {
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
- lookup-style builtins will matter
- nested `for` plus explicit helper calls is still readable without methods

## Sketch 3: Validate manifest-derived input

This tests domain diagnostics instead of only graph emission.

```rust
fn validate_registry_dependencies(graph: CrateGraph) -> Result<Unit, InputError> {
    for krate in graph.crates {
        match krate.source {
            CrateSource::Registry { name, version, checksum } => {
                let exists = cargo.registry_package_exists(name, version)?;
                if !exists {
                    return Err(InputError::MissingRegistryCrate {
                        name: name,
                        version: version,
                    });
                }

                let valid = cargo.registry_checksum_matches(name, version, checksum)?;
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
            out = list.push(out, krate);
        }
    }

    out
}
```

What this suggests:

- without methods or references, collection-building becomes reassignment-based
- that is workable, but more verbose than mutation syntax
- if this pattern appears constantly, the language may want a more direct story
  for local collection mutation

## Sketch 5: Construct and update a local record

This tests whether field assignment feels okay without methods.

```rust
fn make_compile_node(krate: CrateNode) -> Node {
    let mut node = Node {
        id: NodeId { value: "" },
        kind: NodeKind::Action,
        label: "",
        metadata: {},
    };

    node.id = NodeId {
        value: format.compile_node_id(krate.crate_name, krate.crate_type),
    };
    node.label = format.concat(["compile ", krate.crate_name]);
    node.metadata = map.insert(node.metadata, "crate_name", krate.crate_name);
    node.metadata = map.insert(node.metadata, "crate_type", "bin");

    node
}
```

What this suggests:

- field assignment feels natural
- map updates without methods or special syntax are clunky
- maps may want dedicated convenience forms, or we may decide maps are mostly
  constructed once and rarely updated incrementally

## Immediate Questions Raised by These Sketches

1. Should local collection mutation be direct syntax, or purely reassignment via
   value-returning functions?
2. Is ambient `emit.*` the right model for output, or should planning functions
   explicitly return plan fragments?
3. Do maps need better incremental update syntax than repeated `map.insert(...)`
   reassignment?
4. Is exact-type `?` sufficient for rule code, or do these examples still feel
   too noisy?
5. Which builtins/helpers are fundamental enough to count as part of the core
   language surface?

## Current Read

The strongest thing these sketches show is:

- method syntax is not necessary to get started
- references are not necessary to express planning logic
- the real pressure point is not methods, it is ergonomic local mutation for
  collections and records

That suggests the next syntax discussion should focus less on `self` and more
on:

- local mutation model
- output-emission model
- error/reporting model
