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
ast_node!(FieldDecl, SyntaxKind::FIELD_DECL);
ast_node!(VariantDecl, SyntaxKind::VARIANT_DECL);
ast_node!(Parameter, SyntaxKind::PARAMETER);
ast_node!(KeywordParameter, SyntaxKind::KEYWORD_PARAMETER);
ast_node!(ReturnType, SyntaxKind::RETURN_TYPE);
ast_node!(BlockExpr, SyntaxKind::BLOCK_EXPR);
ast_node!(CallExpr, SyntaxKind::CALL_EXPR);
ast_node!(IfExpr, SyntaxKind::IF_EXPR);
ast_node!(MatchExpr, SyntaxKind::MATCH_EXPR);
ast_node!(MatchArm, SyntaxKind::MATCH_ARM);
ast_node!(ArrayExpr, SyntaxKind::ARRAY_EXPR);
ast_node!(MapExpr, SyntaxKind::MAP_EXPR);
ast_node!(MapEntry, SyntaxKind::MAP_ENTRY);
ast_node!(StructLiteralExpr, SyntaxKind::STRUCT_LITERAL_EXPR);
ast_node!(StructLiteralField, SyntaxKind::STRUCT_LITERAL_FIELD);
ast_node!(FieldExpr, SyntaxKind::FIELD_EXPR);
ast_node!(BinaryExpr, SyntaxKind::BINARY_EXPR);
ast_node!(WithExpr, SyntaxKind::WITH_EXPR);
ast_node!(NameRefExpr, SyntaxKind::NAME_REF_EXPR);
ast_node!(LiteralExpr, SyntaxKind::LITERAL_EXPR);
ast_node!(InterpolatedStringExpr, SyntaxKind::INTERPOLATED_STRING_EXPR);
ast_node!(StringInterpolation, SyntaxKind::STRING_INTERPOLATION);
ast_node!(LetStmt, SyntaxKind::LET_STMT);
ast_node!(AssignStmt, SyntaxKind::ASSIGN_STMT);
ast_node!(CompoundUpdateStmt, SyntaxKind::COMPOUND_UPDATE_STMT);
ast_node!(FieldAssignStmt, SyntaxKind::FIELD_ASSIGN_STMT);
ast_node!(FieldAssignTarget, SyntaxKind::FIELD_ASSIGN_TARGET);
ast_node!(ReturnStmt, SyntaxKind::RETURN_STMT);

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
    Call(CallExpr),
    If(IfExpr),
    Match(MatchExpr),
    Array(ArrayExpr),
    Map(MapExpr),
    StructLiteral(StructLiteralExpr),
    Field(FieldExpr),
    Binary(BinaryExpr),
    With(WithExpr),
    NameRef(NameRefExpr),
    Literal(LiteralExpr),
    InterpolatedString(InterpolatedStringExpr),
}

