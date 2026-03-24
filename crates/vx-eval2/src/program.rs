//! Shared compiled source/program state for eval2.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

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
    ByteOffset, ColumnIndex, EvalError, EvalErrorKind, EvalExprKind, LineIndex, SourcePoint,
    SourceSpan,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct BytecodeBodyId {
    source: u32,
    index: u32,
}

impl BytecodeBodyId {
    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
    }
}

impl From<BlockBodyId> for BytecodeBodyId {
    fn from(value: BlockBodyId) -> Self {
        Self {
            source: value.source,
            index: value.index,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct LiteralPlanId {
    source: u32,
    index: u32,
}

impl LiteralPlanId {
    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct PathLiteralPlanId {
    source: u32,
    index: u32,
}

impl PathLiteralPlanId {
    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct NameRefPlanId {
    source: u32,
    index: u32,
}

impl NameRefPlanId {
    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct CallPlanId {
    source: u32,
    index: u32,
}

impl CallPlanId {
    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct CallCalleePlanId {
    source: u32,
    index: u32,
}

impl CallCalleePlanId {
    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct DirectCallCalleePlanId {
    source: u32,
    index: u32,
}

impl DirectCallCalleePlanId {
    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct FieldPlanId {
    source: u32,
    index: u32,
}

impl FieldPlanId {
    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct WherePlanId {
    source: u32,
    index: u32,
}

impl WherePlanId {
    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct FalliblePlanId {
    source: u32,
    index: u32,
}

impl FalliblePlanId {
    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct IfPlanId {
    source: u32,
    index: u32,
}

impl IfPlanId {
    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct PrefixPlanId {
    source: u32,
    index: u32,
}

impl PrefixPlanId {
    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct BinaryPlanId {
    source: u32,
    index: u32,
}

impl BinaryPlanId {
    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ArrayPlanId {
    source: u32,
    index: u32,
}

impl ArrayPlanId {
    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct TuplePlanId {
    source: u32,
    index: u32,
}

impl TuplePlanId {
    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct RangePlanId {
    source: u32,
    index: u32,
}

impl RangePlanId {
    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct MapPlanId {
    source: u32,
    index: u32,
}

impl MapPlanId {
    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct FailPlanId {
    source: u32,
    index: u32,
}

impl FailPlanId {
    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct InterpolatedStringPlanId {
    source: u32,
    index: u32,
}

impl InterpolatedStringPlanId {
    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct StructLiteralPlanId {
    source: u32,
    index: u32,
}

impl StructLiteralPlanId {
    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ParenPlanId {
    source: u32,
    index: u32,
}

impl ParenPlanId {
    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ClosurePlanId {
    source: u32,
    index: u32,
}

impl ClosurePlanId {
    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct MatchPlanId {
    source: u32,
    index: u32,
}

impl MatchPlanId {
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
    pub(crate) body: Option<ExprBodyRef>,
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
    pub(crate) body: ExprBodyRef,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ExprExecKind {
    Expr,
    Block,
    Ready,
    Access,
    Builder,
    Control,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ExprExecPlan {
    kind: ExprExecKind,
    trace_kind: EvalExprKind,
    detail: u8,
    payload: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ExprBodyRef {
    source: u32,
    index: u32,
    plan: ExprExecPlan,
}

impl ExprExecPlan {
    fn new(kind: ExprExecKind, trace_kind: EvalExprKind, detail: u8, payload: u32) -> Self {
        Self {
            kind,
            trace_kind,
            detail,
            payload,
        }
    }

    pub(crate) fn expr() -> Self {
        Self::new(ExprExecKind::Expr, EvalExprKind::Unsupported, 0, 0)
    }

    fn block(trace_kind: EvalExprKind, payload: u32) -> Self {
        Self::new(ExprExecKind::Block, trace_kind, 0, payload)
    }

    fn ready(trace_kind: EvalExprKind, kind: ReadyPlanKind, payload: u32) -> Self {
        Self::new(ExprExecKind::Ready, trace_kind, kind as u8, payload)
    }

    fn builder(trace_kind: EvalExprKind, kind: BuilderPlanKind, payload: u32) -> Self {
        Self::new(ExprExecKind::Builder, trace_kind, kind as u8, payload)
    }

    fn control(trace_kind: EvalExprKind, kind: ControlPlanKind, payload: u32) -> Self {
        Self::new(ExprExecKind::Control, trace_kind, kind as u8, payload)
    }

    fn access(trace_kind: EvalExprKind, kind: AccessPlanKind, payload: u32) -> Self {
        Self::new(ExprExecKind::Access, trace_kind, kind as u8, payload)
    }

    pub(crate) fn kind(self) -> ExprExecKind {
        self.kind
    }

    pub(crate) fn trace_kind(self) -> EvalExprKind {
        self.trace_kind
    }

    pub(crate) fn ready_kind(self) -> ReadyPlanKind {
        debug_assert_eq!(self.kind, ExprExecKind::Ready);
        match self.detail {
            x if x == ReadyPlanKind::Literal as u8 => ReadyPlanKind::Literal,
            x if x == ReadyPlanKind::PathLiteral as u8 => ReadyPlanKind::PathLiteral,
            x if x == ReadyPlanKind::Closure as u8 => ReadyPlanKind::Closure,
            x if x == ReadyPlanKind::NameRef as u8 => ReadyPlanKind::NameRef,
            other => panic!("invalid ready plan kind {other}"),
        }
    }

    pub(crate) fn builder_kind(self) -> BuilderPlanKind {
        debug_assert_eq!(self.kind, ExprExecKind::Builder);
        match self.detail {
            x if x == BuilderPlanKind::Array as u8 => BuilderPlanKind::Array,
            x if x == BuilderPlanKind::Tuple as u8 => BuilderPlanKind::Tuple,
            x if x == BuilderPlanKind::Range as u8 => BuilderPlanKind::Range,
            x if x == BuilderPlanKind::Map as u8 => BuilderPlanKind::Map,
            x if x == BuilderPlanKind::InterpolatedString as u8 => {
                BuilderPlanKind::InterpolatedString
            }
            x if x == BuilderPlanKind::StructLiteral as u8 => BuilderPlanKind::StructLiteral,
            other => panic!("invalid builder plan kind {other}"),
        }
    }

    pub(crate) fn control_kind(self) -> ControlPlanKind {
        debug_assert_eq!(self.kind, ExprExecKind::Control);
        match self.detail {
            x if x == ControlPlanKind::Fail as u8 => ControlPlanKind::Fail,
            x if x == ControlPlanKind::Fallible as u8 => ControlPlanKind::Fallible,
            x if x == ControlPlanKind::If as u8 => ControlPlanKind::If,
            x if x == ControlPlanKind::Match as u8 => ControlPlanKind::Match,
            x if x == ControlPlanKind::Where as u8 => ControlPlanKind::Where,
            x if x == ControlPlanKind::Prefix as u8 => ControlPlanKind::Prefix,
            x if x == ControlPlanKind::Binary as u8 => ControlPlanKind::Binary,
            other => panic!("invalid control plan kind {other}"),
        }
    }

    pub(crate) fn access_kind(self) -> AccessPlanKind {
        debug_assert_eq!(self.kind, ExprExecKind::Access);
        match self.detail {
            x if x == AccessPlanKind::Paren as u8 => AccessPlanKind::Paren,
            x if x == AccessPlanKind::Call as u8 => AccessPlanKind::Call,
            x if x == AccessPlanKind::Field as u8 => AccessPlanKind::Field,
            other => panic!("invalid access plan kind {other}"),
        }
    }

    pub(crate) fn block_body_id(self, source: SourceId) -> BlockBodyId {
        debug_assert_eq!(self.kind, ExprExecKind::Block);
        BlockBodyId {
            source: source.0,
            index: self.payload,
        }
    }

    pub(crate) fn literal_plan_id(self, source: SourceId) -> LiteralPlanId {
        debug_assert_eq!(self.ready_kind(), ReadyPlanKind::Literal);
        LiteralPlanId {
            source: source.0,
            index: self.payload,
        }
    }

    pub(crate) fn path_literal_plan_id(self, source: SourceId) -> PathLiteralPlanId {
        debug_assert_eq!(self.ready_kind(), ReadyPlanKind::PathLiteral);
        PathLiteralPlanId {
            source: source.0,
            index: self.payload,
        }
    }

    pub(crate) fn closure_plan_id(self, source: SourceId) -> ClosurePlanId {
        debug_assert_eq!(self.ready_kind(), ReadyPlanKind::Closure);
        ClosurePlanId {
            source: source.0,
            index: self.payload,
        }
    }

    pub(crate) fn name_ref_plan_id(self, source: SourceId) -> NameRefPlanId {
        debug_assert_eq!(self.ready_kind(), ReadyPlanKind::NameRef);
        NameRefPlanId {
            source: source.0,
            index: self.payload,
        }
    }

    pub(crate) fn array_plan_id(self, source: SourceId) -> ArrayPlanId {
        debug_assert_eq!(self.builder_kind(), BuilderPlanKind::Array);
        ArrayPlanId {
            source: source.0,
            index: self.payload,
        }
    }

    pub(crate) fn tuple_plan_id(self, source: SourceId) -> TuplePlanId {
        debug_assert_eq!(self.builder_kind(), BuilderPlanKind::Tuple);
        TuplePlanId {
            source: source.0,
            index: self.payload,
        }
    }

    pub(crate) fn range_plan_id(self, source: SourceId) -> RangePlanId {
        debug_assert_eq!(self.builder_kind(), BuilderPlanKind::Range);
        RangePlanId {
            source: source.0,
            index: self.payload,
        }
    }

    pub(crate) fn map_plan_id(self, source: SourceId) -> MapPlanId {
        debug_assert_eq!(self.builder_kind(), BuilderPlanKind::Map);
        MapPlanId {
            source: source.0,
            index: self.payload,
        }
    }

    pub(crate) fn interpolated_string_plan_id(self, source: SourceId) -> InterpolatedStringPlanId {
        debug_assert_eq!(self.builder_kind(), BuilderPlanKind::InterpolatedString);
        InterpolatedStringPlanId {
            source: source.0,
            index: self.payload,
        }
    }

    pub(crate) fn struct_literal_plan_id(self, source: SourceId) -> StructLiteralPlanId {
        debug_assert_eq!(self.builder_kind(), BuilderPlanKind::StructLiteral);
        StructLiteralPlanId {
            source: source.0,
            index: self.payload,
        }
    }

    pub(crate) fn fail_plan_id(self, source: SourceId) -> FailPlanId {
        debug_assert_eq!(self.control_kind(), ControlPlanKind::Fail);
        FailPlanId {
            source: source.0,
            index: self.payload,
        }
    }

    pub(crate) fn fallible_plan_id(self, source: SourceId) -> FalliblePlanId {
        debug_assert_eq!(self.control_kind(), ControlPlanKind::Fallible);
        FalliblePlanId {
            source: source.0,
            index: self.payload,
        }
    }

    pub(crate) fn if_plan_id(self, source: SourceId) -> IfPlanId {
        debug_assert_eq!(self.control_kind(), ControlPlanKind::If);
        IfPlanId {
            source: source.0,
            index: self.payload,
        }
    }

    pub(crate) fn match_plan_id(self, source: SourceId) -> MatchPlanId {
        debug_assert_eq!(self.control_kind(), ControlPlanKind::Match);
        MatchPlanId {
            source: source.0,
            index: self.payload,
        }
    }

    pub(crate) fn where_plan_id(self, source: SourceId) -> WherePlanId {
        debug_assert_eq!(self.control_kind(), ControlPlanKind::Where);
        WherePlanId {
            source: source.0,
            index: self.payload,
        }
    }

    pub(crate) fn prefix_plan_id(self, source: SourceId) -> PrefixPlanId {
        debug_assert_eq!(self.control_kind(), ControlPlanKind::Prefix);
        PrefixPlanId {
            source: source.0,
            index: self.payload,
        }
    }

    pub(crate) fn binary_plan_id(self, source: SourceId) -> BinaryPlanId {
        debug_assert_eq!(self.control_kind(), ControlPlanKind::Binary);
        BinaryPlanId {
            source: source.0,
            index: self.payload,
        }
    }

    pub(crate) fn paren_plan_id(self, source: SourceId) -> ParenPlanId {
        debug_assert_eq!(self.access_kind(), AccessPlanKind::Paren);
        ParenPlanId {
            source: source.0,
            index: self.payload,
        }
    }

    pub(crate) fn call_plan_id(self, source: SourceId) -> CallPlanId {
        debug_assert_eq!(self.access_kind(), AccessPlanKind::Call);
        CallPlanId {
            source: source.0,
            index: self.payload,
        }
    }

    pub(crate) fn field_plan_id(self, source: SourceId) -> FieldPlanId {
        debug_assert_eq!(self.access_kind(), AccessPlanKind::Field);
        FieldPlanId {
            source: source.0,
            index: self.payload,
        }
    }
}

impl ExprBodyRef {
    pub(crate) fn unresolved(expr: ExprId) -> Self {
        Self {
            source: expr.source,
            index: expr.index,
            plan: ExprExecPlan::expr(),
        }
    }

    pub(crate) fn expr_id(self) -> ExprId {
        ExprId {
            source: self.source,
            index: self.index,
        }
    }

    #[cfg(test)]
    pub(crate) fn source(self) -> SourceId {
        SourceId(self.source)
    }

    pub(crate) fn plan(self) -> ExprExecPlan {
        self.plan
    }

    fn with_plan(self, plan: ExprExecPlan) -> Self {
        Self {
            source: self.source,
            index: self.index,
            plan,
        }
    }
}

impl From<ExprBodyRef> for ExprId {
    fn from(value: ExprBodyRef) -> Self {
        value.expr_id()
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadyPlanKind {
    Literal,
    PathLiteral,
    Closure,
    NameRef,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuilderPlanKind {
    Array,
    Tuple,
    Range,
    Map,
    InterpolatedString,
    StructLiteral,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlPlanKind {
    Fail,
    Fallible,
    If,
    Match,
    Where,
    Prefix,
    Binary,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccessPlanKind {
    Paren,
    Call,
    Field,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LiteralPlan {
    U64 {
        handle: ProgramNodeHandle,
        value: u64,
    },
    Bool {
        handle: ProgramNodeHandle,
        value: bool,
    },
    String {
        handle: ProgramNodeHandle,
        value: Arc<str>,
    },
    RawToken {
        handle: ProgramNodeHandle,
        kind: SyntaxKind,
        text: Arc<str>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PathLiteralPlan {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) descriptor: Option<Arc<str>>,
    pub(crate) interpolated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NameRefPlan {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) name: Arc<str>,
    pub(crate) resolved: Option<ResolvedNameRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedFunctionDirectCallCalleePlan {
    pub(crate) resolved_name: Arc<str>,
    pub(crate) function: CompiledFunction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VariantConstructorDirectCallCalleePlan {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) constructor: VariantConstructor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewtypeConstructorDirectCallCalleePlan {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) display_name: Arc<str>,
    pub(crate) newtype_name: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StaticMemberDirectCallCalleePlan {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) full_name: Arc<str>,
    pub(crate) variant_constructor: Option<VariantConstructor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirectCallCalleePlanKind {
    ResolvedFunction,
    VariantConstructor,
    NewtypeConstructor,
    StaticMember,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DirectCallCalleePlan {
    kind: DirectCallCalleePlanKind,
    payload: u32,
}

impl DirectCallCalleePlan {
    fn new(kind: DirectCallCalleePlanKind, payload: u32) -> Self {
        Self { kind, payload }
    }

    pub(crate) fn kind(&self) -> DirectCallCalleePlanKind {
        self.kind
    }

    pub(crate) fn resolved_function_index(&self) -> u32 {
        debug_assert_eq!(self.kind, DirectCallCalleePlanKind::ResolvedFunction);
        self.payload
    }

    pub(crate) fn variant_constructor_index(&self) -> u32 {
        debug_assert_eq!(self.kind, DirectCallCalleePlanKind::VariantConstructor);
        self.payload
    }

    pub(crate) fn newtype_constructor_index(&self) -> u32 {
        debug_assert_eq!(self.kind, DirectCallCalleePlanKind::NewtypeConstructor);
        self.payload
    }

    pub(crate) fn static_member_index(&self) -> u32 {
        debug_assert_eq!(self.kind, DirectCallCalleePlanKind::StaticMember);
        self.payload
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallCalleePlanKind {
    NameRef,
    Direct,
    Field,
    Expr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallCalleePlan {
    kind: CallCalleePlanKind,
    payload: u32,
}

impl CallCalleePlan {
    fn new(kind: CallCalleePlanKind, payload: u32) -> Self {
        Self { kind, payload }
    }

    pub(crate) fn kind(&self) -> CallCalleePlanKind {
        self.kind
    }

    pub(crate) fn name_ref_plan_id(&self, source: SourceId) -> NameRefPlanId {
        debug_assert_eq!(self.kind, CallCalleePlanKind::NameRef);
        NameRefPlanId {
            source: source.0,
            index: self.payload,
        }
    }

    pub(crate) fn direct_call_callee_plan_id(&self, source: SourceId) -> DirectCallCalleePlanId {
        debug_assert_eq!(self.kind, CallCalleePlanKind::Direct);
        DirectCallCalleePlanId {
            source: source.0,
            index: self.payload,
        }
    }

    pub(crate) fn field_plan_id(&self, source: SourceId) -> FieldPlanId {
        debug_assert_eq!(self.kind, CallCalleePlanKind::Field);
        FieldPlanId {
            source: source.0,
            index: self.payload,
        }
    }

    pub(crate) fn expr_id(&self, source: SourceId) -> ExprId {
        debug_assert_eq!(self.kind, CallCalleePlanKind::Expr);
        ExprId {
            source: source.0,
            index: self.payload,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallPlan {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) callee: Option<CallCalleePlanId>,
    pub(crate) type_args: Arc<[Ty]>,
    pub(crate) positional_args: Arc<[ExprBodyRef]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FieldPlan {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) receiver: Option<ExprBodyRef>,
    pub(crate) field_name: Option<Arc<str>>,
    pub(crate) static_member: Option<StaticMemberPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WherePlan {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) call: Option<CallPlanId>,
    pub(crate) keyword_bindings: Arc<[KeywordBindingPlan]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeywordBindingPlan {
    pub(crate) name: Arc<str>,
    pub(crate) expr: Option<ExprBodyRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FalliblePlan {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) expr: Option<ExprBodyRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IfPlan {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) condition: Option<ExprBodyRef>,
    pub(crate) then_branch: Option<ExprBodyRef>,
    pub(crate) else_branch: Option<ExprBodyRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PrefixPlan {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) operator: Option<SyntaxKind>,
    pub(crate) expr: Option<ExprBodyRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BinaryPlan {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) lhs: Option<ExprBodyRef>,
    pub(crate) rhs: Option<ExprBodyRef>,
    pub(crate) operator: Option<SyntaxKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArrayPlan {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) items: Arc<[ArrayPlanItem]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TuplePlan {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) items: Arc<[ExprBodyRef]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RangePlan {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) start: Option<ExprBodyRef>,
    pub(crate) end: Option<ExprBodyRef>,
    pub(crate) inclusive_end: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MapPlan {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) entries: Arc<[MapEntryPlan]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FailPlan {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) message: Option<ExprBodyRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InterpolatedStringPlan {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) parts: Arc<[InterpolatedStringPlanPart]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StructLiteralPlan {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) type_name: Option<Arc<str>>,
    pub(crate) parts: Arc<[StructLiteralPlanPart]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParenPlan {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) expr: Option<ExprBodyRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClosurePlan {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) parameters: Arc<[CompiledParameter]>,
    pub(crate) body: Option<ExprBodyRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MatchPlan {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) scrutinee: Option<ExprBodyRef>,
    pub(crate) arms: Arc<[MatchArmPlan]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArrayPlanItem {
    Expr(ExprBodyRef),
    Spread(ExprBodyRef),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MapEntryPlan {
    pub(crate) key: ExprBodyRef,
    pub(crate) value: ExprBodyRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InterpolatedStringPlanPart {
    Text(Arc<str>),
    Interpolation(ExprBodyRef),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StructLiteralPlanPart {
    Field {
        name: Arc<str>,
        expr: Option<ExprBodyRef>,
    },
    Spread(ExprBodyRef),
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
pub(crate) struct BytecodeBody {
    pub(crate) local_slots: Arc<[BytecodeLocalSlot]>,
    pub(crate) local_count: usize,
    pub(crate) ops: Arc<[BytecodeOp]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BytecodeLocalSlot {
    pub(crate) name: Arc<str>,
    pub(crate) prefill_from_scope: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BytecodeOp {
    EvalReady(ExprExecPlan),
    LoadLocal {
        slot: u32,
        source: SourceSpan,
    },
    EvalBytecodeBody(BytecodeBodyId),
    EvalCall {
        body: ExprBodyRef,
        call: CallPlan,
    },
    EvalMatch {
        body: ExprBodyRef,
        match_plan: MatchPlan,
    },
    EvalWhere {
        body: ExprBodyRef,
        where_plan: WherePlan,
    },
    EvalBody(ExprBodyRef),
    EvalPlan(ExprBodyRef),
    EvalIfBranches {
        then_branch: ExprBodyRef,
        else_branch: ExprBodyRef,
    },
    EvalBinary {
        operator: SyntaxKind,
        rhs: ExprBodyRef,
    },
    ApplyPrefix(SyntaxKind),
    ProjectField {
        field_name: Arc<str>,
        field_source: SourceSpan,
    },
    RaiseUserFail {
        fail_source: SourceSpan,
    },
    BindLet {
        pattern: LetPattern,
        local_bindings: Arc<[(Arc<str>, u32)]>,
    },
    BindFnDecl {
        function: FunctionStmt,
        local_slot: Option<u32>,
    },
    SetLast,
    ReturnLast,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BlockStep {
    Let {
        pattern: LetPattern,
        expr: ExprBodyRef,
    },
    FnDecl(FunctionStmt),
    Expr(ExprBodyRef),
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
    pub(crate) resolved: Option<ResolvedNameRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResolvedNameRef {
    Function {
        resolved_name: Arc<str>,
        function: CompiledFunction,
    },
    VariantConstructor(VariantConstructor),
    NewtypeConstructor {
        display_name: Arc<str>,
        newtype_name: Arc<str>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CallCallee {
    NameRef(NameRefExpr),
    ResolvedFunction {
        resolved_name: Arc<str>,
        function: CompiledFunction,
    },
    VariantConstructor {
        handle: ProgramNodeHandle,
        constructor: VariantConstructor,
    },
    NewtypeConstructor {
        handle: ProgramNodeHandle,
        display_name: Arc<str>,
        newtype_name: Arc<str>,
    },
    StaticMember {
        handle: ProgramNodeHandle,
        full_name: Arc<str>,
        variant_constructor: Option<VariantConstructor>,
    },
    Field(FieldExpr),
    Expr(ExprId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallExpr {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) callee: Option<CallCallee>,
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
    pub(crate) call_expr: Option<ExprId>,
    pub(crate) call: Option<CallExpr>,
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
    pub(crate) static_receiver: Option<FieldStaticReceiver>,
    pub(crate) field_name: Option<Arc<str>>,
    pub(crate) static_member: Option<StaticMemberPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FieldStaticReceiver {
    pub(crate) handle: ProgramNodeHandle,
    pub(crate) name: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StaticMemberPlan {
    pub(crate) full_name: Arc<str>,
    pub(crate) variant_constructor: Option<VariantConstructor>,
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

#[derive(Default)]
struct ExprPlanTables {
    literal_plans: Vec<LiteralPlan>,
    path_literal_plans: Vec<PathLiteralPlan>,
    name_ref_plans: Vec<NameRefPlan>,
    direct_call_callee_plans: Vec<DirectCallCalleePlan>,
    resolved_function_direct_call_callee_plans: Vec<ResolvedFunctionDirectCallCalleePlan>,
    variant_constructor_direct_call_callee_plans: Vec<VariantConstructorDirectCallCalleePlan>,
    newtype_constructor_direct_call_callee_plans: Vec<NewtypeConstructorDirectCallCalleePlan>,
    static_member_direct_call_callee_plans: Vec<StaticMemberDirectCallCalleePlan>,
    call_callee_plans: Vec<CallCalleePlan>,
    call_plans: Vec<CallPlan>,
    field_plans: Vec<FieldPlan>,
    where_plans: Vec<WherePlan>,
    fallible_plans: Vec<FalliblePlan>,
    if_plans: Vec<IfPlan>,
    prefix_plans: Vec<PrefixPlan>,
    binary_plans: Vec<BinaryPlan>,
    array_plans: Vec<ArrayPlan>,
    tuple_plans: Vec<TuplePlan>,
    range_plans: Vec<RangePlan>,
    map_plans: Vec<MapPlan>,
    fail_plans: Vec<FailPlan>,
    interpolated_string_plans: Vec<InterpolatedStringPlan>,
    struct_literal_plans: Vec<StructLiteralPlan>,
    paren_plans: Vec<ParenPlan>,
    closure_plans: Vec<ClosurePlan>,
    match_plans: Vec<MatchPlan>,
}

struct CompiledProgramSource {
    source_id: Arc<str>,
    line_index: ParserLineIndex,
    parse: Parse,
    functions_by_name: BTreeMap<String, CompiledFunction>,
    functions_by_definition: BTreeMap<ProgramNodeHandle, (Arc<str>, CompiledFunction)>,
    exprs: Vec<Expr>,
    expr_plans: Vec<ExprExecPlan>,
    literal_plans: Vec<LiteralPlan>,
    path_literal_plans: Vec<PathLiteralPlan>,
    name_ref_plans: Vec<NameRefPlan>,
    direct_call_callee_plans: Vec<DirectCallCalleePlan>,
    resolved_function_direct_call_callee_plans: Vec<ResolvedFunctionDirectCallCalleePlan>,
    variant_constructor_direct_call_callee_plans: Vec<VariantConstructorDirectCallCalleePlan>,
    newtype_constructor_direct_call_callee_plans: Vec<NewtypeConstructorDirectCallCalleePlan>,
    static_member_direct_call_callee_plans: Vec<StaticMemberDirectCallCalleePlan>,
    call_callee_plans: Vec<CallCalleePlan>,
    call_plans: Vec<CallPlan>,
    field_plans: Vec<FieldPlan>,
    where_plans: Vec<WherePlan>,
    fallible_plans: Vec<FalliblePlan>,
    if_plans: Vec<IfPlan>,
    prefix_plans: Vec<PrefixPlan>,
    binary_plans: Vec<BinaryPlan>,
    array_plans: Vec<ArrayPlan>,
    tuple_plans: Vec<TuplePlan>,
    range_plans: Vec<RangePlan>,
    map_plans: Vec<MapPlan>,
    fail_plans: Vec<FailPlan>,
    interpolated_string_plans: Vec<InterpolatedStringPlan>,
    struct_literal_plans: Vec<StructLiteralPlan>,
    paren_plans: Vec<ParenPlan>,
    closure_plans: Vec<ClosurePlan>,
    match_plans: Vec<MatchPlan>,
    expr_ids: BTreeMap<ProgramNodeHandle, ExprId>,
    block_bodies: Vec<BlockBody>,
    bytecode_bodies: Vec<BytecodeBody>,
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
        .with_resolved_call_callees()
        .with_expr_plans()
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

    pub(crate) fn resolve_expr_id(&self, id: impl Into<ExprId>) -> Result<&Expr, EvalError> {
        let id = id.into();
        self.source(id.source())?.resolve_expr(id)
    }

    #[cfg(test)]
    pub(crate) fn resolve_expr_plan(
        &self,
        id: impl Into<ExprId>,
    ) -> Result<ExprExecPlan, EvalError> {
        let id = id.into();
        self.source(id.source())?.resolve_expr_plan(id)
    }

    pub(crate) fn resolve_expr_body_ref(
        &self,
        id: impl Into<ExprId>,
    ) -> Result<ExprBodyRef, EvalError> {
        let id = id.into();
        self.source(id.source())?.resolve_expr_body_ref(id)
    }

    pub(crate) fn resolve_literal_plan(
        &self,
        id: LiteralPlanId,
    ) -> Result<&LiteralPlan, EvalError> {
        self.source(id.source())?.resolve_literal_plan(id)
    }

    pub(crate) fn resolve_path_literal_plan(
        &self,
        id: PathLiteralPlanId,
    ) -> Result<&PathLiteralPlan, EvalError> {
        self.source(id.source())?.resolve_path_literal_plan(id)
    }

    pub(crate) fn resolve_name_ref_plan(
        &self,
        id: NameRefPlanId,
    ) -> Result<&NameRefPlan, EvalError> {
        self.source(id.source())?.resolve_name_ref_plan(id)
    }

    pub(crate) fn resolve_direct_call_callee_plan(
        &self,
        id: DirectCallCalleePlanId,
    ) -> Result<&DirectCallCalleePlan, EvalError> {
        self.source(id.source())?
            .resolve_direct_call_callee_plan(id)
    }

    pub(crate) fn resolve_resolved_function_direct_call_callee_plan(
        &self,
        source: SourceId,
        index: u32,
    ) -> Result<&ResolvedFunctionDirectCallCalleePlan, EvalError> {
        self.source(source)?
            .resolve_resolved_function_direct_call_callee_plan(index)
    }

    pub(crate) fn resolve_variant_constructor_direct_call_callee_plan(
        &self,
        source: SourceId,
        index: u32,
    ) -> Result<&VariantConstructorDirectCallCalleePlan, EvalError> {
        self.source(source)?
            .resolve_variant_constructor_direct_call_callee_plan(index)
    }

    pub(crate) fn resolve_newtype_constructor_direct_call_callee_plan(
        &self,
        source: SourceId,
        index: u32,
    ) -> Result<&NewtypeConstructorDirectCallCalleePlan, EvalError> {
        self.source(source)?
            .resolve_newtype_constructor_direct_call_callee_plan(index)
    }

    pub(crate) fn resolve_static_member_direct_call_callee_plan(
        &self,
        source: SourceId,
        index: u32,
    ) -> Result<&StaticMemberDirectCallCalleePlan, EvalError> {
        self.source(source)?
            .resolve_static_member_direct_call_callee_plan(index)
    }

    pub(crate) fn resolve_call_callee_plan(
        &self,
        id: CallCalleePlanId,
    ) -> Result<&CallCalleePlan, EvalError> {
        self.source(id.source())?.resolve_call_callee_plan(id)
    }

    pub(crate) fn resolve_call_plan(&self, id: CallPlanId) -> Result<&CallPlan, EvalError> {
        self.source(id.source())?.resolve_call_plan(id)
    }

    pub(crate) fn resolve_field_plan(&self, id: FieldPlanId) -> Result<&FieldPlan, EvalError> {
        self.source(id.source())?.resolve_field_plan(id)
    }

    pub(crate) fn resolve_where_plan(&self, id: WherePlanId) -> Result<&WherePlan, EvalError> {
        self.source(id.source())?.resolve_where_plan(id)
    }

    pub(crate) fn resolve_fallible_plan(
        &self,
        id: FalliblePlanId,
    ) -> Result<&FalliblePlan, EvalError> {
        self.source(id.source())?.resolve_fallible_plan(id)
    }

    pub(crate) fn resolve_if_plan(&self, id: IfPlanId) -> Result<&IfPlan, EvalError> {
        self.source(id.source())?.resolve_if_plan(id)
    }

    pub(crate) fn resolve_prefix_plan(&self, id: PrefixPlanId) -> Result<&PrefixPlan, EvalError> {
        self.source(id.source())?.resolve_prefix_plan(id)
    }

    pub(crate) fn resolve_binary_plan(&self, id: BinaryPlanId) -> Result<&BinaryPlan, EvalError> {
        self.source(id.source())?.resolve_binary_plan(id)
    }

    pub(crate) fn resolve_array_plan(&self, id: ArrayPlanId) -> Result<&ArrayPlan, EvalError> {
        self.source(id.source())?.resolve_array_plan(id)
    }

    pub(crate) fn resolve_tuple_plan(&self, id: TuplePlanId) -> Result<&TuplePlan, EvalError> {
        self.source(id.source())?.resolve_tuple_plan(id)
    }

    pub(crate) fn resolve_range_plan(&self, id: RangePlanId) -> Result<&RangePlan, EvalError> {
        self.source(id.source())?.resolve_range_plan(id)
    }

    pub(crate) fn resolve_map_plan(&self, id: MapPlanId) -> Result<&MapPlan, EvalError> {
        self.source(id.source())?.resolve_map_plan(id)
    }

    pub(crate) fn resolve_fail_plan(&self, id: FailPlanId) -> Result<&FailPlan, EvalError> {
        self.source(id.source())?.resolve_fail_plan(id)
    }

    pub(crate) fn resolve_interpolated_string_plan(
        &self,
        id: InterpolatedStringPlanId,
    ) -> Result<&InterpolatedStringPlan, EvalError> {
        self.source(id.source())?
            .resolve_interpolated_string_plan(id)
    }

    pub(crate) fn resolve_struct_literal_plan(
        &self,
        id: StructLiteralPlanId,
    ) -> Result<&StructLiteralPlan, EvalError> {
        self.source(id.source())?.resolve_struct_literal_plan(id)
    }

    pub(crate) fn resolve_paren_plan(&self, id: ParenPlanId) -> Result<&ParenPlan, EvalError> {
        self.source(id.source())?.resolve_paren_plan(id)
    }

    pub(crate) fn resolve_closure_plan(
        &self,
        id: ClosurePlanId,
    ) -> Result<&ClosurePlan, EvalError> {
        self.source(id.source())?.resolve_closure_plan(id)
    }

    pub(crate) fn resolve_match_plan(&self, id: MatchPlanId) -> Result<&MatchPlan, EvalError> {
        self.source(id.source())?.resolve_match_plan(id)
    }

    pub(crate) fn resolve_expr(&self, key: impl IntoExprLookupId) -> Result<&Expr, EvalError> {
        let id = self.expr_id(key)?;
        self.resolve_expr_id(id)
    }

    pub(crate) fn resolve_block_body(&self, id: BlockBodyId) -> Result<&BlockBody, EvalError> {
        self.source(id.source())?.resolve_block_body(id)
    }

    pub(crate) fn resolve_bytecode_body(
        &self,
        id: BytecodeBodyId,
    ) -> Result<&BytecodeBody, EvalError> {
        self.source(id.source())?.resolve_bytecode_body(id)
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

    fn with_resolved_call_callees(mut self) -> Self {
        let source_ids = self.sources.keys().copied().collect::<Vec<_>>();
        for source_key in source_ids {
            let source_id = SourceId(source_key);
            let rewrites = self
                .collect_execution_ref_rewrites(source_id)
                .expect("compiled execution refs should resolve");
            let source = self
                .sources
                .get_mut(&source_key)
                .expect("compiled source should exist for execution ref rewrites");
            source
                .apply_call_callee_rewrites(rewrites.call_callees)
                .expect("compiled call callee rewrites should target call expressions");
            source
                .apply_expr_ref_rewrites(rewrites.expr_refs)
                .expect("compiled expression ref rewrites should target name or field expressions");
            source
                .apply_where_call_rewrites(rewrites.where_calls)
                .expect("compiled where call rewrites should target where expressions");
        }
        self
    }

    fn with_expr_plans(mut self) -> Self {
        for source in self.sources.values_mut() {
            source.rebuild_expr_plans();
        }
        self
    }

    fn collect_execution_ref_rewrites(
        &self,
        source_id: SourceId,
    ) -> Result<ExecutionRefRewrites, EvalError> {
        let source = self.source(source_id)?;
        source.ensure_parsed()?;
        let mut call_callees = Vec::new();
        let mut expr_refs = Vec::new();
        let mut where_calls = Vec::new();
        for (index, expr) in source.exprs.iter().enumerate() {
            let expr_id = ExprId {
                source: source_id.0,
                index: index as u32,
            };
            match expr.kind() {
                ExprKind::Call(call) => {
                    let Some(callee) = call.callee.as_ref() else {
                        continue;
                    };
                    let resolved = self.resolve_call_callee(callee)?;
                    if resolved != *callee {
                        call_callees.push((expr_id, resolved));
                    }
                }
                ExprKind::NameRef(name_ref) => {
                    if let Some(resolved) = self.resolve_name_ref_expr(name_ref)? {
                        expr_refs.push(ExprRefRewrite::NameRef { expr_id, resolved });
                    }
                }
                ExprKind::Field(field) => {
                    if let Some(static_member) = self.resolve_static_member_expr(field)? {
                        expr_refs.push(ExprRefRewrite::Field {
                            expr_id,
                            static_member,
                        });
                    }
                }
                ExprKind::Where(where_expr) => {
                    let Some(call_expr_id) = where_expr.call_expr else {
                        continue;
                    };
                    let call_expr = self.resolve_expr_id(call_expr_id)?;
                    let ExprKind::Call(call) = call_expr.kind() else {
                        continue;
                    };
                    let mut call = call.clone();
                    if let Some(callee) = call.callee.as_ref() {
                        call.callee = Some(self.resolve_call_callee(callee)?);
                    }
                    where_calls.push((expr_id, call));
                }
                _ => {}
            }
        }
        Ok(ExecutionRefRewrites {
            call_callees,
            expr_refs,
            where_calls,
        })
    }

    fn resolve_call_callee(&self, callee: &CallCallee) -> Result<CallCallee, EvalError> {
        match callee {
            CallCallee::NameRef(name_ref) => self.resolve_name_call_callee(name_ref),
            CallCallee::Field(field) => self.resolve_field_call_callee(field),
            other => Ok(other.clone()),
        }
    }

    fn resolve_name_call_callee(&self, name_ref: &NameRefExpr) -> Result<CallCallee, EvalError> {
        match self.resolve_name_ref_expr(name_ref)? {
            Some(ResolvedNameRef::Function {
                resolved_name,
                function,
            }) => Ok(CallCallee::ResolvedFunction {
                resolved_name,
                function,
            }),
            Some(ResolvedNameRef::VariantConstructor(constructor)) => {
                Ok(CallCallee::VariantConstructor {
                    handle: name_ref.handle,
                    constructor,
                })
            }
            Some(ResolvedNameRef::NewtypeConstructor {
                display_name,
                newtype_name,
            }) => Ok(CallCallee::NewtypeConstructor {
                handle: name_ref.handle,
                display_name,
                newtype_name,
            }),
            None => Ok(CallCallee::NameRef(name_ref.clone())),
        }
    }

    fn resolve_field_call_callee(&self, field: &FieldExpr) -> Result<CallCallee, EvalError> {
        match self.resolve_static_member_expr(field)? {
            Some(static_member) => Ok(CallCallee::StaticMember {
                handle: field.handle,
                full_name: static_member.full_name,
                variant_constructor: static_member.variant_constructor,
            }),
            None => Ok(CallCallee::Field(field.clone())),
        }
    }

    fn resolve_name_ref_expr(
        &self,
        name_ref: &NameRefExpr,
    ) -> Result<Option<ResolvedNameRef>, EvalError> {
        let Some(name_token) = name_ref.token.as_ref() else {
            return Ok(None);
        };
        let reference = name_token.handle();
        let name = name_ref.name.as_ref();
        if let Some((resolved_name, function)) = self.resolve_function_reference(reference, name)? {
            return Ok(Some(ResolvedNameRef::Function {
                resolved_name,
                function,
            }));
        }
        let constructor_name = name.rsplit('.').next().unwrap_or(name);
        if let Some(constructor) = self.resolve_variant_constructor(constructor_name) {
            return Ok(Some(ResolvedNameRef::VariantConstructor(constructor)));
        }
        if let Some(newtype_name) = self.resolve_newtype_constructor(constructor_name) {
            return Ok(Some(ResolvedNameRef::NewtypeConstructor {
                display_name: name_ref.name.clone(),
                newtype_name,
            }));
        }
        Ok(None)
    }

    fn resolve_static_member_expr(
        &self,
        field: &FieldExpr,
    ) -> Result<Option<StaticMemberPlan>, EvalError> {
        let Some(receiver) = field.static_receiver.as_ref() else {
            return Ok(None);
        };
        if let Some(scope) = self
            .scopes
            .scope_at(receiver.handle.source(), receiver.handle.to_range().start())
        {
            if let Some(definition) = self.scopes.lookup_at(
                receiver.handle.source(),
                receiver.name.as_ref(),
                scope,
                receiver.handle.to_range().start(),
            ) {
                if matches!(
                    definition.kind,
                    DefKind::Binding | DefKind::Parameter | DefKind::Function
                ) {
                    return Ok(None);
                }
            }
        }
        let Some(field_name) = field.field_name.as_ref() else {
            return Ok(None);
        };
        Ok(Some(StaticMemberPlan {
            full_name: Arc::<str>::from(format!("{}.{}", receiver.name, field_name)),
            variant_constructor: self.resolve_variant_constructor(field_name),
        }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExprRefRewrite {
    NameRef {
        expr_id: ExprId,
        resolved: ResolvedNameRef,
    },
    Field {
        expr_id: ExprId,
        static_member: StaticMemberPlan,
    },
}

#[derive(Debug, Default)]
struct ExecutionRefRewrites {
    call_callees: Vec<(ExprId, CallCallee)>,
    expr_refs: Vec<ExprRefRewrite>,
    where_calls: Vec<(ExprId, CallExpr)>,
}

impl CompiledProgramSource {
    fn new(source: SourceId, program_source: &EvalProgramSource) -> Self {
        let source_id = Arc::clone(&program_source.source_id);
        let (_source_text, line_index, parse) =
            normalized_parse(program_source.source_text.as_ref());

        let mut functions_by_name = BTreeMap::new();
        let mut functions_by_definition = BTreeMap::new();
        let mut exprs = Vec::new();
        let expr_plans = Vec::new();
        let literal_plans = Vec::new();
        let path_literal_plans = Vec::new();
        let name_ref_plans = Vec::new();
        let direct_call_callee_plans = Vec::new();
        let resolved_function_direct_call_callee_plans = Vec::new();
        let variant_constructor_direct_call_callee_plans = Vec::new();
        let newtype_constructor_direct_call_callee_plans = Vec::new();
        let static_member_direct_call_callee_plans = Vec::new();
        let call_callee_plans = Vec::new();
        let call_plans = Vec::new();
        let field_plans = Vec::new();
        let where_plans = Vec::new();
        let fallible_plans = Vec::new();
        let if_plans = Vec::new();
        let prefix_plans = Vec::new();
        let binary_plans = Vec::new();
        let array_plans = Vec::new();
        let tuple_plans = Vec::new();
        let range_plans = Vec::new();
        let map_plans = Vec::new();
        let fail_plans = Vec::new();
        let interpolated_string_plans = Vec::new();
        let struct_literal_plans = Vec::new();
        let paren_plans = Vec::new();
        let closure_plans = Vec::new();
        let match_plans = Vec::new();
        let bytecode_bodies = Vec::new();
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
            expr_plans,
            literal_plans,
            path_literal_plans,
            name_ref_plans,
            direct_call_callee_plans,
            resolved_function_direct_call_callee_plans,
            variant_constructor_direct_call_callee_plans,
            newtype_constructor_direct_call_callee_plans,
            static_member_direct_call_callee_plans,
            call_callee_plans,
            call_plans,
            field_plans,
            where_plans,
            fallible_plans,
            if_plans,
            prefix_plans,
            binary_plans,
            array_plans,
            tuple_plans,
            range_plans,
            map_plans,
            fail_plans,
            interpolated_string_plans,
            struct_literal_plans,
            paren_plans,
            closure_plans,
            match_plans,
            expr_ids,
            block_bodies,
            bytecode_bodies,
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

    fn apply_call_callee_rewrites(
        &mut self,
        rewrites: Vec<(ExprId, CallCallee)>,
    ) -> Result<(), EvalError> {
        for (expr_id, callee) in rewrites {
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
                        "call callee rewrite targeted non-call expression {:?}",
                        expr_id
                    ),
                ));
            };
            call.callee = Some(callee);
        }
        Ok(())
    }

    fn apply_expr_ref_rewrites(&mut self, rewrites: Vec<ExprRefRewrite>) -> Result<(), EvalError> {
        for rewrite in rewrites {
            match rewrite {
                ExprRefRewrite::NameRef { expr_id, resolved } => {
                    let expr = self.exprs.get_mut(expr_id.index as usize).ok_or_else(|| {
                        EvalError::new(
                            EvalErrorKind::Internal,
                            format!("expression id {:?} was not found", expr_id),
                        )
                    })?;
                    let ExprKind::NameRef(name_ref) = &mut expr.kind else {
                        return Err(EvalError::new(
                            EvalErrorKind::Internal,
                            format!(
                                "expression ref rewrite targeted non-name expression {:?}",
                                expr_id
                            ),
                        ));
                    };
                    name_ref.resolved = Some(resolved);
                }
                ExprRefRewrite::Field {
                    expr_id,
                    static_member,
                } => {
                    let expr = self.exprs.get_mut(expr_id.index as usize).ok_or_else(|| {
                        EvalError::new(
                            EvalErrorKind::Internal,
                            format!("expression id {:?} was not found", expr_id),
                        )
                    })?;
                    let ExprKind::Field(field) = &mut expr.kind else {
                        return Err(EvalError::new(
                            EvalErrorKind::Internal,
                            format!(
                                "expression ref rewrite targeted non-field expression {:?}",
                                expr_id
                            ),
                        ));
                    };
                    field.static_member = Some(static_member);
                }
            }
        }
        Ok(())
    }

    fn apply_where_call_rewrites(
        &mut self,
        rewrites: Vec<(ExprId, CallExpr)>,
    ) -> Result<(), EvalError> {
        for (expr_id, call) in rewrites {
            let expr = self.exprs.get_mut(expr_id.index as usize).ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    format!("expression id {:?} was not found", expr_id),
                )
            })?;
            let ExprKind::Where(where_expr) = &mut expr.kind else {
                return Err(EvalError::new(
                    EvalErrorKind::Internal,
                    format!(
                        "where call rewrite targeted non-where expression {:?}",
                        expr_id
                    ),
                ));
            };
            where_expr.call = Some(call);
        }
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

    fn resolve_expr_plan(&self, id: ExprId) -> Result<ExprExecPlan, EvalError> {
        self.ensure_parsed()?;
        self.expr_plans
            .get(id.index as usize)
            .cloned()
            .ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    format!("expression plan {:?} was not found", id),
                )
            })
    }

    fn resolve_expr_body_ref(&self, id: ExprId) -> Result<ExprBodyRef, EvalError> {
        Ok(ExprBodyRef {
            source: id.source,
            index: id.index,
            plan: self.resolve_expr_plan(id)?,
        })
    }

    fn resolve_literal_plan(&self, id: LiteralPlanId) -> Result<&LiteralPlan, EvalError> {
        self.ensure_parsed()?;
        self.literal_plans.get(id.index as usize).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("literal plan {:?} was not found", id),
            )
        })
    }

    fn resolve_path_literal_plan(
        &self,
        id: PathLiteralPlanId,
    ) -> Result<&PathLiteralPlan, EvalError> {
        self.ensure_parsed()?;
        self.path_literal_plans
            .get(id.index as usize)
            .ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    format!("path literal plan {:?} was not found", id),
                )
            })
    }

    fn resolve_name_ref_plan(&self, id: NameRefPlanId) -> Result<&NameRefPlan, EvalError> {
        self.ensure_parsed()?;
        self.name_ref_plans.get(id.index as usize).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("name ref plan {:?} was not found", id),
            )
        })
    }

    fn resolve_direct_call_callee_plan(
        &self,
        id: DirectCallCalleePlanId,
    ) -> Result<&DirectCallCalleePlan, EvalError> {
        self.ensure_parsed()?;
        self.direct_call_callee_plans
            .get(id.index as usize)
            .ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    format!("direct call callee plan {:?} was not found", id),
                )
            })
    }

    fn resolve_resolved_function_direct_call_callee_plan(
        &self,
        index: u32,
    ) -> Result<&ResolvedFunctionDirectCallCalleePlan, EvalError> {
        self.ensure_parsed()?;
        self.resolved_function_direct_call_callee_plans
            .get(index as usize)
            .ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    format!("resolved function direct call callee plan #{index} was not found"),
                )
            })
    }

    fn resolve_variant_constructor_direct_call_callee_plan(
        &self,
        index: u32,
    ) -> Result<&VariantConstructorDirectCallCalleePlan, EvalError> {
        self.ensure_parsed()?;
        self.variant_constructor_direct_call_callee_plans
            .get(index as usize)
            .ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    format!("variant constructor direct call callee plan #{index} was not found"),
                )
            })
    }

    fn resolve_newtype_constructor_direct_call_callee_plan(
        &self,
        index: u32,
    ) -> Result<&NewtypeConstructorDirectCallCalleePlan, EvalError> {
        self.ensure_parsed()?;
        self.newtype_constructor_direct_call_callee_plans
            .get(index as usize)
            .ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    format!("newtype direct call callee plan #{index} was not found"),
                )
            })
    }

    fn resolve_static_member_direct_call_callee_plan(
        &self,
        index: u32,
    ) -> Result<&StaticMemberDirectCallCalleePlan, EvalError> {
        self.ensure_parsed()?;
        self.static_member_direct_call_callee_plans
            .get(index as usize)
            .ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    format!("static member direct call callee plan #{index} was not found"),
                )
            })
    }

    fn resolve_call_callee_plan(&self, id: CallCalleePlanId) -> Result<&CallCalleePlan, EvalError> {
        self.ensure_parsed()?;
        self.call_callee_plans
            .get(id.index as usize)
            .ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    format!("call callee plan {:?} was not found", id),
                )
            })
    }

    fn resolve_call_plan(&self, id: CallPlanId) -> Result<&CallPlan, EvalError> {
        self.ensure_parsed()?;
        self.call_plans.get(id.index as usize).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("call plan {:?} was not found", id),
            )
        })
    }

    fn resolve_field_plan(&self, id: FieldPlanId) -> Result<&FieldPlan, EvalError> {
        self.ensure_parsed()?;
        self.field_plans.get(id.index as usize).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("field plan {:?} was not found", id),
            )
        })
    }

    fn resolve_where_plan(&self, id: WherePlanId) -> Result<&WherePlan, EvalError> {
        self.ensure_parsed()?;
        self.where_plans.get(id.index as usize).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("where plan {:?} was not found", id),
            )
        })
    }

    fn resolve_fallible_plan(&self, id: FalliblePlanId) -> Result<&FalliblePlan, EvalError> {
        self.ensure_parsed()?;
        self.fallible_plans.get(id.index as usize).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("fallible plan {:?} was not found", id),
            )
        })
    }

    fn resolve_if_plan(&self, id: IfPlanId) -> Result<&IfPlan, EvalError> {
        self.ensure_parsed()?;
        self.if_plans.get(id.index as usize).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("if plan {:?} was not found", id),
            )
        })
    }

    fn resolve_prefix_plan(&self, id: PrefixPlanId) -> Result<&PrefixPlan, EvalError> {
        self.ensure_parsed()?;
        self.prefix_plans.get(id.index as usize).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("prefix plan {:?} was not found", id),
            )
        })
    }

    fn resolve_binary_plan(&self, id: BinaryPlanId) -> Result<&BinaryPlan, EvalError> {
        self.ensure_parsed()?;
        self.binary_plans.get(id.index as usize).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("binary plan {:?} was not found", id),
            )
        })
    }

    fn resolve_array_plan(&self, id: ArrayPlanId) -> Result<&ArrayPlan, EvalError> {
        self.ensure_parsed()?;
        self.array_plans.get(id.index as usize).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("array plan {:?} was not found", id),
            )
        })
    }

    fn resolve_tuple_plan(&self, id: TuplePlanId) -> Result<&TuplePlan, EvalError> {
        self.ensure_parsed()?;
        self.tuple_plans.get(id.index as usize).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("tuple plan {:?} was not found", id),
            )
        })
    }

    fn resolve_range_plan(&self, id: RangePlanId) -> Result<&RangePlan, EvalError> {
        self.ensure_parsed()?;
        self.range_plans.get(id.index as usize).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("range plan {:?} was not found", id),
            )
        })
    }

    fn resolve_map_plan(&self, id: MapPlanId) -> Result<&MapPlan, EvalError> {
        self.ensure_parsed()?;
        self.map_plans.get(id.index as usize).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("map plan {:?} was not found", id),
            )
        })
    }

    fn resolve_fail_plan(&self, id: FailPlanId) -> Result<&FailPlan, EvalError> {
        self.ensure_parsed()?;
        self.fail_plans.get(id.index as usize).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("fail plan {:?} was not found", id),
            )
        })
    }

    fn resolve_interpolated_string_plan(
        &self,
        id: InterpolatedStringPlanId,
    ) -> Result<&InterpolatedStringPlan, EvalError> {
        self.ensure_parsed()?;
        self.interpolated_string_plans
            .get(id.index as usize)
            .ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    format!("interpolated string plan {:?} was not found", id),
                )
            })
    }

    fn resolve_struct_literal_plan(
        &self,
        id: StructLiteralPlanId,
    ) -> Result<&StructLiteralPlan, EvalError> {
        self.ensure_parsed()?;
        self.struct_literal_plans
            .get(id.index as usize)
            .ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    format!("struct literal plan {:?} was not found", id),
                )
            })
    }

    fn resolve_paren_plan(&self, id: ParenPlanId) -> Result<&ParenPlan, EvalError> {
        self.ensure_parsed()?;
        self.paren_plans.get(id.index as usize).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("paren plan {:?} was not found", id),
            )
        })
    }

    fn resolve_closure_plan(&self, id: ClosurePlanId) -> Result<&ClosurePlan, EvalError> {
        self.ensure_parsed()?;
        self.closure_plans.get(id.index as usize).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("closure plan {:?} was not found", id),
            )
        })
    }

    fn resolve_match_plan(&self, id: MatchPlanId) -> Result<&MatchPlan, EvalError> {
        self.ensure_parsed()?;
        self.match_plans.get(id.index as usize).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("match plan {:?} was not found", id),
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

    fn resolve_bytecode_body(&self, id: BytecodeBodyId) -> Result<&BytecodeBody, EvalError> {
        self.ensure_parsed()?;
        self.bytecode_bodies.get(id.index as usize).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("bytecode body id {:?} was not found", id),
            )
        })
    }

    fn source_id(&self) -> Arc<str> {
        Arc::clone(&self.source_id)
    }

    fn rebuild_expr_plans(&mut self) {
        let mut plans = ExprPlanTables::default();
        self.expr_plans = self
            .exprs
            .iter()
            .map(|expr| compile_expr_plan(expr, &mut plans))
            .collect();
        self.literal_plans = plans.literal_plans;
        self.path_literal_plans = plans.path_literal_plans;
        self.name_ref_plans = plans.name_ref_plans;
        self.direct_call_callee_plans = plans.direct_call_callee_plans;
        self.resolved_function_direct_call_callee_plans =
            plans.resolved_function_direct_call_callee_plans;
        self.variant_constructor_direct_call_callee_plans =
            plans.variant_constructor_direct_call_callee_plans;
        self.newtype_constructor_direct_call_callee_plans =
            plans.newtype_constructor_direct_call_callee_plans;
        self.static_member_direct_call_callee_plans = plans.static_member_direct_call_callee_plans;
        self.call_callee_plans = plans.call_callee_plans;
        self.call_plans = plans.call_plans;
        self.field_plans = plans.field_plans;
        self.where_plans = plans.where_plans;
        self.fallible_plans = plans.fallible_plans;
        self.if_plans = plans.if_plans;
        self.prefix_plans = plans.prefix_plans;
        self.binary_plans = plans.binary_plans;
        self.array_plans = plans.array_plans;
        self.tuple_plans = plans.tuple_plans;
        self.range_plans = plans.range_plans;
        self.map_plans = plans.map_plans;
        self.fail_plans = plans.fail_plans;
        self.interpolated_string_plans = plans.interpolated_string_plans;
        self.struct_literal_plans = plans.struct_literal_plans;
        self.paren_plans = plans.paren_plans;
        self.closure_plans = plans.closure_plans;
        self.match_plans = plans.match_plans;
        self.resolve_plan_child_body_refs();
        self.rebuild_bytecode_bodies();
    }

    fn resolve_plan_child_body_refs(&mut self) {
        let resolve = |body: ExprBodyRef, expr_plans: &[ExprExecPlan]| -> ExprBodyRef {
            body.with_plan(
                *expr_plans
                    .get(body.index as usize)
                    .expect("child body ref should resolve to a cached plan"),
            )
        };
        let resolve_function = |function: &mut CompiledFunction, expr_plans: &[ExprExecPlan]| {
            function.body = function.body.map(|body| resolve(body, expr_plans));
        };

        for function in self.functions_by_name.values_mut() {
            resolve_function(function, &self.expr_plans);
        }
        for (_name, function) in self.functions_by_definition.values_mut() {
            resolve_function(function, &self.expr_plans);
        }
        for body in &mut self.block_bodies {
            body.steps = Arc::from(
                body.steps
                    .iter()
                    .cloned()
                    .map(|step| match step {
                        BlockStep::Let { pattern, expr } => BlockStep::Let {
                            pattern,
                            expr: resolve(expr, &self.expr_plans),
                        },
                        BlockStep::FnDecl(mut function) => {
                            resolve_function(&mut function.compiled, &self.expr_plans);
                            BlockStep::FnDecl(function)
                        }
                        BlockStep::Expr(expr) => BlockStep::Expr(resolve(expr, &self.expr_plans)),
                    })
                    .collect::<Vec<_>>(),
            );
        }

        for plan in &mut self.call_plans {
            plan.positional_args = Arc::from(
                plan.positional_args
                    .iter()
                    .copied()
                    .map(|body| resolve(body, &self.expr_plans))
                    .collect::<Vec<_>>(),
            );
        }
        for plan in &mut self.field_plans {
            plan.receiver = plan.receiver.map(|body| resolve(body, &self.expr_plans));
        }
        for plan in &mut self.where_plans {
            plan.keyword_bindings = Arc::from(
                plan.keyword_bindings
                    .iter()
                    .cloned()
                    .map(|binding| KeywordBindingPlan {
                        name: binding.name,
                        expr: binding.expr.map(|body| resolve(body, &self.expr_plans)),
                    })
                    .collect::<Vec<_>>(),
            );
        }
        for plan in &mut self.fail_plans {
            plan.message = plan.message.map(|body| resolve(body, &self.expr_plans));
        }
        for plan in &mut self.fallible_plans {
            plan.expr = plan.expr.map(|body| resolve(body, &self.expr_plans));
        }
        for plan in &mut self.if_plans {
            plan.condition = plan.condition.map(|body| resolve(body, &self.expr_plans));
            plan.then_branch = plan.then_branch.map(|body| resolve(body, &self.expr_plans));
            plan.else_branch = plan.else_branch.map(|body| resolve(body, &self.expr_plans));
        }
        for plan in &mut self.prefix_plans {
            plan.expr = plan.expr.map(|body| resolve(body, &self.expr_plans));
        }
        for plan in &mut self.binary_plans {
            plan.lhs = plan.lhs.map(|body| resolve(body, &self.expr_plans));
            plan.rhs = plan.rhs.map(|body| resolve(body, &self.expr_plans));
        }
        for plan in &mut self.array_plans {
            plan.items = Arc::from(
                plan.items
                    .iter()
                    .cloned()
                    .map(|item| match item {
                        ArrayPlanItem::Expr(body) => {
                            ArrayPlanItem::Expr(resolve(body, &self.expr_plans))
                        }
                        ArrayPlanItem::Spread(body) => {
                            ArrayPlanItem::Spread(resolve(body, &self.expr_plans))
                        }
                    })
                    .collect::<Vec<_>>(),
            );
        }
        for plan in &mut self.tuple_plans {
            plan.items = Arc::from(
                plan.items
                    .iter()
                    .copied()
                    .map(|body| resolve(body, &self.expr_plans))
                    .collect::<Vec<_>>(),
            );
        }
        for plan in &mut self.range_plans {
            plan.start = plan.start.map(|body| resolve(body, &self.expr_plans));
            plan.end = plan.end.map(|body| resolve(body, &self.expr_plans));
        }
        for plan in &mut self.map_plans {
            plan.entries = Arc::from(
                plan.entries
                    .iter()
                    .cloned()
                    .map(|entry| MapEntryPlan {
                        key: resolve(entry.key, &self.expr_plans),
                        value: resolve(entry.value, &self.expr_plans),
                    })
                    .collect::<Vec<_>>(),
            );
        }
        for plan in &mut self.interpolated_string_plans {
            plan.parts = Arc::from(
                plan.parts
                    .iter()
                    .cloned()
                    .map(|part| match part {
                        InterpolatedStringPlanPart::Text(text) => {
                            InterpolatedStringPlanPart::Text(text)
                        }
                        InterpolatedStringPlanPart::Interpolation(body) => {
                            InterpolatedStringPlanPart::Interpolation(resolve(
                                body,
                                &self.expr_plans,
                            ))
                        }
                    })
                    .collect::<Vec<_>>(),
            );
        }
        for plan in &mut self.struct_literal_plans {
            plan.parts = Arc::from(
                plan.parts
                    .iter()
                    .cloned()
                    .map(|part| match part {
                        StructLiteralPlanPart::Field { name, expr } => {
                            StructLiteralPlanPart::Field {
                                name,
                                expr: expr.map(|body| resolve(body, &self.expr_plans)),
                            }
                        }
                        StructLiteralPlanPart::Spread(body) => {
                            StructLiteralPlanPart::Spread(resolve(body, &self.expr_plans))
                        }
                    })
                    .collect::<Vec<_>>(),
            );
        }
        for plan in &mut self.paren_plans {
            plan.expr = plan.expr.map(|body| resolve(body, &self.expr_plans));
        }
        for plan in &mut self.closure_plans {
            plan.body = plan.body.map(|body| resolve(body, &self.expr_plans));
        }
        for plan in &mut self.match_plans {
            plan.scrutinee = plan.scrutinee.map(|body| resolve(body, &self.expr_plans));
            plan.arms = Arc::from(
                plan.arms
                    .iter()
                    .cloned()
                    .map(|arm| MatchArmPlan {
                        pattern: arm.pattern,
                        body: resolve(arm.body, &self.expr_plans),
                    })
                    .collect::<Vec<_>>(),
            );
        }
    }

    fn rebuild_bytecode_bodies(&mut self) {
        self.bytecode_bodies = self
            .block_bodies
            .iter()
            .map(|block| compile_bytecode_body(self, block))
            .collect();
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

impl ExprPlanTables {
    fn push_literal(&mut self, source: SourceId, plan: LiteralPlan) -> LiteralPlanId {
        let id = LiteralPlanId {
            source: source.0,
            index: self.literal_plans.len() as u32,
        };
        self.literal_plans.push(plan);
        id
    }

    fn push_path_literal(&mut self, source: SourceId, plan: PathLiteralPlan) -> PathLiteralPlanId {
        let id = PathLiteralPlanId {
            source: source.0,
            index: self.path_literal_plans.len() as u32,
        };
        self.path_literal_plans.push(plan);
        id
    }

    fn push_name_ref(&mut self, source: SourceId, plan: NameRefPlan) -> NameRefPlanId {
        let id = NameRefPlanId {
            source: source.0,
            index: self.name_ref_plans.len() as u32,
        };
        self.name_ref_plans.push(plan);
        id
    }

    fn push_direct_call_callee(
        &mut self,
        source: SourceId,
        plan: DirectCallCalleePlan,
    ) -> DirectCallCalleePlanId {
        let id = DirectCallCalleePlanId {
            source: source.0,
            index: self.direct_call_callee_plans.len() as u32,
        };
        self.direct_call_callee_plans.push(plan);
        id
    }

    fn push_resolved_function_direct_call_callee(
        &mut self,
        source: SourceId,
        plan: ResolvedFunctionDirectCallCalleePlan,
    ) -> u32 {
        let index = self.resolved_function_direct_call_callee_plans.len() as u32;
        let _ = source;
        self.resolved_function_direct_call_callee_plans.push(plan);
        index
    }

    fn push_variant_constructor_direct_call_callee(
        &mut self,
        source: SourceId,
        plan: VariantConstructorDirectCallCalleePlan,
    ) -> u32 {
        let index = self.variant_constructor_direct_call_callee_plans.len() as u32;
        let _ = source;
        self.variant_constructor_direct_call_callee_plans.push(plan);
        index
    }

    fn push_newtype_constructor_direct_call_callee(
        &mut self,
        source: SourceId,
        plan: NewtypeConstructorDirectCallCalleePlan,
    ) -> u32 {
        let index = self.newtype_constructor_direct_call_callee_plans.len() as u32;
        let _ = source;
        self.newtype_constructor_direct_call_callee_plans.push(plan);
        index
    }

    fn push_static_member_direct_call_callee(
        &mut self,
        source: SourceId,
        plan: StaticMemberDirectCallCalleePlan,
    ) -> u32 {
        let index = self.static_member_direct_call_callee_plans.len() as u32;
        let _ = source;
        self.static_member_direct_call_callee_plans.push(plan);
        index
    }

    fn push_call_callee(&mut self, source: SourceId, plan: CallCalleePlan) -> CallCalleePlanId {
        let id = CallCalleePlanId {
            source: source.0,
            index: self.call_callee_plans.len() as u32,
        };
        self.call_callee_plans.push(plan);
        id
    }

    fn push_call(&mut self, source: SourceId, plan: CallPlan) -> CallPlanId {
        let id = CallPlanId {
            source: source.0,
            index: self.call_plans.len() as u32,
        };
        self.call_plans.push(plan);
        id
    }

    fn push_field(&mut self, source: SourceId, plan: FieldPlan) -> FieldPlanId {
        let id = FieldPlanId {
            source: source.0,
            index: self.field_plans.len() as u32,
        };
        self.field_plans.push(plan);
        id
    }

    fn push_where(&mut self, source: SourceId, plan: WherePlan) -> WherePlanId {
        let id = WherePlanId {
            source: source.0,
            index: self.where_plans.len() as u32,
        };
        self.where_plans.push(plan);
        id
    }

    fn push_fallible(&mut self, source: SourceId, plan: FalliblePlan) -> FalliblePlanId {
        let id = FalliblePlanId {
            source: source.0,
            index: self.fallible_plans.len() as u32,
        };
        self.fallible_plans.push(plan);
        id
    }

    fn push_if(&mut self, source: SourceId, plan: IfPlan) -> IfPlanId {
        let id = IfPlanId {
            source: source.0,
            index: self.if_plans.len() as u32,
        };
        self.if_plans.push(plan);
        id
    }

    fn push_prefix(&mut self, source: SourceId, plan: PrefixPlan) -> PrefixPlanId {
        let id = PrefixPlanId {
            source: source.0,
            index: self.prefix_plans.len() as u32,
        };
        self.prefix_plans.push(plan);
        id
    }

    fn push_binary(&mut self, source: SourceId, plan: BinaryPlan) -> BinaryPlanId {
        let id = BinaryPlanId {
            source: source.0,
            index: self.binary_plans.len() as u32,
        };
        self.binary_plans.push(plan);
        id
    }

    fn push_array(&mut self, source: SourceId, plan: ArrayPlan) -> ArrayPlanId {
        let id = ArrayPlanId {
            source: source.0,
            index: self.array_plans.len() as u32,
        };
        self.array_plans.push(plan);
        id
    }

    fn push_tuple(&mut self, source: SourceId, plan: TuplePlan) -> TuplePlanId {
        let id = TuplePlanId {
            source: source.0,
            index: self.tuple_plans.len() as u32,
        };
        self.tuple_plans.push(plan);
        id
    }

    fn push_range(&mut self, source: SourceId, plan: RangePlan) -> RangePlanId {
        let id = RangePlanId {
            source: source.0,
            index: self.range_plans.len() as u32,
        };
        self.range_plans.push(plan);
        id
    }

    fn push_map(&mut self, source: SourceId, plan: MapPlan) -> MapPlanId {
        let id = MapPlanId {
            source: source.0,
            index: self.map_plans.len() as u32,
        };
        self.map_plans.push(plan);
        id
    }

    fn push_fail(&mut self, source: SourceId, plan: FailPlan) -> FailPlanId {
        let id = FailPlanId {
            source: source.0,
            index: self.fail_plans.len() as u32,
        };
        self.fail_plans.push(plan);
        id
    }

    fn push_interpolated_string(
        &mut self,
        source: SourceId,
        plan: InterpolatedStringPlan,
    ) -> InterpolatedStringPlanId {
        let id = InterpolatedStringPlanId {
            source: source.0,
            index: self.interpolated_string_plans.len() as u32,
        };
        self.interpolated_string_plans.push(plan);
        id
    }

    fn push_struct_literal(
        &mut self,
        source: SourceId,
        plan: StructLiteralPlan,
    ) -> StructLiteralPlanId {
        let id = StructLiteralPlanId {
            source: source.0,
            index: self.struct_literal_plans.len() as u32,
        };
        self.struct_literal_plans.push(plan);
        id
    }

    fn push_paren(&mut self, source: SourceId, plan: ParenPlan) -> ParenPlanId {
        let id = ParenPlanId {
            source: source.0,
            index: self.paren_plans.len() as u32,
        };
        self.paren_plans.push(plan);
        id
    }

    fn push_closure(&mut self, source: SourceId, plan: ClosurePlan) -> ClosurePlanId {
        let id = ClosurePlanId {
            source: source.0,
            index: self.closure_plans.len() as u32,
        };
        self.closure_plans.push(plan);
        id
    }

    fn push_match(&mut self, source: SourceId, plan: MatchPlan) -> MatchPlanId {
        let id = MatchPlanId {
            source: source.0,
            index: self.match_plans.len() as u32,
        };
        self.match_plans.push(plan);
        id
    }
}

fn compile_expr_plan(expr: &Expr, plans: &mut ExprPlanTables) -> ExprExecPlan {
    let source = expr.handle().source();
    match expr.kind() {
        ExprKind::Block(block) => ExprExecPlan::block(EvalExprKind::Block, block.body.index),
        ExprKind::Literal(literal) => {
            let plan = plans.push_literal(source, compile_literal_plan(literal));
            ExprExecPlan::ready(EvalExprKind::Literal, ReadyPlanKind::Literal, plan.index)
        }
        ExprKind::PathLiteral(path_literal) => {
            let plan = plans.push_path_literal(source, compile_path_literal_plan(path_literal));
            ExprExecPlan::ready(
                EvalExprKind::Literal,
                ReadyPlanKind::PathLiteral,
                plan.index,
            )
        }
        ExprKind::Paren(paren) => {
            let plan = plans.push_paren(source, compile_paren_plan(paren));
            ExprExecPlan::access(EvalExprKind::Paren, AccessPlanKind::Paren, plan.index)
        }
        ExprKind::Closure(closure) => {
            let plan = plans.push_closure(source, compile_closure_plan(closure));
            ExprExecPlan::ready(EvalExprKind::Closure, ReadyPlanKind::Closure, plan.index)
        }
        ExprKind::NameRef(name_ref) => {
            let plan = plans.push_name_ref(source, compile_name_ref_plan(name_ref));
            ExprExecPlan::ready(EvalExprKind::NameRef, ReadyPlanKind::NameRef, plan.index)
        }
        ExprKind::Call(call) => {
            let plan = compile_call_plan(source, call, plans);
            let plan = plans.push_call(source, plan);
            ExprExecPlan::access(EvalExprKind::Call, AccessPlanKind::Call, plan.index)
        }
        ExprKind::Array(array) => {
            let plan = plans.push_array(source, compile_array_plan(array));
            ExprExecPlan::builder(EvalExprKind::Array, BuilderPlanKind::Array, plan.index)
        }
        ExprKind::Tuple(tuple) => {
            let plan = plans.push_tuple(source, compile_tuple_plan(tuple));
            ExprExecPlan::builder(EvalExprKind::Tuple, BuilderPlanKind::Tuple, plan.index)
        }
        ExprKind::Range(range) => {
            let plan = plans.push_range(source, compile_range_plan(range));
            ExprExecPlan::builder(EvalExprKind::Range, BuilderPlanKind::Range, plan.index)
        }
        ExprKind::Map(map) => {
            let plan = plans.push_map(source, compile_map_plan(map));
            ExprExecPlan::builder(EvalExprKind::Map, BuilderPlanKind::Map, plan.index)
        }
        ExprKind::Fail(fail) => {
            let plan = plans.push_fail(source, compile_fail_plan(fail));
            ExprExecPlan::control(EvalExprKind::Fail, ControlPlanKind::Fail, plan.index)
        }
        ExprKind::Fallible(fallible) => {
            let plan = plans.push_fallible(source, compile_fallible_plan(fallible));
            ExprExecPlan::control(
                EvalExprKind::Unsupported,
                ControlPlanKind::Fallible,
                plan.index,
            )
        }
        ExprKind::If(if_expr) => {
            let plan = plans.push_if(source, compile_if_plan(if_expr));
            ExprExecPlan::control(EvalExprKind::If, ControlPlanKind::If, plan.index)
        }
        ExprKind::Match(match_expr) => {
            let plan = plans.push_match(source, compile_match_plan(match_expr));
            ExprExecPlan::control(EvalExprKind::Match, ControlPlanKind::Match, plan.index)
        }
        ExprKind::Where(where_expr) => {
            let plan = compile_where_plan(source, where_expr, plans);
            let plan = plans.push_where(source, plan);
            ExprExecPlan::control(EvalExprKind::Where, ControlPlanKind::Where, plan.index)
        }
        ExprKind::Prefix(prefix) => {
            let plan = plans.push_prefix(source, compile_prefix_plan(prefix));
            ExprExecPlan::control(EvalExprKind::Prefix, ControlPlanKind::Prefix, plan.index)
        }
        ExprKind::Binary(binary) => {
            let plan = plans.push_binary(source, compile_binary_plan(binary));
            ExprExecPlan::control(EvalExprKind::Binary, ControlPlanKind::Binary, plan.index)
        }
        ExprKind::InterpolatedString(interpolated) => {
            let plan = plans
                .push_interpolated_string(source, compile_interpolated_string_plan(interpolated));
            ExprExecPlan::builder(
                EvalExprKind::InterpolatedString,
                BuilderPlanKind::InterpolatedString,
                plan.index,
            )
        }
        ExprKind::StructLiteral(struct_literal) => {
            let plan =
                plans.push_struct_literal(source, compile_struct_literal_plan(struct_literal));
            ExprExecPlan::builder(
                EvalExprKind::StructLiteral,
                BuilderPlanKind::StructLiteral,
                plan.index,
            )
        }
        ExprKind::Field(field) => {
            let plan = plans.push_field(source, compile_field_plan(field));
            ExprExecPlan::access(EvalExprKind::Field, AccessPlanKind::Field, plan.index)
        }
        ExprKind::Unsupported { .. } => ExprExecPlan::expr(),
    }
}

fn compile_literal_plan(literal: &LiteralExpr) -> LiteralPlan {
    let handle = literal.handle;
    match literal.token.as_ref() {
        Some(token) => match token.kind() {
            SyntaxKind::INT => token
                .text()
                .parse::<u64>()
                .map(|value| LiteralPlan::U64 { handle, value })
                .unwrap_or_else(|_| LiteralPlan::RawToken {
                    handle,
                    kind: token.kind(),
                    text: Arc::from(token.text()),
                }),
            SyntaxKind::TRUE_KW => LiteralPlan::Bool {
                handle,
                value: true,
            },
            SyntaxKind::FALSE_KW => LiteralPlan::Bool {
                handle,
                value: false,
            },
            SyntaxKind::STRING => decode_string_literal(token.text())
                .map(|value| LiteralPlan::String {
                    handle,
                    value: Arc::from(value),
                })
                .unwrap_or_else(|_| LiteralPlan::RawToken {
                    handle,
                    kind: token.kind(),
                    text: Arc::from(token.text()),
                }),
            _ => LiteralPlan::RawToken {
                handle,
                kind: token.kind(),
                text: Arc::from(token.text()),
            },
        },
        None => LiteralPlan::RawToken {
            handle,
            kind: SyntaxKind::ERROR,
            text: Arc::from(""),
        },
    }
}

fn compile_path_literal_plan(path_literal: &PathLiteralExpr) -> PathLiteralPlan {
    PathLiteralPlan {
        handle: path_literal.handle,
        descriptor: path_literal.descriptor.clone(),
        interpolated: path_literal.interpolated,
    }
}

fn compile_name_ref_plan(name_ref: &NameRefExpr) -> NameRefPlan {
    NameRefPlan {
        handle: name_ref.handle,
        name: name_ref.name.clone(),
        resolved: name_ref.resolved.clone(),
    }
}

fn compile_call_callee_plan(
    source: SourceId,
    callee: &CallCallee,
    plans: &mut ExprPlanTables,
) -> CallCalleePlan {
    match callee {
        CallCallee::NameRef(name_ref) => {
            let plan = compile_name_ref_plan(name_ref);
            CallCalleePlan::new(
                CallCalleePlanKind::NameRef,
                plans.push_name_ref(source, plan).index,
            )
        }
        CallCallee::ResolvedFunction {
            resolved_name,
            function,
        } => {
            let payload = plans.push_resolved_function_direct_call_callee(
                source,
                ResolvedFunctionDirectCallCalleePlan {
                    resolved_name: resolved_name.clone(),
                    function: function.clone(),
                },
            );
            CallCalleePlan::new(
                CallCalleePlanKind::Direct,
                plans
                    .push_direct_call_callee(
                        source,
                        DirectCallCalleePlan::new(
                            DirectCallCalleePlanKind::ResolvedFunction,
                            payload,
                        ),
                    )
                    .index,
            )
        }
        CallCallee::VariantConstructor {
            handle,
            constructor,
        } => {
            let payload = plans.push_variant_constructor_direct_call_callee(
                source,
                VariantConstructorDirectCallCalleePlan {
                    handle: *handle,
                    constructor: constructor.clone(),
                },
            );
            CallCalleePlan::new(
                CallCalleePlanKind::Direct,
                plans
                    .push_direct_call_callee(
                        source,
                        DirectCallCalleePlan::new(
                            DirectCallCalleePlanKind::VariantConstructor,
                            payload,
                        ),
                    )
                    .index,
            )
        }
        CallCallee::NewtypeConstructor {
            handle,
            display_name,
            newtype_name,
        } => {
            let payload = plans.push_newtype_constructor_direct_call_callee(
                source,
                NewtypeConstructorDirectCallCalleePlan {
                    handle: *handle,
                    display_name: display_name.clone(),
                    newtype_name: newtype_name.clone(),
                },
            );
            CallCalleePlan::new(
                CallCalleePlanKind::Direct,
                plans
                    .push_direct_call_callee(
                        source,
                        DirectCallCalleePlan::new(
                            DirectCallCalleePlanKind::NewtypeConstructor,
                            payload,
                        ),
                    )
                    .index,
            )
        }
        CallCallee::StaticMember {
            handle,
            full_name,
            variant_constructor,
        } => {
            let payload = plans.push_static_member_direct_call_callee(
                source,
                StaticMemberDirectCallCalleePlan {
                    handle: *handle,
                    full_name: full_name.clone(),
                    variant_constructor: variant_constructor.clone(),
                },
            );
            CallCalleePlan::new(
                CallCalleePlanKind::Direct,
                plans
                    .push_direct_call_callee(
                        source,
                        DirectCallCalleePlan::new(DirectCallCalleePlanKind::StaticMember, payload),
                    )
                    .index,
            )
        }
        CallCallee::Field(field) => {
            let plan = compile_field_plan(field);
            CallCalleePlan::new(
                CallCalleePlanKind::Field,
                plans.push_field(source, plan).index,
            )
        }
        CallCallee::Expr(expr) => CallCalleePlan::new(CallCalleePlanKind::Expr, expr.index),
    }
}

fn compile_call_plan(source: SourceId, call: &CallExpr, plans: &mut ExprPlanTables) -> CallPlan {
    CallPlan {
        handle: call.handle,
        callee: call.callee.as_ref().map(|callee| {
            let plan = compile_call_callee_plan(source, callee, plans);
            plans.push_call_callee(source, plan)
        }),
        type_args: Arc::clone(&call.type_args),
        positional_args: Arc::from(
            call.positional_args
                .iter()
                .copied()
                .map(ExprBodyRef::unresolved)
                .collect::<Vec<_>>(),
        ),
    }
}

fn compile_array_plan(array: &ArrayExpr) -> ArrayPlan {
    ArrayPlan {
        handle: array.handle,
        items: Arc::from(
            array
                .items
                .iter()
                .cloned()
                .map(|item| match item {
                    ArrayItem::Expr(expr) => ArrayPlanItem::Expr(ExprBodyRef::unresolved(expr)),
                    ArrayItem::Spread(expr) => ArrayPlanItem::Spread(ExprBodyRef::unresolved(expr)),
                })
                .collect::<Vec<_>>(),
        ),
    }
}

fn compile_tuple_plan(tuple: &TupleExpr) -> TuplePlan {
    TuplePlan {
        handle: tuple.handle,
        items: Arc::from(
            tuple
                .items
                .iter()
                .copied()
                .map(ExprBodyRef::unresolved)
                .collect::<Vec<_>>(),
        ),
    }
}

fn compile_range_plan(range: &RangeExpr) -> RangePlan {
    RangePlan {
        handle: range.handle,
        start: range.start.map(ExprBodyRef::unresolved),
        end: range.end.map(ExprBodyRef::unresolved),
        inclusive_end: range.inclusive_end,
    }
}

fn compile_map_plan(map: &MapExpr) -> MapPlan {
    MapPlan {
        handle: map.handle,
        entries: Arc::from(
            map.entries
                .iter()
                .copied()
                .map(|(key, value)| MapEntryPlan {
                    key: ExprBodyRef::unresolved(key),
                    value: ExprBodyRef::unresolved(value),
                })
                .collect::<Vec<_>>(),
        ),
    }
}

fn compile_fail_plan(fail: &FailExpr) -> FailPlan {
    FailPlan {
        handle: fail.handle,
        message: fail.message.map(ExprBodyRef::unresolved),
    }
}

fn compile_interpolated_string_plan(
    interpolated: &InterpolatedStringExpr,
) -> InterpolatedStringPlan {
    InterpolatedStringPlan {
        handle: interpolated.handle,
        parts: Arc::from(
            interpolated
                .parts
                .iter()
                .cloned()
                .map(|part| match part {
                    StringPart::Text(text) => InterpolatedStringPlanPart::Text(text),
                    StringPart::Interpolation(expr) => {
                        InterpolatedStringPlanPart::Interpolation(ExprBodyRef::unresolved(expr))
                    }
                })
                .collect::<Vec<_>>(),
        ),
    }
}

fn compile_struct_literal_plan(struct_literal: &StructLiteralExpr) -> StructLiteralPlan {
    StructLiteralPlan {
        handle: struct_literal.handle,
        type_name: struct_literal.type_name.clone(),
        parts: Arc::from(
            struct_literal
                .parts
                .iter()
                .cloned()
                .map(|part| match part {
                    StructLiteralPart::Field { name, expr } => StructLiteralPlanPart::Field {
                        name,
                        expr: expr.map(ExprBodyRef::unresolved),
                    },
                    StructLiteralPart::Spread(expr) => {
                        StructLiteralPlanPart::Spread(ExprBodyRef::unresolved(expr))
                    }
                })
                .collect::<Vec<_>>(),
        ),
    }
}

fn compile_field_plan(field: &FieldExpr) -> FieldPlan {
    FieldPlan {
        handle: field.handle,
        receiver: field.receiver.map(ExprBodyRef::unresolved),
        field_name: field.field_name.clone(),
        static_member: field.static_member.clone(),
    }
}

fn compile_where_plan(
    source: SourceId,
    where_expr: &WhereExpr,
    plans: &mut ExprPlanTables,
) -> WherePlan {
    let call = where_expr.call.as_ref().map(|call| {
        let call_plan = compile_call_plan(source, call, plans);
        plans.push_call(source, call_plan)
    });
    WherePlan {
        handle: where_expr.handle,
        call,
        keyword_bindings: Arc::from(
            where_expr
                .keyword_bindings
                .iter()
                .map(|binding| KeywordBindingPlan {
                    name: binding.name.clone(),
                    expr: binding.expr.map(ExprBodyRef::unresolved),
                })
                .collect::<Vec<_>>(),
        ),
    }
}

fn compile_fallible_plan(fallible: &FallibleExpr) -> FalliblePlan {
    FalliblePlan {
        handle: fallible.handle,
        expr: fallible.expr.map(ExprBodyRef::unresolved),
    }
}

fn compile_if_plan(if_expr: &IfExpr) -> IfPlan {
    IfPlan {
        handle: if_expr.handle,
        condition: if_expr.condition.map(ExprBodyRef::unresolved),
        then_branch: if_expr.then_branch.map(ExprBodyRef::unresolved),
        else_branch: if_expr.else_branch.map(ExprBodyRef::unresolved),
    }
}

fn compile_prefix_plan(prefix: &PrefixExpr) -> PrefixPlan {
    PrefixPlan {
        handle: prefix.handle,
        operator: prefix.operator,
        expr: prefix.expr.map(ExprBodyRef::unresolved),
    }
}

fn compile_binary_plan(binary: &BinaryExpr) -> BinaryPlan {
    BinaryPlan {
        handle: binary.handle,
        lhs: binary.lhs.map(ExprBodyRef::unresolved),
        rhs: binary.rhs.map(ExprBodyRef::unresolved),
        operator: binary.operator,
    }
}

fn compile_paren_plan(paren: &ParenExpr) -> ParenPlan {
    ParenPlan {
        handle: paren.handle,
        expr: paren.expr.map(ExprBodyRef::unresolved),
    }
}

fn compile_closure_plan(closure: &ClosureExpr) -> ClosurePlan {
    ClosurePlan {
        handle: closure.handle,
        parameters: Arc::clone(&closure.parameters),
        body: closure.body.map(ExprBodyRef::unresolved),
    }
}

fn compile_match_plan(match_expr: &MatchExpr) -> MatchPlan {
    MatchPlan {
        handle: match_expr.handle,
        scrutinee: match_expr.scrutinee.map(ExprBodyRef::unresolved),
        arms: Arc::from(
            match_expr
                .arms
                .iter()
                .cloned()
                .map(|arm| MatchArmPlan {
                    pattern: arm.pattern,
                    body: arm.body,
                })
                .collect::<Vec<_>>(),
        ),
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
                    .map(ExprBodyRef::unresolved)
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
            resolved: None,
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
                    .map(|callee| match callee {
                        ParserExpr::NameRef(name_ref) => Ok(CallCallee::NameRef(NameRefExpr {
                            handle: ProgramNodeHandle::for_node(source, name_ref),
                            token: name_ref.token().map(|token| compile_token(source, &token)),
                            name: Arc::from(name_ref_text_from_ast(name_ref)),
                            resolved: None,
                        })),
                        ParserExpr::Field(field) => Ok(CallCallee::Field(compile_field_expr_ast(
                            source, field, expr_ids,
                        )?)),
                        _ => Ok(CallCallee::Expr(require_expr_id(
                            expr_ids,
                            ProgramNodeHandle::for_node(source, callee),
                            "call callee",
                        )?)),
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
                            body: ExprBodyRef::unresolved(require_expr_id(
                                expr_ids,
                                ProgramNodeHandle::for_node(source, &body),
                                "match arm body",
                            )?),
                        })
                    })
                    .collect::<Result<Vec<_>, EvalError>>()?,
            ),
        }),
        ParserExpr::Where(where_expr) => ExprKind::Where(WhereExpr {
            handle,
            call_expr: where_expr
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
            call: None,
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
        ParserExpr::Field(field) => {
            ExprKind::Field(compile_field_expr_ast(source, field, expr_ids)?)
        }
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
                .map(ExprBodyRef::unresolved)
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
        ParserStmt::Expr(expr) => BlockStep::Expr(ExprBodyRef::unresolved(require_expr_id(
            expr_ids,
            ProgramNodeHandle::for_node(source, expr),
            "expression statement",
        )?)),
        other => {
            return Err(EvalError::new(
                EvalErrorKind::Internal,
                format!("unsupported block statement: {other:?}"),
            ));
        }
    })
}

