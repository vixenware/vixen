use std::{marker::PhantomData, vec::IntoIter};

use rowan::NodeOrToken;

use crate::{SyntaxKind, SyntaxNode, SyntaxToken};

pub trait AstNode: Sized {
    fn can_cast(kind: SyntaxKind) -> bool;
    fn cast(node: SyntaxNode) -> Option<Self>;
    fn syntax(&self) -> &SyntaxNode;
}

pub struct AstChildren<N> {
    inner: IntoIter<SyntaxNode>,
    _marker: PhantomData<N>,
}

impl<N> AstChildren<N> {
    fn new(parent: &SyntaxNode) -> Self {
        Self::from_nodes(parent.children().collect())
    }

    fn from_nodes(nodes: Vec<SyntaxNode>) -> Self {
        Self {
            inner: nodes.into_iter(),
            _marker: PhantomData,
        }
    }
}

impl<N: AstNode> Iterator for AstChildren<N> {
    type Item = N;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.find_map(N::cast)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallArgExpr {
    expr: Expr,
}

impl CallArgExpr {
    pub fn expr(&self) -> Option<Expr> {
        Some(self.expr.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallKeywordArg {
    entry: MapEntry,
}

impl CallKeywordArg {
    pub fn name(&self) -> Option<SyntaxToken> {
        self.entry.name()
    }

    pub fn expr(&self) -> Option<Expr> {
        self.entry.value()
    }
}

macro_rules! ast_node {
    ($name:ident, $kind:path) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name {
            syntax: SyntaxNode,
        }

        impl AstNode for $name {
            fn can_cast(kind: SyntaxKind) -> bool {
                kind == $kind
            }

            fn cast(node: SyntaxNode) -> Option<Self> {
                Self::can_cast(node.kind()).then_some(Self { syntax: node })
            }

            fn syntax(&self) -> &SyntaxNode {
                &self.syntax
            }
        }
    };
}

ast_node!(SourceFile, SyntaxKind::SOURCE_FILE);
ast_node!(FnDecl, SyntaxKind::FN_DECL);
ast_node!(StructDecl, SyntaxKind::STRUCT_DECL);
ast_node!(EnumDecl, SyntaxKind::ENUM_DECL);
ast_node!(NewtypeDecl, SyntaxKind::NEWTYPE_DECL);
ast_node!(TypeAliasDecl, SyntaxKind::TYPE_ALIAS_DECL);
ast_node!(NamespaceDecl, SyntaxKind::NAMESPACE_DECL);
ast_node!(NamespaceBody, SyntaxKind::NAMESPACE_BODY);
ast_node!(FieldDecl, SyntaxKind::FIELD_DECL);
ast_node!(VariantDecl, SyntaxKind::VARIANT_DECL);
ast_node!(Parameter, SyntaxKind::PARAMETER);
ast_node!(KeywordParameter, SyntaxKind::KEYWORD_PARAMETER);
ast_node!(ReturnType, SyntaxKind::RETURN_TYPE);
ast_node!(SlotType, SyntaxKind::SLOT_TYPE);
ast_node!(TypeArgs, SyntaxKind::TYPE_ARGS);
ast_node!(OptionalType, SyntaxKind::OPTIONAL_TYPE);
ast_node!(FunctionType, SyntaxKind::FUNCTION_TYPE);
ast_node!(TupleType, SyntaxKind::TUPLE_TYPE);
ast_node!(ParenType, SyntaxKind::PAREN_TYPE);
ast_node!(SumType, SyntaxKind::SUM_TYPE);
ast_node!(TypePath, SyntaxKind::TYPE_PATH);
ast_node!(BlockExpr, SyntaxKind::BLOCK_EXPR);
ast_node!(ClosureExpr, SyntaxKind::CLOSURE_EXPR);
ast_node!(ClosureParamClause, SyntaxKind::CLOSURE_PARAM_CLAUSE);
ast_node!(ParenExpr, SyntaxKind::PAREN_EXPR);
ast_node!(PartialAppExpr, SyntaxKind::PARTIAL_APP_EXPR);
ast_node!(CallExpr, SyntaxKind::CALL_EXPR);
ast_node!(WhereExpr, SyntaxKind::WHERE_EXPR);
ast_node!(InvokeExpr, SyntaxKind::INVOKE_EXPR);
ast_node!(ArgList, SyntaxKind::ARG_LIST);
ast_node!(Arg, SyntaxKind::ARG);
ast_node!(KeywordArg, SyntaxKind::KEYWORD_ARG);
ast_node!(IndexExpr, SyntaxKind::INDEX_EXPR);
ast_node!(IfExpr, SyntaxKind::IF_EXPR);
ast_node!(ElseClause, SyntaxKind::ELSE_CLAUSE);
ast_node!(MatchExpr, SyntaxKind::MATCH_EXPR);
ast_node!(MatchArm, SyntaxKind::MATCH_ARM);
ast_node!(ArrayExpr, SyntaxKind::ARRAY_EXPR);
ast_node!(SpreadExpr, SyntaxKind::SPREAD_EXPR);
ast_node!(RecordExpr, SyntaxKind::RECORD_EXPR);
ast_node!(MapExpr, SyntaxKind::MAP_EXPR);
ast_node!(MapEntry, SyntaxKind::MAP_ENTRY);
ast_node!(StructLiteralExpr, SyntaxKind::STRUCT_LITERAL_EXPR);
ast_node!(StructLiteralField, SyntaxKind::STRUCT_LITERAL_FIELD);
ast_node!(FieldExpr, SyntaxKind::FIELD_EXPR);
ast_node!(FailExpr, SyntaxKind::FAIL_EXPR);
ast_node!(PrefixExpr, SyntaxKind::PREFIX_EXPR);
ast_node!(BinaryExpr, SyntaxKind::BINARY_EXPR);
ast_node!(RangeExpr, SyntaxKind::RANGE_EXPR);
ast_node!(TypeOpExpr, SyntaxKind::TYPE_OP_EXPR);
ast_node!(WithExpr, SyntaxKind::WITH_EXPR);
ast_node!(FallibleExpr, SyntaxKind::FALLIBLE_EXPR);
ast_node!(NameRefExpr, SyntaxKind::NAME_REF_EXPR);
ast_node!(LiteralExpr, SyntaxKind::LITERAL_EXPR);
ast_node!(InterpolatedStringExpr, SyntaxKind::INTERPOLATED_STRING_EXPR);
ast_node!(PathLiteralExpr, SyntaxKind::PATH_LITERAL_EXPR);
ast_node!(StringInterpolation, SyntaxKind::STRING_INTERPOLATION);
ast_node!(LetStmt, SyntaxKind::LET_STMT);
ast_node!(AssignStmt, SyntaxKind::ASSIGN_STMT);
ast_node!(CompoundUpdateStmt, SyntaxKind::COMPOUND_UPDATE_STMT);
ast_node!(FieldAssignStmt, SyntaxKind::FIELD_ASSIGN_STMT);
ast_node!(FieldAssignTarget, SyntaxKind::FIELD_ASSIGN_TARGET);
ast_node!(ReturnStmt, SyntaxKind::RETURN_STMT);
ast_node!(BindingPattern, SyntaxKind::BINDING_PATTERN);
ast_node!(WildcardPattern, SyntaxKind::WILDCARD_PATTERN);
ast_node!(LiteralPattern, SyntaxKind::LITERAL_PATTERN);
ast_node!(TuplePattern, SyntaxKind::TUPLE_PATTERN);
ast_node!(StructPattern, SyntaxKind::STRUCT_PATTERN);
ast_node!(StructPatternField, SyntaxKind::STRUCT_PATTERN_FIELD);
ast_node!(ArrayPattern, SyntaxKind::ARRAY_PATTERN);
ast_node!(ArrayRestPattern, SyntaxKind::ARRAY_REST_PATTERN);
ast_node!(ParenPattern, SyntaxKind::PAREN_PATTERN);
ast_node!(UnitPattern, SyntaxKind::UNIT_PATTERN);
ast_node!(VariantPattern, SyntaxKind::VARIANT_PATTERN);
ast_node!(AtPattern, SyntaxKind::AT_PATTERN);
ast_node!(OrPattern, SyntaxKind::OR_PATTERN);
ast_node!(CapabilityList, SyntaxKind::CAPABILITY_LIST);
ast_node!(CapabilityTag, SyntaxKind::CAPABILITY_TAG);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Item {
    FnDecl(FnDecl),
    StructDecl(StructDecl),
    EnumDecl(EnumDecl),
    NewtypeDecl(NewtypeDecl),
    TypeAliasDecl(TypeAliasDecl),
}

impl AstNode for Item {
    fn can_cast(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::FN_DECL
                | SyntaxKind::STRUCT_DECL
                | SyntaxKind::ENUM_DECL
                | SyntaxKind::NEWTYPE_DECL
                | SyntaxKind::TYPE_ALIAS_DECL
        )
    }

    fn cast(node: SyntaxNode) -> Option<Self> {
        match node.kind() {
            SyntaxKind::FN_DECL => Some(Self::FnDecl(FnDecl::cast(node)?)),
            SyntaxKind::STRUCT_DECL => Some(Self::StructDecl(StructDecl::cast(node)?)),
            SyntaxKind::ENUM_DECL => Some(Self::EnumDecl(EnumDecl::cast(node)?)),
            SyntaxKind::NEWTYPE_DECL => Some(Self::NewtypeDecl(NewtypeDecl::cast(node)?)),
            SyntaxKind::TYPE_ALIAS_DECL => Some(Self::TypeAliasDecl(TypeAliasDecl::cast(node)?)),
            _ => None,
        }
    }

    fn syntax(&self) -> &SyntaxNode {
        match self {
            Self::FnDecl(node) => node.syntax(),
            Self::StructDecl(node) => node.syntax(),
            Self::EnumDecl(node) => node.syntax(),
            Self::NewtypeDecl(node) => node.syntax(),
            Self::TypeAliasDecl(node) => node.syntax(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Expr {
    Block(BlockExpr),
    Closure(ClosureExpr),
    Paren(ParenExpr),
    PartialApp(PartialAppExpr),
    Call(CallExpr),
    Where(WhereExpr),
    Invoke(InvokeExpr),
    Index(IndexExpr),
    If(IfExpr),
    Match(MatchExpr),
    Array(ArrayExpr),
    Record(RecordExpr),
    Map(MapExpr),
    StructLiteral(StructLiteralExpr),
    Field(FieldExpr),
    Fail(FailExpr),
    Prefix(PrefixExpr),
    Binary(BinaryExpr),
    Range(RangeExpr),
    TypeOp(TypeOpExpr),
    With(WithExpr),
    Fallible(FallibleExpr),
    NameRef(NameRefExpr),
    Literal(LiteralExpr),
    InterpolatedString(InterpolatedStringExpr),
    PathLiteral(PathLiteralExpr),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ArrayItem {
    Expr(Expr),
    Spread(SpreadExpr),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Optional(OptionalType),
    Function(FunctionType),
    Tuple(TupleType),
    Paren(ParenType),
    Sum(SumType),
    Path(TypePath),
}

impl AstNode for Type {
    fn can_cast(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::OPTIONAL_TYPE
                | SyntaxKind::FUNCTION_TYPE
                | SyntaxKind::TUPLE_TYPE
                | SyntaxKind::PAREN_TYPE
                | SyntaxKind::SUM_TYPE
                | SyntaxKind::TYPE_PATH
        )
    }

    fn cast(node: SyntaxNode) -> Option<Self> {
        match node.kind() {
            SyntaxKind::OPTIONAL_TYPE => Some(Self::Optional(OptionalType::cast(node)?)),
            SyntaxKind::FUNCTION_TYPE => Some(Self::Function(FunctionType::cast(node)?)),
            SyntaxKind::TUPLE_TYPE => Some(Self::Tuple(TupleType::cast(node)?)),
            SyntaxKind::PAREN_TYPE => Some(Self::Paren(ParenType::cast(node)?)),
            SyntaxKind::SUM_TYPE => Some(Self::Sum(SumType::cast(node)?)),
            SyntaxKind::TYPE_PATH => Some(Self::Path(TypePath::cast(node)?)),
            _ => None,
        }
    }

    fn syntax(&self) -> &SyntaxNode {
        match self {
            Self::Optional(node) => node.syntax(),
            Self::Function(node) => node.syntax(),
            Self::Tuple(node) => node.syntax(),
            Self::Paren(node) => node.syntax(),
            Self::Sum(node) => node.syntax(),
            Self::Path(node) => node.syntax(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Pattern {
    Binding(BindingPattern),
    Wildcard(WildcardPattern),
    Literal(LiteralPattern),
    Tuple(TuplePattern),
    Struct(StructPattern),
    Array(ArrayPattern),
    ArrayRest(ArrayRestPattern),
    Paren(ParenPattern),
    Unit(UnitPattern),
    Variant(VariantPattern),
    At(AtPattern),
    Or(OrPattern),
}

impl AstNode for Pattern {
    fn can_cast(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::BINDING_PATTERN
                | SyntaxKind::WILDCARD_PATTERN
                | SyntaxKind::LITERAL_PATTERN
                | SyntaxKind::TUPLE_PATTERN
                | SyntaxKind::STRUCT_PATTERN
                | SyntaxKind::ARRAY_PATTERN
                | SyntaxKind::ARRAY_REST_PATTERN
                | SyntaxKind::PAREN_PATTERN
                | SyntaxKind::UNIT_PATTERN
                | SyntaxKind::VARIANT_PATTERN
                | SyntaxKind::AT_PATTERN
                | SyntaxKind::OR_PATTERN
        )
    }

    fn cast(node: SyntaxNode) -> Option<Self> {
        match node.kind() {
            SyntaxKind::BINDING_PATTERN => Some(Self::Binding(BindingPattern::cast(node)?)),
            SyntaxKind::WILDCARD_PATTERN => Some(Self::Wildcard(WildcardPattern::cast(node)?)),
            SyntaxKind::LITERAL_PATTERN => Some(Self::Literal(LiteralPattern::cast(node)?)),
            SyntaxKind::TUPLE_PATTERN => Some(Self::Tuple(TuplePattern::cast(node)?)),
            SyntaxKind::STRUCT_PATTERN => Some(Self::Struct(StructPattern::cast(node)?)),
            SyntaxKind::ARRAY_PATTERN => Some(Self::Array(ArrayPattern::cast(node)?)),
            SyntaxKind::ARRAY_REST_PATTERN => Some(Self::ArrayRest(ArrayRestPattern::cast(node)?)),
            SyntaxKind::PAREN_PATTERN => Some(Self::Paren(ParenPattern::cast(node)?)),
            SyntaxKind::UNIT_PATTERN => Some(Self::Unit(UnitPattern::cast(node)?)),
            SyntaxKind::VARIANT_PATTERN => Some(Self::Variant(VariantPattern::cast(node)?)),
            SyntaxKind::AT_PATTERN => Some(Self::At(AtPattern::cast(node)?)),
            SyntaxKind::OR_PATTERN => Some(Self::Or(OrPattern::cast(node)?)),
            _ => None,
        }
    }

    fn syntax(&self) -> &SyntaxNode {
        match self {
            Self::Binding(node) => node.syntax(),
            Self::Wildcard(node) => node.syntax(),
            Self::Literal(node) => node.syntax(),
            Self::Tuple(node) => node.syntax(),
            Self::Struct(node) => node.syntax(),
            Self::Array(node) => node.syntax(),
            Self::ArrayRest(node) => node.syntax(),
            Self::Paren(node) => node.syntax(),
            Self::Unit(node) => node.syntax(),
            Self::Variant(node) => node.syntax(),
            Self::At(node) => node.syntax(),
            Self::Or(node) => node.syntax(),
        }
    }
}

impl AstNode for Expr {
    fn can_cast(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::BLOCK_EXPR
                | SyntaxKind::CLOSURE_EXPR
                | SyntaxKind::PAREN_EXPR
                | SyntaxKind::PARTIAL_APP_EXPR
                | SyntaxKind::CALL_EXPR
                | SyntaxKind::WHERE_EXPR
                | SyntaxKind::INVOKE_EXPR
                | SyntaxKind::INDEX_EXPR
                | SyntaxKind::IF_EXPR
                | SyntaxKind::MATCH_EXPR
                | SyntaxKind::ARRAY_EXPR
                | SyntaxKind::RECORD_EXPR
                | SyntaxKind::MAP_EXPR
                | SyntaxKind::STRUCT_LITERAL_EXPR
                | SyntaxKind::FIELD_EXPR
                | SyntaxKind::FAIL_EXPR
                | SyntaxKind::PREFIX_EXPR
                | SyntaxKind::BINARY_EXPR
                | SyntaxKind::RANGE_EXPR
                | SyntaxKind::TYPE_OP_EXPR
                | SyntaxKind::WITH_EXPR
                | SyntaxKind::FALLIBLE_EXPR
                | SyntaxKind::NAME_REF_EXPR
                | SyntaxKind::LITERAL_EXPR
                | SyntaxKind::INTERPOLATED_STRING_EXPR
                | SyntaxKind::PATH_LITERAL_EXPR
        )
    }

    fn cast(node: SyntaxNode) -> Option<Self> {
        match node.kind() {
            SyntaxKind::BLOCK_EXPR => Some(Self::Block(BlockExpr::cast(node)?)),
            SyntaxKind::CLOSURE_EXPR => Some(Self::Closure(ClosureExpr::cast(node)?)),
            SyntaxKind::PAREN_EXPR => Some(Self::Paren(ParenExpr::cast(node)?)),
            SyntaxKind::PARTIAL_APP_EXPR => Some(Self::PartialApp(PartialAppExpr::cast(node)?)),
            SyntaxKind::CALL_EXPR => Some(Self::Call(CallExpr::cast(node)?)),
            SyntaxKind::WHERE_EXPR => Some(Self::Where(WhereExpr::cast(node)?)),
            SyntaxKind::INVOKE_EXPR => Some(Self::Invoke(InvokeExpr::cast(node)?)),
            SyntaxKind::INDEX_EXPR => Some(Self::Index(IndexExpr::cast(node)?)),
            SyntaxKind::IF_EXPR => Some(Self::If(IfExpr::cast(node)?)),
            SyntaxKind::MATCH_EXPR => Some(Self::Match(MatchExpr::cast(node)?)),
            SyntaxKind::ARRAY_EXPR => Some(Self::Array(ArrayExpr::cast(node)?)),
            SyntaxKind::RECORD_EXPR => Some(Self::Record(RecordExpr::cast(node)?)),
            SyntaxKind::MAP_EXPR => Some(Self::Map(MapExpr::cast(node)?)),
            SyntaxKind::STRUCT_LITERAL_EXPR => {
                Some(Self::StructLiteral(StructLiteralExpr::cast(node)?))
            }
            SyntaxKind::FIELD_EXPR => Some(Self::Field(FieldExpr::cast(node)?)),
            SyntaxKind::FAIL_EXPR => Some(Self::Fail(FailExpr::cast(node)?)),
            SyntaxKind::PREFIX_EXPR => Some(Self::Prefix(PrefixExpr::cast(node)?)),
            SyntaxKind::BINARY_EXPR => Some(Self::Binary(BinaryExpr::cast(node)?)),
            SyntaxKind::RANGE_EXPR => Some(Self::Range(RangeExpr::cast(node)?)),
            SyntaxKind::TYPE_OP_EXPR => Some(Self::TypeOp(TypeOpExpr::cast(node)?)),
            SyntaxKind::WITH_EXPR => Some(Self::With(WithExpr::cast(node)?)),
            SyntaxKind::FALLIBLE_EXPR => Some(Self::Fallible(FallibleExpr::cast(node)?)),
            SyntaxKind::NAME_REF_EXPR => Some(Self::NameRef(NameRefExpr::cast(node)?)),
            SyntaxKind::LITERAL_EXPR => Some(Self::Literal(LiteralExpr::cast(node)?)),
            SyntaxKind::INTERPOLATED_STRING_EXPR => Some(Self::InterpolatedString(
                InterpolatedStringExpr::cast(node)?,
            )),
            SyntaxKind::PATH_LITERAL_EXPR => Some(Self::PathLiteral(PathLiteralExpr::cast(node)?)),
            _ => None,
        }
    }

    fn syntax(&self) -> &SyntaxNode {
        match self {
            Self::Block(node) => node.syntax(),
            Self::Closure(node) => node.syntax(),
            Self::Paren(node) => node.syntax(),
            Self::PartialApp(node) => node.syntax(),
            Self::Call(node) => node.syntax(),
            Self::Where(node) => node.syntax(),
            Self::Invoke(node) => node.syntax(),
            Self::Index(node) => node.syntax(),
            Self::If(node) => node.syntax(),
            Self::Match(node) => node.syntax(),
            Self::Array(node) => node.syntax(),
            Self::Record(node) => node.syntax(),
            Self::Map(node) => node.syntax(),
            Self::StructLiteral(node) => node.syntax(),
            Self::Field(node) => node.syntax(),
            Self::Fail(node) => node.syntax(),
            Self::Prefix(node) => node.syntax(),
            Self::Binary(node) => node.syntax(),
            Self::Range(node) => node.syntax(),
            Self::TypeOp(node) => node.syntax(),
            Self::With(node) => node.syntax(),
            Self::Fallible(node) => node.syntax(),
            Self::NameRef(node) => node.syntax(),
            Self::Literal(node) => node.syntax(),
            Self::InterpolatedString(node) => node.syntax(),
            Self::PathLiteral(node) => node.syntax(),
        }
    }
}

impl AstNode for ArrayItem {
    fn can_cast(kind: SyntaxKind) -> bool {
        Expr::can_cast(kind) || kind == SyntaxKind::SPREAD_EXPR
    }

    fn cast(node: SyntaxNode) -> Option<Self> {
        if let Some(expr) = Expr::cast(node.clone()) {
            return Some(Self::Expr(expr));
        }
        Some(Self::Spread(SpreadExpr::cast(node)?))
    }

    fn syntax(&self) -> &SyntaxNode {
        match self {
            Self::Expr(node) => node.syntax(),
            Self::Spread(node) => node.syntax(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Stmt {
    Let(LetStmt),
    Assign(AssignStmt),
    CompoundUpdate(CompoundUpdateStmt),
    FieldAssign(FieldAssignStmt),
    Return(ReturnStmt),
    FnDecl(FnDecl),
    StructDecl(StructDecl),
    EnumDecl(EnumDecl),
    NewtypeDecl(NewtypeDecl),
    TypeAliasDecl(TypeAliasDecl),
    Expr(Expr),
}

impl AstNode for Stmt {
    fn can_cast(kind: SyntaxKind) -> bool {
        LetStmt::can_cast(kind)
            || AssignStmt::can_cast(kind)
            || CompoundUpdateStmt::can_cast(kind)
            || FieldAssignStmt::can_cast(kind)
            || ReturnStmt::can_cast(kind)
            || FnDecl::can_cast(kind)
            || StructDecl::can_cast(kind)
            || EnumDecl::can_cast(kind)
            || NewtypeDecl::can_cast(kind)
            || TypeAliasDecl::can_cast(kind)
            || Expr::can_cast(kind)
    }

    fn cast(node: SyntaxNode) -> Option<Self> {
        match node.kind() {
            SyntaxKind::LET_STMT => Some(Self::Let(LetStmt::cast(node)?)),
            SyntaxKind::ASSIGN_STMT => Some(Self::Assign(AssignStmt::cast(node)?)),
            SyntaxKind::COMPOUND_UPDATE_STMT => {
                Some(Self::CompoundUpdate(CompoundUpdateStmt::cast(node)?))
            }
            SyntaxKind::FIELD_ASSIGN_STMT => Some(Self::FieldAssign(FieldAssignStmt::cast(node)?)),
            SyntaxKind::RETURN_STMT => Some(Self::Return(ReturnStmt::cast(node)?)),
            SyntaxKind::FN_DECL => Some(Self::FnDecl(FnDecl::cast(node)?)),
            SyntaxKind::STRUCT_DECL => Some(Self::StructDecl(StructDecl::cast(node)?)),
            SyntaxKind::ENUM_DECL => Some(Self::EnumDecl(EnumDecl::cast(node)?)),
            SyntaxKind::NEWTYPE_DECL => Some(Self::NewtypeDecl(NewtypeDecl::cast(node)?)),
            SyntaxKind::TYPE_ALIAS_DECL => Some(Self::TypeAliasDecl(TypeAliasDecl::cast(node)?)),
            _ => Expr::cast(node).map(Self::Expr),
        }
    }

    fn syntax(&self) -> &SyntaxNode {
        match self {
            Self::Let(node) => node.syntax(),
            Self::Assign(node) => node.syntax(),
            Self::CompoundUpdate(node) => node.syntax(),
            Self::FieldAssign(node) => node.syntax(),
            Self::Return(node) => node.syntax(),
            Self::FnDecl(node) => node.syntax(),
            Self::StructDecl(node) => node.syntax(),
            Self::EnumDecl(node) => node.syntax(),
            Self::NewtypeDecl(node) => node.syntax(),
            Self::TypeAliasDecl(node) => node.syntax(),
            Self::Expr(node) => node.syntax(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StringPart {
    Text(SyntaxToken),
    Interpolation(StringInterpolation),
}

impl SourceFile {
    pub fn cast_root(root: SyntaxNode) -> Self {
        Self::cast(root).expect("root should be a source file")
    }

    pub fn statements(&self) -> AstChildren<Stmt> {
        AstChildren::new(self.syntax())
    }

    pub fn items(&self) -> AstChildren<Item> {
        AstChildren::new(self.syntax())
    }
}

impl Item {
    pub fn name(&self) -> Option<SyntaxToken> {
        first_token(self.syntax(), |kind| {
            matches!(kind, SyntaxKind::LOWER_IDENT | SyntaxKind::UPPER_IDENT)
        })
    }
}

impl FnDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        first_token(self.syntax(), |kind| {
            matches!(kind, SyntaxKind::LOWER_IDENT | SyntaxKind::UPPER_IDENT)
        })
    }

    pub fn parameters(&self) -> AstChildren<Parameter> {
        nested_children(
            self.syntax(),
            &[SyntaxKind::PARAMETER_CLAUSE, SyntaxKind::PARAMETER_LIST],
        )
    }

    pub fn keyword_parameters(&self) -> AstChildren<KeywordParameter> {
        nested_children(
            self.syntax(),
            &[
                SyntaxKind::PARAMETER_CLAUSE,
                SyntaxKind::KEYWORD_PARAMETER_LIST,
            ],
        )
    }

    pub fn return_type(&self) -> Option<ReturnType> {
        child(self.syntax())
    }

    pub fn body(&self) -> Option<BlockExpr> {
        child(self.syntax())
    }

    pub fn capability_list(&self) -> Option<CapabilityList> {
        child(self.syntax())
    }
}

impl StructDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        first_token(self.syntax(), |kind| kind == SyntaxKind::UPPER_IDENT)
    }

    pub fn fields(&self) -> AstChildren<FieldDecl> {
        nested_children(self.syntax(), &[SyntaxKind::STRUCT_BODY])
    }
}

impl EnumDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        first_token(self.syntax(), |kind| kind == SyntaxKind::UPPER_IDENT)
    }

    pub fn variants(&self) -> AstChildren<VariantDecl> {
        nested_children(self.syntax(), &[SyntaxKind::ENUM_BODY])
    }
}

impl NewtypeDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        first_token(self.syntax(), |kind| kind == SyntaxKind::UPPER_IDENT)
    }

    pub fn inner_type(&self) -> Option<Type> {
        self.syntax().children().find_map(Type::cast)
    }
}

impl TypeAliasDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        first_token(self.syntax(), |kind| kind == SyntaxKind::UPPER_IDENT)
    }

    pub fn aliased_type(&self) -> Option<Type> {
        self.syntax().children().find_map(Type::cast)
    }
}

impl NamespaceDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        first_descendant_token(self.syntax(), |kind| kind == SyntaxKind::UPPER_IDENT)
    }

    pub fn target(&self) -> Option<TypePath> {
        child(self.syntax())
    }

    pub fn body(&self) -> Option<NamespaceBody> {
        child(self.syntax())
    }
}

impl NamespaceBody {
    pub fn items(&self) -> AstChildren<Item> {
        AstChildren::new(self.syntax())
    }
}

impl FieldDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        first_token(self.syntax(), |kind| kind == SyntaxKind::LOWER_IDENT)
    }

    pub fn slot_type(&self) -> Option<SlotType> {
        child(self.syntax())
    }
}

impl VariantDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        first_token(self.syntax(), |kind| kind == SyntaxKind::UPPER_IDENT)
    }

