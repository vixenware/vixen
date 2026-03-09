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
> Shape = enum { ... }
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
> not an attribute. `@` in expression position (`@f(args, ..)`) is
> partial application, not an attribute. The parser distinguishes these
> by syntactic context.

### Semicolons

> r[lex.semicolons]
>
> Semicolons separate statements. This applies uniformly — at module
> top level and inside blocks. A trailing semicolon after the last
> statement is permitted but not required. In blocks, the last
> expression (without a trailing semicolon) is the block's value; a
> trailing semicolon turns it into a statement and the block evaluates
> to `()`. A source file is a sequence of semicolon-separated
> statements; there is no trailing expression value at file level.

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
> Other: `?` (fallible suffix), `@` (pattern binding, attributes), `#` (capability
> tags on function types), `:` (named slots — type annotations and field-value
> pairs, always after an identifier), `::` (expression-level type ascription,
> distinct from `as` conversion).

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
> | Precedence | Operators              | Associativity | Description                 |
> |------------|------------------------|---------------|-----------------------------|
> | 1          | `with`                 | none          | Struct update               |
> | 2          | `\|\|`                 | left          | Logical OR                  |
> | 3          | `&&`                   | left          | Logical AND                 |
> | 4          | `==` `!=`              | left          | Equality                    |
> | 5          | `<` `<=` `>` `>=`      | left          | Comparison                  |
> | 6          | `..` `..=`             | none          | Range                       |
> | 7          | `+` `-`                | left          | Additive                    |
> | 8          | `*` `/` `%`            | left          | Multiplicative              |
> | 9          | unary `-` `!`          | prefix        | Negation, logical NOT       |
> | 10         | `as` `as?` `is` `::`   | left          | Type cast / test / ascription |
> | 11         | `?`                    | postfix       | Fallible                    |
> | 12         | `.`                    | left          | Field access / UFCS         |
> | 13         | `()` `[]`              | postfix       | Call, index                 |
>
> Assignment (`=`) is a statement, not an expression. `@` in
> `@f(args, ..)` is a prefix modifier on call expressions, not a
> standalone operator — it binds at the call level. `|` is not an
> expression operator; it appears in type declarations (sum shorthand)
> and in patterns (or-patterns).

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

> r[decl.struct.defaults]
>
> Fields can have default values:
>
> ```vixen
> Config = struct {
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
> The `?` suffix catches poisoning and returns `Option` instead:
> `Some(result)` on success, `None` on failure. It applies to any
> expression that may poison — function calls (`f(x)?`), UFCS calls
> (`x.f()?`), and index expressions (`array[i]?`). The `as?` operator
> follows the same pattern (see [type.as-fallible]).

### Statements

> r[expr.statement]
>
> Assignment (`name = expr`, `name: Type = expr`) and compound update
> (`name with= { ... }`) are statements, not expressions. Declarations
> (`fn`, `struct`, `enum`, `newtype`) are also statements.

## Modules

> r[lang.modules]
>
> A `.vixen` file is a module. Its top-level bindings (including types)
> are the module's exports. There is no visibility modifier — everything
> at the top level is visible to importers.

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
> Function types use `Fn(Args) -> Ret` syntax. Capability tags appear
> between the parameter list and the return type arrow: `Fn(Args) #fs -> Ret`.
> See [cap.model]. Only functions with positional arguments can have their
> type named. Functions with keyword arguments cannot be passed as values —
> use partial application (`@`) to close over keyword arguments and produce
> a passable `Fn`.

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
> types of the arguments at the call site. An exact type match always
> wins over a match via widening.

> r[fn.overload.no-sum-dispatch]
>
> Overload resolution does not synthesize dispatch over enum variants.
> If `fn digest(x: Blob)` and `fn digest(x: Tree)` both exist, calling
> `digest(entry)` where `entry: Entry` (and `Entry = Blob | Tree`) is
> an error — there is no overload for `Entry`. Write an explicit `match`
> or add `fn digest(x: Entry)`.

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

> r[comb.model.ordering]
>
> Because combinators always produce `Stream`, calling `.map(f)` on an
> `Array` loses input order. The elements are processed concurrently
> and the resulting stream is unordered. To recover a deterministic
> array, call `.to_array()`, which sorts by universal total ordering —
> not by the original input position. This means
> `[3, 1, 2].map($ * 2).to_array()` returns `[2, 4, 6]`, not
> `[6, 2, 4]`. This is a deliberate trade-off: concurrent fan-out and
> canonical determinism over positional preservation.

> r[comb.map]
>
> `.map(f)` applies `f` to each element and returns a `Stream` of results.

