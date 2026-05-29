//! Formatter for Vixen source files.
//!
//! Walks a rowan CST and emits normalized whitespace while preserving comments.

use rowan::NodeOrToken;

use crate::{AstNode, SyntaxKind, SyntaxNode, SyntaxToken};

/// Format a Vixen source file from its syntax tree.
pub fn format(root: &SyntaxNode) -> String {
    let mut f = Formatter::new();
    f.fmt_node(root);
    f.finish()
}

struct Formatter {
    buf: String,
    indent: u32,
    /// True when we just emitted a newline (next content needs indent prefix).
    at_line_start: bool,
    /// Track trailing newlines to avoid excess blank lines.
    trailing_newlines: u32,
}

impl Formatter {
    fn new() -> Self {
        Self {
            buf: String::new(),
            indent: 0,
            at_line_start: true,
            trailing_newlines: 1,
        }
    }

    fn finish(mut self) -> String {
        // Ensure file ends with exactly one newline
        let trimmed = self.buf.trim_end();
        self.buf.truncate(trimmed.len());
        self.buf.push('\n');
        self.buf
    }

    fn write(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.at_line_start {
            for _ in 0..self.indent {
                self.buf.push_str("    ");
            }
            self.at_line_start = false;
        }
        self.buf.push_str(text);
        self.trailing_newlines = 0;
    }

    fn space(&mut self) {
        if !self.at_line_start {
            self.buf.push(' ');
        }
    }

    fn newline(&mut self) {
        self.buf.push('\n');
        self.at_line_start = true;
        self.trailing_newlines += 1;
    }

    fn blank_line(&mut self) {
        while self.trailing_newlines < 2 {
            self.newline();
        }
    }

    fn inc_indent(&mut self) {
        self.indent += 1;
    }

    fn dec_indent(&mut self) {
        self.indent = self.indent.saturating_sub(1);
    }

    // ── Main dispatch ────────────────────────────────────────────────

