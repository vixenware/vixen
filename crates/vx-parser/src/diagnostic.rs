//! Structured parser diagnostics.
//!
//! [`ParseDiagnostic`] is the typed diagnostic produced by the Vixen parser.
//! Each variant carries the structural data needed to render a user-facing
//! message — spans pinpoint exact source locations, and enums classify what
//! kind of construct was expected. No preformatted strings live inside
//! diagnostic variants.
//!
//! Rendering to a user-facing report happens at the consumer boundary (LSP,
//! CLI). [`ParseDiagnostic::to_report`] (in [`crate::diagnostic_report`])
//! converts a diagnostic into a renderable [`margin::Report`]; consumers
//! may also pattern-match on variants directly for structured handling.

use crate::SourceSpan;
use crate::SyntaxKind;

/// A diagnostic produced by the Vixen parser.
///
/// All variants carry only structural data — no preformatted strings.
/// Spans pinpoint exact source locations; the [`ExpectedKind`] /
/// [`LegacySyntaxKind`] enums classify the kind of construct involved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseDiagnostic {
    /// We expected some specific syntactic construct, found something else
    /// (or EOF).
    Expected {
        /// What construct we expected.
        what: ExpectedKind,
        /// The source location of the offending token (or EOF position).
        at: SourceSpan,
    },
    /// Old syntax that's no longer supported.
    LegacySyntax {
        /// Which legacy construct was used.
        kind: LegacySyntaxKind,
        /// The source location of the offending token.
        at: SourceSpan,
    },
}

/// Classification of what the parser expected at a given position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpectedKind {
    /// A specific token kind.
    Token(SyntaxKind),
    /// A specific token OR a semicolon (used for parameter-list-style recovery).
    TokenOrSemicolon(SyntaxKind),
    /// A top-level (or namespace-body) declaration.
    Declaration,
    /// The key part of an `#[key = value]` attribute entry.
    AttributeKey,
    /// A type name (the identifier after `struct`/`enum`/`newtype`/`type`).
    TypeName,
    /// An inline type declaration (`struct`/`enum`/`newtype`/`type` keyword).
    InlineTypeDeclaration,
    /// A field name inside a struct body.
    FieldName,
    /// A variant name inside an enum body.
    VariantName,
    /// A keyword-parameter name inside a `where { ... }` block (or after `;`).
    KeywordParameterName,
    /// A type parameter inside `[T, ...]`.
    TypeParameter,
    /// A type-member identifier after a `.` in a type path.
    TypeMember,
    /// A binding pattern (for `let`, function parameters, etc.).
    BindingPattern,
    /// A field inside a struct pattern (`{ ... }`).
    StructPatternField,
    /// A bare path literal (e.g. `@bin/lua`) or a parenthesized path literal
    /// (`@(...)`).
    BarePathOrPathLiteral,
    /// The name part of a shorthand field (`:name`).
    ShorthandFieldName,
    /// A field name inside a record literal (`{ key: value, ... }`).
    RecordFieldName,
    /// A field name inside a struct literal (`Name { field: value, ... }`).
    StructFieldName,
    /// A field path (e.g. `a.b.c`) inside an update body.
    FieldPath,
    /// A symbol literal name (after `:`).
    SymbolName,
    /// A generic identifier (e.g. as a function name).
    Identifier,
    /// A string literal (e.g. inside `import("...")`).
    StringLiteral,
    /// A type expression (after `|`, etc).
    Type,
    /// An expression (atom).
    Expression,
    /// A pattern (atom).
    Pattern,
    /// Content inside an interpolated string (`...` with `{...}` parts).
    InterpolatedStringContent,
}

/// A specific deprecated syntax that's no longer accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacySyntaxKind {
    /// The old `@key(...)` attribute syntax. Use `#[...]` now.
    AtAttribute,
}

impl ParseDiagnostic {
    /// The primary source span associated with this diagnostic.
    pub fn primary_span(&self) -> SourceSpan {
        match self {
            ParseDiagnostic::Expected { at, .. } => *at,
            ParseDiagnostic::LegacySyntax { at, .. } => *at,
        }
    }
}
