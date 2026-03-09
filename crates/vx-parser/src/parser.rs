use std::mem;

use rowan::{GreenNode, GreenNodeBuilder, Language, TextRange, TextSize};

use crate::{SyntaxKind, SyntaxNode, Token, VxLanguage, lex};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxError {
    pub message: String,
    pub range: TextRange,
}

#[derive(Debug, Clone)]
pub struct Parse {
    green: GreenNode,
    errors: Vec<SyntaxError>,
}

impl Parse {
    pub fn syntax(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.green.clone())
    }

    pub fn errors(&self) -> &[SyntaxError] {
        &self.errors
    }
}

pub fn parse(source: &str) -> Parse {
    let tokens = lex(source);
    let significant = tokens
        .iter()
        .enumerate()
        .filter_map(|(idx, token)| (!token.kind.is_trivia()).then_some(idx))
        .collect::<Vec<_>>();
    let mut parser = Parser::new(source, &tokens, significant);
    source_file(&mut parser);
    let events = parser.finish();
    Sink::new(source, &tokens, events).finish()
}

#[derive(Debug)]
enum Event {
    Start {
        kind: SyntaxKind,
        forward_parent: Option<usize>,
    },
    Finish,
    Token {
        token_index: usize,
    },
    Error(SyntaxError),
}

impl Event {
    fn tombstone() -> Self {
        Self::Start {
            kind: SyntaxKind::TOMBSTONE,
            forward_parent: None,
        }
    }
}

struct Parser<'a> {
    source: &'a str,
    tokens: &'a [Token],
    significant: Vec<usize>,
    pos: usize,
    events: Vec<Event>,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str, tokens: &'a [Token], significant: Vec<usize>) -> Self {
        Self {
            source,
            tokens,
            significant,
            pos: 0,
            events: Vec::new(),
        }
    }

    fn finish(self) -> Vec<Event> {
        self.events
    }

    fn start(&mut self) -> Marker {
        let pos = self.events.len();
        self.events.push(Event::tombstone());
        Marker { pos }
    }

    fn at(&self, kind: SyntaxKind) -> bool {
        self.current() == kind
    }

    fn nth(&self, n: usize) -> SyntaxKind {
        self.significant
            .get(self.pos + n)
            .map(|&idx| self.tokens[idx].kind)
            .unwrap_or(SyntaxKind::EOF)
    }

    fn current(&self) -> SyntaxKind {
        self.nth(0)
    }

    fn bump(&mut self) {
        if let Some(&token_index) = self.significant.get(self.pos) {
            self.events.push(Event::Token { token_index });
            self.pos += 1;
        }
    }

    fn eat(&mut self, kind: SyntaxKind) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: SyntaxKind) {
        if !self.eat(kind) {
            self.error(format!("expected {:?}", kind));
        }
    }

    fn error(&mut self, message: impl Into<String>) {
        self.events.push(Event::Error(SyntaxError {
            message: message.into(),
            range: self.current_range(),
        }));
    }

    fn error_and_bump(&mut self, message: impl Into<String>) {
        let marker = self.start();
        self.error(message);
        if self.current() != SyntaxKind::EOF {
            self.bump();
        }
        marker.complete(self, SyntaxKind::ERROR);
    }

    fn current_range(&self) -> TextRange {
        self.significant
            .get(self.pos)
            .map(|&idx| self.tokens[idx].range)
            .unwrap_or_else(|| {
                let end =
                    TextSize::from(u32::try_from(self.source.len()).expect("source too large"));
                TextRange::new(end, end)
            })
    }
}

#[derive(Debug, Clone, Copy)]
struct Marker {
    pos: usize,
}

impl Marker {
    fn complete(self, parser: &mut Parser<'_>, kind: SyntaxKind) -> CompletedMarker {
        parser.events[self.pos] = Event::Start {
            kind,
            forward_parent: None,
        };
        parser.events.push(Event::Finish);
        CompletedMarker { pos: self.pos }
    }
}

#[derive(Debug, Clone, Copy)]
struct CompletedMarker {
    pos: usize,
}

impl CompletedMarker {
    fn precede(self, parser: &mut Parser<'_>) -> Marker {
        let new_marker = parser.start();
        match &mut parser.events[self.pos] {
            Event::Start { forward_parent, .. } => {
                *forward_parent = Some(new_marker.pos - self.pos);
            }
            _ => unreachable!("completed marker must point at a start event"),
        }
        new_marker
    }
}

