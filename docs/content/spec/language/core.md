+++
title = "Core Language"
insert_anchor_links = "heading"
weight = 20
+++

# Vixen Core Language Specification

## Value semantics

> r[lang.value-semantics]
>
> Every value in Vixen has value semantics. There are no references, no
> borrowing, no ownership. Passing a value to a function does not prevent
> the caller from using it again. There is only `Fn` — no `FnOnce`, no
> `FnMut`. Iterating over an array does not consume it.
>
> There is no way to observe mutation from another binding. Reassignment
> (`x.field = val`) is syntactic sugar for creating a new value with that
> field changed — it shadows the previous binding, it does not mutate
> anything in place.

> r[lang.value-semantics.universal]
>
> Because Vixen has value semantics and no references, every value can be
> compared for equality, hashed, and serialized/deserialized. This
> includes closures (compared and hashed by their function identity and
> captured values) and `F64` (which uses total ordering: `NaN == NaN`,
> `NaN` sorts last, `-0.0 == +0.0`). All types have a universal total
> ordering, which is what enables deterministic `.to_array()` on streams.

## Conventions

> r[lang.naming]
>
> All types are written in UpperCamelCase. All bindings are written in
> snake_case. This is enforced by the evaluator. Type declarations are
> keyword-led (`struct Name`, `enum Name`, `newtype Name`, `type Name = ...`);
> bindings remain snake_case (`let name = Name { ... }`).

## Lexical grammar

### Keywords

> r[lex.keywords]
>
> Reserved words: `fn`, `let`, `type`, `struct`, `enum`, `newtype`,
> `of`, `with`, `where`, `match`, `if`, `else`, `is`, `as`, `import`,
> `return`, `fail`, `true`, `false`.
>
> These cannot be used as binding or type names.

### Identifiers

> r[lex.ident]
>
> Identifiers start with a letter or underscore and continue with letters,
> digits, and underscores. `_` alone is the wildcard pattern — it is not a
> valid binding name.

> r[lex.dollar-ident]
>
> `$`, `$0`, `$1`, ... are reserved closure-shorthand identifiers
> (`r[expr.closure.dollar]`). They are lexed distinctly from ordinary
> identifiers and are only meaningful in closure-shorthand positions.
> Inside a double-quoted string or a backtick command invocation, `$name` /
> `${ expr }` are interpolation holes (`r[lex.string.interpolation]`), not
> closure shorthand — the enclosing literal disambiguates.

> r[lex.ident.case]
>
> UpperCamelCase identifiers denote types. snake_case identifiers denote
> bindings. The evaluator enforces this — using the wrong case is an error.
> Top-level type declarations are keyword-led (`struct Name`, `enum Name`,
> `newtype Name`, `type Name = ...`). Casing still distinguishes type
> names from bindings everywhere else.

### Whitespace

> r[lex.whitespace]
>
> Whitespace (spaces, tabs, newlines) separates tokens but is otherwise
> insignificant. There is no indentation sensitivity.

### Comments

> r[lex.comments]
>
> Line comments begin with `//` and extend to end of line. Block comments
> begin with `/*` and end with `*/`. Block comments nest —
> `/* /* inner */ outer */` is one comment.

### Attributes

> r[lex.attributes]
>
> Attributes use `#[ ... ]` with comma-separated key/value entries:
> `#[rename = "name"]`, `#[tag = "type"]`. Attributes attach
> to the declaration or field that immediately follows them. Multiple
> attributes can stack:
>
> ```vixen
> #[tag = "type"]
> #[rename_all = "kebab-case"]
> enum Shape { ... }
> ```
>
> Attributes may appear before:
>
> - Type declarations (`struct`, `enum`, `newtype`)
> - Struct fields
> - Enum variants
> - Function declarations
>
> `@` in pattern position (`name @ pattern`) is the binding operator. It
> has no other use: paths are ordinary strings distinguished by
> provenance, not by syntax (see the Build and Runtime Standard Library),
> and attributes use `#[...]`.

### Semicolons

> r[lex.semicolons]
>
> Semicolons separate statements inside blocks. A trailing semicolon
> after the last statement is permitted but not required. In blocks, the
> last expression (without a trailing semicolon) is the block's value.
> A bare expression in any other statement position is an unused
> expression statement and is a static error; adding a trailing semicolon
> to the tail expression has the same effect.
>
> At file scope, Vixen uses the same statement-list shape as a block,
> except a source file has no resulting value. Top-level `let`
> bindings and declarations contribute exported names. Top-level
> bare expressions are therefore invalid. `return` remains
> function-only.
>
> Semicolons do not separate positional from keyword arguments. Keyword
> application uses `with`, as described in `r[expr.call.kwargs]`.

### Operators and delimiters

> r[lex.operators]
>
> Arithmetic: `+`, `-`, `*`, `/`, `%`.
>
> Comparison: `==`, `!=`, `<`, `<=`, `>`, `>=`.
>
> Logical: `&&`, `||`, `!` (prefix).
>
> Assignment: `=`, `with=` (compound update).
>
> Range: `..`, `..=`.
>
> Arrow: `=>` (match arms, short closures), `->` (return
> types).
>
> Dot: `.` (field and method navigation).
>
> Pipe: `|>` (free-function / pipeline chaining).
>
> Other: `?` (Option type sugar `T?`; and the fallible-variant suffix —
> `f?` is the auto-derived `Option`-returning form of a poisoning `f`,
> including `as?`), `@`
> (pattern binding), `#` (attribute opener in `#[...]`),
> `:` (named slots — type annotations and field-value pairs), `::`
> (expression-level type ascription, distinct from `as` conversion).

> r[lex.delimiters]
>
> Delimiters: `(` `)`, `[` `]`, `{` `}`.

### Numeric literals

> r[lex.numeric]
>
> Integer literals are sequences of digits, optionally separated by
> underscores for readability: `1_000_000`. They default to `U64`.
> Use type ascription (`::`) to specify a different type: `4::I64`.
>
> Dotted-numeric literals are `SemVer`, not floats — Vixen is a build
> language and versions are the common case. `0.13`, `1.0`, `1.0.0`, and
> `1.0.0-rc.1` are `SemVer` literals (two or three segments, optional
> pre-release / build metadata). A `F64` literal requires the `f` suffix:
> `3.14f`, `0.5f`.

### String literals

> r[lex.string]
>
> Two string forms, on the shell convention:
>
> - **Raw**, single-quoted: `'hello'`. No interpolation; the only escapes
>   are `\'` and `\\`. Every other character — including `$` and `` ` `` —
>   is literal. Best for paths, regexes, and text that carries sigils.
> - **Interpolated**, double-quoted: `"hello"`. Escape sequences `\\`,
>   `\"`, `\n`, `\t`, `\r`, `\0`, `\u{...}` (Unicode code point), plus
>   interpolation (`r[lex.string.interpolation]`).

> r[lex.string.interpolation]
>
> Inside a double-quoted string, `$name` interpolates the binding `name`,
> and `${ expr }` interpolates an arbitrary expression (calls, field
> access, `if`, `match`, nested interpolation). A `$` not followed by an
> identifier start — or a `\$` — is a literal `$`. Each interpolation calls
> `.to_string()` on its value. Single-quoted strings never interpolate, and
> backticks are command invocations (`r[expr.command]`), not strings.

> r[lex.heredoc]
>
> A heredoc is a fenced multi-line string: an opening line of three
> backticks with an optional tag, the content lines, and a closing line of
> three backticks — the same fence Markdown uses. The tag (`c`, `sh`,
> `styx`, …) is a highlighting / tooling hint only and carries no
> semantics. A heredoc interpolates `$name` / `${ ... }` like a
> double-quoted string (`r[lex.string.interpolation]`), and leading
> indentation is stripped to the column of the closing fence. A heredoc
> evaluates to a `String`. Three backticks fence a multi-line *string*; a
> single backtick is a *command invocation* (`r[expr.command]`) — the same
> block-vs-inline split Markdown uses. A non-interpolating raw heredoc
> (single-quote-style) is deferred — `r[lex.heredoc.raw]` TBD.

### Operator precedence