fn compile_bytecode_body(source: &CompiledProgramSource, block: &BlockBody) -> BytecodeBody {
    fn local_slot_for_name(
        local_slots: &mut HashMap<String, u32>,
        local_slot_defs: &mut Vec<BytecodeLocalSlot>,
        name: &str,
        prefill_from_scope: bool,
    ) -> u32 {
        if let Some(&slot) = local_slots.get(name) {
            if prefill_from_scope && let Some(existing) = local_slot_defs.get_mut(slot as usize) {
                existing.prefill_from_scope = true;
            }
            return slot;
        }
        let slot = local_slot_defs.len() as u32;
        local_slots.insert(name.to_string(), slot);
        local_slot_defs.push(BytecodeLocalSlot {
            name: Arc::from(name),
            prefill_from_scope,
        });
        slot
    }

    fn push_eval_ops(
        source: &CompiledProgramSource,
        ops: &mut Vec<BytecodeOp>,
        local_slots: &mut HashMap<String, u32>,
        local_slot_defs: &mut Vec<BytecodeLocalSlot>,
        expr: ExprBodyRef,
    ) {
        let plan = expr.plan();
        match plan.kind() {
            ExprExecKind::Ready if plan.ready_kind() == ReadyPlanKind::NameRef => {
                let name_ref = source
                    .resolve_name_ref_plan(plan.name_ref_plan_id(expr.expr_id().source()))
                    .expect("bytecode name ref plan should resolve");
                if name_ref.resolved.is_none() {
                    let slot = local_slot_for_name(
                        local_slots,
                        local_slot_defs,
                        name_ref.name.as_ref(),
                        true,
                    );
                    ops.push(BytecodeOp::LoadLocal {
                        slot,
                        source: source.span_for_range(name_ref.handle.to_range()),
                    });
                    return;
                }
                ops.push(BytecodeOp::EvalReady(plan));
            }
            ExprExecKind::Ready => ops.push(BytecodeOp::EvalReady(plan)),
            ExprExecKind::Block => {
                ops.push(BytecodeOp::EvalBytecodeBody(
                    plan.block_body_id(expr.expr_id().source()).into(),
                ));
            }
            ExprExecKind::Access if plan.access_kind() == AccessPlanKind::Paren => {
                let paren = source
                    .resolve_paren_plan(plan.paren_plan_id(expr.expr_id().source()))
                    .expect("bytecode paren plan should resolve");
                let Some(inner) = paren.expr else {
                    ops.push(BytecodeOp::EvalPlan(expr));
                    return;
                };
                push_eval_ops(source, ops, local_slots, local_slot_defs, inner);
            }
            ExprExecKind::Access if plan.access_kind() == AccessPlanKind::Call => {
                let call = source
                    .resolve_call_plan(plan.call_plan_id(expr.expr_id().source()))
                    .expect("bytecode call plan should resolve");
                ops.push(BytecodeOp::EvalCall {
                    body: expr,
                    call: call.clone(),
                });
            }
            ExprExecKind::Access if plan.access_kind() == AccessPlanKind::Field => {
                let field = source
                    .resolve_field_plan(plan.field_plan_id(expr.expr_id().source()))
                    .expect("bytecode field plan should resolve");
                let (Some(receiver), Some(field_name)) =
                    (field.receiver, field.field_name.as_ref())
                else {
                    ops.push(BytecodeOp::EvalPlan(expr));
                    return;
                };
                if field.static_member.is_some() {
                    ops.push(BytecodeOp::EvalPlan(expr));
                    return;
                }
                push_eval_ops(source, ops, local_slots, local_slot_defs, receiver);
                ops.push(BytecodeOp::ProjectField {
                    field_name: Arc::clone(field_name),
                    field_source: source.span_for_range(field.handle.to_range()),
                });
            }
            ExprExecKind::Control if plan.control_kind() == ControlPlanKind::If => {
                let if_plan = source
                    .resolve_if_plan(plan.if_plan_id(expr.expr_id().source()))
                    .expect("bytecode if plan should resolve");
                let (Some(condition), Some(then_branch), Some(else_branch)) =
                    (if_plan.condition, if_plan.then_branch, if_plan.else_branch)
                else {
                    ops.push(BytecodeOp::EvalPlan(expr));
                    return;
                };
                push_eval_ops(source, ops, local_slots, local_slot_defs, condition);
                ops.push(BytecodeOp::EvalIfBranches {
                    then_branch,
                    else_branch,
                });
            }
            ExprExecKind::Control if plan.control_kind() == ControlPlanKind::Prefix => {
                let prefix = source
                    .resolve_prefix_plan(plan.prefix_plan_id(expr.expr_id().source()))
                    .expect("bytecode prefix plan should resolve");
                let (Some(operator), Some(operand)) = (prefix.operator, prefix.expr) else {
                    ops.push(BytecodeOp::EvalPlan(expr));
                    return;
                };
                push_eval_ops(source, ops, local_slots, local_slot_defs, operand);
                ops.push(BytecodeOp::ApplyPrefix(operator));
            }
            ExprExecKind::Control if plan.control_kind() == ControlPlanKind::Binary => {
                let binary = source
                    .resolve_binary_plan(plan.binary_plan_id(expr.expr_id().source()))
                    .expect("bytecode binary plan should resolve");
                let (Some(lhs), Some(rhs), Some(operator)) =
                    (binary.lhs, binary.rhs, binary.operator)
                else {
                    ops.push(BytecodeOp::EvalPlan(expr));
                    return;
                };
                push_eval_ops(source, ops, local_slots, local_slot_defs, lhs);
                ops.push(BytecodeOp::EvalBinary { operator, rhs });
            }
            ExprExecKind::Control if plan.control_kind() == ControlPlanKind::Fail => {
                let fail = source
                    .resolve_fail_plan(plan.fail_plan_id(expr.expr_id().source()))
                    .expect("bytecode fail plan should resolve");
                let Some(message) = fail.message else {
                    ops.push(BytecodeOp::EvalPlan(expr));
                    return;
                };
                push_eval_ops(source, ops, local_slots, local_slot_defs, message);
                ops.push(BytecodeOp::RaiseUserFail {
                    fail_source: source.span_for_range(fail.handle.to_range()),
                });
            }
            ExprExecKind::Control if plan.control_kind() == ControlPlanKind::Match => {
                let match_plan = source
                    .resolve_match_plan(plan.match_plan_id(expr.expr_id().source()))
                    .expect("bytecode match plan should resolve");
                ops.push(BytecodeOp::EvalMatch {
                    body: expr,
                    match_plan: match_plan.clone(),
                });
            }
            ExprExecKind::Control if plan.control_kind() == ControlPlanKind::Where => {
                let where_plan = source
                    .resolve_where_plan(plan.where_plan_id(expr.expr_id().source()))
                    .expect("bytecode where plan should resolve");
                ops.push(BytecodeOp::EvalWhere {
                    body: expr,
                    where_plan: where_plan.clone(),
                });
            }
            ExprExecKind::Access | ExprExecKind::Builder | ExprExecKind::Control => {
                ops.push(BytecodeOp::EvalPlan(expr));
            }
            ExprExecKind::Expr => ops.push(BytecodeOp::EvalBody(expr)),
        }
    }

    fn collect_pattern_local_bindings(
        pattern: &LetPattern,
        local_slots: &mut HashMap<String, u32>,
        local_slot_defs: &mut Vec<BytecodeLocalSlot>,
        out: &mut Vec<(Arc<str>, u32)>,
    ) {
        match pattern {
            LetPattern::Binding { name, .. } => {
                let slot = local_slot_for_name(local_slots, local_slot_defs, name.as_str(), false);
                out.push((Arc::from(name.as_str()), slot));
            }
            LetPattern::Tuple(items) => {
                for item in items {
                    collect_pattern_local_bindings(item, local_slots, local_slot_defs, out);
                }
            }
            LetPattern::Wildcard | LetPattern::Unit => {}
        }
    }

    let mut ops = Vec::new();
    let mut local_slots = HashMap::new();
    let mut local_slot_defs = Vec::new();
    for step in block.steps.iter() {
        match step {
            BlockStep::Let { pattern, expr } => {
                push_eval_ops(
                    source,
                    &mut ops,
                    &mut local_slots,
                    &mut local_slot_defs,
                    *expr,
                );
                let mut local_bindings = Vec::new();
                collect_pattern_local_bindings(
                    pattern,
                    &mut local_slots,
                    &mut local_slot_defs,
                    &mut local_bindings,
                );
                ops.push(BytecodeOp::BindLet {
                    pattern: pattern.clone(),
                    local_bindings: Arc::from(local_bindings),
                });
            }
            BlockStep::FnDecl(function) => {
                let local_slot = Some(local_slot_for_name(
                    &mut local_slots,
                    &mut local_slot_defs,
                    function.name.as_ref(),
                    false,
                ));
                ops.push(BytecodeOp::BindFnDecl {
                    function: function.clone(),
                    local_slot,
                });
            }
            BlockStep::Expr(expr) => {
                push_eval_ops(
                    source,
                    &mut ops,
                    &mut local_slots,
                    &mut local_slot_defs,
                    *expr,
                );
                ops.push(BytecodeOp::SetLast);
            }
        }
    }
    ops.push(BytecodeOp::ReturnLast);
    BytecodeBody {
        local_slots: Arc::from(local_slot_defs),
        local_count: local_slots.len(),
        ops: Arc::from(ops),
    }
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

fn compile_field_expr_ast(
    source: SourceId,
    field: &vx_parser::FieldExpr,
    expr_ids: &BTreeMap<ProgramNodeHandle, ExprId>,
) -> Result<FieldExpr, EvalError> {
    let static_receiver = match field.receiver() {
        Some(ParserExpr::NameRef(name_ref)) => {
            let token = name_ref.token();
            if token
                .as_ref()
                .is_some_and(|token| token.kind() == SyntaxKind::UPPER_IDENT)
            {
                Some(FieldStaticReceiver {
                    handle: ProgramNodeHandle::for_node(source, &name_ref),
                    name: Arc::from(name_ref_text_from_ast(&name_ref)),
                })
            } else {
                None
            }
        }
        _ => None,
    };
    Ok(FieldExpr {
        handle: ProgramNodeHandle::for_node(source, field),
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
        static_receiver,
        field_name: field.field_name().map(|token| Arc::from(token.text())),
        static_member: None,
    })
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