    pub fn fields(&self) -> AstChildren<FieldDecl> {
        nested_children(self.syntax(), &[SyntaxKind::STRUCT_BODY])
    }
}

impl Parameter {
    pub fn name(&self) -> Option<SyntaxToken> {
        first_descendant_token(self.syntax(), |kind| {
            matches!(kind, SyntaxKind::LOWER_IDENT | SyntaxKind::UNDERSCORE)
        })
    }

    pub fn slot_type(&self) -> Option<SlotType> {
        child(self.syntax())
    }
}

impl KeywordParameter {
    pub fn name(&self) -> Option<SyntaxToken> {
        first_token(self.syntax(), |kind| kind == SyntaxKind::LOWER_IDENT)
    }

    pub fn slot_type(&self) -> Option<SlotType> {
        child(self.syntax())
    }
}

impl ReturnType {
    pub fn ty(&self) -> Option<Type> {
        child(self.syntax())
    }
}

impl SlotType {
    pub fn ty(&self) -> Option<Type> {
        child(self.syntax())
    }
}

impl TypeArgs {
    pub fn types(&self) -> AstChildren<Type> {
        AstChildren::new(self.syntax())
    }
}

impl OptionalType {
    pub fn inner(&self) -> Option<Type> {
        self.syntax().children().find_map(Type::cast)
    }
}

