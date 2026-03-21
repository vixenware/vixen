//! Shared compiled source/program state for eval2.

use std::{collections::BTreeMap, sync::Arc};

use vx_parser::{
    AstNode, Expr, FnDecl, Item, LineIndex as ParserLineIndex, Parse, SourceFile, Stmt, SyntaxKind,
    SyntaxNode, SyntaxToken, TextRange, TextSize, Type, parse, trimmed_range,
};
use vx_resolve::{
    DefKind, ImportGraph, ProgramScopes, SourceFileRef, SourceId, TypeCollection,
    collect_types_for_sources, stdlib_sources,
};
use vx_types::Ty;

use crate::{
    ByteOffset, ColumnIndex, EvalError, EvalErrorKind, LineIndex, SourcePoint, SourceSpan,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalProgramSource {
    pub source_id: Arc<str>,
    pub source_text: Arc<str>,
}

impl EvalProgramSource {
    pub fn new(source_id: impl Into<String>, source_text: impl Into<String>) -> Self {
        Self {
            source_id: Arc::from(source_id.into()),
            source_text: Arc::from(source_text.into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvalProgramImport {
    pub from: usize,
    pub to: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalProgram {
    pub entry_source: usize,
    pub sources: Arc<[EvalProgramSource]>,
    pub imports: Arc<[EvalProgramImport]>,
}

impl EvalProgram {
    pub fn single_source(source_id: impl Into<String>, source_text: impl Into<String>) -> Self {
        Self {
            entry_source: 0,
            sources: Arc::from([EvalProgramSource::new(source_id, source_text)]),
            imports: Arc::from([]),
        }
    }

    pub fn new(
        entry_source: usize,
        sources: impl Into<Vec<EvalProgramSource>>,
        imports: impl Into<Vec<EvalProgramImport>>,
    ) -> Result<Self, EvalError> {
        let sources = sources.into();
        if sources.is_empty() {
            return Err(EvalError::new(
                EvalErrorKind::Internal,
                "eval program must contain at least one source",
            ));
        }
        if entry_source >= sources.len() {
            return Err(EvalError::new(
                EvalErrorKind::Internal,
                format!(
                    "eval program entry source index {entry_source} is out of bounds for {} source(s)",
                    sources.len()
                ),
            ));
        }
        let imports = imports.into();
        for import in &imports {
            if import.from >= sources.len() || import.to >= sources.len() {
                return Err(EvalError::new(
                    EvalErrorKind::Internal,
                    format!(
                        "eval program import {} -> {} is out of bounds for {} source(s)",
                        import.from,
                        import.to,
                        sources.len()
                    ),
                ));
            }
        }
        Ok(Self {
            entry_source,
            sources: Arc::from(sources),
            imports: Arc::from(imports),
        })
    }

    pub fn uses_stateful_slots(&self) -> Result<bool, EvalError> {
        for source in self.sources.iter() {
            let parse = parse(source.source_text.as_ref());
            let tokens = parse.syntax().descendants_with_tokens().filter_map(|node| {
                let token = node.into_token()?;
                (!token.kind().is_trivia()).then_some(token)
            });
            if tokens
                .zip(
                    parse
                        .syntax()
                        .descendants_with_tokens()
                        .filter_map(|node| node.into_token())
                        .filter(|token| !token.kind().is_trivia())
                        .skip(1),
                )
                .any(|(current, next)| is_stateful_slot_token_pair(&current, &next))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ProgramNodeHandle {
    source: u32,
    start: u32,
    end: u32,
}

impl ProgramNodeHandle {
    pub(crate) fn for_node<T: AstNode>(source: SourceId, node: &T) -> Self {
        Self::from_range(source, trimmed_range(node.syntax()))
    }

    pub(crate) fn for_token(source: SourceId, token: &SyntaxToken) -> Self {
        Self::from_range(source, token.text_range())
    }

    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
    }

    fn from_range(source: SourceId, range: TextRange) -> Self {
        Self {
            source: source.0,
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
    pub(crate) source: SourceId,
    pub(crate) definition: ProgramNodeHandle,
    pub(crate) parameters: Arc<[CompiledParameter]>,
    pub(crate) has_keyword_parameters: bool,
    pub(crate) body: Option<ProgramNodeHandle>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexedNode {
    path: Arc<[u32]>,
}

struct CompiledProgramSource {
    source_id: Arc<str>,
    line_index: ParserLineIndex,
    parse: Parse,
    functions_by_name: BTreeMap<String, CompiledFunction>,
    functions_by_definition: BTreeMap<ProgramNodeHandle, (Arc<str>, CompiledFunction)>,
    exprs: BTreeMap<ProgramNodeHandle, IndexedNode>,
    stmts: BTreeMap<ProgramNodeHandle, IndexedNode>,
    parse_error: Option<EvalError>,
    #[cfg(test)]
    parse_count: usize,
}

pub(crate) struct CompiledProgram {
    entry_source: SourceId,
    sources: BTreeMap<u32, CompiledProgramSource>,
    scopes: ProgramScopes,
    types: TypeCollection,
}

impl CompiledProgram {
    pub(crate) fn new(source_id: impl Into<String>, source_text: impl Into<String>) -> Self {
        Self::from_eval_program(EvalProgram::single_source(source_id, source_text))
    }

    pub(crate) fn from_eval_program(program: EvalProgram) -> Self {
        let entry_source = SourceId(program.entry_source as u32);
        let mut sources = BTreeMap::new();
        let mut scope_sources = Vec::new();

        for (index, source) in program.sources.iter().enumerate() {
            let source_key = SourceId(index as u32);
            let compiled = CompiledProgramSource::new(source_key, source);
            let file = SourceFile::cast_root(compiled.parse.syntax());
            scope_sources.push(SourceFileRef {
                id: source_key,
                file,
            });
            sources.insert(source_key.0, compiled);
        }

        let mut imports = ImportGraph::new();
        for import in program.imports.iter().copied() {
            imports.add_import(SourceId(import.from as u32), SourceId(import.to as u32));
        }

        let mut type_sources = scope_sources.clone();
        for (index, stdlib) in stdlib_sources().iter().enumerate() {
            let parse = parse(stdlib.text);
            let file = SourceFile::cast_root(parse.syntax());
            type_sources.push(SourceFileRef {
                id: SourceId(program.sources.len() as u32 + index as u32),
                file,
            });
        }

        Self {
            entry_source,
            sources,
            scopes: ProgramScopes::build(&scope_sources, imports),
            types: collect_types_for_sources(&type_sources),
        }
    }

    pub(crate) fn entry_source(&self) -> SourceId {
        self.entry_source
    }

    pub(crate) fn find_function(&self, entry: &str) -> Result<CompiledFunction, EvalError> {
        self.source(self.entry_source)?
            .lookup_function_by_name(entry)?
            .ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    format!("function {entry} was not found"),
                )
            })
    }

    pub(crate) fn resolve_function_reference(
        &self,
        reference: ProgramNodeHandle,
        name: &str,
    ) -> Result<Option<(Arc<str>, CompiledFunction)>, EvalError> {
        self.ensure_parsed(reference.source())?;
        let Some(scope) = self
            .scopes
            .scope_at(reference.source(), reference.to_range().start())
        else {
            return Ok(None);
        };
        let Some(definition) = self.scopes.lookup_at(
            reference.source(),
            name,
            scope,
            reference.to_range().start(),
        ) else {
            return Ok(None);
        };
        if definition.kind != DefKind::Function {
            return Ok(None);
        }
        self.source(definition.source())?
            .lookup_function_by_definition(ProgramNodeHandle::from_range(
                definition.source(),
                definition.name_span.range,
            ))
    }

    pub(crate) fn compile_function_decl(
        &self,
        source: SourceId,
        function: &FnDecl,
    ) -> Result<Option<(Arc<str>, CompiledFunction)>, EvalError> {
        self.ensure_parsed(source)?;
        Ok(compile_function_decl(source, function)
            .map(|(name, compiled)| (Arc::from(name), compiled)))
    }

    pub(crate) fn resolve_expr(&self, handle: ProgramNodeHandle) -> Result<Expr, EvalError> {
        self.source(handle.source())?.resolve_expr(handle)
    }

    pub(crate) fn resolve_stmt(&self, handle: ProgramNodeHandle) -> Result<Stmt, EvalError> {
        self.source(handle.source())?.resolve_stmt(handle)
    }

    pub(crate) fn span_for_handle(&self, handle: ProgramNodeHandle) -> SourceSpan {
        self.source(handle.source())
            .expect("span lookup should use a valid source handle")
            .span_for_range(handle.to_range())
    }

    pub(crate) fn source_id(&self, source: SourceId) -> Arc<str> {
        self.source(source)
            .expect("source id lookup should use a valid source")
            .source_id()
    }

    pub(crate) fn span_for_node<T: AstNode>(&self, source: SourceId, node: &T) -> SourceSpan {
        self.source(source)
            .expect("node span lookup should use a valid source")
            .span_for_range(trimmed_range(node.syntax()))
    }

    pub(crate) fn span_for_token(&self, source: SourceId, token: &SyntaxToken) -> SourceSpan {
        self.source(source)
            .expect("token span lookup should use a valid source")
            .span_for_range(token.text_range())
    }

    pub(crate) fn resolve_type_annotation(&self, ty: &Type) -> Ty {
        self.types.resolve_type_annotation(ty)
    }

    pub(crate) fn types(&self) -> &TypeCollection {
        &self.types
    }

    #[cfg(test)]
    pub(crate) fn parse_count(&self) -> usize {
        self.sources
            .values()
            .map(CompiledProgramSource::parse_count)
            .sum()
    }

    fn ensure_parsed(&self, source: SourceId) -> Result<(), EvalError> {
        self.source(source)?.ensure_parsed()
    }

    fn source(&self, source: SourceId) -> Result<&CompiledProgramSource, EvalError> {
        self.sources.get(&source.0).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("compiled source {:?} was not found", source),
            )
        })
    }
}

impl CompiledProgramSource {
    fn new(source: SourceId, program_source: &EvalProgramSource) -> Self {
        let source_id = Arc::clone(&program_source.source_id);
        let (_source_text, line_index, parse) =
            normalized_parse(program_source.source_text.as_ref());

        let mut functions_by_name = BTreeMap::new();
        let mut functions_by_definition = BTreeMap::new();
        let mut exprs = BTreeMap::new();
        let mut stmts = BTreeMap::new();
        let parse_error = if parse.errors().is_empty() {
            let root = SourceFile::cast_root(parse.syntax());
            for item in root.items() {
                if let Item::FnDecl(function) = item {
                    let Some((name, compiled)) = compile_function_decl(source, &function) else {
                        continue;
                    };
                    functions_by_definition.insert(
                        compiled.definition,
                        (Arc::from(name.as_str()), compiled.clone()),
                    );
                    functions_by_name.insert(name, compiled);
                }
            }
            let mut path = Vec::new();
            index_nodes(source, root.syntax(), &mut path, &mut exprs, &mut stmts);
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
            parse,
            functions_by_name,
            functions_by_definition,
            exprs,
            stmts,
            parse_error,
            #[cfg(test)]
            parse_count: 1,
        }
    }

    fn lookup_function_by_name(&self, name: &str) -> Result<Option<CompiledFunction>, EvalError> {
        self.ensure_parsed()?;
        Ok(self.functions_by_name.get(name).cloned())
    }

    fn lookup_function_by_definition(
        &self,
        handle: ProgramNodeHandle,
    ) -> Result<Option<(Arc<str>, CompiledFunction)>, EvalError> {
        self.ensure_parsed()?;
        Ok(self.functions_by_definition.get(&handle).cloned())
    }

    fn resolve_expr(&self, handle: ProgramNodeHandle) -> Result<Expr, EvalError> {
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

    fn resolve_stmt(&self, handle: ProgramNodeHandle) -> Result<Stmt, EvalError> {
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

    fn source_id(&self) -> Arc<str> {
        Arc::clone(&self.source_id)
    }

    #[cfg(test)]
    fn parse_count(&self) -> usize {
        self.parse_count
    }

    fn ensure_parsed(&self) -> Result<(), EvalError> {
        match &self.parse_error {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    fn resolve_node(&self, indexed: &IndexedNode) -> Result<SyntaxNode, EvalError> {
        let mut node = self.parse.syntax();
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

fn normalized_parse(source_text: &str) -> (String, ParserLineIndex, Parse) {
    let parse_result = parse(source_text);
    if parse_result.errors().is_empty() {
        return (
            source_text.to_string(),
            ParserLineIndex::new(source_text),
            parse_result,
        );
    }

    let normalized_source = normalize_call_trivia(source_text);
    if normalized_source == source_text {
        return (
            source_text.to_string(),
            ParserLineIndex::new(source_text),
            parse_result,
        );
    }

    let normalized_parse = parse(&normalized_source);
    if normalized_parse.errors().len() < parse_result.errors().len() {
        (
            normalized_source.clone(),
            ParserLineIndex::new(&normalized_source),
            normalized_parse,
        )
    } else {
        (
            source_text.to_string(),
            ParserLineIndex::new(source_text),
            parse_result,
        )
    }
}

fn normalize_call_trivia(source_text: &str) -> String {
    let tokens = parse(source_text)
        .syntax()
        .descendants_with_tokens()
        .filter_map(|node| node.into_token())
        .collect::<Vec<_>>();
    let mut normalized = String::with_capacity(source_text.len());
    let mut index = 0usize;
    while index < tokens.len() {
        let token = &tokens[index];
        normalized.push_str(token.text());
        if token.kind() == SyntaxKind::LOWER_IDENT {
            let mut next = index + 1;
            while next < tokens.len() && tokens[next].kind().is_trivia() {
                next += 1;
            }
            if next > index + 1 && next < tokens.len() && tokens[next].kind() == SyntaxKind::L_PAREN
            {
                index = next;
                continue;
            }
        }
        index += 1;
    }
    normalized
}

fn compile_function_decl(
    source: SourceId,
    function: &FnDecl,
) -> Option<(String, CompiledFunction)> {
    let name = function.name()?;
    let parameters = function
        .parameters()
        .filter_map(|parameter| {
            let name = parameter.name()?;
            Some(CompiledParameter {
                label: Arc::from(name.text()),
                name: ProgramNodeHandle::for_token(source, &name),
            })
        })
        .collect::<Vec<_>>();
    Some((
        name.text().to_string(),
        CompiledFunction {
            source,
            definition: ProgramNodeHandle::for_token(source, &name),
            parameters: Arc::from(parameters),
            has_keyword_parameters: function.keyword_parameters().next().is_some(),
            body: function
                .body_expr()
                .as_ref()
                .map(|body| ProgramNodeHandle::for_node(source, body)),
        },
    ))
}

fn index_nodes(
    source: SourceId,
    node: &SyntaxNode,
    path: &mut Vec<u32>,
    exprs: &mut BTreeMap<ProgramNodeHandle, IndexedNode>,
    stmts: &mut BTreeMap<ProgramNodeHandle, IndexedNode>,
) {
    if let Some(expr) = Expr::cast(node.clone()) {
        exprs.insert(
            ProgramNodeHandle::for_node(source, &expr),
            IndexedNode {
                path: Arc::from(path.clone()),
            },
        );
    }
    if let Some(stmt) = Stmt::cast(node.clone()) {
        stmts.insert(
            ProgramNodeHandle::for_node(source, &stmt),
            IndexedNode {
                path: Arc::from(path.clone()),
            },
        );
    }
    for (index, child) in node.children().enumerate() {
        path.push(index as u32);
        index_nodes(source, &child, path, exprs, stmts);
        path.pop();
    }
}

fn is_stateful_slot_token_pair(current: &SyntaxToken, next: &SyntaxToken) -> bool {
    current.kind() == SyntaxKind::LOWER_IDENT
        && matches!(current.text(), "slot_current" | "slot_refresh")
        && next.kind() == SyntaxKind::L_PAREN
}