struct Sink<'a> {
    source: &'a str,
    tokens: &'a [Token],
    events: Vec<Event>,
    builder: GreenNodeBuilder<'static>,
    errors: Vec<SyntaxError>,
    token_pos: usize,
}

impl<'a> Sink<'a> {
    fn new(source: &'a str, tokens: &'a [Token], events: Vec<Event>) -> Self {
        Self {
            source,
            tokens,
            events,
            builder: GreenNodeBuilder::new(),
            errors: Vec::new(),
            token_pos: 0,
        }
    }

    fn finish(mut self) -> Parse {
        for idx in 0..self.events.len() {
            match mem::replace(&mut self.events[idx], Event::tombstone()) {
                Event::Start {
                    kind,
                    forward_parent,
                } => {
                    let mut kinds = vec![kind];
                    let mut next = forward_parent.map(|offset| idx + offset);
                    while let Some(next_idx) = next {
                        match mem::replace(&mut self.events[next_idx], Event::tombstone()) {
                            Event::Start {
                                kind,
                                forward_parent,
                            } => {
                                kinds.push(kind);
                                next = forward_parent.map(|offset| next_idx + offset);
                            }
                            _ => {
                                unreachable!("forward parent chain must only contain start events")
                            }
                        }
                    }
                    for kind in kinds
                        .into_iter()
                        .rev()
                        .filter(|kind| *kind != SyntaxKind::TOMBSTONE)
                    {
                        self.builder.start_node(VxLanguage::kind_to_raw(kind));
                    }
                }
                Event::Finish => self.builder.finish_node(),
                Event::Token { token_index } => self.emit_token(token_index),
                Event::Error(error) => self.errors.push(error),
            }
        }
        Parse {
            green: self.builder.finish(),
            errors: self.errors,
        }
    }

    fn emit_token(&mut self, token_index: usize) {
        while self.token_pos < token_index {
            self.emit_raw_token(self.token_pos);
            self.token_pos += 1;
        }
        self.emit_raw_token(token_index);
        self.token_pos = token_index + 1;
    }

    fn emit_raw_token(&mut self, token_index: usize) {
        let token = self.tokens[token_index];
        let text = &self.source[usize::from(token.range.start())..usize::from(token.range.end())];
        self.builder
            .token(VxLanguage::kind_to_raw(token.kind), text);
    }
}

fn source_file(p: &mut Parser<'_>) {
    let marker = p.start();
    while p.current() != SyntaxKind::EOF {
        item(p);
    }
    marker.complete(p, SyntaxKind::SOURCE_FILE);
}

fn item(p: &mut Parser<'_>) {
    attributes(p);
    match p.current() {
        SyntaxKind::FN_KW => fn_decl_rest(p),
        SyntaxKind::STRUCT_KW => struct_decl_rest(p, SyntaxKind::STRUCT_DECL),
        SyntaxKind::ENUM_KW => enum_decl_rest(p, SyntaxKind::ENUM_DECL),
        SyntaxKind::NEWTYPE_KW => newtype_decl_rest(p, SyntaxKind::NEWTYPE_DECL),
        SyntaxKind::TYPE_KW => type_alias_decl_rest(p, SyntaxKind::TYPE_ALIAS_DECL),
        _ => p.error_and_bump("expected top-level declaration"),
    }
}

fn attributes(p: &mut Parser<'_>) {
    if !p.at(SyntaxKind::AT) {
        return;
    }
    let marker = p.start();
    while p.at(SyntaxKind::AT) {
        let attr = p.start();
        p.bump();
        name(p, false);
        if p.eat(SyntaxKind::L_PAREN) {
            let args = p.start();
            if !p.at(SyntaxKind::R_PAREN) {
                loop {
                    if p.at(SyntaxKind::LOWER_IDENT) && p.nth(1) == SyntaxKind::COLON {
                        let kw = p.start();
                        p.bump();
                        p.bump();
                        expr(p);
                        kw.complete(p, SyntaxKind::KEYWORD_ATTRIBUTE_ARGUMENT);
                    } else {
                        expr(p);
                    }
                    if !p.eat(SyntaxKind::COMMA) || p.at(SyntaxKind::R_PAREN) {
                        break;
                    }
                }
            }
            p.expect(SyntaxKind::R_PAREN);
            args.complete(p, SyntaxKind::ATTRIBUTE_ARGUMENTS);
        }
        attr.complete(p, SyntaxKind::ATTRIBUTE);
    }
    marker.complete(p, SyntaxKind::ATTRIBUTE_LIST);
}

