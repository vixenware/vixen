//! Conversion from [`ParseDiagnostic`] to [`margin::Report`].
//!
//! This is the last-mile conversion from the structured, string-free
//! [`ParseDiagnostic`] enum into the rich, renderable [`margin::Report`]
//! shape. Strings appear here for the first time — sourced from the
//! diagnostic variant's structural data (kind labels, expected-construct
//! prose, etc.). Nothing inside a [`ParseDiagnostic`] is text.
//!
//! Adding a new diagnostic variant: extend the `match` in
//! [`ParseDiagnostic::to_report`] below.

use margin::{Annotation, AnnotationRole, Report, Severity, Span};

use crate::diagnostic::{ExpectedKind, LegacySyntaxKind, ParseDiagnostic};
use crate::source::SourceSpan;
use crate::syntax_kind::SyntaxKind;

impl ParseDiagnostic {
    /// Render this diagnostic as a [`margin::Report`].
    ///
    /// All parser diagnostics currently produce [`Severity::Error`].
    pub fn to_report(&self) -> Report {
        match self {
            ParseDiagnostic::Expected { what, at } => Report {
                severity: Severity::Error,
                title: expected_title(what),
                annotations: vec![primary(*at, Some(expected_message(what)))],
                notes: Vec::new(),
                sections: Vec::new(),
            },
            ParseDiagnostic::LegacySyntax { kind, at } => Report {
                severity: Severity::Error,
                title: "deprecated syntax".to_string(),
                annotations: vec![primary(*at, Some(legacy_message(*kind)))],
                notes: Vec::new(),
                sections: Vec::new(),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Annotation builders
// ---------------------------------------------------------------------------

fn primary(span: SourceSpan, message: Option<String>) -> Annotation {
    Annotation {
        spans: vec![to_margin_span(span)],
        role: AnnotationRole::PrimaryLabel,
        syntax_class: None,
        message,
        priority: 100,
    }
}

fn to_margin_span(s: SourceSpan) -> Span {
    Span {
        source_id: margin::SourceId(s.source.0.to_string()),
        start: s.start() as usize,
        end: s.end() as usize,
    }
}

// ---------------------------------------------------------------------------
// Variant-specific labels and messages
// ---------------------------------------------------------------------------

fn expected_title(what: &ExpectedKind) -> String {
    match what {
        ExpectedKind::Token(kind) => format!("parse error: expected `{}`", token_display(*kind)),
        ExpectedKind::TokenOrSemicolon(kind) => {
            format!("parse error: expected `{}` or `;`", token_display(*kind))
        }
        _ => format!("parse error: {}", short_expected_label(what)),
    }
}

fn expected_message(what: &ExpectedKind) -> String {
    match what {
        ExpectedKind::Token(kind) => format!("expected `{}` token", token_display(*kind)),
        ExpectedKind::TokenOrSemicolon(kind) => {
            format!("expected `{}` or `;`", token_display(*kind))
        }
        ExpectedKind::Declaration => "expected a declaration".to_string(),
        ExpectedKind::AttributeKey => "expected an attribute key".to_string(),
        ExpectedKind::TypeName => "expected a type name".to_string(),
        ExpectedKind::InlineTypeDeclaration => "expected an inline type declaration".to_string(),
        ExpectedKind::FieldName => "expected a field name".to_string(),
        ExpectedKind::VariantName => "expected a variant name".to_string(),
        ExpectedKind::KeywordParameterName => "expected a keyword parameter name".to_string(),
        ExpectedKind::TypeParameter => "expected a type parameter".to_string(),
        ExpectedKind::TypeMember => "expected a type member".to_string(),
        ExpectedKind::BindingPattern => "expected a binding pattern".to_string(),
        ExpectedKind::StructPatternField => "expected a struct pattern field".to_string(),
        ExpectedKind::ShorthandFieldName => "expected a shorthand field name".to_string(),
        ExpectedKind::RecordFieldName => "expected a record field name".to_string(),
        ExpectedKind::StructFieldName => "expected a struct field name".to_string(),
        ExpectedKind::FieldPath => "expected a field path".to_string(),
        ExpectedKind::SymbolName => "expected a symbol name".to_string(),
        ExpectedKind::Identifier => "expected an identifier".to_string(),
        ExpectedKind::StringLiteral => "expected a string literal".to_string(),
        ExpectedKind::Type => "expected a type".to_string(),
        ExpectedKind::Expression => "expected an expression".to_string(),
        ExpectedKind::Pattern => "expected a pattern".to_string(),
        ExpectedKind::InterpolatedStringContent => {
            "expected interpolated string content".to_string()
        }
    }
}

fn short_expected_label(what: &ExpectedKind) -> &'static str {
    match what {
        ExpectedKind::Token(_) | ExpectedKind::TokenOrSemicolon(_) => unreachable!(),
        ExpectedKind::Declaration => "expected declaration",
        ExpectedKind::AttributeKey => "expected attribute key",
        ExpectedKind::TypeName => "expected type name",
        ExpectedKind::InlineTypeDeclaration => "expected inline type declaration",
        ExpectedKind::FieldName => "expected field name",
        ExpectedKind::VariantName => "expected variant name",
        ExpectedKind::KeywordParameterName => "expected keyword parameter name",
        ExpectedKind::TypeParameter => "expected type parameter",
        ExpectedKind::TypeMember => "expected type member",
        ExpectedKind::BindingPattern => "expected binding pattern",
        ExpectedKind::StructPatternField => "expected struct pattern field",
        ExpectedKind::ShorthandFieldName => "expected shorthand field name",
        ExpectedKind::RecordFieldName => "expected record field name",
        ExpectedKind::StructFieldName => "expected struct field name",
        ExpectedKind::FieldPath => "expected field path",
        ExpectedKind::SymbolName => "expected symbol name",
        ExpectedKind::Identifier => "expected identifier",
        ExpectedKind::StringLiteral => "expected string literal",
        ExpectedKind::Type => "expected type",
        ExpectedKind::Expression => "expected expression",
        ExpectedKind::Pattern => "expected pattern",
        ExpectedKind::InterpolatedStringContent => "expected interpolated string content",
    }
}

fn legacy_message(kind: LegacySyntaxKind) -> String {
    match kind {
        LegacySyntaxKind::AtAttribute => "use `#[...]` attribute syntax instead".to_string(),
    }
}

/// Source-level token label for a [`SyntaxKind`].
///
/// This is the small Display-like helper for parser diagnostics. It's
/// intentionally local to the diagnostic-report module — the public
/// [`SyntaxKind`] type intentionally has no `Display`, because most kinds
/// don't have a single canonical surface text (e.g. node kinds like
/// `SOURCE_FILE`). Here we only need labels for the punctuation/keyword
/// tokens that actually appear in `Expected::Token` diagnostics, plus a
/// debug fallback for the rest.
pub(crate) fn token_display(kind: SyntaxKind) -> String {
    use SyntaxKind::*;
    let label = match kind {
        SEMICOLON => ";",
        COMMA => ",",
        COLON => ":",
        COLON2 => "::",
        DOT => ".",
        DOT2 => "..",
        DOT2_EQ => "..=",
        EQ => "=",
        EQ2 => "==",
        BANG => "!",
        BANG_EQ => "!=",
        LT => "<",
        LT_EQ => "<=",
        GT => ">",
        GT_EQ => ">=",
        PLUS => "+",
        PLUS2 => "++",
        MINUS => "-",
        STAR => "*",
        SLASH => "/",
        PERCENT => "%",
        PERCENT_L_CURLY => "%{",
        AMP2 => "&&",
        PIPE => "|",
        PIPE_GT => "|>",
        PIPE2 => "||",
        FAT_ARROW => "=>",
        THIN_ARROW => "->",
        AT => "@",
        HASH => "#",
        QUESTION => "?",
        WITH_EQ => "with=",
        L_PAREN => "(",
        R_PAREN => ")",
        L_BRACKET => "[",
        R_BRACKET => "]",
        L_CURLY => "{",
        R_CURLY => "}",
        UNDERSCORE => "_",
        BACKTICK => "`",
        FN_KW => "fn",
        LET_KW => "let",
        TYPE_KW => "type",
        NAMESPACE_KW => "namespace",
        STRUCT_KW => "struct",
        ENUM_KW => "enum",
        NEWTYPE_KW => "newtype",
        OF_KW => "of",
        MATCH_KW => "match",
        IF_KW => "if",
        ELSE_KW => "else",
        WITH_KW => "with",
        WHERE_KW => "where",
        IS_KW => "is",
        AS_KW => "as",
        IMPORT_KW => "import",
        RETURN_KW => "return",
        FAIL_KW => "fail",
        PARTIAL_KW => "partial",
        TRUE_KW => "true",
        FALSE_KW => "false",
        FN_TYPE_KW => "Fn",
        LOWER_IDENT => "<identifier>",
        UPPER_IDENT => "<Identifier>",
        DOLLAR_IDENT => "$<identifier>",
        INT => "<int>",
        FLOAT => "<float>",
        STRING => "<string>",
        EOF => "<eof>",
        other => return format!("{:?}", other),
    };
    label.to_string()
}
