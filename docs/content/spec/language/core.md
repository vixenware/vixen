+++
title = "Core Language"
insert_anchor_links = "heading"
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
> ordering, which is what enables deterministic `.to_array!` on streams.

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
> `of`, `with`, `match`, `if`, `else`, `is`, `as`, `import`, `return`,
> `fail`, `true`, `false`.
>
> These cannot be used as binding or type names.

### Identifiers

> r[lex.ident]
>
> Identifiers start with a letter or underscore and continue with letters,
> digits, and underscores. `_` alone is the wildcard pattern — it is not a
> valid binding name.

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
> Attributes use `@` followed by a name and optional arguments:
> `@rename("name")`, `@tag("type")`. Attributes attach
> to the declaration or field that immediately follows them. Multiple
> attributes can stack:
>
> ```vixen
> @tag("type")
> @rename_all("kebab-case")
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
> `@` in pattern position (`name @ pattern`) is the binding operator,
> not an attribute. `@` before a path literal (`@src/main.c`,
> `@(out/{name}.o)`) is also distinct from attributes.
> In patterns, `@name` remains a binding marker. The parser distinguishes
> these by syntactic context.

### Semicolons

> r[lex.semicolons]
>
> Semicolons separate statements inside blocks. A trailing semicolon
> after the last statement is permitted but not required. In blocks, the
> last expression (without a trailing semicolon) is the block's value; a
> trailing semicolon turns it into a statement and the block evaluates
> to `()`.
>
> At file scope, Vixen has **top-level items**, not statements. A source
> file is a sequence of top-level immutable `let` bindings, type
> declarations, and function declarations; there is no file-level
> trailing expression value.
>
> Semicolons do not separate positional from keyword arguments. Keyword
> application uses `where`, as described in `r[expr.call.where]`.

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
> Other: `?` (fallible suffix), `!` (postfix zero-arg invocation), `@`
> (path prefix, pattern binding, attributes), `#` (capability tags on
> function types), `:` (named slots — type annotations and field-value
> pairs), `::` (expression-level type ascription, distinct from `as`
> conversion).

> r[lex.delimiters]
>
> Delimiters: `(` `)`, `[` `]`, `{` `}`, `%{` (legacy map literal opener).

### Numeric literals

> r[lex.numeric]
>
> Integer literals are sequences of digits, optionally separated by
> underscores for readability: `1_000_000`. They default to `U64`.
> Use type ascription (`::`) to specify a different type: `4::I64`.
>
> Float literals contain a decimal point: `3.14`, `0.5`. They default to
> `F64`.

### String literals

> r[lex.string]
>
> String literals are enclosed in double quotes: `"hello"`. Escape
> sequences: `\\`, `\"`, `\n`, `\t`, `\r`, `\0`, `\u{...}` (Unicode code
> point).

