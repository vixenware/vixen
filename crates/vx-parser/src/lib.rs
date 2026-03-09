//! Event-based parser for Vixen source files.

mod ast;
pub use ast::{
    ArrayExpr, ArrayPattern, ArrayRestPattern, AssignStmt, AstChildren, AstNode, AtPattern,
    BinaryExpr, BindingPattern, BlockExpr, CallExpr, CompoundUpdateStmt, EnumDecl, Expr,
    FieldAssignStmt, FieldAssignTarget, FieldDecl, FieldExpr, FnDecl, FunctionType, IfExpr,
    InterpolatedStringExpr, Item, LetStmt, LiteralExpr, LiteralPattern, MapEntry, MapExpr,
    MatchArm, MatchExpr, NameRefExpr, NewtypeDecl, OptionalType, OrPattern, Parameter,
    ParenPattern, ParenType, Pattern, ReturnStmt, ReturnType, SlotType, SourceFile, Stmt,
    StringInterpolation, StringPart, StructDecl, StructLiteralExpr, StructLiteralField,
    StructPattern, StructPatternField, SumType, TuplePattern, TupleType, Type, TypeAliasDecl,
    TypeArgs, TypePath, UnitPattern, VariantPattern, WildcardPattern, WithExpr,
};

mod lexer;
pub use lexer::{Token, lex};

mod parser;
pub use parser::{Parse, SyntaxError, parse};

mod syntax_kind;
pub use syntax_kind::{SyntaxKind, SyntaxToken, VxLanguage};

pub type SyntaxNode = rowan::SyntaxNode<VxLanguage>;

#[cfg(test)]
mod tests;