fn fn_decl_rest(p: &mut Parser<'_>) {
    let marker = p.start();
    p.expect(SyntaxKind::FN_KW);
    name(p, true);
    type_param_list(p);
    parameter_clause(p);
    capability_list(p);
    if p.eat(SyntaxKind::THIN_ARROW) {
        let ret = p.start();
        type_expr(p);
        ret.complete(p, SyntaxKind::RETURN_TYPE);
    }
    if p.at(SyntaxKind::L_CURLY) {
        block_expr(p);
    }
    marker.complete(p, SyntaxKind::FN_DECL);
}

fn struct_decl_rest(p: &mut Parser<'_>, kind: SyntaxKind) {
    let marker = p.start();
    p.expect(SyntaxKind::STRUCT_KW);
    if p.at(SyntaxKind::UPPER_IDENT) {
        p.bump();
    } else {
        p.error("expected type name");
    }
    type_param_list(p);
    if p.at(SyntaxKind::L_CURLY) {
        struct_body(p);
    }
    marker.complete(p, kind);
}

fn enum_decl_rest(p: &mut Parser<'_>, kind: SyntaxKind) {
    let marker = p.start();
    p.expect(SyntaxKind::ENUM_KW);
    if p.at(SyntaxKind::UPPER_IDENT) {
        p.bump();
    } else {
        p.error("expected type name");
    }
    type_param_list(p);
    enum_body(p);
    marker.complete(p, kind);
}

fn newtype_decl_rest(p: &mut Parser<'_>, kind: SyntaxKind) {
    let marker = p.start();
    p.expect(SyntaxKind::NEWTYPE_KW);
    if p.at(SyntaxKind::UPPER_IDENT) {
        p.bump();
    } else {
        p.error("expected type name");
    }
    type_param_list(p);
    p.expect(SyntaxKind::OF_KW);
    type_expr(p);
    marker.complete(p, kind);
}

fn type_alias_decl_rest(p: &mut Parser<'_>, kind: SyntaxKind) {
    let marker = p.start();
    p.expect(SyntaxKind::TYPE_KW);
    if p.at(SyntaxKind::UPPER_IDENT) {
        p.bump();
    } else {
        p.error("expected type name");
    }
    type_param_list(p);
    p.expect(SyntaxKind::EQ);
    let first = type_expr(p);
    if p.at(SyntaxKind::PIPE) {
        let sum = first.precede(p);
        while p.eat(SyntaxKind::PIPE) {
            type_expr(p);
        }
        sum.complete(p, SyntaxKind::SUM_TYPE);
    }
    marker.complete(p, kind);
}

fn struct_body(p: &mut Parser<'_>) {
    let marker = p.start();
    p.expect(SyntaxKind::L_CURLY);
    while !p.at(SyntaxKind::R_CURLY) && !p.at(SyntaxKind::EOF) {
        let field = p.start();
        attributes(p);
        if p.at(SyntaxKind::LOWER_IDENT) {
            p.bump();
        } else {
            p.error("expected field name");
        }
        p.expect(SyntaxKind::COLON);
        slot_type(p);
        if p.eat(SyntaxKind::EQ) {
            expr(p);
        }
        field.complete(p, SyntaxKind::FIELD_DECL);
        if !p.eat(SyntaxKind::COMMA) {
            break;
        }
    }
    p.expect(SyntaxKind::R_CURLY);
    marker.complete(p, SyntaxKind::STRUCT_BODY);
}

fn enum_body(p: &mut Parser<'_>) {
    let marker = p.start();
    p.expect(SyntaxKind::L_CURLY);
    while !p.at(SyntaxKind::R_CURLY) && !p.at(SyntaxKind::EOF) {
        let variant = p.start();
        attributes(p);
        p.expect(SyntaxKind::STRUCT_KW);
        if p.at(SyntaxKind::UPPER_IDENT) {
            p.bump();
        } else {
            p.error("expected variant name");
        }
        if p.at(SyntaxKind::L_CURLY) {
            struct_body(p);
        }
        variant.complete(p, SyntaxKind::VARIANT_DECL);
        if !p.eat(SyntaxKind::COMMA) {
            break;
        }
    }
    p.expect(SyntaxKind::R_CURLY);
    marker.complete(p, SyntaxKind::ENUM_BODY);
}