> r[lex.precedence]
>
> From lowest to highest precedence:
>
> | Precedence | Operators              | Associativity | Description                 |
> |------------|------------------------|---------------|-----------------------------|
> | 1          | `\|>`                  | left          | Pipe                        |
> | 2          | `with`                 | none          | Struct update               |
> | 3          | `\|\|`                 | left          | Logical OR                  |
> | 4          | `&&`                   | left          | Logical AND                 |
> | 5          | `==` `!=`              | left          | Equality                    |
> | 6          | `<` `<=` `>` `>=` `^=` `~=` `contains` `in` `matches` | left | Comparison, semver match, membership |
> | 7          | `..` `..=`             | none          | Range                       |
> | 8          | `+` `-`                | left          | Additive                    |
> | 9          | `*` `/` `%`            | left          | Multiplicative              |
> | 10         | unary `-` `!`          | prefix        | Negation, logical NOT       |
> | 11         | `as` `as?` `is` `::`   | left          | Type cast / test / ascription |
> | 12         | application            | left          | Unary application           |
> | 13         | postfix chain          | left          | `.name`                     |
>
> Assignment (`=`) is a statement, not an expression. The postfix chain
> associates left-to-right:
>
> ```vixen
> x.y.z     // means: ((x).y).z
> f(x).y    // group the application result before navigating it
> ```
>
> `|` is not an expression operator; it appears in type declarations
> (sum shorthand) and in patterns (or-patterns).

## Declaration grammar

### Bindings

> r[decl.binding]
>
> A binding associates a pattern with a value:
> `let name = expr`. An optional type annotation can be added:
> `let name: Type = expr`. Bindings are always introduced with `let`.

> r[decl.binding.top-level]
>
> Bindings may appear at file scope as well as inside blocks. A top-level
> `let` creates an immutable module binding and is exported like any
> other top-level name.

> r[decl.binding.destructure]
>
> Bindings can destructure: `let { first, last } = name`. The type is
> inferred from the right-hand side. Tuple destructuring:
> `let (a, b) = pair`.

### Type declarations

> r[decl.type]
>
> Type declarations are keyword-led:
> `struct Name { ... }`, `enum Name { ... }`, `newtype Name of T`.
> The `type` keyword is reserved for aliases:
> `type Name = Foo | Bar`.

### Struct declarations

> r[decl.struct]
>
> ```vixen
> struct Point {
>   x: F64,
>   y: F64,
> }
> ```
>
> Fields are comma-separated. Trailing commas are allowed.

> r[decl.struct.defaults]
>
> Fields can have default values:
>
> ```vixen
> struct Config {
>   optimize: Bool = false,
>   jobs: U64 = 8,
>   target: String,
> }
> ```
>
> When constructing, fields with defaults can be omitted:
> `Config { target: "x86_64" }` uses the defaults for `optimize` and
> `jobs`. Providing a value overrides the default. Defaults are
> evaluated at construction time.

> r[decl.struct.nested]
>
> A slot type can be declared inline immediately after `:`:
>
> ```vixen
> struct CargoToml {
>   package: struct Package {
>     name: String,
>     edition: String,
>   },
> }
> ```
>
> The nested type is accessible as `CargoToml.Package`.

### Enum declarations

> r[decl.enum]
>
> ```vixen
> enum Shape {
>   struct Circle {
>     radius: F64,
>   },
>   struct Rect {
>     width: F64,
>     height: F64,
>   },
>   struct Empty,
> }
> ```
>
> Variants are comma-separated. Every variant is an explicit named struct
> declaration. Zero-sized variants can omit the body: `struct Empty`.

### Newtype declarations

> r[decl.newtype]
>
> ```vixen
> newtype Blake3 of String
> ```
>
> Creates a distinct type backed by an existing type. Requires explicit
> `as` conversion.

### Function declarations

> r[decl.fn]
>
> ```vixen
> fn add(a: U64, b: U64) -> U64 (
>   a + b
> )
> ```
>
> Parameter types are always annotated. The return type may be omitted,
> in which case it defaults to `()` (the empty tuple). The last
> expression in the body is the return value. `return` can be used for
> early exit. A function body that ends with a statement (including an
> empty body) implicitly returns `()`.
>
> Function application is unary. A declaration with multiple positional
> parameters is sugar for one tuple payload that is destructured by the
> parameter list. So:
>
> ```vixen
> fn add(a: U64, b: U64) -> U64
> ```
>
> is called as:
>
> ```vixen
> add(1, 2)
> ```
>
> and is equivalent in call shape to:
>
> ```vixen
> fn add_pair((a, b): (U64, U64)) -> U64
> ```

> r[decl.fn.nested]
>
> Function declarations may appear inside blocks as well as at file
> scope. A nested `fn` closes over its enclosing bindings by value, just
> like a closure. The name is bound in the enclosing block scope.

> r[decl.fn.destructure]
>
> Parameters can use destructuring patterns. The type annotation applies
> to the whole pattern:
>
> ```vixen
> fn distance((x1, y1): (F64, F64), (x2, y2): (F64, F64)) -> F64 (
>   ((x2 - x1) * (x2 - x1) + (y2 - y1) * (y2 - y1)).sqrt()
> )
>
> fn greet({ first, last }: Name) -> String (
>   "Hello ${first} ${last}"
> )
> ```

> r[decl.fn.generic]
>
> Generic parameters use square brackets after the function name:
>
> ```vixen
> fn first[T](items: Array[T]) -> T (
>   items.first()
> )
> ```

> r[decl.fn.keyword-args]
>
> Keyword parameters live in a separate `with { ... }` block after the
> positional parameter list. Keyword-ness and default-ness are separate:
> a keyword parameter may be required (no default) or optional (with a
> default).
>
> ```vixen
> fn greet(name: String) with {
>   cheerful: Bool = false,
> } -> String (
>   if cheerful ("Hey ${name}!!!") else ("Hello ${name}")
> )
>
> greet("Amos") with { cheerful: true }
> ```

### Scope

> r[decl.scope]
>
> A source file is a statement list with module export semantics.
> Blocks are statement lists with expression semantics. In both places,
> declarations and bindings may appear alongside tail expressions.
>
> Nested type declarations are allowed in statement position. They are
> scoped to the enclosing block, and when nested inside a named
> declaration they are additionally reachable through that declaration's
> namespace path.

## Expression grammar

### Literal expressions

> r[expr.literal.int]
>
> Integer literals: a sequence of digits, optionally separated by
> underscores. Default type is `U64`. Use type ascription to specify a
> different type: `42`, `1_000_000`, `4::I64`.

> r[expr.literal.float]
>
> Float literals are digits with a decimal point and a trailing `f`
> suffix: `3.14f`, `0.5f`, `1_000.0f`. Default type is `F64`. A
> dotted-numeric literal *without* the `f` suffix is a `SemVer`, not a
> float (`r[lex.numeric]`).

> r[expr.literal.bool]
>
> Boolean literals: `true`, `false`. Type is `Bool`.

> r[expr.literal.string]
>
> A string literal is single-quoted (raw) or double-quoted (interpolated):
> `'a raw string'`, `"hello world"`. See `r[lex.string]` and
> `r[lex.string.interpolation]` for the lexical detail.

> r[expr.literal.interpolation]
>
> A double-quoted string interpolates `$name` (a binding) and `${ expr }`
> (any Vixen expression — calls, field access, `if`, `match`, nested
> interpolation):
>
> ```vixen
> let msg = "found ${results.len()} results in ${elapsed}s"
> let label = "status: ${if ok ('pass') else ('fail')}"
> ```
>
> `$name` is shorthand for `${ name }`; `${ }` is the general form.

### Command invocations

> r[expr.command]
>
> A command invocation is written in backticks:
> `` `$tool -c main.c -o out/x.o` ``. Unlike a string, the content is parsed
> as command-line **tokens** — a leading hole, then whitespace-separated
> arguments — not as Vixen text. Holes use interpolation syntax: `$name` for
> a binding, `${ expr }` for an expression.
>
> The leading hole must be a **Tool** (`r[exec.tool]`); the tokens after it
> are validated against, and classified (input / output / text) by, that
> tool's **command** — its grammar (`r[exec.command]`). A backtick
> invocation whose lead is not a tool is a compile error.
>
> An invocation is the value an executor runs. Forcing its value runs it and
> yields the observer's result (default `run.ok.out`); a trailing
> `with { observer: …, writable: …, … }` decorates it. The execution model —
> tools, the `Run`, observers, sandboxing — is `r[exec.*]` in the build
> standard library.

