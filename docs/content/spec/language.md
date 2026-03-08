+++
title = "Specification"
insert_anchor_links = "heading"
+++

# Vixen Language Specification

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
> snake_case. This is enforced by the evaluator. Because of this
> distinction, there is no `type` keyword — `Name = struct { ... }` is a
> type declaration (UpperCamelCase), `name = Name { ... }` is a binding
> (snake_case). The casing rule makes them unambiguous.

## Lexical grammar

### Keywords

> r[lex.keywords]
>
> Reserved words: `fn`, `struct`, `enum`, `newtype`, `of`, `with`,
> `match`, `if`, `else`, `is`, `as`, `import`, `return`, `fail`, `true`,
> `false`.
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
> This replaces a `type` keyword: `Name = struct { ... }` is a type
> declaration because `Name` is UpperCamelCase.

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

### Semicolons

> r[lex.semicolons]
>
> Statements are separated by semicolons. This applies uniformly — at
> module top level and inside blocks. In blocks, the last expression is
> what the block evaluates to (this includes function bodies). A source
> file is a sequence of semicolon-separated statements; there is no
> trailing expression value at file level.

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
> Arrow: `=>` (match arms, short closures, map entries), `->` (return
> types).
>
> Dot: `.` (field access, UFCS function calls).
>
> Other: `?` (fallible suffix), `@` (pattern binding, attributes), `:` (named
> slots — type annotations and field-value pairs, always after an identifier),
> `::` (expression-level type ascription, distinct from `as` conversion).

> r[lex.delimiters]
>
> Delimiters: `(` `)`, `[` `]`, `{` `}`, `%{` (map literal opener).

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
> expressions are allowed inside `${ }`. There is no bare-identifier
> shorthand — `${ }` is the only interpolation syntax.

### Operator precedence

> r[lex.precedence]
>
> From lowest to highest precedence:
>
> | Precedence | Operators          | Associativity | Description           |
> |------------|--------------------|---------------|-----------------------|
> | 1          | `\|\|`             | left          | Logical OR            |
> | 2          | `&&`               | left          | Logical AND           |
> | 3          | `==` `!=`          | left          | Equality              |
> | 4          | `<` `<=` `>` `>=`  | left          | Comparison            |
> | 5          | `..` `..=`         | none          | Range                 |
> | 6          | `+` `-`            | left          | Additive              |
> | 7          | `*` `/` `%`        | left          | Multiplicative        |
> | 8          | unary `-` `!`      | prefix        | Negation, logical NOT |
> | 9          | `as` `as?` `is`    | left          | Type cast / test      |
> | 10         | `?`                | postfix       | Fallible              |
> | 11         | `.`                | left          | Field access / UFCS   |
> | 12         | `()` `[]`          | postfix       | Call, index           |
>
> Assignment (`=`) is a statement, not an expression.

## Declaration grammar

### Bindings

> r[decl.binding]
>
> A binding associates a name with a value: `name = expr`. An optional
> type annotation can be added: `name: Type = expr`. The name must be
> snake_case. There is no `let` keyword — casing distinguishes bindings
> from type declarations.

> r[decl.binding.destructure]
>
> Bindings can destructure: `{ first, last } = name`. The type is inferred
> from the right-hand side. Tuple destructuring: `(a, b) = pair`.

### Type declarations

> r[decl.type]
>
> A type declaration uses UpperCamelCase on the left side of `=`:
> `Name = struct { ... }` or `Name = enum { ... }`. No `type` keyword —
> casing is sufficient.

### Struct declarations

> r[decl.struct]
>
> ```vixen
> Point = struct {
>   x: F64,
>   y: F64,
> }
> ```
>
> Fields are comma-separated. Trailing commas are allowed.

