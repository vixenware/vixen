use std::mem;

use rowan::{GreenNode, GreenNodeBuilder, Language, TextRange, TextSize};

use crate::diagnostic::{ExpectedKind, LegacySyntaxKind, ParseDiagnostic};
use crate::source::{SourceId, SourceSpan};
use crate::{SyntaxKind, SyntaxNode, Token, VxLanguage, lex};

#[derive(Debug, Clone)]
pub struct Parse {
    green: GreenNode,
    errors: Vec<ParseDiagnostic>,
}

impl Parse {
    pub fn syntax(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.green.clone())
    }

    pub fn errors(&self) -> &[ParseDiagnostic] {
        &self.errors
    }
}

pub fn parse(source: &str, source_id: SourceId) -> Parse {
    let tokens = lex(source);
    let significant = tokens
        .iter()
        .enumerate()
        .filter_map(|(idx, token)| (!token.kind.is_trivia()).then_some(idx))
        .collect::<Vec<_>>();
    let mut parser = Parser::new(source, source_id, &tokens, significant);
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
    Error(ParseDiagnostic),
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
    source_id: SourceId,
    tokens: &'a [Token],
    significant: Vec<usize>,
    pos: usize,
    events: Vec<Event>,
    disallow_block_application: bool,
}

impl<'a> Parser<'a> {
    fn new(
        source: &'a str,
        source_id: SourceId,
        tokens: &'a [Token],
        significant: Vec<usize>,
    ) -> Self {
        Self {
            source,
            source_id,
            tokens,
            significant,
            pos: 0,
            events: Vec::new(),
            disallow_block_application: false,
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

    fn current_text(&self) -> &str {
        let Some(&token_index) = self.significant.get(self.pos) else {
            return "";
        };
        let range = self.tokens[token_index].range;
        &self.source[u32::from(range.start()) as usize..u32::from(range.end()) as usize]
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
            self.expect_token(kind);
        }
    }

    /// Diagnose "expected a specific token kind".
    fn expect_token(&mut self, kind: SyntaxKind) {
        self.diagnose(ParseDiagnostic::Expected {
            what: ExpectedKind::Token(kind),
            at: self.current_span(),
        });
    }

    /// Diagnose "expected <construct>" at the current position.
    fn diagnose_at(&mut self, what: ExpectedKind) {
        self.diagnose(ParseDiagnostic::Expected {
            what,
            at: self.current_span(),
        });
    }

    /// Diagnose a legacy-syntax usage at the current position.
    fn diagnose_legacy(&mut self, kind: LegacySyntaxKind) {
        self.diagnose(ParseDiagnostic::LegacySyntax {
            kind,
            at: self.current_span(),
        });
    }

    fn diagnose(&mut self, d: ParseDiagnostic) {
        self.events.push(Event::Error(d));
    }

    fn diagnose_and_bump(&mut self, what: ExpectedKind) -> CompletedMarker {
        let marker = self.start();
        self.diagnose_at(what);
        if self.current() != SyntaxKind::EOF {
            self.bump();
        }
        marker.complete(self, SyntaxKind::ERROR)
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

    fn current_span(&self) -> SourceSpan {
        crate::source::span_from_range(self.source_id, self.current_range())
    }

    fn has_significant_gap_before_current(&self) -> bool {
        let Some(&current_token_index) = self.significant.get(self.pos) else {
            return false;
        };
        let Some(prev_pos) = self.pos.checked_sub(1) else {
            return false;
        };
        let prev_token_index = self.significant[prev_pos];
        self.tokens[prev_token_index].range.end() != self.tokens[current_token_index].range.start()
    }

    fn recover_while(&mut self, mut keep_going: impl FnMut(SyntaxKind) -> bool) {
        while self.current() != SyntaxKind::EOF && keep_going(self.current()) {
            self.bump();
        }
    }

    fn with_block_application_disallowed<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let prev = self.disallow_block_application;
        self.disallow_block_application = true;
        let result = f(self);
        self.disallow_block_application = prev;
        result
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
    errors: Vec<ParseDiagnostic>,
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

// r[decl.scope]
fn source_file(p: &mut Parser<'_>) {
    let marker = p.start();
    // Leading frontmatter (a ```lang … ``` fence) is a single opaque token,
    // kept as a child of the source file so the runtime can read it back via
    // `SourceFile::frontmatter()`. Its content is foreign (e.g. styx), parsed by
    // the consumer, not here.
    p.eat(SyntaxKind::FRONTMATTER);
    while p.current() != SyntaxKind::EOF {
        stmt(p);
        p.eat(SyntaxKind::SEMICOLON);
    }
    marker.complete(p, SyntaxKind::SOURCE_FILE);
}

fn decl_stmt(p: &mut Parser<'_>) {
    let marker = p.start();
    attributes(p);
    match p.current() {
        SyntaxKind::FN_KW => {
            fn_decl_contents(p, true);
            marker.complete(p, SyntaxKind::FN_DECL);
        }
        SyntaxKind::STRUCT_KW => {
            struct_decl_contents(p, false);
            marker.complete(p, SyntaxKind::STRUCT_DECL);
        }
        SyntaxKind::ENUM_KW => {
            enum_decl_contents(p, false);
            marker.complete(p, SyntaxKind::ENUM_DECL);
        }
        SyntaxKind::NEWTYPE_KW => {
            newtype_decl_contents(p, false);
            marker.complete(p, SyntaxKind::NEWTYPE_DECL);
        }
        SyntaxKind::TYPE_KW => {
            type_alias_decl_contents(p, false);
            marker.complete(p, SyntaxKind::TYPE_ALIAS_DECL);
        }
        SyntaxKind::NAMESPACE_KW => {
            namespace_decl_contents(p);
            marker.complete(p, SyntaxKind::NAMESPACE_DECL);
        }
        _ => {
            p.diagnose_at(ExpectedKind::Declaration);
            p.recover_while(|kind| !is_stmt_start(kind));
            marker.complete(p, SyntaxKind::ERROR);
        }
    }
}

// r[lex.attributes]
fn attributes(p: &mut Parser<'_>) {
    if !at_attribute(p) {
        return;
    }
    let marker = p.start();
    while at_attribute(p) {
        let attr = p.start();
        p.expect(SyntaxKind::HASH);
        p.expect(SyntaxKind::L_BRACKET);
        if !p.at(SyntaxKind::R_BRACKET) {
            loop {
                attribute_entry(p);
                if !p.eat(SyntaxKind::COMMA) || p.at(SyntaxKind::R_BRACKET) {
                    break;
                }
            }
            p.eat(SyntaxKind::COMMA);
        }
        p.expect(SyntaxKind::R_BRACKET);
        attr.complete(p, SyntaxKind::ATTRIBUTE);
    }
    marker.complete(p, SyntaxKind::ATTRIBUTE_LIST);
}

// r[attr.entry] — `key`, `key = value`, or `key(nested, …)` (Rust-like meta).
fn attribute_entry(p: &mut Parser<'_>) {
    let entry = p.start();
    if p.at(SyntaxKind::LOWER_IDENT) {
        p.bump();
    } else {
        p.diagnose_at(ExpectedKind::AttributeKey);
    }
    if p.eat(SyntaxKind::EQ) {
        expr(p);
    } else if p.eat(SyntaxKind::L_PAREN) {
        if !p.at(SyntaxKind::R_PAREN) {
            loop {
                attribute_entry(p);
                if !p.eat(SyntaxKind::COMMA) || p.at(SyntaxKind::R_PAREN) {
                    break;
                }
            }
        }
        p.expect(SyntaxKind::R_PAREN);
    }
    entry.complete(p, SyntaxKind::ATTRIBUTE_ENTRY);
}

// r[decl.fn]
fn fn_decl_contents(p: &mut Parser<'_>, allow_upper_name: bool) {
    p.expect(SyntaxKind::FN_KW);
    name(p, allow_upper_name);
    type_param_list(p);
    parameter_clause(p);
    if p.at(SyntaxKind::WITH_KW) {
        keyword_parameter_block(p);
    }
    if p.eat(SyntaxKind::THIN_ARROW) {
        let ret = p.start();
        type_expr(p);
        ret.complete(p, SyntaxKind::RETURN_TYPE);
    }
    if p.at(SyntaxKind::L_CURLY) {
        block_expr(p);
    } else if p.at(SyntaxKind::L_PAREN) {
        paren_block_expr(p);
    } else if starts_expr(p.current()) {
        expr(p);
    }
}

// r[decl.struct]
fn struct_decl_contents(p: &mut Parser<'_>, allow_anonymous: bool) {
    p.expect(SyntaxKind::STRUCT_KW);
    if p.at(SyntaxKind::UPPER_IDENT) {
        p.bump();
    } else if !allow_anonymous {
        p.diagnose_at(ExpectedKind::TypeName);
    }
    type_param_list(p);
    if p.at(SyntaxKind::L_CURLY) {
        struct_body(p);
    }
}

// r[decl.enum]
fn enum_decl_contents(p: &mut Parser<'_>, allow_anonymous: bool) {
    p.expect(SyntaxKind::ENUM_KW);
    if p.at(SyntaxKind::UPPER_IDENT) {
        p.bump();
    } else if !allow_anonymous {
        p.diagnose_at(ExpectedKind::TypeName);
    }
    type_param_list(p);
    enum_body(p);
}

// r[decl.newtype]
fn newtype_decl_contents(p: &mut Parser<'_>, allow_anonymous: bool) {
    p.expect(SyntaxKind::NEWTYPE_KW);
    if p.at(SyntaxKind::UPPER_IDENT) {
        p.bump();
    } else if !allow_anonymous {
        p.diagnose_at(ExpectedKind::TypeName);
    }
    type_param_list(p);
    p.expect(SyntaxKind::OF_KW);
    type_expr(p);
}

// r[decl.type]
fn type_alias_decl_contents(p: &mut Parser<'_>, allow_anonymous: bool) {
    p.expect(SyntaxKind::TYPE_KW);
    if p.at(SyntaxKind::UPPER_IDENT) {
        p.bump();
    } else if !allow_anonymous {
        p.diagnose_at(ExpectedKind::TypeName);
    }
    type_param_list(p);
    if p.eat(SyntaxKind::EQ) {
        type_expr(p);
    }
}

fn namespace_decl_contents(p: &mut Parser<'_>) {
    p.expect(SyntaxKind::NAMESPACE_KW);
    type_path(p);
    namespace_body(p);
}

fn inline_type_decl(p: &mut Parser<'_>) {
    let marker = p.start();
    attributes(p);
    match p.current() {
        SyntaxKind::STRUCT_KW => {
            struct_decl_contents(p, true);
            marker.complete(p, SyntaxKind::INLINE_STRUCT_DECL);
        }
        SyntaxKind::ENUM_KW => {
            enum_decl_contents(p, true);
            marker.complete(p, SyntaxKind::INLINE_ENUM_DECL);
        }
        SyntaxKind::NEWTYPE_KW => {
            newtype_decl_contents(p, true);
            marker.complete(p, SyntaxKind::INLINE_NEWTYPE_DECL);
        }
        SyntaxKind::TYPE_KW => {
            type_alias_decl_contents(p, true);
            marker.complete(p, SyntaxKind::INLINE_TYPE_ALIAS_DECL);
        }
        _ => {
            p.diagnose_at(ExpectedKind::InlineTypeDeclaration);
            marker.complete(p, SyntaxKind::ERROR);
        }
    }
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
            p.diagnose_at(ExpectedKind::FieldName);
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
        // Three variant forms: `struct Name { … }` (struct-body), `Name(T1, …)`
        // (tuple payload), and bare `Name` (unit). The leading `struct` keyword is
        // optional — only the struct-body form uses it.
        p.eat(SyntaxKind::STRUCT_KW);
        if p.at(SyntaxKind::UPPER_IDENT) {
            p.bump();
        } else {
            p.diagnose_at(ExpectedKind::VariantName);
        }
        if p.at(SyntaxKind::L_CURLY) {
            struct_body(p);
        } else if p.at(SyntaxKind::L_PAREN) {
            variant_tuple(p);
        }
        variant.complete(p, SyntaxKind::VARIANT_DECL);
        if !p.eat(SyntaxKind::COMMA) {
            break;
        }
    }
    p.expect(SyntaxKind::R_CURLY);
    marker.complete(p, SyntaxKind::ENUM_BODY);
}

/// A tuple-variant payload `(T1, T2, …)` — a parenthesized list of element types,
/// the positional analog of a struct body.
fn variant_tuple(p: &mut Parser<'_>) {
    let marker = p.start();
    p.expect(SyntaxKind::L_PAREN);
    while !p.at(SyntaxKind::R_PAREN) && !p.at(SyntaxKind::EOF) {
        type_expr(p);
        if !p.eat(SyntaxKind::COMMA) {
            break;
        }
    }
    p.expect(SyntaxKind::R_PAREN);
    marker.complete(p, SyntaxKind::VARIANT_TUPLE);
}

fn namespace_body(p: &mut Parser<'_>) {
    let marker = p.start();
    p.expect(SyntaxKind::L_CURLY);
    while !p.at(SyntaxKind::R_CURLY) && !p.at(SyntaxKind::EOF) {
        if decl_stmt_start(p) {
            decl_stmt(p);
            p.eat(SyntaxKind::SEMICOLON);
        } else {
            p.diagnose_at(ExpectedKind::Declaration);
            recover_decl_list(p);
            p.eat(SyntaxKind::SEMICOLON);
        }
    }
    p.expect(SyntaxKind::R_CURLY);
    marker.complete(p, SyntaxKind::NAMESPACE_BODY);
}

fn keyword_parameter_block(p: &mut Parser<'_>) {
    p.expect(SyntaxKind::WITH_KW);
    let params = p.start();
    p.expect(SyntaxKind::L_CURLY);
    while !p.at(SyntaxKind::R_CURLY) && !p.at(SyntaxKind::EOF) {
        let param = p.start();
        if p.at(SyntaxKind::LOWER_IDENT) {
            p.bump();
        } else {
            p.diagnose_at(ExpectedKind::KeywordParameterName);
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
    p.expect(SyntaxKind::R_CURLY);
    params.complete(p, SyntaxKind::KEYWORD_PARAMETER_LIST);
}

// r[decl.fn.generic]
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
            p.diagnose_at(ExpectedKind::TypeParameter);
            break;
        }
        if !p.eat(SyntaxKind::COMMA) {
            break;
        }
    }
    p.expect(SyntaxKind::R_BRACKET);
    marker.complete(p, SyntaxKind::TYPE_PARAM_LIST);
}

// r[decl.fn.destructure]
fn parameter_clause(p: &mut Parser<'_>) {
    let marker = p.start();
    p.expect(SyntaxKind::L_PAREN);
    if !p.at(SyntaxKind::R_PAREN) && !p.at(SyntaxKind::SEMICOLON) {
        let params = p.start();
        loop {
            let param = p.start();
            let shorthand_self = p.at(SyntaxKind::LOWER_IDENT) && p.current_text() == "self";
            binding_pattern(p);
            if p.eat(SyntaxKind::COLON) {
                slot_type(p);
            } else if shorthand_self {
                // Allow namespace methods to omit the explicit `self: Type`.
            }
            param.complete(p, SyntaxKind::PARAMETER);
            if !p.eat(SyntaxKind::COMMA) || p.at(SyntaxKind::SEMICOLON) || p.at(SyntaxKind::R_PAREN)
            {
                break;
            }
        }
        params.complete(p, SyntaxKind::PARAMETER_LIST);
    }
    // r[decl.fn.keyword-args]
    if p.eat(SyntaxKind::SEMICOLON) {
        let params = p.start();
        while !p.at(SyntaxKind::R_PAREN) && !p.at(SyntaxKind::EOF) {
            let param = p.start();
            if p.at(SyntaxKind::LOWER_IDENT) {
                p.bump();
            } else {
                p.diagnose_at(ExpectedKind::KeywordParameterName);
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

// r[decl.struct.nested]
fn slot_type(p: &mut Parser<'_>) {
    let marker = p.start();
    if at_attribute(p)
        || matches!(
            p.current(),
            SyntaxKind::STRUCT_KW
                | SyntaxKind::ENUM_KW
                | SyntaxKind::NEWTYPE_KW
                | SyntaxKind::TYPE_KW
        )
    {
        inline_type_decl(p);
    } else {
        type_expr(p);
    }
    marker.complete(p, SyntaxKind::SLOT_TYPE);
}

// r[option.type]
fn type_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let mut lhs = type_sum_term(p);
    while p.at(SyntaxKind::PIPE) {
        let marker = lhs.precede(p);
        while p.eat(SyntaxKind::PIPE) {
            type_sum_term(p);
        }
        lhs = marker.complete(p, SyntaxKind::SUM_TYPE);
    }
    lhs
}

fn type_sum_term(p: &mut Parser<'_>) -> CompletedMarker {
    let mut lhs = match p.current() {
        SyntaxKind::FN_TYPE_KW => function_type(p),
        SyntaxKind::L_PAREN => tuple_or_paren_type(p),
        SyntaxKind::UPPER_IDENT => type_path(p),
        _ => return p.diagnose_and_bump(ExpectedKind::Type),
    };

    if p.at(SyntaxKind::QUESTION) {
        let marker = lhs.precede(p);
        while p.eat(SyntaxKind::QUESTION) {}
        lhs = marker.complete(p, SyntaxKind::OPTIONAL_TYPE);
    }

    lhs
}

// r[fn.type]
fn function_type(p: &mut Parser<'_>) -> CompletedMarker {
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
    if p.at(SyntaxKind::WITH_KW) {
        keyword_parameter_block(p);
    }
    p.expect(SyntaxKind::THIN_ARROW);
    type_expr(p);
    marker.complete(p, SyntaxKind::FUNCTION_TYPE)
}

// r[type.tuple]
fn tuple_or_paren_type(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    p.expect(SyntaxKind::L_PAREN);
    if p.eat(SyntaxKind::R_PAREN) {
        return marker.complete(p, SyntaxKind::TUPLE_TYPE);
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
        marker.complete(p, SyntaxKind::TUPLE_TYPE)
    } else {
        p.expect(SyntaxKind::R_PAREN);
        marker.complete(p, SyntaxKind::PAREN_TYPE)
    }
}

// r[type.generic]
fn type_path(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    p.expect(SyntaxKind::UPPER_IDENT);
    while p.eat(SyntaxKind::DOT) {
        if p.at(SyntaxKind::UPPER_IDENT) {
            p.bump();
        } else {
            p.diagnose_at(ExpectedKind::TypeMember);
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
    marker.complete(p, SyntaxKind::TYPE_PATH)
}

fn binding_pattern(p: &mut Parser<'_>) -> CompletedMarker {
    match p.current() {
        SyntaxKind::LOWER_IDENT => {
            let marker = p.start();
            p.bump();
            marker.complete(p, SyntaxKind::BINDING_PATTERN)
        }
        SyntaxKind::L_PAREN => tuple_or_paren_pattern(p),
        SyntaxKind::L_CURLY => struct_pattern(p),
        SyntaxKind::L_BRACKET => array_pattern(p),
        _ => wildcard_or_unit_pattern(p),
    }
}

fn wildcard_or_unit_pattern(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    if p.at(SyntaxKind::UNDERSCORE) {
        p.bump();
        return marker.complete(p, SyntaxKind::WILDCARD_PATTERN);
    } else if p.at(SyntaxKind::LOWER_IDENT) {
        p.bump();
    } else if p.at(SyntaxKind::L_PAREN) && p.nth(1) == SyntaxKind::R_PAREN {
        p.bump();
        p.bump();
        return marker.complete(p, SyntaxKind::UNIT_PATTERN);
    } else if p.at(SyntaxKind::ERROR_TOKEN) {
        p.bump();
    } else {
        p.diagnose_at(ExpectedKind::BindingPattern);
    }
    marker.complete(p, SyntaxKind::WILDCARD_PATTERN)
}

fn tuple_or_paren_pattern(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    p.expect(SyntaxKind::L_PAREN);
    if p.eat(SyntaxKind::R_PAREN) {
        return marker.complete(p, SyntaxKind::UNIT_PATTERN);
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
        marker.complete(p, SyntaxKind::TUPLE_PATTERN)
    } else {
        p.expect(SyntaxKind::R_PAREN);
        marker.complete(p, SyntaxKind::PAREN_PATTERN)
    }
}

fn struct_pattern(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    p.expect(SyntaxKind::L_CURLY);
    while !p.at(SyntaxKind::R_CURLY) && !p.at(SyntaxKind::EOF) {
        let field = p.start();
        // Lower-ident binds a value export; upper-ident binds a type export
        // (e.g. `let { Grammar, Flag } = import "…"`).
        if p.at(SyntaxKind::LOWER_IDENT) || p.at(SyntaxKind::UPPER_IDENT) {
            p.bump();
            if p.eat(SyntaxKind::COLON) {
                binding_pattern(p);
            }
        } else if p.at(SyntaxKind::DOT2) {
            p.bump();
        } else {
            p.diagnose_at(ExpectedKind::StructPatternField);
            break;
        }
        field.complete(p, SyntaxKind::STRUCT_PATTERN_FIELD);
        if !p.eat(SyntaxKind::COMMA) {
            break;
        }
    }
    p.expect(SyntaxKind::R_CURLY);
    marker.complete(p, SyntaxKind::STRUCT_PATTERN)
}

fn array_pattern(p: &mut Parser<'_>) -> CompletedMarker {
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
    marker.complete(p, SyntaxKind::ARRAY_PATTERN)
}

// r[expr.block]
// r[control.block]
fn block_expr(p: &mut Parser<'_>) -> CompletedMarker {
    delimited_block_expr(p, SyntaxKind::L_CURLY, SyntaxKind::R_CURLY)
}

fn paren_block_expr(p: &mut Parser<'_>) -> CompletedMarker {
    delimited_block_expr(p, SyntaxKind::L_PAREN, SyntaxKind::R_PAREN)
}

fn delimited_block_expr(
    p: &mut Parser<'_>,
    open: SyntaxKind,
    close: SyntaxKind,
) -> CompletedMarker {
    let marker = p.start();
    p.expect(open);
    // r[lex.semicolons]
    while !p.at(close) && !p.at(SyntaxKind::EOF) {
        let stmt_kind = stmt(p);
        if p.eat(SyntaxKind::SEMICOLON) {
            continue;
        }
        if p.at(close) || p.at(SyntaxKind::EOF) {
            break;
        }
        if stmt_kind == StmtKind::Expr && is_stmt_start(p.current()) {
            p.expect_token(SyntaxKind::SEMICOLON);
            continue;
        }
        if stmt_kind == StmtKind::Decl
            && (p.at(close) || p.at(SyntaxKind::EOF) || is_stmt_start(p.current()))
        {
            continue;
        }
        if stmt_kind == StmtKind::Delimited || stmt_kind == StmtKind::Decl {
            p.expect_token(SyntaxKind::SEMICOLON);
        } else {
            p.diagnose_at(ExpectedKind::TokenOrSemicolon(close));
        }
        recover_block(p);
        p.eat(SyntaxKind::SEMICOLON);
    }
    p.expect(close);
    marker.complete(p, SyntaxKind::BLOCK_EXPR)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StmtKind {
    Expr,
    Delimited,
    Decl,
}

fn stmt(p: &mut Parser<'_>) -> StmtKind {
    match p.current() {
        SyntaxKind::LET_KW => {
            let_stmt(p);
            StmtKind::Delimited
        }
        SyntaxKind::RETURN_KW => {
            return_stmt(p);
            StmtKind::Delimited
        }
        _ if decl_stmt_start(p) => {
            decl_stmt(p);
            StmtKind::Decl
        }
        SyntaxKind::LOWER_IDENT if p.nth(1) == SyntaxKind::WITH_EQ => {
            compound_update_stmt(p);
            StmtKind::Delimited
        }
        SyntaxKind::LOWER_IDENT if looks_like_assignment_stmt(p) => {
            assign_or_field_assign_stmt(p);
            StmtKind::Delimited
        }
        _ => {
            expr(p);
            StmtKind::Expr
        }
    }
}

// r[decl.binding]
fn let_stmt(p: &mut Parser<'_>) {
    let marker = p.start();
    p.expect(SyntaxKind::LET_KW);
    binding_pattern(p); // r[decl.binding.destructure]
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

fn compound_update_stmt(p: &mut Parser<'_>) {
    let marker = p.start();
    p.expect(SyntaxKind::LOWER_IDENT);
    p.expect(SyntaxKind::WITH_EQ);
    update_body(p);
    marker.complete(p, SyntaxKind::COMPOUND_UPDATE_STMT);
}

fn assign_or_field_assign_stmt(p: &mut Parser<'_>) {
    if matches!(p.nth(1), SyntaxKind::EQ | SyntaxKind::COLON) {
        let marker = p.start();
        p.expect(SyntaxKind::LOWER_IDENT);
        if p.eat(SyntaxKind::COLON) {
            slot_type(p);
        }
        p.expect(SyntaxKind::EQ);
        expr(p);
        marker.complete(p, SyntaxKind::ASSIGN_STMT);
        return;
    }

    let marker = p.start();
    let target = p.start();
    p.expect(SyntaxKind::LOWER_IDENT);
    while p.at(SyntaxKind::DOT) {
        p.bump();
        p.expect(SyntaxKind::LOWER_IDENT);
    }
    target.complete(p, SyntaxKind::FIELD_ASSIGN_TARGET);
    p.expect(SyntaxKind::EQ);
    expr(p);
    marker.complete(p, SyntaxKind::FIELD_ASSIGN_STMT);
}

fn expr(p: &mut Parser<'_>) {
    pipe_expr(p);
}

fn pipe_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let mut lhs = with_expr(p);
    while p.at(SyntaxKind::PIPE_GT) {
        let marker = lhs.precede(p);
        p.bump();
        with_expr(p);
        lhs = marker.complete(p, SyntaxKind::BINARY_EXPR);
    }
    lhs
}

// r[expr.with]
// r[lex.precedence]
fn with_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let lhs = logical_or_expr(p);
    if p.at(SyntaxKind::WHEREIN_KW) {
        let marker = lhs.precede(p);
        p.bump();
        update_body(p);
        marker.complete(p, SyntaxKind::WITH_EXPR)
    } else {
        lhs
    }
}

fn logical_or_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let mut lhs = logical_and_expr(p);
    while p.at(SyntaxKind::PIPE2) {
        let marker = lhs.precede(p);
        p.bump();
        logical_and_expr(p);
        lhs = marker.complete(p, SyntaxKind::BINARY_EXPR);
    }
    lhs
}

fn logical_and_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let mut lhs = equality_expr(p);
    while p.at(SyntaxKind::AMP2) {
        let marker = lhs.precede(p);
        p.bump();
        equality_expr(p);
        lhs = marker.complete(p, SyntaxKind::BINARY_EXPR);
    }
    lhs
}

fn equality_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let mut lhs = comparison_expr(p);
    while matches!(p.current(), SyntaxKind::EQ2 | SyntaxKind::BANG_EQ) {
        let marker = lhs.precede(p);
        p.bump();
        comparison_expr(p);
        lhs = marker.complete(p, SyntaxKind::BINARY_EXPR);
    }
    lhs
}

fn comparison_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let mut lhs = range_expr(p);
    while matches!(
        p.current(),
        SyntaxKind::LT | SyntaxKind::LT_EQ | SyntaxKind::GT | SyntaxKind::GT_EQ
    ) {
        let marker = lhs.precede(p);
        p.bump();
        range_expr(p);
        lhs = marker.complete(p, SyntaxKind::BINARY_EXPR);
    }
    lhs
}

fn range_expr(p: &mut Parser<'_>) -> CompletedMarker {
    if matches!(p.current(), SyntaxKind::DOT2 | SyntaxKind::DOT2_EQ) {
        let marker = p.start();
        p.bump();
        if starts_expr(p.current()) {
            additive_expr(p);
        }
        return marker.complete(p, SyntaxKind::RANGE_EXPR);
    }

    let lhs = additive_expr(p);
    if matches!(p.current(), SyntaxKind::DOT2 | SyntaxKind::DOT2_EQ) {
        let marker = lhs.precede(p);
        p.bump();
        if starts_expr(p.current()) {
            additive_expr(p);
        }
        marker.complete(p, SyntaxKind::RANGE_EXPR)
    } else {
        lhs
    }
}

fn additive_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let mut lhs = multiplicative_expr(p);
    while matches!(
        p.current(),
        SyntaxKind::PLUS | SyntaxKind::PLUS2 | SyntaxKind::MINUS
    ) {
        let marker = lhs.precede(p);
        p.bump();
        multiplicative_expr(p);
        lhs = marker.complete(p, SyntaxKind::BINARY_EXPR);
    }
    lhs
}

fn multiplicative_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let mut lhs = prefix_expr(p);
    while matches!(
        p.current(),
        SyntaxKind::STAR | SyntaxKind::SLASH | SyntaxKind::PERCENT
    ) {
        let marker = lhs.precede(p);
        p.bump();
        prefix_expr(p);
        lhs = marker.complete(p, SyntaxKind::BINARY_EXPR);
    }
    lhs
}