impl FunctionType {
    pub fn parameter_types(&self) -> AstChildren<Type> {
        let mut types = self
            .syntax()
            .children()
            .filter_map(Type::cast)
            .collect::<Vec<_>>();
        let _ = types.pop();
        AstChildren::from_nodes(types.into_iter().map(|ty| ty.syntax().clone()).collect())
    }

    pub fn return_type(&self) -> Option<Type> {
        self.syntax().children().filter_map(Type::cast).last()
    }

    pub fn capability_list(&self) -> Option<CapabilityList> {
        child(self.syntax())
    }
}

impl TupleType {
    pub fn items(&self) -> AstChildren<Type> {
        AstChildren::new(self.syntax())
    }
}

impl ParenType {
    pub fn inner(&self) -> Option<Type> {
        child(self.syntax())
    }
}

impl SumType {
    pub fn variants(&self) -> AstChildren<Type> {
        AstChildren::new(self.syntax())
    }
}

impl TypePath {
    pub fn segments(&self) -> impl Iterator<Item = SyntaxToken> + '_ {
        self.syntax()
            .children_with_tokens()
            .filter_map(|child| child.into_token())
            .filter(|token| token.kind() == SyntaxKind::UPPER_IDENT)
    }

    pub fn type_args(&self) -> Option<TypeArgs> {
        child(self.syntax())
    }
}

