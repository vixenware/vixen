+++
title = "Big Ideas"
insert_anchor_links = "heading"
weight = 10
+++

# Vixen Big Ideas

This page is an overview, not a normative rulebook. The goal is to explain
what Vixen is for and why the rest of the specification looks the way it does.

> v[ideas.overview]
>
> Vixen is a lazy, demand-driven language for build and execution
> orchestration. Its design centers on explicit values, explicit boundaries,
> deterministic results, and a runtime that can eventually analyze authored
> Vixen and compile straight-line kernels without changing language semantics.

## Function, application, and entry

Vixen should keep the word `function`, but it needs more precise operational
vocabulary around function use.

- a `function` is a reusable parameterized computation
- an `application` is source syntax such as `f(x)`
- an `entry` is the runtime act of actually entering a computation to satisfy
  some demand

The important distinction is:

> In Vixen, function application does not imply immediate entry.

That is the mental shift this page is trying to teach. A function is still a
function. What changes is that writing `f(x)` does not mean "evaluate the
arguments, jump into the body, and run it now." It means a computation has been
applied and is now available to be entered if some observation demands it.

## The magic is selective demand

The thing that is easy to miss on first read is that Vixen syntax does not
imply eager execution.

If you see a product, an array, or a closure that mentions several expensive
subcomputations, that does not mean all of them have to run right now. Syntax
describes available structure. Demand decides what computations actually have
to be entered.

The old eval3 examples are still the shortest way to see that.

### Example 1: dead work stays dead

```vixen
fn bomb() -> U64 (
  fail `bomb`
)

fn main() -> U64 (
  let pair = (41 + 1, bomb());
  pair._0
)
```

This returns `42`.

The important part is not the arithmetic. The important part is that `bomb()`
exists in the program and still never has to run, because the only demanded
part of `pair` is field `0`.

That is the first "this is not an eager language" moment. A value can contain
latent work without forcing it just because the syntax mentions it.

### Example 2: closure application does not force unused siblings

```vixen
fn bomb() -> U64 (
  fail `bomb`
)

fn main(n: U64) -> U64 (
  let add_n = (x) => x + n;
  let pair = (add_n(bomb()), add_n(2));
  pair._1
)
```

For `n = 10`, this returns `12`.

What matters:

- one closure is allocated
- one captured environment is allocated
- the left application never has to be entered
- only the demanded sibling application has to be entered

The closure is not some compiler-only fiction. It is an ordinary value with
identity, captures, and callability.

This is a better example than `add_n(1)` and `add_n(2)` because an eager model
would try to evaluate `bomb()` before building the pair. Vixen does not have to
do that, because only the second field is ever demanded.

### Example 3: shared work runs once

```vixen
fn plus_one(n: U64) -> U64 (
  n + 1
)

fn main(n: U64) -> U64 (
  let x = plus_one(n);
  x + x
)
```

For `n = 41`, this returns `84`.

The important part is that `plus_one(n)` is shared. The two uses of `x` do not
mean "run the call twice." They mean "the same node is demanded twice." That
single node is entered once, cached, and reused.

### Example 4: higher-order application stays inside the same model

```vixen
fn bomb() -> U64 (
  fail `bomb`
)

fn apply_if(flag: Bool, f: Fn(U64) -> U64, x: U64) -> U64 (
  if flag (
    f(x)
  ) else (
    0
  )
)

fn main(n: U64) -> U64 (
  let add_n = (x) => x + n;
  apply_if(false, add_n, bomb())
)
```

This returns `0`.

This matters because Vixen is not trying to preserve laziness only for direct
first-order application. Callables are values, closures can be passed around,
and applying through a local helper still fits the same execution model.

Again, the visible point is not the numeric result. The visible point is that
an eager model would try to evaluate `bomb()` before entering `apply_if`. In
Vixen, the helper receives a demandable input, and the branch that would enter
it is never selected.

## Independent work becomes concurrent

Vixen has no `spawn`, `async`, or `await` in the core language. Concurrency
comes from independent demanded structure.

In build code that often looks like ordinary collection work:

