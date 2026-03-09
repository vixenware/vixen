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
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            pos: 0,
            tokens: Vec::new(),
        }
    }

    fn lex(mut self) -> Vec<Token> {
        while !self.is_eof() {
            let start = self.pos;
            let kind = match self.current_char() {
                Some(ch) if ch.is_whitespace() => {
                    self.bump_char();
                    while matches!(self.current_char(), Some(next) if next.is_whitespace()) {
                        self.bump_char();
                    }
                    SyntaxKind::WHITESPACE
                }
                Some('/') if self.starts_with("//") => self.line_comment(),
                Some('/') if self.starts_with("/*") => self.block_comment(),
                Some('"') => self.string(),
                Some('$') => self.dollar_ident(),
                Some(ch) if is_ident_start(ch) => self.ident_or_keyword(),
                Some(ch) if ch.is_ascii_digit() => self.number(),
                Some('(') => self.bump_one(SyntaxKind::L_PAREN),
                Some(')') => self.bump_one(SyntaxKind::R_PAREN),
                Some('[') => self.bump_one(SyntaxKind::L_BRACKET),
                Some(']') => self.bump_one(SyntaxKind::R_BRACKET),
                Some('{') => self.bump_one(SyntaxKind::L_CURLY),
                Some('}') => self.bump_one(SyntaxKind::R_CURLY),
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
                None => break,
            };
            self.tokens.push(Token {
                kind,
                range: range(start, self.pos),
            });
        }
        self.tokens
    }

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

    fn ident_or_keyword(&mut self) -> SyntaxKind {
        let start = self.pos;
        self.bump_char();
        while matches!(self.current_char(), Some(ch) if is_ident_continue(ch)) {
            self.bump_char();
        }
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
            text if text.chars().next().is_some_and(|ch| ch.is_uppercase()) => {
                SyntaxKind::UPPER_IDENT
            }
            _ => SyntaxKind::LOWER_IDENT,
        }
    }

    fn dollar_ident(&mut self) -> SyntaxKind {
        self.bump_char();
        while matches!(self.current_char(), Some(ch) if ch.is_ascii_digit()) {
            self.bump_char();
        }
        SyntaxKind::DOLLAR_IDENT
    }

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