impl BlockExpr {
    pub fn statements(&self) -> AstChildren<Stmt> {
        AstChildren::new(self.syntax())
    }

    pub fn exprs(&self) -> AstChildren<Expr> {
        AstChildren::new(self.syntax())
    }
}

impl ClosureExpr {
    pub fn parameters(&self) -> AstChildren<BindingPattern> {
        nested_children(self.syntax(), &[SyntaxKind::CLOSURE_PARAM_CLAUSE])
    }

    pub fn body(&self) -> Option<Expr> {
        self.syntax().children().filter_map(Expr::cast).last()
    }
}

impl ParenExpr {
    pub fn expr(&self) -> Option<Expr> {
        child(self.syntax())
    }
}

impl PartialAppExpr {
    pub fn callee(&self) -> Option<Expr> {
        child(self.syntax())
    }
}

impl CallExpr {
    pub fn callee(&self) -> Option<Expr> {
        self.root_callee()
    }

    pub fn immediate_callee(&self) -> Option<Expr> {
        nth_expr(self.syntax(), 0)
    }

    pub fn arg(&self) -> Option<Expr> {
        nth_expr(self.syntax(), 1)
    }

    pub fn root_callee(&self) -> Option<Expr> {
        let mut current = self.clone();
        loop {
            match current.immediate_callee()? {
                Expr::Call(call) => current = call,
                expr => return Some(expr),
            }
        }
    }

