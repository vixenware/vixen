//! Event-based parser for Vixen source files.

mod ast;
pub use ast::{
    Arg, ArgList, ArrayExpr, ArrayItem, ArrayPattern, ArrayRestPattern, AssignStmt, AstChildren,
    AstNode, AtPattern, Attribute, AttributeEntry, AttributeList, BinaryExpr, BindingPattern,
    BlockExpr, CallArgExpr, CallExpr, CallKeywordArg, ClosureExpr, ClosureParamClause,
    CompoundUpdateStmt, ElseClause, EnumDecl, Expr, FailExpr, FallibleExpr, FieldAssignStmt,
    FieldAssignTarget, FieldDecl, FieldExpr, FnDecl, FunctionType, IfExpr, ImportExpr, IndexExpr,
    InterpolatedStringExpr, InvokeExpr, Item, KeywordParameter, LetStmt, LiteralExpr,
    LiteralPattern, MapEntry, MapExpr, MatchArm, MatchExpr, NameRefExpr, NamespaceBody,
    NamespaceDecl, NewtypeDecl, OptionalType, OrPattern, Parameter, ParenPattern, ParenType,
    PartialAppExpr, PathLiteralExpr, Pattern, PrefixExpr, RangeExpr, RecordExpr, ReturnStmt,
    ReturnType, SlotType, SourceFile, SpreadExpr, Stmt, StringInterpolation, StringPart,
    StructDecl, StructLiteralExpr, StructLiteralField, StructPattern, StructPatternField, SumType,
    TupleExpr, TuplePattern, TupleType, Type, TypeAliasDecl, TypeArgs, TypeOpExpr, TypePath,
    UnitPattern, VariantDecl, VariantPattern, WhereExpr, WildcardPattern, WithExpr, doc_comment,
};

mod fmt;
pub use fmt::format;

mod lexer;
pub use lexer::{Token, lex};

mod lsp;
pub use lsp::{
    Breadcrumb, DocumentSymbol, FoldingRange, FoldingRangeKind, LineIndex, Position, SemanticToken,
    SemanticTokenType, SymbolKind, document_symbols, enclosing_nodes, folding_ranges,
    semantic_tokens,
};

mod parser;
pub use parser::{Parse, SyntaxError, parse};

mod parsed;
pub use parsed::{Parsed, parse_source_file};

mod syntax_kind;
pub use rowan::{TextRange, TextSize};
pub use syntax_kind::{SyntaxKind, SyntaxToken, VxLanguage};

pub type SyntaxNode = rowan::SyntaxNode<VxLanguage>;

/// Returns the text range of a syntax node with leading and trailing trivia
/// (whitespace, comments) stripped.
///
/// Rowan attaches trivia to the tokens they precede, so `node.text_range()`
/// includes any whitespace/comments before the first real token and after the
/// last real token. This function returns the tighter range that starts at the
/// first non-trivia token and ends at the last non-trivia token.
pub fn trimmed_range(node: &SyntaxNode) -> TextRange {
    let full = node.text_range();
    let start = node
        .descendants_with_tokens()
        .filter_map(|it| it.into_token())
        .find(|tok| !tok.kind().is_trivia())
        .map(|tok| tok.text_range().start())
        .unwrap_or(full.start());
    let end = node
        .descendants_with_tokens()
        .filter_map(|it| it.into_token())
        .filter(|tok| !tok.kind().is_trivia())
        .last()
        .map(|tok| tok.text_range().end())
        .unwrap_or(full.end());
    TextRange::new(start, end)
}

#[cfg(test)]
mod tests;
