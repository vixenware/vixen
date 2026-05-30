# First-class CLI literals

## Why

Build code shells out. In every other language that means an array of opaque
strings — `["cc", "-c", "-O2", src, "-o", out]` — that nothing understands: no
completion, no validation, no idea which elements are inputs to mount, no cache
key. The `command-model` engine already knows all of that for a given tool
(spelling, value domains, the input/output `Kind` of every token, the
content-addressed effective command, cache-equivalence). A **CLI literal** makes
that knowledge first-class in Vixen source: a command you write is *typed by its
tool's grammar*, the way the rest of the language is typed.

This is the ceiling of command-model. The cache key was only the first reader of
the model; the literal is the second and larger one — it turns the grammar into
a type, the editor into an assistant, and `exec` into something that knows its
own inputs.

The closest prior art is Rust's `sqlx::query!`, which type-checks embedded SQL
against a live database schema before the program runs. We do the same for
command lines against a tool grammar — plus the literal's *value* carries the
effective command (mounts + identity), which `sqlx` has no analogue for.

## Syntax

A CLI literal is a **sigil**, an in-scope **grammar name**, and a **square-
bracketed** body:

```vixen
let { cc } = import "@vixen/grammar-cc"

let obj = @cc[ -c -O2 -I src main.c -o $out ]
```

- **Sigil.** `@` and `~` are both free (paths are now provenance-carrying
  strings, so `@` is no longer a path sigil). A sigil is *required* because
  `cc[…]` already means array indexing (`cc[0]`); `@cc[…]` cannot. Open: `@`
  vs `~` vs a doubled delimiter that needs no sigil (`cc[[ … ]]`) — see
  [Open questions](#open-questions).
- **Grammar name.** `cc` is an ordinary identifier that must resolve to a
  `Grammar` **imported from a package** (`@vixen/grammar-cc`). It names the
  grammar *and* (via the grammar) the program to run. You cannot target a
  grammar defined in the module currently being checked — the checker needs the
  grammar already resolved, and grammars travel as their own packages.
- **Square brackets** delimit the body. Curly is taken (blocks/structs), `%{…}`
  is taken; square brackets are free for this.

### The interior is a different language

Inside the brackets the tokens are **command words, not Vixen** — `-c`, `-O2`,
`main.c` are bare, exactly as on a command line. The lexer enters a **command
mode** (the same foreign-content technique as frontmatter fences) and lexes
words until the matching `]`, with three exceptions:

- **Double-quoted strings** — `-DMSG="hello world"` — group a value containing
  spaces, just like a shell. The quotes are part of the lexical grouping, not
  the value.
- **`$ident` interpolation** — `-o $out` — splices the Vixen value bound to
  `out`. Deliberately shell-shaped: `$name` reads as "the variable" to anyone
  who has used a shell.
- **`{ expr }` interpolation** — `-I {workspace.join("inc")}` — splices an
  arbitrary Vixen expression where a bare `$ident` is not enough.

Everything else is a literal command token. (Escaping rules for `$`, `]`, and
quotes inside the body are an open detail.)

## The grammar is the type

When the checker sees `@cc[ … ]`, it resolves `cc` to its `Grammar` and checks
the body against it — the same engine that powers caching, run statically.

**Static parts are checked.** Unknown flags, values outside a `ValueDomain.OneOf`
(`-std=c++99`), a flag used in a mode it is not legal in — all become
**diagnostics**, surfaced as early as possible: during lowering, during JIT
evaluation, in the LSP as you type, and in CI. (Vixen is JIT-evaluated, so this
is not strictly "compile time," but it is caught long before the command would
run.)

**Holes are typed by position.** This is the crux. Each interpolation hole sits
at a grammar position with a `ValueDomain`, and that domain becomes the *expected
type* of the spliced Vixen expression:

| Position | Grammar says | Hole must be |
| --- | --- | --- |
| `-I {x}` | `ValueDomain.Path { dir: true }`, `Kind::Input` | a directory path (or list of them) |
| `-o {out}` | `ValueDomain.Path { dir: false }`, `Kind::Output` | a single output path |
| `-std={v}` | `ValueDomain.OneOf { … }` | one of the allowed strings |
| trailing `{src}` | positional `source`, `Kind::Input` | a source path (or list) |

A hole whose Vixen expression doesn't match its position's domain is a type
error — `@cc[ -o $dir ]` where `dir: Dir` fails because `-o` wants a file. This
is "type-safe as the rest of Vixen": the grammar's value domains *are* the type
system for command lines, and the holes are where Vixen's own type system meets
it.

## The value: an effective command that knows its own inputs

A CLI literal evaluates to a **command value** — the effective command from
`command-model`: canonical argv, the declared env it consumes, the
content-addressed cache key (compat-normalized), and, crucially, the set of
**inputs and outputs discovered from the grammar's `Kind` classification**.

This closes a gap that was previously nebulous: `exec` was imagined to take "an
array of typed exec-path things" (string | path | …) so it could auto-mount the
paths, but it was never clear how the typing or the mounting was specified. The
CLI literal answers it precisely:

- The grammar classifies every token's `Kind` — `Input{dir}` (mount read-only,
  content-hash into the key), `Output{dir}` (writable target, not an input
  hash), `Text` (never mounted), `InOut` (read + rewritten).
