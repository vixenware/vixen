use rowan::{TextRange, TextSize, WalkEvent};

use crate::{
    AstNode, EnumDecl, FnDecl, Item, NewtypeDecl, SourceFile, StructDecl, SyntaxKind, SyntaxNode,
    SyntaxToken, TypeAliasDecl, ast::VariantDecl,
};

/// Line/column position (both 0-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: u32,
    pub col: u32,
}

/// Maps byte offsets to line/column positions and back.
pub struct LineIndex {
    line_starts: Vec<u32>,
}

impl LineIndex {
    pub fn new(text: &str) -> Self {
        let mut line_starts = vec![0];
        for (i, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }
        Self { line_starts }
    }

    pub fn position(&self, offset: TextSize) -> Position {
        let offset = u32::from(offset);
        let line = self
            .line_starts
            .partition_point(|&start| start <= offset)
            .saturating_sub(1);
        let col = offset - self.line_starts[line];
        Position {
            line: line as u32,
            col,
        }
    }

    pub fn offset(&self, position: Position) -> Option<TextSize> {
        let start = *self.line_starts.get(position.line as usize)?;
        Some(TextSize::from(start + position.col))
    }

    pub fn line_count(&self) -> u32 {
        self.line_starts.len() as u32
    }
}

/// Symbol kind for document outline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Variant,
    Field,
    Newtype,
    TypeAlias,
}

/// A document symbol with optional children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentSymbol {
    pub name: String,
    pub kind: SymbolKind,
    pub range: TextRange,
    pub selection_range: TextRange,
    pub children: Vec<DocumentSymbol>,
}

/// Extract document symbols from a parsed source file.
pub fn document_symbols(file: &SourceFile) -> Vec<DocumentSymbol> {
    file.items().filter_map(item_symbol).collect()
}

fn item_symbol(item: Item) -> Option<DocumentSymbol> {
    match item {
        Item::FnDecl(decl) => fn_symbol(&decl),
        Item::StructDecl(decl) => struct_symbol(&decl),
        Item::EnumDecl(decl) => enum_symbol(&decl),
        Item::NewtypeDecl(decl) => newtype_symbol(&decl),
        Item::TypeAliasDecl(decl) => type_alias_symbol(&decl),
    }
}

fn fn_symbol(decl: &FnDecl) -> Option<DocumentSymbol> {
    let name_token = decl.name()?;
    Some(DocumentSymbol {
        name: name_token.text().to_string(),
        kind: SymbolKind::Function,
        range: decl.syntax().text_range(),
        selection_range: name_token.text_range(),
        children: vec![],
    })
}

fn struct_symbol(decl: &StructDecl) -> Option<DocumentSymbol> {
    let name_token = decl.name()?;
    let children = decl
        .fields()
        .filter_map(|field| {
            let name = field.name()?;
            Some(DocumentSymbol {
                name: name.text().to_string(),
                kind: SymbolKind::Field,
                range: field.syntax().text_range(),
                selection_range: name.text_range(),
                children: vec![],
            })
        })
        .collect();
    Some(DocumentSymbol {
        name: name_token.text().to_string(),
        kind: SymbolKind::Struct,
        range: decl.syntax().text_range(),
        selection_range: name_token.text_range(),
        children,
    })
}

fn enum_symbol(decl: &EnumDecl) -> Option<DocumentSymbol> {
    let name_token = decl.name()?;
    let children = decl
        .variants()
        .filter_map(|variant| variant_symbol(&variant))
        .collect();
    Some(DocumentSymbol {
        name: name_token.text().to_string(),
        kind: SymbolKind::Enum,
        range: decl.syntax().text_range(),
        selection_range: name_token.text_range(),
        children,
    })
}

fn variant_symbol(variant: &VariantDecl) -> Option<DocumentSymbol> {
    let name_token = variant.name()?;
    let children = variant
        .fields()
        .filter_map(|field: crate::FieldDecl| {
            let name = field.name()?;
            Some(DocumentSymbol {
                name: name.text().to_string(),
                kind: SymbolKind::Field,
                range: field.syntax().text_range(),
                selection_range: name.text_range(),
                children: vec![],
            })
        })
        .collect();
    Some(DocumentSymbol {
        name: name_token.text().to_string(),
        kind: SymbolKind::Variant,
        range: variant.syntax().text_range(),
        selection_range: name_token.text_range(),
        children,
    })
}