    fn fmt_node(&mut self, node: &SyntaxNode) {
        match node.kind() {
            SyntaxKind::SOURCE_FILE => self.fmt_source_file(node),
            SyntaxKind::FN_DECL => self.fmt_fn_decl(node),
            SyntaxKind::STRUCT_DECL | SyntaxKind::INLINE_STRUCT_DECL => self.fmt_struct_decl(node),
            SyntaxKind::ENUM_DECL | SyntaxKind::INLINE_ENUM_DECL => self.fmt_enum_decl(node),
            SyntaxKind::NEWTYPE_DECL | SyntaxKind::INLINE_NEWTYPE_DECL => {
                self.fmt_newtype_decl(node)
            }
            SyntaxKind::TYPE_ALIAS_DECL | SyntaxKind::INLINE_TYPE_ALIAS_DECL => {
                self.fmt_type_alias_decl(node)
            }
            SyntaxKind::STRUCT_BODY => self.fmt_struct_body(node),
            SyntaxKind::ENUM_BODY => self.fmt_enum_body(node),
            SyntaxKind::FIELD_DECL => self.fmt_field_decl(node),
            SyntaxKind::VARIANT_DECL => self.fmt_variant_decl(node),
            SyntaxKind::ATTRIBUTE_LIST => self.fmt_attribute_list(node),
            SyntaxKind::ATTRIBUTE => self.fmt_inline(node),
            SyntaxKind::ATTRIBUTE_ENTRY => self.fmt_inline(node),
            SyntaxKind::PARAMETER_CLAUSE => self.fmt_parameter_clause(node),
            SyntaxKind::PARAMETER_LIST => self.fmt_comma_separated_inline(node),
            SyntaxKind::KEYWORD_PARAMETER_LIST => self.fmt_comma_separated_inline(node),
            SyntaxKind::PARAMETER => self.fmt_inline(node),
            SyntaxKind::KEYWORD_PARAMETER => self.fmt_inline(node),
            SyntaxKind::RETURN_TYPE => self.fmt_return_type(node),
            SyntaxKind::CAPABILITY_LIST => self.fmt_inline(node),
            SyntaxKind::CAPABILITY_TAG => self.fmt_inline(node),
            SyntaxKind::TYPE_PARAM_LIST => self.fmt_inline(node),
            SyntaxKind::SLOT_TYPE => self.fmt_slot_type(node),
            SyntaxKind::BLOCK_EXPR => self.fmt_block_expr(node),
            SyntaxKind::IF_EXPR => self.fmt_if_expr(node),
            SyntaxKind::ELSE_CLAUSE => self.fmt_else_clause(node),
            SyntaxKind::MATCH_EXPR => self.fmt_match_expr(node),
            SyntaxKind::MATCH_ARM => self.fmt_match_arm(node),
            SyntaxKind::CALL_EXPR => self.fmt_call_expr(node),
            SyntaxKind::WHERE_EXPR => self.fmt_where_expr(node),
            SyntaxKind::INVOKE_EXPR => self.fmt_invoke_expr(node),
            SyntaxKind::ARG_LIST => self.fmt_arg_list(node),
            SyntaxKind::BINARY_EXPR => self.fmt_binary_expr(node),
            SyntaxKind::PREFIX_EXPR => self.fmt_prefix_expr(node),
            SyntaxKind::FIELD_EXPR => self.fmt_field_expr(node),
            SyntaxKind::INDEX_EXPR => self.fmt_index_expr(node),
            SyntaxKind::CLOSURE_EXPR => self.fmt_closure_expr(node),
            SyntaxKind::WITH_EXPR => self.fmt_with_expr(node),
            SyntaxKind::RECORD_EXPR => self.fmt_brace_literal(node),
            SyntaxKind::STRUCT_LITERAL_EXPR => self.fmt_struct_literal_expr(node),
            SyntaxKind::STRUCT_LITERAL_BODY => self.fmt_struct_literal_body(node),
            SyntaxKind::STRUCT_LITERAL_FIELD => self.fmt_inline(node),
            SyntaxKind::MAP_EXPR => self.fmt_map_expr(node),
            SyntaxKind::MAP_ENTRY => self.fmt_inline(node),
            SyntaxKind::ARRAY_EXPR => self.fmt_bracket_list(node),
            SyntaxKind::LET_STMT => self.fmt_let_stmt(node),
            SyntaxKind::ASSIGN_STMT => self.fmt_inline(node),
            SyntaxKind::RETURN_STMT => self.fmt_return_stmt(node),
            SyntaxKind::COMPOUND_UPDATE_STMT => self.fmt_inline(node),
            SyntaxKind::FIELD_ASSIGN_STMT => self.fmt_inline(node),
            SyntaxKind::INTERPOLATED_STRING_EXPR => self.fmt_verbatim(node),
            SyntaxKind::UPDATE_BODY => self.fmt_struct_literal_body(node),
            SyntaxKind::UPDATE_FIELD => self.fmt_inline(node),
            SyntaxKind::IMPORT_EXPR => self.fmt_inline(node),
            SyntaxKind::FAIL_EXPR => self.fmt_fail_expr(node),
            SyntaxKind::BOMB_EXPR => self.fmt_inline(node),
            SyntaxKind::TYPE_OP_EXPR => self.fmt_inline(node),
            SyntaxKind::RANGE_EXPR => self.fmt_inline(node),
            SyntaxKind::FALLIBLE_EXPR => self.fmt_inline(node),
            SyntaxKind::PATH_LITERAL_EXPR => self.fmt_verbatim(node),
            SyntaxKind::PARTIAL_APP_EXPR => self.fmt_inline(node),
            SyntaxKind::SPREAD_EXPR => self.fmt_inline(node),
            SyntaxKind::ERROR => self.fmt_verbatim(node),
            _ => self.fmt_inline(node),
        }
    }

    // ── Token handling ───────────────────────────────────────────────

    fn fmt_token(&mut self, token: &SyntaxToken) {
        match token.kind() {
            SyntaxKind::WHITESPACE => {} // Always discard; formatter controls whitespace
            SyntaxKind::LINE_COMMENT | SyntaxKind::DOC_COMMENT => {
                self.write(token.text().trim_end());
                self.newline();
            }
            SyntaxKind::BLOCK_COMMENT => {
                self.write(token.text().trim_end());
            }
            _ => {
                self.write(token.text());
            }
        }
    }

    // ── Source file ──────────────────────────────────────────────────