> r[lex.string.interpolation]
>
> Interpolated strings use backticks: `` `Hello ${name}` ``. Arbitrary
> expressions are allowed inside `${ }`. Each interpolation calls
> `.to_string()` on the interpolated value. There is no bare-identifier
> shorthand — `${ }` is the only interpolation syntax.

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
> | 6          | `<` `<=` `>` `>=`      | left          | Comparison                  |
> | 7          | `..` `..=`             | none          | Range                       |
> | 8          | `+` `-`                | left          | Additive                    |
> | 9          | `*` `/` `%`            | left          | Multiplicative              |
> | 10         | unary `-` `!`          | prefix        | Negation, logical NOT       |
> | 11         | `as` `as?` `is` `::`   | left          | Type cast / test / ascription |
> | 12         | application            | left          | Unary application           |
> | 13         | postfix chain          | left          | `.name`, `!`, `?`, `[]`     |
>
> Assignment (`=`) is a statement, not an expression. The postfix chain
> associates left-to-right:
>
> ```vixen
> x.y.z!    // means: (((x).y).z)!
> x.y!.z!   // means: (((((x).y)!).z)!)
> f x.y!    // means: f ((((x).y)!))
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
> Top-level type declarations are keyword-led:
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
> fn add(a: U64, b: U64) -> U64 {
>   a + b
> }
> ```
>
> Parameter types are always annotated. The return type may be omitted,
> in which case it defaults to `()` (the empty tuple). The last
> expression in the body is the return value. `return` can be used for
> early exit. A function body that ends with a statement (including an
> empty body) implicitly returns `()`.

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
> fn distance((x1, y1): (F64, F64), (x2, y2): (F64, F64)) -> F64 {
>   ((x2 - x1) * (x2 - x1) + (y2 - y1) * (y2 - y1)).sqrt()
> }
>
> fn greet({ first, last }: Name) -> String {
>   `Hello ${first} ${last}`
> }
> ```

> r[decl.fn.generic]
>
> Generic parameters use square brackets after the function name:
>
> ```vixen
> fn first[T](items: Array[T]) -> T {
>   items[0]
> }
> ```

> r[decl.fn.keyword-args]
>
> Keyword parameters and arguments follow positional ones after `;`:
>
> ```vixen
> fn greet(name: String; cheerful: Bool = false) -> String {
>   if cheerful { `Hey ${name}!!!` } else { `Hello ${name}` }
> }
>
> greet("Amos"; cheerful: true)
> ```

### Scope

> r[decl.scope]
>
> Top-level items are type declarations (`struct`, `enum`, `newtype`,
> `type`) and function declarations. Bindings do not appear at top
> level.
>
> Inside blocks, bindings and local functions are allowed. Top-level type
> declarations are not allowed in statement position; nested type
> declarations appear only in slot type position, immediately after `:`.

## Expression grammar

### Literal expressions

> r[expr.literal.int]
>
> Integer literals: a sequence of digits, optionally separated by
> underscores. Default type is `U64`. Use type ascription to specify a
> different type: `42`, `1_000_000`, `4::I64`.

> r[expr.literal.float]
>
> Float literals: digits with a decimal point. Default type is `F64`:
> `3.14`, `0.5`, `1_000.0`.

> r[expr.literal.bool]
>
> Boolean literals: `true`, `false`. Type is `Bool`.

> r[expr.literal.string]
>
> String literals are enclosed in double quotes: `"hello world"`. Escape
> sequences: `\\`, `\"`, `\n`, `\t`, `\r`, `\0`, `\u{...}` (Unicode code
> point by hex value).

> r[expr.literal.interpolation]
>
> Interpolated strings use backticks: `` `Hello ${name}` ``. The `${ }`
> delimiter contains an arbitrary expression — any expression valid in
> Vixen can appear inside, including function calls, field access, `if`,
> `match`, and nested interpolations:
>
> ```vixen
> let msg = `found ${results.len()} results in ${elapsed}s`
> let label = `status: ${if ok { "pass" } else { "fail" }}`
> ```
>
> There is no bare-identifier shorthand — `${ }` is the only
> interpolation syntax.

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
> A block `{ stmt; stmt; expr }` is an expression. Statements are
> separated by semicolons. The last expression in the block is its value.
> A block ending with a statement (or empty) evaluates to `()`.
> Declarations inside a block are scoped to that block.
>
> ```vixen
> let area = {
>   let w = rect.width;
>   let h = rect.height;
>   w * h
> }
> ```

### Array expressions

> r[expr.array]
>
> Array literals use square brackets: `[1, 2, 3]`. Elements are
> comma-separated. Trailing commas are allowed. Spread syntax inserts
> elements from another array: `[1, ..more, 4]`. In an ordinary array
> literal, a spread element must have type `Array[T]`. The type is
> `Array[T]` where `T` is inferred from the elements.

### Map expressions

> r[expr.map]
>
> Map literals use braces with general key expressions:
>
> ```vixen
> let outputs = { @bin/lua: lua_exe, @lib/liblua.a: liblua }
> let port_names = { 80: "http", 443: "https" }
> ```
>
> Keys and values are arbitrary expressions. Trailing commas are allowed.
> The type is `Map[K, V]`. A braced literal whose entries are all bare
> field labels is a record instead.

### Function call expressions

> r[expr.call]
>
> Function application is unary. `f x` applies `f` to `x`. Chained
> application is left-associative: `f x y` means `(f x) y`, not
> `f(x, y)`.
>
> Tuple, array, and braced payloads are ordinary values, so these are all
> just unary application:
>
> ```vixen
> f x
> f (x, y)
> f [x, y]
> f { ... }
> ```
>
> To access a member on the result of an application, group the
> application first:
>
> ```vixen
> (discover_tool class).path
> ```

> r[expr.call.where]
>
> Keyword application uses `where expr`, where `expr` evaluates to a
> record-shaped value:
>
> ```vixen
> exec [cc, "-c", file] where defaults
> exec [cc, "-c", file] where { mounts: src }
> ```

