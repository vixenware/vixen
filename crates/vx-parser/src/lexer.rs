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
    /// The body of a `"…"` interpolated string: literal text plus `$name` /
    /// `${ expr }` holes, until the closing `"`.
    Str,
    /// A `${ … }` interpolation expression. Popping it returns to the enclosing
    /// string / command mode — the mode stack remembers which.
    Interp { brace_depth: usize },
    /// The body of a `` `…` `` command invocation: command words, quoted-string
    /// args, and `$name` / `${ }` holes, until the closing backtick.
    Command,
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
        self.frontmatter();
        while !self.is_eof() {
            let start = self.pos;
            let kind = match self.modes.last().copied() {
                Some(Mode::Str) => self.str_chunk(),
                Some(Mode::Interp { .. }) => self.interp_expr_token(),
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

    // r[expr.command] — one piece of a command-invocation body: the closing
    // backtick, whitespace, a quoted-string arg, a `${ expr }` hole (delegates to
    // interp-expr mode), a `$name` hole, or a bare command word (backslash-escaped).
    fn command_chunk(&mut self) -> SyntaxKind {
        match self.current_char() {
            Some('`') => {
                self.bump_char();
                self.modes.pop();
                SyntaxKind::BACKTICK
            }
            Some(c) if c.is_whitespace() => {
                self.bump_char();
                while matches!(self.current_char(), Some(c) if c.is_whitespace()) {
                    self.bump_char();
                }
                SyntaxKind::WHITESPACE
            }
            Some('"') => self.string(),
            Some('\'') => self.raw_string(),
            Some('$') if self.peek_char() == Some('{') => {
                self.pos += 2; // consume "${"
                self.modes.push(Mode::Interp { brace_depth: 0 });
                SyntaxKind::INTERP_START
            }
            Some('$') if matches!(self.peek_char(), Some(c) if is_ident_start(c)) => {
                self.bump_char(); // '$'
                while matches!(self.current_char(), Some(c) if is_ident_continue(c)) {
                    self.bump_char();
                }
                SyntaxKind::CLI_INTERP_VAR
            }
            // A bare word runs until whitespace / `` ` `` / `"` / `'` / `$`; `\`
            // escapes the next char. A lone `$` starting no hole is a one-char word.
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
                        Some(c)
                            if c.is_whitespace()
                                || c == '`'
                                || c == '"'
                                || c == '\''
                                || c == '$' =>
                        {
                            break;
                        }
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
            // r[lex.string] / r[lex.string.interpolation] — a `"…"` that contains a
            // `$name` / `${ }` hole opens an interpolated string (DQUOTE + Str mode);
            // a `"…"` with no hole is a plain string; `'…'` is a raw string.
            Some('"') if self.dquote_has_interp() => {
                self.bump_char();
                self.modes.push(Mode::Str);
                SyntaxKind::DQUOTE
            }
            Some('"') => self.string(),
            Some('\'') => self.raw_string(),
            // r[expr.command] — a single backtick opens a command invocation.
            Some('`') => {
                self.bump_char();
                self.modes.push(Mode::Command);
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

    // r[lex.string.interpolation] — one piece of a `"…"` body: the closing quote,
    // a `${ expr }` hole, a `$name` hole, or a run of literal text.
    fn str_chunk(&mut self) -> SyntaxKind {
        match self.current_char() {
            Some('"') => {
                self.bump_char();
                self.modes.pop();
                SyntaxKind::DQUOTE
            }
            Some('$') if self.peek_char() == Some('{') => {
                self.pos += 2; // consume "${"
                self.modes.push(Mode::Interp { brace_depth: 0 });
                SyntaxKind::INTERP_START
            }
            Some('$') if matches!(self.peek_char(), Some(c) if is_ident_start(c)) => {
                self.bump_char(); // '$'
                while matches!(self.current_char(), Some(c) if is_ident_continue(c)) {
                    self.bump_char();
                }
                SyntaxKind::INTERP_VAR
            }
            // A run of literal text up to the next `"` or interpolating `$`. `\`
            // escapes the next char; a `$` that starts no hole is literal text.
            _ => {
                loop {
                    match self.current_char() {
                        Some('"') => break,
                        Some('\\') => {
                            self.bump_char();
                            if !self.is_eof() {
                                self.bump_char();
                            }
                        }
                        Some('$')
                            if self.peek_char() == Some('{')
                                || matches!(self.peek_char(), Some(c) if is_ident_start(c)) =>
                        {
                            break;
                        }
                        Some(_) => self.bump_char(),
                        None => break,
                    }
                }
                SyntaxKind::INTERP_TEXT
            }
        }
    }

    // r[lex.string.interpolation] — a token inside a `${ … }` interpolation. Brace
    // depth tracks nested `{ }`; the matching `}` closes the hole, popping back to
    // the enclosing string/command mode. Nested `"…"` / `` `…` `` recurse through
    // `normal_token` (which pushes the right mode).
    fn interp_expr_token(&mut self) -> SyntaxKind {
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
                .expect("interp expr mode should only run before eof"),
        }
    }

    fn template_brace_depth(&self) -> usize {
        match self.modes.last().copied() {
            Some(Mode::Interp { brace_depth }) => brace_depth,
            _ => 0,
        }
    }

    fn bump_template_brace_depth(&mut self, delta: isize) {
        if let Some(Mode::Interp { brace_depth }) = self.modes.last_mut() {
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

    // Does the `"…"` starting at the cursor contain an interpolation hole — a `$`
    // followed by `{` or an identifier start, not backslash-escaped? Decides whether
    // `"…"` is a plain string (`STRING`) or an interpolated one (`DQUOTE` + body).
    fn dquote_has_interp(&self) -> bool {
        let mut chars = self.source[self.pos + 1..].chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '"' => return false,
                '\\' => {
                    chars.next();
                }
                '$' if matches!(chars.peek(), Some('{'))
                    || matches!(chars.peek(), Some(&n) if is_ident_start(n)) =>
                {
                    return true;
                }
                _ => {}
            }
        }
        false // unterminated → treat as plain (string() reports the error)
    }

    // r[lex.string] — a raw (single-quoted) string: no interpolation, only `\'`
    // and `\\` escape; every other character, including `$` and `` ` ``, is literal.
    fn raw_string(&mut self) -> SyntaxKind {
        self.bump_char();
        while let Some(ch) = self.current_char() {
            match ch {
                '\'' => {
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