fn prefix_expr(p: &mut Parser<'_>) -> CompletedMarker {
    match p.current() {
        // `@` no longer introduces anything — commands are backticks now, paths are
        // plain strings. A bare `@` is the old `@key(...)` attribute syntax (now
        // `#[...]`): flag it and recover by consuming the `@` as an error node.
        SyntaxKind::AT => {
            let marker = p.start();
            p.diagnose_legacy(LegacySyntaxKind::AtAttribute);
            p.bump();
            marker.complete(p, SyntaxKind::ERROR)
        }
        SyntaxKind::BANG | SyntaxKind::MINUS => {
            let marker = p.start();
            p.bump();
            prefix_expr(p);
            marker.complete(p, SyntaxKind::PREFIX_EXPR)
        }
        // r[error.fail]
        SyntaxKind::FAIL_KW => {
            let marker = p.start();
            p.bump();
            prefix_expr(p);
            marker.complete(p, SyntaxKind::FAIL_EXPR)
        }
        SyntaxKind::PARTIAL_KW => partial_app_expr(p),
        _ => type_op_expr(p),
    }
}

fn partial_app_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    p.expect(SyntaxKind::PARTIAL_KW);
    application_expr(p);
    marker.complete(p, SyntaxKind::PARTIAL_APP_EXPR)
}