> r[expr.call.where.spread]
>
> Record spread works inside keyword application. Spreads merge
> left-to-right before keyword checking:
>
> ```vixen
> exec [cc, "-c", file] where { ..common, mounts: src }
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
> let p = Point { x: 1.0, y: 2.0 }
> ```

### Index expressions

> r[expr.index]
>
> `array[i]` accesses an element by index. `map[key]` accesses a value by
> key. Out-of-bounds or missing keys poison the result. The fallible
> version `array[i]?` returns `Option` instead.

### Field access expressions

> r[expr.field]
>
> `value.field` accesses a struct field by name. `tuple._0`, `tuple._1`
> access tuple elements by position (underscore prefix avoids ambiguity
> with float literals). When `field` names a zero-arg method,
> `value.field` is the bound method value and `value.field!` invokes it.
> `.` and postfix `!` form one left-associative postfix chain, so
> `x.y.z!` means `(((x).y).z)!` and `x.y!.z!` means `(((((x).y)!).z)!)`.

> r[expr.field.variant]
>
> `Enum.Variant` accesses a variant under the enum's namespace.
> `Shape.Circle` names both the variant-specific type and the variant's
> constructor value. `Shape.Circle { radius: 5.0 }` produces a
> `Shape.Circle`, which widens to `Shape` in checking contexts.

### Method navigation and pipe expressions

> r[expr.method]
>
> Dot access never falls back to arbitrary free functions. `x.f` means
> field access or method lookup only. Methods live in `namespace Type {
> ... }`; extensions live in import-scoped `extend Type { ... }`.
>
> Leading-dot forms are method adapters. `.name` yields a bound-method
> adapter; `.name!` yields an adapter that invokes the zero-arg method.

> r[expr.pipe]
>
> `|>` is the chaining form for free functions and pipeline transforms.
> `_` inside a pipe RHS names the piped value:
>
> ```vixen
> cexec [ar, ...] |> cexec [ranlib, _]
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
> expression, including a block: `(x) => { let y = x + 1; y }`.
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
> items.map ($ + 1)      // means: items.map((x) => x + 1)
> files.map ($.utf8!)    // means: files.map((x) => x.utf8!)
> ```

> r[expr.closure.dollar-numbered]
>
> `$` is `$0`. For multi-argument closures, use `$0`, `$1`, `$2`, etc.
> The highest numbered `$N` determines the closure's arity:
>
> ```vixen
> items.reduce ($0 + $1)   // means: items.reduce((a, b) => a + b)
> ```
>
> `$` and `$N` bind to the nearest enclosing function argument position
> where a closure is expected. When `$` appears in nested calls, it
> belongs to the innermost one:
>
> ```vixen
> // $ belongs to find, not map:
> items.map (list.find ($ == 2))
>
> // To reference the map element, use a named closure:
> items.map ((item) => list.find ($ == item))
> // here $ is find's argument, item is map's argument
> ```

### Partial application expressions

> r[expr.partial]
>
> `partial f(...)` partially applies a function, producing a new function
> with the written arguments bound. Omitted positional arguments remain
> open, and unspecified keyword arguments remain open:
>
> ```vixen
> let add_two = partial add(2)
> let loud_compile = partial compile where { loudly: true }
> ```

> r[expr.partial.keyword]
>
> Partial application can bind keyword arguments through `where`. Bound
> keyword arguments are sealed — they cannot be overridden by the caller:
>
> ```vixen
> fn compile(src: String) where { optimize: Bool = false, loudly: Bool = false } { ... }
>
> let loud_compile = partial compile where { loudly: true }
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
> `if condition { then_body } else { else_body }`. No parentheses around
> the condition, braces required around both branches. `else if` chains
> are allowed. `if` is always an expression, so `else` is always
> required.

### Match expressions

> r[expr.match]
>
> ```vixen
> match value {
>   pattern => expr,
>   pattern if guard => expr,
>   _ => default_expr,
> }
> ```
>
> Arms are comma-separated. Each arm is `pattern => expr` with an optional
> `if guard` between the pattern and the arrow. Arms can use block bodies:
> `pattern => { stmts; expr }`. `match` must be exhaustive — every
> possible value must be covered. A wildcard `_` catches everything else.

### Import expressions

> r[expr.import]
>
> `import("path")` loads a module and makes its exported names available
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
> let pi = 3.14 :: F64   // explicit (though F64 is the default)
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
> if config.edition is Some { value: edition } {
>   use_edition(edition)
> }
> ```