### Identifier expressions

> r[expr.ident]
>
> An identifier in expression position evaluates to the value of the
> binding or type it refers to. snake_case identifiers refer to bindings,
> UpperCamelCase identifiers refer to types.

### Tuple expressions

> r[expr.tuple]
>
> The empty tuple is `()`. A single-element tuple requires a trailing
> comma: `(expr,)`. Two or more elements: `(a, b)`. Trailing commas are
> always allowed. Spread syntax inserts elements from another tuple:
> `(1, ..two_three, 4)`.

### Parenthesized expressions

> r[expr.paren]
>
> `(expr)` groups an expression for precedence. A parenthesized expression
> without trailing comma is not a tuple — `(1 + 2)` is `3`, not `(3,)`.

### Block expressions

> r[expr.block]
>
> A block `( stmt; stmt; expr )` is an expression. Statements are
> separated by semicolons. The last expression in the block is its value.
> A block ending with a statement (or empty) evaluates to `()`.
> Declarations inside a block are scoped to that block.
>
> ```vixen
> let area = (
>   let w = rect.width;
>   let h = rect.height;
>   w * h
> )
> ```

### Array expressions

> r[expr.array]
>
> Array literals use square brackets: `[1, 2, 3]`. Elements are
> comma-separated. Trailing commas are allowed. Spread syntax inserts
> elements from another array: `[1, ..more, 4]`. In an ordinary array
> literal, a spread element must have type `Array[T]`. The type is
> `Array[T]` where `T` is inferred from the elements.

> r[expr.array.concat]
>
> `left ++ right` concatenates two arrays of the same element type and
> returns a new array. Order is preserved. `++` is array-only; it is not
> the merge operator for maps, records, trees, or streams.

### Map expressions

> r[expr.map]
>
> Map literals use `%{ ... }` with general key expressions:
>
> ```vixen
> let outputs = %{ "bin/lua" => lua_exe, "lib/liblua.a" => liblua }
> let port_names = %{ 80 => "http", 443 => "https" }
> ```
>
> Keys and values are arbitrary expressions. Trailing commas are allowed.
> The type is `Map[K, V]`. Records and maps are distinct surface forms:
> `{ ... }` is a record literal and `%{ ... }` is a map literal.

### Function call expressions

> r[expr.call]
>
> Function application is unary. Parenthesized payloads are the
> ordinary call form:
>
> ```vixen
> f(x)
> ```
>
> Array, record, map, string, and interpolated-string payloads can also
> be applied directly:
>
> ```vixen
> f [x, y]
> f { left: x, right: y }
> f %{ "bin/lua" => exe }
> f "out/bin/lua"
> ```
>
> Canonical formatting has no space before `(`, and a separating space
> before `[`, `{`, or `%{`.
>
> A function receives one payload value. If you want to pass what would
> traditionally be “multiple positional arguments”, package them into a
> tuple and pass the tuple as the one payload:
>
> ```vixen
> add(1, 2)
> distance((x1, y1), (x2, y2))
> ```
>
> Juxtaposition application is only allowed when the payload is
> self-delimiting: `(...)`, `[...]`, `{...}`, `%{...}`, `"..."`, or
> `` `...` `` (a string or interpolated string). Bare identifiers and most
> other expressions must use parentheses.
>
> Chained application is left-associative:
>
> ```vixen
> f(x)(y)            // means: (f(x))(y)
> f [x, y] { z: 1 }  // means: (f [x, y]) { z: 1 }
> ```
>
> So:
>
> ```vixen
> f("out/bin/lua")
> f "out/bin/lua"
> ```
>
> are valid, but:
>
> ```vixen
> f x
> ```
>
> is not (bare identifier — wrap as `f(x)`).
>
> `f { ... }` is ordinary unary application to a record payload.
> `f %{ ... }` is ordinary unary application to a map payload.
> Neither is keyword application. `with` is the only keyword-application
> form.
>
> A self-delimiting payload closes the application before any following
> postfix chain. So:
>
> ```vixen
> includes.map ["-I", $].flatten()
> ```
>
> means:
>
> ```vixen
> (includes.map ["-I", $]).flatten()
> ```
>
> To access a member on the result of an application, group the
> application first:
>
> ```vixen
> (discover_tool(class)).path
> ```

> r[expr.call.kwargs]
>
> Keyword application uses `with expr`, where `expr` evaluates to a
> record-shaped value:
>
> ```vixen
> exec [cc, "-c", file] with defaults
> exec [cc, "-c", file] with { mounts: src }
> ```
>
> `with expr` does not introduce a second positional argument. It
> applies the record value in keyword position against the callee's
> declared keyword parameter set.

> r[expr.call.kwargs.spread]
>
> Record spread works inside keyword application. Spreads merge
> left-to-right before keyword checking:
>
> ```vixen
> exec [cc, "-c", file] with { ..common, mounts: src }
> ```
>
> After merge, keyword matching is closed: unknown fields are an error,
> and missing required keyword parameters are an error.

### Named-field constructor expressions

> r[expr.named-constructor]
>
> `TypeName { field: value, field2: value2 }` applies a named-field
> constructor surface. For ordinary structs, the constructor is generated
> from the struct declaration and accepts an anonymous record payload.
> For enum variants, the constructor is generated from the variant
> declaration and returns the variant-specific type, which widens to the
> parent enum in checking contexts. The exact payload kind is part of
> the constructor's type.
>
> ```vixen
> let p = Point { x: 1.0f, y: 2.0f }
> ```
>
> The same unary application model applies to non-record constructors:
> `Tree %{ "bin/lua" => exe }` is `Tree` applied to a map payload, not a
> record payload.

### Field access expressions

> r[expr.field]
>
> `value.field` accesses a struct field by name. `tuple._0`, `tuple._1`
> access tuple elements by position (underscore prefix avoids ambiguity
> with float literals). Zero-arg method calls use ordinary `()`, so
> `value.field` is the bound method value and `value.field()` invokes it.
> Field and method navigation (`.`) forms one left-associative postfix chain.

> r[expr.field.variant]
>
> `Enum.Variant` accesses a variant under the enum's namespace.
> `Shape.Circle` names both the variant-specific type and the variant's
> constructor value. `Shape.Circle { radius: 5.0f }` produces a
> `Shape.Circle`, which widens to `Shape` in checking contexts.

### Method navigation and pipe expressions

> r[expr.method]
>
> Dot access never falls back to arbitrary free functions. `x.f` means
> field access or method lookup only. Methods live in `namespace Type {
> ... }`.

> r[expr.pipe]
>
> `|>` is the chaining form for pipeline stages. A stage may be a plain
> callable stage or any expression that uses `_` as the pipe
> placeholder.
>
> Without `_`, a plain stage receives the piped value as its payload:
>
> ```vixen
> x |> f        // means: f(x)
> ```
>
> With `_`, the piped value is substituted where `_` appears:
>
> ```vixen
> x |> _.utf8()      // means: x.utf8()
> x |> pair(y, _)   // means: pair(y, x)
> ```
>
> `_` inside a pipe RHS names the piped value:
>
> ```vixen
> get_blob(my_blob) |> _.utf8()
> archive |> `$ranlib ${_}`
> ```

### Update expressions

> r[expr.with]
>
> `value with { field: new_value }` produces a new value with some fields
> changed. Fields not mentioned are kept from the original. See
> `r[type.struct.with]` and `r[type.struct.with-assign]`.

> r[expr.with.path]
>
> Dotted field names in `with` blocks are sugar for nested updates:
>
> ```vixen
> config with { package.name: "new", package.edition: "2024" }
> // desugars to:
> config with { :package with { name: "new", edition: "2024" } }
> ```
>
> Paths can be arbitrarily deep: `config with { a.b.c: value }`.

### Constraint expressions