// r[expr.command] — a backtick command invocation `` `$tool args…` ``. The leading
// hole carries the tool (and its grammar); the body is lexed in command mode, and
// here we group its tokens into args, between the opening and closing backtick.
fn command_invocation_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    p.expect(SyntaxKind::BACKTICK);
    while !p.at(SyntaxKind::BACKTICK) && !p.at(SyntaxKind::EOF) {
        if at_cli_arg_start(p) {
            cli_arg(p);
        } else {
            // An unexpected token in the body (shouldn't happen from the lexer);
            // recover by consuming it so we still make progress.
            p.diagnose_at(ExpectedKind::Expression);
            p.bump();
        }
    }
    p.expect(SyntaxKind::BACKTICK);
    marker.complete(p, SyntaxKind::CLI_LITERAL_EXPR)
}

fn at_cli_arg_start(p: &Parser<'_>) -> bool {
    matches!(
        p.current(),
        SyntaxKind::CLI_WORD
            | SyntaxKind::STRING
            | SyntaxKind::CLI_INTERP_VAR
            | SyntaxKind::INTERP_START
    )
}

// One argument is a run of *adjacent* pieces — bare words, quoted strings, and
// `$ident` / `${ expr }` interpolations. A whitespace gap (or `]`) ends it, so
// `-I $dir` is one flag word and one separate argument, while `-I$dir` is one.
fn cli_arg(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    loop {
        match p.current() {
            SyntaxKind::CLI_WORD | SyntaxKind::STRING | SyntaxKind::CLI_INTERP_VAR => p.bump(),
            SyntaxKind::INTERP_START => {
                let hole = p.start();
                p.bump();
                expr(p);
                p.expect(SyntaxKind::INTERP_END);
                hole.complete(p, SyntaxKind::CLI_INTERP);
            }
            _ => break,
        }
        if p.has_significant_gap_before_current() {
            break;
        }
    }
    marker.complete(p, SyntaxKind::CLI_ARG)
}