    pub fn positional_args(&self) -> Vec<Expr> {
        let mut args = Vec::new();
        collect_call_args(self, &mut args);
        args
    }

    pub fn args(&self) -> IntoIter<CallArgExpr> {
        self.positional_args()
            .into_iter()
            .map(|expr| CallArgExpr { expr })
            .collect::<Vec<_>>()
            .into_iter()
    }

    pub fn keyword_args(&self) -> IntoIter<CallKeywordArg> {
        self.keyword_record()
            .map(|record| {
                record
                    .fields()
                    .filter(|entry| entry.name().is_some())
                    .map(|entry| CallKeywordArg { entry })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
            .into_iter()
    }

    fn keyword_record(&self) -> Option<RecordExpr> {
        let where_expr = WhereExpr::cast(self.syntax().parent()?)?;
        match where_expr.keywords()? {
            Expr::Record(record) => Some(record),
            _ => None,
        }
    }
}

impl Arg {
    pub fn expr(&self) -> Option<Expr> {
        child(self.syntax())
    }
}

impl KeywordArg {
    pub fn name(&self) -> Option<SyntaxToken> {
        first_token(self.syntax(), |kind| kind == SyntaxKind::LOWER_IDENT)
    }

    pub fn expr(&self) -> Option<Expr> {
        self.syntax().children().filter_map(Expr::cast).last()
    }
}

impl WhereExpr {
    pub fn callee(&self) -> Option<Expr> {
        nth_expr(self.syntax(), 0)
    }

    pub fn keywords(&self) -> Option<Expr> {
        nth_expr(self.syntax(), 1)
    }
}

impl InvokeExpr {
    pub fn callee(&self) -> Option<Expr> {
        child(self.syntax())
    }
}

impl IndexExpr {
    pub fn receiver(&self) -> Option<Expr> {
        nth_expr(self.syntax(), 0)
    }

    pub fn index(&self) -> Option<Expr> {
        nth_expr(self.syntax(), 1)
    }
}

impl IfExpr {
    pub fn condition(&self) -> Option<Expr> {
        self.syntax().children().find_map(Expr::cast)
    }

    pub fn then_branch(&self) -> Option<Expr> {
        self.syntax().children().filter_map(Expr::cast).nth(1)
    }

    pub fn else_branch(&self) -> Option<Expr> {
        child::<ElseClause>(self.syntax())?.expr()
    }
}

impl ElseClause {
    pub fn expr(&self) -> Option<Expr> {
        child(self.syntax())
    }
}

impl MatchExpr {
    pub fn arms(&self) -> AstChildren<MatchArm> {
        AstChildren::new(self.syntax())
    }
}

impl MatchArm {
    pub fn pattern(&self) -> Option<Pattern> {
        child(self.syntax())
    }

    pub fn expr(&self) -> Option<Expr> {
        self.syntax().children().filter_map(Expr::cast).last()
    }
}

impl ArrayExpr {
    pub fn items(&self) -> AstChildren<ArrayItem> {
        AstChildren::new(self.syntax())
    }
}

impl SpreadExpr {
    pub fn expr(&self) -> Option<Expr> {
        child(self.syntax())
    }
}

impl RecordExpr {
    pub fn fields(&self) -> AstChildren<MapEntry> {
        AstChildren::new(self.syntax())
    }
}

impl MapExpr {
    pub fn entries(&self) -> AstChildren<MapEntry> {
        AstChildren::new(self.syntax())
    }
}

impl MapEntry {
    pub fn name(&self) -> Option<SyntaxToken> {
        let token = self
            .syntax()
            .children_with_tokens()
            .filter_map(|child| child.into_token())
            .find(|token| !token.kind().is_trivia())?;
        (token.kind() == SyntaxKind::LOWER_IDENT).then_some(token)
    }

    pub fn spread(&self) -> Option<SpreadExpr> {
        child(self.syntax())
    }

    pub fn key(&self) -> Option<Expr> {
        if self.name().is_some() {
            None
        } else {
            nth_expr(self.syntax(), 0)
        }
    }

    pub fn value(&self) -> Option<Expr> {
        if self.spread().is_some() {
            None
        } else if self.name().is_some() {
            nth_expr(self.syntax(), 0)
        } else {
            nth_expr(self.syntax(), 1)
        }
    }
}

impl StructLiteralExpr {
    pub fn name(&self) -> Option<SyntaxToken> {
        first_token(self.syntax(), |kind| kind == SyntaxKind::UPPER_IDENT)
    }

    pub fn fields(&self) -> AstChildren<StructLiteralField> {
        nested_children(self.syntax(), &[SyntaxKind::STRUCT_LITERAL_BODY])
    }
}

impl StructLiteralField {
    pub fn name(&self) -> Option<SyntaxToken> {
        first_token(self.syntax(), |kind| kind == SyntaxKind::LOWER_IDENT)
    }

    pub fn expr(&self) -> Option<Expr> {
        child(self.syntax())
    }
}

impl FieldExpr {
    pub fn receiver(&self) -> Option<Expr> {
        child(self.syntax())
    }

    pub fn field_name(&self) -> Option<SyntaxToken> {
        last_token(self.syntax(), |kind| {
            matches!(
                kind,
                SyntaxKind::LOWER_IDENT | SyntaxKind::UPPER_IDENT | SyntaxKind::OF_KW
            )
        })
    }
}

impl FailExpr {
    pub fn message(&self) -> Option<Expr> {
        child(self.syntax())
    }
}

impl PrefixExpr {
    pub fn operator_token(&self) -> Option<SyntaxToken> {
        first_token(self.syntax(), |kind| {
            matches!(kind, SyntaxKind::BANG | SyntaxKind::MINUS)
        })
    }

    pub fn expr(&self) -> Option<Expr> {
        child(self.syntax())
    }
}

impl BinaryExpr {
    pub fn lhs(&self) -> Option<Expr> {
        nth_expr(self.syntax(), 0)
    }

    pub fn rhs(&self) -> Option<Expr> {
        nth_expr(self.syntax(), 1)
    }
}

impl RangeExpr {
    pub fn operands(&self) -> AstChildren<Expr> {
        AstChildren::new(self.syntax())
    }
}

impl TypeOpExpr {
    pub fn expr(&self) -> Option<Expr> {
        child(self.syntax())
    }

    pub fn operator_token(&self) -> Option<SyntaxToken> {
        first_token(self.syntax(), |kind| {
            matches!(
                kind,
                SyntaxKind::AS_KW | SyntaxKind::IS_KW | SyntaxKind::COLON2
            )
        })
    }

    pub fn ty(&self) -> Option<Type> {
        self.syntax().children().find_map(Type::cast)
    }

    pub fn pattern(&self) -> Option<Pattern> {
        self.syntax().children().find_map(Pattern::cast)
    }
}

impl WithExpr {
    pub fn receiver(&self) -> Option<Expr> {
        nth_expr(self.syntax(), 0)
    }
}

impl FallibleExpr {
    pub fn expr(&self) -> Option<Expr> {
        child(self.syntax())
    }
}

impl NameRefExpr {
    pub fn token(&self) -> Option<SyntaxToken> {
        self.syntax()
            .children_with_tokens()
            .filter_map(|child| child.into_token())
            .find(|token| {
                matches!(
                    token.kind(),
                    SyntaxKind::LOWER_IDENT | SyntaxKind::UPPER_IDENT | SyntaxKind::DOLLAR_IDENT
                )
            })
    }
}

impl LiteralExpr {
    pub fn token(&self) -> Option<SyntaxToken> {
        first_token(self.syntax(), |kind| {
            matches!(
                kind,
                SyntaxKind::INT
                    | SyntaxKind::FLOAT
                    | SyntaxKind::STRING
                    | SyntaxKind::TRUE_KW
                    | SyntaxKind::FALSE_KW
            )
        })
    }
}

impl InterpolatedStringExpr {
    pub fn parts(&self) -> impl Iterator<Item = StringPart> + '_ {
        self.syntax()
            .children_with_tokens()
            .filter_map(|child| match child {
                NodeOrToken::Node(node) => {
                    StringInterpolation::cast(node).map(StringPart::Interpolation)
                }
                NodeOrToken::Token(token) if token.kind() == SyntaxKind::INTERP_TEXT => {
                    Some(StringPart::Text(token))
                }
                _ => None,
            })
    }
}

impl PathLiteralExpr {
    pub fn literal(&self) -> Option<LiteralExpr> {
        child(self.syntax())
    }

