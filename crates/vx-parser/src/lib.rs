//! Event-based parser for Vixen source files.

mod ast;
pub use ast::{
    ArrayExpr, ArrayPattern, ArrayRestPattern, AssignStmt, AstChildren, AstNode, AtPattern,
    BinaryExpr, BindingPattern, BlockExpr, CallExpr, ClosureExpr, ClosureParamClause,
    CompoundUpdateStmt, EnumDecl, Expr, FallibleExpr, FieldAssignStmt, FieldAssignTarget,
    FieldDecl, FieldExpr, FnDecl, FunctionType, IfExpr, IndexExpr, InterpolatedStringExpr, Item,
    LetStmt, LiteralExpr, LiteralPattern, MapEntry, MapExpr, MatchArm, MatchExpr, NameRefExpr,
    NewtypeDecl, OptionalType, OrPattern, Parameter, ParenPattern, ParenType, PartialAppExpr,
    Pattern, PrefixExpr, RangeExpr, ReturnStmt, ReturnType, SlotType, SourceFile, Stmt,
    StringInterpolation, StringPart, StructDecl, StructLiteralExpr, StructLiteralField,
    StructPattern, StructPatternField, SumType, TuplePattern, TupleType, Type, TypeAliasDecl,
    TypeArgs, TypeOpExpr, TypePath, UnitPattern, VariantPattern, WildcardPattern, WithExpr,
    doc_comment,
};

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
pub use syntax_kind::{SyntaxKind, SyntaxToken, VxLanguage};

pub type SyntaxNode = rowan::SyntaxNode<VxLanguage>;

#[cfg(test)]
mod tests;