fn type_op_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let mut lhs = application_expr(p);
    loop {
        lhs = match p.current() {
            // r[expr.as]
            SyntaxKind::AS_KW => {
                let marker = lhs.precede(p);
                p.bump();
                p.eat(SyntaxKind::QUESTION);
                type_expr(p);
                marker.complete(p, SyntaxKind::TYPE_OP_EXPR)
            }
            // r[expr.is]
            SyntaxKind::IS_KW => {
                let marker = lhs.precede(p);
                p.bump();
                pattern(p);
                marker.complete(p, SyntaxKind::TYPE_OP_EXPR)
            }
            // r[expr.ascribe]
            SyntaxKind::COLON2 => {
                let marker = lhs.precede(p);
                p.bump();
                type_expr(p);
                marker.complete(p, SyntaxKind::TYPE_OP_EXPR)
            }
            _ => break,
        };
    }
    lhs
}

fn application_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let mut lhs = atom_expr(p);
    loop {
        if p.at(SyntaxKind::DOT) {
            let marker = lhs.precede(p);
            p.bump();
            if matches!(
                p.current(),
                SyntaxKind::LOWER_IDENT | SyntaxKind::UPPER_IDENT | SyntaxKind::OF_KW
            ) {
                p.bump();
            } else {
                p.diagnose_at(ExpectedKind::FieldName);
            }
            lhs = marker.complete(p, SyntaxKind::FIELD_EXPR);
            continue;
        }

        if starts_tight_type_arg_call(p) {
            let marker = lhs.precede(p);
            value_type_args(p);
            application_arg_expr(p);
            lhs = marker.complete(p, SyntaxKind::CALL_EXPR);
            continue;
        }

        // r[expr.index] — tight `recv[selector]` is a computed selection.
        // (Spaced `recv [arr]` is application; `recv[T](..)` is a type-arg call,
        // both handled above.)
        if p.at(SyntaxKind::L_BRACKET) && !p.has_significant_gap_before_current() {
            let marker = lhs.precede(p);
            index_selector(p);
            lhs = marker.complete(p, SyntaxKind::INDEX_EXPR);
            continue;
        }

        // `with { … }` = keyword args; `where ( … )` = capability/predicate query.
        // Both complete to WHERE_EXPR (the checker dispatches on record vs. predicate).
        if p.at(SyntaxKind::WITH_KW) || p.at(SyntaxKind::WHERE_KW) {
            let marker = lhs.precede(p);
            p.bump();
            expr(p);
            lhs = marker.complete(p, SyntaxKind::WHERE_EXPR);
            continue;
        }

        if starts_application_arg(p) {
            let marker = lhs.precede(p);
            application_arg_expr(p);
            lhs = marker.complete(p, SyntaxKind::CALL_EXPR);
            continue;
        }

        if p.at(SyntaxKind::QUESTION) {
            let marker = lhs.precede(p);
            p.bump();
            lhs = marker.complete(p, SyntaxKind::FALLIBLE_EXPR);
            continue;
        }

        break;
    }
    lhs
}

