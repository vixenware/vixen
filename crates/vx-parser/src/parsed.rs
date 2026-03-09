use std::marker::PhantomData;

use crate::{AstNode, Parse, SourceFile, SyntaxError, SyntaxNode, parse};

#[derive(Debug, Clone)]
pub struct Parsed<T> {
    parse: Parse,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Parsed<T> {
    pub(crate) fn new(parse: Parse) -> Self {
        Self {
            parse,
            _marker: PhantomData,
        }
    }

    pub fn errors(&self) -> &[SyntaxError] {
        self.parse.errors()
    }

    pub fn syntax(&self) -> SyntaxNode {
        self.parse.syntax()
    }

    pub fn into_parse(self) -> Parse {
        self.parse
    }
}

impl<T: AstNode> Parsed<T> {
    pub fn tree(&self) -> T {
        T::cast(self.syntax()).expect("parsed tree should match requested AST root")
    }
}

pub fn parse_source_file(source: &str) -> Parsed<SourceFile> {
    Parsed::new(parse(source))
}