> r[decl.struct.nested]
>
> A field type can be declared inline:
>
> ```vixen
> CargoToml = struct {
>   package: Package = struct {
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
> Shape = enum {
>   Circle { radius: F64 },
>   Rect { width: F64, height: F64 },
>   Empty,
> }
> ```
>
> Variants are comma-separated. Each variant can have no payload, a tuple
> payload (`Variant(Type)`), or a struct payload (`Variant { field: Type }`).

> r[decl.enum.alias]
>
> A variant can alias an existing type: `File: Blob`. The shorthand `:Blob`
> means `Blob: Blob`.

> r[decl.enum.sum]
>
> `Entry = Blob | Tree` is shorthand for an enum where each variant is an
> alias named after its type.

### Newtype declarations

> r[decl.newtype]
>
> ```vixen
> Blake3 = newtype of String;
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
> Keyword arguments follow positional arguments in a separate braced group:
>
> ```vixen
> fn greet(name: String, { cheerful: Bool = false }) -> String {
>   if cheerful { `Hey ${name}!!!` } else { `Hello ${name}` }
> }
>
> greet("Amos", cheerful: true)
> ```

### Scope

> r[decl.scope]
>
> Declarations (bindings, types, functions) can appear anywhere — at
> module top level or inside any block. Declarations inside a block are
> scoped to that block.

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
> msg = `found ${results.len()} results in ${elapsed}s`
> label = `status: ${if ok { "pass" } else { "fail" }}`
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
> area = {
>   w = rect.width;
>   h = rect.height;
>   w * h
> }
> ```

### Array expressions

> r[expr.array]
>
> Array literals use square brackets: `[1, 2, 3]`. Elements are
> comma-separated. Trailing commas are allowed. The type is `Array[T]`
> where `T` is inferred from the elements.

### Map expressions

> r[expr.map]
>
> Map literals use `%{ }` with `=>` separating keys from values:
>
> ```vixen
> coords = %{ "lat" => 53.156, "lng" => 145.23 }
> port_names = %{ 80 => "http", 443 => "https" }
> ```
>
> Keys and values are arbitrary expressions. Trailing commas are allowed.
> The type is `Map[K, V]`.

### Function call expressions

> r[expr.call]
>
> `f(arg1, arg2)` calls a function with positional arguments. Trailing
> commas are allowed. Keyword arguments follow positional arguments using
> `:` syntax:
>
> ```vixen
> greet("Amos", cheerful: true)
> acquire(url, sha256: hash)
> ```
>
> Keyword arguments can appear in any order but must come after all
> positional arguments.

> r[expr.call.spread]
>
> Spread syntax `..value` in keyword argument position expands a struct's
> fields into keyword arguments. Only fields whose names match a keyword
> parameter are used — extra fields are ignored. Order matters: explicit
> keyword arguments after the spread override spread values.
>
> ```vixen
> fn render(template: String, { width: U64, height: U64, title: String }) { ... }
>
> defaults = Config { width: 800, height: 600, title: "untitled", debug: true };
> render("page.html", ..defaults, title: "Home")
> // debug is ignored (not a keyword param), title is overridden
> ```

### Struct literal expressions