fn newtype_symbol(decl: &NewtypeDecl) -> Option<DocumentSymbol> {
    let name_token = decl.name()?;
    Some(DocumentSymbol {
        name: name_token.text().to_string(),
        kind: SymbolKind::Newtype,
        range: decl.syntax().text_range(),
        selection_range: name_token.text_range(),
        children: vec![],
    })
}

fn type_alias_symbol(decl: &TypeAliasDecl) -> Option<DocumentSymbol> {
    let name_token = decl.name()?;
    Some(DocumentSymbol {
        name: name_token.text().to_string(),
        kind: SymbolKind::TypeAlias,
        range: decl.syntax().text_range(),
        selection_range: name_token.text_range(),
        children: vec![],
    })
}

/// A node in the breadcrumb trail from root to cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Breadcrumb {
    pub name: Option<String>,
    pub kind: SyntaxKind,
    pub range: TextRange,
}

/// Find the chain of enclosing nodes at a cursor position, from outermost to innermost.
///
/// Returns items, blocks, match arms, and other structurally significant nodes.
pub fn enclosing_nodes(root: &SyntaxNode, offset: TextSize) -> Vec<Breadcrumb> {
    let mut result = Vec::new();
    let mut node = root.clone();

    loop {
        if is_breadcrumb_node(node.kind()) {
            let name = breadcrumb_name(&node);
            result.push(Breadcrumb {
                name,
                kind: node.kind(),
                range: node.text_range(),
            });
        }

        match node
            .children()
            .find(|child| child.text_range().contains_inclusive(offset))
        {
            Some(child) => node = child,
            None => break,
        }
    }

    result
}

fn is_breadcrumb_node(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::SOURCE_FILE
            | SyntaxKind::FN_DECL
            | SyntaxKind::STRUCT_DECL
            | SyntaxKind::ENUM_DECL
            | SyntaxKind::NEWTYPE_DECL
            | SyntaxKind::TYPE_ALIAS_DECL
            | SyntaxKind::BLOCK_EXPR
            | SyntaxKind::IF_EXPR
            | SyntaxKind::MATCH_EXPR
            | SyntaxKind::MATCH_ARM
            | SyntaxKind::CLOSURE_EXPR
    )
}

fn breadcrumb_name(node: &SyntaxNode) -> Option<String> {
    match node.kind() {
        SyntaxKind::FN_DECL => FnDecl::cast(node.clone())?.name(),
        SyntaxKind::STRUCT_DECL => StructDecl::cast(node.clone())?.name(),
        SyntaxKind::ENUM_DECL => EnumDecl::cast(node.clone())?.name(),
        SyntaxKind::NEWTYPE_DECL => NewtypeDecl::cast(node.clone())?.name(),
        SyntaxKind::TYPE_ALIAS_DECL => TypeAliasDecl::cast(node.clone())?.name(),
        _ => return None,
    }
    .map(|token| token.text().to_string())
}

/// A range that can be folded in the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoldingRange {
    pub range: TextRange,
    pub kind: FoldingRangeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldingRangeKind {
    Block,
    Comment,
}

/// Extract folding ranges from a syntax tree.
pub fn folding_ranges(root: &SyntaxNode) -> Vec<FoldingRange> {
    let mut ranges = Vec::new();
    collect_folding_ranges(root, &mut ranges);
    ranges
}

fn collect_folding_ranges(node: &SyntaxNode, out: &mut Vec<FoldingRange>) {
    if is_foldable(node.kind()) {
        let range = node.text_range();
        // Only fold multi-line regions (range must span at least some content)
        if range.len() > TextSize::from(0) {
            let kind = match node.kind() {
                SyntaxKind::LINE_COMMENT | SyntaxKind::BLOCK_COMMENT => FoldingRangeKind::Comment,
                _ => FoldingRangeKind::Block,
            };
            out.push(FoldingRange { range, kind });
        }
    }

    for child in node.children() {
        collect_folding_ranges(&child, out);
    }
}

