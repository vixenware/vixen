# cargo-vix

Reimplementing cargo in vix, starting from the one end of it that runs today:
read a real `Cargo.toml` **and its `Cargo.lock`**, discover the crate's targets,
order the locked packages into a build sequence, and plan the rustc invocation
each one needs.

The lock is the load-bearing input, not the manifest: the 0.1 north star is "an
existing `Cargo.lock` → a built binary", so nothing here resolves a version or
visits an index. The manifest names the root package and its targets; the lock
supplies the resolved graph, and its `checksum` is the sha256 that pins every
fetch (`machine.primitive.fetch-is-pinned`, as amended 2026-07-26).

```
cargo build -p vixen-runtime --bin vx
./target/debug/vx cargo-vix/cargo.vix
```

`vx` is a shim (`vixen-runtime/src/bin/vx.rs`), not a product: it runs one file's
`#[test]` declarations through the same production path the ratchet drives and
reports the checks. It roots the harness `FixtureStore` at the `.vix` file's own
directory, which is why the crate under test lives in `trees/small-crate/` —
`fixture_tree("small-crate")` reads `<root>/trees/<name>`.

## vix compiles and runs a Rust binary

`hello.vix` is rung one of the ladder, and it is green:

```
RUSTC=$(rustup which rustc) cargo test -p vixen-runtime --test exec_tree_mounts
```

That test runs `hello.vix` itself, so the shipped example and the proof are one
source. Running it through `vx` directly needs a machine manifest offering
`Rustc` — the default offers only `Echo`/`Sh`/`ProgressiveSh`, so `vx` refuses
before anything runs. `hello.vix`'s header has the manifest to copy.

The example also **pins the toolchain it needs**:
`rustc: Rustc where { toolchain_range: ">=1.56, <2" }`, because `--edition 2021`
is a demand no argv element expresses. The pin is a version set, not a string, so
`1.89.3` satisfies `>=1.89`. It is checked against the manifest's stated
`toolchain` at binding time — attribution, never verification: nothing probes
the tool to decide anything, and the strongest claim on offer is "we asked for
this, the machine said that, they agree". A machine that states no toolchain
refuses the pin rather than quietly satisfying it. The proof test authors its
own manifest by asking `rustc --version` once, which is the allowed kind of
probe: writing the statement down, not making a decision with it.

There are two spellings because they ask different questions.
`toolchain_range: ">=1.89"` asks an ORDERING question, so both sides have to be
numbers. `toolchain: "22.1std"` asks for one exact string and parses neither
side — which is the only honest question about a tool like Quartus, whose
version has no ordering at all. Arity is free on both sides, so Xcode's `15.2`
and MSVC's `19.38.33130.0` are ordinary versions rather than parse failures.

Writing a range under the exact key is refused at compile time: `toolchain:
">=1.89"` would compare unequal to every version forever and the refusal would
read as the machine's fault.

The exact pin compares TEXT, so `toolchain: "1.89"` does not match a machine
stating `1.89.0` — same version, different strings, and the exact pin was asked
about the string. When the version has an ordering, `toolchain_range: "=1.89"`
is the way to say "exactly this version" and compares as versions. Reach for
the exact pin when there is no ordering to compare.

One sharp edge, kept on purpose: a **nightly** states `1.99.0-nightly`, and
Cargo's prerelease rule means a plain `>=1.89` does not admit it. A nightly is
a different tool from the stable release whose number it carries, so it has to
be named (`>=1.99.0-nightly`) rather than slipped through.

Three processes — write the source, compile it with rustc, run the binary it
produced — and the check reads `hello from vix` off the third one's stdout.

This works because a `Tree` is now spliceable into a command template. The tree
joins the request's `mounts` array and the splice renders as its deterministic
workspace-relative path, so `{src}/src/main.rs` fuses through the ordinary
adjacency rule and needs no path-building surface. The tree's value identity is
IN the request, which is what makes "the same plan over changed sources" a
different demand rather than a stale memo hit.

## Where the road stops now

- ~~**Origin-backed trees cannot be mounted.**~~ **CLOSED.** A `fixture_tree` is
  a lazy handle and cannot be enumerated from its own bytes, so the mount walks
  the effect authority's directory verb instead — the same neutral verb a codata
  drain already had (`EffectCtx::tree_directory`, one witnessed `Directory` read
  per directory and one `TreePath` read per file, so a mounted workspace is
  fully in the receipt). A build walk can now read a workspace off disk:
  `exec_tree_mounts.rs` mounts `small-crate` straight from the fixture root,
  compiles it with real rustc, and runs the binary. Two bounds remain, both in
  the origin seam's vocabulary rather than the mount: `TreeEntryKind` carries no
  executable axis, so an origin-backed file mounts non-executable; and no verb
  reads a symlink's target, so an origin-backed symlink is a loud refusal rather
  than a guess.
