use crate::runtime::{PlannerBuiltinId, PlannerCall, PlannerExpr, PlannerProgram, PlannerValue};
use crate::{RuleId, RulePlanProvenance, RulePlanSourceId, RulePlanSpan};
use camino::Utf8Path;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VxParseError {
    pub message: String,
    pub line: u32,
    pub column: u32,
}

impl std::fmt::Display for VxParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (at {}:{})", self.message, self.line, self.column)
    }
}

impl std::error::Error for VxParseError {}

pub fn parse_vx_program(
    source_path: &Utf8Path,
    source_id: RulePlanSourceId,
    input: &str,
) -> Result<PlannerProgram, VxParseError> {
    let _ = source_path;

    let mut lexer = Lexer::new(input);
    let mut tokens = Vec::new();
    loop {
        let token = lexer.next_token()?;
        let at_eof = matches!(token.kind, TokenKind::Eof);
        tokens.push(token);
        if at_eof {
            break;
        }
    }

    Parser::new(tokens, source_id).parse_program()
}

// =============================================================================
// vx-lang v0 lexer
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    kind: TokenKind,
    line: u32,
    column: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TokenKind {
    Ident(String),
    String(String),
    Int(i64),
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Dot,
    Dollar,
    Colon,
    Comma,
    Semi,
    Eq,
    Eof,
}

struct Lexer<'a> {
    chars: std::str::Chars<'a>,
    lookahead: Option<char>,
    offset: usize,
    line: u32,
    column: u32,
}

impl<'a> Lexer<'a> {
    fn new(input: &'a str) -> Self {
        let mut chars = input.chars();
        let lookahead = chars.next();
        Self {
            chars,
            lookahead,
            offset: 0,
            line: 1,
            column: 1,
        }
    }

    fn next_token(&mut self) -> Result<Token, VxParseError> {
        self.skip_ws_and_comments()?;
        let line = self.line;
        let column = self.column;

        let Some(ch) = self.lookahead else {
            return Ok(Token {
                kind: TokenKind::Eof,
                line,
                column,
            });
        };

        let kind = match ch {
            '(' => {
                self.bump();
                TokenKind::LParen
            }
            ')' => {
                self.bump();
                TokenKind::RParen
            }
            '{' => {
                self.bump();
                TokenKind::LBrace
            }
            '}' => {
                self.bump();
                TokenKind::RBrace
            }
            '[' => {
                self.bump();
                TokenKind::LBracket
            }
            ']' => {
                self.bump();
                TokenKind::RBracket
            }
            '.' => {
                self.bump();
                TokenKind::Dot
            }
            '$' => {
                self.bump();
                TokenKind::Dollar
            }
            ':' => {
                self.bump();
                TokenKind::Colon
            }
            ',' => {
                self.bump();
                TokenKind::Comma
            }
            ';' => {
                self.bump();
                TokenKind::Semi
            }
            '=' => {
                self.bump();
                TokenKind::Eq
            }
            '"' => TokenKind::String(self.lex_string()?),
            '-' | '0'..='9' => {
                // Either a negative integer or an identifier starting with '-'.
                if ch == '-' && !self.peek_is_digit() {
                    TokenKind::Ident(self.lex_ident()?)
                } else {
                    TokenKind::Int(self.lex_int()?)
                }
            }
            _ if is_ident_start(ch) => TokenKind::Ident(self.lex_ident()?),
            other => {
                return Err(VxParseError {
                    message: format!("unexpected character '{other}'"),
                    line,
                    column,
                });
            }
        };

        Ok(Token { kind, line, column })
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.lookahead?;
        self.offset += ch.len_utf8();
        if ch == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        self.lookahead = self.chars.next();
        Some(ch)
    }

    fn peek(&self) -> Option<char> {
        self.lookahead
    }

    fn peek_is_digit(&self) -> bool {
        matches!(self.peek_next_char(), Some('0'..='9'))
    }

    fn peek_next_char(&self) -> Option<char> {
        self.chars.clone().next()
    }