> r[comb.filter]
>
> `.filter(pred)` returns a `Stream` of elements where `pred` is true.

> r[comb.find]
>
> `.find(pred)` returns the first element to resolve for which `pred`
> is true. The result is `Option[T]`. If at most one element can satisfy
> `pred`, the result is deterministic. Nondeterminism arises only when
> multiple elements may satisfy `pred`, because any of them may resolve
> first. `None` is returned only if the stream completes with no
> matching element.

> r[comb.fold]
>
> `.fold(init, f)` reduces all elements to a single value using an
> accumulator. Only available on `Array`, not on `Stream` — fold depends
> on element order, which streams do not have.

> r[comb.any]
>
> `.any(pred)` returns `true` if any element satisfies the predicate.

> r[comb.all]
>
> `.all(pred)` returns `true` if all elements satisfy the predicate.

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
> accumulator. Only available on `Array`, not on `Stream` — use
> `.to_array()` first.

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

> r[comb.collect.ordering]
>
> Arrays produced by stream collection operations are canonically
> ordered using Vixen's universal total ordering. This applies to
> `.to_array()`, group arrays in `.group_by()`, and both arrays
> returned by `.partition()`.

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
> function calls, UFCS calls, and index expressions. `f(x)?` returns
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

> r[store.blob.to-string]
>
> `blob.to_string()` returns the blob's content interpreted as a UTF-8
> string. Poisons if the content is not valid UTF-8.

> r[store.tree]
>
> `Tree` is a directory with its full structure and filenames preserved.
> There is no separate "local tree" vs "store tree" type — the runtime
> handles materialization transparently.
>
> `Tree.of(map)` creates a tree from a map of names to entries.
> `Tree.empty()` is shorthand for `Tree.of(%{})`.
> `.subtree(path)` selects a subtree. `.glob(pattern)` filters by glob.
> `.merge(other)` merges two trees (later entries win). These operations
> are lazy.

> r[store.tree.file]
>
> `tree.file(path)` returns the `Blob` at the given relative path
> within the tree. Poisons if the path does not exist or points to a
> directory. The fallible version `tree.file(path)?` returns
> `Option[Blob]`.

> r[store.path]
>
> `Path` is its own type — not a newtype of `String`. Root paths come
> from `.at()` on store types. Sub-paths are built with the `/` operator.
> Paths carry their store references — they remember which `Tree` or
> `Blob` they point into and what sub-path within it.
>
> In `exec`, Paths in `args` and `env` are resolved to VFS locations
> using the mount table. The runtime finds the mount that covers the
> Path's store reference and constructs the VFS path (mount point +
> sub-path). If no mount covers the referenced content, the evaluator
> reports an error. See [exec.mount].
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
>   {
>     env: Map[String, ExecArg] = %{},
>     mounts: Map[Path, Blob | Tree] = %{},
>     writable: Array[Path] = [],
>     executor: ExecutorClass = executor_class("default"),
>     verifier: Fn() -> () = fn() {},
>     observer: Fn(Process, Tree) -> Stream[T],
>   }
> ) -> Stream[T]
> ```

> r[exec.arg]
>
> `ExecArg = String | Path`. A plain `String` is a literal argument. A
> `Path` carries a store reference — the runtime ensures the referenced
> content is available in the VFS.

> r[exec.mount]
>
> Mounts map VFS paths (`Path`) to store content (`Blob` or `Tree`).
> The content is available at the specified path in the sandbox
> filesystem. All mount paths are relative. Mounts are read-only by
> default.
>
> Every `Path` that appears in `args` or `env` must be covered by a
> mount — the runtime resolves each `Path` to its VFS location by
> finding the mount whose content matches the Path's store reference,
> then joining the mount point with the Path's sub-path. If no mount
> covers a referenced `Path`, the evaluator reports an error. This
> makes the VFS layout explicit — nothing is silently materialized.

> r[exec.writable]
>
> `writable` lists VFS paths (`Path`) where the process is allowed to
> write. These are empty directories created before the process starts.
> The process can create files and subdirectories within them. The
> observer can read written content from the VFS tree after the process
> runs. Writable paths are scratch space — they have nothing to do
> with store types.

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

> r[exec.verifier]
>
> The `verifier` parameter is an optional closure that runs on the
> executor in parallel with the actual process. It checks that the
> host environment matches what was discovered — hashing binaries,
> checking paths, verifying versions. If the verifier poisons, the
> `exec` result poisons with a **verification failure**, a distinct
> poison kind that signals the executor's environment has drifted from
> the recorded discovery.
>
> Verification does not gate execution. The process runs speculatively
> while verification proceeds concurrently. If verification fails after
> the process completes, the result is discarded and the exec poisons.

> r[exec.verifier.compose]
>
> `exec` accepts one verifier. If multiple environment properties must
> be checked, compose them into a single closure:
>
> ```vixen
> verifier = fn() {
>   check_gcc();
>   check_ld();
>   check_sdk_version();
> }
>
> exec(["gcc", "-c", "main.c"], verifier: verifier, ...)
> ```

> r[exec.executor-class]
>
> `executor_class(label: String) -> ExecutorClass` creates a value
> representing a class of executor environments (e.g.,
> `executor_class("ubuntu-22.04")`). The class is the unit of cache
> reuse for `exec` — cached results are keyed by executor class, not
> by specific runner instance. Different instances of the same class
> are expected to be equivalent; the verifier detects when they are
> not.

## Executor discovery

> r[exec.discover]
>
> Some toolchains cannot be acquired through `acquire` — they are
> non-redistributable (MSVC, Xcode) or must be found on the executor
> host. `discover_exec` probes an executor without sandboxing. It runs
> a command directly on the host and returns its output. It requires
> the `#discover` capability and must run inside a `stabilize` resolve
> callback.
>
> ```vixen
> fn discover_exec(
>   class: ExecutorClass,
>   args: Array[String],
>   { env: Map[String, String] = %{} },
> ) -> DiscoveryProcess;
> ```
>
> `DiscoveryProcess` has `stdout: Blob`, `stderr: Blob`, and
> `exit_code: U64`. There is no VFS, no mounts, no observer — the
> process sees the executor's real host filesystem.

