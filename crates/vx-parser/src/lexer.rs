use rowan::{TextRange, TextSize};

use crate::SyntaxKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: SyntaxKind,
    pub range: TextRange,
}

pub fn lex(source: &str) -> Vec<Token> {
    Lexer::new(source).lex()
}

struct Lexer<'a> {
    source: &'a str,
    pos: usize,
    tokens: Vec<Token>,
    modes: Vec<Mode>,
    /// Tracks the `@ ident [` CLI-literal opener across iterations so the opener
    /// bracket can be emitted as `CLI_OPEN` (and command mode entered).
    cli: CliOpen,
}

#[derive(Debug, Clone, Copy)]
enum Mode {
    Template,
    TemplateExpr {
        brace_depth: usize,
    },
    /// The body of a CLI literal (`@cc[ … ]`): command words, quoted strings,
    /// and `$`/`${ }` interpolation, until the matching `]`.
    Command,
}

/// Progress through a CLI-literal opener. `@` (recognised as an opener) → `AfterAt`;
/// the grammar identifier → `AfterIdent`; the `[` → emit `CLI_OPEN`, enter command mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliOpen {
    None,
    AfterAt,
    AfterIdent,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            pos: 0,
            tokens: Vec::new(),
            modes: Vec::new(),
            cli: CliOpen::None,
        }
    }

    fn lex(mut self) -> Vec<Token> {
        self.frontmatter();
        while !self.is_eof() {
            let start = self.pos;
            let kind = match self.modes.last().copied() {
                Some(Mode::Template) => self.template_chunk(),
                Some(Mode::TemplateExpr { .. }) => self.template_expr_token(),
                Some(Mode::Command) => self.command_chunk(),
                None => match self.normal_token() {
                    Some(kind) => kind,
                    None => break,
                },
            };
            if self.pos == start {
                break;
            }
            self.tokens.push(Token {
                kind,
                range: range(start, self.pos),
            });
        }
        self.tokens
    }

    // r[lex.frontmatter] — a leading ```lang … ``` fence at the very top of the
    // file is package frontmatter: one trivia token spanning open fence to close.
    fn frontmatter(&mut self) {
        if self.pos != 0 || !self.source.starts_with("```") {
            return;
        }
        let Some(open_line) = self.source.find('\n') else {
            return;
        };
        let Some(rel) = self.source[open_line + 1..].find("```") else {
            return;
        };
        let close = open_line + 1 + rel + 3;
        let end = self.source[close..]
            .find('\n')
            .map_or(self.source.len(), |n| close + n + 1);
        self.tokens.push(Token {
            kind: SyntaxKind::FRONTMATTER,
            range: range(0, end),
        });
        self.pos = end;
    }

    // r[lex.cli-literal] — does `@` at the cursor open a CLI literal? Either
    // `@[` (grammar-less; the grammar comes from the leading hole) or `@<ident>[`
    // (named grammar), with no gaps. Distinguishes it from a path literal / bare `@`.
    fn is_cli_opener(&self) -> bool {
        let Some(after_at) = self.source[self.pos..].strip_prefix('@') else {
            return false;
        };
        if after_at.starts_with('[') {
            return true;
        }
        if !matches!(after_at.chars().next(), Some(c) if is_ident_start(c)) {
            return false;
        }
        let ident_end = after_at
            .char_indices()
            .take_while(|(_, c)| is_ident_continue(*c))
            .map(|(i, c)| i + c.len_utf8())
            .last()
            .unwrap_or(0);
        after_at[ident_end..].starts_with('[')
    }

    // r[lex.cli-literal] — one piece of a CLI-literal body: the closing `]`,
    // whitespace, a quoted string, a `${ expr }` hole (delegates to template-expr
    // mode), a `$ident` interpolation, or a bare command word (backslash-escaped).
    fn command_chunk(&mut self) -> SyntaxKind {
        match self.current_char() {
            Some(']') => {
                self.bump_char();
                self.modes.pop();
                SyntaxKind::R_BRACKET
            }
            Some(c) if c.is_whitespace() => {
                self.bump_char();
                while matches!(self.current_char(), Some(c) if c.is_whitespace()) {
                    self.bump_char();
                }
                SyntaxKind::WHITESPACE
            }
            Some('"') => self.string(),
            Some('$') if self.peek_char() == Some('{') => {
                self.pos += 2; // consume "${"
                self.modes.push(Mode::TemplateExpr { brace_depth: 0 });
                SyntaxKind::INTERP_START
            }
            Some('$') if matches!(self.peek_char(), Some(c) if is_ident_start(c)) => {
                self.bump_char(); // '$'
                while matches!(self.current_char(), Some(c) if is_ident_continue(c)) {
                    self.bump_char();
                }
                SyntaxKind::CLI_INTERP_VAR
            }
            // A bare word runs until whitespace / `]` / `"` / `$`; `\` escapes the
            // next char (so `\]`, `\$`, `\"` stay in the word). A lone `$` that
            // started neither form above is itself a one-char word.
            Some('$') => {
                self.bump_char();
                SyntaxKind::CLI_WORD
            }
            Some(_) => {
                loop {
                    match self.current_char() {
                        Some('\\') => {
                            self.bump_char();
                            if !self.is_eof() {
                                self.bump_char();
                            }
                        }
                        Some(c) if c.is_whitespace() || c == ']' || c == '"' || c == '$' => break,
                        Some(_) => self.bump_char(),
                        None => break,
                    }
                }
                SyntaxKind::CLI_WORD
            }
            None => SyntaxKind::CLI_WORD, // EOF inside the body: no advance ⇒ lexer loop stops
        }
    }

    fn normal_token(&mut self) -> Option<SyntaxKind> {
        // r[lex.cli-literal] — drive the contiguous `@ [` / `@ ident [` opener:
        // after a recognised `@`, either `[` opens the body directly (grammar-less
        // `@[`), or the grammar name comes first and then `[` becomes CLI_OPEN.
        // Either way the body enters command mode. (is_cli_opener guarantees the
        // sequence, so this always completes.)
        match self.cli {
            CliOpen::AfterAt if self.current_char() == Some('[') => {
                self.cli = CliOpen::None;
                self.bump_char();
                self.modes.push(Mode::Command);
                return Some(SyntaxKind::CLI_OPEN);
            }
            CliOpen::AfterAt if matches!(self.current_char(), Some(c) if is_ident_start(c)) => {
                self.cli = CliOpen::AfterIdent;
                return Some(self.ident_or_keyword());
            }
            CliOpen::AfterIdent if self.current_char() == Some('[') => {
                self.cli = CliOpen::None;
                self.bump_char();
                self.modes.push(Mode::Command);
                return Some(SyntaxKind::CLI_OPEN);
            }
            CliOpen::None => {}
            _ => self.cli = CliOpen::None,
        }
        let kind = match self.current_char() {
            // r[lex.whitespace]
            Some(ch) if ch.is_whitespace() => {
                self.bump_char();
                while matches!(self.current_char(), Some(next) if next.is_whitespace()) {
                    self.bump_char();
                }
                SyntaxKind::WHITESPACE
            }
            Some('/') if self.starts_with("//") => self.line_comment(), // r[lex.comments]
            Some('/') if self.starts_with("/*") => self.block_comment(), // r[lex.comments]
            Some('"') => self.string(),                                 // r[lex.string]
            // r[lex.string.interpolation]
            Some('`') => {
                self.bump_char();
                self.modes.push(Mode::Template);
                SyntaxKind::BACKTICK
            }
            Some('$') => self.dollar_ident(), // r[lex.dollar-ident]
            Some(ch) if is_ident_start(ch) => self.ident_or_keyword(), // r[lex.ident]
            Some(ch) if ch.is_ascii_digit() => self.number(), // r[lex.numeric]
            // r[lex.delimiters]
            Some('(') => self.bump_one(SyntaxKind::L_PAREN),
            Some(')') => self.bump_one(SyntaxKind::R_PAREN),
            Some('[') => self.bump_one(SyntaxKind::L_BRACKET),
            Some(']') => self.bump_one(SyntaxKind::R_BRACKET),
            Some('{') => self.bump_one(SyntaxKind::L_CURLY),
            Some('}') => self.bump_one(SyntaxKind::R_CURLY),
            // r[lex.operators]
            Some(',') => self.bump_one(SyntaxKind::COMMA),
            Some(';') => self.bump_one(SyntaxKind::SEMICOLON),
            Some(':') if self.starts_with("::") => self.bump_two(SyntaxKind::COLON2),
            Some(':') => self.bump_one(SyntaxKind::COLON),
            Some('.') if self.starts_with("..=") => self.bump_three(SyntaxKind::DOT2_EQ),
            Some('.') if self.starts_with("..") => self.bump_two(SyntaxKind::DOT2),
            Some('.') => self.bump_one(SyntaxKind::DOT),
            Some('=') if self.starts_with("=>") => self.bump_two(SyntaxKind::FAT_ARROW),
            Some('=') if self.starts_with("==") => self.bump_two(SyntaxKind::EQ2),
            Some('=') => self.bump_one(SyntaxKind::EQ),
            Some('!') if self.starts_with("!=") => self.bump_two(SyntaxKind::BANG_EQ),
            Some('!') => self.bump_one(SyntaxKind::BANG),
            Some('<') if self.starts_with("<=") => self.bump_two(SyntaxKind::LT_EQ),
            Some('<') => self.bump_one(SyntaxKind::LT),
            Some('>') if self.starts_with(">=") => self.bump_two(SyntaxKind::GT_EQ),
            Some('>') => self.bump_one(SyntaxKind::GT),
            Some('+') if self.starts_with("++") => self.bump_two(SyntaxKind::PLUS2),
            Some('+') => self.bump_one(SyntaxKind::PLUS),
            Some('-') if self.starts_with("->") => self.bump_two(SyntaxKind::THIN_ARROW),
            Some('-') => self.bump_one(SyntaxKind::MINUS),
            Some('*') => self.bump_one(SyntaxKind::STAR),
            Some('%') if self.starts_with("%{") => self.bump_two(SyntaxKind::PERCENT_L_CURLY),
            Some('%') => self.bump_one(SyntaxKind::PERCENT),
            Some('&') if self.starts_with("&&") => self.bump_two(SyntaxKind::AMP2),
            Some('|') if self.starts_with("|>") => self.bump_two(SyntaxKind::PIPE_GT),
            Some('|') if self.starts_with("||") => self.bump_two(SyntaxKind::PIPE2),
            Some('|') => self.bump_one(SyntaxKind::PIPE),
            Some('@') if self.is_cli_opener() => {
                self.bump_char();
                self.cli = CliOpen::AfterAt;
                SyntaxKind::AT
            }
            Some('@') => self.bump_one(SyntaxKind::AT),
            Some('#') => self.bump_one(SyntaxKind::HASH),
            Some('?') => self.bump_one(SyntaxKind::QUESTION),
            Some('/') => self.bump_one(SyntaxKind::SLASH),
            // r[lex.bomb] — the demand-driven tripwire literal
            Some('💣') => self.bump_one(SyntaxKind::BOMB),
            Some(_) => {
                self.bump_char();
                SyntaxKind::ERROR_TOKEN
            }
            None => return None,
        };
        Some(kind)
    }

    // r[lex.string.interpolation]
    fn template_chunk(&mut self) -> SyntaxKind {
        if self.current_char() == Some('`') {
            self.bump_char();
            self.modes.pop();
            return SyntaxKind::BACKTICK;
        }
        if self.current_char() == Some('{') {
            self.bump_char();
            self.modes.push(Mode::TemplateExpr { brace_depth: 0 });
            return SyntaxKind::INTERP_START;
        }

        while let Some(ch) = self.current_char() {
            if ch == '`' || ch == '{' {
                break;
            }
            if ch == '\\' {
                self.bump_char();
                if !self.is_eof() {
                    self.bump_char();
                }
            } else {
                self.bump_char();
            }
        }
        SyntaxKind::INTERP_TEXT
    }

    // r[lex.string.interpolation]
    fn template_expr_token(&mut self) -> SyntaxKind {
        if self.current_char() == Some('`') {
            self.bump_char();
            self.modes.push(Mode::Template);
            return SyntaxKind::BACKTICK;
        }

        match self.current_char() {
            Some('%') if self.starts_with("%{") => {
                self.pos += 2;
                self.bump_template_brace_depth(1);
                SyntaxKind::PERCENT_L_CURLY
            }
            Some('{') => {
                self.bump_char();
                self.bump_template_brace_depth(1);
                SyntaxKind::L_CURLY
            }
            Some('}') if self.template_brace_depth() == 0 => {
                self.bump_char();
                self.modes.pop();
                SyntaxKind::INTERP_END
            }
            Some('}') => {
                self.bump_char();
                self.bump_template_brace_depth(-1);
                SyntaxKind::R_CURLY
            }
            _ => self
                .normal_token()
                .expect("template expr mode should only run before eof"),
        }
    }

    fn template_brace_depth(&self) -> usize {
        match self.modes.last().copied() {
            Some(Mode::TemplateExpr { brace_depth }) => brace_depth,
            _ => 0,
        }
    }

    fn bump_template_brace_depth(&mut self, delta: isize) {
        if let Some(Mode::TemplateExpr { brace_depth }) = self.modes.last_mut() {
            if delta.is_positive() {
                *brace_depth += delta as usize;
            } else {
                *brace_depth -= (-delta) as usize;
            };
        }
    }

    // r[lex.comments]
    fn line_comment(&mut self) -> SyntaxKind {
        // r[lex.comments] — `///` is a doc comment, but `////` is not
        let is_doc = self.source[self.pos..].starts_with("///")
            && !self.source[self.pos..].starts_with("////");
        self.pos += 2;
        while let Some(ch) = self.current_char() {
            if ch == '\n' {
                break;
            }
            self.bump_char();
        }
        if is_doc {
            SyntaxKind::DOC_COMMENT
        } else {
            SyntaxKind::LINE_COMMENT
        }
    }

    // r[lex.comments]
    fn block_comment(&mut self) -> SyntaxKind {
        self.pos += 2;
        let mut depth = 1usize;
        while depth > 0 && !self.is_eof() {
            if self.starts_with("/*") {
                self.pos += 2;
                depth += 1;
            } else if self.starts_with("*/") {
                self.pos += 2;
                depth -= 1;
            } else {
                self.bump_char();
            }
        }
        SyntaxKind::BLOCK_COMMENT
    }

    // r[lex.string]
    fn string(&mut self) -> SyntaxKind {
        self.bump_char();
        while let Some(ch) = self.current_char() {
            match ch {
                '"' => {
                    self.bump_char();
                    return SyntaxKind::STRING;
                }
                '\\' => {
                    self.bump_char();
                    if !self.is_eof() {
                        self.bump_char();
                    }
                }
                _ => self.bump_char(),
            }
        }
        SyntaxKind::ERROR_TOKEN
    }

    // r[lex.ident]
    fn ident_or_keyword(&mut self) -> SyntaxKind {
        let start = self.pos;
        self.bump_char();
        while matches!(self.current_char(), Some(ch) if is_ident_continue(ch)) {
            self.bump_char();
        }
        // r[lex.keywords]
        match &self.source[start..self.pos] {
            "fn" => SyntaxKind::FN_KW,
            "let" => SyntaxKind::LET_KW,
            "type" => SyntaxKind::TYPE_KW,
            "namespace" => SyntaxKind::NAMESPACE_KW,
            "struct" => SyntaxKind::STRUCT_KW,
            "enum" => SyntaxKind::ENUM_KW,
            "newtype" => SyntaxKind::NEWTYPE_KW,
            "of" => SyntaxKind::OF_KW,
            "match" => SyntaxKind::MATCH_KW,
            "if" => SyntaxKind::IF_KW,
            "else" => SyntaxKind::ELSE_KW,
            "with" if self.current_char() == Some('=') => {
                self.bump_char();
                SyntaxKind::WITH_EQ
            }
            "with" => SyntaxKind::WITH_KW,
            "where" => SyntaxKind::WHERE_KW,
            "wherein" => SyntaxKind::WHEREIN_KW,
            "is" => SyntaxKind::IS_KW,
            "as" => SyntaxKind::AS_KW,
            "import" => SyntaxKind::IMPORT_KW,
            "return" => SyntaxKind::RETURN_KW,
            "fail" => SyntaxKind::FAIL_KW,
            "partial" => SyntaxKind::PARTIAL_KW,
            "true" => SyntaxKind::TRUE_KW,
            "false" => SyntaxKind::FALSE_KW,
            "Fn" => SyntaxKind::FN_TYPE_KW,
            "_" => SyntaxKind::UNDERSCORE,
            text if text.chars().next().is_some_and(|ch| ch.is_uppercase()) => {
                SyntaxKind::UPPER_IDENT
            }
            _ => SyntaxKind::LOWER_IDENT,
        }
    }

    // r[lex.dollar-ident]
    fn dollar_ident(&mut self) -> SyntaxKind {
        self.bump_char();
        while matches!(self.current_char(), Some(ch) if ch.is_ascii_digit()) {
            self.bump_char();
        }
        SyntaxKind::DOLLAR_IDENT
    }

    // r[lex.numeric]
    fn number(&mut self) -> SyntaxKind {
        self.bump_char();
        while matches!(self.current_char(), Some(ch) if ch.is_ascii_digit() || ch == '_') {
            self.bump_char();
        }
        if self.current_char() == Some('.')
            && self.peek_char() != Some('.')
            && matches!(self.peek_char(), Some(ch) if ch.is_ascii_digit())
        {
            self.bump_char();
            while matches!(self.current_char(), Some(ch) if ch.is_ascii_digit() || ch == '_') {
                self.bump_char();
            }
            SyntaxKind::FLOAT
        } else {
            SyntaxKind::INT
        }
    }

    fn bump_one(&mut self, kind: SyntaxKind) -> SyntaxKind {
        self.bump_char();
        kind
    }

    fn bump_two(&mut self, kind: SyntaxKind) -> SyntaxKind {
        self.pos += 2;
        kind
    }

    fn bump_three(&mut self, kind: SyntaxKind) -> SyntaxKind {
        self.pos += 3;
        kind
    }

    fn starts_with(&self, needle: &str) -> bool {
        self.source[self.pos..].starts_with(needle)
    }

    fn current_char(&self) -> Option<char> {
        self.source[self.pos..].chars().next()
    }

    fn peek_char(&self) -> Option<char> {
        let mut chars = self.source[self.pos..].chars();
        chars.next()?;
        chars.next()
    }

    fn bump_char(&mut self) {
        if let Some(ch) = self.current_char() {
            self.pos += ch.len_utf8();
        }
    }

    fn is_eof(&self) -> bool {
        self.pos >= self.source.len()
    }
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_alphabetic()
}

fn is_ident_continue(ch: char) -> bool {
    ch == '_' || ch.is_alphanumeric()
}

fn range(start: usize, end: usize) -> TextRange {
    TextRange::new(to_text_size(start), to_text_size(end))
}

fn to_text_size(value: usize) -> TextSize {
    TextSize::from(u32::try_from(value).expect("source offset exceeds u32"))
}
