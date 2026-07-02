# Sketch: building Lua 5.4 in vix

First run-with-it sketch (Amos critiques; nothing here is settled). Goal: make the surface
concrete enough to argue about — every line forces a design decision, catalogued below.

## The program

```vix
// lua.vix — Lua 5.4 for any target the fleet can serve.

use vix::{Tree, Path, Target};
use caps::{Cc, Ar};

/// Lua's sources, pinned by content. `fetch` is pure: the checksum IS the identity;
/// the URL is only a hint about where satisfying bytes may be found.
fn sources() -> Tree {
    let tar = fetch(
        url: "https://www.lua.org/ftp/lua-5.4.8.tar.gz",
        sha256: "4f18…d1",
    );
    extract(tar) / p"lua-5.4.8/src"       // `/` joins Tree x Path; strings NEVER coerce
}

/// One translation unit -> one object file, wherever `cc` lives.
fn object(cc: Cc, src: Tree, unit: Path, defines: [Flag]) -> Tree {
    cc! {
        -O2 -Wall {defines}
        -c {src / unit}                    // grammar knows -c takes the input
        -o {unit.with_ext("o")}            // grammar knows -o names the output
    }
}

/// The interpreter, one target at a time.
pub fn lua(target: Target) -> Tree {
    let cc = Cc::acquire(target);          // observation: toolchain fingerprint pinned
    let ar = Ar::acquire(target);
    let src = sources();

    // Control flow lives in the graph: `target` is demanded, one arm is built.
    let defines = match target.os {
        Linux => [-DLUA_USE_LINUX],
        Macos => [-DLUA_USE_MACOSX],
        _     => [],
    };

    let units = src.glob("*.c")            // method, not infix; enumeration joins the read-set
        .filter(|u| u != p"lua.c" && u != p"luac.c" && u != p"onelua.c");

    // map = parallel fan-out under demand; collect = CANONICAL total order (inv. 13)
    let objs: Tree = units.map(|u| object(cc, src, u, defines)).collect();

    let lib  = ar! { rcs liblua.a {objs / "*.o"} };
    let main = object(cc, src, "lua.c", defines);
    cc! { -o lua {main / "lua.o"} {lib / "liblua.a"} -lm }
}
```

Selection from the edge:

```
$ vix build 'lua(host)'
$ vix build 'lua(aarch64-linux-musl)'     # cross: routes to/uses a capability that has it
```

Demanding both = two graphs sharing `sources()` and its fetch pin; only `Cc::acquire` and
everything downstream of `defines` forks.

## Decisions this sketch forces (the critique agenda)

1. **Capability acquisition surface**: `Cc::acquire(target)` — acquisition as an associated
   function on the capability kind, target as argument, result pinned. Alternatives: an
   explicit broker value threaded from the entry point (`fleet.cc(target)`) — more honest
   about where capabilities COME from, more threading. Which honesty wins?

2. **Grammar-literal exec blocks** (`cc! { … }`): you write the actual command line; the
   capability's command grammar parses it. `{…}` holes are typed splices. Chosen over a
   struct-ish builder (`cc.compile{input:…, out:…}`) because migration is paste-your-Makefile
   and the grammar is the single source of truth anyway. CONTENTION: statically checking the
   literal requires a grammar at typecheck time, but the exact grammar version arrives with
   the acquired capability at graph-build time. Proposal: check against the grammar FAMILY
   (vocabulary floor) statically; re-validate against the pinned exact version at graph
   build — still strictly before exec; a flag unknown to the pinned version is a build error,
   never a runtime one.

3. **Outputs in grammar-literals**: `-o {unit.with_ext("o")}` names an output INSIDE the
   sandbox; the invocation's result is the Tree of grammar-declared outputs. Multi-output
   commands (rustc: rlib + rmeta + d) return a tree keyed by role — projections select
   (`out / "lua.o"`). Is role-keyed-tree the right return shape?

4. **Paths and trees as algebra** (REVISED after critique): paths are a DISTINCT literal
   type — `p"lua-5.4.8/src"` — and strings never coerce to paths. `/` is one join operator
   over Tree x Path (and Path x Path); a multi-segment path literal carries its own interior
   slashes, so the join-once vs join-repeatedly redundancy dissolves: `tree / p"a/b/c"` and
   `tree / seg_a / seg_b` both make sense, and `tree / "a/b"` is a TYPE ERROR. `glob` is a
   method (`src.glob("*.c")`) — the earlier infix `src glob "*.c"` was an S-expression slip,
   inconsistent with everything else. Glob patterns are compile-validated in literal position.

5. **match on target in-graph**: condition demanded, one arm constructed. This is the
   control-flow-in-the-graph story in four lines. Any objection to `match` being exactly
   Rust-shaped?

6. **`-lm` and sysroots**: system libs resolve inside the capability's opaque blob — a Cc
   capability BUNDLES its sysroot (that's what makes cross work: `Cc for aarch64-linux-musl`
   includes the musl sysroot as part of its fingerprint). Right factoring?

7. **`[Flag]` and splicing** `{defines}` into a grammar literal: flags are typed atoms from
   the grammar's vocabulary, not strings. An unquoted `-DLUA_USE_LINUX` in source is parsed
   by the grammar at compile time; a spliced list must be `[Flag]` (typed), so injection by
   string concatenation is unrepresentable. Good? (This kills shell-quoting bugs dead.)

8. **Functions like `sources()` with no arguments**: pure + memoized means it runs once per
   universe, shared by every target. The fetch checksum lives in SOURCE (not the journal) —
   pinned-by-author, not discovered. Contrast: `Cc::acquire` IS journaled. The line between
   "constant in source" and "observation in journal" = who gets to change it (author vs
   refresh). Crisp enough?

9. **Deliberate omissions**: luac, install tree, tests, Windows arms, incremental-lua-dev
   (editing lua sources locally — that's the tracked-tree/read-set path, exercised by the
   libpng sketch better). Also NOT shown: streams (nothing here streams), the solver
   (no deps), fold (no accumulation) — later sketches.

## What the runtime sees (trace of the weld)

`vix build 'lua(aarch64-linux-musl)'` demands `lua(t)` → `Cc::acquire(t)` observation → pin;
`sources()` memo-hit or fetch-by-checksum; `match` demands `target.os`; glob enumerates
(read-set); ~30 `object` nodes materialize as the unit list does (dynamic fan-out), routed to
executors advertising the pinned cc; each is `closure hash + cc fingerprint + read-set(src
files actually opened, incl. negative include probes)`; `collect` orders results canonically;
`ar`/link nodes demand-chain; the final tree's hash is the build's identity. `vix wtf` can
show every one of these edges; `vix why lua` explains any hit or miss.