    pub fn interpolated(&self) -> Option<InterpolatedStringExpr> {
        child(self.syntax())
    }
}

impl StringInterpolation {
    pub fn expr(&self) -> Option<Expr> {
        child(self.syntax())
    }
}

impl LetStmt {
    pub fn name(&self) -> Option<SyntaxToken> {
        first_descendant_token(self.syntax(), |kind| {
            matches!(kind, SyntaxKind::LOWER_IDENT | SyntaxKind::UNDERSCORE)
        })
    }

    pub fn slot_type(&self) -> Option<SlotType> {
        child(self.syntax())
    }

    pub fn expr(&self) -> Option<Expr> {
        self.syntax().children().filter_map(Expr::cast).last()
    }
}

impl AssignStmt {
    pub fn name(&self) -> Option<SyntaxToken> {
        first_token(self.syntax(), |kind| kind == SyntaxKind::LOWER_IDENT)
    }

    pub fn expr(&self) -> Option<Expr> {
        self.syntax().children().filter_map(Expr::cast).last()
    }
}

impl CompoundUpdateStmt {
    pub fn name(&self) -> Option<SyntaxToken> {
        first_token(self.syntax(), |kind| kind == SyntaxKind::LOWER_IDENT)
    }
}

impl FieldAssignStmt {
    pub fn target(&self) -> Option<FieldAssignTarget> {
        child(self.syntax())
    }