fn application_arg_expr(p: &mut Parser<'_>) -> CompletedMarker {
    if p.at(SyntaxKind::AT) {
        prefix_expr(p)
    } else {
        atom_expr(p)
    }
}

fn atom_expr(p: &mut Parser<'_>) -> CompletedMarker {
    match p.current() {
        SyntaxKind::COLON if p.nth(1) == SyntaxKind::LOWER_IDENT => symbol_literal_expr(p),
        SyntaxKind::PERCENT_L_CURLY => map_expr(p),
        SyntaxKind::L_CURLY if looks_like_brace_literal(p) => brace_literal_expr(p),
        SyntaxKind::L_CURLY => block_expr(p),
        SyntaxKind::L_PAREN if looks_like_closure(p) => closure_expr(p), // r[expr.closure]
        SyntaxKind::L_PAREN => tuple_or_paren_expr(p), // r[expr.tuple] r[expr.paren]
        SyntaxKind::L_BRACKET => array_expr(p),        // r[expr.array]
        SyntaxKind::IF_KW => if_expr(p),               // r[expr.if] r[control.if]
        SyntaxKind::MATCH_KW => match_expr(p),         // r[expr.match] r[control.match]
        SyntaxKind::IMPORT_KW => import_expr(p),       // r[expr.import]
        SyntaxKind::LOWER_IDENT | SyntaxKind::DOLLAR_IDENT | SyntaxKind::UNDERSCORE => name_expr(p), // r[expr.ident]
        SyntaxKind::UPPER_IDENT => upper_expr(p), // r[expr.named-constructor]
        SyntaxKind::DQUOTE => interpolated_string_expr(p), // r[expr.literal.interpolation]
        SyntaxKind::BACKTICK => command_invocation_expr(p), // r[expr.command]
        // r[expr.literal.int] r[expr.literal.float] r[expr.literal.bool] r[expr.literal.string]
        SyntaxKind::INT
        | SyntaxKind::FLOAT
        | SyntaxKind::STRING
        | SyntaxKind::TRUE_KW
        | SyntaxKind::FALSE_KW => literal_expr(p),
        // r[expr.bomb] — the demand-driven tripwire literal
        SyntaxKind::BOMB => bomb_expr(p),
        _ => p.diagnose_and_bump(ExpectedKind::Expression),
    }
}

// r[expr.bomb]
fn bomb_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    p.expect(SyntaxKind::BOMB);
    marker.complete(p, SyntaxKind::BOMB_EXPR)
}

// r[expr.index] — the selector inside `recv[...]`.
fn index_selector(p: &mut Parser<'_>) {
    p.expect(SyntaxKind::L_BRACKET);
    if !p.at(SyntaxKind::R_BRACKET) {
        expr(p);
    }
    p.expect(SyntaxKind::R_BRACKET);
}

fn symbol_literal_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    p.expect(SyntaxKind::COLON);
    if p.at(SyntaxKind::LOWER_IDENT) {
        p.bump();
    } else {
        p.diagnose_at(ExpectedKind::SymbolName);
    }
    marker.complete(p, SyntaxKind::LITERAL_EXPR)
}

// r[expr.ident]
fn name_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    match p.current() {
        SyntaxKind::LOWER_IDENT | SyntaxKind::DOLLAR_IDENT | SyntaxKind::UNDERSCORE => p.bump(),
        _ => p.diagnose_at(ExpectedKind::Identifier),
    }
    marker.complete(p, SyntaxKind::NAME_REF_EXPR)
}

// r[expr.named-constructor]
fn upper_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    p.expect(SyntaxKind::UPPER_IDENT);
    while p.at(SyntaxKind::DOT) && p.nth(1) == SyntaxKind::UPPER_IDENT {
        p.bump();
        p.bump();
    }
    if p.at(SyntaxKind::L_CURLY) {
        struct_literal_body(p);
        marker.complete(p, SyntaxKind::STRUCT_LITERAL_EXPR)
    } else {
        marker.complete(p, SyntaxKind::NAME_REF_EXPR)
    }
}

// r[expr.literal.int] r[expr.literal.float] r[expr.literal.bool] r[expr.literal.string]
fn literal_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    p.bump();
    marker.complete(p, SyntaxKind::LITERAL_EXPR)
}

// r[expr.literal.interpolation]
fn interpolated_string_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    p.expect(SyntaxKind::DQUOTE);
    while !p.at(SyntaxKind::DQUOTE) && !p.at(SyntaxKind::EOF) {
        // `INTERP_TEXT` is literal text; `INTERP_VAR` is a `$name` shorthand hole.
        if p.at(SyntaxKind::INTERP_TEXT) || p.at(SyntaxKind::INTERP_VAR) {
            p.bump();
        } else if p.at(SyntaxKind::INTERP_START) {
            let interpolation = p.start();
            p.bump();
            expr(p);
            p.expect(SyntaxKind::INTERP_END);
            interpolation.complete(p, SyntaxKind::STRING_INTERPOLATION);
        } else {
            p.diagnose_and_bump(ExpectedKind::InterpolatedStringContent);
        }
    }
    p.expect(SyntaxKind::DQUOTE);
    marker.complete(p, SyntaxKind::INTERPOLATED_STRING_EXPR)
}

// r[expr.tuple]
// r[expr.paren]
fn tuple_or_paren_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    p.expect(SyntaxKind::L_PAREN);
    if p.eat(SyntaxKind::R_PAREN) {
        return marker.complete(p, SyntaxKind::TUPLE_EXPR);
    }
    expr(p);
    if p.eat(SyntaxKind::COMMA) {
        while !p.at(SyntaxKind::R_PAREN) && !p.at(SyntaxKind::EOF) {
            if p.at(SyntaxKind::DOT2) {
                spread_expr(p);
            } else {
                expr(p);
            }
            if !p.eat(SyntaxKind::COMMA) {
                break;
            }
        }
        p.expect(SyntaxKind::R_PAREN);
        marker.complete(p, SyntaxKind::TUPLE_EXPR)
    } else {
        p.expect(SyntaxKind::R_PAREN);
        marker.complete(p, SyntaxKind::PAREN_EXPR)
    }
}

// r[expr.closure]
fn closure_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    let params = p.start();
    p.expect(SyntaxKind::L_PAREN);
    while !p.at(SyntaxKind::R_PAREN) && !p.at(SyntaxKind::EOF) {
        binding_pattern(p);
        if !p.eat(SyntaxKind::COMMA) {
            break;
        }
    }
    p.expect(SyntaxKind::R_PAREN);
    params.complete(p, SyntaxKind::CLOSURE_PARAM_CLAUSE);
    p.expect(SyntaxKind::FAT_ARROW);
    if p.at(SyntaxKind::L_PAREN) && looks_like_paren_block_body(p) {
        paren_block_expr(p);
    } else {
        expr(p);
    }
    marker.complete(p, SyntaxKind::CLOSURE_EXPR)
}

