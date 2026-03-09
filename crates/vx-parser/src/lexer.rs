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
}

#[derive(Debug, Clone, Copy)]
enum Mode {
    Template,
    TemplateExpr { brace_depth: usize },
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            pos: 0,
            tokens: Vec::new(),
            modes: Vec::new(),
        }
    }

    fn lex(mut self) -> Vec<Token> {
        while !self.is_eof() {
            let start = self.pos;
            let kind = match self.modes.last().copied() {
                Some(Mode::Template) => self.template_chunk(),
                Some(Mode::TemplateExpr { .. }) => self.template_expr_token(),
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

    fn normal_token(&mut self) -> Option<SyntaxKind> {
        let kind = match self.current_char() {
            // r[lex.whitespace]
            Some(ch) if ch.is_whitespace() => {
                self.bump_char();
                while matches!(self.current_char(), Some(next) if next.is_whitespace()) {
                    self.bump_char();
                }
                SyntaxKind::WHITESPACE
            }
            Some('/') if self.starts_with("//") => self.line_comment(), // r[lex.comment.line]
            Some('/') if self.starts_with("/*") => self.block_comment(), // r[lex.comment.block]
            Some('"') => self.string(),                                 // r[lex.string]
            // r[lex.interp-string]
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
            Some('+') => self.bump_one(SyntaxKind::PLUS),
            Some('-') if self.starts_with("->") => self.bump_two(SyntaxKind::THIN_ARROW),
            Some('-') => self.bump_one(SyntaxKind::MINUS),
            Some('*') => self.bump_one(SyntaxKind::STAR),
            Some('%') => self.bump_one(SyntaxKind::PERCENT),
            Some('&') if self.starts_with("&&") => self.bump_two(SyntaxKind::AMP2),
            Some('|') if self.starts_with("||") => self.bump_two(SyntaxKind::PIPE2),
            Some('|') => self.bump_one(SyntaxKind::PIPE),
            Some('@') => self.bump_one(SyntaxKind::AT),
            Some('#') => self.bump_one(SyntaxKind::HASH),
            Some('?') => self.bump_one(SyntaxKind::QUESTION),
            Some('/') => self.bump_one(SyntaxKind::SLASH),
            Some(_) => {
                self.bump_char();
                SyntaxKind::ERROR_TOKEN
            }
            None => return None,
        };
        Some(kind)
    }

    // r[lex.interp-string]
    fn template_chunk(&mut self) -> SyntaxKind {
        if self.current_char() == Some('`') {
            self.bump_char();
            self.modes.pop();
            return SyntaxKind::BACKTICK;
        }
        if self.starts_with("${") {
            self.pos += 2;
            self.modes.push(Mode::TemplateExpr { brace_depth: 0 });
            return SyntaxKind::INTERP_START;
        }

        while let Some(ch) = self.current_char() {
            if ch == '`' || self.starts_with("${") {
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

    // r[lex.interp-string]
    fn template_expr_token(&mut self) -> SyntaxKind {
        if self.current_char() == Some('`') {
            self.bump_char();
            self.modes.push(Mode::Template);
            return SyntaxKind::BACKTICK;
        }

        match self.current_char() {
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

    // r[lex.comment.line]
    fn line_comment(&mut self) -> SyntaxKind {
        self.pos += 2;
        while let Some(ch) = self.current_char() {
            if ch == '\n' {
                break;
            }
            self.bump_char();
        }
        SyntaxKind::LINE_COMMENT
    }

    // r[lex.comment.block]
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
            "is" => SyntaxKind::IS_KW,
            "as" => SyntaxKind::AS_KW,
            "import" => SyntaxKind::IMPORT_KW,
            "return" => SyntaxKind::RETURN_KW,
            "fail" => SyntaxKind::FAIL_KW,
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