fn type_param_list(p: &mut Parser<'_>) {
    if !p.at(SyntaxKind::L_BRACKET) {
        return;
    }
    let marker = p.start();
    p.bump();
    while !p.at(SyntaxKind::R_BRACKET) && !p.at(SyntaxKind::EOF) {
        if p.at(SyntaxKind::UPPER_IDENT) {
            p.bump();
        } else {
            p.error("expected type parameter");
            break;
        }
        if !p.eat(SyntaxKind::COMMA) {
            break;
        }
    }
    p.expect(SyntaxKind::R_BRACKET);
    marker.complete(p, SyntaxKind::TYPE_PARAM_LIST);
}

fn parameter_clause(p: &mut Parser<'_>) {
    let marker = p.start();
    p.expect(SyntaxKind::L_PAREN);
    if !p.at(SyntaxKind::R_PAREN) && !p.at(SyntaxKind::SEMICOLON) {
        let params = p.start();
        loop {
            let param = p.start();
            binding_pattern(p);
            p.expect(SyntaxKind::COLON);
            slot_type(p);
            param.complete(p, SyntaxKind::PARAMETER);
            if !p.eat(SyntaxKind::COMMA) || p.at(SyntaxKind::SEMICOLON) || p.at(SyntaxKind::R_PAREN)
            {
                break;
            }
        }
        params.complete(p, SyntaxKind::PARAMETER_LIST);
    }
    if p.eat(SyntaxKind::SEMICOLON) {
        let params = p.start();
        while !p.at(SyntaxKind::R_PAREN) && !p.at(SyntaxKind::EOF) {
            let param = p.start();
            if p.at(SyntaxKind::LOWER_IDENT) {
                p.bump();
            } else {
                p.error("expected keyword parameter name");
            }
            p.expect(SyntaxKind::COLON);
            slot_type(p);
            if p.eat(SyntaxKind::EQ) {
                expr(p);
            }
            param.complete(p, SyntaxKind::KEYWORD_PARAMETER);
            if !p.eat(SyntaxKind::COMMA) {
                break;
            }
        }
        params.complete(p, SyntaxKind::KEYWORD_PARAMETER_LIST);
    }
    p.expect(SyntaxKind::R_PAREN);
    marker.complete(p, SyntaxKind::PARAMETER_CLAUSE);
}

fn capability_list(p: &mut Parser<'_>) {
    if !p.at(SyntaxKind::HASH) {
        return;
    }
    let marker = p.start();
    while p.eat(SyntaxKind::HASH) {
        let tag = p.start();
        name(p, false);
        tag.complete(p, SyntaxKind::CAPABILITY_TAG);
    }
    marker.complete(p, SyntaxKind::CAPABILITY_LIST);
}

fn slot_type(p: &mut Parser<'_>) {
    let marker = p.start();
    if p.at(SyntaxKind::AT)
        || matches!(
            p.current(),
            SyntaxKind::STRUCT_KW
                | SyntaxKind::ENUM_KW
                | SyntaxKind::NEWTYPE_KW
                | SyntaxKind::TYPE_KW
        )
    {
        attributes(p);
        match p.current() {
            SyntaxKind::STRUCT_KW => struct_decl_rest(p, SyntaxKind::INLINE_STRUCT_DECL),
            SyntaxKind::ENUM_KW => enum_decl_rest(p, SyntaxKind::INLINE_ENUM_DECL),
            SyntaxKind::NEWTYPE_KW => newtype_decl_rest(p, SyntaxKind::INLINE_NEWTYPE_DECL),
            SyntaxKind::TYPE_KW => type_alias_decl_rest(p, SyntaxKind::INLINE_TYPE_ALIAS_DECL),
            _ => p.error_and_bump("expected inline type declaration"),
        }
    } else {
        type_expr(p);
    }
    marker.complete(p, SyntaxKind::SLOT_TYPE);
}

fn type_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    match p.current() {
        SyntaxKind::FN_TYPE_KW => function_type(p),
        SyntaxKind::L_PAREN => tuple_or_paren_type(p),
        SyntaxKind::UPPER_IDENT => type_path(p),
        _ => {
            p.error_and_bump("expected type");
        }
    }
    while p.eat(SyntaxKind::QUESTION) {}
    marker.complete(p, SyntaxKind::OPTIONAL_TYPE)
}