- ~~**No declared env**~~ **CLOSED.** `exec … where { env: %{ … } }` lowers to
  an `env` field on the request record, so the environment is declared surface
  and identity-bearing: two execs whose plans agree but whose environments
  differ are two demands, not one with a stale memo hit. A name the capability
  package's command grammar already owns — carved out of the plan, or named as
  one of its env ROLES — is refused rather than resolved by precedence, which
  is also what keeps a target requirement from entering behind
  `collect_exec_requirements`' back. Rung `074-exec-env.vix` is green and
  wired.
- **An exec template cannot contain a literal brace** — `{` always starts an
  interpolation and there is no escape. Harmless for rustc argv; crippling for
  `sh -c` with real shell code (`hello.vix` writes `\173`/`\175` octal escapes
  and lets printf produce the braces).
- **A template splices bare NAMES only**, not expressions: `{sources.tree}` is
  an unbound identifier, so each tree is bound to a `let` first.
- **No rmeta pipelining.** The machinery exists and is proven — rung 128 has a
  subfile consumer finishing before its producer exits — but `Rustc` declares
  `ExecOutputProtocol::ExitOnly`, so nothing is announced. rustc does emit the
  signal (`--json=artifacts` writes `{"$message_type":"artifact",…,"emit":"metadata"}`
  to stderr, metadata before link). Two easy pieces (a rustc artifact protocol;
  read the protocol off stderr, not just stdout) and one hard one: the
  progressive rail only engages when the projection path is a COMPILE-TIME
  CONSTANT, and a build walk computes `lib{name}.rmeta` from lock data.

## Gaps found while writing this

Recorded where they were hit, so they are not rediscovered:

- **Dynamic `Path` construction.** `Path / segment` requires a string LITERAL,
  so a `Path` cannot be rebuilt from computed text. Computed `Path` *values*
  still exist (`Tree.glob` yields them, tree projection accepts them) — you just
  cannot build one. This is why `stdlib/paths.vix` is all queries and why its
  `parent_text` returns text.
- **Closure-return type inference.** A type parameter appearing only in a
  closure's return position cannot be inferred (`cannot infer type argument
  `U``), and instantiation is inference-only, so it cannot be stated at the call
  either. `Option.map` / `and_then` are unwritable today; so is the array twin
  `fn amap<T, U>(xs: [T]) where { f: fn(T) -> U }`.
- **No `#[decode(rename)]`.** Hyphenated TOML keys (`dev-dependencies`,
  `default-features`, `crate-type`) have no spelling that decodes onto a struct
  field, which bounds how much of a manifest can be ingested typed.
- **The build order keys on package NAME alone.** A real lock may hold one name
  at two versions, and its `dependencies` entries then read `"name version"`.
  The Kahn walk would come up short and the truncation reads as a cycle. Keying
  on name+version is the fix; the length check in `orders_the_locked_packages`
  is what keeps it loud meanwhile.
- **No `String.len`, slicing, or right-split**, which is why `stdlib/strings.vix`
  ships no `ends_with`: the "split on the suffix, is the last piece empty" trick
  is wrong for a suffix that overlaps an earlier occurrence (`"a---"` vs `"--"`),
  and that is the shape extensions and operators take.
- **`.vix-mounts` is a reserved name, and the reservation is enforced unevenly.**
  Capture drops that whole top-level subtree, so a process that mounts something
  AND writes new files under `.vix-mounts/` loses them silently. The zero-mount
  case — where the program could not have known the name was taken — is refused
  loudly instead. Closing the gap means threading the materialized file list
  into capture so it can drop exactly what it wrote and no more.
- **No `Map` decode target.** `[dependencies]` cannot decode into
  `Map<String, String>` — only into a struct naming each dependency, which a
  general tool cannot write ahead of time. Not on the critical path (the lock's
  `dependencies` array carries the resolved graph), but it is what a manifest's
  open-keyed tables want.
- **Fixed while writing this: sequences did not decode at all.** Every array
  target fell through to `UnsupportedTarget`, so no `Cargo.lock` could be read
  — `[[package]]` needs `[LockPackage]` and `dependencies` needs `[String]`.
  Rungs 062–066 have no array field between them, and an infallible `decode`
  reports the failure as an `InvalidRequest` machine fault, so the symptom
  never named its cause. See `vixen-runtime/tests/decode_sequences.rs`.
- **Method calls take no `where` clause** ("named method arguments"): a
  multi-argument helper is called method-style with positional arguments
  (`text.replace("-", "_")`), never `.replace("-") where { to: "_" }`.