> r[expr.where]
>
> `Type where (constraint)` requests an **ability** — `Type` is a capability
> spec and `constraint` is a boolean **constraint expression** (see
> `r[ability.request]` in the build standard library for the runtime model).
> The free identifiers in `constraint` desugar to **fields of `Type`**, not to
> bindings in scope:
>
> ```vixen
> RustToolchain where (version >= 1.91 && targets contains "aarch64-apple-darwin")
> ```
>
> Here `version` and `targets` resolve to fields of `RustToolchain`. The body
> uses the comparison, logical, membership, and semver-match operators
> (`==`, `!=`, `<`, `<=`, `>`, `>=`, `^=`, `~=`, `&&`, `||`, `contains`, `in`),
> and is extracted as a **structured predicate** — not run as ordinary code —
> so the orchestrator can index advertised providers by it. The result is a
> **predicate-identified value**: a thunk whose identity is the constraint,
> bound to a concrete provider lazily, when something downstream demands it.

### Duplicate resolution

> r[expr.duplicate]
>
> In literals and update/spread expressions, when the same field or key
> is provided more than once, the last occurrence wins. This applies
> uniformly to struct literals, struct update (`with`) blocks, spread
> syntax, map literals, and `.to_map()` with duplicate keys.

### Closure expressions

> r[expr.closure]
>
> Closures use arrow syntax only: `(x, y) => expr`. The body may be any
> expression, including a block: `(x) => ( let y = x + 1; y )`.
> `fn` is declaration-only.
>
> Parameter types are inferred from context. Closures capture bindings
> from their enclosing scope by value.

> r[expr.closure.dollar]
>
> `$` is shorthand for the first argument of an implicit closure. Any
> expression containing `$` that appears in a function argument position
> is automatically wrapped in a closure:
>
> ```vixen
> items.map($ + 1)       // means: items.map((x) => x + 1)
> files.map($.utf8())    // means: items.map((x) => x.utf8())
> ```

> r[expr.closure.dollar-numbered]
>
> `$` is `$0`. For multi-argument closures, use `$0`, `$1`, `$2`, etc.
> The highest numbered `$N` determines the closure's arity:
>
> ```vixen
> items.reduce($0 + $1)    // means: items.reduce((a, b) => a + b)
> ```
>
> `$` and `$N` bind to the nearest enclosing function argument position
> where a closure is expected. When `$` appears in nested calls, it
> belongs to the innermost one:
>
> ```vixen
> // $ belongs to find, not map:
> items.map(list.find($ == 2))
>
> // To reference the map element, use a named closure:
> items.map((item) => list.find($ == item))
> // here $ is find's argument, item is map's argument
> ```

### Partial application expressions

> r[expr.partial]
>
> `partial` freezes the application surface written to its right,
> producing a new callable value. Bound positional arguments are fixed,
> omitted positional arguments remain open, and unspecified keyword
> arguments remain open:
>
> ```vixen
> let cexec = partial exec with common
> let loud_compile = partial compile with { loudly: true }
> ```

> r[expr.partial.keyword]
>
> Partial application can bind keyword arguments through `with`. Bound
> keyword arguments are sealed — they cannot be overridden by the caller:
>
> ```vixen
> fn compile(src: String) with { optimize: Bool = false, loudly: Bool = false } ( ... )
>
> let loud_compile = partial compile with { loudly: true }
> // loudly is sealed at true, optimize uses its default
> ```

> r[expr.partial.passable]
>
> Functions with keyword arguments are first-class values. Partial
> application is only needed to seal some keyword arguments, not to make
> the function passable.

### Conditional expressions

> r[expr.if]
>
> `if condition ( then_body ) else ( else_body )`. No parentheses around
> the condition; parentheses delimit both branches. `else if` chains
> are allowed. `if` is always an expression, so `else` is always
> required.

### Match expressions

> r[expr.match]
>
> ```vixen
> match value (
>   pattern => expr,
>   pattern if guard => expr,
>   _ => default_expr,
> )
> ```
>
> Arms are comma-separated. Each arm is `pattern => expr` with an optional
> `if guard` between the pattern and the arrow. Arms can use block bodies:
> `pattern => ( stmts; expr )`. `match` must be exhaustive — every
> possible value must be covered. A wildcard `_` catches everything else.

### Import expressions

> r[expr.import]
>
> `import "path"` loads a module and makes its exported names available
> as a namespace. The argument is a string literal — not a general
> expression. The result supports dot access and destructuring but is
> not a first-class value.

### Type cast expressions

> r[expr.as]
>
> `expr as Type` converts a value to another type. The conversion must
> succeed or the node poisons. `expr as? Type` is the fallible version —
> returns `Option`, `Some` on success, `None` on failure.

### Type ascription expressions

> r[expr.ascribe]
>
> `expr :: Type` asserts the type of an expression without conversion.
> This is a static check — the expression must already be of the given
> type. Unlike `as`, no conversion occurs. This is useful for
> disambiguating literal types and documenting intent:
>
> ```vixen
> let zero = 0 :: I64    // literal 0 as I64, no conversion
> let pi = 3.14f :: F64  // 3.14f is already F64; ascription documents intent
> ```
>
> Spaces around `::` are canonical formatting but not required:
> `0::I64` and `0 :: I64` are equivalent.

### Type test expressions

> r[expr.is]
>
> `expr is pattern` tests whether a value matches a pattern. Evaluates to
> `Bool`. Variables bound in the pattern are in scope in the enclosing
> `if` body:
>
> ```vixen
> if config.edition is Some { value: edition } (
>   use_edition(edition)
> )
> ```

### Statements

> r[expr.statement]
>
> Assignment (`name = expr`, `name: Type = expr`) and compound update
> (`name with= { ... }`) are statements, not expressions. Declarations
> (`fn`, `struct`, `enum`, `newtype`, `type`) are also statements.
>
> A bare expression is only valid in tail position of a block, where it
> becomes the block's value. Any other expression statement is a static
> error.

## Modules

> r[lang.modules]
>
> A `.vx` file is a module. Its top-level statement list may contain
> bindings and declarations. Top-level bindings and declarations are the
> module's exports. Top-level bare expressions are invalid because a
> module has no resulting value position for them to flow into.

> r[lang.modules.frontmatter]
>
> A `.vx` file may open with a **frontmatter** block — a fenced block
> (three backticks, like a heredoc, conventionally tagged `styx`) that is
> the first item in the file. It carries package metadata and capability
> declarations in styx: `id`, `version`, dependencies, and
> `capability { advertises T }`. There is no separate `package.styx`
> manifest — a package that publishes, declares dependencies, or advertises
> a capability does so in frontmatter; a leaf build script needs none.

> r[lang.modules.namespace]
>
> Modules are namespaces, not runtime values. `import "path"` makes
> the module's exported names available. There is no first-class
> `Module` object that can be stored in a binding, passed to a function,
> or compared for equality.

> r[lang.modules.access]
>
> Access module contents with dots: `std.rust.workspace (src, args, env)`.
> Destructuring binds names from the namespace:
> `{ rust } = import "std"`.

> r[lang.modules.sources]
>
> Three kinds of module sources:
>
> - **Local files** — paths starting with `./` are relative to the current
>   file: `import "./lib/helpers"` loads `./lib/helpers.vx`.
> - **Standard library** — `import "std"` loads the built-in standard
>   library.
> - **External packages** — resolution of non-local, non-std imports is a
>   future concern. The mechanism is the same: `import` returns a module
>   value.

## Scalar types

> r[type.scalar]
>
> Vixen has scalar types: `U64`, `I64`, `F64`, `Bool`, `String`, `SemVer`.
> `SemVer` is structured (segments plus optional pre-release / build
> metadata) but built-in and first-class — versions are core to a build
> language, so the runtime owns them rather than leaving them to a package.

> r[type.scalar.literals]
>
> Integer literals default to `U64`; dotted-numeric literals are `SemVer`
> (floats require the `f` suffix). Literal syntax is specified in
> `r[lex.numeric]`, `r[lex.string]`, and `r[lex.string.interpolation]`.
> Type ascription selects a different type: `4::I64`.

## Newtypes

> r[type.newtype]
>
> A newtype is a distinct type backed by an existing type. The new type
> requires explicit conversion via `as`:
>
> ```vixen
> newtype Blake3 of String
> let hash = blake3_hash_of(something); // type Blake3
> let raw = hash as String;             // explicit conversion
> ```

## Tuples