> r[exec.discover.verifier]
>
> Discovery results are opaque to the build graph. The build does not
> branch on what version of MSVC was found — it just runs `cl.exe`.
> What discovery produces is a **verifier**: a closure that re-checks
> the same facts on the concrete executor instance running each `exec`.
>
> The verifier is constructed from discovery output by library or user
> code and passed to `exec` via the `verifier` parameter. The runtime
> does not inspect or synthesize verifiers — they are ordinary closures.

> r[exec.discover.reuse]
>
> Discovery results are cached by executor class through `stabilize`.
> The witness reads local project state (what toolchain is requested);
> the resolve calls `discover_exec` to learn what is actually present.
> The stabilized result — including the verifier — is reused across
> builds as long as the witness is unchanged.
>
> Per-instance verification happens at every `exec`: the verifier runs
> on the actual runner that executes the work. If verification fails
> (the runner drifted from what was discovered), the exec poisons with
> a verification failure. The cached discovery for that class is
> invalidated and rediscovery runs on retry.

> r[exec.discover.retry]
>
> A verification failure poison carries structured evidence: what was
> expected, what was observed. This distinguishes it from other poison
> kinds. The runtime or caller may retry by rerunning the `stabilize`
> witness/resolve cycle to update the cached discovery, then retrying
> the exec with the new verifier.

## Deserialization

> r[deser.parse]
>
> Vixen has built-in parsers for structured data formats. Parsing is
> driven by type — the target type's shape determines how the input is
> deserialized. Field names map to keys, `Option` fields are optional,
> sum types handle polymorphic values.

> r[deser.formats]
>
> Built-in formats:
>
> - `Toml.parse[T](blob)` — TOML.
> - `Json.parse[T](blob)` — JSON.
> - `Yaml.parse[T](blob)` — YAML.
>
> All take a `Blob` and return a value of type `T`. Parsing poisons on
> malformed input or type mismatches.

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

### Control flow demand propagation

> r[eval.control-demand]
>
> `if`, `match`, and `is` do not force pending values. They propagate
> demand to the scrutinee/condition. Demand propagates to a branch only
> after that branch is selected by the resolved scrutinee. Branches not
> taken receive no demand — no work is done for dead branches.

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
> The evaluator tracks the depth of the demand chain. Two distinct
> failure modes exist:
>
> - **Circular dependency** — the evaluator detects that a value
>   transitively demands itself. This is a logic error in the program
>   and poisons with a circular dependency diagnostic.
> - **Depth exhaustion** — the demand chain exceeds a maximum depth
>   without cycling. This is a resource limit and poisons with a
>   recursion depth diagnostic. The limit is configurable.

### Memoization

