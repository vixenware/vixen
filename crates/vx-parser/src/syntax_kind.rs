use rowan::Language;

macro_rules! syntax_kinds {
    ($($kind:ident),+ $(,)?) => {
        #[allow(non_camel_case_types)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(u16)]
        pub enum SyntaxKind {
            $($kind),+
        }

        impl From<u16> for SyntaxKind {
            fn from(raw: u16) -> Self {
                assert!(raw <= SyntaxKind::ERROR as u16, "invalid syntax kind {raw}");
                // SAFETY: the enum is repr(u16) and the assertion above bounds the value.
                unsafe { std::mem::transmute::<u16, SyntaxKind>(raw) }
            }
        }
    };
}

syntax_kinds!(
    TOMBSTONE,
    EOF,
    WHITESPACE,
    LINE_COMMENT,
    BLOCK_COMMENT,
    DOC_COMMENT,
    FRONTMATTER,
    ERROR_TOKEN,
    LOWER_IDENT,
    UPPER_IDENT,
    DOLLAR_IDENT,
    UNDERSCORE,
    FN_TYPE_KW,
    INT,
    FLOAT,
    STRING,
    BACKTICK,
    INTERP_TEXT,
    INTERP_START,
    INTERP_END,
    INTERP_STRING,
    // CLI-literal tokens (`@cc[ … ]`): the opener bracket, a bare command word,
    // and a `$ident` interpolation. `${ expr }` reuses INTERP_START/INTERP_END;
    // the body is lexed in a dedicated command mode.
    CLI_OPEN,
    CLI_WORD,
    CLI_INTERP_VAR,
    FN_KW,
    LET_KW,
    TYPE_KW,
    NAMESPACE_KW,
    STRUCT_KW,
    ENUM_KW,
    NEWTYPE_KW,
    OF_KW,
    MATCH_KW,
    IF_KW,
    ELSE_KW,
    WITH_KW,
    WHERE_KW,
    IS_KW,
    AS_KW,
    IMPORT_KW,
    RETURN_KW,
    FAIL_KW,
    PARTIAL_KW,
    TRUE_KW,
    FALSE_KW,
    BOMB,
    L_PAREN,
    R_PAREN,
    L_BRACKET,
    R_BRACKET,
    L_CURLY,
    R_CURLY,
    COMMA,
    SEMICOLON,
    COLON,
    COLON2,
    DOT,
    DOT2,
    DOT2_EQ,
    EQ,
    EQ2,
    BANG,
    BANG_EQ,
    LT,
    LT_EQ,
    GT,
    GT_EQ,
    PLUS,
    PLUS2,
    MINUS,
    STAR,
    SLASH,
    PERCENT,
    PERCENT_L_CURLY,
    AMP2,
    PIPE,
    PIPE_GT,
    PIPE2,
    FAT_ARROW,
    THIN_ARROW,
    AT,
    HASH,
    QUESTION,
    WITH_EQ,
    SOURCE_FILE,
    ATTRIBUTE_LIST,
    ATTRIBUTE,
    ATTRIBUTE_ENTRY,
    FN_DECL,
    STRUCT_DECL,
    ENUM_DECL,
    NEWTYPE_DECL,
    TYPE_ALIAS_DECL,
    NAMESPACE_DECL,
    STRUCT_BODY,
    ENUM_BODY,
    NAMESPACE_BODY,
    FIELD_DECL,
    VARIANT_DECL,
    VARIANT_TUPLE,
    TYPE_PARAM_LIST,
    PARAMETER_CLAUSE,
    PARAMETER_LIST,
    PARAMETER,
    KEYWORD_PARAMETER_LIST,
    KEYWORD_PARAMETER,
    RETURN_TYPE,
    CAPABILITY_LIST,
    CAPABILITY_TAG,
    SLOT_TYPE,
    INLINE_STRUCT_DECL,
    INLINE_ENUM_DECL,
    INLINE_NEWTYPE_DECL,
    INLINE_TYPE_ALIAS_DECL,
    TYPE_PATH,
    TYPE_ARGS,
    FUNCTION_TYPE,
    OPTIONAL_TYPE,
    TUPLE_TYPE,
    PAREN_TYPE,
    SUM_TYPE,
    BINDING_PATTERN,
    WILDCARD_PATTERN,
    LITERAL_PATTERN,
    TUPLE_PATTERN,
    STRUCT_PATTERN,
    STRUCT_PATTERN_FIELD,
    ARRAY_PATTERN,
    ARRAY_REST_PATTERN,
    PAREN_PATTERN,
    UNIT_PATTERN,
    VARIANT_PATTERN,
    AT_PATTERN,
    OR_PATTERN,
    BLOCK_EXPR,
    CLOSURE_EXPR,
    CLOSURE_PARAM_CLAUSE,
    INTERPOLATED_STRING_EXPR,
    PATH_LITERAL_EXPR,
    // CLI-literal nodes: the whole literal, one argument (a run of adjacent
    // words/strings/interpolations), and a `${ expr }` interpolation hole.
    CLI_LITERAL_EXPR,
    CLI_ARG,
    CLI_INTERP,
    STRING_INTERPOLATION,
    TUPLE_EXPR,
    PAREN_EXPR,
    ARRAY_EXPR,
    RECORD_EXPR,
    RECORD_FIELD,
    MAP_EXPR,
    MAP_ENTRY,
    STRUCT_LITERAL_EXPR,
    STRUCT_LITERAL_BODY,
    STRUCT_LITERAL_FIELD,
    CALL_EXPR,
    WHERE_EXPR,
    INVOKE_EXPR,
    PARTIAL_APP_EXPR,
    ARG_LIST,
    ARG,
    KEYWORD_ARG,
    ARG_PLACEHOLDER,
    SPREAD_EXPR,
    INDEX_EXPR,
    FIELD_EXPR,
    IF_EXPR,
    ELSE_CLAUSE,
    MATCH_EXPR,
    MATCH_ARM,
    IMPORT_EXPR,
    FAIL_EXPR,
    BOMB_EXPR,
    PREFIX_EXPR,
    BINARY_EXPR,
    RANGE_EXPR,
    TYPE_OP_EXPR,
    WITH_EXPR,
    UPDATE_BODY,
    UPDATE_FIELD,
    FIELD_PATH,
    FALLIBLE_EXPR,
    LET_STMT,
    ASSIGN_STMT,
    COMPOUND_UPDATE_STMT,
    FIELD_ASSIGN_STMT,
    FIELD_ASSIGN_TARGET,
    RETURN_STMT,
    NAME_REF_EXPR,
    LITERAL_EXPR,
    ERROR
);

impl SyntaxKind {
    pub const fn is_trivia(self) -> bool {
        matches!(
            self,
            Self::WHITESPACE | Self::LINE_COMMENT | Self::BLOCK_COMMENT | Self::DOC_COMMENT
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VxLanguage {}

impl Language for VxLanguage {
    type Kind = SyntaxKind;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> Self::Kind {
        SyntaxKind::from(raw.0)
    }

    fn kind_to_raw(kind: Self::Kind) -> rowan::SyntaxKind {
        rowan::SyntaxKind(kind as u16)
    }
}

pub type SyntaxToken = rowan::SyntaxToken<VxLanguage>;