fn is_foldable(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::FN_DECL
            | SyntaxKind::STRUCT_DECL
            | SyntaxKind::ENUM_DECL
            | SyntaxKind::STRUCT_BODY
            | SyntaxKind::ENUM_BODY
            | SyntaxKind::BLOCK_EXPR
            | SyntaxKind::IF_EXPR
            | SyntaxKind::MATCH_EXPR
            | SyntaxKind::BLOCK_COMMENT
    )
}

// --- Semantic tokens ---

/// Semantic token types used by the vixen LSP.
///
/// The order here defines the token type index in the LSP legend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum SemanticTokenType {
    Keyword = 0,
    Function = 1,
    Variable = 2,
    Parameter = 3,
    Type = 4,
    Property = 5,
    EnumMember = 6,
    Number = 7,
    String = 8,
    Comment = 9,
    Operator = 10,
}

impl SemanticTokenType {
    /// The LSP legend entries in order.
    pub const LEGEND: &[&str] = &[
        "keyword",
        "function",
        "variable",
        "parameter",
        "type",
        "property",
        "enumMember",
        "number",
        "string",
        "comment",
        "operator",
    ];
}

/// A single classified token for semantic highlighting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticToken {
    pub range: TextRange,
    pub token_type: SemanticTokenType,
}

/// Extract semantic tokens from a syntax tree.
pub fn semantic_tokens(root: &SyntaxNode) -> Vec<SemanticToken> {
    let mut tokens = Vec::new();
    for event in root.preorder_with_tokens() {
        if let WalkEvent::Enter(rowan::NodeOrToken::Token(token)) = event
            && let Some(token_type) = classify_token(&token)
        {
            tokens.push(SemanticToken {
                range: token.text_range(),
                token_type,
            });
        }
    }
    tokens
}

fn classify_token(token: &SyntaxToken) -> Option<SemanticTokenType> {
    use SemanticTokenType as T;

    match token.kind() {
        // Keywords
        SyntaxKind::FN_KW
        | SyntaxKind::LET_KW
        | SyntaxKind::TYPE_KW
        | SyntaxKind::STRUCT_KW
        | SyntaxKind::ENUM_KW
        | SyntaxKind::NEWTYPE_KW
        | SyntaxKind::OF_KW
        | SyntaxKind::MATCH_KW
        | SyntaxKind::IF_KW
        | SyntaxKind::ELSE_KW
        | SyntaxKind::WITH_KW
        | SyntaxKind::IS_KW
        | SyntaxKind::AS_KW
        | SyntaxKind::IMPORT_KW
        | SyntaxKind::RETURN_KW
        | SyntaxKind::FAIL_KW
        | SyntaxKind::TRUE_KW
        | SyntaxKind::FALSE_KW
        | SyntaxKind::FN_TYPE_KW => Some(T::Keyword),

        // Literals
        SyntaxKind::INT | SyntaxKind::FLOAT => Some(T::Number),
        SyntaxKind::STRING | SyntaxKind::INTERP_TEXT | SyntaxKind::BACKTICK => Some(T::String),

        // Comments
        SyntaxKind::LINE_COMMENT | SyntaxKind::BLOCK_COMMENT | SyntaxKind::DOC_COMMENT => {
            Some(T::Comment)
        }

        // Operators and punctuation with semantic meaning
        SyntaxKind::EQ2
        | SyntaxKind::BANG_EQ
        | SyntaxKind::LT
        | SyntaxKind::LT_EQ
        | SyntaxKind::GT
        | SyntaxKind::GT_EQ
        | SyntaxKind::PLUS
        | SyntaxKind::MINUS
        | SyntaxKind::STAR
        | SyntaxKind::SLASH
        | SyntaxKind::PERCENT
        | SyntaxKind::AMP2
        | SyntaxKind::PIPE2
        | SyntaxKind::BANG
        | SyntaxKind::DOT2
        | SyntaxKind::DOT2_EQ
        | SyntaxKind::PIPE
        | SyntaxKind::QUESTION => Some(T::Operator),

        // Interpolation delimiters
        SyntaxKind::INTERP_START | SyntaxKind::INTERP_END => Some(T::Operator),

        // Context-sensitive identifiers
        SyntaxKind::UPPER_IDENT => classify_upper_ident(token),
        SyntaxKind::LOWER_IDENT => classify_lower_ident(token),
        SyntaxKind::DOLLAR_IDENT => Some(T::Parameter),

        _ => None,
    }
}