impl AstNode for Expr {
    fn can_cast(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::BLOCK_EXPR
                | SyntaxKind::CALL_EXPR
                | SyntaxKind::IF_EXPR
                | SyntaxKind::MATCH_EXPR
                | SyntaxKind::ARRAY_EXPR
                | SyntaxKind::MAP_EXPR
                | SyntaxKind::STRUCT_LITERAL_EXPR
                | SyntaxKind::FIELD_EXPR
                | SyntaxKind::BINARY_EXPR
                | SyntaxKind::WITH_EXPR
                | SyntaxKind::NAME_REF_EXPR
                | SyntaxKind::LITERAL_EXPR
                | SyntaxKind::INTERPOLATED_STRING_EXPR
        )
    }

    fn cast(node: SyntaxNode) -> Option<Self> {
        match node.kind() {
            SyntaxKind::BLOCK_EXPR => Some(Self::Block(BlockExpr::cast(node)?)),
            SyntaxKind::CALL_EXPR => Some(Self::Call(CallExpr::cast(node)?)),
            SyntaxKind::IF_EXPR => Some(Self::If(IfExpr::cast(node)?)),
            SyntaxKind::MATCH_EXPR => Some(Self::Match(MatchExpr::cast(node)?)),
            SyntaxKind::ARRAY_EXPR => Some(Self::Array(ArrayExpr::cast(node)?)),
            SyntaxKind::MAP_EXPR => Some(Self::Map(MapExpr::cast(node)?)),
            SyntaxKind::STRUCT_LITERAL_EXPR => {
                Some(Self::StructLiteral(StructLiteralExpr::cast(node)?))
            }
            SyntaxKind::FIELD_EXPR => Some(Self::Field(FieldExpr::cast(node)?)),
            SyntaxKind::BINARY_EXPR => Some(Self::Binary(BinaryExpr::cast(node)?)),
            SyntaxKind::WITH_EXPR => Some(Self::With(WithExpr::cast(node)?)),
            SyntaxKind::NAME_REF_EXPR => Some(Self::NameRef(NameRefExpr::cast(node)?)),
            SyntaxKind::LITERAL_EXPR => Some(Self::Literal(LiteralExpr::cast(node)?)),
            SyntaxKind::INTERPOLATED_STRING_EXPR => Some(Self::InterpolatedString(
                InterpolatedStringExpr::cast(node)?,
            )),
            _ => None,
        }
    }

    fn syntax(&self) -> &SyntaxNode {
        match self {
            Self::Block(node) => node.syntax(),
            Self::Call(node) => node.syntax(),
            Self::If(node) => node.syntax(),
            Self::Match(node) => node.syntax(),
            Self::Array(node) => node.syntax(),
            Self::Map(node) => node.syntax(),
            Self::StructLiteral(node) => node.syntax(),
            Self::Field(node) => node.syntax(),
            Self::Binary(node) => node.syntax(),
            Self::With(node) => node.syntax(),
            Self::NameRef(node) => node.syntax(),
            Self::Literal(node) => node.syntax(),
            Self::InterpolatedString(node) => node.syntax(),
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
}

impl TypeAliasDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        first_token(self.syntax(), |kind| kind == SyntaxKind::UPPER_IDENT)
    }
}

impl FieldDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        first_token(self.syntax(), |kind| kind == SyntaxKind::LOWER_IDENT)
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
}

impl KeywordParameter {
    pub fn name(&self) -> Option<SyntaxToken> {
        first_token(self.syntax(), |kind| kind == SyntaxKind::LOWER_IDENT)
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

impl CallExpr {
    pub fn callee(&self) -> Option<Expr> {
        self.syntax().children().find_map(Expr::cast)
    }
}

impl IfExpr {
    pub fn condition(&self) -> Option<Expr> {
        self.syntax().children().find_map(Expr::cast)
    }
}

impl MatchExpr {
    pub fn arms(&self) -> AstChildren<MatchArm> {
        AstChildren::new(self.syntax())
    }
}

impl MatchArm {
    pub fn expr(&self) -> Option<Expr> {
        self.syntax().children().filter_map(Expr::cast).last()
    }
}

impl ArrayExpr {
    pub fn items(&self) -> AstChildren<Expr> {
        AstChildren::new(self.syntax())
    }
}

impl MapExpr {
    pub fn entries(&self) -> AstChildren<MapEntry> {
        AstChildren::new(self.syntax())
    }
}

impl MapEntry {
    pub fn key(&self) -> Option<Expr> {
        nth_expr(self.syntax(), 0)
    }

    pub fn value(&self) -> Option<Expr> {
        nth_expr(self.syntax(), 1)
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
            matches!(kind, SyntaxKind::LOWER_IDENT | SyntaxKind::UPPER_IDENT)
        })
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

impl WithExpr {
    pub fn receiver(&self) -> Option<Expr> {
        nth_expr(self.syntax(), 0)
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
                    SyntaxKind::LOWER_IDENT | SyntaxKind::DOLLAR_IDENT
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

fn child<N: AstNode>(parent: &SyntaxNode) -> Option<N> {
    parent.children().find_map(N::cast)
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
