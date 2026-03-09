use std::marker::PhantomData;

use rowan::NodeOrToken;

use crate::{SyntaxKind, SyntaxNode, SyntaxToken};

pub trait AstNode: Sized {
    fn can_cast(kind: SyntaxKind) -> bool;
    fn cast(node: SyntaxNode) -> Option<Self>;
    fn syntax(&self) -> &SyntaxNode;
}

pub struct AstChildren<N> {
    inner: std::vec::IntoIter<SyntaxNode>,
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
ast_node!(NameRefExpr, SyntaxKind::NAME_REF_EXPR);
ast_node!(InterpolatedStringExpr, SyntaxKind::INTERPOLATED_STRING_EXPR);
ast_node!(StringInterpolation, SyntaxKind::STRING_INTERPOLATION);

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
    NameRef(NameRefExpr),
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
                | SyntaxKind::NAME_REF_EXPR
                | SyntaxKind::INTERPOLATED_STRING_EXPR
        )
    }

    fn cast(node: SyntaxNode) -> Option<Self> {
        match node.kind() {
            SyntaxKind::BLOCK_EXPR => Some(Self::Block(BlockExpr::cast(node)?)),
            SyntaxKind::CALL_EXPR => Some(Self::Call(CallExpr::cast(node)?)),
            SyntaxKind::IF_EXPR => Some(Self::If(IfExpr::cast(node)?)),
            SyntaxKind::MATCH_EXPR => Some(Self::Match(MatchExpr::cast(node)?)),
            SyntaxKind::NAME_REF_EXPR => Some(Self::NameRef(NameRefExpr::cast(node)?)),
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
            Self::NameRef(node) => node.syntax(),
            Self::InterpolatedString(node) => node.syntax(),
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
        first_token(self.syntax(), |kind| {
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

fn child<N: AstNode>(parent: &SyntaxNode) -> Option<N> {
    parent.children().find_map(N::cast)
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