    fn fmt_source_file(&mut self, node: &SyntaxNode) {
        let mut first = true;
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE => {}
                    SyntaxKind::LINE_COMMENT | SyntaxKind::DOC_COMMENT => {
                        if !first {
                            // Comments between items get a blank line before them
                            // unless they're at the very start
                            self.blank_line();
                        }
                        self.write(t.text().trim_end());
                        self.newline();
                        first = false;
                    }
                    SyntaxKind::BLOCK_COMMENT => {
                        if !first {
                            self.blank_line();
                        }
                        self.write(t.text().trim_end());
                        self.newline();
                        first = false;
                    }
                    _ => {}
                },
                NodeOrToken::Node(n) => {
                    if !first {
                        self.blank_line();
                    }
                    self.fmt_node(n);
                    first = false;
                }
            }
        }
    }

    // ── Declarations ─────────────────────────────────────────────────

    fn fmt_fn_decl(&mut self, node: &SyntaxNode) {
        // Attributes, then: fn name[T](params) #caps -> Ret { body }
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE => {}
                    SyntaxKind::FN_KW => self.write("fn"),
                    SyntaxKind::LOWER_IDENT | SyntaxKind::UPPER_IDENT => {
                        self.space();
                        self.write(t.text());
                    }
                    SyntaxKind::THIN_ARROW => {
                        self.space();
                        self.write("->");
                        self.space();
                    }
                    SyntaxKind::LINE_COMMENT | SyntaxKind::DOC_COMMENT => {
                        self.write(t.text().trim_end());
                        self.newline();
                    }
                    SyntaxKind::BLOCK_COMMENT => {
                        self.space();
                        self.write(t.text().trim_end());
                        self.space();
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => match n.kind() {
                    SyntaxKind::ATTRIBUTE_LIST => {
                        self.fmt_attribute_list(n);
                    }
                    SyntaxKind::TYPE_PARAM_LIST => self.fmt_inline(n),
                    SyntaxKind::PARAMETER_CLAUSE => self.fmt_parameter_clause(n),
                    SyntaxKind::CAPABILITY_LIST => {
                        self.space();
                        self.fmt_inline(n);
                    }
                    SyntaxKind::RETURN_TYPE => {
                        self.fmt_node(n);
                    }
                    SyntaxKind::BLOCK_EXPR => {
                        self.space();
                        self.fmt_block_expr(n);
                    }
                    _ => self.fmt_node(n),
                },
            }
        }
    }

    fn fmt_struct_decl(&mut self, node: &SyntaxNode) {
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE => {}
                    SyntaxKind::STRUCT_KW => self.write("struct"),
                    SyntaxKind::UPPER_IDENT => {
                        self.space();
                        self.write(t.text());
                    }
                    SyntaxKind::LINE_COMMENT | SyntaxKind::DOC_COMMENT => {
                        self.write(t.text().trim_end());
                        self.newline();
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => match n.kind() {
                    SyntaxKind::ATTRIBUTE_LIST => self.fmt_attribute_list(n),
                    SyntaxKind::TYPE_PARAM_LIST => self.fmt_inline(n),
                    SyntaxKind::STRUCT_BODY => {
                        self.space();
                        self.fmt_struct_body(n);
                    }
                    _ => self.fmt_node(n),
                },
            }
        }
    }

    fn fmt_enum_decl(&mut self, node: &SyntaxNode) {
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE => {}
                    SyntaxKind::ENUM_KW => self.write("enum"),
                    SyntaxKind::UPPER_IDENT => {
                        self.space();
                        self.write(t.text());
                    }
                    SyntaxKind::LINE_COMMENT | SyntaxKind::DOC_COMMENT => {
                        self.write(t.text().trim_end());
                        self.newline();
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => match n.kind() {
                    SyntaxKind::ATTRIBUTE_LIST => self.fmt_attribute_list(n),
                    SyntaxKind::TYPE_PARAM_LIST => self.fmt_inline(n),
                    SyntaxKind::ENUM_BODY => {
                        self.space();
                        self.fmt_enum_body(n);
                    }
                    _ => self.fmt_node(n),
                },
            }
        }
    }

    fn fmt_newtype_decl(&mut self, node: &SyntaxNode) {
        // newtype Name[T] of Type
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE => {}
                    SyntaxKind::NEWTYPE_KW => self.write("newtype"),
                    SyntaxKind::OF_KW => {
                        self.space();
                        self.write("of");
                        self.space();
                    }
                    SyntaxKind::UPPER_IDENT => {
                        self.space();
                        self.write(t.text());
                    }
                    SyntaxKind::LINE_COMMENT | SyntaxKind::DOC_COMMENT => {
                        self.write(t.text().trim_end());
                        self.newline();
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => match n.kind() {
                    SyntaxKind::ATTRIBUTE_LIST => self.fmt_attribute_list(n),
                    SyntaxKind::TYPE_PARAM_LIST => self.fmt_inline(n),
                    _ => self.fmt_node(n),
                },
            }
        }
    }

    fn fmt_type_alias_decl(&mut self, node: &SyntaxNode) {
        // type Name[T] = Type
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE => {}
                    SyntaxKind::TYPE_KW => self.write("type"),
                    SyntaxKind::UPPER_IDENT => {
                        self.space();
                        self.write(t.text());
                    }
                    SyntaxKind::EQ => {
                        self.space();
                        self.write("=");
                        self.space();
                    }
                    SyntaxKind::LINE_COMMENT | SyntaxKind::DOC_COMMENT => {
                        self.write(t.text().trim_end());
                        self.newline();
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => match n.kind() {
                    SyntaxKind::ATTRIBUTE_LIST => self.fmt_attribute_list(n),
                    SyntaxKind::TYPE_PARAM_LIST => self.fmt_inline(n),
                    _ => self.fmt_node(n),
                },
            }
        }
    }

    // ── Bodies ───────────────────────────────────────────────────────

    fn fmt_struct_body(&mut self, node: &SyntaxNode) {
        self.write("{");
        self.inc_indent();

        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::L_CURLY
                    | SyntaxKind::R_CURLY
                    | SyntaxKind::WHITESPACE
                    | SyntaxKind::COMMA => {}
                    SyntaxKind::LINE_COMMENT | SyntaxKind::DOC_COMMENT => {
                        self.newline();
                        self.write(t.text().trim_end());
                    }
                    SyntaxKind::BLOCK_COMMENT => {
                        self.newline();
                        self.write(t.text().trim_end());
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) if n.kind() == SyntaxKind::FIELD_DECL => {
                    self.newline();
                    self.fmt_field_decl(n);
                    self.write(",");
                }
                NodeOrToken::Node(n) => {
                    self.newline();
                    self.fmt_node(n);
                }
            }
        }

        self.dec_indent();
        self.newline();
        self.write("}");
    }

    fn fmt_enum_body(&mut self, node: &SyntaxNode) {
        self.write("{");
        self.inc_indent();

        let variants: Vec<_> = node
            .children()
            .filter(|n| n.kind() == SyntaxKind::VARIANT_DECL)
            .collect();
        let has_bodied_variant = variants
            .iter()
            .any(|v| v.children().any(|c| c.kind() == SyntaxKind::STRUCT_BODY));

        let mut first = true;
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::L_CURLY
                    | SyntaxKind::R_CURLY
                    | SyntaxKind::WHITESPACE
                    | SyntaxKind::COMMA => {}
                    SyntaxKind::LINE_COMMENT | SyntaxKind::DOC_COMMENT => {
                        self.newline();
                        self.write(t.text().trim_end());
                    }
                    SyntaxKind::BLOCK_COMMENT => {
                        self.newline();
                        self.write(t.text().trim_end());
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) if n.kind() == SyntaxKind::VARIANT_DECL => {
                    if !first && has_bodied_variant {
                        self.blank_line();
                    } else {
                        self.newline();
                    }
                    self.fmt_variant_decl(n);
                    self.write(",");
                    first = false;
                }
                NodeOrToken::Node(n) => {
                    self.newline();
                    self.fmt_node(n);
                    first = false;
                }
            }
        }

        self.dec_indent();
        self.newline();
        self.write("}");
    }

    fn fmt_field_decl(&mut self, node: &SyntaxNode) {
        // [attrs] name: SlotType [= default]
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE => {}
                    SyntaxKind::COLON => {
                        self.write(":");
                        self.space();
                    }
                    SyntaxKind::EQ => {
                        self.space();
                        self.write("=");
                        self.space();
                    }
                    SyntaxKind::LINE_COMMENT | SyntaxKind::DOC_COMMENT => {
                        self.write(t.text().trim_end());
                        self.newline();
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => match n.kind() {
                    SyntaxKind::ATTRIBUTE_LIST => {
                        self.fmt_attribute_list(n);
                    }
                    SyntaxKind::SLOT_TYPE => self.fmt_slot_type(n),
                    _ => self.fmt_node(n),
                },
            }
        }
    }

    fn fmt_variant_decl(&mut self, node: &SyntaxNode) {
        // [attrs] struct Name [{ fields }]
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE => {}
                    SyntaxKind::STRUCT_KW => self.write("struct"),
                    SyntaxKind::UPPER_IDENT => {
                        self.space();
                        self.write(t.text());
                    }
                    SyntaxKind::LINE_COMMENT | SyntaxKind::DOC_COMMENT => {
                        self.write(t.text().trim_end());
                        self.newline();
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => match n.kind() {
                    SyntaxKind::ATTRIBUTE_LIST => self.fmt_attribute_list(n),
                    SyntaxKind::STRUCT_BODY => {
                        self.space();
                        self.fmt_struct_body(n);
                    }
                    _ => self.fmt_node(n),
                },
            }
        }
    }

    fn fmt_slot_type(&mut self, node: &SyntaxNode) {
        // May contain inline type declarations or regular type expressions
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) if t.kind() == SyntaxKind::WHITESPACE => {}
                NodeOrToken::Token(t) => self.fmt_token(t),
                NodeOrToken::Node(n) => self.fmt_node(n),
            }
        }
    }

    // ── Attributes ───────────────────────────────────────────────────

    fn fmt_attribute_list(&mut self, node: &SyntaxNode) {
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) if t.kind() == SyntaxKind::WHITESPACE => {}
                NodeOrToken::Token(t) => self.fmt_token(t),
                NodeOrToken::Node(n) => {
                    self.fmt_inline(n);
                    self.newline();
                }
            }
        }
    }

    // ── Parameters ───────────────────────────────────────────────────

    fn fmt_parameter_clause(&mut self, node: &SyntaxNode) {
        // (params; keyword_params)
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE => {}
                    SyntaxKind::SEMICOLON => {
                        self.write(";");
                        self.space();
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => self.fmt_node(n),
            }
        }
    }

    fn fmt_return_type(&mut self, node: &SyntaxNode) {
        // -> Type
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE => {}
                    SyntaxKind::THIN_ARROW => {
                        self.write("->");
                        self.space();
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => self.fmt_node(n),
            }
        }
    }

    // ── Expressions ──────────────────────────────────────────────────

    fn fmt_block_expr(&mut self, node: &SyntaxNode) {
        self.write("{");
        self.inc_indent();

        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::L_CURLY | SyntaxKind::R_CURLY | SyntaxKind::WHITESPACE => {}
                    SyntaxKind::LINE_COMMENT | SyntaxKind::DOC_COMMENT => {
                        self.newline();
                        self.write(t.text().trim_end());
                    }
                    SyntaxKind::BLOCK_COMMENT => {
                        self.newline();
                        self.write(t.text().trim_end());
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => {
                    self.newline();
                    self.fmt_node(n);
                }
            }
        }

        self.dec_indent();
        self.newline();
        self.write("}");
    }

    fn fmt_if_expr(&mut self, node: &SyntaxNode) {
        // if cond { ... } else { ... }
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE => {}
                    SyntaxKind::IF_KW => self.write("if"),
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => match n.kind() {
                    SyntaxKind::BLOCK_EXPR => {
                        self.space();
                        self.fmt_block_expr(n);
                    }
                    SyntaxKind::ELSE_CLAUSE => {
                        self.space();
                        self.fmt_else_clause(n);
                    }
                    _ => {
                        self.space();
                        self.fmt_node(n);
                    }
                },
            }
        }
    }

    fn fmt_else_clause(&mut self, node: &SyntaxNode) {
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE => {}
                    SyntaxKind::ELSE_KW => self.write("else"),
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => match n.kind() {
                    SyntaxKind::BLOCK_EXPR => {
                        self.space();
                        self.fmt_block_expr(n);
                    }
                    SyntaxKind::IF_EXPR => {
                        self.space();
                        self.fmt_if_expr(n);
                    }
                    _ => {
                        self.space();
                        self.fmt_node(n);
                    }
                },
            }
        }
    }

    fn fmt_match_expr(&mut self, node: &SyntaxNode) {
        // match subject { arm, arm, ... }
        let mut in_body = false;
        let mut first_arm = true;
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE => {}
                    SyntaxKind::MATCH_KW => self.write("match"),
                    SyntaxKind::L_CURLY => {
                        self.space();
                        self.write("{");
                        self.inc_indent();
                        in_body = true;
                    }
                    SyntaxKind::R_CURLY => {
                        self.dec_indent();
                        self.newline();
                        self.write("}");
                    }
                    SyntaxKind::COMMA if in_body => {}
                    SyntaxKind::LINE_COMMENT | SyntaxKind::DOC_COMMENT if in_body => {
                        self.newline();
                        self.write(t.text().trim_end());
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) if n.kind() == SyntaxKind::MATCH_ARM => {
                    self.newline();
                    self.fmt_match_arm(n);
                    self.write(",");
                    first_arm = false;
                }
                NodeOrToken::Node(n) if !in_body => {
                    // The match subject expression
                    self.space();
                    self.fmt_node(n);
                }
                NodeOrToken::Node(n) => {
                    let _ = first_arm;
                    self.fmt_node(n);
                }
            }
        }
    }

    fn fmt_match_arm(&mut self, node: &SyntaxNode) {
        // pattern [if guard] => expr
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE => {}
                    SyntaxKind::FAT_ARROW => {
                        self.space();
                        self.write("=>");
                        self.space();
                    }
                    SyntaxKind::IF_KW => {
                        self.space();
                        self.write("if");
                        self.space();
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => self.fmt_node(n),
            }
        }
    }

    fn fmt_call_expr(&mut self, node: &SyntaxNode) {
        let parts = collect_call_parts(node);
        for (index, part) in parts.iter().enumerate() {
            if index > 0
                && !matches!(
                    part.kind(),
                    SyntaxKind::PAREN_EXPR | SyntaxKind::TUPLE_EXPR | SyntaxKind::TYPE_ARGS
                )
            {
                self.space();
            }
            self.fmt_node(part);
        }
    }

    fn fmt_where_expr(&mut self, node: &SyntaxNode) {
        let mut children = node.children().filter_map(crate::Expr::cast);
        if let Some(base) = children.next() {
            self.fmt_node(base.syntax());
        }
        self.space();
        self.write("where");
        if let Some(keywords) = children.next() {
            self.space();
            self.fmt_node(keywords.syntax());
        }
    }

    fn fmt_invoke_expr(&mut self, node: &SyntaxNode) {
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) if t.kind() == SyntaxKind::WHITESPACE => {}
                NodeOrToken::Token(t) => self.write(t.text()),
                NodeOrToken::Node(n) => self.fmt_node(n),
            }
        }
    }

    fn fmt_arg_list(&mut self, node: &SyntaxNode) {
        // Determine if multi-line: if contains keyword args or is long
        let has_keyword_args = node.children().any(|c| c.kind() == SyntaxKind::KEYWORD_ARG);
        let has_semicolon = node
            .children_with_tokens()
            .any(|c| matches!(&c, NodeOrToken::Token(t) if t.kind() == SyntaxKind::SEMICOLON));

        if has_semicolon || has_keyword_args {
            self.fmt_arg_list_multiline(node);
        } else {
            self.fmt_arg_list_inline(node);
        }
    }

    fn fmt_arg_list_inline(&mut self, node: &SyntaxNode) {
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE => {}
                    SyntaxKind::COMMA => {
                        self.write(",");
                        self.space();
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => self.fmt_node(n),
            }
        }
    }

    fn fmt_arg_list_multiline(&mut self, node: &SyntaxNode) {
        self.write("(");
        self.inc_indent();

        let mut first = true;
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::L_PAREN | SyntaxKind::R_PAREN | SyntaxKind::WHITESPACE => {}
                    SyntaxKind::COMMA => {
                        self.write(",");
                    }
                    SyntaxKind::SEMICOLON => {
                        self.write(";");
                    }
                    SyntaxKind::LINE_COMMENT | SyntaxKind::DOC_COMMENT => {
                        self.space();
                        self.write(t.text().trim_end());
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => {
                    self.newline();
                    self.fmt_node(n);
                    first = false;
                }
            }
        }

        let _ = first;
        self.dec_indent();
        self.newline();
        self.write(")");
    }

    fn fmt_binary_expr(&mut self, node: &SyntaxNode) {
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE => {}
                    k if is_binary_op(k) => {
                        self.space();
                        self.write(t.text());
                        self.space();
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => self.fmt_node(n),
            }
        }
    }

    fn fmt_prefix_expr(&mut self, node: &SyntaxNode) {
        // !expr or -expr (no space between operator and operand)
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) if t.kind() == SyntaxKind::WHITESPACE => {}
                NodeOrToken::Token(t) => self.write(t.text()),
                NodeOrToken::Node(n) => self.fmt_node(n),
            }
        }
    }

    fn fmt_field_expr(&mut self, node: &SyntaxNode) {
        // expr.field (no spaces around dot)
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) if t.kind() == SyntaxKind::WHITESPACE => {}
                NodeOrToken::Token(t) => self.write(t.text()),
                NodeOrToken::Node(n) => self.fmt_node(n),
            }
        }
    }

    fn fmt_index_expr(&mut self, node: &SyntaxNode) {
        // expr[index] (no spaces around brackets)
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) if t.kind() == SyntaxKind::WHITESPACE => {}
                NodeOrToken::Token(t) => self.write(t.text()),
                NodeOrToken::Node(n) => self.fmt_node(n),
            }
        }
    }

    fn fmt_closure_expr(&mut self, node: &SyntaxNode) {
        // (params) => body
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE => {}
                    SyntaxKind::FAT_ARROW => {
                        self.space();
                        self.write("=>");
                        self.space();
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => self.fmt_node(n),
            }
        }
    }

    fn fmt_with_expr(&mut self, node: &SyntaxNode) {
        // expr with { updates }
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE => {}
                    SyntaxKind::WITH_KW => {
                        self.space();
                        self.write("with");
                        self.space();
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => self.fmt_node(n),
            }
        }
    }

    fn fmt_struct_literal_expr(&mut self, node: &SyntaxNode) {
        // TypeName { field: value, ... }
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE => {}
                    SyntaxKind::DOT => self.write("."),
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => match n.kind() {
                    SyntaxKind::STRUCT_LITERAL_BODY => {
                        self.space();
                        self.fmt_struct_literal_body(n);
                    }
                    _ => self.fmt_node(n),
                },
            }
        }
    }

    fn fmt_struct_literal_body(&mut self, node: &SyntaxNode) {
        // { field: value, ... } — multi-line for multiple fields, inline for single/none
        let field_count = node
            .children()
            .filter(|n| {
                matches!(
                    n.kind(),
                    SyntaxKind::STRUCT_LITERAL_FIELD | SyntaxKind::UPDATE_FIELD
                )
            })
            .count();

        if field_count <= 1 {
            // Inline
            self.fmt_braces_inline(node);
        } else {
            self.fmt_braces_multiline(node);
        }
    }

    fn fmt_brace_literal(&mut self, node: &SyntaxNode) {
        let field_count = node.children().count();
        if field_count <= 1 {
            self.fmt_braces_inline(node);
        } else {
            self.fmt_braces_multiline(node);
        }
    }

    fn fmt_braces_inline(&mut self, node: &SyntaxNode) {
        self.write("{");
        let mut needs_space = false;
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::L_CURLY | SyntaxKind::R_CURLY | SyntaxKind::WHITESPACE => {}
                    SyntaxKind::COMMA => {
                        self.write(",");
                        needs_space = true;
                    }
                    _ => {
                        if needs_space {
                            self.space();
                            needs_space = false;
                        }
                        self.write(t.text());
                    }
                },
                NodeOrToken::Node(n) => {
                    self.space();
                    self.fmt_inline(n);
                    needs_space = false;
                }
            }
        }
        self.space();
        self.write("}");
    }

    fn fmt_braces_multiline(&mut self, node: &SyntaxNode) {
        self.write("{");
        self.inc_indent();

        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::L_CURLY
                    | SyntaxKind::R_CURLY
                    | SyntaxKind::WHITESPACE
                    | SyntaxKind::COMMA => {}
                    SyntaxKind::LINE_COMMENT | SyntaxKind::DOC_COMMENT => {
                        self.newline();
                        self.write(t.text().trim_end());
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => {
                    self.newline();
                    self.fmt_inline(n);
                    self.write(",");
                }
            }
        }

        self.dec_indent();
        self.newline();
        self.write("}");
    }

    fn fmt_map_expr(&mut self, node: &SyntaxNode) {
        let field_count = node.children().count();
        if field_count <= 1 {
            self.fmt_map_inline(node);
        } else {
            self.fmt_map_multiline(node);
        }
    }

    fn fmt_map_inline(&mut self, node: &SyntaxNode) {
        self.write("%{");
        let mut needs_space = false;
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::PERCENT_L_CURLY | SyntaxKind::R_CURLY | SyntaxKind::WHITESPACE => {}
                    SyntaxKind::COMMA => {
                        self.write(",");
                        needs_space = true;
                    }
                    _ => {
                        if needs_space {
                            self.space();
                            needs_space = false;
                        }
                        self.write(t.text());
                    }
                },
                NodeOrToken::Node(n) => {
                    self.space();
                    self.fmt_inline(n);
                    needs_space = false;
                }
            }
        }
        self.space();
        self.write("}");
    }

    fn fmt_map_multiline(&mut self, node: &SyntaxNode) {
        self.write("%{");
        self.inc_indent();

        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::PERCENT_L_CURLY
                    | SyntaxKind::R_CURLY
                    | SyntaxKind::WHITESPACE
                    | SyntaxKind::COMMA => {}
                    SyntaxKind::LINE_COMMENT | SyntaxKind::DOC_COMMENT => {
                        self.newline();
                        self.write(t.text().trim_end());
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => {
                    self.newline();
                    self.fmt_inline(n);
                    self.write(",");
                }
            }
        }

        self.dec_indent();
        self.newline();
        self.write("}");
    }

    fn fmt_bracket_list(&mut self, node: &SyntaxNode) {
        // [items] — inline for short, multi-line for long
        let item_count = node.children().count();
        if item_count <= 3 {
            self.fmt_inline_list(node, SyntaxKind::L_BRACKET, SyntaxKind::R_BRACKET);
        } else {
            self.fmt_multiline_list(node, SyntaxKind::L_BRACKET, SyntaxKind::R_BRACKET);
        }
    }

    fn fmt_inline_list(&mut self, node: &SyntaxNode, _open: SyntaxKind, _close: SyntaxKind) {
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE => {}
                    SyntaxKind::COMMA => {
                        self.write(",");
                        self.space();
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => self.fmt_node(n),
            }
        }
    }

    fn fmt_multiline_list(&mut self, node: &SyntaxNode, _open: SyntaxKind, close: SyntaxKind) {
        let mut started = false;
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE | SyntaxKind::COMMA => {}
                    k if k == close => {}
                    SyntaxKind::L_BRACKET | SyntaxKind::L_PAREN => {
                        self.write(t.text());
                        self.inc_indent();
                        started = true;
                    }
                    SyntaxKind::LINE_COMMENT | SyntaxKind::DOC_COMMENT => {
                        self.space();
                        self.write(t.text().trim_end());
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => {
                    self.newline();
                    self.fmt_node(n);
                    self.write(",");
                }
            }
        }
        let _ = started;
        self.dec_indent();
        self.newline();
        self.write(if close == SyntaxKind::R_BRACKET {
            "]"
        } else {
            ")"
        });
    }

    // ── Statements ───────────────────────────────────────────────────

    fn fmt_let_stmt(&mut self, node: &SyntaxNode) {
        // let pattern [: Type] = expr
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE => {}
                    SyntaxKind::LINE_COMMENT | SyntaxKind::DOC_COMMENT => {
                        self.write(t.text().trim_end());
                        self.newline();
                    }
                    SyntaxKind::LET_KW => self.write("let"),
                    SyntaxKind::EQ => {
                        self.space();
                        self.write("=");
                        self.space();
                    }
                    SyntaxKind::COLON => {
                        self.write(":");
                        self.space();
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => match n.kind() {
                    SyntaxKind::BINDING_PATTERN => {
                        self.space();
                        self.fmt_node(n);
                    }
                    SyntaxKind::SLOT_TYPE => self.fmt_slot_type(n),
                    _ => self.fmt_node(n),
                },
            }
        }
    }

    fn fmt_return_stmt(&mut self, node: &SyntaxNode) {
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE => {}
                    SyntaxKind::RETURN_KW => self.write("return"),
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => {
                    self.space();
                    self.fmt_node(n);
                }
            }
        }
    }

    fn fmt_fail_expr(&mut self, node: &SyntaxNode) {
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE => {}
                    SyntaxKind::FAIL_KW => self.write("fail"),
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => {
                    self.space();
                    self.fmt_node(n);
                }
            }
        }
    }

    // ── Generic helpers ──────────────────────────────────────────────

    /// Format a node inline, emitting all children with minimal spacing.
    /// Skips whitespace, adds spaces around colons and equals.
    fn fmt_inline(&mut self, node: &SyntaxNode) {
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE => {}
                    SyntaxKind::LINE_COMMENT | SyntaxKind::DOC_COMMENT => {
                        self.space();
                        self.write(t.text().trim_end());
                        self.newline();
                    }
                    SyntaxKind::BLOCK_COMMENT => {
                        self.space();
                        self.write(t.text().trim_end());
                        self.space();
                    }
                    SyntaxKind::COMMA => {
                        self.write(",");
                        self.space();
                    }
                    SyntaxKind::COLON => {
                        self.write(":");
                        self.space();
                    }
                    SyntaxKind::EQ => {
                        self.space();
                        self.write("=");
                        self.space();
                    }
                    SyntaxKind::FAT_ARROW => {
                        self.space();
                        self.write("=>");
                        self.space();
                    }
                    SyntaxKind::THIN_ARROW => {
                        self.space();
                        self.write("->");
                        self.space();
                    }
                    SyntaxKind::PIPE
                        if node.kind() == SyntaxKind::SUM_TYPE
                            || node.kind() == SyntaxKind::OR_PATTERN =>
                    {
                        self.space();
                        self.write("|");
                        self.space();
                    }
                    k if is_binary_op(k) && node.kind() == SyntaxKind::BINARY_EXPR => {
                        self.space();
                        self.write(t.text());
                        self.space();
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => self.fmt_node(n),
            }
        }
    }

    /// Comma-separated inline: a, b, c
    fn fmt_comma_separated_inline(&mut self, node: &SyntaxNode) {
        for child in node.children_with_tokens() {
            match &child {
                NodeOrToken::Token(t) => match t.kind() {
                    SyntaxKind::WHITESPACE => {}
                    SyntaxKind::COMMA => {
                        self.write(",");
                        self.space();
                    }
                    _ => self.write(t.text()),
                },
                NodeOrToken::Node(n) => self.fmt_node(n),
            }
        }
    }

    /// Emit a node completely verbatim (for interpolated strings etc.)
    fn fmt_verbatim(&mut self, node: &SyntaxNode) {
        self.write(&node.text().to_string());
    }
}

