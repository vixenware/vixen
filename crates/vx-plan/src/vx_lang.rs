use crate::runtime::{PlannerBuiltinId, PlannerCall, PlannerExpr, PlannerProgram, PlannerValue};
use crate::{RuleId, RulePlanProvenance, RulePlanSourceId, RulePlanSpan};
use camino::Utf8Path;
use std::collections::{BTreeMap, BTreeSet};

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

    if looks_like_legacy_vx(input) {
        return parse_vx_program_legacy(source_id, input);
    }

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

fn looks_like_legacy_vx(input: &str) -> bool {
    for raw_line in input.lines() {
        let trimmed = strip_legacy_comment(raw_line).trim();
        if trimmed.is_empty() {
            continue;
        }
        // Legacy format: `emit-node id=... kind=...`
        // New format starts with `ident(`.
        if trimmed.contains('(') {
            return false;
        }
        if trimmed.contains('=') {
            return true;
        }
        return false;
    }
    false
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
        self.parse_call()
    }

    fn parse_call(&mut self) -> Result<PlannerExpr, VxParseError> {
        let (name, line, column) = self.expect_ident("expected builtin name")?;
        let builtin = builtin_from_name(&name).ok_or_else(|| VxParseError {
            message: format!("unknown builtin '{name}'"),
            line,
            column,
        })?;

        self.expect(TokenKind::LParen, "expected '(' after builtin name")?;

        let map_value = if self.eat(TokenKind::RParen) {
            PlannerValue::Map(BTreeMap::new())
        } else if self.peek_is(TokenKind::LBrace) {
            let expr = self.parse_expr()?;
            self.expect(TokenKind::RParen, "expected ')' after argument")?;
            match expr {
                PlannerExpr::Value(PlannerValue::Map(map)) => PlannerValue::Map(map),
                _ => {
                    return Err(
                        self.error_here("expected map literal as sole argument".to_string())
                    );
                }
            }
        } else {
            let map = self.parse_named_args()?;
            self.expect(TokenKind::RParen, "expected ')' after argument list")?;
            PlannerValue::Map(map)
        };

        Ok(PlannerExpr::Call(PlannerCall {
            builtin,
            args: vec![PlannerExpr::Value(map_value)],
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

    fn parse_named_args(&mut self) -> Result<BTreeMap<String, PlannerValue>, VxParseError> {
        let mut map = BTreeMap::new();
        let mut seen = BTreeSet::new();

        loop {
            let (key, _line, _col) = self.expect_ident("expected argument name")?;
            if !seen.insert(key.clone()) {
                return Err(self.error_here(format!("duplicate argument '{key}'")));
            }

            if self.eat(TokenKind::Colon) || self.eat(TokenKind::Eq) {
                // ok
            } else {
                return Err(self.error_here("expected ':' or '=' after argument name".to_string()));
            }

            let value = self.parse_value_expr()?;
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

    fn parse_expr(&mut self) -> Result<PlannerExpr, VxParseError> {
        Ok(PlannerExpr::Value(self.parse_value_expr()?))
    }

    fn parse_value_expr(&mut self) -> Result<PlannerValue, VxParseError> {
        match self.peek_kind() {
            TokenKind::String(_) => {
                let s = match self.bump().kind {
                    TokenKind::String(s) => s,
                    _ => unreachable!(),
                };
                Ok(PlannerValue::String(s))
            }
            TokenKind::Int(_) => {
                let v = match self.bump().kind {
                    TokenKind::Int(v) => v,
                    _ => unreachable!(),
                };
                Ok(PlannerValue::Int(v))
            }
            TokenKind::Colon => {
                self.bump();
                let (ident, _line, _col) = self.expect_ident("expected identifier after ':'")?;
                Ok(PlannerValue::Keyword(ident))
            }
            TokenKind::Ident(_) => {
                let (ident, _line, _column) = self.expect_ident("expected identifier")?;
                match ident.as_str() {
                    "true" => Ok(PlannerValue::Bool(true)),
                    "false" => Ok(PlannerValue::Bool(false)),
                    "null" => Ok(PlannerValue::Null),
                    _ => Ok(PlannerValue::Keyword(ident)),
                }
            }
            TokenKind::LBracket => self.parse_vector(),
            TokenKind::LBrace => self.parse_map(),
            _ => Err(self.error_here("expected value".to_string())),
        }
    }

    fn parse_vector(&mut self) -> Result<PlannerValue, VxParseError> {
        self.expect(TokenKind::LBracket, "expected '['")?;
        let mut values = Vec::new();
        if self.eat(TokenKind::RBracket) {
            return Ok(PlannerValue::Vector(values));
        }
        loop {
            values.push(self.parse_value_expr()?);
            if self.eat(TokenKind::Comma) {
                if self.peek_is(TokenKind::RBracket) {
                    break;
                }
                continue;
            }
            break;
        }
        self.expect(TokenKind::RBracket, "expected ']'")?;
        Ok(PlannerValue::Vector(values))
    }

    fn parse_map(&mut self) -> Result<PlannerValue, VxParseError> {
        self.expect(TokenKind::LBrace, "expected '{'")?;
        let mut map = BTreeMap::new();
        if self.eat(TokenKind::RBrace) {
            return Ok(PlannerValue::Map(map));
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
            let value = self.parse_value_expr()?;
            map.insert(key, value);

            if self.eat(TokenKind::Comma) {
                if self.peek_is(TokenKind::RBrace) {
                    break;
                }
                continue;
            }
            break;
        }
        self.expect(TokenKind::RBrace, "expected '}'")?;
        Ok(PlannerValue::Map(map))
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
    let normalized = raw.to_ascii_lowercase().replace('_', "-");
    builtin_from_str(normalized.as_str())
}

fn builtin_from_str(raw: &str) -> Option<PlannerBuiltinId> {
    match raw {
        "emit-node" => Some(PlannerBuiltinId::EmitNode),
        "emit-edge" => Some(PlannerBuiltinId::EmitEdge),
        "emit-fact" => Some(PlannerBuiltinId::EmitFact),
        "sort-map-keys" => Some(PlannerBuiltinId::SortMapKeys),
        "sort-scalars" => Some(PlannerBuiltinId::SortScalars),
        "clock-now" => Some(PlannerBuiltinId::ClockNow),
        "random-int" => Some(PlannerBuiltinId::RandomInt),
        "net-request" => Some(PlannerBuiltinId::NetRequest),
        "fs-read" => Some(PlannerBuiltinId::FsRead),
        "process-spawn" => Some(PlannerBuiltinId::ProcessSpawn),
        _ => None,
    }
}

// =============================================================================
// Legacy key=value parser (compat)
// =============================================================================

fn parse_vx_program_legacy(
    source_id: RulePlanSourceId,
    input: &str,
) -> Result<PlannerProgram, VxParseError> {
    let mut forms = Vec::new();

    for (line_idx, raw_line) in input.lines().enumerate() {
        let line = (line_idx + 1) as u32;
        let trimmed = strip_legacy_comment(raw_line).trim();
        if trimmed.is_empty() {
            continue;
        }

        let tokens = legacy_split_tokens(trimmed).map_err(|e| VxParseError {
            message: e,
            line,
            column: 1,
        })?;
        let Some((builtin_raw, _col)) = tokens.first() else {
            continue;
        };

        let builtin =
            builtin_from_str(&builtin_raw.to_ascii_lowercase()).ok_or_else(|| VxParseError {
                message: format!("unknown builtin '{builtin_raw}'"),
                line,
                column: 1,
            })?;

        let mut map = BTreeMap::new();
        let mut seen = BTreeSet::new();
        for (token, col) in tokens.iter().skip(1) {
            let (key, value_raw) = token.split_once('=').ok_or_else(|| VxParseError {
                message: format!("expected key=value token, got '{token}'"),
                line,
                column: *col,
            })?;
            if key.is_empty() {
                return Err(VxParseError {
                    message: "expected key=value token with non-empty key".to_string(),
                    line,
                    column: *col,
                });
            }
            if !seen.insert(key.to_string()) {
                return Err(VxParseError {
                    message: format!("duplicate key '{key}'"),
                    line,
                    column: *col,
                });
            }

            let value = legacy_parse_value(value_raw).map_err(|msg| VxParseError {
                message: msg,
                line,
                column: *col,
            })?;
            map.insert(key.to_string(), value);
        }

        let provenance = RulePlanProvenance {
            rule_id: RuleId::from(format!("rule.vx.{}", builtin.as_str())),
            source_id: source_id.clone(),
            span: RulePlanSpan {
                line,
                column: 1,
                end_line: line,
                end_column: raw_line.len().max(1) as u32,
            },
            comment: None,
            metadata: BTreeMap::new(),
        };

        forms.push(PlannerExpr::Call(PlannerCall {
            builtin,
            args: vec![PlannerExpr::Value(PlannerValue::Map(map))],
            provenance,
        }));
    }

    Ok(PlannerProgram { forms })
}

fn strip_legacy_comment(line: &str) -> &str {
    for (idx, ch) in line.char_indices() {
        if ch == ';' || ch == '#' {
            return &line[..idx];
        }
    }
    line
}

fn legacy_split_tokens(input: &str) -> Result<Vec<(String, u32)>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    let mut escape = false;
    let mut token_col = 1u32;

    for (idx, ch) in input.chars().enumerate() {
        let col = (idx + 1) as u32;
        if in_string {
            if escape {
                current.push(match ch {
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
                '"' => in_string = false,
                other => current.push(other),
            }
            continue;
        }

        if ch.is_whitespace() {
            if !current.is_empty() {
                tokens.push((current.clone(), token_col));
                current.clear();
            }
            continue;
        }

        if current.is_empty() {
            token_col = col;
        }

        if ch == '"' {
            in_string = true;
            continue;
        }

        current.push(ch);
    }

    if escape {
        return Err("unterminated escape sequence".to_string());
    }
    if in_string {
        return Err("unterminated string literal".to_string());
    }
    if !current.is_empty() {
        tokens.push((current, token_col));
    }

    Ok(tokens)
}

fn legacy_parse_value(raw: &str) -> Result<PlannerValue, String> {
    if raw.is_empty() {
        return Err("expected non-empty value".to_string());
    }
    if let Some(keyword) = raw.strip_prefix(':') {
        return Ok(PlannerValue::Keyword(keyword.to_string()));
    }
    match raw {
        "null" => return Ok(PlannerValue::Null),
        "true" => return Ok(PlannerValue::Bool(true)),
        "false" => return Ok(PlannerValue::Bool(false)),
        _ => {}
    }
    if let Ok(value) = raw.parse::<i64>() {
        return Ok(PlannerValue::Int(value));
    }
    Ok(PlannerValue::String(raw.to_string()))
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
    fn parses_legacy_key_value_format() {
        let program = parse_vx_program(
            Utf8Path::new(".vx/rules.vx"),
            RulePlanSourceId::from("rules".to_string()),
            "emit-node id=n1 kind=action label=\"compile\"",
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
}