    pub fn expr(&self) -> Option<Expr> {
        self.syntax().children().filter_map(Expr::cast).last()
    }
}

impl FieldAssignTarget {
    pub fn segments(&self) -> impl Iterator<Item = SyntaxToken> + '_ {
        self.syntax()
            .children_with_tokens()
            .filter_map(|child| child.into_token())
            .filter(|token| token.kind() == SyntaxKind::LOWER_IDENT)
    }
}

impl ReturnStmt {
    pub fn expr(&self) -> Option<Expr> {
        child(self.syntax())
    }
}

impl BindingPattern {
    pub fn name(&self) -> Option<SyntaxToken> {
        first_descendant_token(self.syntax(), |kind| {
            matches!(kind, SyntaxKind::LOWER_IDENT | SyntaxKind::UNDERSCORE)
        })
    }
}

impl LiteralPattern {
    pub fn token(&self) -> Option<SyntaxToken> {
        first_token(self.syntax(), |kind| {
            matches!(
                kind,
                SyntaxKind::INT
                    | SyntaxKind::FLOAT
                    | SyntaxKind::STRING
                    | SyntaxKind::TRUE_KW
                    | SyntaxKind::FALSE_KW
            )
        })
    }
}

impl TuplePattern {
    pub fn items(&self) -> AstChildren<Pattern> {
        AstChildren::new(self.syntax())
    }
}