fn is_binary_op(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::PLUS
            | SyntaxKind::MINUS
            | SyntaxKind::STAR
            | SyntaxKind::SLASH
            | SyntaxKind::PERCENT
            | SyntaxKind::EQ2
            | SyntaxKind::BANG_EQ
            | SyntaxKind::LT
            | SyntaxKind::LT_EQ
            | SyntaxKind::GT
            | SyntaxKind::GT_EQ
            | SyntaxKind::AMP2
            | SyntaxKind::PIPE2
    )
}

fn collect_call_parts(node: &SyntaxNode) -> Vec<SyntaxNode> {
    fn walk(node: &SyntaxNode, out: &mut Vec<SyntaxNode>) {
        if node.kind() == SyntaxKind::CALL_EXPR {
            let mut expr_children = node
                .children()
                .filter(|child| crate::Expr::can_cast(child.kind()));
            if let Some(callee) = expr_children.next() {
                walk(&callee, out);
            }
            if let Some(type_args) = node
                .children()
                .find(|child| child.kind() == SyntaxKind::TYPE_ARGS)
            {
                out.push(type_args);
            }
            if let Some(arg) = expr_children.next() {
                out.push(arg);
            }
        } else {
            out.push(node.clone());
        }
    }

    let mut out = Vec::new();
    walk(node, &mut out);
    out
}