fn classify_upper_ident(token: &SyntaxToken) -> Option<SemanticTokenType> {
    let parent = token.parent()?;
    match parent.kind() {
        SyntaxKind::VARIANT_PATTERN | SyntaxKind::VARIANT_DECL => {
            Some(SemanticTokenType::EnumMember)
        }
        _ => Some(SemanticTokenType::Type),
    }
}

fn classify_lower_ident(token: &SyntaxToken) -> Option<SemanticTokenType> {
    let parent = token.parent()?;
    match parent.kind() {
        // Function declaration name
        SyntaxKind::FN_DECL => Some(SemanticTokenType::Function),

        // Field declarations and access
        SyntaxKind::FIELD_DECL
        | SyntaxKind::FIELD_EXPR
        | SyntaxKind::STRUCT_LITERAL_FIELD
        | SyntaxKind::STRUCT_PATTERN_FIELD
        | SyntaxKind::FIELD_ASSIGN_TARGET
        | SyntaxKind::UPDATE_FIELD
        | SyntaxKind::KEYWORD_ARG => Some(SemanticTokenType::Property),

        // Call expressions — the callee identifier is a function
        SyntaxKind::CALL_EXPR | SyntaxKind::PARTIAL_APP_EXPR => Some(SemanticTokenType::Function),

        // Name references — could be variable or function, default to variable
        SyntaxKind::NAME_REF_EXPR => Some(SemanticTokenType::Variable),

        // Capability tags
        SyntaxKind::CAPABILITY_TAG => Some(SemanticTokenType::Type),

        // Closure parameters
        SyntaxKind::CLOSURE_PARAM_CLAUSE => Some(SemanticTokenType::Parameter),

        // Keyword parameters (name directly under KEYWORD_PARAMETER)
        SyntaxKind::KEYWORD_PARAMETER => Some(SemanticTokenType::Parameter),

        // Binding patterns — check if inside a parameter or a let binding
        SyntaxKind::BINDING_PATTERN | SyntaxKind::AT_PATTERN => classify_binding_context(&parent),

        _ => Some(SemanticTokenType::Variable),
    }
}

/// A binding pattern can appear in parameters or in let statements.
/// Walk up to find out which.
fn classify_binding_context(binding: &SyntaxNode) -> Option<SemanticTokenType> {
    let mut node = binding.parent()?;
    loop {
        match node.kind() {
            SyntaxKind::PARAMETER | SyntaxKind::KEYWORD_PARAMETER => {
                return Some(SemanticTokenType::Parameter);
            }
            SyntaxKind::CLOSURE_PARAM_CLAUSE => {
                return Some(SemanticTokenType::Parameter);
            }
            SyntaxKind::LET_STMT => {
                return Some(SemanticTokenType::Variable);
            }
            SyntaxKind::MATCH_ARM => {
                return Some(SemanticTokenType::Variable);
            }
            // Keep climbing through pattern wrappers
            SyntaxKind::TUPLE_PATTERN
            | SyntaxKind::STRUCT_PATTERN
            | SyntaxKind::STRUCT_PATTERN_FIELD
            | SyntaxKind::ARRAY_PATTERN
            | SyntaxKind::PAREN_PATTERN
            | SyntaxKind::OR_PATTERN
            | SyntaxKind::VARIANT_PATTERN
            | SyntaxKind::AT_PATTERN => {}
            _ => return Some(SemanticTokenType::Variable),
        }
        node = node.parent()?;
    }
}