    fn lex_ident(&mut self) -> Result<String, VxParseError> {
        let mut s = String::new();
        let Some(first) = self.peek() else {
            return Ok(s);
        };
        if !is_ident_start(first) && first != '-' {
            return Ok(s);
        }
        s.push(first);
        self.bump();
        while let Some(ch) = self.peek() {
            if is_ident_continue(ch) {
                s.push(ch);
                self.bump();
            } else {
                break;
            }
        }
        Ok(s)
    }

    fn lex_int(&mut self) -> Result<i64, VxParseError> {
        let start_line = self.line;
        let start_col = self.column;
        let mut s = String::new();
        if self.peek() == Some('-') {
            s.push('-');
            self.bump();
        }
        while let Some(ch) = self.peek() {
            if ch.is_ascii_digit() {
                s.push(ch);
                self.bump();
            } else {
                break;
            }
        }
        s.parse::<i64>().map_err(|_| VxParseError {
            message: format!("invalid integer literal '{s}'"),
            line: start_line,
            column: start_col,
        })
    }

    fn lex_string(&mut self) -> Result<String, VxParseError> {
        let start_line = self.line;
        let start_col = self.column;
        let mut out = String::new();
        debug_assert_eq!(self.peek(), Some('"'));
        self.bump();

        let mut escape = false;
        while let Some(ch) = self.peek() {
            self.bump();
            if escape {
                out.push(match ch {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    '"' => '"',
                    '\\' => '\\',
                    other => other,
                });
                escape = false;
                continue;
            }
            match ch {
                '\\' => escape = true,
                '"' => return Ok(out),
                other => out.push(other),
            }
        }

        Err(VxParseError {
            message: "unterminated string literal".to_string(),
            line: start_line,
            column: start_col,
        })
    }

    fn skip_ws_and_comments(&mut self) -> Result<(), VxParseError> {
        loop {
            while matches!(self.peek(), Some(ch) if ch.is_whitespace()) {
                self.bump();
            }

            match (self.peek(), self.peek_next_char()) {
                (Some('/'), Some('/')) => {
                    // Line comment.
                    self.bump();
                    self.bump();
                    while let Some(ch) = self.peek() {
                        self.bump();
                        if ch == '\n' {
                            break;
                        }
                    }
                }
                (Some('/'), Some('*')) => {
                    // Block comment.
                    let start_line = self.line;
                    let start_col = self.column;
                    self.bump();
                    self.bump();
                    loop {
                        match (self.peek(), self.peek_next_char()) {
                            (Some('*'), Some('/')) => {
                                self.bump();
                                self.bump();
                                break;
                            }
                            (Some(_), _) => {
                                self.bump();
                            }
                            (None, _) => {
                                return Err(VxParseError {
                                    message: "unterminated block comment".to_string(),
                                    line: start_line,
                                    column: start_col,
                                });
                            }
                        }
                    }
                }
                _ => break,
            }
        }
        Ok(())
    }
}

fn is_ident_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_'
}

fn is_ident_continue(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')
}

// =============================================================================
// vx-lang v0 parser (to PlannerProgram)
// =============================================================================

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    source_id: RulePlanSourceId,
}

impl Parser {
    fn new(tokens: Vec<Token>, source_id: RulePlanSourceId) -> Self {
        Self {
            tokens,
            pos: 0,
            source_id,
        }
    }

    fn parse_program(mut self) -> Result<PlannerProgram, VxParseError> {
        let mut forms = Vec::new();
        while !self.at_eof() {
            if self.eat(TokenKind::Semi) {
                continue;
            }
            forms.push(self.parse_stmt()?);
            self.eat(TokenKind::Semi);
        }
        Ok(PlannerProgram { forms })
    }

    fn parse_stmt(&mut self) -> Result<PlannerExpr, VxParseError> {
        if self.peek_ident_is("let") {
            return self.parse_let();
        }
        if self.peek_ident_is("if") {
            return self.parse_if();
        }
        if self.peek_ident_is("for") {
            return self.parse_for();
        }
        self.parse_expr()
    }