// r[expr.array]
fn array_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    p.expect(SyntaxKind::L_BRACKET);
    while !p.at(SyntaxKind::R_BRACKET) && !p.at(SyntaxKind::EOF) {
        if p.at(SyntaxKind::DOT2) {
            spread_expr(p);
        } else {
            expr(p);
        }
        if !p.eat(SyntaxKind::COMMA) {
            break;
        }
    }
    p.expect(SyntaxKind::R_BRACKET);
    marker.complete(p, SyntaxKind::ARRAY_EXPR)
}

fn brace_literal_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    p.expect(SyntaxKind::L_CURLY);
    while !p.at(SyntaxKind::R_CURLY) && !p.at(SyntaxKind::EOF) {
        let entry = p.start();
        if p.at(SyntaxKind::DOT2) {
            spread_expr(p);
        } else if p.at(SyntaxKind::COLON) {
            p.bump();
            if p.at(SyntaxKind::LOWER_IDENT) {
                p.bump();
            } else {
                p.diagnose_at(ExpectedKind::ShorthandFieldName);
            }
        } else if p.at(SyntaxKind::LOWER_IDENT) {
            p.bump();
            if p.eat(SyntaxKind::COLON) {
                expr(p);
            }
        } else {
            p.diagnose_at(ExpectedKind::RecordFieldName);
            p.bump();
        }
        entry.complete(p, SyntaxKind::MAP_ENTRY);
        if !p.eat(SyntaxKind::COMMA) {
            break;
        }
    }
    p.expect(SyntaxKind::R_CURLY);
    marker.complete(p, SyntaxKind::RECORD_EXPR)
}

// r[expr.map]
fn map_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    p.expect(SyntaxKind::PERCENT_L_CURLY);
    while !p.at(SyntaxKind::R_CURLY) && !p.at(SyntaxKind::EOF) {
        let entry = p.start();
        if p.at(SyntaxKind::DOT2) {
            spread_expr(p);
        } else {
            expr(p);
            p.expect(SyntaxKind::FAT_ARROW);
            expr(p);
        }
        entry.complete(p, SyntaxKind::MAP_ENTRY);
        if !p.eat(SyntaxKind::COMMA) {
            break;
        }
    }
    p.expect(SyntaxKind::R_CURLY);
    marker.complete(p, SyntaxKind::MAP_EXPR)
}

fn spread_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    p.expect(SyntaxKind::DOT2);
    expr(p);
    marker.complete(p, SyntaxKind::SPREAD_EXPR)
}

fn struct_literal_body(p: &mut Parser<'_>) {
    let marker = p.start();
    p.expect(SyntaxKind::L_CURLY);
    while !p.at(SyntaxKind::R_CURLY) && !p.at(SyntaxKind::EOF) {
        let field = p.start();
        if p.at(SyntaxKind::DOT2) {
            p.bump();
            expr(p);
        } else if p.eat(SyntaxKind::COLON) {
            if p.at(SyntaxKind::LOWER_IDENT) {
                p.bump();
            } else {
                p.diagnose_at(ExpectedKind::ShorthandFieldName);
            }
        } else if p.at(SyntaxKind::LOWER_IDENT) {
            p.bump();
            if p.eat(SyntaxKind::COLON) {
                expr(p);
            }
        } else {
            p.diagnose_at(ExpectedKind::StructFieldName);
            p.bump();
        }
        field.complete(p, SyntaxKind::STRUCT_LITERAL_FIELD);
        if !p.eat(SyntaxKind::COMMA) {
            break;
        }
    }
    p.expect(SyntaxKind::R_CURLY);
    marker.complete(p, SyntaxKind::STRUCT_LITERAL_BODY);
}

fn update_body(p: &mut Parser<'_>) {
    let marker = p.start();
    p.expect(SyntaxKind::L_CURLY);
    while !p.at(SyntaxKind::R_CURLY) && !p.at(SyntaxKind::EOF) {
        let field = p.start();
        if p.at(SyntaxKind::DOT2) {
            p.bump();
            expr(p);
        } else if p.eat(SyntaxKind::COLON) {
            field_path(p);
            p.expect(SyntaxKind::WHEREIN_KW);
            update_body(p);
        } else {
            field_path(p);
            p.expect(SyntaxKind::COLON);
            expr(p);
        }
        field.complete(p, SyntaxKind::UPDATE_FIELD);
        if !p.eat(SyntaxKind::COMMA) {
            break;
        }
    }
    p.expect(SyntaxKind::R_CURLY);
    marker.complete(p, SyntaxKind::UPDATE_BODY);
}

fn field_path(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    if p.at(SyntaxKind::LOWER_IDENT) {
        p.bump();
    } else {
        p.diagnose_at(ExpectedKind::FieldPath);
    }
    while p.at(SyntaxKind::DOT) {
        p.bump();
        if p.at(SyntaxKind::LOWER_IDENT) {
            p.bump();
        } else {
            p.diagnose_at(ExpectedKind::FieldName);
            break;
        }
    }
    marker.complete(p, SyntaxKind::FIELD_PATH)
}

// r[expr.if]
// r[control.if]
fn if_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    p.expect(SyntaxKind::IF_KW);
    p.with_block_application_disallowed(expr);
    if p.at(SyntaxKind::L_PAREN) {
        paren_block_expr(p);
    } else {
        block_expr(p);
    }
    if p.at(SyntaxKind::ELSE_KW) {
        let else_clause = p.start();
        p.bump();
        if p.at(SyntaxKind::IF_KW) {
            if_expr(p);
        } else if p.at(SyntaxKind::L_PAREN) {
            paren_block_expr(p);
        } else {
            block_expr(p);
        }
        else_clause.complete(p, SyntaxKind::ELSE_CLAUSE);
    }
    marker.complete(p, SyntaxKind::IF_EXPR)
}

// r[expr.match]
// r[control.match]
fn match_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    p.expect(SyntaxKind::MATCH_KW);
    p.with_block_application_disallowed(expr);
    let close = if p.at(SyntaxKind::L_PAREN) {
        p.bump();
        SyntaxKind::R_PAREN
    } else {
        p.expect(SyntaxKind::L_CURLY);
        SyntaxKind::R_CURLY
    };
    while !p.at(close) && !p.at(SyntaxKind::EOF) {
        let arm = p.start();
        pattern(p);
        if p.eat(SyntaxKind::IF_KW) {
            expr(p);
        }
        p.expect(SyntaxKind::FAT_ARROW);
        if p.at(SyntaxKind::L_PAREN) {
            paren_block_expr(p);
        } else {
            expr(p);
        }
        arm.complete(p, SyntaxKind::MATCH_ARM);
        if p.eat(SyntaxKind::COMMA) {
            continue;
        }
        if p.at(close) || p.at(SyntaxKind::EOF) {
            break;
        }
        p.expect_token(SyntaxKind::COMMA);
        recover_match(p);
        p.eat(SyntaxKind::COMMA);
    }
    p.expect(close);
    marker.complete(p, SyntaxKind::MATCH_EXPR)
}

// r[pattern.or]
fn pattern(p: &mut Parser<'_>) -> CompletedMarker {
    let mut lhs = at_pattern(p);
    while p.at(SyntaxKind::PIPE) {
        let marker = lhs.precede(p);
        p.bump();
        at_pattern(p);
        lhs = marker.complete(p, SyntaxKind::OR_PATTERN);
    }
    lhs
}

// r[pattern.at]
fn at_pattern(p: &mut Parser<'_>) -> CompletedMarker {
    if p.at(SyntaxKind::LOWER_IDENT) && p.nth(1) == SyntaxKind::AT {
        let marker = p.start();
        p.bump();
        p.bump();
        pattern_atom(p);
        marker.complete(p, SyntaxKind::AT_PATTERN)
    } else {
        pattern_atom(p)
    }
}

