# Vixen Program Sketches: `do/end` Variant

This note explores one specific move:

- blocks stop using braces
- blocks use `do ... end` instead

The goal is to isolate that change and see what it buys us before we bundle it
with other syntax decisions.

This note is deliberately narrower than
[vx-program-sketches-styx.md](/Users/amos/bearcove/vixen/docs/vx-program-sketches-styx.md).
It is trying to answer:

- if blocks move off `{}`,
- does the rest of the surface get easier to design?

## Working Assumptions

This variant assumes:

- `fn`, `if`, `for`, and `match` blocks use `do ... end`
- sequences stay comma-separated
- the call syntax question stays open
- the record/object literal question stays open
- tags are still available as a compact enum surface

To keep the experiment focused, most sketches below use `[]` for lists and
either:

- ordinary paren calls, or
- statement-style calls for obviously effectful operations

The point is not to settle those choices yet. It is to see whether `do/end`
solves the block-vs-data conflict cleanly.

## Sketch 1: Plan compile nodes, minimal `do/end`

```text
fn plan_crates(graph: CrateGraph) -> Unit do
  for krate in graph.crates do
    if krate.crate_type == @bin do
      let node_id = format::compile_node_id(
        krate.crate_name,
        krate.crate_type,
      )

      emit::node(Node{
        id: node_id,
        kind: @action,
        label: `compile ${krate.crate_name}`,
        metadata: {
          crate_name: krate.crate_name,
          crate_type: "bin",
        },
      })
    end
  end
end
```

What this shows:

- `do/end` already removes the ugliest visual conflict
- braces can now feel much more data-shaped
- we can explore lighter record syntax later without also worrying about block
  syntax

## Sketch 2: Same sketch, but more declarative emission

```text
fn plan_crates(graph: CrateGraph) -> Unit do
  for krate in graph.crates do
    if krate.crate_type == @bin do
      let node_id = format::compile_node_id(
        krate.crate_name,
        krate.crate_type,
      )

      emit::node Node{
        id: node_id,
        kind: @action,
        label: `compile ${krate.crate_name}`,
        metadata: {
          crate_name: krate.crate_name,
          crate_type: "bin",
        },
      }
    end
  end
end
```

What this shows:

- `do/end` makes statement-style calls much easier to scan
- once blocks are keyword-delimited, a command-like surface starts to look less
  magical
- this may be the right home for `emit::node` and similar planner operations

## Sketch 3: Plan compile edges

```text
fn plan_edges(graph: CrateGraph) -> Unit do
  for krate in graph.crates do
    let compile_id = format::compile_node_id(
      krate.crate_name,
      krate.crate_type,
    )

    for dep in krate.deps do
      let dep_krate = graph.lookup_crate(dep.crate_id)?
      let dep_id = format::compile_node_id(
        dep_krate.crate_name,
        dep_krate.crate_type,
      )

      emit::edge Edge{
        from: dep_id,
        to: compile_id,
        kind: @always,
        metadata: {},
      }
    end
  end
end
```

What this shows:

- nested loops still read cleanly
- `do/end` makes vertical structure very explicit
- `@always` already feels nicer than `EdgeKind::Always`, independently of the
  block syntax

## Sketch 4: Validate manifest-derived input

```text
fn validate_registry_dependencies(graph: CrateGraph) -> Result<Unit, InputError> do
  for krate in graph.crates do
    match krate.source do
      @registry{name, version, checksum} do
        let exists = cargo::registry_package_exists(name, version)?
        if !exists do
          return Err(MissingRegistryCrate{
            name: name,
            version: version,
          })
        end

        let valid = cargo::registry_checksum_matches(name, version, checksum)?
        if !valid do
          return Err(ChecksumMismatch{
            name: name,
            version: version,
          })
        end
      end

      @path{} do
      end
    end
  end

  Ok(())
end
```

What this shows:

- `match ... do ... end` is plausible
- pattern syntax can still be compact without forcing a full Styx value model
- `do/end` does not require abandoning `Result`/`Err` if we do not want to

## Sketch 5: Build a filtered list

```text
fn binary_crates(graph: CrateGraph) -> [CrateNode] do
  let mut out = []

  for krate in graph.crates do
    if krate.crate_type == @bin do
      out.push(krate)
    end
  end

  out
end
```

Alternative, if method-call mutation remains suspect:

```text
fn binary_crates(graph: CrateGraph) -> [CrateNode] do
  let mut out = []

  for krate in graph.crates do
    if krate.crate_type == @bin do
      push out, krate
    end
  end

  out
end
```

What this shows:

- `do/end` does not force a choice on collection updates
- list syntax can remain conventional while the block story changes radically

## What `do/end` Buys Immediately

- blocks and records no longer compete for the same visual shape
- indentation becomes more meaningful without being mandatory syntax
- data literals can plausibly get lighter later
- statement-style planner operations stop fighting with brace blocks

This is the first experiment so far that seems to solve a real structural
problem without forcing answers to five other syntax questions.

## What `do/end` Does Not Solve

It does **not** settle:

- how function calls should look
- whether records use `:` and `,` or `key value`
- how list literals should be delimited long-term
- whether method calls are first-class surface syntax
- whether `match` arms want `=>`, `do`, or something else

That is a feature, not a bug. It means this experiment is focused.

## Current Read

This feels much more promising than the fully Styx-heavy sketch.

Not because it is safer, but because it moves exactly the thing that was
causing the most visual confusion:

- blocks stop being brace-shaped

That creates room for a more data-oriented expression surface without forcing
Vixen to become fully whitespace-significant or fully config-shaped.

The main next question is not "is `do/end` good?" It is:

- if blocks are `do/end`, how far should records and calls then move away from
  Rust?