### Fallible expressions

> r[expr.fallible]
>
> The `?` suffix catches poisoning and returns `Option` instead:
> success becomes `Some`, failure becomes `None`. It applies to any
> expression that may poison — unary application (`(f x)?`), zero-arg
> invocation (`value.method!?`), and index expressions (`array[i]?`).
> The `as?` operator
> follows the same pattern (see `r[type.as-fallible]`).

### Statements

> r[expr.statement]
>
> Assignment (`name = expr`, `name: Type = expr`) and compound update
> (`name with= { ... }`) are statements, not expressions. Declarations
> (`fn`, `struct`, `enum`, `newtype`) are also statements.

## Modules

> r[lang.modules]
>
> A `.vx` file is a module. Its top-level bindings (including immutable
> `let` bindings, functions, and types) are the module's exports. There
> is no visibility modifier — everything at the top level is visible to
> importers.

> r[lang.modules.namespace]
>
> Modules are namespaces, not runtime values. `import("path")` makes
> the module's exported names available. There is no first-class
> `Module` object that can be stored in a binding, passed to a function,
> or compared for equality.

> r[lang.modules.access]
>
> Access module contents with dots: `std.rust.workspace(src, args, env)`.
> Destructuring binds names from the namespace:
> `{ rust } = import("std")`.

> r[lang.modules.sources]
>
> Three kinds of module sources:
>
> - **Local files** — paths starting with `./` are relative to the current
>   file: `import("./lib/helpers")` loads `./lib/helpers.vx`.
> - **Standard library** — `import("std")` loads the built-in standard
>   library.
> - **External packages** — resolution of non-local, non-std imports is a
>   future concern. The mechanism is the same: `import` returns a module
>   value.

## Scalar types

> r[type.scalar]
>
> Vixen has scalar types: `U64`, `I64`, `F64`, `Bool`, `String`.

> r[type.scalar.literals]
>
> Integer literals default to `U64`. Float literals default to `F64`.
> Literal syntax is specified in `r[lex.numeric]`, `r[lex.string]`, and
> `r[lex.string.interpolation]`. Type ascription selects a different type:
> `4::I64`.

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
> `Shape.Circle { radius: 5.0 }`. Zero-sized variants use the type name:
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
> String interpolation (`` `${expr}` ``) calls `.to_string()` on the
> interpolated value. Built-in scalar and path-like types provide
> canonical `to_string()` methods. User-defined types participate by
> defining `to_string(self) -> String` in their canonical namespace.

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
> `if`/`else` works like in Rust — no parentheses around the condition,
> braces required. `if`/`else` is an expression — it evaluates to a value.

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
> `match (status, kind) { (0, "lib") => ..., (code, _) => ... }`.

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
> if config.edition is Some { value: edition } {
>   use_edition(edition)
> } else {
>   use_default()
> }
> ```

### Block expressions

> r[control.block]
>
> A block `{ ... }` is an expression. The last expression in the block is
> its value. Bindings inside a block are scoped to that block. Blocks
> replace `let`/`in` and `where` clauses from other languages.

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
> match items {
>   [] => "empty",
>   [only] => `just ${only}`,
>   [first, ..] => `starts with ${first}`,
>   [first, .., last] => `${first} to ${last}`,
>   [_, second, ..rest] => use(second, rest),
> }
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
> Function types use `Fn(Args) -> Ret` syntax. Capability tags appear
> between the parameter list and the return type arrow: `Fn(Args) #fs -> Ret`.
> See `r[cap.model]`. Functions with keyword arguments are first-class;
> keyword-parameter shape is part of the function's call surface rather
> than a reason the function cannot be passed around.

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
> anywhere in an expression, including in function arguments, enabling
> partial application: `items.map (add($, 1))` means
> `items.map((x) => add(x, 1))`.

> r[fn.closure.numbered]
>
> `$` is `$0`. For multi-argument closures, use `$0`, `$1`, `$2`, etc.:
> `items.reduce ($0 + $1)` means `items.reduce((a, b) => a + b)`.

## Methods and pipe

