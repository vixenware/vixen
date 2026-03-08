# Deserialization

Vixen has built-in deserialization from JSON, TOML, and YAML into typed
structs and enums. This is a core language feature, not a library.

## Basic usage

```vixen
line.json[RustcMessage]()
blob.read_utf8().toml[CargoManifest]()
```

The type parameter tells the deserializer what to produce. If the data
doesn't match the type, deserialization fails with a real error message.

## Option shorthand

`T?` is shorthand for `Option[T]`:

```vixen
// These are equivalent:
version: Option[String]
version: String?
```

Fields with `T?` type are optional during deserialization — a missing key
produces `none`.

## Enum variants

Enum variants can have named fields (struct-style) or positional fields
(tuple-style):

```vixen
enum DepValue {
  Simple(String),                    // tuple-style: wraps a bare value
  Detailed { version: String? },     // struct-style: named fields
}
```

For deserialization, the value kind determines the variant. A TOML/JSON
string matches `Simple`. A table/object matches `Detailed`. This is
unambiguous because a bare string and an object are different value kinds.

This is critical for Cargo.toml parsing, where dependencies can be either:

```toml
serde = "1.0"                           # Simple(String)
serde = { version = "1.0", optional = true }  # Detailed { ... }
```

## Annotations

Annotations control how Vixen types map to wire formats. The type name
and field names are Vixen concepts — annotations define the mapping to
the external format.

### Struct annotations

```vixen
@toml(rename_all: "kebab-case")
struct Package {
  name: String,
  version: String,
  default_features: bool?,  // maps to "default-features" in TOML
}
```

### Tagged enums

Single-field tag:

```vixen
@json(tag: "$message_type", rename_all: "lowercase")
enum RustcMessage {
  Diagnostic { message: String, level: String, rendered: String, spans: List[Span] },
  Artifact { inner: RustcArtifact },
}
```

### Multi-field dispatch with `where`

When the JSON uses multiple fields to discriminate variants, use `where`
instead of `tag`. Each variant specifies exactly which field values identify
it. Variant names are purely Vixen concepts — they don't appear in the JSON.

```vixen
enum RustcMessage {
  @json(where: { "$message_type": "diagnostic" })
  Diagnostic { message: String, level: String, rendered: String, spans: List[Span] },

  @json(where: { "$message_type": "artifact", "emit": "metadata" })
  RMeta { artifact: String },

  @json(where: { "$message_type": "artifact", "emit": "link" })
  RLib { artifact: String },
}
```

This avoids nested enums when the JSON has two levels of tagging.

### Another example: nextest output

```vixen
enum NextestMessage {
  @json(where: { "type": "suite", "event": "started" })
  SuiteStarted { test_count: u64 },

  @json(where: { "type": "test", "event": "started" })
  TestStarted { name: String },

  @json(where: { "type": "test", "event": "ok" })
  TestPassed { name: String, exec_time: f64 },

  @json(where: { "type": "test", "event": "ignored" })
  TestIgnored { name: String },

  @json(where: { "type": "test", "event": "failed" })
  TestFailed { name: String, exec_time: f64, stdout: String },

  @json(where: { "type": "suite", "event": "ok" })
  SuiteFinished { passed: u64, failed: u64, ignored: u64 },
}
```

## Cargo.toml example

Full Cargo manifest parsing using the deserialization system:

```vixen
@toml(rename_all: "kebab-case")
struct CargoManifest {
  package: Package,
  dependencies: Map[String, DepValue]?,
  build_dependencies: Map[String, DepValue]?,
  dev_dependencies: Map[String, DepValue]?,
  features: Map[String, List[String]]?,
  target: Map[String, TargetTable]?,
}

struct Package {
  name: String,
  version: String,
  edition: String?,
}

// Dependencies can be a bare string or a detailed table.
// The value kind (string vs table) determines the variant.
@toml(rename_all: "kebab-case")
enum DepValue {
  Simple(String),
  Detailed {
    version: String?,
    path: String?,
    git: String?,
    branch: String?,
    rev: String?,
    workspace: bool?,
    package: String?,
    features: List[String]?,
    optional: bool?,
    default_features: bool?,
  },
}

// Target-specific dependencies: [target.'cfg(...)'.dependencies]
// The Map key is the cfg expression as a string.
@toml(rename_all: "kebab-case")
struct TargetTable {
  dependencies: Map[String, DepValue]?,
  build_dependencies: Map[String, DepValue]?,
}
```

## Supported annotations

  * `rename_all`: rename strategy for all fields or variants. Values:
    `"lowercase"`, `"UPPERCASE"`, `"camelCase"`, `"PascalCase"`,
    `"snake_case"`, `"SCREAMING_SNAKE_CASE"`, `"kebab-case"`.
  * `rename`: rename a single field or variant.
  * `tag`: the field used to discriminate enum variants (single-field
    dispatch).
  * `where`: multi-field dispatch. Each variant specifies a map of field
    names to expected values.
  * `flatten`: inline a nested struct's fields into the parent.
  * `default`: use the type's default value if the field is missing.
  * `skip`: do not deserialize this field.

## Error behavior

If deserialization fails (unknown variant, missing required field, type
mismatch), the error includes:

  * What was expected (the type and its variants/fields)
  * What was found (the actual JSON/TOML value)
  * Where in the input it happened

No silent skipping. No fallback to a default variant. No dynamic value
types — everything goes through typed deserialization.

## Open questions

  * Do we support `@toml(...)` and `@yaml(...)` as separate annotation
    namespaces, or is one set of annotations shared across all formats?
  * Serialization: do we need it? Observers consume external formats, but
    does Vixen ever need to produce JSON/TOML?
  * f64: is this a Vixen type? The language doc only mentions u64 and i64.
    Needed for nextest's `exec_time` and similar.
  * Workspace inheritance: `workspace = true` means "look up the real value
    in the workspace root." Is this handled by the deserialization system
    or by Vixen code that merges workspace and crate manifests?