> r[expr.struct-literal]
>
> `TypeName { field: value, field2: value2 }` constructs a struct. The
> `:x` shorthand means `x: x` — reuses a binding as the field value.
> Spread syntax `..other` copies remaining fields from another value.
> Trailing commas are allowed.
>
> ```vixen
> p = Point { x: 1.0, y: 2.0 }
> p2 = Point { :x, y: 2.0 }
> p3 = Point { ..p, y: 3.0 }
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
> with float literals).

> r[expr.field.variant]
>
> `Enum.Variant` accesses a variant as a type under the enum's namespace.
> `Shape.Circle` is a type — distinct from `Shape`. This is field access
> on a type, not on a value. Variant types can be used to construct
> values (`Shape.Circle { radius: 5.0 }`) or in type position.

### UFCS expressions

> r[expr.ufcs]
>
> `x.f(args)` where `f` is not a field of `x` desugars to `f(x, args)`.
> The dot operator tries field access first; if no field matches, it
> rewrites to a function call with the left-hand side as the first
> argument:
>
> ```vixen
> items.map($ + 1).filter($ > 3).to_array()
> // desugars to:
> to_array(filter(map(items, $ + 1), $ > 3))
> ```

### Update expressions

> r[expr.with]
>
> `value with { field: new_value }` produces a new value with some fields
> changed. Fields not mentioned are kept from the original. See
> [type.struct.with] and [type.struct.with-assign].

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

### Closure expressions

> r[expr.closure]
>
> Two syntactic forms:
>
> - Block form: `fn(x, y) { body }` — parameters in parentheses, body in
>   braces.
> - Arrow form: `(x, y) => expr` — parameters in parentheses, body is a
>   single expression.
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
> items.map($ + 1)          // means: items.map((x) => x + 1)
> items.map(add($, 1))      // means: items.map((x) => add(x, 1))
> items.filter($.len() > 3) // means: items.filter((x) => x.len() > 3)
> ```

> r[expr.closure.dollar-numbered]
>
> `$` is `$0`. For multi-argument closures, use `$0`, `$1`, `$2`, etc.
> The highest numbered `$N` determines the closure's arity:
>
> ```vixen
> items.reduce($0 + $1)   // means: items.reduce((a, b) => a + b)
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
> `@f(args, ..)` partially applies a function, producing a new function
> with some arguments bound. `..` marks where the remaining positional
> arguments go and appears exactly once:
>
> ```vixen
> add_two = @add(2, ..)              // Fn(U64) -> U64
> double = @multiply(.., 2)          // Fn(U64) -> U64
> both = @clamp(0, .., 255)          // Fn(U64) -> U64
> ```

> r[expr.partial.keyword]
>
> Partial application can bind keyword arguments. Once bound, keyword
> arguments are sealed — they cannot be overridden by the caller:
>
> ```vixen
> fn compile(src: String, { optimize: Bool = false, loudly: Bool = false }) { ... }
>
> loud_compile = @compile(.., loudly: true)  // Fn(String) -> ...
> // loudly is sealed at true, optimize uses its default
> ```

> r[expr.partial.passable]
>
> A function with keyword arguments cannot be passed as a value. Partial
> application with `@` closes over keyword arguments, producing a plain
> `Fn` that can be passed:
>
> ```vixen
> fn greet(name: String, { cheerful: Bool = false }) -> String { ... }
>
> items.map(greet)                        // ERROR: greet has keyword args
> items.map(@greet(.., cheerful: true))   // OK: Fn(String) -> String
> items.map(@greet(..))                   // OK: cheerful uses default
> ```

### Conditional expressions

> r[expr.if]
>
> `if condition { then_body } else { else_body }`. No parentheses around
> the condition, braces required around both branches. `else if` chains
> are allowed. When used as an expression (its value is used), the `else`
> branch is required. When used as a statement (value discarded), the
> `else` branch is optional and the whole thing evaluates to `()`.

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
> `import("path")` is an expression that evaluates to a module value.
> The argument is a string literal — not a general expression.

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
> zero = 0 :: I64    // literal 0 as I64, no conversion
> pi = 3.14 :: F64   // explicit (though F64 is the default)
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
> if config.edition is Some(edition) {
>   use_edition(edition)
> }
> ```

### Fallible expressions

> r[expr.fallible]
>
> Any function or method call can be suffixed with `?` to catch poisoning
> and return `Option` instead: `f(x)?` returns `None` on failure,
> `Some(result)` on success.

### Statements

> r[expr.statement]
>
> Assignment (`name = expr`, `name: Type = expr`) and compound update
> (`name with= { ... }`) are statements, not expressions. Declarations
> (`fn`, `struct`, `enum`, `newtype`) are also statements.

## Modules