> r[eval.memoization]
>
> The runtime memoizes function calls transparently. The cache key is
> **(code identity, arguments)** — the function's transitive code
> identity (see [fn.identity]) plus its resolved argument values.
> Because all values are hashable and comparable, any function call can
> be memoized. This is what makes incremental builds work — and because
> code identity is stable across comment/formatting edits, the cache
> survives source changes that do not affect behavior.
>
> `exec` cache keys are **(observer code identity, observer captures,
> args, env, mounts, executor class)**. The executor class is the unit
> of cache reuse — two `exec` calls with the same inputs and the same
> executor class may share a cached result. Because the observer's code
> identity is transitive, changing a helper function that the observer
> calls invalidates the `exec` cache — even though the observer's own
> AST did not change.

> r[eval.memoization.pending]
>
> Only fully resolved values participate in memoization. Pending values
> are not hashable for cache-key purposes and do not participate in
> memo table lookup. A function call is memoized only once all its
> arguments have resolved.

## Demand model

> r[eval.entry]
>
> A Vixen build starts with a `build.vixen` file at the repository root.
> The entry point is invoked by the `build` intrinsic, which grants the
> `#fs` capability. The entry point receives the workspace path as a
> `Path`, CLI arguments as `Array[String]`, and the environment as
> `Map[String, String]`, and returns a `Tree`.
>
> ```vixen
> fn build[T](
>   entry: Fn(Path, Array[String], Map[String, String]) #fs -> T,
> ) -> T;
> ```
>
> The workspace is not pre-walked into a `Tree` — filesystem access is
> lazy. Files are read on demand through the `#fs` capability, and those
> reads are tracked by the runtime's dependency tracking machinery.

> r[eval.entry.cargo-compat]
>
> If no `build.vixen` exists and a `Cargo.toml` is found, Vixen evaluates
> as if a thin build file were present calling `std.rust.workspace`. This
> makes `vx build` a drop-in replacement for `cargo build`.

## Acquire

> r[exec.acquire]
>
> `acquire(url, sha256: hash)` fetches a resource by URL and returns a
> `Blob`. The hash is always required — the result is content-addressed
> and cached permanently. The hash is verified; a mismatch poisons.
> Because acquisition is deterministic (same URL + same hash = same
> content), `acquire` is ambient — it requires no capability and can be
> called anywhere. Fetching without a known hash is discovery, which
> goes through `stabilize` (see [stabilize.model]).

> r[exec.acquire.extract]
>
> `acquire(url, sha256: hash, extract: format)` fetches a URL and
> extracts the archive in one streaming step, returning a `Tree`.
> Supported formats: `"tar.gz"`, `"tar.xz"`, `"tar.zst"`, `"zip"`.
> Extraction streams directly to the content-addressable store — the
> `Tree` is pending until extraction completes, and the entire
> operation (fetch + extract) is a single cached unit keyed by URL and
> hash.

## Capabilities

> r[cap.model]
>
> Every closure and function has a **capability fingerprint** — the set
> of capabilities it can exercise, inferred by the evaluator from the
> operations it performs and the authority-bearing values it captures.
> A function has zero capabilities by default. Capabilities are acquired
> by capturing authority-bearing values from a scope where a capability
> is active, or by being invoked through an intrinsic that grants a
> capability.

> r[cap.tags]
>
> Capability tags on `Fn` types use `#name` syntax between the parameter
> list and the return type arrow:
>
> ```vixen
> Fn() #fs -> W
> Fn(W) #discover -> T
> Fn(T) #fs #discover -> U
> ```
>
> Tags are valid only in function type and function signature position.
> `#` is not an expression operator.

> r[cap.mask]
>
> Capability tags on a `Fn` type are an **upper bound** (a mask), not
> an exact requirement. A closure's inferred capability fingerprint must
> be a **subset** of the tagged set. A closure with no capabilities
> satisfies any mask. A closure with `#fs #discover` fails a mask that
> only allows `#fs`.
>
> A `Fn` type with no capability tags accepts only closures with an
> empty fingerprint — pure functions.

> r[cap.kinds]
>
> Two capabilities exist:
>
> - **`#fs`** — filesystem access. Reading files, walking directories,
>   evaluating globs. Operates on local/project state that is cheap to
>   check between builds.
> - **`#discover`** — volatile external state. Registry queries,
>   toolchain channel resolution, latest-version lookups. Operates on
>   state that may differ between builds and is expensive to verify.

> r[cap.propagation]
>
> Capability fingerprints propagate through closures: if a closure
> captures an authority-bearing value (a handle that can exercise a
> capability), the closure carries that capability in its fingerprint.
> Calling a function that requires a capability also propagates that
> capability to the caller.
>
> Propagation is about **authority**, not data provenance. A `String`
> computed inside a `#discover` context is an ordinary `String` — capturing
> it does not make another closure carry `#discover`. Only values that
> can actually exercise authority (perform filesystem operations, issue
> discovery requests) propagate capabilities.