- So `exec(@cc[ … ])` derives its **mount set** and its **cache identity**
  straight from the literal. No hand-wired input lists; the dependency edge and
  the sandbox mounts fall out of the grammar.

`exec`'s signature stops being `exec(Array[ExecPath])` with a fuzzy element type
and becomes `exec(Command)`, where a `Command` is exactly what a CLI literal
produces — carrying its argv, its mounts, and its identity.

## Tooling

All of it reads the same engine; every grammar authored once pays off across all
of these.

- **LSP intelligence** — completion from `ValueDomain`, hover from the grammar's
  `doc` strings, diagnostics = the static checks above, and a codelens showing
  the effective cache key and the inputs the command will mount.
- **Why-did-it-rebuild** — `diff` over two effective commands pinpoints exactly
  which arg / env / input changed the identity. Surfaced when a cached build
  misses.
- **Playground** — a small web UI (vite + ts + react + vox): paste or type a
  command, watch it flow parse → canonical → effective → compat live, see the
  cache key and the mount set. Doubles as the grammar-authoring/debugging tool.

(`explain` stays an internal source for hover/codelens text; it is not a
product surface on its own.)

## Architecture

- Grammars are authored in Vixen (`*.vx`, against `@vixen/command-grammar`) and
  distributed as packages. The **parser-only loader** (`command-grammar-vx`)
  lowers a grammar package to a `command_model::GrammarSet` with no evaluator.
- The Vixen **checker consults `command-model`** to check a CLI literal: it
  resolves the literal's grammar identifier to the loaded `Grammar`, parses the
  body, validates the static parts, and types the holes. Grammar resolution is
  therefore part of checking — acceptable precisely because grammars are
  imported packages (resolved dependencies), never the module under check.
- At evaluation, the literal produces the effective command; `exec` consumes it.

## Prior art

- **`sqlx::query!` (Rust)** — compile-time type-checking of an embedded literal
  (SQL) against an external schema (a live DB). The direct analogue; we swap the
  DB schema for a tool grammar and add a cache-keyable value.
- **Racket `#lang` / reader macros** — the gold standard for first-class
  embedded sublanguages with full tooling; more general than we need but the
  right mental model for "a different language inside the brackets."
- **Elixir/Ruby sigils** (`~w`, `%w`) — the sigil-plus-delimiter ergonomics.
- **F#/Scala type providers** — types materialized from an external schema,
  checked statically.

## Open questions

1. **Sigil.** `@cc[ … ]` vs `~cc[ … ]` vs a doubled delimiter (`cc[[ … ]]`) that
   needs no sigil because `[[` can't be indexing. Leaning `@` (free, short), but
   not locked.
2. **Hole syntax.** `$ident` for the common case (shell feel, which we want) and
   `{ expr }` for arbitrary expressions — confirm we want both, and settle
   `${ … }` vs `{ … }` for the general form.
3. **Splatting.** A hole bound to `Array[Path]` in an `-I`/positional position
   should expand to multiple tokens (`-I a -I b …`). Define how repetition vs a
   single multi-valued token is expressed, driven by the flag's `Policy`.
4. **Escaping.** How to write a literal `$`, `]`, or quote inside the body.
5. **Program vs grammar name.** When the program binary differs from the grammar
   (cross-compilers, wrappers), how the literal names the program — a grammar
   field, or a form like `@cc as "aarch64-linux-gnu-gcc"[ … ]`.
