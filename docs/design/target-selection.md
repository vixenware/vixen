# Target Selection

Status: active design note.

This note exists to separate several ideas that keep getting conflated:

- what artifact a build wants to produce
- where build steps are allowed to run
- how a site maps those wishes onto actual runners
- how provider-specific executor inventories are exposed
- what facts the runtime must later recheck before trusting cached results

If these stay mixed together, Vixen cannot have a coherent story for:

- local versus remote execution
- native versus cross compilation
- host-backed toolchains like Xcode or MSVC
- cache reuse across changing executor pools
- provider-specific executor selection

## The Core Split

There are four different layers.

### 1. Target

The target describes what the produced artifact is for.

Examples:

- native
- linux x86_64
- darwin aarch64
- windows x86_64

This is about the output, not where the work runs.

### 2. Placement Policy

Placement policy describes where work is allowed or preferred to run.

Examples:

- prefer local
- require local
- prefer remote
- require remote
- exclude local
- use only executors from some named pool

This is operational policy. It should mostly come from CLI flags, config, or
reusable package defaults, not from random hardcoded host labels in authored
build logic.

### 3. Site-Local Binding

Each installation has some set of available executors.

Those may be:

- the local machine
- SSH hosts
- GitHub runners
- cloud VMs
- some internal runner pool

This mapping is site-local configuration.

Strings like:

- `default`
- `ssh:orb`
- `github:macos-large`
- `local`

are acceptable here as local aliases.

They are not, by themselves, a portable semantic identity.

### 3.5. Provider Package

Executor providers should not be hardcoded into the runtime or standard library.

Provider logic belongs in packages.

Examples:

- SSH fleets
- GitHub runners
- cloud VM pools
- internal executor services

Those packages are responsible for:

- exposing whatever selector fields a provider can advertise before scheduling
- turning provider-specific inventory into candidate executors
- integrating provider-specific placement APIs
- returning ordinary Vixen requests and witnesses to the rest of the system

The runtime should provide the generic machinery:

- discovery
- sandboxed execution
- witness stabilization
- witness validation

It should not bake in:

- provider names
- provider inventory schemas
- provider scheduling conventions
- provider-specific capability metadata

### 4. Resolved Witness

Once target requirements and placement policy have been applied to the available
candidate executors, Vixen must resolve to a concrete environment witness.

That witness is the thing downstream work actually depends on.

For example, a resolved witness for Apple builds might eventually include facts
such as:

- executor operating system and architecture
- selected developer directory
- Xcode version/build number
- selected SDK identity
- resolved tool paths
- hashes of critical directly-invoked binaries

The exact fields are still open, but the important part is the shape:

- authored code requests something abstract
- the runtime resolves that to something concrete
- later `exec` nodes rely on the concrete witness, not the abstract request

## Direction Of Information Flow

There are two different flows, and they point in opposite directions on
purpose.

### Downward: requirements

The build or package declares:

- target requirements
- toolchain requirements
- placement preferences

Example:

- I want a Darwin build
- I prefer local execution
- I need an Xcode-backed C toolchain

### Upward: discovered facts

After a candidate executor has been chosen, discovery runs there and returns the
facts that were actually observed on that executor.

Example:

- this runner is Darwin aarch64
- it currently has Xcode 26.2 selected
- the selected SDK is macOS 15.2
- `clang` resolves to this path

Those observed facts are what later execution is allowed to trust.

## Default Behavior

The default most users want is:

- placement policy: prefer local
- target: native

Meaning:

- use the local executor if possible
- build for whatever platform that executor naturally targets

This should not require special build code.

The same authored build should work:

- on a laptop, using the local executor
- on CI, using some remote executor
- on a remote pool, if policy excludes local execution

At most, the user changes placement policy or config. The build logic should not
need to be rewritten.

## Native Versus Cross Compilation

Placement and target must stay separate or cross compilation becomes impossible
to describe cleanly.

### Native build on a Linux executor

- placement: some Linux executor
- target: native

The produced artifact is for Linux because the chosen executor is Linux.

### Cross build from macOS to Linux

- placement: local macOS executor
- target: linux x86_64

The produced artifact is for Linux even though the work runs on macOS.

The target drives toolchain selection. The placement policy drives where the
work is scheduled.

## Host-Backed Toolchains

Some toolchains are easy to treat as explicit build inputs.

Others are not.

Xcode and MSVC are the motivating examples:

- too large to fully content-address casually
- too tied to host installation state
- too ambient to pretend they are ordinary mounted inputs