fn pattern_atom(p: &mut Parser<'_>) -> CompletedMarker {
    match p.current() {
        // r[pattern.wildcard]
        SyntaxKind::UNDERSCORE => {
            let marker = p.start();
            p.bump();
            marker.complete(p, SyntaxKind::WILDCARD_PATTERN)
        }
        // r[pattern.literal]
        SyntaxKind::INT
        | SyntaxKind::FLOAT
        | SyntaxKind::STRING
        | SyntaxKind::TRUE_KW
        | SyntaxKind::FALSE_KW => {
            let marker = p.start();
            p.bump();
            marker.complete(p, SyntaxKind::LITERAL_PATTERN)
        }
        // r[pattern.binding]
        SyntaxKind::LOWER_IDENT => {
            let marker = p.start();
            p.bump();
            marker.complete(p, SyntaxKind::BINDING_PATTERN)
        }
        SyntaxKind::UPPER_IDENT => variant_pattern(p), // r[pattern.variant]
        SyntaxKind::L_PAREN => general_tuple_or_paren_pattern(p), // r[pattern.tuple]
        SyntaxKind::L_CURLY => general_struct_pattern(p), // r[pattern.struct]
        SyntaxKind::L_BRACKET => general_array_pattern(p), // r[pattern.array]
        _ => p.diagnose_and_bump(ExpectedKind::Pattern),
    }
}

// r[pattern.variant]
fn variant_pattern(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    p.expect(SyntaxKind::UPPER_IDENT);
    while p.at(SyntaxKind::DOT) && p.nth(1) == SyntaxKind::UPPER_IDENT {
        p.bump();
        p.bump();
    }
    // `Variant { … }` (struct-form destructure) or `Variant(p1, …)` (tuple-form,
    // positional). A bare `Variant` matches on the tag alone.
    if p.at(SyntaxKind::L_CURLY) {
        general_struct_pattern(p);
    } else if p.at(SyntaxKind::L_PAREN) {
        variant_tuple_pattern(p);
    }
    marker.complete(p, SyntaxKind::VARIANT_PATTERN)
}

/// A tuple-variant destructure `(p1, p2, …)` — positional sub-patterns, bound to
/// the variant's `"0"`, `"1"`, … fields during the IR build.
fn variant_tuple_pattern(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    p.expect(SyntaxKind::L_PAREN);
    while !p.at(SyntaxKind::R_PAREN) && !p.at(SyntaxKind::EOF) {
        pattern(p);
        if !p.eat(SyntaxKind::COMMA) {
            break;
        }
    }
    p.expect(SyntaxKind::R_PAREN);
    marker.complete(p, SyntaxKind::TUPLE_PATTERN)
}

// r[pattern.tuple]
fn general_tuple_or_paren_pattern(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    p.expect(SyntaxKind::L_PAREN);
    if p.eat(SyntaxKind::R_PAREN) {
        return marker.complete(p, SyntaxKind::UNIT_PATTERN);
    }
    pattern(p);
    if p.eat(SyntaxKind::COMMA) {
        while !p.at(SyntaxKind::R_PAREN) && !p.at(SyntaxKind::EOF) {
            pattern(p);
            if !p.eat(SyntaxKind::COMMA) {
                break;
            }
        }
        p.expect(SyntaxKind::R_PAREN);
        marker.complete(p, SyntaxKind::TUPLE_PATTERN)
    } else {
        p.expect(SyntaxKind::R_PAREN);
        marker.complete(p, SyntaxKind::PAREN_PATTERN)
    }
}

// r[pattern.struct]
fn general_struct_pattern(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    p.expect(SyntaxKind::L_CURLY);
    while !p.at(SyntaxKind::R_CURLY) && !p.at(SyntaxKind::EOF) {
        let field = p.start();
        if p.at(SyntaxKind::LOWER_IDENT) {
            p.bump();
            if p.eat(SyntaxKind::COLON) {
                pattern(p);
            }
        } else if p.at(SyntaxKind::DOT2) {
            p.bump();
        } else {
            p.diagnose_at(ExpectedKind::StructPatternField);
            break;
        }
        field.complete(p, SyntaxKind::STRUCT_PATTERN_FIELD);
        if !p.eat(SyntaxKind::COMMA) {
            break;
        }
    }
    p.expect(SyntaxKind::R_CURLY);
    marker.complete(p, SyntaxKind::STRUCT_PATTERN)
}

// r[pattern.array]
fn general_array_pattern(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    p.expect(SyntaxKind::L_BRACKET);
    while !p.at(SyntaxKind::R_BRACKET) && !p.at(SyntaxKind::EOF) {
        if p.at(SyntaxKind::DOT2) {
            let rest = p.start();
            p.bump();
            if p.at(SyntaxKind::LOWER_IDENT) {
                p.bump();
            }
            rest.complete(p, SyntaxKind::ARRAY_REST_PATTERN);
        } else {
            pattern(p);
        }
        if !p.eat(SyntaxKind::COMMA) {
            break;
        }
    }
    p.expect(SyntaxKind::R_BRACKET);
    marker.complete(p, SyntaxKind::ARRAY_PATTERN)
}

// r[expr.import] — `import` is a builtin function: its string argument is
// applied by juxtaposition (`import "x"`), with parens optional (`import("x")`).
fn import_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    p.expect(SyntaxKind::IMPORT_KW);
    let parens = p.eat(SyntaxKind::L_PAREN);
    if p.at(SyntaxKind::STRING) {
        p.bump();
    } else {
        p.diagnose_at(ExpectedKind::StringLiteral);
    }
    if parens {
        p.expect(SyntaxKind::R_PAREN);
    }
    marker.complete(p, SyntaxKind::IMPORT_EXPR)
}

fn looks_like_closure(p: &Parser<'_>) -> bool {
    if !p.at(SyntaxKind::L_PAREN) {
        return false;
    }

    let mut depth = 0usize;
    let mut offset = 0usize;
    loop {
        match p.nth(offset) {
            SyntaxKind::L_PAREN => depth += 1,
            SyntaxKind::R_PAREN => {
                depth -= 1;
                if depth == 0 {
                    return p.nth(offset + 1) == SyntaxKind::FAT_ARROW;
                }
            }
            SyntaxKind::EOF => return false,
            _ => {}
        }
        offset += 1;
    }
}

fn looks_like_brace_literal(p: &Parser<'_>) -> bool {
    if !p.at(SyntaxKind::L_CURLY) {
        return false;
    }

    let mut depth = 0usize;
    let mut offset = 0usize;
    let mut saw_any = false;
    let mut only_bare_labels = true;
    let mut expecting_entry_start = true;

    loop {
        match p.nth(offset) {
            SyntaxKind::L_CURLY
            | SyntaxKind::PERCENT_L_CURLY
            | SyntaxKind::L_PAREN
            | SyntaxKind::L_BRACKET => {
                depth += 1;
            }
            SyntaxKind::R_CURLY | SyntaxKind::R_PAREN | SyntaxKind::R_BRACKET => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            SyntaxKind::EOF => return false,
            SyntaxKind::SEMICOLON
            | SyntaxKind::LET_KW
            | SyntaxKind::RETURN_KW
            | SyntaxKind::FN_KW
                if depth == 1 =>
            {
                return false;
            }
            SyntaxKind::COLON | SyntaxKind::DOT2 if depth == 1 => {
                return true;
            }
            SyntaxKind::COMMA if depth == 1 => {
                if expecting_entry_start {
                    return false;
                }
                expecting_entry_start = true;
            }
            SyntaxKind::LOWER_IDENT if depth == 1 && expecting_entry_start => {
                saw_any = true;
                expecting_entry_start = false;
            }
            kind if depth == 1 && expecting_entry_start => {
                saw_any = !matches!(kind, SyntaxKind::WHITESPACE);
                only_bare_labels = false;
                expecting_entry_start = false;
            }
            _ if depth == 1 => {
                only_bare_labels = false;
            }
            _ => {}
        }

        offset += 1;
    }

    !saw_any || only_bare_labels
}

fn looks_like_paren_block_body(p: &Parser<'_>) -> bool {
    if !p.at(SyntaxKind::L_PAREN) {
        return false;
    }

    let mut depth = 0usize;
    let mut offset = 0usize;
    loop {
        match p.nth(offset) {
            SyntaxKind::L_PAREN
            | SyntaxKind::L_BRACKET
            | SyntaxKind::L_CURLY
            | SyntaxKind::PERCENT_L_CURLY => {
                depth += 1;
            }
            SyntaxKind::R_PAREN | SyntaxKind::R_BRACKET | SyntaxKind::R_CURLY => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return false;
                }
            }
            SyntaxKind::SEMICOLON if depth == 1 => return true,
            SyntaxKind::LET_KW
            | SyntaxKind::RETURN_KW
            | SyntaxKind::FN_KW
            | SyntaxKind::STRUCT_KW
            | SyntaxKind::ENUM_KW
            | SyntaxKind::NEWTYPE_KW
            | SyntaxKind::TYPE_KW
            | SyntaxKind::NAMESPACE_KW
                if depth == 1 =>
            {
                return true;
            }
            SyntaxKind::EOF => return false,
            _ => {}
        }
        offset += 1;
    }
}