fn function_type(p: &mut Parser<'_>) {
    let marker = p.start();
    p.expect(SyntaxKind::FN_TYPE_KW);
    p.expect(SyntaxKind::L_PAREN);
    if !p.at(SyntaxKind::R_PAREN) {
        loop {
            type_expr(p);
            if !p.eat(SyntaxKind::COMMA) || p.at(SyntaxKind::R_PAREN) {
                break;
            }
        }
    }
    p.expect(SyntaxKind::R_PAREN);
    capability_list(p);
    p.expect(SyntaxKind::THIN_ARROW);
    type_expr(p);
    marker.complete(p, SyntaxKind::FUNCTION_TYPE);
}

fn tuple_or_paren_type(p: &mut Parser<'_>) {
    let marker = p.start();
    p.expect(SyntaxKind::L_PAREN);
    if p.eat(SyntaxKind::R_PAREN) {
        marker.complete(p, SyntaxKind::TUPLE_TYPE);
        return;
    }
    type_expr(p);
    if p.eat(SyntaxKind::COMMA) {
        while !p.at(SyntaxKind::R_PAREN) && !p.at(SyntaxKind::EOF) {
            type_expr(p);
            if !p.eat(SyntaxKind::COMMA) {
                break;
            }
        }
        p.expect(SyntaxKind::R_PAREN);
        marker.complete(p, SyntaxKind::TUPLE_TYPE);
    } else {
        p.expect(SyntaxKind::R_PAREN);
        marker.complete(p, SyntaxKind::PAREN_TYPE);
    }
}

fn type_path(p: &mut Parser<'_>) {
    let marker = p.start();
    p.expect(SyntaxKind::UPPER_IDENT);
    while p.eat(SyntaxKind::DOT) {
        if p.at(SyntaxKind::UPPER_IDENT) {
            p.bump();
        } else {
            p.error("expected type member");
            break;
        }
    }
    if p.eat(SyntaxKind::L_BRACKET) {
        let args = p.start();
        while !p.at(SyntaxKind::R_BRACKET) && !p.at(SyntaxKind::EOF) {
            type_expr(p);
            if !p.eat(SyntaxKind::COMMA) {
                break;
            }
        }
        p.expect(SyntaxKind::R_BRACKET);
        args.complete(p, SyntaxKind::TYPE_ARGS);
    }
    marker.complete(p, SyntaxKind::TYPE_PATH);
}

fn binding_pattern(p: &mut Parser<'_>) {
    let marker = p.start();
    match p.current() {
        SyntaxKind::LOWER_IDENT => p.bump(),
        SyntaxKind::L_PAREN => tuple_or_paren_pattern(p),
        SyntaxKind::L_CURLY => struct_pattern(p),
        SyntaxKind::L_BRACKET => array_pattern(p),
        _ => wildcard_or_unit_pattern(p),
    }
    marker.complete(p, SyntaxKind::BINDING_PATTERN);
}

fn wildcard_or_unit_pattern(p: &mut Parser<'_>) {
    let marker = p.start();
    if p.at(SyntaxKind::LOWER_IDENT) {
        p.bump();
    } else if p.at(SyntaxKind::L_PAREN) && p.nth(1) == SyntaxKind::R_PAREN {
        p.bump();
        p.bump();
        marker.complete(p, SyntaxKind::UNIT_PATTERN);
        return;
    } else if p.at(SyntaxKind::ERROR_TOKEN) {
        p.bump();
    } else {
        p.error("expected binding pattern");
    }
    marker.complete(p, SyntaxKind::WILDCARD_PATTERN);
}

fn tuple_or_paren_pattern(p: &mut Parser<'_>) {
    let marker = p.start();
    p.expect(SyntaxKind::L_PAREN);
    if p.eat(SyntaxKind::R_PAREN) {
        marker.complete(p, SyntaxKind::UNIT_PATTERN);
        return;
    }
    binding_pattern(p);
    if p.eat(SyntaxKind::COMMA) {
        while !p.at(SyntaxKind::R_PAREN) && !p.at(SyntaxKind::EOF) {
            binding_pattern(p);
            if !p.eat(SyntaxKind::COMMA) {
                break;
            }
        }
        p.expect(SyntaxKind::R_PAREN);
        marker.complete(p, SyntaxKind::TUPLE_PATTERN);
    } else {
        p.expect(SyntaxKind::R_PAREN);
        marker.complete(p, SyntaxKind::PAREN_PATTERN);
    }
}

