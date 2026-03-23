//! Shared compiled source/program state for eval2.

use std::{collections::BTreeMap, sync::Arc};

use vx_parser::{
    AstNode, Expr as ParserExpr, FnDecl as ParserFnDecl, Item, LineIndex as ParserLineIndex, Parse,
    Pattern as ParserPattern, SourceFile, Stmt as ParserStmt, StringPart as ParserStringPart,
    StructPatternField as ParserStructPatternField, SyntaxKind, SyntaxNode, SyntaxToken, TextRange,
    TextSize, Type, parse, trimmed_range,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ExprId {
    source: u32,
    index: u32,
}

impl ExprId {
    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct BlockBodyId {
    source: u32,
    index: u32,
}

impl BlockBodyId {
    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
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
    pub(crate) body: Option<ExprId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Token {
    handle: ProgramNodeHandle,
    kind: SyntaxKind,
    text: Arc<str>,
}

pub(crate) trait HasHandle {
    fn handle(&self) -> ProgramNodeHandle;
}

impl HasHandle for Token {
    fn handle(&self) -> ProgramNodeHandle {
        self.handle
    }
}

impl Token {
    pub(crate) fn kind(&self) -> SyntaxKind {
        self.kind
    }

    pub(crate) fn text(&self) -> &str {
        self.text.as_ref()
    }

    pub(crate) fn handle(&self) -> ProgramNodeHandle {
        self.handle
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MatchArmPlan {
    pub(crate) pattern: MatchPattern,
    pub(crate) body: ExprId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LetPattern {
    Binding { name: String, source: SourceSpan },
    Wildcard,
    Tuple(Vec<LetPattern>),
    Unit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MatchFieldPattern {
    pub(crate) name: String,
    pub(crate) source: SourceSpan,
    pub(crate) pattern: Option<Box<MatchPattern>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MatchPattern {
    Binding {
        name: String,
        source: SourceSpan,
    },
    Wildcard,
    Literal(crate::Value),
    Tuple(Vec<MatchPattern>),
    Struct(Vec<MatchFieldPattern>),
    Unit,
    Variant {
        name: Arc<str>,
        fields: Option<Vec<MatchFieldPattern>>,
    },
    At {
        binding_name: String,
        binding_source: SourceSpan,
        pattern: Box<MatchPattern>,
    },
    Or(Vec<MatchPattern>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Expr {
    handle: ProgramNodeHandle,
    kind: ExprKind,
}

impl Expr {
    pub(crate) fn handle(&self) -> ProgramNodeHandle {
        self.handle
    }

    pub(crate) fn kind(&self) -> &ExprKind {
        &self.kind
    }
}

impl HasHandle for Expr {
    fn handle(&self) -> ProgramNodeHandle {
        self.handle
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExprKind {
    Block(BlockExpr),
    Literal(LiteralExpr),
    PathLiteral(PathLiteralExpr),
    Paren(ParenExpr),
    Closure(ClosureExpr),
    NameRef(NameRefExpr),
    Call(CallExpr),
    Array(ArrayExpr),
    Tuple(TupleExpr),
    Range(RangeExpr),
    Map(MapExpr),
    Fail(FailExpr),
    Fallible(FallibleExpr),
    If(IfExpr),
    Match(MatchExpr),
    Where(WhereExpr),
    Prefix(PrefixExpr),
    Binary(BinaryExpr),
    InterpolatedString(InterpolatedStringExpr),
    StructLiteral(StructLiteralExpr),
    Field(FieldExpr),
    Unsupported { kind: SyntaxKind },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BlockExpr {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) body: BlockBodyId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BlockBody {
    pub(crate) steps: Arc<[BlockStep]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BlockStep {
    Let { pattern: LetPattern, expr: ExprId },
    FnDecl(FunctionStmt),
    Expr(ExprId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiteralExpr {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) token: Option<Token>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PathLiteralExpr {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) descriptor: Option<Arc<str>>,
    pub(crate) interpolated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParenExpr {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) expr: Option<ExprId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClosureExpr {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) parameters: Arc<[CompiledParameter]>,
    pub(crate) body: Option<ExprId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NameRefExpr {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) token: Option<Token>,
    pub(crate) name: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallExpr {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) callee: Option<ExprId>,
    pub(crate) type_args: Arc<[Ty]>,
    pub(crate) positional_args: Arc<[ExprId]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArrayExpr {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) items: Arc<[ArrayItem]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArrayItem {
    Expr(ExprId),
    Spread(ExprId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TupleExpr {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) items: Arc<[ExprId]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RangeExpr {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) start: Option<ExprId>,
    pub(crate) end: Option<ExprId>,
    pub(crate) inclusive_end: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MapExpr {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) entries: Arc<[(ExprId, ExprId)]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FailExpr {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) message: Option<ExprId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FallibleExpr {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) expr: Option<ExprId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IfExpr {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) condition: Option<ExprId>,
    pub(crate) then_branch: Option<ExprId>,
    pub(crate) else_branch: Option<ExprId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MatchExpr {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) scrutinee: Option<ExprId>,
    pub(crate) arms: Arc<[MatchArmPlan]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WhereExpr {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) call: Option<ExprId>,
    pub(crate) keyword_bindings: Arc<[KeywordBinding]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeywordBinding {
    pub(crate) name: Arc<str>,
    pub(crate) expr: Option<ExprId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PrefixExpr {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) operator: Option<SyntaxKind>,
    pub(crate) expr: Option<ExprId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BinaryExpr {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) lhs: Option<ExprId>,
    pub(crate) rhs: Option<ExprId>,
    pub(crate) operator: Option<SyntaxKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InterpolatedStringExpr {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) parts: Arc<[StringPart]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StringPart {
    Text(Arc<str>),
    Interpolation(ExprId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StructLiteralExpr {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) type_name: Option<Arc<str>>,
    pub(crate) parts: Arc<[StructLiteralPart]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StructLiteralPart {
    Field {
        name: Arc<str>,
        expr: Option<ExprId>,
    },
    Spread(ExprId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FieldExpr {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) receiver: Option<ExprId>,
    pub(crate) field_name: Option<Token>,
}

macro_rules! impl_has_handle {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl HasHandle for $ty {
                fn handle(&self) -> ProgramNodeHandle {
                    self.handle
                }
            }
        )+
    };
}

impl_has_handle!(
    BlockExpr,
    LiteralExpr,
    PathLiteralExpr,
    ParenExpr,
    ClosureExpr,
    NameRefExpr,
    CallExpr,
    ArrayExpr,
    TupleExpr,
    RangeExpr,
    MapExpr,
    FailExpr,
    FallibleExpr,
    IfExpr,
    MatchExpr,
    WhereExpr,
    PrefixExpr,
    BinaryExpr,
    InterpolatedStringExpr,
    StructLiteralExpr,
    FieldExpr,
    FunctionStmt,
);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FunctionStmt {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) name: Arc<str>,
    pub(crate) compiled: CompiledFunction,
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
    exprs: Vec<Expr>,
    expr_ids: BTreeMap<ProgramNodeHandle, ExprId>,
    block_bodies: Vec<BlockBody>,
    type_nodes: BTreeMap<ProgramNodeHandle, IndexedNode>,
    pending_call_type_args: Vec<(ExprId, Arc<[ProgramNodeHandle]>)>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VariantConstructor {
    pub(crate) name: Arc<str>,
    pub(crate) fields: Arc<[Arc<str>]>,
}

pub(crate) trait IntoExprLookupId {
    fn into_expr_id(self, program: &CompiledProgram) -> Result<ExprId, EvalError>;
}

impl IntoExprLookupId for ProgramNodeHandle {
    fn into_expr_id(self, program: &CompiledProgram) -> Result<ExprId, EvalError> {
        program.source(self.source())?.expr_id(self)
    }
}

impl IntoExprLookupId for ExprId {
    fn into_expr_id(self, _program: &CompiledProgram) -> Result<ExprId, EvalError> {
        Ok(self)
    }
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
        .with_resolved_type_args()
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

    pub(crate) fn expr_id(&self, key: impl IntoExprLookupId) -> Result<ExprId, EvalError> {
        key.into_expr_id(self)
    }

    pub(crate) fn resolve_expr_id(&self, id: ExprId) -> Result<&Expr, EvalError> {
        self.source(id.source())?.resolve_expr(id)
    }

    pub(crate) fn resolve_expr(&self, key: impl IntoExprLookupId) -> Result<&Expr, EvalError> {
        let id = self.expr_id(key)?;
        self.resolve_expr_id(id)
    }

    pub(crate) fn resolve_block_body(&self, id: BlockBodyId) -> Result<&BlockBody, EvalError> {
        self.source(id.source())?.resolve_block_body(id)
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

    pub(crate) fn span_for_expr(&self, expr: &Expr) -> SourceSpan {
        self.span_for_handle(expr.handle())
    }

    pub(crate) fn span_for_expr_id(&self, id: ExprId) -> Result<SourceSpan, EvalError> {
        Ok(self.span_for_expr(self.resolve_expr_id(id)?))
    }

    pub(crate) fn span_for_token_ref(&self, token: &Token) -> SourceSpan {
        self.span_for_handle(token.handle())
    }

    pub(crate) fn types(&self) -> &TypeCollection {
        &self.types
    }

    pub(crate) fn resolve_variant_constructor(&self, name: &str) -> Option<VariantConstructor> {
        let variant_id = self
            .types
            .env
            .lookup(name)
            .iter()
            .copied()
            .find(|type_id| {
                matches!(
                    self.types.env.get(*type_id),
                    vx_types::TypeDef::Struct(struct_def) if struct_def.parent_enum.is_some()
                )
            })?;
        let vx_types::TypeDef::Struct(struct_def) = self.types.env.get(variant_id) else {
            return None;
        };
        Some(VariantConstructor {
            name: Arc::from(struct_def.name.as_str()),
            fields: Arc::from(
                struct_def
                    .fields
                    .iter()
                    .map(|field| Arc::from(field.name.as_str()))
                    .collect::<Vec<_>>(),
            ),
        })
    }

    pub(crate) fn resolve_newtype_constructor(&self, name: &str) -> Option<Arc<str>> {
        self.types
            .env
            .lookup(name)
            .iter()
            .copied()
            .find(|type_id| matches!(self.types.env.get(*type_id), vx_types::TypeDef::Newtype(_)))
            .map(|type_id| match self.types.env.get(type_id) {
                vx_types::TypeDef::Newtype(def) => Arc::from(def.name.as_str()),
                _ => unreachable!("checked above"),
            })
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

    fn with_resolved_type_args(mut self) -> Self {
        for source in self.sources.values_mut() {
            source
                .resolve_pending_call_type_args(&self.types)
                .expect("compiled call type arguments should resolve");
        }
        self
    }
}

impl CompiledProgramSource {
    fn new(source: SourceId, program_source: &EvalProgramSource) -> Self {
        let source_id = Arc::clone(&program_source.source_id);
        let (_source_text, line_index, parse) =
            normalized_parse(program_source.source_text.as_ref());

        let mut functions_by_name = BTreeMap::new();
        let mut functions_by_definition = BTreeMap::new();
        let mut exprs = Vec::new();
        let mut expr_ids = BTreeMap::new();
        let mut block_bodies = Vec::new();
        let mut type_nodes = BTreeMap::new();
        let mut pending_call_type_args = Vec::new();
        let parse_error = if parse.errors().is_empty() {
            let root = SourceFile::cast_root(parse.syntax());
            let mut path = Vec::new();
            match index_nodes(
                source,
                &source_id,
                &line_index,
                root.syntax(),
                &mut path,
                &mut exprs,
                &mut expr_ids,
                &mut block_bodies,
                &mut type_nodes,
                &mut pending_call_type_args,
            ) {
                Ok(()) => {
                    for item in root.items() {
                        if let Item::FnDecl(function) = item {
                            let Some((name, compiled)) =
                                compile_function_decl(source, &function, &expr_ids)
                            else {
                                continue;
                            };
                            functions_by_definition.insert(
                                compiled.definition,
                                (Arc::from(name.as_str()), compiled.clone()),
                            );
                            functions_by_name.insert(name, compiled);
                        }
                    }
                    None
                }
                Err(error) => Some(error),
            }
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
            expr_ids,
            block_bodies,
            type_nodes,
            pending_call_type_args,
            parse_error,
            #[cfg(test)]
            parse_count: 1,
        }
    }

    fn resolve_type_handle_with(
        &self,
        handle: ProgramNodeHandle,
        types: &TypeCollection,
    ) -> Result<Ty, EvalError> {
        self.ensure_parsed()?;
        let indexed = self.type_nodes.get(&handle).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("type at range {:?} was not found", handle.to_range()),
            )
        })?;
        let ty = Type::cast(self.resolve_node(indexed)?).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!(
                    "type at range {:?} resolved to a non-type node",
                    handle.to_range()
                ),
            )
        })?;
        Ok(types.resolve_type_annotation(&ty))
    }

    fn resolve_pending_call_type_args(&mut self, types: &TypeCollection) -> Result<(), EvalError> {
        self.ensure_parsed()?;
        for (expr_id, handles) in &self.pending_call_type_args {
            let resolved = Arc::<[Ty]>::from(
                handles
                    .iter()
                    .copied()
                    .map(|handle| self.resolve_type_handle_with(handle, types))
                    .collect::<Result<Vec<_>, _>>()?,
            );
            let expr = self.exprs.get_mut(expr_id.index as usize).ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    format!("expression id {:?} was not found", expr_id),
                )
            })?;
            let ExprKind::Call(call) = &mut expr.kind else {
                return Err(EvalError::new(
                    EvalErrorKind::Internal,
                    format!(
                        "pending call type args targeted non-call expression {:?}",
                        expr_id
                    ),
                ));
            };
            call.type_args = resolved;
        }
        self.pending_call_type_args.clear();
        Ok(())
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

    fn expr_id(&self, handle: ProgramNodeHandle) -> Result<ExprId, EvalError> {
        self.ensure_parsed()?;
        self.expr_ids.get(&handle).copied().ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("expression at range {:?} was not found", handle.to_range()),
            )
        })
    }

    fn resolve_expr(&self, id: ExprId) -> Result<&Expr, EvalError> {
        self.ensure_parsed()?;
        self.exprs.get(id.index as usize).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("expression id {:?} was not found", id),
            )
        })
    }

    fn resolve_block_body(&self, id: BlockBodyId) -> Result<&BlockBody, EvalError> {
        self.ensure_parsed()?;
        self.block_bodies.get(id.index as usize).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("block body id {:?} was not found", id),
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
    function: &ParserFnDecl,
    expr_ids: &BTreeMap<ProgramNodeHandle, ExprId>,
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
            body: function.body_expr().as_ref().and_then(|body| {
                expr_ids
                    .get(&ProgramNodeHandle::for_node(source, body))
                    .copied()
            }),
        },
    ))
}

fn require_expr_id(
    expr_ids: &BTreeMap<ProgramNodeHandle, ExprId>,
    handle: ProgramNodeHandle,
    label: &str,
) -> Result<ExprId, EvalError> {
    expr_ids.get(&handle).copied().ok_or_else(|| {
        EvalError::new(
            EvalErrorKind::Internal,
            format!(
                "{label} expression at range {:?} was not indexed",
                handle.to_range()
            ),
        )
    })
}

fn index_nodes(
    source: SourceId,
    source_id: &Arc<str>,
    line_index: &ParserLineIndex,
    node: &SyntaxNode,
    path: &mut Vec<u32>,
    exprs: &mut Vec<Expr>,
    expr_ids: &mut BTreeMap<ProgramNodeHandle, ExprId>,
    block_bodies: &mut Vec<BlockBody>,
    type_nodes: &mut BTreeMap<ProgramNodeHandle, IndexedNode>,
    pending_call_type_args: &mut Vec<(ExprId, Arc<[ProgramNodeHandle]>)>,
) -> Result<(), EvalError> {
    for (index, child) in node.children().enumerate() {
        path.push(index as u32);
        index_nodes(
            source,
            source_id,
            line_index,
            &child,
            path,
            exprs,
            expr_ids,
            block_bodies,
            type_nodes,
            pending_call_type_args,
        )?;
        path.pop();
    }
    if let Some(expr) = ParserExpr::cast(node.clone()) {
        let id = ExprId {
            source: source.0,
            index: exprs.len() as u32,
        };
        let compiled = compile_expr(
            source,
            source_id,
            line_index,
            &expr,
            expr_ids,
            id,
            block_bodies,
            pending_call_type_args,
        )?;
        expr_ids.insert(compiled.handle(), id);
        exprs.push(compiled);
    }
    if let Some(ty) = Type::cast(node.clone()) {
        type_nodes.insert(
            ProgramNodeHandle::for_node(source, &ty),
            IndexedNode {
                path: Arc::from(path.clone()),
            },
        );
    }
    Ok(())
}

fn compile_expr(
    source: SourceId,
    source_id: &Arc<str>,
    line_index: &ParserLineIndex,
    expr: &ParserExpr,
    expr_ids: &BTreeMap<ProgramNodeHandle, ExprId>,
    expr_id: ExprId,
    block_bodies: &mut Vec<BlockBody>,
    pending_call_type_args: &mut Vec<(ExprId, Arc<[ProgramNodeHandle]>)>,
) -> Result<Expr, EvalError> {
    let handle = ProgramNodeHandle::for_node(source, expr);
    let kind = match expr {
        ParserExpr::Block(block) => {
            let body = BlockBodyId {
                source: source.0,
                index: block_bodies.len() as u32,
            };
            block_bodies.push(BlockBody {
                steps: Arc::from(
                    block
                        .statements()
                        .map(|stmt| {
                            compile_block_step(source, source_id, line_index, &stmt, expr_ids)
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                ),
            });
            ExprKind::Block(BlockExpr { handle, body })
        }
        ParserExpr::Literal(literal) => ExprKind::Literal(LiteralExpr {
            handle,
            token: literal.token().map(|token| compile_token(source, &token)),
        }),
        ParserExpr::PathLiteral(path_literal) => ExprKind::PathLiteral(PathLiteralExpr {
            handle,
            descriptor: compile_path_literal_descriptor(path_literal),
            interpolated: path_literal.interpolated().is_some(),
        }),
        ParserExpr::Paren(paren) => ExprKind::Paren(ParenExpr {
            handle,
            expr: paren
                .expr()
                .as_ref()
                .map(|expr| {
                    require_expr_id(
                        expr_ids,
                        ProgramNodeHandle::for_node(source, expr),
                        "paren inner",
                    )
                })
                .transpose()?,
        }),
        ParserExpr::Closure(closure) => ExprKind::Closure(ClosureExpr {
            handle,
            parameters: compile_closure_parameters_ast(source, closure)?,
            body: closure
                .body()
                .as_ref()
                .map(|body| {
                    require_expr_id(
                        expr_ids,
                        ProgramNodeHandle::for_node(source, body),
                        "closure body",
                    )
                })
                .transpose()?,
        }),
        ParserExpr::NameRef(name_ref) => ExprKind::NameRef(NameRefExpr {
            handle,
            token: name_ref.token().map(|token| compile_token(source, &token)),
            name: Arc::from(name_ref_text_from_ast(name_ref)),
        }),
        ParserExpr::Call(call) => {
            let raw_type_args: Arc<[ProgramNodeHandle]> = Arc::from(
                call.type_args()
                    .into_iter()
                    .flat_map(|args| args.types())
                    .map(|ty| ProgramNodeHandle::for_node(source, &ty))
                    .collect::<Vec<_>>(),
            );
            if !raw_type_args.is_empty() {
                pending_call_type_args.push((expr_id, Arc::clone(&raw_type_args)));
            }
            ExprKind::Call(CallExpr {
                handle,
                callee: call
                    .callee()
                    .as_ref()
                    .map(|callee| {
                        require_expr_id(
                            expr_ids,
                            ProgramNodeHandle::for_node(source, callee),
                            "call callee",
                        )
                    })
                    .transpose()?,
                type_args: Arc::from([]),
                positional_args: Arc::from(
                    call.positional_args()
                        .into_iter()
                        .map(|arg| {
                            require_expr_id(
                                expr_ids,
                                ProgramNodeHandle::for_node(source, &arg),
                                "call argument",
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                ),
            })
        }
        ParserExpr::Array(array) => ExprKind::Array(ArrayExpr {
            handle,
            items: Arc::from(
                array
                    .items()
                    .map(|item| match item {
                        vx_parser::ArrayItem::Expr(expr) => Ok(ArrayItem::Expr(require_expr_id(
                            expr_ids,
                            ProgramNodeHandle::for_node(source, &expr),
                            "array item",
                        )?)),
                        vx_parser::ArrayItem::Spread(spread) => {
                            Ok(ArrayItem::Spread(require_expr_id(
                                expr_ids,
                                spread
                                    .expr()
                                    .as_ref()
                                    .map(|expr| ProgramNodeHandle::for_node(source, expr))
                                    .ok_or_else(|| {
                                        EvalError::new(
                                            EvalErrorKind::Internal,
                                            "array spread expression has no operand",
                                        )
                                    })?,
                                "array spread",
                            )?))
                        }
                    })
                    .collect::<Result<Vec<_>, EvalError>>()?,
            ),
        }),
        ParserExpr::Tuple(tuple) => ExprKind::Tuple(TupleExpr {
            handle,
            items: Arc::from(
                tuple
                    .items()
                    .map(|item| {
                        require_expr_id(
                            expr_ids,
                            ProgramNodeHandle::for_node(source, &item),
                            "tuple item",
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ),
        }),
        ParserExpr::Range(range) => ExprKind::Range(RangeExpr {
            handle,
            start: range
                .start()
                .as_ref()
                .map(|expr| {
                    require_expr_id(
                        expr_ids,
                        ProgramNodeHandle::for_node(source, expr),
                        "range start",
                    )
                })
                .transpose()?,
            end: range
                .end()
                .as_ref()
                .map(|expr| {
                    require_expr_id(
                        expr_ids,
                        ProgramNodeHandle::for_node(source, expr),
                        "range end",
                    )
                })
                .transpose()?,
            inclusive_end: range.is_inclusive(),
        }),
        ParserExpr::Map(map) => ExprKind::Map(MapExpr {
            handle,
            entries: Arc::from(
                map.entries()
                    .map(|entry| {
                        let key = entry.key().ok_or_else(|| {
                            EvalError::new(EvalErrorKind::Internal, "map entry has no explicit key")
                        })?;
                        let value = entry.value().ok_or_else(|| {
                            EvalError::new(EvalErrorKind::Internal, "map entry has no value")
                        })?;
                        Ok((
                            require_expr_id(
                                expr_ids,
                                ProgramNodeHandle::for_node(source, &key),
                                "map key",
                            )?,
                            require_expr_id(
                                expr_ids,
                                ProgramNodeHandle::for_node(source, &value),
                                "map value",
                            )?,
                        ))
                    })
                    .collect::<Result<Vec<_>, EvalError>>()?,
            ),
        }),
        ParserExpr::Fail(fail) => ExprKind::Fail(FailExpr {
            handle,
            message: fail
                .message()
                .as_ref()
                .map(|expr| {
                    require_expr_id(
                        expr_ids,
                        ProgramNodeHandle::for_node(source, expr),
                        "fail message",
                    )
                })
                .transpose()?,
        }),
        ParserExpr::Fallible(fallible) => ExprKind::Fallible(FallibleExpr {
            handle,
            expr: fallible
                .expr()
                .as_ref()
                .map(|expr| {
                    require_expr_id(
                        expr_ids,
                        ProgramNodeHandle::for_node(source, expr),
                        "fallible operand",
                    )
                })
                .transpose()?,
        }),
        ParserExpr::If(if_expr) => ExprKind::If(IfExpr {
            handle,
            condition: if_expr
                .condition()
                .as_ref()
                .map(|expr| {
                    require_expr_id(
                        expr_ids,
                        ProgramNodeHandle::for_node(source, expr),
                        "if condition",
                    )
                })
                .transpose()?,
            then_branch: if_expr
                .then_branch()
                .as_ref()
                .map(|expr| {
                    require_expr_id(
                        expr_ids,
                        ProgramNodeHandle::for_node(source, expr),
                        "if then branch",
                    )
                })
                .transpose()?,
            else_branch: if_expr
                .else_branch()
                .as_ref()
                .map(|expr| {
                    require_expr_id(
                        expr_ids,
                        ProgramNodeHandle::for_node(source, expr),
                        "if else branch",
                    )
                })
                .transpose()?,
        }),
        ParserExpr::Match(match_expr) => ExprKind::Match(MatchExpr {
            handle,
            scrutinee: match_expr
                .syntax()
                .children()
                .find_map(ParserExpr::cast)
                .as_ref()
                .map(|expr| {
                    require_expr_id(
                        expr_ids,
                        ProgramNodeHandle::for_node(source, expr),
                        "match scrutinee",
                    )
                })
                .transpose()?,
            arms: Arc::from(
                match_expr
                    .arms()
                    .map(|arm| {
                        let pattern = arm.pattern().ok_or_else(|| {
                            EvalError::new(EvalErrorKind::Internal, "match arm has no pattern")
                        })?;
                        let body = arm.expr().ok_or_else(|| {
                            EvalError::new(EvalErrorKind::Internal, "match arm has no body")
                        })?;
                        Ok(MatchArmPlan {
                            pattern: compile_match_pattern(
                                source, source_id, line_index, &pattern,
                            )?,
                            body: require_expr_id(
                                expr_ids,
                                ProgramNodeHandle::for_node(source, &body),
                                "match arm body",
                            )?,
                        })
                    })
                    .collect::<Result<Vec<_>, EvalError>>()?,
            ),
        }),
        ParserExpr::Where(where_expr) => ExprKind::Where(WhereExpr {
            handle,
            call: where_expr
                .callee()
                .as_ref()
                .map(|expr| {
                    require_expr_id(
                        expr_ids,
                        ProgramNodeHandle::for_node(source, expr),
                        "where callee",
                    )
                })
                .transpose()?,
            keyword_bindings: Arc::from(compile_where_keyword_bindings(
                source, where_expr, expr_ids,
            )?),
        }),
        ParserExpr::Prefix(prefix) => ExprKind::Prefix(PrefixExpr {
            handle,
            operator: prefix.operator_token().map(|token| token.kind()),
            expr: prefix
                .expr()
                .as_ref()
                .map(|expr| {
                    require_expr_id(
                        expr_ids,
                        ProgramNodeHandle::for_node(source, expr),
                        "prefix operand",
                    )
                })
                .transpose()?,
        }),
        ParserExpr::Binary(binary) => ExprKind::Binary(BinaryExpr {
            handle,
            lhs: binary
                .lhs()
                .as_ref()
                .map(|expr| {
                    require_expr_id(
                        expr_ids,
                        ProgramNodeHandle::for_node(source, expr),
                        "binary lhs",
                    )
                })
                .transpose()?,
            rhs: binary
                .rhs()
                .as_ref()
                .map(|expr| {
                    require_expr_id(
                        expr_ids,
                        ProgramNodeHandle::for_node(source, expr),
                        "binary rhs",
                    )
                })
                .transpose()?,
            operator: binary
                .syntax()
                .children_with_tokens()
                .filter_map(|child| child.into_token())
                .map(|token| token.kind())
                .find(|kind| {
                    matches!(
                        kind,
                        SyntaxKind::AMP2
                            | SyntaxKind::PIPE2
                            | SyntaxKind::EQ2
                            | SyntaxKind::BANG_EQ
                            | SyntaxKind::PLUS
                            | SyntaxKind::MINUS
                            | SyntaxKind::STAR
                            | SyntaxKind::SLASH
                            | SyntaxKind::PERCENT
                            | SyntaxKind::LT
                            | SyntaxKind::LT_EQ
                            | SyntaxKind::GT
                            | SyntaxKind::GT_EQ
                    )
                }),
        }),
        ParserExpr::InterpolatedString(interpolated) => {
            ExprKind::InterpolatedString(InterpolatedStringExpr {
                handle,
                parts: Arc::from(
                    interpolated
                        .parts()
                        .map(|part| match part {
                            ParserStringPart::Text(token) => {
                                Ok(StringPart::Text(Arc::from(token.text())))
                            }
                            ParserStringPart::Interpolation(interpolation) => {
                                Ok(StringPart::Interpolation(require_expr_id(
                                    expr_ids,
                                    interpolation
                                        .expr()
                                        .as_ref()
                                        .map(|expr| ProgramNodeHandle::for_node(source, expr))
                                        .ok_or_else(|| {
                                            EvalError::new(
                                                EvalErrorKind::Internal,
                                                "string interpolation has no embedded expression",
                                            )
                                        })?,
                                    "string interpolation",
                                )?))
                            }
                        })
                        .collect::<Result<Vec<_>, EvalError>>()?,
                ),
            })
        }
        ParserExpr::StructLiteral(struct_literal) => ExprKind::StructLiteral(StructLiteralExpr {
            handle,
            type_name: compile_struct_literal_type_name(struct_literal),
            parts: Arc::from(compile_struct_literal_parts(
                source,
                struct_literal,
                expr_ids,
            )?),
        }),
        ParserExpr::Field(field) => ExprKind::Field(FieldExpr {
            handle,
            receiver: field
                .receiver()
                .as_ref()
                .map(|expr| {
                    require_expr_id(
                        expr_ids,
                        ProgramNodeHandle::for_node(source, expr),
                        "field receiver",
                    )
                })
                .transpose()?,
            field_name: field
                .field_name()
                .map(|token| compile_token(source, &token)),
        }),
        other => ExprKind::Unsupported {
            kind: other.syntax().kind(),
        },
    };
    Ok(Expr { handle, kind })
}

fn compile_block_step(
    source: SourceId,
    source_id: &Arc<str>,
    line_index: &ParserLineIndex,
    stmt: &ParserStmt,
    expr_ids: &BTreeMap<ProgramNodeHandle, ExprId>,
) -> Result<BlockStep, EvalError> {
    Ok(match stmt {
        ParserStmt::Let(let_stmt) => BlockStep::Let {
            pattern: let_stmt
                .pattern()
                .map(|pattern| compile_let_pattern(source, source_id, line_index, &pattern))
                .transpose()?
                .ok_or_else(|| {
                    EvalError::new(EvalErrorKind::Internal, "let statement has no pattern")
                })?,
            expr: let_stmt
                .expr()
                .as_ref()
                .map(|expr| {
                    require_expr_id(
                        expr_ids,
                        ProgramNodeHandle::for_node(source, expr),
                        "let value",
                    )
                })
                .transpose()?
                .ok_or_else(|| {
                    EvalError::new(EvalErrorKind::Internal, "let statement has no value")
                })?,
        },
        ParserStmt::FnDecl(function) => {
            let Some((name, compiled)) = compile_function_decl(source, function, expr_ids) else {
                return Err(EvalError::new(
                    EvalErrorKind::Internal,
                    "function declaration could not be compiled",
                ));
            };
            BlockStep::FnDecl(FunctionStmt {
                handle: ProgramNodeHandle::for_node(source, stmt),
                name: Arc::from(name),
                compiled,
            })
        }
        ParserStmt::Expr(expr) => BlockStep::Expr(require_expr_id(
            expr_ids,
            ProgramNodeHandle::for_node(source, expr),
            "expression statement",
        )?),
        other => {
            return Err(EvalError::new(
                EvalErrorKind::Internal,
                format!("unsupported block statement: {other:?}"),
            ));
        }
    })
}

fn compile_token(source: SourceId, token: &SyntaxToken) -> Token {
    Token {
        handle: ProgramNodeHandle::for_token(source, token),
        kind: token.kind(),
        text: Arc::from(token.text()),
    }
}

fn compile_path_literal_descriptor(path_literal: &vx_parser::PathLiteralExpr) -> Option<Arc<str>> {
    let descriptor = path_literal
        .syntax()
        .children_with_tokens()
        .filter_map(|child| child.into_token())
        .filter(|token| {
            !matches!(
                token.kind(),
                SyntaxKind::AT
                    | SyntaxKind::L_PAREN
                    | SyntaxKind::R_PAREN
                    | SyntaxKind::WHITESPACE
                    | SyntaxKind::LINE_COMMENT
                    | SyntaxKind::BLOCK_COMMENT
                    | SyntaxKind::DOC_COMMENT
            )
        })
        .map(|token| token.text().to_string())
        .collect::<String>();
    (!descriptor.is_empty()).then(|| Arc::from(descriptor))
}

fn compile_struct_literal_type_name(
    struct_literal: &vx_parser::StructLiteralExpr,
) -> Option<Arc<str>> {
    let raw_name = struct_literal
        .syntax()
        .children_with_tokens()
        .take_while(|child| child.kind() != SyntaxKind::STRUCT_LITERAL_BODY)
        .filter_map(|child| child.into_token())
        .filter(|token| matches!(token.kind(), SyntaxKind::UPPER_IDENT | SyntaxKind::DOT))
        .map(|token| token.text().to_string())
        .collect::<String>();
    (!raw_name.is_empty()).then(|| Arc::from(raw_name.rsplit('.').next().unwrap_or(&raw_name)))
}

fn compile_struct_literal_parts(
    source: SourceId,
    struct_literal: &vx_parser::StructLiteralExpr,
    expr_ids: &BTreeMap<ProgramNodeHandle, ExprId>,
) -> Result<Vec<StructLiteralPart>, EvalError> {
    struct_literal
        .fields()
        .map(|field| match field.expr() {
            Some(expr) => {
                if let Some(name_token) = field.name() {
                    Ok(StructLiteralPart::Field {
                        name: Arc::from(name_token.text()),
                        expr: Some(require_expr_id(
                            expr_ids,
                            ProgramNodeHandle::for_node(source, &expr),
                            "struct field value",
                        )?),
                    })
                } else {
                    Ok(StructLiteralPart::Spread(require_expr_id(
                        expr_ids,
                        ProgramNodeHandle::for_node(source, &expr),
                        "struct spread",
                    )?))
                }
            }
            None => {
                let name = field.name().ok_or_else(|| {
                    EvalError::new(EvalErrorKind::Internal, "struct field has no name")
                })?;
                Ok(StructLiteralPart::Field {
                    name: Arc::from(name.text()),
                    expr: None,
                })
            }
        })
        .collect()
}

fn compile_where_keyword_bindings(
    source: SourceId,
    where_expr: &vx_parser::WhereExpr,
    expr_ids: &BTreeMap<ProgramNodeHandle, ExprId>,
) -> Result<Vec<KeywordBinding>, EvalError> {
    let keywords = where_expr.keywords().ok_or_else(|| {
        EvalError::new(
            EvalErrorKind::Internal,
            "where expression has no keyword record",
        )
    })?;
    let ParserExpr::Record(record) = keywords else {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            "where expression currently requires a record keyword payload",
        ));
    };
    record
        .fields()
        .map(|field| {
            let name_token = field.name().ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    "where keyword field is missing a name",
                )
            })?;
            Ok(KeywordBinding {
                name: Arc::from(name_token.text()),
                expr: field
                    .value()
                    .as_ref()
                    .map(|expr| {
                        require_expr_id(
                            expr_ids,
                            ProgramNodeHandle::for_node(source, expr),
                            "where keyword binding",
                        )
                    })
                    .transpose()?,
            })
        })
        .collect()
}

fn compile_let_pattern(
    source: SourceId,
    source_id: &Arc<str>,
    line_index: &ParserLineIndex,
    pattern: &ParserPattern,
) -> Result<LetPattern, EvalError> {
    match pattern {
        ParserPattern::Binding(binding) => {
            let name_token = binding.name().ok_or_else(|| {
                EvalError::new(EvalErrorKind::Internal, "binding pattern has no name")
            })?;
            let name = name_token.text().to_string();
            Ok(LetPattern::Binding {
                name,
                source: span_for_token(source, source_id, line_index, &name_token),
            })
        }
        ParserPattern::Wildcard(_) => Ok(LetPattern::Wildcard),
        ParserPattern::Paren(paren) => {
            let inner = paren.inner().ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    "paren pattern has no inner pattern",
                )
            })?;
            compile_let_pattern(source, source_id, line_index, &inner)
        }
        ParserPattern::Unit(_) => Ok(LetPattern::Unit),
        ParserPattern::Tuple(tuple) => Ok(LetPattern::Tuple(
            tuple
                .items()
                .map(|item| compile_let_pattern(source, source_id, line_index, &item))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        other => Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("unsupported let pattern: {other:?}"),
        )),
    }
}

fn compile_match_pattern(
    source: SourceId,
    source_id: &Arc<str>,
    line_index: &ParserLineIndex,
    pattern: &ParserPattern,
) -> Result<MatchPattern, EvalError> {
    match pattern {
        ParserPattern::Binding(binding) => {
            let name_token = binding.name().ok_or_else(|| {
                EvalError::new(EvalErrorKind::Internal, "binding pattern has no name")
            })?;
            Ok(MatchPattern::Binding {
                name: name_token.text().to_string(),
                source: span_for_token(source, source_id, line_index, &name_token),
            })
        }
        ParserPattern::Wildcard(_) => Ok(MatchPattern::Wildcard),
        ParserPattern::Literal(literal) => Ok(MatchPattern::Literal(
            compile_literal_pattern_value(source, source_id, line_index, literal)?,
        )),
        ParserPattern::Tuple(tuple) => Ok(MatchPattern::Tuple(
            tuple
                .items()
                .map(|item| compile_match_pattern(source, source_id, line_index, &item))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        ParserPattern::Struct(struct_pattern) => Ok(MatchPattern::Struct(
            struct_pattern
                .fields()
                .map(|field| compile_match_field_pattern(source, source_id, line_index, &field))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        ParserPattern::Paren(paren) => {
            let inner = paren.inner().ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    "paren pattern has no inner pattern",
                )
            })?;
            compile_match_pattern(source, source_id, line_index, &inner)
        }
        ParserPattern::Unit(_) => Ok(MatchPattern::Unit),
        ParserPattern::Variant(variant) => Ok(MatchPattern::Variant {
            name: Arc::from(
                variant
                    .segments()
                    .last()
                    .ok_or_else(|| {
                        EvalError::new(EvalErrorKind::Internal, "variant pattern has no name")
                    })?
                    .text(),
            ),
            fields: variant
                .fields()
                .map(|fields| {
                    fields
                        .fields()
                        .map(|field| {
                            compile_match_field_pattern(source, source_id, line_index, &field)
                        })
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?,
        }),
        ParserPattern::At(at) => {
            let binding_name = at.binding_name().ok_or_else(|| {
                EvalError::new(EvalErrorKind::Internal, "at pattern has no binding name")
            })?;
            let nested = at.pattern().ok_or_else(|| {
                EvalError::new(EvalErrorKind::Internal, "at pattern has no nested pattern")
            })?;
            Ok(MatchPattern::At {
                binding_name: binding_name.text().to_string(),
                binding_source: span_for_token(source, source_id, line_index, &binding_name),
                pattern: Box::new(compile_match_pattern(
                    source, source_id, line_index, &nested,
                )?),
            })
        }
        ParserPattern::Or(or) => Ok(MatchPattern::Or(
            or.patterns()
                .map(|pattern| compile_match_pattern(source, source_id, line_index, &pattern))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        other => Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("unsupported match pattern: {other:?}"),
        )),
    }
}

fn compile_match_field_pattern(
    source: SourceId,
    source_id: &Arc<str>,
    line_index: &ParserLineIndex,
    field: &ParserStructPatternField,
) -> Result<MatchFieldPattern, EvalError> {
    let name_token = field.name().ok_or_else(|| {
        EvalError::new(EvalErrorKind::Internal, "struct pattern field has no name")
    })?;
    Ok(MatchFieldPattern {
        name: name_token.text().to_string(),
        source: span_for_token(source, source_id, line_index, &name_token),
        pattern: field
            .pattern()
            .map(|pattern| {
                compile_match_pattern(source, source_id, line_index, &pattern).map(Box::new)
            })
            .transpose()?,
    })
}

fn compile_literal_pattern_value(
    source: SourceId,
    source_id: &Arc<str>,
    line_index: &ParserLineIndex,
    literal: &vx_parser::LiteralPattern,
) -> Result<crate::Value, EvalError> {
    let token = literal
        .token()
        .ok_or_else(|| EvalError::new(EvalErrorKind::Internal, "literal pattern has no token"))?;
    let source = span_for_token(source, source_id, line_index, &token);
    match token.kind() {
        SyntaxKind::INT => {
            let value = token.text().parse::<u64>().map_err(|source| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    format!("invalid integer literal {}: {source}", token.text()),
                )
            })?;
            Ok(crate::Value::u64(value).with_provenance(crate::Provenance::literal(source)))
        }
        SyntaxKind::TRUE_KW => {
            Ok(crate::Value::bool(true).with_provenance(crate::Provenance::literal(source)))
        }
        SyntaxKind::FALSE_KW => {
            Ok(crate::Value::bool(false).with_provenance(crate::Provenance::literal(source)))
        }
        SyntaxKind::STRING => Ok(crate::Value::string(decode_string_literal(token.text())?)
            .with_provenance(crate::Provenance::literal(source))),
        other => Err(EvalError::new(
            EvalErrorKind::Internal,
            format!("unsupported literal pattern token: {other:?}"),
        )),
    }
}

fn span_for_token(
    source: SourceId,
    source_id: &Arc<str>,
    line_index: &ParserLineIndex,
    token: &SyntaxToken,
) -> SourceSpan {
    span_for_range(source, source_id, line_index, token.text_range())
}

fn span_for_range(
    _source: SourceId,
    source_id: &Arc<str>,
    line_index: &ParserLineIndex,
    range: TextRange,
) -> SourceSpan {
    let start = line_index.position(range.start());
    let end = line_index.position(range.end());
    SourceSpan::new(
        source_id.to_string(),
        SourcePoint::new(
            ByteOffset(u32::from(range.start())),
            LineIndex(start.line),
            ColumnIndex(start.col),
        ),
        SourcePoint::new(
            ByteOffset(u32::from(range.end())),
            LineIndex(end.line),
            ColumnIndex(end.col),
        ),
    )
}

fn name_ref_text_from_ast(name_ref: &vx_parser::NameRefExpr) -> String {
    name_ref
        .syntax()
        .children_with_tokens()
        .filter_map(|child| child.into_token())
        .filter(|token| {
            matches!(
                token.kind(),
                SyntaxKind::LOWER_IDENT
                    | SyntaxKind::UPPER_IDENT
                    | SyntaxKind::DOLLAR_IDENT
                    | SyntaxKind::UNDERSCORE
                    | SyntaxKind::DOT
            )
        })
        .map(|token| token.text().to_string())
        .collect()
}

fn compile_closure_parameters_ast(
    source: SourceId,
    closure: &vx_parser::ClosureExpr,
) -> Result<Arc<[CompiledParameter]>, EvalError> {
    closure
        .parameters()
        .map(|parameter| {
            let name = parameter.name().ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    "closure parameter has no binding name",
                )
            })?;
            Ok(CompiledParameter {
                label: Arc::from(name.text()),
                name: ProgramNodeHandle::for_token(source, &name),
            })
        })
        .collect::<Result<Vec<_>, EvalError>>()
        .map(Arc::from)
}

fn decode_string_literal(text: &str) -> Result<String, EvalError> {
    let inner = text
        .strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("invalid string literal token: {text}"),
            )
        })?;
    let mut decoded = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            decoded.push(ch);
            continue;
        }
        let escaped = chars.next().ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("unterminated string escape in literal: {text}"),
            )
        })?;
        match escaped {
            'n' => decoded.push('\n'),
            'r' => decoded.push('\r'),
            't' => decoded.push('\t'),
            '"' => decoded.push('"'),
            '\\' => decoded.push('\\'),
            other => decoded.push(other),
        }
    }
    Ok(decoded)
}

fn is_stateful_slot_token_pair(current: &SyntaxToken, next: &SyntaxToken) -> bool {
    current.kind() == SyntaxKind::LOWER_IDENT
        && matches!(current.text(), "slot_current" | "slot_refresh")
        && next.kind() == SyntaxKind::L_PAREN
}
