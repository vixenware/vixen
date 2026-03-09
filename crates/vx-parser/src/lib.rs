//! Event-based parser for Vixen source files.

mod lexer;
pub use lexer::{Token, lex};

mod parser;
pub use parser::{Parse, SyntaxError, parse};

mod syntax_kind;
pub use syntax_kind::{SyntaxKind, SyntaxToken, VxLanguage};

pub type SyntaxNode = rowan::SyntaxNode<VxLanguage>;

#[cfg(test)]
mod tests;
