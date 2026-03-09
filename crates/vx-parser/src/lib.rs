//! Event-based parser for Vixen source files.

mod ast;
pub use ast::{
    ArrayExpr, AssignStmt, AstChildren, AstNode, BinaryExpr, BlockExpr, CallExpr,
    CompoundUpdateStmt, EnumDecl, Expr, FieldAssignStmt, FieldAssignTarget, FieldDecl, FieldExpr,
    FnDecl, IfExpr, InterpolatedStringExpr, Item, LetStmt, LiteralExpr, MapEntry, MapExpr,
    MatchArm, MatchExpr, NameRefExpr, NewtypeDecl, Parameter, ReturnStmt, SourceFile, Stmt,
    StringInterpolation, StringPart, StructDecl, StructLiteralExpr, StructLiteralField,
    TypeAliasDecl, WithExpr,
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