    fn parse_expr(&mut self) -> Result<PlannerExpr, VxParseError> {
        if self.peek_ident_is("if") {
            return self.parse_if();
        }
        if self.peek_ident_is("for") {
            return self.parse_for();
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<PlannerExpr, VxParseError> {
        match self.peek_kind() {
            TokenKind::String(_) => {
                let s = match self.bump().kind {
                    TokenKind::String(s) => s,
                    _ => unreachable!(),
                };
                Ok(PlannerExpr::Value(PlannerValue::String(s)))
            }
            TokenKind::Int(_) => {
                let v = match self.bump().kind {
                    TokenKind::Int(v) => v,
                    _ => unreachable!(),
                };
                Ok(PlannerExpr::Value(PlannerValue::Int(v)))
            }
            TokenKind::Colon => {
                self.bump();
                let (ident, _line, _col) = self.expect_ident("expected identifier after ':'")?;
                Ok(PlannerExpr::Value(PlannerValue::Keyword(ident)))
            }
            TokenKind::Dollar => self.parse_var_ref(),
            TokenKind::Ident(_) => {
                if self.peek_call_like() {
                    self.parse_call()
                } else {
                    let (ident, _line, _column) = self.expect_ident("expected identifier")?;
                    match ident.as_str() {
                        "true" => Ok(PlannerExpr::Value(PlannerValue::Bool(true))),
                        "false" => Ok(PlannerExpr::Value(PlannerValue::Bool(false))),
                        "null" => Ok(PlannerExpr::Value(PlannerValue::Null)),
                        _ => Ok(PlannerExpr::Value(PlannerValue::Keyword(ident))),
                    }
                }
            }
            TokenKind::LBracket => self.parse_vector_literal(),
            TokenKind::LBrace => self.parse_map_literal(),
            _ => Err(self.error_here("expected expression".to_string())),
        }
    }

    fn parse_call(&mut self) -> Result<PlannerExpr, VxParseError> {
        let (name, line, column) = self.expect_qualified_ident("expected builtin name")?;
        let builtin = builtin_from_name(&name).ok_or_else(|| VxParseError {
            message: format!("unknown builtin '{name}'"),
            line,
            column,
        })?;

        self.expect(TokenKind::LParen, "expected '(' after builtin name")?;

        let args = if self.eat(TokenKind::RParen) {
            Vec::new()
        } else if self.peek_is(TokenKind::LBrace) {
            let expr = self.parse_map_literal()?;
            self.expect(TokenKind::RParen, "expected ')' after argument")?;
            vec![expr]
        } else if self.peek_named_arg_list_start() {
            let (arg_line, arg_col) = self.peek_line_col();
            let entries = self.parse_named_arg_entries()?;
            self.expect(TokenKind::RParen, "expected ')' after argument list")?;
            vec![PlannerExpr::MapLiteral {
                entries,
                provenance: self.provenance_script(arg_line, arg_col),
            }]
        } else {
            let expr = self.parse_expr()?;
            if self.eat(TokenKind::Comma) {
                return Err(self
                    .error_here("only one positional argument is supported for now".to_string()));
            }
            self.expect(TokenKind::RParen, "expected ')' after argument")?;
            vec![expr]
        };

        Ok(PlannerExpr::Call(PlannerCall {
            builtin,
            args,
            provenance: RulePlanProvenance {
                rule_id: RuleId::from(format!("rule.vx.{}", builtin.as_str())),
                source_id: self.source_id.clone(),
                span: RulePlanSpan {
                    line,
                    column,
                    end_line: line,
                    end_column: column + 1,
                },
                comment: None,
                metadata: BTreeMap::new(),
            },
        }))
    }

    fn parse_named_arg_entries(&mut self) -> Result<BTreeMap<String, PlannerExpr>, VxParseError> {
        let mut map = BTreeMap::new();
        loop {
            let key = match self.peek_kind() {
                TokenKind::Ident(_) => {
                    let (k, _line, _col) = self.expect_ident("expected argument name")?;
                    k
                }
                TokenKind::String(_) => match self.bump().kind {
                    TokenKind::String(s) => s,
                    _ => unreachable!(),
                },
                _ => return Err(self.error_here("expected argument name".to_string())),
            };
            if self.eat(TokenKind::Eq) {
                return Err(self.error_here("use ':' not '=' after argument name".to_string()));
            }
            self.expect(TokenKind::Colon, "expected ':' after argument name")?;
            let value = self.parse_expr()?;
            map.insert(key, value);

            if self.eat(TokenKind::Comma) {
                if self.peek_is(TokenKind::RParen) {
                    break;
                }
                continue;
            }
            break;
        }
        Ok(map)
    }

    fn parse_var_ref(&mut self) -> Result<PlannerExpr, VxParseError> {
        let tok = self.bump();
        debug_assert!(matches!(tok.kind, TokenKind::Dollar));
        let (name, line, column) = self.expect_ident("expected identifier after '$'")?;
        Ok(PlannerExpr::VarRef {
            name,
            provenance: self.provenance_script(line, column),
        })
    }

    fn parse_vector_literal(&mut self) -> Result<PlannerExpr, VxParseError> {
        let (line, column) = self.peek_line_col();
        self.expect(TokenKind::LBracket, "expected '['")?;
        let mut items = Vec::new();
        if self.eat(TokenKind::RBracket) {
            return Ok(PlannerExpr::VectorLiteral {
                items,
                provenance: self.provenance_script(line, column),
            });
        }
        loop {
            items.push(self.parse_expr()?);
            if self.eat(TokenKind::Comma) {
                if self.peek_is(TokenKind::RBracket) {
                    break;
                }
                continue;
            }
            break;
        }
        self.expect(TokenKind::RBracket, "expected ']'")?;
        Ok(PlannerExpr::VectorLiteral {
            items,
            provenance: self.provenance_script(line, column),
        })
    }

    fn parse_map_literal(&mut self) -> Result<PlannerExpr, VxParseError> {
        let (line, column) = self.peek_line_col();
        self.expect(TokenKind::LBrace, "expected '{'")?;
        let mut entries = BTreeMap::new();
        if self.eat(TokenKind::RBrace) {
            return Ok(PlannerExpr::MapLiteral {
                entries,
                provenance: self.provenance_script(line, column),
            });
        }
        loop {
            let key = match self.peek_kind() {
                TokenKind::Ident(_) => {
                    let (k, _line, _col) = self.expect_ident("expected map key")?;
                    k
                }
                TokenKind::String(_) => match self.bump().kind {
                    TokenKind::String(s) => s,
                    _ => unreachable!(),
                },
                _ => return Err(self.error_here("expected map key".to_string())),
            };

            self.expect(TokenKind::Colon, "expected ':' after map key")?;
            let value = self.parse_expr()?;
            entries.insert(key, value);

            if self.eat(TokenKind::Comma) {
                if self.peek_is(TokenKind::RBrace) {
                    break;
                }
                continue;
            }
            break;
        }
        self.expect(TokenKind::RBrace, "expected '}'")?;
        Ok(PlannerExpr::MapLiteral {
            entries,
            provenance: self.provenance_script(line, column),
        })
    }

    fn parse_let(&mut self) -> Result<PlannerExpr, VxParseError> {
        let (_kw, line, column) = self.expect_ident("expected 'let'")?;
        let (name, _l2, _c2) = self.expect_ident("expected identifier after 'let'")?;
        self.expect(TokenKind::Eq, "expected '=' in let statement")?;
        let value = self.parse_expr()?;
        Ok(PlannerExpr::Let {
            name,
            value: Box::new(value),
            provenance: self.provenance_script(line, column),
        })
    }

    fn parse_if(&mut self) -> Result<PlannerExpr, VxParseError> {
        let (_kw, line, column) = self.expect_ident("expected 'if'")?;
        let cond = self.parse_expr()?;
        let then_branch = self.parse_block()?;
        let else_branch = if self.peek_ident_is("else") {
            let _ = self.expect_ident("expected 'else'")?;
            self.parse_block()?
        } else {
            Vec::new()
        };

        Ok(PlannerExpr::If {
            cond: Box::new(cond),
            then_branch,
            else_branch,
            provenance: self.provenance_script(line, column),
        })
    }

    fn parse_for(&mut self) -> Result<PlannerExpr, VxParseError> {
        let (_kw, line, column) = self.expect_ident("expected 'for'")?;
        let (name, _l2, _c2) = self.expect_ident("expected loop variable name")?;
        if !self.peek_ident_is("in") {
            return Err(self.error_here("expected 'in' in for loop".to_string()));
        }
        let _ = self.expect_ident("expected 'in'")?;
        let iter = self.parse_expr()?;
        let body = self.parse_block()?;
        Ok(PlannerExpr::ForEach {
            name,
            iter: Box::new(iter),
            body,
            provenance: self.provenance_script(line, column),
        })
    }

    fn parse_block(&mut self) -> Result<Vec<PlannerExpr>, VxParseError> {
        self.expect(TokenKind::LBrace, "expected '{' to start block")?;
        let mut exprs = Vec::new();
        while !self.peek_is(TokenKind::RBrace) && !self.at_eof() {
            if self.eat(TokenKind::Semi) {
                continue;
            }
            exprs.push(self.parse_stmt()?);
            self.eat(TokenKind::Semi);
        }
        self.expect(TokenKind::RBrace, "expected '}' to end block")?;
        Ok(exprs)
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek_kind(), TokenKind::Eof)
    }

    fn peek_kind(&self) -> &TokenKind {
        &self.tokens[self.pos].kind
    }

    fn peek_is(&self, kind: TokenKind) -> bool {
        std::mem::discriminant(self.peek_kind()) == std::mem::discriminant(&kind)
    }

    fn eat(&mut self, kind: TokenKind) -> bool {
        if self.peek_is(kind) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn bump(&mut self) -> Token {
        let tok = self.tokens[self.pos].clone();
        self.pos += 1;
        tok
    }

    fn expect(&mut self, kind: TokenKind, message: &str) -> Result<(), VxParseError> {
        if self.eat(kind) {
            Ok(())
        } else {
            Err(self.error_here(message.to_string()))
        }
    }

    fn expect_ident(&mut self, message: &str) -> Result<(String, u32, u32), VxParseError> {
        let tok = self
            .tokens
            .get(self.pos)
            .cloned()
            .ok_or_else(|| VxParseError {
                message: message.to_string(),
                line: 1,
                column: 1,
            })?;
        match tok.kind {
            TokenKind::Ident(s) => {
                self.pos += 1;
                Ok((s, tok.line, tok.column))
            }
            _ => Err(VxParseError {
                message: message.to_string(),
                line: tok.line,
                column: tok.column,
            }),
        }
    }

    fn expect_qualified_ident(
        &mut self,
        message: &str,
    ) -> Result<(String, u32, u32), VxParseError> {
        let (first, line, column) = self.expect_ident(message)?;
        let mut s = first;
        while self.eat(TokenKind::Dot) {
            let (part, _l2, _c2) = self.expect_ident("expected identifier after '.'")?;
            s.push('.');
            s.push_str(&part);
        }
        Ok((s, line, column))
    }

    fn peek_line_col(&self) -> (u32, u32) {
        let tok = &self.tokens[self.pos];
        (tok.line, tok.column)
    }

    fn peek_ident_is(&self, expected: &str) -> bool {
        match self.peek_kind() {
            TokenKind::Ident(s) => s == expected,
            _ => false,
        }
    }

    fn peek_named_arg_list_start(&self) -> bool {
        let Some(tok0) = self.tokens.get(self.pos) else {
            return false;
        };
        let Some(tok1) = self.tokens.get(self.pos + 1) else {
            return false;
        };
        matches!(tok0.kind, TokenKind::Ident(_) | TokenKind::String(_))
            && matches!(tok1.kind, TokenKind::Colon)
    }

    fn peek_call_like(&self) -> bool {
        // `foo(` or `foo.bar(`.
        let Some(tok0) = self.tokens.get(self.pos) else {
            return false;
        };
        if !matches!(tok0.kind, TokenKind::Ident(_)) {
            return false;
        }
        matches!(
            self.tokens.get(self.pos + 1).map(|t| &t.kind),
            Some(TokenKind::LParen) | Some(TokenKind::Dot)
        )
    }

    fn provenance_script(&self, line: u32, column: u32) -> RulePlanProvenance {
        RulePlanProvenance {
            rule_id: RuleId::from("rule.vx.script".to_string()),
            source_id: self.source_id.clone(),
            span: RulePlanSpan {
                line,
                column,
                end_line: line,
                end_column: column + 1,
            },
            comment: None,
            metadata: BTreeMap::new(),
        }
    }

    fn error_here(&self, message: String) -> VxParseError {
        let tok = &self.tokens[self.pos];
        VxParseError {
            message,
            line: tok.line,
            column: tok.column,
        }
    }
}

fn builtin_from_name(raw: &str) -> Option<PlannerBuiltinId> {
    let normalized = raw.to_ascii_lowercase().replace(['_', '.'], "-");
    builtin_from_str(normalized.as_str())
}

fn builtin_from_str(raw: &str) -> Option<PlannerBuiltinId> {
    match raw {
        "emit-node" => Some(PlannerBuiltinId::EmitNode),
        "emit-edge" => Some(PlannerBuiltinId::EmitEdge),
        "emit-fact" => Some(PlannerBuiltinId::EmitFact),
        "sort-map-keys" => Some(PlannerBuiltinId::SortMapKeys),
        "sort-scalars" => Some(PlannerBuiltinId::SortScalars),
        "env-profile" => Some(PlannerBuiltinId::EnvProfile),
        "env-target-triple" => Some(PlannerBuiltinId::EnvTargetTriple),
        "clock-now" => Some(PlannerBuiltinId::ClockNow),
        "random-int" => Some(PlannerBuiltinId::RandomInt),
        "net-request" => Some(PlannerBuiltinId::NetRequest),
        "fs-read" => Some(PlannerBuiltinId::FsRead),
        "process-spawn" => Some(PlannerBuiltinId::ProcessSpawn),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_v0_call_syntax() {
        let program = parse_vx_program(
            Utf8Path::new(".vx/rules.vx"),
            RulePlanSourceId::from("rules".to_string()),
            r#"
// comment
emit_node(id: n1, kind: action, label: "compile");
emit_edge(from: n1, to: n2, kind: ifwritten)
"#,
        )
        .expect("parse should succeed");
        assert_eq!(program.forms.len(), 2);
    }

    #[test]
    fn parses_map_literal_arg() {
        let program = parse_vx_program(
            Utf8Path::new(".vx/rules.vx"),
            RulePlanSourceId::from("rules".to_string()),
            r#"emit-node({ id: n1, kind: action, label: "compile" })"#,
        )
        .expect("parse should succeed");
        assert_eq!(program.forms.len(), 1);
    }

    #[test]
    fn rejects_unknown_builtin() {
        let err = parse_vx_program(
            Utf8Path::new(".vx/rules.vx"),
            RulePlanSourceId::from("rules".to_string()),
            "wat(id: n1)",
        )
        .unwrap_err();
        assert!(err.message.contains("unknown builtin"));
    }

    #[test]
    fn parses_let_if_for_and_var_refs() {
        let program = parse_vx_program(
            Utf8Path::new(".vx/rules.vx"),
            RulePlanSourceId::from("rules".to_string()),
            r#"
let roots = ["app"];
for c in $roots {
  if true { emit_node(id: n1, kind: action, label: "compile"); } else { }
}
"#,
        )
        .expect("parse should succeed");
        assert_eq!(program.forms.len(), 2);
    }

    #[test]
    fn parses_dotted_env_builtins() {
        let program = parse_vx_program(
            Utf8Path::new(".vx/rules.vx"),
            RulePlanSourceId::from("rules".to_string()),
            r#"env.profile(); env.target_triple();"#,
        )
        .expect("parse should succeed");
        assert_eq!(program.forms.len(), 2);
    }
}