> r[lang.modules]
>
> `import` is an expression that returns a module value. A `.vixen` file
> is a module. Its top-level bindings (including types) are the module's
> exports. There is no visibility modifier — everything at the top level
> is visible to importers.

> r[lang.modules.access]
>
> Access module contents with dots: `std.rust.workspace(src, args, env)`.
> Destructuring works too: `{ rust } = import("std")`.

> r[lang.modules.sources]
>
> Three kinds of module sources:
>
> - **Local files** — paths starting with `./` are relative to the current
>   file: `import("./lib/helpers")` loads `./lib/helpers.vixen`.
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
> Literal syntax is specified in [lex.numeric], [lex.string], and
> [lex.string.interpolation]. Type ascription selects a different type:
> `4::I64`.

## Newtypes

> r[type.newtype]
>
> A newtype is a distinct type backed by an existing type. The new type
> requires explicit conversion via `as`:
>
> ```vixen
> Blake3 = newtype of String;
> hash = blake3_hash_of(something); // type Blake3
> raw = hash as String;             // explicit conversion
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
> Structs are defined with `Name = struct { ... }`. The `:` shorthand in
> construction — `:x` means `x: x` — reuses a binding name as the field
> name. Spread syntax `..other` copies fields from another value.

> r[type.struct.with]
>
> `with` creates a new value with some fields changed:
>
> ```vixen
> updated = name with { last: "Wilson" }
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
> Types can be declared inline in field position: `package: Package =
> struct { ... }`. The nested type lives under the parent's namespace and
> can be referenced as `CargoToml.Package`.

> r[type.struct.destructure]
>
> Structs can be destructured in bindings. The type name is not needed —
> it is inferred from the right-hand side: `{ first, last } = name`.
> For enum variants in `match` or `is`, the variant name is always
> required.

## Enums

> r[type.enum]
>
> Vixen has Rust-like enums. Each variant is a type under the enum's
> namespace. `Shape.Circle` is a type, and `Shape` is a type. They are
> distinct.

> r[type.enum.variant-kinds]
>
> Variants can have no payload, a tuple payload, or a struct payload:
>
> - `Color.Red` — zero-sized type
> - `Token.Number` — tuple type, fields accessed with `._0`
> - `Shape.Circle` — struct type, fields accessed by name

> r[type.enum.no-recursion]
>
> Recursive types are not allowed. Every value must have a finite size.
> There is no indirection (no references, no boxing) to break cycles.

> r[type.enum.alias]
>
> A variant can be an alias for an existing type using colon syntax:
> `File: Blob`. The `:` shorthand reuses the type name: `:Blob` means
> `Blob: Blob`.

> r[type.enum.sum-shorthand]
>
> `Entry = Blob | Tree` is shorthand for an enum where each variant is
> an alias named after its type.

> r[type.enum.build]
>
> To build a variant value (narrow type): `Shape.Circle { radius: 5.0 }`.
> To build an enum value (wide type), use the enum as a constructor:
> `Shape(Circle { radius: 5.0 })` — variant names are in scope inside
> the constructor.

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
> A variant value auto-widens to its parent enum type when the context
> expects the enum. This chains through alias variants.
> `Shape.Circle { radius: 5.0 }` widens to `Shape` automatically. Sum
> type shorthands also widen: a `String` widens to `ExecArg` when
> `ExecArg = String | Path`.
>
> The reverse is not true — narrowing requires `as` or `match`.