impl StructPattern {
    pub fn fields(&self) -> AstChildren<StructPatternField> {
        AstChildren::new(self.syntax())
    }
}

impl StructPatternField {
    pub fn name(&self) -> Option<SyntaxToken> {
        first_token(self.syntax(), |kind| kind == SyntaxKind::LOWER_IDENT)
    }

    pub fn pattern(&self) -> Option<Pattern> {
        self.syntax().children().find_map(Pattern::cast)
    }
}

impl ArrayPattern {
    pub fn items(&self) -> AstChildren<Pattern> {
        AstChildren::new(self.syntax())
    }
}

impl ArrayRestPattern {
    pub fn name(&self) -> Option<SyntaxToken> {
        first_token(self.syntax(), |kind| kind == SyntaxKind::LOWER_IDENT)
    }
}

impl ParenPattern {
    pub fn inner(&self) -> Option<Pattern> {
        child(self.syntax())
    }
}

impl VariantPattern {
    pub fn segments(&self) -> impl Iterator<Item = SyntaxToken> + '_ {
        self.syntax()
            .children_with_tokens()
            .filter_map(|child| child.into_token())
            .filter(|token| token.kind() == SyntaxKind::UPPER_IDENT)
    }

    pub fn fields(&self) -> Option<StructPattern> {
        child(self.syntax())
    }
}

impl AtPattern {
    pub fn binding_name(&self) -> Option<SyntaxToken> {
        first_token(self.syntax(), |kind| kind == SyntaxKind::LOWER_IDENT)
    }

    pub fn pattern(&self) -> Option<Pattern> {
        self.syntax().children().find_map(Pattern::cast)
    }
}

impl OrPattern {
    pub fn patterns(&self) -> AstChildren<Pattern> {
        AstChildren::new(self.syntax())
    }
}

impl CapabilityList {
    pub fn tags(&self) -> AstChildren<CapabilityTag> {
        AstChildren::new(self.syntax())
    }
}

impl CapabilityTag {
    pub fn name(&self) -> Option<SyntaxToken> {
        first_token(self.syntax(), |kind| kind == SyntaxKind::LOWER_IDENT)
    }
}

fn child<N: AstNode>(parent: &SyntaxNode) -> Option<N> {
    parent.children().find_map(N::cast)
}

fn collect_call_args(call: &CallExpr, out: &mut Vec<Expr>) {
    if let Some(Expr::Call(inner)) = call.immediate_callee() {
        collect_call_args(&inner, out);
    }
    if let Some(arg) = call.arg() {
        out.push(arg);
    }
}

fn nth_expr(parent: &SyntaxNode, index: usize) -> Option<Expr> {
    parent.children().filter_map(Expr::cast).nth(index)
}

fn nested_children<N: AstNode>(parent: &SyntaxNode, path: &[SyntaxKind]) -> AstChildren<N> {
    let target = path.iter().try_fold(parent.clone(), |node, &kind| {
        node.children().find(|child| child.kind() == kind)
    });
    let nodes = target
        .into_iter()
        .flat_map(|node| node.children())
        .collect();
    AstChildren::from_nodes(nodes)
}

fn first_token(
    parent: &SyntaxNode,
    mut predicate: impl FnMut(SyntaxKind) -> bool,
) -> Option<SyntaxToken> {
    parent
        .children_with_tokens()
        .filter_map(|child| child.into_token())
        .find(|token| predicate(token.kind()))
}

fn first_descendant_token(
    parent: &SyntaxNode,
    mut predicate: impl FnMut(SyntaxKind) -> bool,
) -> Option<SyntaxToken> {
    parent
        .descendants_with_tokens()
        .filter_map(|child| child.into_token())
        .find(|token| predicate(token.kind()))
}

fn last_token(
    parent: &SyntaxNode,
    mut predicate: impl FnMut(SyntaxKind) -> bool,
) -> Option<SyntaxToken> {
    parent
        .children_with_tokens()
        .filter_map(|child| child.into_token())
        .filter(|token| predicate(token.kind()))
        .last()
}

/// Extract doc comment text from the leading trivia of a syntax node.
///
/// Walks backward from the node's first token through preceding siblings,
/// collecting contiguous `///` doc comment lines. Returns `None` if there
/// are no doc comments.
pub fn doc_comment(node: &SyntaxNode) -> Option<String> {
    // Find the first non-trivia token of this node
    let first_token = node
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| !t.kind().is_trivia())?;

    // Walk backward through preceding siblings collecting doc comments
    let mut doc_tokens = Vec::new();
    let mut tok = first_token.prev_sibling_or_token();
    while let Some(el) = tok {
        match el {
            rowan::NodeOrToken::Token(ref t) => match t.kind() {
                SyntaxKind::DOC_COMMENT => {
                    doc_tokens.push(t.clone());
                }
                SyntaxKind::WHITESPACE => {
                    // whitespace between doc comment lines is fine
                }
                _ => break,
            },
            rowan::NodeOrToken::Node(_) => break,
        }
        tok = el.prev_sibling_or_token();
    }

    if doc_tokens.is_empty() {
        return None;
    }

    // Reverse since we collected backward
    doc_tokens.reverse();

    let text = doc_tokens
        .iter()
        .map(|t| {
            let s = t.text();
            // Strip the `///` prefix and optional leading space
            s.strip_prefix("///")
                .unwrap_or(s)
                .strip_prefix(' ')
                .unwrap_or(s.strip_prefix("///").unwrap_or(s))
        })
        .collect::<Vec<_>>()
        .join("\n");

    Some(text)
}