```vixen
canonical_requests(crate.dependencies)
  .map((dep) => demand_package(dep) where { config, lock, target, workspace })
  .for_each();
```

That does not mean "iterate in order and block on each dependency." It means
"each demanded dependency can progress independently." The structure of the
program already exposes concurrency.

This is another place where the model feels magical at first. A normal strict
interpreter would need explicit scheduling machinery just to recover this. In
Vixen, the language semantics already expose it.

## Join is how multiple demands meet

The other key idea is `join`.

When the same node is demanded from multiple places, Vixen does not pick one
demand arbitrarily and it does not duplicate the node. It joins the compatible
demands into one stronger demand on the same computation.

For simple scalar observations, the shape is tiny:

```text
NoObs <= NeedValue
join(NoObs, NeedValue) = NeedValue
```

For products, join is fieldwise.

```vixen
let pair = ((1, 2), 3);
pair._0._1
```

That does not demand the whole outer tuple uniformly. It demands:

- field `0` of the outer tuple
- field `1` of the inner tuple
- nothing else

And when the same binding is used twice:

```vixen
let x = plus_one(n);
x + x
```

the two observations on `x` join into one reusable `NeedValue` demand on the
same shared computation.

This is not just an implementation trick. It is the semantic merge point that
lets Vixen preserve laziness, sharing, and determinism at the same time.

## Cargo-style package resolution is the proof case

The cargo resolution sketch is important because it shows these ideas are not
just toy examples.

The file starts by making the intended point directly:

```vixen
// Cargo-style dependency resolution sketch
//
// This is not a Cargo builtin. The only ambient pieces are:
// - Http.get(url)
// - Toml.parse[T](blob)
// - Toml.serialize(value)
// - Json.parse[T](blob)
//
// Everything else is ordinary Vixen code over those primitives.
```

That is the bar. Package resolution is supposed to be expressible in Vixen, not
hardcoded as a special Rust-side subsystem.

The same join idea shows up in the resolver:

```vixen
fn merge_joined_requests(requests: Array[DependencyRequest]) -> DependencyRequest (
  array_fold(array_rest(requests), array_first(requests), merge_requests)
)

fn demand_package(request: DependencyRequest) where {
  config: SparseConfig,
  lock: CargoLock?,
  target: BuildTarget,
  workspace: Tree,
} #discover -> ResolvedCrate (
  keyed(
    package_request_key(request),
    [request],
    ((ctx) => resolve_package_node(ctx) where {
      config,
      lock,
      target,
      workspace,
    }),
  )
)
```

That says:

- requests for the same package key meet at one keyed computation
- the keyed node receives all joined requests
- ordinary Vixen code decides how those requests merge
- the runtime does not need Cargo-specific semantics baked into it

This is why `join`, keyed memoization, and selective demand matter so much.
They are part of the foundation for cargo-shaped workloads.

## A build is still just code

All of this only works if the build side does not fork into a separate DSL.

A Vixen build is still an ordinary Vixen program that returns a `Tree`. That
means:

- abstraction is ordinary language abstraction
- helpers are ordinary functions
- discovery logic is ordinary code
- build composition is ordinary value composition

The build surface is a library and runtime surface layered on top of the
language, not a separate language pretending to look like one.

## Boundaries stay explicit

Real builds need host interaction. Vixen does not hide that. It makes it
explicit:

- `discover` for executor-host observation
- `exec` for sandboxed process execution
- `Blob`, `Tree`, and `Path` for build artifacts and locations

That explicitness is what makes caching, replay, verification, and remote
execution tractable instead of hand-wavy.

## The runtime is supposed to get smarter without changing the model

The long-term goal is not "an interpreter, but faster." The goal is a runtime
that can analyze authored Vixen and decide what can be compiled into straight-
line kernels and what must remain lazy runtime structure.

That only works if the semantics above stay fixed:

- dead work can stay dead
- independent work can progress concurrently
- shared work can be reused
- joined demands can stay explicit
- higher-order code still fits the same machine

If those properties are not preserved, the implementation may get faster and
still cease to be Vixen.
