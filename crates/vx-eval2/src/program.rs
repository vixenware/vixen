//! Shared compiled source/program state for eval2.

use std::{collections::BTreeMap, sync::Arc};

use vx_parser::{
    AstNode, Expr, Item, LineIndex as ParserLineIndex, Parse, SourceFile, Stmt, SyntaxNode,
    SyntaxToken, TextRange, TextSize, parse, trimmed_range,
};

use crate::{
    ByteOffset, ColumnIndex, EvalError, EvalErrorKind, LineIndex, SourcePoint, SourceSpan,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ProgramNodeHandle {
    start: u32,
    end: u32,
}

impl ProgramNodeHandle {
    pub(crate) fn for_node<T: AstNode>(node: &T) -> Self {
        Self::from_range(trimmed_range(node.syntax()))
    }

    pub(crate) fn for_token(token: &SyntaxToken) -> Self {
        Self::from_range(token.text_range())
    }

    fn from_range(range: TextRange) -> Self {
        Self {
            start: u32::from(range.start()),
            end: u32::from(range.end()),
        }
    }

    fn to_range(self) -> TextRange {
        TextRange::new(TextSize::from(self.start), TextSize::from(self.end))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompiledParameter {
    pub(crate) label: Arc<str>,
    pub(crate) name: ProgramNodeHandle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompiledFunction {
    pub(crate) parameters: Arc<[CompiledParameter]>,
    pub(crate) has_keyword_parameters: bool,
    pub(crate) body: Option<ProgramNodeHandle>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexedNode {
    path: Arc<[u32]>,
}

pub(crate) struct CompiledProgram {
    source_id: Arc<str>,
    line_index: ParserLineIndex,
    parse: Option<Parse>,
    functions: BTreeMap<String, CompiledFunction>,
    exprs: BTreeMap<ProgramNodeHandle, IndexedNode>,
    stmts: BTreeMap<ProgramNodeHandle, IndexedNode>,
    parse_error: Option<EvalError>,
    #[cfg(test)]
    parse_count: usize,
}

impl CompiledProgram {
    pub(crate) fn new(source_id: impl Into<String>, source_text: impl Into<String>) -> Self {
        let source_id = Arc::<str>::from(source_id.into());
        let source_text = source_text.into();
        let line_index = ParserLineIndex::new(&source_text);
        let parse = parse(&source_text);

        let mut functions = BTreeMap::new();
        let mut exprs = BTreeMap::new();
        let mut stmts = BTreeMap::new();
        let parse_error = if parse.errors().is_empty() {
            let root = SourceFile::cast_root(parse.syntax());
            for item in root.items() {
                if let Item::FnDecl(function) = item {
                    let Some(name) = function.name() else {
                        continue;
                    };
                    let parameters = function
                        .parameters()
                        .filter_map(|parameter| {
                            let name = parameter.name()?;
                            Some(CompiledParameter {
                                label: Arc::from(name.text()),
                                name: ProgramNodeHandle::for_token(&name),
                            })
                        })
                        .collect::<Vec<_>>();
                    functions.insert(
                        name.text().to_string(),
                        CompiledFunction {
                            parameters: Arc::from(parameters),
                            has_keyword_parameters: function.keyword_parameters().next().is_some(),
                            body: function
                                .body_expr()
                                .as_ref()
                                .map(ProgramNodeHandle::for_node),
                        },
                    );
                }
            }
            let mut path = Vec::new();
            index_nodes(root.syntax(), &mut path, &mut exprs, &mut stmts);
            None
        } else {
            Some(EvalError::new(
                EvalErrorKind::Internal,
                format!("parse failed: {:?}", parse.errors()),
            ))
        };

        Self {
            source_id,
            line_index,
            parse: Some(parse),
            functions,
            exprs,
            stmts,
            parse_error,
            #[cfg(test)]
            parse_count: 1,
        }
    }

    pub(crate) fn find_function(&self, entry: &str) -> Result<CompiledFunction, EvalError> {
        self.ensure_parsed()?;
        self.functions.get(entry).cloned().ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("function {entry} was not found"),
            )
        })
    }

    pub(crate) fn lookup_function(
        &self,
        name: &str,
    ) -> Result<Option<CompiledFunction>, EvalError> {
        self.ensure_parsed()?;
        Ok(self.functions.get(name).cloned())
    }

    pub(crate) fn resolve_expr(&self, handle: ProgramNodeHandle) -> Result<Expr, EvalError> {
        self.ensure_parsed()?;
        let indexed = self.exprs.get(&handle).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("expression at range {:?} was not found", handle.to_range()),
            )
        })?;
        Expr::cast(self.resolve_node(indexed)?).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!(
                    "expression at range {:?} resolved to a non-expression node",
                    handle.to_range()
                ),
            )
        })
    }

    pub(crate) fn resolve_stmt(&self, handle: ProgramNodeHandle) -> Result<Stmt, EvalError> {
        self.ensure_parsed()?;
        let indexed = self.stmts.get(&handle).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("statement at range {:?} was not found", handle.to_range()),
            )
        })?;
        Stmt::cast(self.resolve_node(indexed)?).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!(
                    "statement at range {:?} resolved to a non-statement node",
                    handle.to_range()
                ),
            )
        })
    }

    pub(crate) fn span_for_handle(&self, handle: ProgramNodeHandle) -> SourceSpan {
        self.span_for_range(handle.to_range())
    }

    pub(crate) fn span_for_node<T: AstNode>(&self, node: &T) -> SourceSpan {
        self.span_for_range(trimmed_range(node.syntax()))
    }

    pub(crate) fn span_for_token(&self, token: &SyntaxToken) -> SourceSpan {
        self.span_for_range(token.text_range())
    }

    #[cfg(test)]
    pub(crate) fn parse_count(&self) -> usize {
        self.parse_count
    }

    fn ensure_parsed(&self) -> Result<(), EvalError> {
        match &self.parse_error {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    fn resolve_node(&self, indexed: &IndexedNode) -> Result<SyntaxNode, EvalError> {
        let parse = self
            .parse
            .as_ref()
            .expect("compiled program always retains its parse");
        let mut node = parse.syntax();
        for index in indexed.path.iter().copied() {
            let next = node.children().nth(index as usize).ok_or_else(|| {
                EvalError::new(EvalErrorKind::Internal, "compiled node path diverged")
            })?;
            node = next;
        }
        Ok(node)
    }

    fn span_for_range(&self, range: TextRange) -> SourceSpan {
        SourceSpan::new(
            self.source_id.to_string(),
            self.point_for_offset(range.start()),
            self.point_for_offset(range.end()),
        )
    }

    fn point_for_offset(&self, offset: TextSize) -> SourcePoint {
        let position = self.line_index.position(offset);
        SourcePoint::new(
            ByteOffset(u32::from(offset)),
            LineIndex(position.line),
            ColumnIndex(position.col),
        )
    }
}

fn index_nodes(
    node: &SyntaxNode,
    path: &mut Vec<u32>,
    exprs: &mut BTreeMap<ProgramNodeHandle, IndexedNode>,
    stmts: &mut BTreeMap<ProgramNodeHandle, IndexedNode>,
) {
    if let Some(expr) = Expr::cast(node.clone()) {
        exprs.insert(
            ProgramNodeHandle::for_node(&expr),
            IndexedNode {
                path: Arc::from(path.clone()),
            },
        );
    }
    if let Some(stmt) = Stmt::cast(node.clone()) {
        stmts.insert(
            ProgramNodeHandle::for_node(&stmt),
            IndexedNode {
                path: Arc::from(path.clone()),
            },
        );
    }
    for (index, child) in node.children().enumerate() {
        path.push(index as u32);
        index_nodes(&child, path, exprs, stmts);
        path.pop();
    }
}