fn struct_pattern(p: &mut Parser<'_>) {
    let marker = p.start();
    p.expect(SyntaxKind::L_CURLY);
    while !p.at(SyntaxKind::R_CURLY) && !p.at(SyntaxKind::EOF) {
        let field = p.start();
        if p.at(SyntaxKind::LOWER_IDENT) {
            p.bump();
            if p.eat(SyntaxKind::COLON) {
                binding_pattern(p);
            }
        } else if p.at(SyntaxKind::DOT2) {
            p.bump();
        } else {
            p.error("expected struct pattern field");
            break;
        }
        field.complete(p, SyntaxKind::STRUCT_PATTERN_FIELD);
        if !p.eat(SyntaxKind::COMMA) {
            break;
        }
    }
    p.expect(SyntaxKind::R_CURLY);
    marker.complete(p, SyntaxKind::STRUCT_PATTERN);
}

fn array_pattern(p: &mut Parser<'_>) {
    let marker = p.start();
    p.expect(SyntaxKind::L_BRACKET);
    while !p.at(SyntaxKind::R_BRACKET) && !p.at(SyntaxKind::EOF) {
        if p.at(SyntaxKind::DOT2) {
            p.bump();
            if p.at(SyntaxKind::LOWER_IDENT) {
                p.bump();
            }
        } else {
            binding_pattern(p);
        }
        if !p.eat(SyntaxKind::COMMA) {
            break;
        }
    }
    p.expect(SyntaxKind::R_BRACKET);
    marker.complete(p, SyntaxKind::ARRAY_PATTERN);
}

fn block_expr(p: &mut Parser<'_>) {
    let marker = p.start();
    p.expect(SyntaxKind::L_CURLY);
    while !p.at(SyntaxKind::R_CURLY) && !p.at(SyntaxKind::EOF) {
        stmt(p);
        if !p.eat(SyntaxKind::SEMICOLON) {
            break;
        }
    }
    p.expect(SyntaxKind::R_CURLY);
    marker.complete(p, SyntaxKind::BLOCK_EXPR);
}

fn stmt(p: &mut Parser<'_>) {
    match p.current() {
        SyntaxKind::LET_KW => let_stmt(p),
        SyntaxKind::RETURN_KW => return_stmt(p),
        _ => expr(p),
    }
}

fn let_stmt(p: &mut Parser<'_>) {
    let marker = p.start();
    p.expect(SyntaxKind::LET_KW);
    binding_pattern(p);
    if p.eat(SyntaxKind::COLON) {
        slot_type(p);
    }
    p.expect(SyntaxKind::EQ);
    expr(p);
    marker.complete(p, SyntaxKind::LET_STMT);
}

fn return_stmt(p: &mut Parser<'_>) {
    let marker = p.start();
    p.expect(SyntaxKind::RETURN_KW);
    if !p.at(SyntaxKind::SEMICOLON) && !p.at(SyntaxKind::R_CURLY) && !p.at(SyntaxKind::EOF) {
        expr(p);
    }
    marker.complete(p, SyntaxKind::RETURN_STMT);
}

fn expr(p: &mut Parser<'_>) {
    match p.current() {
        SyntaxKind::L_CURLY => block_expr(p),
        SyntaxKind::LOWER_IDENT | SyntaxKind::UPPER_IDENT => name_expr(p),
        SyntaxKind::INT
        | SyntaxKind::FLOAT
        | SyntaxKind::STRING
        | SyntaxKind::TRUE_KW
        | SyntaxKind::FALSE_KW => literal_expr(p),
        _ => p.error_and_bump("expected expression"),
    }
}

fn name_expr(p: &mut Parser<'_>) {
    let marker = p.start();
    name(p, true);
    marker.complete(p, SyntaxKind::NAME_REF_EXPR);
}

fn literal_expr(p: &mut Parser<'_>) {
    let marker = p.start();
    p.bump();
    marker.complete(p, SyntaxKind::LITERAL_EXPR);
}

fn name(p: &mut Parser<'_>, allow_upper: bool) {
    match p.current() {
        SyntaxKind::LOWER_IDENT => p.bump(),
        SyntaxKind::UPPER_IDENT if allow_upper => p.bump(),
        _ => p.error("expected identifier"),
    }
}