> r[type.coerce.stringify]
>
> String interpolation (`` `${expr}` ``) calls `stringify(expr)`. The
> `stringify` function is overloaded — built-in overloads exist for
> scalar types (`U64`, `I64`, `F64`, `Bool`, `String`). User-defined
> types must provide their own `fn stringify(x: MyType) -> String`
> overload or interpolation is a compile error.

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
> success. It replaces `if let` from Rust — Vixen has no `let` keyword.
> `is` is an expression that evaluates to `Bool`.
>
> ```vixen
> if config.edition is Some(edition) {
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
> `Circle { radius }`, `Some(value)`, `None`. In `match`, the variant
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
>   [only] => `just ${stringify(only)}`,
>   [first, ..] => `starts with ${stringify(first)}`,
>   [first, .., last] => `${stringify(first)} to ${stringify(last)}`,
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
> Function types use `Fn(Args) -> Ret` syntax. Only functions with
> positional arguments can have their type named. Functions with keyword
> arguments cannot be passed as values — use partial application (`@`)
> to close over keyword arguments and produce a passable `Fn`.

## Closures

> r[fn.closure]
>
> Closures can be written as `fn(x) { body }` or `(x) => body`.

> r[fn.closure.dollar]
>
> `$` is shorthand for the first argument of a closure. `$` works
> anywhere in an expression, including in function arguments, enabling
> partial application: `items.map(add($, 1))` means
> `items.map((x) => add(x, 1))`.

> r[fn.closure.numbered]
>
> `$` is `$0`. For multi-argument closures, use `$0`, `$1`, `$2`, etc.:
> `items.reduce($0 + $1)` means `items.reduce((a, b) => a + b)`.

## UFCS

> r[fn.ufcs]
>
> `x.f(args)` where `f` is not a field of `x` desugars to `f(x, args)`.
> The dot operator tries field access first; if no field matches, it
> rewrites to a function call with the left-hand side as the first
> argument. This is the only call chaining mechanism — there is no pipe
> operator and no methods.
>
> ```vixen
> items.map($ + 1).filter($ > 3).to_array()
> // desugars to:
> to_array(filter(map(items, $ + 1), $ > 3))
> ```

## Function overloading

> r[fn.overload]
>
> Multiple functions can share the same name if they differ in parameter
> types. The evaluator selects the matching overload based on the known
> types of the arguments at the call site.

## Constructors

> r[fn.constructor]
>
> A type name can be overloaded as a constructor function. The evaluator
> distinguishes struct literal syntax (`Name { ... }`) from function call
> syntax (`Name(...)`) by the delimiter:
>
> ```vixen
> Blob = struct { content: String };
>
> fn Blob(s: String) -> Blob { Blob { content: s } }
> fn Blob(n: U64) -> Blob { Blob { content: n.to_string() } }
>
> b = Blob("hello")           // function call — constructor
> b2 = Blob { content: "x" }  // struct literal — direct construction
> ```

## Arrays

> r[collect.array]
>
> Array literals use square brackets: `[1, 2, 3]`. The type is
> `Array[T]`.

## Maps

> r[collect.map]
>
> Map literals use `%{ }` with `=>` for entries:
>
> ```vixen
> coords = %{ "lat" => 53.156, "lng" => 145.23 }
> port_names = %{ 80 => "http", 443 => "https" }
> build_flags = %{ target => flags_for(target) }
> ```
>
> The type is `Map[K, V]`. Keys can be any Vixen type — all types are
> comparable for equality and hashable.

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
> (to), `0..` (from), `..` (full). Ranges can be used for slicing
> arrays.

## Combinators

> r[comb.model]
>
> There are no `for` loops in Vixen. Iteration is done through
> combinators. `.map()` and `.filter()` always return `Stream`, whether
> called on an Array, Map, or Stream. Every element is an independent
> concurrent computation. Results are produced as they become available,
> not in input order.

> r[comb.map]
>
> `.map(f)` applies `f` to each element and returns a `Stream` of results.

> r[comb.filter]
>
> `.filter(pred)` returns a `Stream` of elements where `pred` is true.

> r[comb.find]
>
> `.find(pred)` returns whichever matching element resolves first. The
> result is `Option[T]`. Because streams are unordered, `.find()` is
> nondeterministic — different runs may return different elements.

> r[comb.fold]
>
> `.fold(init, f)` reduces all elements to a single value using an
> accumulator.

> r[comb.any]
>
> `.any(pred)` returns `true` if any element satisfies the predicate.

> r[comb.all]
>
> `.all(pred)` returns `true` if all elements satisfy the predicate.

> r[comb.flat-map]
>
> `.flat_map(f)` maps each element to a stream (or array) and flattens
> the results into a single stream. `.flatten()` flattens a stream of
> streams.

> r[comb.filter-map]
>
> `.filter_map(f)` maps and filters in one step — the function returns
> `Option[T]`. `Some` values are produced, `None` values are skipped.

> r[comb.reduce]
>
> `.reduce(f)` is like `.fold` but uses the first element as the initial
> accumulator. On streams, element order is nondeterministic — use
> `.to_array()` first if a deterministic reduction order is needed.

> r[comb.group-by]
>
> `.group_by(f)` groups elements by key function, returning
> `Map[K, Array[T]]`.

> r[comb.partition]
>
> `.partition(pred)` splits elements into two arrays by predicate,
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

> r[comb.map-combinators]
>
> Map combinators pass `(key, value)` pairs to the closure. `.keys()`
> returns `Array[K]`, `.values()` returns `Array[V]`.

## Option

> r[option.type]
>
> `Option[T]` is a built-in enum with variants `Some(T)` and `None`.

> r[option.sugar]
>
> `T?` is sugar for `Option[T]`.

> r[option.methods]
>
> Option methods: `.unwrap_or(default)` provides a fallback value.
> `.map(f)` transforms the inner value (`None` stays `None`).
> `.and_then(f)` flat maps — `f` returns `Option`, the result is
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
> Any function or method call can be suffixed with `?` to catch poisoning
> and return `Option` instead. `f(x)?` returns `None` on failure,
> `Some(result)` on success. The `?` suffix always means "the fallible
> version that returns Option instead of poisoning."
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
> become available, not in input order. `.map(f)` over a stream is
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
> This means `[3, 2, 1].to_stream().to_array()` returns `[1, 2, 3]`.
> For build systems, this is the right default — the collection of
> artifacts is in a stable order before linking, regardless of which
> compilation finished first. If a specific semantic order is needed
> (e.g., sorting files by size), call a custom sort on the resulting
> `Array`.

> r[stream.of]
>
> `Stream.of(value)` creates a single-element stream.

## Store types

> r[store.blob]
>
> `Blob` is a single file in the content-addressable store (keyed by
> Blake3 hashes). `Blob.from_string(s)` creates a blob from a text
> string. Blobs with binary content come from `exec`.

> r[store.tree]
>
> `Tree` is a directory with its full structure and filenames preserved.
> There is no separate "local tree" vs "store tree" type — the runtime
> handles materialization transparently.
>
> `Tree.of(map)` creates a tree from a map of names to entries.
> `.subtree(path)` selects a subtree. `.glob(pattern)` filters by glob.
> `.merge(other)` merges two trees (later entries win). These operations
> are lazy.

> r[store.path]
>
> `Path` is its own type — not a newtype of `String`. Root paths come
> from `.at()` on store types. Sub-paths are built with the `/` operator.
> Paths carry their store references — when a `Path` appears in an `exec`
> argument, the runtime materializes the referenced content in the VFS.
>
> String interpolation with a `Path` produces an `ExecArg` (not a plain
> `String`) that preserves store dependencies.

## Exec

> r[exec.signature]
>
> `exec` runs an external process on an executor — a sandboxed runner
> with a virtual filesystem. It is generic over `T`:
>
> ```vixen
> exec[T](
>   args: Array[ExecArg],
>   env: Map[String, ExecArg],
>   mounts: Map[Path, Mount],
>   observer: Fn(Process, Tree) -> Stream[T],
> ) -> Stream[T]
> ```

> r[exec.arg]
>
> `ExecArg = String | Path`. A plain `String` is a literal argument. A
> `Path` carries a store reference — the runtime ensures the referenced
> content is available in the VFS.

> r[exec.mount]
>
> `Mount` has a `source: Blob | Tree` and `writable: Bool`. Read-only
> mounts (default) make content available but immutable. Writable mounts
> allow the process to write output. All mount paths are relative.

> r[exec.process]
>
> `Process` has `stdout: Stream[Blob]`, `stderr: Stream[Blob]`, and
> `exit_code: U64` (pending until the process exits). No stdin — processes
> are not interactive. No signal API.

> r[exec.observer]
>
> The observer runs on the executor, not the orchestrator. It is
> serialized along with its captured values and shipped to the executor.
> A closure cannot be sent until all its captures are resolved. The
> observer receives a `Process` and the VFS root as a `Tree`, and returns
> `Stream[T]`.

> r[exec.observer.materialization]
>
> When the observer produces a value back to the orchestrator, any VFS
> paths in the value are committed to the content-addressable store
> automatically. The runtime handles data movement so the executor can be
> released.

> r[exec.observer.streaming]
>
> Streaming observers parse process output incrementally and produce
> events as artifacts become available. Downstream dependents can start
> work early — e.g., compiling a dependent crate as soon as metadata is
> emitted, without waiting for code generation.

> r[exec.observer.simple]
>
> Simple observers reference `exit_code` and VFS paths for output.
> Everything resolves when the process finishes.

> r[exec.errors]
>
> When a process crashes or the executor fails, the stream terminates.
> Pending values derived from that stream become poisoned.
> Application-level errors are the observer's responsibility — parse
> stdout/stderr and call `fail`.

## Deserialization

> r[deser.parse]
>
> Vixen has built-in parsers for structured data formats. Parsing is
> driven by type: `Toml.parse[CargoToml](blob)`. Field names map to keys,
> `Option` fields are optional, sum types handle polymorphic values.

> r[deser.rename]
>
> `@rename("name")` renames a single field or variant.
> `@rename_all("kebab-case")` renames all fields or variants in a
> container. Supported conventions: `"snake_case"`, `"camelCase"`,
> `"PascalCase"`, `"kebab-case"`, `"SCREAMING_SNAKE_CASE"`.

> r[deser.flatten]
>
> `@flatten` merges a nested struct's fields into the parent during
> serialization/deserialization.

> r[deser.tag]
>
> Enum tagging modes: externally tagged (default), `@tag("field")` for
> internally tagged, `@tag("field", content: "data")` for adjacently
> tagged, `@untagged` for untagged (tries each variant in order).

## String methods

> r[string.test]
>
> `.starts_with(s)`, `.ends_with(s)`, `.contains(s)`, `.is_empty()` — all
> return `Bool`.

> r[string.split]
>
> `.split(sep)` returns `Array[String]`. `.split_once(sep)` returns
> `Option[(String, String)]`. `.join(sep)` on an array joins elements.
> `.lines()` splits by newline.

> r[string.transform]
>
> `.trim()`, `.trim_start()`, `.trim_end()` strip whitespace.
> `.to_upper()`, `.to_lower()` change case. `.replace(old, new)` replaces
> all occurrences. `.replace_once(old, new)` replaces the first.

> r[string.slice]
>
> `.len()` returns byte length. `.chars()` returns `Array[String]` of
> Unicode grapheme clusters. `.slice(range)` returns a substring.

> r[string.parse]
>
> `.parse_u64()`, `.parse_i64()`, `.parse_f64()` parse strings to numbers.
> They poison on invalid input. The `?` suffix returns `Option` instead.

> r[string.regex]
>
> `.matches(pattern)` tests a regex. `.find_all(pattern)` returns all
> matches. `.captures(pattern)` returns capture groups.
> `.replace_regex(pattern, replacement)` replaces by regex.

> r[string.path-ops]
>
> `.strip_prefix(s)` and `.strip_suffix(s)` return `Option[String]`.

## Evaluation model

### Demand-driven dataflow

> r[eval.demand]
>
> Vixen uses demand-driven dataflow evaluation. The runtime calls `build`
> and needs its return value — that's the demand. Everything flows backward
> from there. There is no separate planning phase.

### Concurrency

> r[eval.concurrency]
>
> There are no concurrency primitives. No `spawn`, no `async`, no `await`,
> no channels. Concurrency comes from the structure of expressions — any
> expression with multiple independent sub-expressions evaluates them
> concurrently. Function arguments, array elements, struct fields, tuple
> elements, and map values all evaluate concurrently when independent.

### Pending values

> r[eval.pending]
>
> A value is either resolved or pending. Any value of any type can be
> pending. A struct can have some fields resolved and some pending.
> Pending values can be freely passed to functions, stored in fields, put
> in arrays, and returned. None of these operations wait.

### Forcing

> r[eval.forcing]
>
> Inside the orchestrator, **nothing forces a pending value**. No
> language construct waits. `a + b` where `a` is pending produces a
> pending result. `if cond { x } else { y }` where `cond` is pending
> produces a pending value. `match`, `is`, indexing, `as`, overload
> dispatch — all build nodes in the dataflow graph. None of them block.
> The language never waits.

> r[eval.forcing.boundary]
>
> Values are forced only when they cross a **boundary** between
> environments:
>
> 1. **Orchestrator → Executor** — when calling `exec`, the arguments,
>    mounts, and serialized observer must be fully resolved before they
>    can be shipped to the executor process. This is a serialization
>    boundary.
> 2. **Orchestrator → External caller** — the external environment (CLI,
>    API, or other driver) demands the final result. This is where the
>    output tree must be fully materialized.
>
> These are the only two forcing points. They are both environment
> boundaries — the moment data leaves the orchestrator. Everything
> inside the orchestrator is lazy dataflow. The runtime resolves the
> graph as dependencies become available, but no language-level
> operation ever blocks waiting for a value.

### The evaluator enters functions

> r[eval.enter-functions]
>
> The evaluator does not treat functions as opaque nodes. It enters
> function bodies, discovers their dataflow graph, and schedules internal
> computations the same way it schedules top-level ones. Demand propagates
> all the way down.

### Demand depth limit

> r[eval.demand-depth]
>
> The evaluator tracks the depth of the demand chain. If it exceeds a
> maximum depth, the entire branch is poisoned with a circular dependency
> error. This prevents infinite recursion and runaway demand loops.

### Memoization

> r[eval.memoization]
>
> The runtime memoizes function calls transparently. The cache key is
> **(function, arguments)**. Because all values are hashable and
> comparable, any function call can be memoized. This is what makes
> incremental builds work.
>
> `exec` cache keys additionally include the runner fingerprint:
> **(args, env, mounts, observer + captures, runner fingerprint)**.
> Runner-contributed inputs (toolchain versions) are fingerprinted and
> tracked.

### Mutable resources

> r[eval.ttl]
>
> `@ttl("duration")` marks functions that access mutable resources. The
> runtime caches results provisionally and revalidates according to the
> policy. If the revalidated result equals the cached result, downstream
> nodes are not re-evaluated (early cutoff).

## Demand model

> r[eval.entry]
>
> A Vixen build starts with a `build.vixen` file at the repository root.
> The entry point receives the working directory as a `Tree`, CLI
> arguments as `Array[String]`, and the environment as
> `Map[String, String]`, and returns a `Tree`.

> r[eval.entry.cargo-compat]
>
> If no `build.vixen` exists and a `Cargo.toml` is found, Vixen evaluates
> as if a thin build file were present calling `std.rust.workspace`. This
> makes `vx build` a drop-in replacement for `cargo build`.

## Acquire

> r[exec.acquire]
>
> `acquire(url, sha256: hash)` fetches a resource by URL. Cached forever
> when a hash is provided.