> r[fn.method]
>
> Dot navigation is for fields and methods only. Vixen does not use
> free-function UFCS. Methods live in type namespaces; free functions are
> chained with `|>`.
>
> A leading-dot method adapter such as `.utf8!` or `.next!` denotes a
> callable adapter value rather than immediate invocation.

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
> Blob "hello"
> Blob 42
> Point { x: 1.0, y: 2.0 }
> ```
>
> Named-field constructor surfaces are canonical for structs and
> record-like enum variants. Struct constructors accept anonymous
> structural record payloads. Variant constructors return the
> variant-specific type and widen to the parent enum when needed.

## Records

> r[type.record]
>
> Anonymous records use braces with field labels:
>
> ```vixen
> let common = { verifier, observer: make_observer() }
> let pair = { left: 1, right: 2 }
> ```
>
> A bare field name is shorthand for `name: name`. Records are real
> values with structural types such as `{ left: U64, right: U64 }`.

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
> Map literals also use braces, but with value keys rather than field
> labels:
>
> ```vixen
> let outputs = { @bin/lua: lua_exe, @lib/liblua.a: liblua }
> let port_names = { 80: "http", 443: "https" }
> let build_flags = { (target): flags_for target }
> ```
>
> The type is `Map[K, V]`. Keys can be any Vixen type — all types are
> comparable for equality and hashable. A braced literal whose entries
> are all bare field labels is a record; a braced literal with general
> key expressions is a map.

> r[collect.map.order]
>
> Maps are ordered by key using Vixen's universal total ordering.
> Iterating a map (via combinators or `.keys!` / `.values!`) always
> produces entries in sorted key order. This makes map iteration
> deterministic.

## Ranges

> r[collect.range]
>
> Range literals: `0..10` (exclusive), `0..=10` (inclusive), `..10`
> (to), `0..` (from), `..` (full). Ranges can be used for slicing
> arrays.

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
> array, call `.to_array!`, which sorts by universal total ordering —
> not by the original input position. This means
> `([3, 1, 2].map ($ * 2)).to_array!` returns `[2, 4, 6]`, not
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
> `.next!` returns the first element to resolve from a `Stream[T]`.
> It poisons if the stream completes cleanly with no elements. The
> fallible form `.next!?` returns `Option[T]` instead. If multiple
> elements may resolve, the chosen element is whichever resolves first.
> Use `.next!` only when that race is semantically intended or when the
> stream is known to contain at most one element.

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
> the results into a single stream. `.flatten!` flattens a stream of
> streams.

> r[comb.merge]
>
> `.merge!` on `Stream[Tree]` waits for all trees and produces a single
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
> `.to_array!` first.

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
> `.to_array!` converts a `Stream` back into an `Array`. It waits for
> all elements to resolve, then sorts them using Vixen's universal total
> ordering to produce a deterministic result. `.to_map!` converts a
> stream of `(key, value)` pairs into a `Map`. Both are pending until
> all elements resolve.

> r[comb.collect.ordering]
>
> Arrays produced by stream collection operations are canonically
> ordered using Vixen's universal total ordering. This applies to
> `.to_array!`, group arrays in `.group_by`, and both arrays
> returned by `.partition()`.

> r[comb.map-combinators]
>
> Map combinators pass `(key, value)` pairs to the closure. `.keys!`
> returns `Array[K]`, `.values!` returns `Array[V]`.

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
> `.map f` transforms the inner value (`None` stays `None`).
> `.and_then f` flat maps — `f` returns `Option`, the result is
> flattened. `.is_some!` and `.is_none!` return `Bool`.

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
> environment drifted from its recorded discovery.

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

> r[error.catch]
>
> The `?` suffix catches poisoning and returns `Option`. It applies to
> unary application, zero-arg invocation, and index expressions.
> `(f x)?` returns
> `None` on failure, `Some(result)` on success. The `?` suffix always
> means "the fallible version that returns Option instead of poisoning."
>
> This is the same pattern as `as` / `as?`.

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
> restored: `.to_array!` waits for all elements to resolve, then sorts
> them using Vixen's universal total ordering (all values are comparable).
> The result is a stable, deterministic `Array` regardless of which
> elements resolved first.
>
> This means `([3, 2, 1].to_stream!).to_array!` returns `[1, 2, 3]`.
> For build systems, this is the right default — the collection of
> artifacts is in a stable order before linking, regardless of which
> compilation finished first. If a specific semantic order is needed
> (e.g., sorting files by size), call a custom sort on the resulting
> `Array`.

> r[stream.of]
>
> `Stream.of(value)` creates a single-element stream.