> r[type.tuple]
>
> Vixen has heterogeneous tuples. Field access uses `._0`, `._1`, etc.
> (the underscore prefix avoids ambiguity with float literals).
> Tuples can be spread into other tuples: `(1, ..two_three, 4)`.
> Trailing commas are allowed.

## Structs

> r[type.struct]
>
> Structs are declared with `struct Name { ... }`. Zero-sized structs may
> omit the body: `struct Unit`. The `:` shorthand in construction —
> `:x` means `x: x` — reuses a binding name as the field name. Spread
> syntax `..other` copies fields from another value.

> r[type.struct.with]
>
> `with` creates a new value with some fields changed:
>
> ```vixen
> let updated = name with { last: "Wilson" }
> ```
>
> Fields not mentioned are kept from the original value.

> r[type.struct.with-assign]
>
> `with=` is compound assignment: `x with= { ... }` means
> `x = x with { ... }`. The `:` shorthand enables nested updates —
> `:field` refers to the existing field value:
>
> ```vixen
> config with= {
>   :package with {
>     name: "new",
>     edition: "2024",
>   },
>   optimize: true,
> }
> // equivalent to:
> // config = config with {
> //   package: config.package with { name: "new", edition: "2024" },
> //   optimize: true,
> // }
> ```

> r[type.struct.nested]
>
> Types can be declared inline in slot position, immediately after `:`:
> `package: struct Package { ... }` or `state: enum { ... }`. Named
> inline types live under the parent's namespace and can be referenced as
> `CargoToml.Package`. Anonymous inline types are allowed when no
> reusable name is needed.

> r[type.struct.destructure]
>
> Structs can be destructured in bindings. The type name is not needed —
> it is inferred from the right-hand side: `{ first, last } = name`.
> For enum variants in `match` or `is`, the variant name is always
> required.

## Enums

> r[type.enum]
>
> Enums are declared with `enum Name { ... }`. Each variant is a named
> struct declaration under the enum's namespace. `Shape.Circle` is a
> type, and `Shape` is a type. They are distinct.

> r[type.enum.variant-kinds]
>
> Variants are always explicit named structs:
>
> - `struct Red,` — zero-sized variant
> - `struct Circle { radius: F64 },` — data-carrying variant
>
> Variants are comma-separated.

> r[type.enum.no-recursion]
>
> Recursive types are not allowed. Every value must have a finite size.
> There is no indirection (no references, no boxing) to break cycles.

> r[type.enum.alias]
>
> Enum variants do not use alias syntax. Shared or sum-like types should
> be named separately with `type Name = A | B`.

> r[type.enum.build]
>
> To build a variant value, use its struct literal form:
> `Shape.Circle { radius: 5.0f }`. Zero-sized variants use the type name:
> `Direction.North`.

## Generics

> r[type.generic]
>
> Type parameters use square brackets: `Pair[A, B]`, `Array[T]`,
> `Map[K, V]`. Generic types and functions are checked at instantiation,
> not in isolation — there are no trait bounds. If a concrete type doesn't
> support an operation used in the body, the error is reported at the call
> site.

## Type inference

> r[type.inference]
>
> Vixen uses local type inference. Types flow forward from literals and
> function return types, and backward from function parameter types.
> Function parameter types are always annotated. Return types may be
> omitted (defaults to `()`). Closure parameter types are inferred from
> context (the expected function signature at the call site). Generic type
> parameters are inferred from arguments.
>
> Inference does not cross function boundaries. Each function is checked
> independently using its explicit signature.

## Implicit coercions

> r[type.coerce.widen]
>
> A variant value auto-widens to its parent enum type, but only when the
> target type is already known — never when the type would be inferred
> from the value. This chains through alias variants. The reverse is not
> true — narrowing requires `as` or `match`.

> r[type.coerce.widen.when]
>
> Widening fires in "checking" mode (expected type is known), never in
> "inference" mode (type flows from the value):
>
> - **Yes**: annotated bindings (`x: Shape = circle`), struct fields
>   (type known from definition), return types (annotated on function),
>   explicit `as` conversion.
> - **No**: unannotated bindings (`x = circle` — `x` is `Circle`, not
>   `Shape`), function call arguments (overload resolution uses the
>   exact type, no widening).
>
> This ensures that adding a function overload or removing a type
> annotation cannot silently change which coercions occur.

> r[type.coerce.stringify]
>
> String interpolation (`"${expr}"`) calls `.to_string()` on the
> interpolated value. Built-in scalar and path-like types provide
> canonical `to_string()` methods. User-defined types participate by
> defining `to_string(self) -> String` in their canonical namespace.

> r[type.coerce.symbol]
>
> `Symbol` may implicitly coerce to `String` in checking contexts. This
> supports APIs that accept textual names while still letting user code
> traffic in symbol values. The reverse coercion does not exist: turning
> a `String` into a `Symbol` is always explicit.

> r[type.coerce.unit]
>
> A function body or block that ends with a statement (or is empty)
> implicitly evaluates to `()`.

> r[type.coerce.none]
>
> There are no implicit numeric coercions. Converting between `U64`,
> `I64`, and `F64` always requires explicit `as`.

## Control flow

### if / else

> r[control.if]
>
> `if`/`else` works like in Rust in the condition position — no
> parentheses around the condition. Branch bodies are expressions,
> commonly parenthesized blocks. `if`/`else` is itself an expression.

### match

> r[control.match]
>
> `match` branches on a value and binds variables in one step. `match` is
> an expression. It must be exhaustive — every variant must be covered. A
> wildcard `_` catches everything else. Struct patterns can use `..` to
> ignore unneeded fields. The `@` operator binds the whole value while
> also destructuring it.

> r[control.match.guard]
>
> A match arm can have a guard — a boolean condition after `if`:
> `n if n < 1024 => "privileged"`. The guard is checked after the pattern
> matches. If the guard is false, the next arm is tried.

> r[control.match.literal]
>
> `match` works on any value, not just enums. String and number literals
> are valid patterns.

> r[control.match.or-pattern]
>
> Multiple patterns in one arm, separated by `|`. Or-patterns can include
> bindings as long as every alternative binds the same names with the same
> types.

> r[control.match.tuple]
>
> `match` works on tuples:
> `match (status, kind) ( (0, "lib") => ..., (code, _) => ... )`.

> r[control.match.nested]
>
> Patterns can be nested arbitrarily — a struct pattern can contain
> literal patterns, other struct patterns, tuple patterns, etc.

### The `is` operator

> r[control.is]
>
> `is` tests whether a value matches a pattern, binding variables on
> success. It replaces `if let` from Rust.
> `is` is an expression that evaluates to `Bool`.
>
> ```vixen
> if config.edition is Some { value: edition } (
>   use_edition(edition)
> ) else (
>   use_default()
> )
> ```

### Block expressions

> r[control.block]
>
> A block `( ... )` is an expression. The last expression in the block is
> its value. Bindings inside a block are scoped to that block. A bare
> expression that does not become the block's value is an unused
> expression statement and is therefore invalid. Blocks replace `let`/`in`
> and `where` clauses from other languages.

## Pattern grammar

> r[pattern.wildcard]
>
> `_` matches any value and binds nothing.

> r[pattern.binding]
>
> A snake_case identifier in pattern position binds the matched value to
> that name: `x`, `name`, `remaining`.

> r[pattern.literal]
>
> Integer, float, string, and boolean literals are valid patterns. They
> match by equality: `0`, `"hello"`, `true`.

> r[pattern.tuple]
>
> `(a, b, c)` destructures a tuple. Nested patterns are allowed in each
> position.

> r[pattern.struct]
>
> `{ field, field2: pat }` destructures a struct. A bare `field` binds
> the field to a variable of the same name. `field: pat` matches the
> field against a nested pattern. `..` ignores remaining fields.