> r[cap.grant]
>
> User code can mention capability tags in function signatures — this
> restricts what callbacks are accepted (masking). User code **cannot
> implement** functions that grant capabilities. Only runtime intrinsics
> can invoke a callback in a capability context the caller did not
> already have.
>
> This means `build` (which grants `#fs`) and `stabilize` (which grants
> `#discover`) have signatures expressible in Vixen but bodies that only
> the runtime can provide. The evaluator rejects user code that attempts
> to implement a capability-granting function.

## Stabilize

> r[stabilize.model]
>
> Some operations consult changing external state — resolving a toolchain
> channel, querying a registry, performing dependency resolution. These
> are not reproducible by themselves. `stabilize` bridges discovery and
> reproducible reuse by splitting such computations into two callbacks
> with different capability masks: a **witness** that captures the
> dependency frontier, and a **resolve** that performs discovery.

> r[stabilize.signature]
>
> ```vixen
> fn stabilize[W, T](
>   witness: Fn() #fs -> W,
>   resolve: Fn(W) #discover -> T,
> ) -> T;
> ```
>
> The witness callback runs under `#fs` — it can read the filesystem but
> cannot perform discovery. The resolve callback runs under `#discover` —
> it can query external state but cannot access the filesystem. All
> local/project state the resolve callback needs must be summarized in
> the witness value it receives.

> r[stabilize.witness]
>
> The witness callback computes the dependency frontier — the set of
> local/project state that determines whether a previous resolution is
> still valid. It reads manifests, walks directories, checks globs,
> and returns a value that summarizes everything relevant. Its
> dependencies are tracked by the same holistic machinery as any other
> Vixen computation: code identity, captures, transitive calls, and all
> values read. No special tracking rules are introduced for the witness.

> r[stabilize.resolve]
>
> The resolve callback performs discovery. It receives the witness value
> and computes the final result — querying registries, resolving
> dependency graphs, looking up versions. It cannot access the
> filesystem; the `#discover` mask excludes `#fs`. This prevents
> untracked filesystem reads from causing over-caching.

> r[stabilize.reuse]
>
> The witness value is the reuse key. If the current witness matches
> the recorded witness from a previous build, the previous resolve
> result is reused without running resolve again. If the witness
> changes, resolve runs again. The witness execution trace — not a
> user-supplied key — determines reuse.

> r[stabilize.caching-bias]
>
> Over-caching — reusing a stale result — must not happen. Under-caching
> — recomputing when reuse was safe — is acceptable. The capability
> split ensures correctness: the witness captures everything that
> determines validity, and the resolve callback cannot introduce
> untracked inputs.

> r[stabilize.lockfile]
>
> Lockfiles are ordinary artifacts. They are outputs of `stabilize` and
> later inputs to reproducible builds. Evaluation never mutates
> workspace files in place — a lockfile artifact is written to disk
> only at the external boundary, like any other output.
>
> "Normal build" vs "update lockfile" is not a semantic distinction.
> The evaluator always computes the correct stabilized result. The
> difference is whether the resulting artifact gets written back to the
> repository.

> r[stabilize.compat]
>
> Ecosystem lockfiles (e.g. `Cargo.lock`) retain their native format —
> `stabilize` does not own any particular lockfile syntax. Library code
> provides package-manager-specific parsing and writing logic. Vixen
> keeps its own reuse metadata (witness fingerprints, dependency traces)
> separately.

> r[stabilize.first-build]
>
> On first evaluation of an existing project, `stabilize` runs the full
> witness/resolve cycle and records its own reuse data. Any existing
> ecosystem lockfile (e.g. a `Cargo.lock` written by `cargo`) may be
> consumed by the resolve callback as an ordinary input, according to
> that callback's logic.

> r[stabilize.nested]
>
> `stabilize` can appear anywhere in the build graph, including inside
> a resolve callback. Nesting is safe because each `stabilize` call
> enforces its own capability masks independently:
>
> ```vixen
> stabilize(
>   witness: fn() { ... },
>   resolve: fn(inputs) {
>     // outer resolve has #discover, not #fs
>     stabilize(
>       witness: fn() { ... },
>       // inner witness: #fs granted by inner stabilize,
>       // #discover from outer resolve filtered by mask
>       resolve: fn(inner) { ... },
>       // inner resolve: #discover granted by inner stabilize
>     )
>   },
> )
> ```