So Vixen must support host-backed toolchains as an explicit escape hatch.

That means:

- a build may discover a host-backed toolchain on a chosen executor
- that discovery returns a concrete witness, not just a path
- later `exec` uses that witness
- validation rechecks the relevant facts on the concrete runner

This is weaker than full hermeticity and should be treated with the same
suspicion we would apply to an unsafe escape hatch.

## Stabilization And Stickiness

Abstract requests are not enough for reproducible reuse.

If a request only says:

- darwin
- aarch64
- xcode

and the executor pool contains machines with different Xcode versions, then the
result would otherwise drift from build to build depending on which runner was
chosen.

So Vixen needs stabilized selection:

1. start from an abstract request plus placement policy
2. resolve to a concrete witness
3. persist that witness
4. reuse it on later builds until it no longer validates

This is analogous to package resolution:

- manifest requirement
- concrete lock
- validation / refresh when the world changed

For executors and toolchains, the concrete lock is the resolved witness.

If a provider exposes a coarse pool such as "some macOS runners", and does not
advertise which Xcode version each candidate has ahead of time, then Vixen
cannot pre-select "the same Xcode as last time" before it has probed a runner.

In that case:

- selection may land on a different concrete environment than last time
- witness validation will reject stale reuse
- the result is correctness plus possible under-caching

That is not a failure of witness-based reuse. It is a limitation of the
provider's advertised selector granularity.

## Validation

Validation is not the same thing as selection.

Selection chooses a concrete witness.

Validation later checks whether the actual runner still matches that witness.

If validation fails:

- the build must not silently trust stale cached work
- the cached witness is invalid
- resolution must run again or the build must fail

This is particularly important for unstable remote platforms such as rotating CI
images where the environment may drift underneath a stable alias.

## Cache Story

This is the most important constraint:

raw site-local labels must not be the full portable cache identity.

Why:

- your config may map `darwin` to one runner pool
- someone else's config may map `darwin` to a completely different pool
- the same local alias might mean different concrete environments over time

Therefore:

- local aliases are fine as candidate selectors inside one installation
- they are not enough to justify portable cache reuse by themselves

The meaningful reuse boundary is closer to:

- authored code identity
- explicit build inputs
- target request
- placement policy inputs
- resolved environment witness
- observer identity / captures for `exec`

The important point is that reuse is justified by witnessed facts, not by hoping
that some local alias means the same thing on another installation.

## What Build Authors Should And Should Not Do

Portable authored build logic should prefer:

- target requests
- abstract toolchain requirements
- reusable package APIs

It should avoid hardcoding site-local executor aliases like:

- `ssh:orb`
- `github:macos-large`
- `amos-desktop`

Those belong in site config or provider packages, not in reusable packages that
are meant to travel across installations.

In other words:

- authored code chooses requirements
- config chooses candidate pools
- runtime resolves a concrete witness
- validation keeps that witness honest

## Relation To Current Zoo Sketches

The current zoo sketches already hint at this split, but only partially.

For example, `zoo/vixen-config.default.vx` maps logical platform names to
`executor_class(...)` aliases, and the tri-platform sketches use that mapping to
pick candidate executors.

That is a useful intermediate step, but it is not yet the full story.

What is still missing is:

- a first-class distinction between target and placement
- stabilized concrete environment witnesses
- a portable cache identity not based only on local alias strings
- a principled discovery story for host-backed toolchains
- provider packages that expose pre-selection fields cleanly

## Open Questions

The following are still unresolved:

1. What exact surface syntax should authored code use for target requests?
2. What exact surface syntax should represent placement policy?
3. How much of placement policy belongs in code versus CLI/config?
4. What fields belong in a concrete environment witness for Xcode, MSVC, GCC,
   Rust, Zig, etc.?
5. Which executor and toolchain facts should providers advertise before
   scheduling, and which can only be witnessed after a candidate is chosen?
6. How should a stabilized executor/toolchain witness be materialized or
   inspected by users?
7. How much executor selection should be automatic versus explicitly
   user-directed?

## Provisional Direction

Despite the open questions, the direction should remain:

1. authored code states abstract requirements
2. config, policy, and provider packages provide candidate executors
3. Vixen resolves and stabilizes a concrete witness
4. `exec` and later work depend on that witness
5. validation rejects drift instead of silently reusing stale work

That is the model that can reconcile:

- local versus remote execution
- native versus cross compilation
- host-backed toolchains
- sticky selection
- cache correctness