> r[pattern.variant]
>
> Enum variant patterns require the variant name:
> `Circle { radius }`, `Some { value }`, `None`. In `match`, the variant
> name is always required (unlike struct destructuring in bindings where
> it's inferred).

> r[pattern.array]
>
> Array patterns match by structure. `..` skips any number of elements.
> `..name` binds the skipped elements as an `Array`:
>
> ```vixen
> match items (
>   [] => "empty",
>   [only] => "just ${only}",
>   [first, ..] => "starts with ${first}",
>   [first, .., last] => "${first} to ${last}",
>   [_, second, ..rest] => use(second, rest),
> )
> ```

> r[pattern.or]
>
> `pat1 | pat2` matches if either pattern matches. All alternatives
> must bind the same names with the same types.

> r[pattern.at]
>
> `name @ pattern` binds the whole matched value to `name` while also
> destructuring it with the inner pattern.

> r[pattern.guard]
>
> In `match` arms, `pattern if condition => expr`. The guard is checked
> after the pattern matches. If the guard is false, the next arm is tried.

> r[pattern.where]
>
> Patterns appear in: `match` arms, `is` expressions, binding
> destructuring, and function parameters (for tuples/structs).

## Functions

> r[fn.recursion]
>
> Recursive functions are allowed. Recursive types are not. Most iteration
> should use combinators; recursion is available for tree-walking and
> similar algorithms.

> r[fn.type]
>
> Function types use `Fn(Args) -> Ret` syntax. There are no capability
> tags — authority is conveyed by parameter types (see the capabilities
> model in the build standard library). Keyword parameters use a trailing
> `with` block:
>
> ```vixen
> Fn(Path) with { mounts: Tree, loudly: Bool = false } -> Tree
> ```
>
> Multiple positional parameter types describe one tuple payload:
>
> ```vixen
> Fn(U64, U64) -> U64
> ```
>
> is called as `f (1, 2)`. `Fn((U64, U64)) -> U64` is the same call
> shape, but spells the tuple payload explicitly in the parameter type.
>
> Functions with keyword arguments are first-class; keyword-parameter
> shape is part of the function's call surface rather than a reason the
> function cannot be passed around.

> r[fn.identity]
>
> Every function has a **code identity** — a fingerprint of the code it
> will execute, transitively. The code identity is computed from:
>
> 1. The function's **canonical AST** — its body and signature,
>    normalized (comments, whitespace, and formatting are excluded).
> 2. The **code identities of all callees** — every function name
>    referenced in the body contributes the code identities of all
>    overloads of that name visible in scope.
>
> This is transitive: if `f` calls `g` and `g` calls `h`, then a
> change to `h` changes `g`'s code identity, which changes `f`'s.
> Editing a comment does not change any code identity. Editing an
> unrelated function does not change code identities that do not
> reference it.

> r[fn.identity.scc]
>
> Mutually recursive functions form a strongly connected component
> (SCC). All functions in an SCC share one code identity — the hash
> of the entire component. A change to any function in the cycle
> invalidates all of them.

> r[fn.identity.closure]
>
> A closure's code identity is derived from its enclosing function's
> canonical AST (which includes the closure site). At runtime, the
> closure's full identity for memoization and serialization is
> **(code identity, captured values)** — the code identity comes from
> the source, the captures come from evaluation.

> r[fn.identity.overloads]
>
> When a function body references a name that has multiple overloads,
> all visible overloads contribute to the code identity. This is
> conservative — changing an overload that is never called still
> invalidates callers. But it is sound without requiring static
> overload resolution, which depends on type information that may
> not be available until evaluation.

> r[fn.identity.stability]
>
> Code identity is stable across: comment edits, whitespace changes,
> formatting changes, and edits to unrelated functions. It changes
> when: the function's logic changes, a callee's logic changes, or the
> set of visible overloads for a referenced name changes. This
> stability is what makes `exec` caching and distributed execution
> correct without whole-program invalidation.

## Closures

> r[fn.closure]
>
> Closures use arrow syntax only: `(x) => body`.

> r[fn.closure.dollar]
>
> `$` is shorthand for the first argument of a closure. `$` works
> anywhere in an expression, including in function arguments:
> `items.map($ + 1)` means `items.map((x) => x + 1)`.

> r[fn.closure.numbered]
>
> `$` is `$0`. For multi-argument closures, use `$0`, `$1`, `$2`, etc.:
> `items.reduce($0 + $1)` means `items.reduce((a, b) => a + b)`.

## Methods and pipe

> r[fn.method]
>
> Methods live in type namespaces; see `r[expr.field]` and
> `r[expr.method]` for the call surface. Vixen does not use free-function
> UFCS. Free functions chain through `r[expr.pipe]`.

## Function overloading

> r[fn.overload]
>
> Multiple functions can share the same name if they differ in parameter
> types. The evaluator selects the matching overload based on the known
> types of the arguments at the call site. An exact type match always
> wins over a match via widening.

> r[fn.overload.no-sum-dispatch]
>
> Overload resolution does not synthesize dispatch over enum variants.
> If `fn digest(x: Blob)` and `fn digest(x: Tree)` both exist, calling
> `digest(entry)` where `entry: Entry` (and `type Entry = Blob | Tree`) is
> an error — there is no overload for `Entry`. Write an explicit `match`
> or add `fn digest(x: Entry)`.

## Constructors

> r[fn.constructor]
>
> A type name can be overloaded as a constructor function. Constructor
> application uses the ordinary unary call model:
>
> ```vixen
> Blob("hello")
> Blob(42)
> Point { x: 1.0f, y: 2.0f }
> ```
>
> Named-field constructor surfaces are canonical for structs and
> record-like enum variants. Struct constructors accept anonymous
> structural record payloads. Variant constructors return the
> variant-specific type and widen to the parent enum when needed.
> Opaque declarations (`type Name;`) do not synthesize a constructor
> surface by themselves. Constructors for opaque builtins may still be
> declared explicitly and may accept other payload kinds, such as maps
> (`Tree %{ ... }`) or arrays (`Tree [ ... ]`), without becoming separate
> syntax classes.

## Records

> r[type.record]
>
> Anonymous records use braces with field labels:
>
> ```vixen
> let common = { observer: make_observer() }
> let pair = { left: 1, right: 2 }
> ```
>
> A bare field name is shorthand for `name: name`. Records are real
> values with structural types such as `{ left: U64, right: U64 }`.
> Record field labels are names, not general expressions.

> r[type.record.spread]
>
> Record spread uses `..record` and merges left-to-right. Later explicit
> fields override earlier spread-provided fields:
>
> ```vixen
> let tuned = { ..defaults, optimize: true }
> ```

## Arrays

> r[collect.array]
>
> Array literals use square brackets: `[1, 2, 3]`. The type is
> `Array[T]`.

## Maps

> r[collect.map]
>
> Map literals use `%{ ... }` and `=>`:
>
> ```vixen
> let outputs = %{ "bin/lua" => lua_exe, "lib/liblua.a" => liblua }
> let port_names = %{ 80 => "http", 443 => "https" }
> let build_flags = %{ (target) => flags_for target }
> ```
>
> The type is `Map[K, V]`. Keys can be any Vixen type — all types are
> comparable for equality and hashable. Records and maps do not share
> one literal form: `{ field: value }` is a record, `%{ key => value }`
> is a map.

> r[collect.map.order]
>
> Maps are ordered by key using Vixen's universal total ordering.
> Iterating a map (via combinators or `.keys()` / `.values()`) always
> produces entries in sorted key order. This makes map iteration
> deterministic.

## Ranges

> r[collect.range]
>
> Range literals: `0..10` (exclusive), `0..=10` (inclusive), `..10`
> (to), `0..` (from), `..` (full). Ranges can be passed to collection
> methods such as `slice(range)`.

## Combinators

> r[comb.model]
>
> There are no `for` loops in Vixen. Iteration is done through
> combinators. `.map` and `.filter` always return `Stream`, whether
> called on an `Array`, `Map`, or `Stream`. Every element is an
> independent concurrent computation. Results are produced as they become
> available, not in input order.

> r[comb.model.ordering]
>
> Because combinators always produce `Stream`, calling `.map f` on an
> `Array` loses input order. The elements are processed concurrently
> and the resulting stream is unordered. To recover a deterministic
> array, call `.to_array()`, which sorts by universal total ordering —
> not by the original input position. This means
> `([3, 1, 2].map($ * 2)).to_array()` returns `[2, 4, 6]`, not
> `[6, 2, 4]`. This is a deliberate trade-off: concurrent fan-out and
> canonical determinism over positional preservation.

> r[comb.map]
>
> `.map f` applies `f` to each element and returns a `Stream` of results.

> r[comb.filter]
>
> `.filter pred` returns a `Stream` of elements where `pred` is true.

> r[comb.find]
>
> `.find pred` returns the first element to resolve for which `pred`
> is true. The result is `Option[T]`. If at most one element can satisfy
> `pred`, the result is deterministic. Nondeterminism arises only when
> multiple elements may satisfy `pred`, because any of them may resolve
> first. `None` is returned only if the stream completes with no
> matching element.

> r[comb.next]
>
> `.next()` returns the first element to resolve from a `Stream[T]` and
> **poisons** if the stream completes cleanly with no elements; `.next?()`
> is the auto-derived `Option[T]` form (`None` on empty), per
> `r[error.option]`. If multiple elements may resolve, the chosen element
> is whichever resolves first — use `.next` only when that race is
> semantically intended or the stream holds at most one element.

> r[comb.only]
>
> `.only()` returns the sole element of a stream, surfaced **on arrival**
> (it does not wait for the stream to end) under an author-asserted
> **uniqueness contract**: at most one element is expected. It **poisons**
> on empty or on a second element; `.only?()` is the auto-derived
> `Option[T]` form (`None` on empty), per `r[error.option]`. Because the
> result is *the* element rather than "whichever raced first," it is
> order-independent and deterministic — early and cacheable at once. (When
> a stream is naturally keyed, `by(key).get(k)` is the checkable
> alternative: deterministic by key lookup rather than by contract.)

> r[comb.determinism]
>
> Combinators split by whether their result can expose stream arrival
> order — the property that decides whether a computation observing a
> stream stays cacheable (see [exec.observer.cacheable]):
>
> - **Order-erasing** (deterministic): `map`, `filter`, `filter_map`,
>   `to_array` (sorts by the universal order), set / sorted collects, and
>   reductions through a commutative-associative merge (an accumulator).
>   The result is a function of the stream's *contents*, not arrival order.
> - **Unique-select** (deterministic, available early): `.only()`
>   (`r[comb.only]`) and keyed `by(k).get(...)`.
> - **Order-exposing** (timing-in-value): `.next()`, racing `.find` — the
>   result *is* the timing when more than one element could match.

> r[comb.fold]
>
> `.fold (init, f)` reduces all elements to a single value using an
> accumulator. Only available on `Array`, not on `Stream` — fold depends
> on element order, which streams do not have.

> r[comb.any]
>
> `.any pred` returns `true` if any element satisfies the predicate.

> r[comb.all]
>
> `.all pred` returns `true` if all elements satisfy the predicate.

> r[comb.terminal-race]
>
> Stream terminal combinators (`any`, `all`, `find`) resolve on the
> earliest event that determines their result. Poison is such an event —
> if poison resolves before a decisive result, the combinator poisons.
> Once a decisive result resolves, remaining stream events are not
> observed:
>
> - `any`: `true` on first satisfying element; `false` only on clean
>   completion with none satisfying.
> - `all`: `false` on first failing element; `true` only on clean
>   completion with all satisfying.
> - `find`: `Some` on first matching element; `None` only on clean
>   completion with none matching.

> r[comb.flat-map]
>
> `.flat_map f` maps each element to a stream (or array) and flattens
> the results into a single stream. `.flatten()` flattens a stream of
> streams.

> r[comb.merge]
>
> `.merge()` on `Stream[Tree]` waits for all trees and produces a single
> `Tree` containing all their entries. Relative paths must be disjoint:
> if the same path appears in more than one input tree, the stream
> poisons. This makes stream merging deterministic even though streams
> are unordered. For ordered overwrite semantics, collect or construct
> an `Array[Tree]` and call `Tree.merge` explicitly.

> r[comb.filter-map]
>
> `.filter_map f` maps and filters in one step — the function returns
> `Option[T]`. `Some` values are produced, `None` values are skipped.

> r[comb.reduce]
>
> `.reduce f` is like `.fold` but uses the first element as the initial
> accumulator. Only available on `Array`, not on `Stream` — use
> `.to_array()` first.

> r[comb.group-by]
>
> `.group_by f` groups elements by key function, returning
> `Map[K, Array[T]]`.

> r[comb.partition]
>
> `.partition pred` splits elements into two arrays by predicate,
> returned as a tuple `(matching, non_matching)`.

> r[comb.no-positional]
>
> Streams are unordered — elements arrive as they resolve, not in input
> order. Positional combinators like `take(n)`, `skip(n)`, `enumerate`,
> `zip`, `chunks`, `windows`, `take_while`, and `skip_while` are not
> available on streams. "Take the first 5" has no meaning when there is
> no first. These operations exist on `Array` (which is ordered).

> r[comb.collect]
>
> `.to_array()` converts a `Stream` back into an `Array`. It waits for
> all elements to resolve, then sorts them using Vixen's universal total
> ordering to produce a deterministic result. `.to_map()` converts a
> stream of `(key, value)` pairs into a `Map`. Both are pending until
> all elements resolve.

> r[comb.collect.ordering]
>
> Arrays produced by stream collection operations are canonically
> ordered using Vixen's universal total ordering. This applies to
> `.to_array()`, group arrays in `.group_by`, and both arrays
> returned by `.partition pred`.

> r[comb.map-combinators]
>
> Map combinators pass `(key, value)` pairs to the closure. `.keys()`
> returns `Array[K]`, `.values()` returns `Array[V]`.

> r[comb.then]
>
> `.then` transforms the return value of a callable while preserving its
> full call surface:
>
> ```vixen
> f.then(g)
> ```
>
> means: call `f` normally, then pass its result to `g`. Positional
> payload shape and keyword parameter shape are preserved.

## Option

> r[option.type]
>
> `Option[T]` is a built-in enum with variants `struct Some { value: T }`
> and `struct None`.

> r[option.sugar]
>
> `T?` is sugar for `Option[T]`.

> r[option.methods]
>
> Option methods: `.unwrap_or default` provides a fallback value.
> `.unwrap()` returns the inner value and **poisons** on `None`;
> `.expect msg` does the same with a caller-supplied message. These are
> the assert-it's-present path now that there is no `?` operator.
> `.map f` transforms the inner value (`None` stays `None`).
> `.and_then f` flat maps — `f` returns `Option`, the result is
> flattened. `.is_some()` and `.is_none()` return `Bool`.

## The `as` operator

> r[type.as]
>
> `as` converts between types. The conversion must succeed or the node
> poisons. Used for narrowing enums to variants and converting newtypes.

> r[type.as-fallible]
>
> `as?` is the fallible version of `as`. Returns `Option` — `Some` on
> success, `None` on failure.

## Errors

> r[error.poison]
>
> There is no `Result` type. Vixen has one error model: poisoning. A
> poisoned value propagates through the dataflow graph — any node that
> depends on a poisoned value also becomes poisoned. The runtime attaches
> context automatically: what function was being evaluated, what its
> inputs were, what demanded it.
>
> Poison values carry a **kind** that distinguishes the failure mode.
> The kind is used by the runtime and tooling — not by user code — to
> decide how to report and respond. One distinct kind is **verification
> failure** (see [exec.verifier]), which signals that an executor's
> environment drifted from host observations previously recorded during
> discovery.

> r[error.fail]
>
> `fail` is a keyword that poisons the current computation with a message.
> It works anywhere. The function doesn't need a special return type.

> r[error.propagation]
>
> Poisoning propagates through the graph automatically. No manual error
> propagation needed. Independent subgraphs are unaffected. If multiple
> inputs are poisoned, all errors are collected. Error reporting is
> deterministic.

> r[error.option]
>
> A fallible function `f` **poisons** on failure — the assertive form, for
> when failure means the build is malformed and there is nothing useful to
> recover to; the poison propagates and callers do nothing. Its **`?`
> variant `f?` is auto-derived** and returns `Option[T]` instead of
> poisoning — the recoverable form, for a probe, an optional lookup, or a
> parse of external text. So `tree.file` poisons while `tree.file?` is
> `Option`; `parse_u64` poisons while `parse_u64?` is `Option`; `.only`
> poisons while `.only?` is `Option`. One convention — no hand-named
> sibling per operation.
>
> `?` suffixes a fallible function's **name**; it is **not** a postfix on
> an arbitrary expression. There is no blanket "catch any poison" operator
> — that was considered and rejected because it silently swallows
> Diagnostics and hides recoverable failure. `f?` exists only where `f` is
> fallible, so recoverability stays visible in the signature.
>
> `Option` composes with `match`, `.unwrap_or`, `.map`, `.and_then`;
> `.unwrap` / `.expect msg` poison on `None` for the assert path. `as?` is
> the same suffix applied to a conversion: `as` poisons, `as?` yields
> `Option`.

## Streams

> r[stream.type]
>
> `Stream[T]` is a built-in generic type that produces values of type `T`
> over time. Streams come from `exec` observers, from combinators on
> other streams, or from converting arrays and ranges.

> r[stream.unordered]
>
> Stream combinators do not impose ordering. Results are produced as they
> become available, not in input order. `.map f` over a stream is
> concurrent — each application of `f` is an independent computation.

> r[stream.determinism]
>
> Streams are nondeterministic in element arrival order. The transition
> from unordered stream to ordered array is where determinism is
> restored: `.to_array()` waits for all elements to resolve, then sorts
> them using Vixen's universal total ordering (all values are comparable).
> The result is a stable, deterministic `Array` regardless of which
> elements resolved first.
>
> This means `([3, 2, 1].to_stream()).to_array()` returns `[1, 2, 3]`.
> For build systems, this is the right default — the collection of
> artifacts is in a stable order before linking, regardless of which
> compilation finished first. If a specific semantic order is needed
> (e.g., sorting files by size), call a custom sort on the resulting
> `Array`.

> r[stream.of]
>
> `Stream.of value` creates a single-element stream.

## Constraints and requirements

> r[constraint.model]
>
> The comparison operators (`==`, `!=`, `<`, `<=`, `>`, `>=`), the
> collection operators (`contains`, `in`, `matches`), and the semver
> matchers (`^=`, `~=`, comparator sets, wildcards) **never return
> `Bool`**. Each produces a `Constraint[T]` — a first-class, typed,
> introspectable, composable, hashable value describing a structural
> predicate over a `T`. There is exactly one return type for these
> operators; `Bool` is reached only by coercion (`r[constraint.coerce]`).

> r[constraint.coerce]
>
> `Constraint[T]` coerces to `Bool`, but only where a `Bool` is demanded
> (an `if` / `while` condition, a boolean-position operand of `&&` / `||`
> / `!`) and only when the constraint is **grounded** — every operand is a
> concrete value. A grounded constraint coerces by evaluating the
> predicate. Coercing an **ungrounded** constraint — one that still refers
> to an unbound candidate — is a compile error: there is no value to
> evaluate it against.
>
> ```vixen
> let c = $.version >= 1.91   // Constraint[SemVer]; $ unbound — stays structural
> if v >= 1.91 ( ... )        // v concrete — coerces to Bool for the condition
> ```

> r[constraint.combinators]
>
> `&&`, `||`, `!` are overloaded: on `Bool`s they are ordinary boolean
> operators; on `Constraint`s they are structural intersection, union, and
> negation, each producing a `Constraint`. A grounded `Constraint` in a
> boolean position coerces per `r[constraint.coerce]` before the boolean
> operator applies.

> r[constraint.algebra]
>
> The constraint operator set is a **closed, entirely built-in algebra.**
> A constraint expression is built from candidate field projection
> (`$.field`, `$.field.subfield`) and the operators in
> `r[constraint.operators]`, and nothing else. Applying any other
> operation to an unbound candidate — an arbitrary method call, control
> flow over `$` — is a compile error. There is no opaque per-candidate
> fallback. This guarantees every constraint is structurally indexable: a
> registry or resolver consuming a `Requirement[T]` can index, compose,
> hash, and explain it without evaluating an opaque closure. User-defined
> operators are not supported; new operators are deliberate built-in
> additions.

> r[constraint.operators]
>
> The built-in operator vocabulary:
>
> - comparisons: `==`, `!=`, `<`, `<=`, `>`, `>=`
> - collections: `contains`, `in`, `matches` (regex)
> - semver matching: `^=` (caret), `~=` (tilde), comparator sets,
>   wildcards, exact, with pre-release rules
> - boolean composition: `&&`, `||`, `!`
> - field projection: `$.field`, nested `$.field.subfield`
>
> `^=`, `~=`, `contains`, `in`, and `matches` bind at comparison
> precedence (`r[lex.precedence]`) and do not chain.

> r[constraint.semver]
>
> `SemVer` is a built-in scalar (`r[type.scalar]`) and its match operators
> are runtime built-ins, not package-declared functions — a build system
> owns version semantics, and the registry indexes semver requirements
> natively. Dotted-numeric literals are `SemVer` (`r[lex.numeric]`).

> r[constraint.requirement]
>
> `T where (predicate)` lifts a `Constraint[T]` into a `Requirement[T]` —
> a routable demand that a resolver (the executor orchestrator, a package
> resolver) satisfies by selecting a concrete `T`. Inside the `(predicate)`
> the candidate's fields are in scope as **bare identifiers**: a free
> `version` desugars to the candidate's `version` field. The named type `T`
> establishes that field scope, so `version` is sugar for `$.version`; an
> identifier that names no field of `T` resolves by ordinary scoping, so
> outer bindings and constants stay reachable. A detached constraint value
> — built with no named candidate in scope — names the candidate
> explicitly with `$` (`r[constraint.coerce]`).
>
> ```vixen
> let rust = RustToolchain where (version >= 1.91 && targets contains "aarch64-apple-darwin")
> ```
>
> `Requirement[T]` is itself a value: constructed where policy is
> expressed, resolved lazily when something downstream demands the
> concrete `T`.

## Accumulators

> r[accum.type]
>
> `Accum[M]` is a handle for collecting contributions of type `M` across a
> demanded subgraph and reducing them under `M`'s merge. It is an ordinary
> value, threaded as a parameter; holding one confers the ability to
> contribute to it. Unlike the authority capabilities (`Host`, `Process`,
> the sandbox `Tree`), `Accum` carries no authority — it is a pure
> computational handle, **user-mintable** (`r[accum.scope]`) rather than
> runtime-only. It is region-scoped to the `accumulate` block that minted
> it (`r[cap.region]`) and may not escape.

> r[accum.merge]
>
> `M` must provide `identity() -> M` (the empty element) and
> `merge(M, M) -> M` (associative) as ordinary overloads in scope. There
> is no trait or bound: like every generic, `accumulate[M]` is checked at
> instantiation (`r[type.generic]`), so a missing `merge` / `identity` for
> a concrete `M` is reported at the `accumulate` call site.

> r[accum.scope]
>
> `accumulate[M] (acc) => body` mints an `Accum[M]`, evaluates `body` with
> it, collects every `emit` made anywhere in `body`'s demanded subgraph,
> and returns the contributions reduced under `merge` (from `identity`).
>
> ```vixen
> let feats = accumulate[FeatureSet] (acc) => walk_active_edges(graph, acc)
> // callees thread `acc` down and call acc.emit(%{ "serde" => {"derive"} });
> // feats : FeatureSet — the unioned result
> ```

> r[accum.ops]
>
> `acc.emit(contribution)` contributes a value of type `M`. `acc.value()`
> reads the current reduction of all contributions emitted so far in the
> enclosing `accumulate`. Contributions are unordered; the result must not
> depend on emission order — hence `merge` is associative, and commutative
> wherever order could vary.

> r[accum.fixpoint]
>
> If `body` reads `acc.value()` and that reading feeds further `emit`s,
> `accumulate` iterates to a least fixpoint: re-evaluate, collect, reduce,
> repeat until the reduction stops changing. Termination is guaranteed
> only when `merge` forms a **bounded join-semilattice** — commutative,
> idempotent, and of finite height. That is an unenforced contract the
> author guarantees (it can no more be checked than associativity can); a
> `merge` that is not bounded-monotone may fail to converge. A `body` that
> never mid-reads converges in a single pass.

> r[accum.purity]
>
> An accumulator's converged value is a pure function of the inputs — the
> contribution set is determined by them — so it is content-addressed and
> cacheable like any value. Intermediate fixpoint iterations are not
> persisted; only the converged result participates in caching.