fn starts_expr(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::BANG
            | SyntaxKind::MINUS
            | SyntaxKind::FAIL_KW
            | SyntaxKind::PARTIAL_KW
            | SyntaxKind::IF_KW
            | SyntaxKind::MATCH_KW
            | SyntaxKind::IMPORT_KW
            | SyntaxKind::UNDERSCORE
            | SyntaxKind::LOWER_IDENT
            | SyntaxKind::UPPER_IDENT
            | SyntaxKind::DOLLAR_IDENT
            | SyntaxKind::COLON
            | SyntaxKind::L_PAREN
            | SyntaxKind::L_BRACKET
            | SyntaxKind::L_CURLY
            | SyntaxKind::PERCENT_L_CURLY
            | SyntaxKind::AT
            | SyntaxKind::DQUOTE
            | SyntaxKind::BACKTICK
            | SyntaxKind::INT
            | SyntaxKind::FLOAT
            | SyntaxKind::STRING
            | SyntaxKind::TRUE_KW
            | SyntaxKind::FALSE_KW
            | SyntaxKind::BOMB
    )
}

fn starts_application_arg(p: &Parser<'_>) -> bool {
    let kind = p.current();
    // String literals (raw `'x'`, plain `"x"`=STRING, interpolated `"x${y}"`=DQUOTE)
    // and backtick command invocations are juxtaposable args — paths are strings
    // now, so `f "x"` is application, exactly as `f @path` once was.
    matches!(
        kind,
        SyntaxKind::L_BRACKET | SyntaxKind::STRING | SyntaxKind::DQUOTE | SyntaxKind::BACKTICK
    ) && p.has_significant_gap_before_current()
        || (kind == SyntaxKind::L_PAREN && !p.has_significant_gap_before_current())
        || (kind == SyntaxKind::L_CURLY
            && !p.disallow_block_application
            && p.has_significant_gap_before_current())
        || (kind == SyntaxKind::PERCENT_L_CURLY && p.has_significant_gap_before_current())
}

fn starts_tight_type_arg_call(p: &Parser<'_>) -> bool {
    if p.current() != SyntaxKind::L_BRACKET || p.has_significant_gap_before_current() {
        return false;
    }

    let Some(close_pos) =
        find_matching_significant_token(p, SyntaxKind::L_BRACKET, SyntaxKind::R_BRACKET)
    else {
        return false;
    };
    let Some(&next_token_index) = p.significant.get(close_pos + 1) else {
        return false;
    };
    let close_token_index = p.significant[close_pos];
    p.tokens[next_token_index].kind == SyntaxKind::L_PAREN
        && p.tokens[close_token_index].range.end() == p.tokens[next_token_index].range.start()
}

fn find_matching_significant_token(
    p: &Parser<'_>,
    open: SyntaxKind,
    close: SyntaxKind,
) -> Option<usize> {
    let mut depth = 0usize;
    for pos in p.pos..p.significant.len() {
        match p.tokens[p.significant[pos]].kind {
            kind if kind == open => depth += 1,
            kind if kind == close => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(pos);
                }
            }
            SyntaxKind::EOF => break,
            _ => {}
        }
    }
    None
}

fn value_type_args(p: &mut Parser<'_>) -> CompletedMarker {
    let marker = p.start();
    p.expect(SyntaxKind::L_BRACKET);
    while !p.at(SyntaxKind::R_BRACKET) && !p.at(SyntaxKind::EOF) {
        type_expr(p);
        if !p.eat(SyntaxKind::COMMA) {
            break;
        }
    }
    p.expect(SyntaxKind::R_BRACKET);
    marker.complete(p, SyntaxKind::TYPE_ARGS)
}

fn at_attribute(p: &Parser<'_>) -> bool {
    p.at(SyntaxKind::HASH) && p.nth(1) == SyntaxKind::L_BRACKET
}

fn attribute_followed_by_decl(p: &Parser<'_>) -> bool {
    let mut offset = 0usize;
    if p.nth(offset) != SyntaxKind::HASH || p.nth(offset + 1) != SyntaxKind::L_BRACKET {
        return false;
    }

    while p.nth(offset) == SyntaxKind::HASH && p.nth(offset + 1) == SyntaxKind::L_BRACKET {
        offset += 2;
        let mut bracket_depth = 1usize;
        while bracket_depth > 0 {
            match p.nth(offset) {
                SyntaxKind::L_BRACKET => bracket_depth += 1,
                SyntaxKind::R_BRACKET => bracket_depth -= 1,
                SyntaxKind::EOF => return false,
                _ => {}
            }
            offset += 1;
        }
    }

    matches!(
        p.nth(offset),
        SyntaxKind::FN_KW
            | SyntaxKind::STRUCT_KW
            | SyntaxKind::ENUM_KW
            | SyntaxKind::NEWTYPE_KW
            | SyntaxKind::TYPE_KW
            | SyntaxKind::NAMESPACE_KW
    )
}

fn decl_stmt_start(p: &Parser<'_>) -> bool {
    matches!(
        p.current(),
        SyntaxKind::FN_KW
            | SyntaxKind::STRUCT_KW
            | SyntaxKind::ENUM_KW
            | SyntaxKind::NEWTYPE_KW
            | SyntaxKind::TYPE_KW
            | SyntaxKind::NAMESPACE_KW
    ) || attribute_followed_by_decl(p)
}

fn is_item_start(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::HASH
            | SyntaxKind::FN_KW
            | SyntaxKind::STRUCT_KW
            | SyntaxKind::ENUM_KW
            | SyntaxKind::NEWTYPE_KW
            | SyntaxKind::TYPE_KW
            | SyntaxKind::NAMESPACE_KW
    )
}

fn is_stmt_start(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::LET_KW
            | SyntaxKind::RETURN_KW
            | SyntaxKind::FN_KW
            | SyntaxKind::STRUCT_KW
            | SyntaxKind::ENUM_KW
            | SyntaxKind::NEWTYPE_KW
            | SyntaxKind::TYPE_KW
            | SyntaxKind::NAMESPACE_KW
            | SyntaxKind::HASH
            | SyntaxKind::LOWER_IDENT
    ) || starts_expr(kind)
}

fn is_pattern_start(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::UNDERSCORE
            | SyntaxKind::LOWER_IDENT
            | SyntaxKind::UPPER_IDENT
            | SyntaxKind::L_PAREN
            | SyntaxKind::L_CURLY
            | SyntaxKind::L_BRACKET
            | SyntaxKind::INT
            | SyntaxKind::FLOAT
            | SyntaxKind::STRING
            | SyntaxKind::TRUE_KW
            | SyntaxKind::FALSE_KW
    )
}

fn recover_block(p: &mut Parser<'_>) {
    p.recover_while(|kind| {
        !matches!(kind, SyntaxKind::SEMICOLON | SyntaxKind::R_CURLY)
            && !is_stmt_start(kind)
            && !is_item_start(kind)
    });
}

fn recover_decl_list(p: &mut Parser<'_>) {
    p.recover_while(|kind| {
        !matches!(kind, SyntaxKind::SEMICOLON | SyntaxKind::R_CURLY) && !is_item_start(kind)
    });
}

fn recover_match(p: &mut Parser<'_>) {
    p.recover_while(|kind| {
        !matches!(kind, SyntaxKind::COMMA | SyntaxKind::R_CURLY) && !is_pattern_start(kind)
    });
}

fn looks_like_assignment_stmt(p: &Parser<'_>) -> bool {
    if p.current() != SyntaxKind::LOWER_IDENT {
        return false;
    }

    let mut offset = 1usize;
    if p.nth(offset) == SyntaxKind::COLON {
        return true;
    }
    if p.nth(offset) == SyntaxKind::EQ {
        return true;
    }
    while p.nth(offset) == SyntaxKind::DOT && p.nth(offset + 1) == SyntaxKind::LOWER_IDENT {
        offset += 2;
    }
    p.nth(offset) == SyntaxKind::EQ
}

fn name(p: &mut Parser<'_>, allow_upper: bool) {
    match p.current() {
        SyntaxKind::LOWER_IDENT => p.bump(),
        SyntaxKind::UPPER_IDENT if allow_upper => p.bump(),
        _ => p.diagnose_at(ExpectedKind::Identifier),
    }
}
