use std::collections::HashMap;
use std::sync::Mutex;

use rowan::TextRange;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};
use vx_ir::arena::VirArena;
use vx_ir::expr::ExprKind;
use vx_ir::item::ItemKind;
use vx_ir::scope::{DefTarget, ScopeEntryTarget, ScopeOwner};
use vx_ir::span_index::SpanIndex;
use vx_ir::type_check::TypeCheckVirResult;
use vx_ir::{BindingId, ItemId, ScopeId, TypeCollection};
use vx_parser::{self as vx, AstNode, FnDecl, Parsed, SourceFile, SyntaxKind, trimmed_range};

use vx_ir::{CapCheckResult, DefId, DefKind, Definition};
use vx_parser::{SourceId, SourceSpan};
pub struct VxLanguageServer {
    client: Client,
    documents: Mutex<HashMap<Url, DocumentState>>,
}

pub(crate) struct DocumentState {
    current_source: SourceId,
    text: String,
    parsed: Parsed<SourceFile>,
    line_index: vx::LineIndex,
    source_docs: HashMap<SourceId, SourceDoc>,
    arena: VirArena,
    span_index: SpanIndex,
    types: TypeCollection,
    type_check: TypeCheckVirResult,
    cap_check: CapCheckResult,
    resolve_errors: Vec<vx_ir::IrDiagnostic>,
    /// Cached materialized Definitions for `all_definitions_in` queries.
    /// Keyed by source so workspace_symbol can scan all loaded docs.
    all_defs: Vec<Definition>,
}

struct SourceDoc {
    uri: Option<Url>,
    text: String,
    parsed: Parsed<SourceFile>,
    line_index: vx::LineIndex,
}

impl DocumentState {
    #[cfg(test)]
    pub(crate) fn types(&self) -> &TypeCollection {
        &self.types
    }

    #[cfg(test)]
    pub(crate) fn current_source(&self) -> &SourceId {
        &self.current_source
    }

    /// Test-only: enumerate all materialized definitions in the current source.
    #[cfg(test)]
    pub(crate) fn all_definitions(&self) -> &[Definition] {
        &self.all_defs
    }

    pub(crate) fn new_with_uri(text: String, current_uri: Option<Url>) -> Self {
        let parsed = vx::parse_source_file(&text);
        let line_index = vx::LineIndex::new(&text);

        // ------------------------------------------------------------------
        // Build source catalog: user source first (SourceId(0)), then stdlib.
        // Mirrors vx_ir::compile::compile_source.
        // ------------------------------------------------------------------
        let current_source = SourceId(0);
        let mut source_docs: HashMap<SourceId, SourceDoc> = HashMap::new();
        source_docs.insert(
            current_source,
            SourceDoc {
                uri: current_uri.clone(),
                text: text.clone(),
                parsed: parsed.clone(),
                line_index: vx::LineIndex::new(&text),
            },
        );

        let mut sources: Vec<(SourceId, vx::SourceFile)> = vec![(current_source, parsed.tree())];
        for (i, stdlib) in vx_stdlib::stdlib_sources().iter().enumerate() {
            let stdlib_id = SourceId(i as u32 + 1);
            let stdlib_parsed = vx::parse_source_file(stdlib.text);
            let stdlib_line_index = vx::LineIndex::new(stdlib.text);
            let uri = Url::from_file_path(vx_stdlib::source_disk_path(stdlib.logical_path)).ok();
            source_docs.insert(
                stdlib_id,
                SourceDoc {
                    uri,
                    text: stdlib.text.to_string(),
                    parsed: stdlib_parsed.clone(),
                    line_index: stdlib_line_index,
                },
            );
            sources.push((stdlib_id, stdlib_parsed.tree()));
        }

        // ------------------------------------------------------------------
        // VIR build + semantic passes.
        // ------------------------------------------------------------------
        let mut program = vx_ir::build::VirBuilder::build_program(&sources);
        vx_ir::compile::inject_implicit_stdlib_imports(&mut program);

        let types = vx_ir::type_collect::collect_types_vir(&mut program.arena);
        let resolve_errors = vx_ir::resolve::resolve_names(&mut program.arena);
        let type_check = vx_ir::type_check::type_check_vir(&mut program.arena, &types);
        let cap_check =
            vx_ir::cap_check::check_capabilities_vir(&program.arena, &types, &type_check);

        let span_index = SpanIndex::build(&program.arena);
        let arena = program.arena;
        let all_defs = materialize_all_definitions(&arena, current_source);

        Self {
            current_source,
            text,
            parsed,
            line_index,
            source_docs,
            arena,
            span_index,
            types,
            type_check,
            cap_check,
            resolve_errors,
            all_defs,
        }
    }

    // ------------------------------------------------------------------
    // Span-index-backed queries.
    // ------------------------------------------------------------------

    /// Materialize a `Definition` for whatever the SpanIndex finds at this name span.
    fn definition_at_name_span(&self, span: SourceSpan) -> Option<Definition> {
        if let Some(item_id) = self.span_index.item_at_name_span(span) {
            return definition_from_item(&self.arena, item_id);
        }
        if let Some(binding_id) = self.span_index.binding_at_name_span(span) {
            return Some(definition_from_binding(&self.arena, binding_id));
        }
        None
    }

    /// Materialize a Definition from a resolved DefTarget.
    fn definition_from_target(&self, target: DefTarget) -> Option<Definition> {
        match target {
            DefTarget::Item(item_id) => definition_from_item(&self.arena, item_id),
            DefTarget::Binding(binding_id) => {
                Some(definition_from_binding(&self.arena, binding_id))
            }
            DefTarget::Variant {
                enum_item,
                variant_index,
            } => definition_from_variant(&self.arena, enum_item, variant_index),
        }
    }

    /// Lookup the expression at exactly this span (unwrapping parens via CST).
    fn expr_ty_at(&self, span: SourceSpan) -> Option<vx_types::Ty> {
        let expr_id = self.span_index.expr_at(span)?;
        self.arena[expr_id].ty.clone()
    }

    /// Walk the CST to unwrap paren expressions, return the inner span.
    fn span_unwrapping_parens(&self, expr: &vx::Expr) -> SourceSpan {
        let mut cur = expr.clone();
        while let vx::Expr::Paren(p) = &cur {
            match p.expr() {
                Some(inner) => cur = inner,
                None => break,
            }
        }
        self.current_span(cur.syntax().text_range())
    }

    /// Resolve a name-ref-like token to a definition by walking up the CST
    /// to find the enclosing NAME_REF_EXPR node and looking up its span in
    /// the SpanIndex. IR spans use the full text_range (with leading trivia)
    /// while token text_ranges are trimmed.
    fn resolved_target_for_token(&self, token: &vx::SyntaxToken) -> Option<DefTarget> {
        let mut node = token.parent();
        while let Some(n) = node {
            let span = self.current_span(n.text_range());
            if let Some(expr_id) = self.span_index.name_ref_at(span, &self.arena)
                && let ExprKind::NameRef(nr) = &self.arena[expr_id].kind
            {
                return nr.resolved;
            }
            if matches!(n.kind(), SyntaxKind::NAME_REF_EXPR) {
                break;
            }
            node = n.parent();
        }
        None
    }

    /// Find the definition under the cursor and the range that should be
    /// treated as the origin span for editor features.
    pub(crate) fn definition_with_origin_at(
        &self,
        offset: rowan::TextSize,
    ) -> Option<(Definition, SourceSpan)> {
        // 1. Try both right-biased and left-biased tokens at the offset.
        // The offset may sit at a token boundary; we need to consider both
        // sides so cursor placement just before an identifier still resolves.
        let token_at = self.parsed.tree().syntax().token_at_offset(offset);
        let candidates: Vec<vx::SyntaxToken> = match token_at {
            rowan::TokenAtOffset::None => return None,
            rowan::TokenAtOffset::Single(t) => vec![t],
            rowan::TokenAtOffset::Between(left, right) => vec![right, left],
        };

        for token in candidates {
            // The IR stores spans that may include leading trivia (e.g. the
            // NAME_REF_EXPR CST node's full text_range). The token's range is
            // trimmed. Look up via both: first the token (matches name_span on
            // bindings/items), then the enclosing CST node (matches IR exprs).
            let token_span = self.current_span(token.text_range());

            // Cursor on a definition site (bindings/items use token spans).
            if let Some(def) = self.definition_at_name_span(token_span) {
                let span = def.name_span;
                return Some((def, span));
            }

            // Member-defs side table is keyed by the token (CST node) span
            // for field/method references — check both.
            if let Some(def) = self.type_check.member_defs.get(&token_span) {
                return Some((def.clone(), token_span));
            }

            // Walk up the syntax tree to find an enclosing NAME_REF_EXPR.
            let mut node = token.parent();
            while let Some(n) = node {
                let n_span = self.current_span(n.text_range());

                // Cursor on a name reference via the CST node's full span.
                if let Some(expr_id) = self.span_index.name_ref_at(n_span, &self.arena)
                    && let ExprKind::NameRef(nr) = &self.arena[expr_id].kind
                    && let Some(target) = nr.resolved
                    && let Some(def) = self.definition_from_target(target)
                {
                    return Some((def, self.current_span(trimmed_range(&n))));
                }

                // Member-defs may be keyed by the enclosing node span too.
                if let Some(def) = self.type_check.member_defs.get(&n_span) {
                    return Some((def.clone(), self.current_span(trimmed_range(&n))));
                }

                // Stop once we exit identifier-like syntax.
                if matches!(n.kind(), SyntaxKind::NAME_REF_EXPR | SyntaxKind::FIELD_EXPR) {
                    break;
                }
                node = n.parent();
            }
        }

        None
    }

    /// Find the definition under the cursor. Works whether the cursor is on
    /// a definition site or on a reference that resolves to one.
    pub(crate) fn definition_at(&self, offset: rowan::TextSize) -> Option<Definition> {
        Some(self.definition_with_origin_at(offset)?.0)
    }

    /// Find all reference ranges that point to a given definition.
    /// Returns trimmed spans (the identifier token range, excluding any
    /// leading trivia carried by the IR span).
    fn references_to(&self, def: &Definition) -> Vec<SourceSpan> {
        let mut refs = Vec::new();
        for expr in &self.arena.exprs {
            if let ExprKind::NameRef(nr) = &expr.kind
                && let Some(target) = nr.resolved
                && let Some(target_def) = self.definition_from_target(target)
                && target_def.id == def.id
            {
                refs.push(self.trim_span(expr.span));
            }
            if let ExprKind::StructLiteral(sl) = &expr.kind
                && let Some(item_id) = sl.resolved
                && let Some(target_def) = definition_from_item(&self.arena, item_id)
                && target_def.id == def.id
            {
                refs.push(sl.name_span);
            }
        }
        for (span, target) in &self.type_check.member_defs {
            if target.id == def.id {
                refs.push(*span);
            }
        }
        refs
    }

    /// Trim a span to exclude leading/trailing trivia by finding the
    /// enclosing CST node and using its trimmed range. Falls back to the
    /// original span if no node matches.
    fn trim_span(&self, span: SourceSpan) -> SourceSpan {
        let Some(doc) = self.source_doc(span.source) else {
            return span;
        };
        let root = doc.parsed.tree().syntax().clone();
        if let Some(node) = root.descendants().find(|n| n.text_range() == span.range) {
            SourceSpan::new(span.source, trimmed_range(&node))
        } else {
            span
        }
    }

    fn source_doc(&self, source: SourceId) -> Option<&SourceDoc> {
        self.source_docs.get(&source)
    }

    pub(crate) fn uri_for_source(&self, source: SourceId) -> Option<Url> {
        self.source_doc(source).and_then(|doc| doc.uri.clone())
    }

    /// Return the slice of source text at `span`. Falls back to an empty
    /// string when the source isn't known (e.g. synthetic spans).
    pub(crate) fn text_at(&self, span: SourceSpan) -> &str {
        let text: &str = if span.source == self.current_source {
            &self.text
        } else {
            match self.source_doc(span.source) {
                Some(doc) => &doc.text,
                None => return "",
            }
        };
        let start = u32::from(span.range.start()) as usize;
        let end = u32::from(span.range.end()) as usize;
        text.get(start..end).unwrap_or("")
    }

    /// Extract a declaration snippet for hover display.
    /// Takes the first line of the full declaration range.
    pub(crate) fn declaration_snippet(&self, def: &Definition) -> String {
        let Some(doc) = self.source_doc(def.source()) else {
            return String::new();
        };
        let start = u32::from(def.full_span.start()) as usize;
        let end = if def.kind == DefKind::Function {
            self.declaration_node_for(def)
                .and_then(FnDecl::cast)
                .and_then(|decl| decl.body_expr())
                .map(|body| u32::from(body.syntax().text_range().start()) as usize)
                .unwrap_or_else(|| u32::from(def.full_span.end()) as usize)
        } else {
            u32::from(def.full_span.end()) as usize
        };
        let slice = &doc.text[start..end];
        slice
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with("//"))
            .unwrap_or(slice.trim())
            .to_string()
    }

    /// Extract the doc comment attached to a definition's syntax node.
    pub(crate) fn doc_comment_for(&self, def: &Definition) -> Option<String> {
        let node = self.declaration_node_for(def)?;
        vx::doc_comment(&node)
    }

    pub(crate) fn documentation_for(&self, def: &Definition) -> Option<String> {
        self.doc_comment_for(def).or_else(|| {
            // closure_param_docs is keyed by BindingId in vx-ir.
            let binding_id = self.span_index.binding_at_name_span(def.name_span)?;
            self.type_check.closure_param_docs.get(&binding_id).cloned()
        })
    }

    fn declaration_node_for(&self, def: &Definition) -> Option<vx::SyntaxNode> {
        let expected_kind = match def.kind {
            DefKind::Function => SyntaxKind::FN_DECL,
            DefKind::Namespace => SyntaxKind::NAMESPACE_DECL,
            DefKind::Struct => SyntaxKind::STRUCT_DECL,
            DefKind::Enum => SyntaxKind::ENUM_DECL,
            DefKind::Newtype => SyntaxKind::NEWTYPE_DECL,
            DefKind::TypeAlias => SyntaxKind::TYPE_ALIAS_DECL,
            DefKind::Binding | DefKind::Parameter | DefKind::Variant | DefKind::Field => {
                return None;
            }
        };

        self.source_doc(def.source())?
            .parsed
            .tree()
            .syntax()
            .descendants()
            .find(|node| node.kind() == expected_kind && node.text_range() == def.full_span.range)
    }

    fn fn_decl_for(&self, def: &Definition) -> Option<FnDecl> {
        if def.kind != DefKind::Function {
            return None;
        }
        FnDecl::cast(self.declaration_node_for(def)?)
    }

    pub(crate) fn inferred_def_type(&self, def: &Definition) -> Option<vx_types::Ty> {
        // For local bindings & parameters: type is on Binding.ty.
        if let Some(binding_id) = self.span_index.binding_at_name_span(def.name_span)
            && let Some(ty) = self.arena[binding_id].ty.clone()
        {
            return Some(ty);
        }

        // Fall back to walking references — first reference site with a type wins.
        for expr in &self.arena.exprs {
            let ExprKind::NameRef(nr) = &expr.kind else {
                continue;
            };
            let Some(target) = nr.resolved else {
                continue;
            };
            let Some(target_def) = self.definition_from_target(target) else {
                continue;
            };
            if target_def.id == def.id
                && let Some(ty) = expr.ty.clone()
            {
                return Some(ty);
            }
        }

        // Last fallback: the definition's own span as an expression.
        self.expr_ty_at(def.name_span)
    }

    pub(crate) fn to_lsp_range(&self, range: TextRange) -> Range {
        self.to_lsp_range_span(SourceSpan::new(self.current_source, range))
    }

    pub(crate) fn to_lsp_range_span(&self, span: SourceSpan) -> Range {
        let line_index = self
            .source_doc(span.source)
            .map(|doc| &doc.line_index)
            .unwrap_or(&self.line_index);
        let start = line_index.position(span.range.start());
        let end = line_index.position(span.range.end());
        Range {
            start: Position::new(start.line, start.col),
            end: Position::new(end.line, end.col),
        }
    }

    fn current_span(&self, range: TextRange) -> SourceSpan {
        SourceSpan::new(self.current_source, range)
    }

    /// Enumerate all materialized definitions in the current source.
    fn all_definitions_in_current(&self) -> &[Definition] {
        &self.all_defs
    }

    /// Enumerate definitions visible at a particular offset in the current source.
    /// Uses vx-ir's scope lookup machinery to walk the scope chain.
    fn visible_at(&self, offset: rowan::TextSize) -> Vec<Definition> {
        let from_scope = self.scope_at_offset(offset);
        visible_definitions_at(&self.arena, from_scope, offset)
    }

    /// Find the innermost scope owning `offset` in the current source.
    fn scope_at_offset(&self, offset: rowan::TextSize) -> ScopeId {
        innermost_scope_at(&self.arena, self.current_source, offset)
    }
}

impl VxLanguageServer {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: Mutex::new(HashMap::new()),
        }
    }

    fn with_document<F, T>(&self, uri: &Url, f: F) -> Option<T>
    where
        F: FnOnce(&DocumentState) -> T,
    {
        let docs = self.documents.lock().unwrap();
        docs.get(uri).map(f)
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for VxLanguageServer {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".to_string()]),
                    ..Default::default()
                }),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec![
                        " ".to_string(),
                        "{".to_string(),
                        ":".to_string(),
                    ]),
                    retrigger_characters: None,
                    work_done_progress_options: Default::default(),
                }),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: Default::default(),
                })),
                inlay_hint_provider: Some(OneOf::Left(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                document_highlight_provider: Some(OneOf::Left(true)),
                selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: SemanticTokensLegend {
                                token_types: vx::SemanticTokenType::LEGEND
                                    .iter()
                                    .map(|s| SemanticTokenType::new(s))
                                    .collect(),
                                token_modifiers: vec![SemanticTokenModifier::DEFAULT_LIBRARY],
                            },
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            range: None,
                            ..Default::default()
                        },
                    ),
                ),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                call_hierarchy_provider: Some(CallHierarchyServerCapability::Simple(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                linked_editing_range_provider: None,
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        tracing::info!("vx-lsp initialized");
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        let state = DocumentState::new_with_uri(text, Some(uri.clone()));
        self.publish_diagnostics(&uri, &state).await;
        self.documents.lock().unwrap().insert(uri, state);
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        if let Some(change) = params.content_changes.into_iter().last() {
            let state = DocumentState::new_with_uri(change.text, Some(uri.clone()));
            self.publish_diagnostics(&uri, &state).await;
            self.documents.lock().unwrap().insert(uri, state);
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents
            .lock()
            .unwrap()
            .remove(&params.text_document.uri);
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let symbols = self.with_document(&params.text_document.uri, |state| {
            let vx_symbols = vx::document_symbols(&state.parsed.tree());
            convert_symbols(&vx_symbols, state)
        });
        Ok(symbols.map(DocumentSymbolResponse::Nested))
    }

    async fn folding_range(&self, params: FoldingRangeParams) -> Result<Option<Vec<FoldingRange>>> {
        let ranges = self.with_document(&params.text_document.uri, |state| {
            let vx_ranges = vx::folding_ranges(state.parsed.tree().syntax());
            vx_ranges
                .into_iter()
                .map(|r| {
                    let start = state.line_index.position(r.range.start());
                    let end = state.line_index.position(r.range.end());
                    FoldingRange {
                        start_line: start.line,
                        start_character: Some(start.col),
                        end_line: end.line,
                        end_character: Some(end.col),
                        kind: Some(match r.kind {
                            vx::FoldingRangeKind::Block => FoldingRangeKind::Region,
                            vx::FoldingRangeKind::Comment => FoldingRangeKind::Comment,
                        }),
                        collapsed_text: None,
                    }
                })
                .collect()
        });
        Ok(ranges)
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let data = self.with_document(&params.text_document.uri, |state| {
            let tokens = vx::semantic_tokens(state.parsed.tree().syntax());
            // Collect ranges of builtin references for modifier tagging.
            let mut builtin_ranges: std::collections::HashSet<rowan::TextRange> =
                std::collections::HashSet::new();
            for expr in &state.arena.exprs {
                let ExprKind::NameRef(nr) = &expr.kind else {
                    continue;
                };
                let Some(target) = nr.resolved else {
                    continue;
                };
                let Some(def) = state.definition_from_target(target) else {
                    continue;
                };
                if def.source() != state.current_source && expr.span.source == state.current_source
                {
                    builtin_ranges.insert(expr.span.range);
                }
            }
            for (span, def) in &state.type_check.member_defs {
                if def.source() != state.current_source && span.source == state.current_source {
                    builtin_ranges.insert(span.range);
                }
            }
            encode_semantic_tokens(&tokens, &state.line_index, &builtin_ranges)
        });
        Ok(data.map(|data| {
            SemanticTokensResult::Tokens(SemanticTokens {
                result_id: None,
                data,
            })
        }))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let result = self.with_document(uri, |state| {
            let offset = state.line_index.offset(vx::Position {
                line: pos.line,
                col: pos.character,
            })?;

            if let Some(def) = state.definition_at(offset) {
                let range = state.to_lsp_range_span(def.name_span);
                return Some(GotoDefinitionResponse::Scalar(Location {
                    uri: state
                        .uri_for_source(def.source())
                        .unwrap_or_else(|| uri.clone()),
                    range,
                }));
            }

            // Try field access: jump to struct field definition
            let root = state.parsed.tree().syntax().clone();
            let token = root.token_at_offset(offset).left_biased()?;
            if let Some(field_node) = token.parent().and_then(vx::FieldExpr::cast)
                && let Some(field_name_tok) = field_node.field_name()
                && field_name_tok.text_range().contains_inclusive(offset)
            {
                let field_name = field_name_tok.text();
                if let Some(receiver_expr) = field_node.receiver() {
                    let receiver_ty =
                        state.expr_ty_at(state.span_unwrapping_parens(&receiver_expr));
                    if let Some(vx_types::Ty::Struct(struct_ty)) = receiver_ty
                        && let vx_types::TypeDef::Struct(sdef) = &state.types.env[struct_ty.id]
                        && let Some(fdef) = sdef.fields.iter().find(|f| f.name == field_name)
                    {
                        return Some(GotoDefinitionResponse::Scalar(Location {
                            uri: uri.clone(),
                            range: state.to_lsp_range_span(fdef.name_span),
                        }));
                    }
                }
            }

            None
        });

        Ok(result.flatten())
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let include_declaration = params.context.include_declaration;

        let result = self.with_document(uri, |state| {
            let offset = state.line_index.offset(vx::Position {
                line: pos.line,
                col: pos.character,
            })?;

            let def = state.definition_at(offset)?;
            let mut locations: Vec<Location> = state
                .references_to(&def)
                .into_iter()
                .map(|span| Location {
                    uri: uri.clone(),
                    range: state.to_lsp_range_span(span),
                })
                .collect();

            if include_declaration {
                locations.push(Location {
                    uri: state
                        .uri_for_source(def.source())
                        .unwrap_or_else(|| uri.clone()),
                    range: state.to_lsp_range_span(def.name_span),
                });
            }

            Some(locations)
        });

        Ok(result.flatten())
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        let uri = &params.text_document.uri;
        let pos = params.position;

        let result = self.with_document(uri, |state| {
            let offset = state.line_index.offset(vx::Position {
                line: pos.line,
                col: pos.character,
            })?;

            let def = state.definition_at(offset)?;
            if def.source() != state.current_source {
                return None;
            }
            Some(PrepareRenameResponse::RangeWithPlaceholder {
                range: state.to_lsp_range_span(def.name_span),
                placeholder: def.name.clone(),
            })
        });

        Ok(result.flatten())
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let new_name = params.new_name;

        let result = self.with_document(uri, |state| {
            let offset = state.line_index.offset(vx::Position {
                line: pos.line,
                col: pos.character,
            })?;

            let def = state.definition_at(offset)?;
            if def.source() != state.current_source {
                return None;
            }

            // Collect all edits: the definition site + all references
            let mut edits: Vec<TextEdit> = state
                .references_to(&def)
                .into_iter()
                .map(|span| TextEdit {
                    range: state.to_lsp_range_span(span),
                    new_text: new_name.clone(),
                })
                .collect();

            // Also rename the definition itself
            edits.push(TextEdit {
                range: state.to_lsp_range_span(def.name_span),
                new_text: new_name.clone(),
            });

            let mut changes = HashMap::new();
            changes.insert(uri.clone(), edits);

            Some(WorkspaceEdit {
                changes: Some(changes),
                ..Default::default()
            })
        });

        Ok(result.flatten())
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let result = self.with_document(uri, |state| {
            let offset = state.line_index.offset(vx::Position {
                line: pos.line,
                col: pos.character,
            })?;

            // First, try definition-based hover (existing behavior)
            if let Some((def, hover_range)) = state.definition_with_origin_at(offset) {
                let kind_label = def_kind_label(def.kind);
                let snippet = state.declaration_snippet(&def);
                let doc = state.documentation_for(&def);

                let mut markdown = format!("```vx\n{snippet}\n```\n---\n*{kind_label}*");

                // Show type signature for functions
                if let Some(sig) = state.types.fn_sig_for_def(&def) {
                    markdown.push_str(&format!(
                        "\n\nReturns `{}`",
                        sig.ret.display_with_env(&state.types.env)
                    ));
                }

                // Show inferred type for parameters and bindings from the type checker
                if matches!(def.kind, DefKind::Parameter | DefKind::Binding)
                    && let Some(ty) = state.inferred_def_type(&def)
                {
                    let display = ty.display_with_env(&state.types.env);
                    markdown.push_str(&format!("\n\nType: `{display}`"));
                }

                // Show fields for structs
                if let Some(type_id) = state.types.type_id_for_def(&def) {
                    match &state.types.env[type_id] {
                        vx_types::TypeDef::Struct(s) if !s.fields.is_empty() => {
                            markdown.push_str("\n\n**Fields:**\n");
                            for f in &s.fields {
                                markdown.push_str(&format!(
                                    "- `{}`: `{}`\n",
                                    f.name,
                                    f.ty.display_with_env(&state.types.env)
                                ));
                            }
                        }
                        vx_types::TypeDef::Enum(e) if !e.variants.is_empty() => {
                            markdown.push_str("\n\n**Variants:**\n");
                            for &vid in &e.variants {
                                let vname = state.types.env[vid].name();
                                markdown.push_str(&format!("- `{vname}`\n"));
                            }
                        }
                        _ => {}
                    }
                }

                if let Some(doc) = doc {
                    markdown.push_str("\n\n");
                    markdown.push_str(&doc);
                }

                return Some(Hover {
                    contents: HoverContents::Markup(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: markdown,
                    }),
                    range: Some(state.to_lsp_range_span(hover_range)),
                });
            }

            // Walk up from the token to find the smallest expression with a type
            let root = state.parsed.tree().syntax().clone();
            let token = root.token_at_offset(offset).left_biased()?;
            let mut node = token.parent();
            while let Some(n) = node {
                if let Some(expr) = vx::Expr::cast(n.clone())
                    && let Some(ty) = state.expr_ty_at(state.span_unwrapping_parens(&expr))
                {
                    let display = ty.display_with_env(&state.types.env);
                    let markdown = format!("```vx\n{display}\n```");
                    return Some(Hover {
                        contents: HoverContents::Markup(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: markdown,
                        }),
                        range: Some(state.to_lsp_range(trimmed_range(expr.syntax()))),
                    });
                }
                node = n.parent();
            }

            None
        });

        Ok(result.flatten())
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;

        let result = self.with_document(uri, |state| {
            let offset = state.line_index.offset(vx::Position {
                line: pos.line,
                col: pos.character,
            })?;

            let root = state.parsed.tree().syntax().clone();

            // Check if we're after a `.` — provide field/variant completions
            if let Some(items) = dot_completions(&root, offset, state) {
                return Some(CompletionResponse::Array(items));
            }

            // Check if we're inside a struct literal — suggest remaining fields
            if let Some(items) = struct_literal_completions(&root, offset, state) {
                return Some(CompletionResponse::Array(items));
            }

            let mut items: Vec<CompletionItem> = state
                .visible_at(offset)
                .into_iter()
                .map(|def| {
                    let documentation = state.documentation_for(&def).map(|doc| {
                        Documentation::MarkupContent(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: doc,
                        })
                    });
                    CompletionItem {
                        label: def.name.clone(),
                        kind: Some(def_kind_to_completion(def.kind)),
                        detail: Some(state.declaration_snippet(&def)),
                        documentation,
                        ..Default::default()
                    }
                })
                .collect();

            // Add keyword snippets
            items.extend(keyword_completions());

            Some(CompletionResponse::Array(items))
        });

        Ok(result.flatten())
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let result = self.with_document(&params.text_document.uri, |state| {
            let formatted = vx::format(state.parsed.tree().syntax());
            let last_line = state.text.lines().count() as u32;
            let last_col = state.text.lines().last().map_or(0, |l| l.len() as u32);
            vec![TextEdit {
                range: Range {
                    start: Position::new(0, 0),
                    end: Position::new(last_line, last_col),
                },
                new_text: formatted,
            }]
        });
        Ok(result)
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let uri = &params.text_document.uri;

        let result = self.with_document(uri, collect_inlay_hints);

        Ok(result)
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let result = self.with_document(uri, |state| {
            let offset = state.line_index.offset(vx::Position {
                line: pos.line,
                col: pos.character,
            })?;

            let root = state.parsed.tree().syntax().clone();
            let token = root.token_at_offset(offset).left_biased()?;

            let (call_expr, where_expr) = enclosing_call_context(&token)?;
            let (callee_expr, positional_args) = match &call_expr {
                vx::Expr::Call(call) => (call.callee()?, call.positional_args()),
                expr => (expr.clone(), Vec::new()),
            };

            // Build parameter info — either from user FnDecl or builtin sig
            let mut params = Vec::new();
            let mut label_parts = Vec::new();
            let mut doc: Option<String> = None;

            // Try method call: receiver.method(args)
            if let vx::Expr::Field(ref field_expr) = callee_expr {
                let method_name_tok = field_expr.field_name()?;
                let member_def = state
                    .type_check
                    .member_defs
                    .get(&state.current_span(method_name_tok.text_range()));
                if let Some(def) = member_def
                    && let Some(fn_node) = state.fn_decl_for(def)
                {
                    let fn_name = fn_node
                        .name()
                        .map(|t| t.text().to_string())
                        .unwrap_or_default();
                    label_parts.push(format!("fn {fn_name}("));

                    let positional: Vec<_> = fn_node.parameters().collect();
                    let keyword: Vec<_> = fn_node.keyword_parameters().collect();
                    let first_positional_is_self = positional
                        .first()
                        .and_then(|param| param.name())
                        .is_some_and(|name| name.text() == "self");

                    for (i, param) in positional
                        .iter()
                        .enumerate()
                        .skip(usize::from(first_positional_is_self))
                    {
                        if i > usize::from(first_positional_is_self) {
                            label_parts.push(", ".to_string());
                        }
                        let name = param
                            .name()
                            .map(|t| t.text().to_string())
                            .unwrap_or("_".to_string());
                        let type_str = param
                            .slot_type()
                            .map(|st| st.syntax().text().to_string())
                            .unwrap_or_default();
                        let param_label = format!("{name}{type_str}");
                        params.push(ParameterInformation {
                            label: ParameterLabel::Simple(param_label.clone()),
                            documentation: None,
                        });
                        label_parts.push(param_label);
                    }

                    if !keyword.is_empty() {
                        if positional.len() > usize::from(first_positional_is_self) {
                            label_parts.push("; ".to_string());
                        }
                        for (i, kw) in keyword.iter().enumerate() {
                            if i > 0 {
                                label_parts.push(", ".to_string());
                            }
                            let name = kw
                                .name()
                                .map(|t| t.text().to_string())
                                .unwrap_or("_".to_string());
                            let type_str = kw
                                .slot_type()
                                .map(|st| st.syntax().text().to_string())
                                .unwrap_or_default();
                            let param_label = format!("{name}{type_str}");
                            params.push(ParameterInformation {
                                label: ParameterLabel::Simple(param_label.clone()),
                                documentation: None,
                            });
                            label_parts.push(param_label);
                        }
                    }

                    label_parts.push(")".to_string());
                    if let Some(ret) = fn_node.return_type() {
                        label_parts.push(format!(" {}", ret.syntax().text()));
                    }
                    doc = state.documentation_for(def);
                }
            }

            // Try simple name call: name(args) or builtin(args)
            if params.is_empty() {
                let callee_token = match callee_expr {
                    vx::Expr::NameRef(ref nr) => nr.token()?,
                    _ if !label_parts.is_empty() => {
                        // Already handled above (method call)
                        return None;
                    }
                    _ => return None,
                };

                let def = state
                    .resolved_target_for_token(&callee_token)
                    .and_then(|target| state.definition_from_target(target));

                if let Some(def) = def
                    && def.kind == DefKind::Function
                {
                    if let Some(fn_node) = state.fn_decl_for(&def) {
                        let fn_name = fn_node
                            .name()
                            .map(|t| t.text().to_string())
                            .unwrap_or_default();
                        label_parts.push(format!("fn {fn_name}("));

                        let positional: Vec<_> = fn_node.parameters().collect();
                        let keyword: Vec<_> = fn_node.keyword_parameters().collect();

                        for (i, param) in positional.iter().enumerate() {
                            if i > 0 {
                                label_parts.push(", ".to_string());
                            }
                            let name = param
                                .name()
                                .map(|t| t.text().to_string())
                                .unwrap_or("_".to_string());
                            let type_str = param
                                .slot_type()
                                .map(|st| st.syntax().text().to_string())
                                .unwrap_or_default();
                            let param_label = format!("{name}{type_str}");
                            params.push(ParameterInformation {
                                label: ParameterLabel::Simple(param_label.clone()),
                                documentation: None,
                            });
                            label_parts.push(param_label);
                        }

                        if !keyword.is_empty() {
                            if !positional.is_empty() {
                                label_parts.push("; ".to_string());
                            }
                            for (i, kw) in keyword.iter().enumerate() {
                                if i > 0 {
                                    label_parts.push(", ".to_string());
                                }
                                let name = kw
                                    .name()
                                    .map(|t| t.text().to_string())
                                    .unwrap_or("_".to_string());
                                let type_str = kw
                                    .slot_type()
                                    .map(|st| st.syntax().text().to_string())
                                    .unwrap_or_default();
                                let param_label = format!("{name}{type_str}");
                                params.push(ParameterInformation {
                                    label: ParameterLabel::Simple(param_label.clone()),
                                    documentation: None,
                                });
                                label_parts.push(param_label);
                            }
                        }

                        label_parts.push(")".to_string());

                        if let Some(ret) = fn_node.return_type() {
                            let ret_text = ret.syntax().text().to_string();
                            label_parts.push(format!(" {ret_text}"));
                        }

                        doc = state.documentation_for(&def);
                    } else {
                        return None;
                    }
                } else {
                    return None;
                }
            } // end if params.is_empty()

            let label = label_parts.concat();

            // Clamp active_param if past keyword args, stay in bounds
            let active_param = if params.is_empty() {
                None
            } else {
                active_call_param(&positional_args, where_expr.as_ref(), offset, &params)
            };

            Some(SignatureHelp {
                signatures: vec![SignatureInformation {
                    label,
                    documentation: doc.map(|d| {
                        Documentation::MarkupContent(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: d,
                        })
                    }),
                    parameters: Some(params),
                    active_parameter: active_param,
                }],
                active_signature: Some(0),
                active_parameter: active_param,
            })
        });

        Ok(result.flatten())
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = &params.text_document.uri;

        let result = self.with_document(uri, |state| {
            let mut actions: Vec<CodeActionOrCommand> = Vec::new();

            for diag in &params.context.diagnostics {
                // "prefix with _" quickfix for unused bindings/parameters
                if diag.source.as_deref() == Some("vx-lint") && diag.message.starts_with("unused ")
                {
                    // Extract the name from the message: `unused binding: \`name\``
                    if let Some(name) = diag.message.split('`').nth(1) {
                        let new_name = format!("_{name}");
                        let edit = TextEdit {
                            range: diag.range,
                            new_text: new_name.clone(),
                        };
                        let mut changes = std::collections::HashMap::new();
                        changes.insert(uri.clone(), vec![edit]);
                        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                            title: format!("Rename to `{new_name}`"),
                            kind: Some(CodeActionKind::QUICKFIX),
                            diagnostics: Some(vec![diag.clone()]),
                            edit: Some(WorkspaceEdit {
                                changes: Some(changes),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }));
                    }
                }

                // "did you mean?" quickfix for unresolved names
                if diag.source.as_deref() == Some("vx-ir")
                    && diag.message.starts_with("unresolved name")
                    && let Some(name) = diag.message.split('`').nth(1)
                {
                    let offset = state.line_index.offset(vx::Position {
                        line: diag.range.start.line,
                        col: diag.range.start.character,
                    });
                    if let Some(offset) = offset {
                        let visible = state.visible_at(offset);
                        if let Some(suggestion) = closest_name(name, &visible) {
                            let edit = TextEdit {
                                range: diag.range,
                                new_text: suggestion.clone(),
                            };
                            let mut changes = std::collections::HashMap::new();
                            changes.insert(uri.clone(), vec![edit]);
                            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                                title: format!("Did you mean `{suggestion}`?"),
                                kind: Some(CodeActionKind::QUICKFIX),
                                diagnostics: Some(vec![diag.clone()]),
                                edit: Some(WorkspaceEdit {
                                    changes: Some(changes),
                                    ..Default::default()
                                }),
                                ..Default::default()
                            }));
                        }
                    }
                }
            }

            if actions.is_empty() {
                None
            } else {
                Some(actions)
            }
        });

        Ok(result.flatten())
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let result = self.with_document(uri, |state| {
            let offset = state.line_index.offset(vx::Position {
                line: pos.line,
                col: pos.character,
            })?;

            let def = state.definition_at(offset)?;

            let mut highlights = Vec::new();
            if def.source() == state.current_source {
                highlights.push(DocumentHighlight {
                    range: state.to_lsp_range_span(def.name_span),
                    kind: Some(DocumentHighlightKind::WRITE),
                });
            }

            for ref_span in state.references_to(&def) {
                highlights.push(DocumentHighlight {
                    range: state.to_lsp_range_span(ref_span),
                    kind: Some(DocumentHighlightKind::READ),
                });
            }

            Some(highlights)
        });

        Ok(result.flatten())
    }

    async fn selection_range(
        &self,
        params: SelectionRangeParams,
    ) -> Result<Option<Vec<SelectionRange>>> {
        let uri = &params.text_document.uri;

        let result = self.with_document(uri, |state| {
            params
                .positions
                .iter()
                .map(|pos| {
                    let offset = state.line_index.offset(vx::Position {
                        line: pos.line,
                        col: pos.character,
                    });
                    build_selection_range(state, offset)
                })
                .collect::<Vec<_>>()
        });

        Ok(result)
    }

    async fn prepare_call_hierarchy(
        &self,
        params: CallHierarchyPrepareParams,
    ) -> Result<Option<Vec<CallHierarchyItem>>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let result = self.with_document(uri, |state| {
            let offset = state.line_index.offset(vx::Position {
                line: pos.line,
                col: pos.character,
            })?;

            let def = state.definition_at(offset)?;
            if def.kind != DefKind::Function {
                return None;
            }

            Some(vec![make_call_hierarchy_item(uri, &def, state)])
        });

        Ok(result.flatten())
    }

    async fn incoming_calls(
        &self,
        params: CallHierarchyIncomingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyIncomingCall>>> {
        let uri = &params.item.uri;

        let result = self.with_document(uri, |state| {
            let root = state.parsed.tree().syntax().clone();

            // Find the definition that matches this item's selection range
            let item_start = state.line_index.offset(vx::Position {
                line: params.item.selection_range.start.line,
                col: params.item.selection_range.start.character,
            })?;
            let target_def = state.definition_at(item_start)?;

            // Find all call sites that reference this function. Walk arena.exprs
            // for NameRef expressions that resolve to target_def and whose token
            // is the root callee of an application chain.
            let mut call_spans: Vec<SourceSpan> = Vec::new();
            for expr in &state.arena.exprs {
                let ExprKind::NameRef(nr) = &expr.kind else {
                    continue;
                };
                let Some(target) = nr.resolved else {
                    continue;
                };
                let Some(def) = state.definition_from_target(target) else {
                    continue;
                };
                if def.id != target_def.id || expr.span.source != state.current_source {
                    continue;
                }
                let token = root.token_at_offset(expr.span.range.start()).right_biased();
                if !token.as_ref().is_some_and(is_root_callee_ref) {
                    continue;
                }
                call_spans.push(expr.span);
            }

            // Group by enclosing function
            let mut by_caller: HashMap<DefId, (Definition, Vec<TextRange>)> = HashMap::new();
            for span in &call_spans {
                let token = root.token_at_offset(span.range.start()).right_biased()?;
                // Walk up to find the enclosing FN_DECL
                let fn_node = token.parent_ancestors().find_map(FnDecl::cast)?;
                let caller_name_token = fn_node.name()?;
                let caller_name_range = caller_name_token.text_range();
                let caller_name_span = state.current_span(caller_name_range);

                // Find the definition for this enclosing function.
                let caller_def = state.definition_at_name_span(caller_name_span)?;

                by_caller
                    .entry(caller_def.id)
                    .or_insert_with(|| (caller_def.clone(), Vec::new()))
                    .1
                    .push(span.range);
            }

            let calls: Vec<_> = by_caller
                .into_values()
                .map(|(caller_def, ranges)| CallHierarchyIncomingCall {
                    from: make_call_hierarchy_item(uri, &caller_def, state),
                    from_ranges: ranges.into_iter().map(|r| state.to_lsp_range(r)).collect(),
                })
                .collect();

            Some(calls)
        });

        Ok(result.flatten())
    }

    async fn outgoing_calls(
        &self,
        params: CallHierarchyOutgoingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyOutgoingCall>>> {
        let uri = &params.item.uri;

        let result = self.with_document(uri, |state| {
            let root = state.parsed.tree().syntax().clone();

            // Find the FN_DECL for this item
            let item_start = state.line_index.offset(vx::Position {
                line: params.item.selection_range.start.line,
                col: params.item.selection_range.start.character,
            })?;
            let token = root.token_at_offset(item_start).right_biased()?;
            let fn_node = token.parent_ancestors().find_map(FnDecl::cast)?;
            let fn_range = fn_node.syntax().text_range();

            // Find all CALL_EXPR nodes inside this function
            let mut by_target: HashMap<vx_ir::DefSite, (Definition, Vec<TextRange>)> =
                HashMap::new();

            for node in fn_node.syntax().descendants() {
                if node.kind() != SyntaxKind::CALL_EXPR {
                    continue;
                }
                let call = vx::CallExpr::cast(node)?;
                if call
                    .syntax()
                    .parent()
                    .is_some_and(|parent| parent.kind() == SyntaxKind::CALL_EXPR)
                {
                    continue;
                }
                let callee = match call.callee() {
                    Some(vx::Expr::NameRef(nr)) => nr,
                    _ => continue,
                };
                let callee_token = callee.token()?;
                let callee_range = callee_token.text_range();

                // Must be inside our function
                if !fn_range.contains_range(callee_range) {
                    continue;
                }

                // Resolve the callee via the parent NAME_REF_EXPR span.
                let Some(target) = state.resolved_target_for_token(&callee_token) else {
                    continue;
                };
                let Some(def) = state.definition_from_target(target) else {
                    continue;
                };
                if def.kind != DefKind::Function {
                    continue;
                }

                by_target
                    .entry(def.site())
                    .or_insert_with(|| (def.clone(), Vec::new()))
                    .1
                    .push(callee_range);
            }

            let calls: Vec<_> = by_target
                .into_values()
                .map(|(target_def, ranges)| CallHierarchyOutgoingCall {
                    to: make_call_hierarchy_item(uri, &target_def, state),
                    from_ranges: ranges.into_iter().map(|r| state.to_lsp_range(r)).collect(),
                })
                .collect();

            Some(calls)
        });

        Ok(result.flatten())
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        let query = params.query.to_lowercase();
        let docs = self.documents.lock().unwrap();

        let mut results = Vec::new();
        for (uri, state) in docs.iter() {
            for def in state.all_definitions_in_current() {
                // Skip local bindings/parameters — only top-level-ish symbols
                if matches!(def.kind, DefKind::Binding | DefKind::Parameter) {
                    continue;
                }

                if query.is_empty() || def.name.to_lowercase().contains(&query) {
                    #[allow(deprecated)]
                    results.push(SymbolInformation {
                        name: def.name.clone(),
                        kind: def_kind_to_symbol(def.kind),
                        tags: None,
                        deprecated: None,
                        location: Location {
                            uri: state
                                .uri_for_source(def.source())
                                .unwrap_or_else(|| uri.clone()),
                            range: state.to_lsp_range_span(def.name_span),
                        },
                        container_name: None,
                    });
                }
            }
        }

        Ok(Some(results))
    }

    async fn linked_editing_range(
        &self,
        _params: LinkedEditingRangeParams,
    ) -> Result<Option<LinkedEditingRanges>> {
        Ok(None)
    }
}

impl VxLanguageServer {
    async fn publish_diagnostics(&self, uri: &Url, state: &DocumentState) {
        let mut diagnostics: Vec<Diagnostic> = state
            .parsed
            .errors()
            .iter()
            .map(|err| Diagnostic {
                range: state.to_lsp_range(err.range),
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("vx-parser".to_string()),
                message: err.message.clone(),
                ..Default::default()
            })
            .collect();

        // Unresolved/invalid name diagnostics from VIR resolve.
        for err in &state.resolve_errors {
            let primary = err.primary_span();
            // Only surface errors that originate in the user's source file.
            if primary.source != state.current_source {
                continue;
            }
            let (message, source_tag) = format_ir_diagnostic(state, err);
            diagnostics.push(Diagnostic {
                range: state.to_lsp_range_span(primary),
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some(source_tag.to_string()),
                message,
                ..Default::default()
            });
        }

        // Capability checking diagnostics
        for err in &state.cap_check.errors {
            let primary = err.primary_span();
            if primary.source != state.current_source {
                continue;
            }
            let (message, source_tag) = format_ir_diagnostic(state, err);
            diagnostics.push(Diagnostic {
                range: state.to_lsp_range_span(primary),
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some(source_tag.to_string()),
                message,
                ..Default::default()
            });
        }

        // Unused binding/parameter warnings
        {
            use std::collections::HashSet;
            // Collect all definition sites that have at least one reference.
            let mut referenced: HashSet<vx_ir::DefSite> = HashSet::new();
            for expr in &state.arena.exprs {
                if let ExprKind::NameRef(nr) = &expr.kind
                    && let Some(target) = nr.resolved
                    && let Some(def) = state.definition_from_target(target)
                {
                    referenced.insert(def.site());
                }
            }
            for def in &state
                .type_check
                .member_defs
                .values()
                .cloned()
                .collect::<Vec<_>>()
            {
                referenced.insert(def.site());
            }

            for def in state.all_definitions_in_current() {
                if !matches!(def.kind, DefKind::Binding | DefKind::Parameter) {
                    continue;
                }
                // Skip names starting with _ (conventional unused marker)
                if def.name.starts_with('_') {
                    continue;
                }
                if !referenced.contains(&def.site()) {
                    diagnostics.push(Diagnostic {
                        range: state.to_lsp_range_span(def.name_span),
                        severity: Some(DiagnosticSeverity::HINT),
                        source: Some("vx-lint".to_string()),
                        message: format!(
                            "unused {}: `{}`",
                            if def.kind == DefKind::Binding {
                                "binding"
                            } else {
                                "parameter"
                            },
                            def.name
                        ),
                        tags: Some(vec![DiagnosticTag::UNNECESSARY]),
                        ..Default::default()
                    });
                }
            }
        }

        // Type checking diagnostics
        for err in &state.type_check.errors {
            let primary = err.primary_span();
            if primary.source != state.current_source {
                continue;
            }
            let (message, source_tag) = format_ir_diagnostic(state, err);
            diagnostics.push(Diagnostic {
                range: state.to_lsp_range_span(primary),
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some(source_tag.to_string()),
                message,
                ..Default::default()
            });
        }

        self.client
            .publish_diagnostics(uri.clone(), diagnostics, None)
            .await;
    }
}

/// Render an [`IrDiagnostic`] to an LSP-protocol message string and a
/// `source` tag (the pass that produced it). The message is the
/// last-mile, human-facing string — the diagnostic itself stays
/// structural.
fn format_ir_diagnostic(
    state: &DocumentState,
    diag: &vx_ir::IrDiagnostic,
) -> (String, &'static str) {
    use vx_ir::IrDiagnostic;

    let env = &state.types.env;
    match diag {
        IrDiagnostic::Unresolved { name } => (
            format!("unresolved name `{}`", state.text_at(*name)),
            "vx-ir",
        ),
        IrDiagnostic::InvalidTarget { name, .. } => (
            format!(
                "name `{}` resolves to an unexpected kind",
                state.text_at(*name)
            ),
            "vx-ir",
        ),
        IrDiagnostic::ArgTypeMismatch {
            param,
            expected,
            found,
            ..
        } => (
            format!(
                "type mismatch for `{}`: expected `{}`, found `{}`",
                state.text_at(*param),
                expected.display_with_env(env),
                found.display_with_env(env),
            ),
            "vx-type",
        ),
        IrDiagnostic::BinaryOpMismatch {
            op, lhs_ty, rhs_ty, ..
        } => (
            format!(
                "cannot apply `{}` to `{}` and `{}`",
                state.text_at(*op),
                lhs_ty.display_with_env(env),
                rhs_ty.display_with_env(env),
            ),
            "vx-type",
        ),
        IrDiagnostic::NoSuchMethod {
            method_call,
            receiver_ty,
            candidates,
            ..
        } => {
            let mut msg = format!(
                "no method `{}` on type `{}`",
                state.text_at(*method_call),
                receiver_ty.display_with_env(env),
            );
            if !candidates.is_empty() {
                let suffix: Vec<String> = candidates
                    .iter()
                    .map(|c| state.text_at(c.name_span).to_string())
                    .collect();
                msg.push_str(" — did you mean ");
                for (i, name) in suffix.iter().enumerate() {
                    if i > 0 {
                        msg.push_str(", ");
                    }
                    msg.push('`');
                    msg.push_str(name);
                    msg.push('`');
                }
                msg.push('?');
            }
            (msg, "vx-type")
        }
        IrDiagnostic::UnusedExprStatement { .. } => {
            ("unused expression statement".to_string(), "vx-type")
        }
        IrDiagnostic::CapabilityViolation {
            slot,
            allowed,
            found,
            ..
        } => {
            let excess = found.satisfies(*allowed);
            (
                format!(
                    "capability violation in `{}`: requires {} but slot only allows {}",
                    state.text_at(*slot),
                    excess,
                    allowed,
                ),
                "vx-cap",
            )
        }
        IrDiagnostic::UnknownCapability { name } => (
            format!("unknown capability `#{}`", state.text_at(*name)),
            "vx-cap",
        ),
    }
}

fn convert_symbols(symbols: &[vx::DocumentSymbol], state: &DocumentState) -> Vec<DocumentSymbol> {
    symbols
        .iter()
        .map(|sym| DocumentSymbol {
            name: sym.name.clone(),
            detail: None,
            kind: convert_symbol_kind(sym.kind),
            tags: None,
            #[allow(deprecated)]
            deprecated: None,
            range: state.to_lsp_range(sym.range),
            selection_range: state.to_lsp_range(sym.selection_range),
            children: Some(convert_symbols(&sym.children, state)),
        })
        .collect()
}

fn convert_symbol_kind(kind: vx::SymbolKind) -> SymbolKind {
    match kind {
        vx::SymbolKind::Function => SymbolKind::FUNCTION,
        vx::SymbolKind::Struct => SymbolKind::STRUCT,
        vx::SymbolKind::Enum => SymbolKind::ENUM,
        vx::SymbolKind::Variant => SymbolKind::ENUM_MEMBER,
        vx::SymbolKind::Field => SymbolKind::FIELD,
        vx::SymbolKind::Newtype => SymbolKind::TYPE_PARAMETER,
        vx::SymbolKind::TypeAlias => SymbolKind::TYPE_PARAMETER,
    }
}

fn def_kind_label(kind: DefKind) -> &'static str {
    match kind {
        DefKind::Function => "function",
        DefKind::Namespace => "namespace",
        DefKind::Struct => "struct",
        DefKind::Enum => "enum",
        DefKind::Newtype => "newtype",
        DefKind::TypeAlias => "type alias",
        DefKind::Binding => "binding",
        DefKind::Parameter => "parameter",
        DefKind::Variant => "variant",
        DefKind::Field => "field",
    }
}

fn def_kind_to_completion(kind: DefKind) -> CompletionItemKind {
    match kind {
        DefKind::Function => CompletionItemKind::FUNCTION,
        DefKind::Namespace => CompletionItemKind::MODULE,
        DefKind::Struct => CompletionItemKind::STRUCT,
        DefKind::Enum => CompletionItemKind::ENUM,
        DefKind::Newtype => CompletionItemKind::CLASS,
        DefKind::TypeAlias => CompletionItemKind::TYPE_PARAMETER,
        DefKind::Binding => CompletionItemKind::VARIABLE,
        DefKind::Parameter => CompletionItemKind::VARIABLE,
        DefKind::Variant => CompletionItemKind::ENUM_MEMBER,
        DefKind::Field => CompletionItemKind::FIELD,
    }
}

/// Build a nested selection range chain by walking up the syntax tree from a token.
fn build_selection_range(state: &DocumentState, offset: Option<rowan::TextSize>) -> SelectionRange {
    let Some(offset) = offset else {
        return SelectionRange {
            range: Range::default(),
            parent: None,
        };
    };

    // Find the token at offset, then collect ancestor node ranges
    let root = state.parsed.tree().syntax().clone();
    let token = root.token_at_offset(offset).right_biased();

    let mut ranges: Vec<Range> = Vec::new();

    if let Some(token) = token {
        // Start with the token itself
        ranges.push(state.to_lsp_range(token.text_range()));

        // Walk up through parent nodes
        let mut node = token.parent();
        while let Some(n) = node {
            let range = state.to_lsp_range(n.text_range());
            // Only add if it's strictly larger than the previous
            if ranges.last().is_none_or(|last| range != *last) {
                ranges.push(range);
            }
            node = n.parent();
        }
    }

    // Build the linked list from innermost to outermost
    let mut result = SelectionRange {
        range: ranges.pop().unwrap_or_default(),
        parent: None,
    };
    while let Some(range) = ranges.pop() {
        result = SelectionRange {
            range,
            parent: Some(Box::new(result)),
        };
    }

    result
}

/// Encode semantic tokens as LSP delta-encoded 5-tuples.
fn encode_semantic_tokens(
    tokens: &[vx::SemanticToken],
    line_index: &vx::LineIndex,
    builtin_ranges: &std::collections::HashSet<rowan::TextRange>,
) -> Vec<tower_lsp::lsp_types::SemanticToken> {
    let mut result = Vec::with_capacity(tokens.len());
    let mut prev_line = 0u32;
    let mut prev_col = 0u32;

    for token in tokens {
        let start = line_index.position(token.range.start());
        let len = u32::from(token.range.len());

        let delta_line = start.line - prev_line;
        let delta_start = if delta_line == 0 {
            start.col - prev_col
        } else {
            start.col
        };

        // Bit 0 = defaultLibrary modifier
        let modifiers = if builtin_ranges.contains(&token.range) {
            1
        } else {
            0
        };

        result.push(tower_lsp::lsp_types::SemanticToken {
            delta_line,
            delta_start,
            length: len,
            token_type: token.token_type as u32,
            token_modifiers_bitset: modifiers,
        });

        prev_line = start.line;
        prev_col = start.col;
    }

    result
}

fn make_call_hierarchy_item(
    uri: &Url,
    def: &Definition,
    state: &DocumentState,
) -> CallHierarchyItem {
    CallHierarchyItem {
        name: def.name.clone(),
        kind: def_kind_to_symbol(def.kind),
        tags: None,
        detail: None,
        uri: state
            .uri_for_source(def.source())
            .unwrap_or_else(|| uri.clone()),
        range: state.to_lsp_range_span(def.full_span),
        selection_range: state.to_lsp_range_span(def.name_span),
        data: None,
    }
}

fn def_kind_to_symbol(kind: DefKind) -> SymbolKind {
    match kind {
        DefKind::Function => SymbolKind::FUNCTION,
        DefKind::Namespace => SymbolKind::NAMESPACE,
        DefKind::Struct => SymbolKind::STRUCT,
        DefKind::Enum => SymbolKind::ENUM,
        DefKind::Newtype => SymbolKind::CLASS,
        DefKind::TypeAlias => SymbolKind::TYPE_PARAMETER,
        DefKind::Binding => SymbolKind::VARIABLE,
        DefKind::Parameter => SymbolKind::VARIABLE,
        DefKind::Variant => SymbolKind::ENUM_MEMBER,
        DefKind::Field => SymbolKind::FIELD,
    }
}

/// Provide field completions inside a struct literal (e.g., `Foo { | }`).
fn struct_literal_completions(
    root: &vx::SyntaxNode,
    offset: rowan::TextSize,
    state: &DocumentState,
) -> Option<Vec<CompletionItem>> {
    let token = root.token_at_offset(offset).left_biased()?;

    // Walk up to find enclosing STRUCT_LITERAL_EXPR
    let mut node = Some(token.parent()?);
    let mut struct_lit = None;
    while let Some(n) = node {
        if n.kind() == SyntaxKind::STRUCT_LITERAL_EXPR {
            struct_lit = vx::StructLiteralExpr::cast(n);
            break;
        }
        node = n.parent();
    }
    let struct_lit = struct_lit?;

    // Get the struct name
    let name_tok = struct_lit.name()?;
    let name = name_tok.text().to_string();

    // Resolve the struct type
    let type_ids = state.types.env.lookup(&name);
    let struct_def = type_ids.iter().find_map(|&id| match &state.types.env[id] {
        vx_types::TypeDef::Struct(s) => Some(s),
        _ => None,
    })?;

    // Collect already-present field names
    let present: std::collections::HashSet<String> = struct_lit
        .fields()
        .filter_map(|f| f.name().map(|t| t.text().to_string()))
        .collect();

    // Suggest remaining fields
    let items: Vec<CompletionItem> = struct_def
        .fields
        .iter()
        .filter(|f| !present.contains(&f.name))
        .map(|f| CompletionItem {
            label: f.name.clone(),
            kind: Some(CompletionItemKind::FIELD),
            detail: Some(format!("{}", f.ty.display_with_env(&state.types.env))),
            insert_text: Some(format!("{}: $0", f.name)),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            ..Default::default()
        })
        .collect();

    if items.is_empty() { None } else { Some(items) }
}

/// Provide field/variant completions when the cursor is after a `.`.
fn dot_completions(
    root: &vx::SyntaxNode,
    offset: rowan::TextSize,
    state: &DocumentState,
) -> Option<Vec<CompletionItem>> {
    // Find the token just before the cursor — should be DOT or an ident after DOT
    let token = root.token_at_offset(offset).left_biased()?;

    let dot_token = if token.kind() == SyntaxKind::DOT {
        token
    } else {
        let field = token.parent().and_then(vx::FieldExpr::cast)?;
        field
            .syntax()
            .children_with_tokens()
            .filter_map(|child| child.into_token())
            .find(|token| token.kind() == SyntaxKind::DOT)?
    };

    // The receiver is the expression before the dot in the parent node
    let parent = dot_token.parent()?;

    // The receiver could be:
    // 1. A name reference to an enum → suggest variants
    // 2. A name reference to a struct-typed binding → suggest fields
    let receiver_expr = parent.children().find_map(vx::Expr::cast)?;

    match &receiver_expr {
        vx::Expr::NameRef(nr) => {
            let name_token = nr.token()?;
            let name_range = name_token.text_range();

            // Try to resolve this name via the parent NAME_REF_EXPR span.
            let _ = name_range;
            let target = state.resolved_target_for_token(&name_token)?;
            let def = state.definition_from_target(target)?;

            // If it's an enum, offer variants
            if def.kind == DefKind::Enum {
                let type_id = state.types.type_id_for_def(&def)?;
                if let vx_types::TypeDef::Enum(e) = &state.types.env[type_id] {
                    let items: Vec<CompletionItem> = e
                        .variants
                        .iter()
                        .map(|&vid| {
                            let vdef = &state.types.env[vid];
                            let vname = vdef.name().to_string();
                            CompletionItem {
                                label: vname,
                                kind: Some(CompletionItemKind::ENUM_MEMBER),
                                ..Default::default()
                            }
                        })
                        .collect();
                    return Some(items);
                }
            }

            // If it's a binding or parameter, offer completions based on inferred type
            if matches!(def.kind, DefKind::Binding | DefKind::Parameter) {
                if let Some(ty) = state.inferred_def_type(&def)
                    && let Some(items) = completions_for_type(&ty, state, offset)
                {
                    return Some(items);
                }
                // Fall back to function signature matching
                if let Some(fields) = resolve_binding_fields(&def, state) {
                    return Some(fields);
                }
            }

            // If it's a function, offer completions on the return type
            if def.kind == DefKind::Function
                && let Some(sig) = state.types.fn_sig_for_def(&def)
                && let Some(items) = completions_for_type(&sig.ret, state, offset)
            {
                return Some(items);
            }

            None
        }
        _ => {
            // For any other expression, try the type checker's expr_types
            let receiver_span = state.span_unwrapping_parens(&receiver_expr);
            if let Some(ty) = state.expr_ty_at(receiver_span) {
                return completions_for_type(&ty, state, offset);
            }
            None
        }
    }
}

/// Try to determine the type of a binding and return its struct fields as completions.
fn resolve_binding_fields(def: &Definition, state: &DocumentState) -> Option<Vec<CompletionItem>> {
    // Check if this binding is a function parameter with a type annotation
    for sig in state.types.fn_sigs() {
        for param in &sig.positional_params {
            if param.name == def.name {
                // This is a rough match — in a real type checker you'd match by range.
                // For now, check if the param name matches and the Ty is a struct.
                return struct_fields_from_ty(&param.ty, state);
            }
        }
        for kw in &sig.keyword_params {
            if kw.name == def.name {
                return struct_fields_from_ty(&kw.ty, state);
            }
        }
    }
    None
}

/// Given a Ty, return completions for dot access — struct fields or namespace members.
pub(crate) fn completions_for_type(
    ty: &vx_types::Ty,
    state: &DocumentState,
    offset: rowan::TextSize,
) -> Option<Vec<CompletionItem>> {
    match ty {
        vx_types::Ty::Optional(inner) => completions_for_type(inner, state, offset),
        _ => struct_fields_from_ty(ty, state),
    }
    .or_else(|| {
        let items: Vec<CompletionItem> = visible_members_for_receiver(state, offset, ty)
            .into_iter()
            .filter(|def| def.kind == DefKind::Function)
            .map(|def| CompletionItem {
                label: def.name.clone(),
                kind: Some(CompletionItemKind::METHOD),
                detail: Some(state.declaration_snippet(&def)),
                documentation: state.documentation_for(&def).map(|doc| {
                    Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: doc,
                    })
                }),
                ..Default::default()
            })
            .collect();
        if items.is_empty() { None } else { Some(items) }
    })
}

/// Given a Ty, if it's a struct, return field completions.
fn struct_fields_from_ty(ty: &vx_types::Ty, state: &DocumentState) -> Option<Vec<CompletionItem>> {
    match ty {
        vx_types::Ty::Struct(s) => {
            if let vx_types::TypeDef::Struct(sdef) = &state.types.env[s.id] {
                let items: Vec<CompletionItem> = sdef
                    .fields
                    .iter()
                    .map(|f| CompletionItem {
                        label: f.name.clone(),
                        kind: Some(CompletionItemKind::FIELD),
                        detail: Some(f.ty.display_with_env(&state.types.env).to_string()),
                        ..Default::default()
                    })
                    .collect();
                if items.is_empty() { None } else { Some(items) }
            } else {
                None
            }
        }
        vx_types::Ty::Optional(inner) => struct_fields_from_ty(inner, state),
        _ => None,
    }
}

fn keyword_completions() -> Vec<CompletionItem> {
    let snippets = [
        (
            "fn",
            "fn ${1:name}(${2}) {\n\t$0\n}",
            "Function declaration",
        ),
        ("let", "let ${1:name} = ${0}", "Let binding"),
        (
            "match",
            "match ${1:expr} {\n\t${2:pattern} => ${0}\n}",
            "Match expression",
        ),
        ("if", "if ${1:condition} {\n\t$0\n}", "If expression"),
        (
            "struct",
            "struct ${1:Name} {\n\t${0}\n}",
            "Struct declaration",
        ),
        ("enum", "enum ${1:Name} {\n\t${0}\n}", "Enum declaration"),
        (
            "newtype",
            "newtype ${1:Name} of ${0:Type}",
            "Newtype declaration",
        ),
        ("type", "type ${1:Name} = ${0:Type}", "Type alias"),
        ("return", "return ${0}", "Return statement"),
    ];

    snippets
        .into_iter()
        .map(|(label, snippet, detail)| CompletionItem {
            label: label.to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some(detail.to_string()),
            insert_text: Some(snippet.to_string()),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            ..Default::default()
        })
        .collect()
}

/// Find the closest name to `target` among visible definitions (Levenshtein distance).
/// Returns None if no name is within distance 2.
fn closest_name(target: &str, visible: &[Definition]) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for def in visible {
        let dist = levenshtein(target, &def.name);
        if dist > 0 && dist <= 2 && best.as_ref().is_none_or(|(d, _)| dist < *d) {
            best = Some((dist, def.name.clone()));
        }
    }
    best.map(|(_, name)| name)
}

/// Simple Levenshtein distance.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (m, n) = (a.len(), b.len());
    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for (i, row) in dp.iter_mut().enumerate().take(m + 1) {
        row[0] = i;
    }
    for (j, cell) in dp[0].iter_mut().enumerate().take(n + 1) {
        *cell = j;
    }
    for i in 1..=m {
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            dp[i][j] = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + cost);
        }
    }
    dp[m][n]
}

fn enclosing_call_context(token: &vx::SyntaxToken) -> Option<(vx::Expr, Option<vx::WhereExpr>)> {
    for node in token.parent_ancestors() {
        if let Some(where_expr) = vx::WhereExpr::cast(node.clone())
            && let Some(callee) = where_expr.callee()
        {
            return Some((callee, Some(where_expr)));
        }
        if let Some(call) = vx::CallExpr::cast(node) {
            return Some((vx::Expr::Call(call), None));
        }
    }
    None
}

fn active_call_param(
    positional_args: &[vx::Expr],
    where_expr: Option<&vx::WhereExpr>,
    offset: rowan::TextSize,
    params: &[ParameterInformation],
) -> Option<u32> {
    for (index, arg) in positional_args.iter().enumerate() {
        if arg.syntax().text_range().contains_inclusive(offset) {
            return Some(index as u32);
        }
    }

    if let Some(where_expr) = where_expr
        && let Some(vx::Expr::Record(record)) = where_expr.keywords()
    {
        let positional_count = positional_args.len() as u32;
        let keyword_fields = record
            .fields()
            .filter_map(|field| {
                Some((
                    field.name()?.text().to_string(),
                    field.syntax().text_range(),
                ))
            })
            .collect::<Vec<_>>();
        for (index, (_, range)) in keyword_fields.iter().enumerate() {
            if range.contains_inclusive(offset) {
                return Some(positional_count + index as u32);
            }
        }
        if where_expr.syntax().text_range().contains_inclusive(offset) && !keyword_fields.is_empty()
        {
            return Some(positional_count);
        }
    }

    if positional_args.is_empty() || params.is_empty() {
        None
    } else {
        Some((positional_args.len() as u32 - 1).min(params.len() as u32 - 1))
    }
}

pub(crate) fn collect_inlay_hints(state: &DocumentState) -> Vec<InlayHint> {
    let root = state.parsed.tree().syntax().clone();
    let mut hints = Vec::new();

    // Walk all CALL_EXPR nodes in the document
    for node in root.descendants() {
        if node.kind() != SyntaxKind::CALL_EXPR {
            continue;
        }

        let call = match vx::CallExpr::cast(node) {
            Some(c) => c,
            None => continue,
        };
        if call
            .syntax()
            .parent()
            .is_some_and(|parent| parent.kind() == SyntaxKind::CALL_EXPR)
        {
            continue;
        }

        // Get callee — must be a simple name reference
        let callee = match call.callee() {
            Some(vx::Expr::NameRef(nr)) => nr,
            _ => continue,
        };
        let callee_token = match callee.token() {
            Some(t) => t,
            None => continue,
        };

        // Resolve to the callee declaration via the parent NAME_REF_EXPR span.
        let def = state
            .resolved_target_for_token(&callee_token)
            .and_then(|target| state.definition_from_target(target));

        // Collect parameter names from the resolved declaration.
        let param_names: Vec<String>;
        if let Some(ref d) = def
            && d.kind == DefKind::Function
        {
            let fn_node = match state.fn_decl_for(d) {
                Some(f) => f,
                None => continue,
            };
            param_names = fn_node
                .parameters()
                .filter_map(|p| p.name().map(|t| t.text().to_string()))
                .collect();
        } else {
            continue;
        };

        let args = call.positional_args();

        // Emit hints for positional args that match named parameters
        for (i, arg) in args.iter().enumerate() {
            let param_name = match param_names.get(i) {
                Some(n) => n.clone(),
                None => break,
            };

            // Skip if the argument is already a simple name matching the parameter
            // (e.g., `foo(x)` where param is `x` — hint would be redundant)
            if is_same_name_as_param(arg, &param_name) {
                continue;
            }

            let pos = state.line_index.position(inlay_hint_anchor_offset(arg));
            hints.push(InlayHint {
                position: Position::new(pos.line, pos.col),
                label: InlayHintLabel::String(format!("{param_name}:")),
                kind: Some(InlayHintKind::PARAMETER),
                text_edits: None,
                tooltip: None,
                padding_left: None,
                padding_right: Some(true),
                data: None,
            });
        }

        // Keyword args already have names in the source — no hints needed
    }

    // Walk all LET_STMT nodes to show inferred binding types
    for node in root.descendants() {
        if node.kind() != SyntaxKind::LET_STMT {
            continue;
        }
        let let_stmt = match vx::LetStmt::cast(node) {
            Some(ls) => ls,
            None => continue,
        };

        // Skip if there's already an explicit type annotation
        if let_stmt.slot_type().is_some() {
            continue;
        }

        let Some(name_tok) = let_stmt.name() else {
            continue;
        };

        // Look up the inferred type for this binding via SpanIndex.
        let name_span = state.current_span(name_tok.text_range());
        let Some(binding_id) = state.span_index.binding_at_name_span(name_span) else {
            continue;
        };
        let Some(ty) = state.arena[binding_id].ty.clone() else {
            continue;
        };

        let pos = state.line_index.position(name_tok.text_range().end());
        hints.push(InlayHint {
            position: Position::new(pos.line, pos.col),
            label: InlayHintLabel::String(format!(": {}", ty.display_with_env(&state.types.env))),
            kind: Some(InlayHintKind::TYPE),
            text_edits: None,
            tooltip: None,
            padding_left: None,
            padding_right: None,
            data: None,
        });
    }

    hints
}

fn is_root_callee_ref(token: &vx::SyntaxToken) -> bool {
    let Some(name_ref) = token.parent().and_then(vx::NameRefExpr::cast) else {
        return false;
    };
    for ancestor in name_ref.syntax().ancestors() {
        if let Some(call) = vx::CallExpr::cast(ancestor.clone()) {
            return call
                .callee()
                .is_some_and(|expr| expr.syntax() == name_ref.syntax());
        }
        if vx::WhereExpr::cast(ancestor).is_some() {
            continue;
        }
    }
    false
}

/// Check if an argument expression is a simple name reference matching the parameter name.
/// If so, the inlay hint would be redundant (e.g., `foo x` where param is `x`).
fn is_same_name_as_param(arg: &vx::Expr, param_name: &str) -> bool {
    match arg {
        vx::Expr::NameRef(nr) => nr.token().is_some_and(|t| t.text() == param_name),
        _ => false,
    }
}

fn inlay_hint_anchor_offset(arg: &vx::Expr) -> rowan::TextSize {
    let anchor_expr = unwrap_paren_expr(arg.clone());
    anchor_expr
        .syntax()
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| !token.kind().is_trivia())
        .map(|token| token.text_range().start())
        .unwrap_or_else(|| anchor_expr.syntax().text_range().start())
}

fn unwrap_paren_expr(expr: vx::Expr) -> vx::Expr {
    match expr {
        vx::Expr::Paren(paren) => match paren.expr() {
            Some(inner) => unwrap_paren_expr(inner),
            None => vx::Expr::Paren(paren),
        },
        other => other,
    }
}

// ---------------------------------------------------------------------------
// VIR -> Definition adapters
// ---------------------------------------------------------------------------

fn definition_from_item(arena: &VirArena, item_id: ItemId) -> Option<Definition> {
    let item = &arena[item_id];
    let (name, name_span, kind) = match &item.kind {
        ItemKind::Function(f) => (f.name.clone(), f.name_span, DefKind::Function),
        ItemKind::Struct(s) => (s.name.clone(), s.name_span, DefKind::Struct),
        ItemKind::Enum(e) => (e.name.clone(), e.name_span, DefKind::Enum),
        ItemKind::Newtype(n) => (n.name.clone(), n.name_span, DefKind::Newtype),
        ItemKind::TypeAlias(t) => (t.name.clone(), t.name_span, DefKind::TypeAlias),
        ItemKind::Namespace(ns) => (ns.name.clone(), ns.name_span, DefKind::Namespace),
        ItemKind::Error => return None,
    };
    Some(Definition {
        id: item.def_id,
        name,
        kind,
        name_span,
        full_span: item.span,
    })
}

fn definition_from_binding(arena: &VirArena, binding_id: BindingId) -> Definition {
    let binding = &arena[binding_id];
    let kind = match binding.kind {
        vx_ir::binding::BindingKind::Parameter => DefKind::Parameter,
        _ => DefKind::Binding,
    };
    Definition {
        id: DefId::Source {
            source: binding.name_span.source,
            // Use BindingId encoded as `local` for stable identity. Bindings
            // are arena-local, distinct from item DefIds (which use the
            // builder's per-source counter). We OR in a high bit to keep
            // them disjoint from item DefIds in equality tests.
            local: 0x8000_0000 | (binding_id.index() as u32),
        },
        name: binding.name.clone(),
        kind,
        name_span: binding.name_span,
        full_span: binding.name_span,
    }
}

fn definition_from_variant(
    arena: &VirArena,
    enum_item: ItemId,
    variant_index: u32,
) -> Option<Definition> {
    let ItemKind::Enum(e) = &arena[enum_item].kind else {
        return None;
    };
    let variant = e.variants.get(variant_index as usize)?;
    let enum_def_id = arena[enum_item].def_id;
    let local = match enum_def_id {
        DefId::Source { local, .. } => local,
    };
    let source = variant.name_span.source;
    Some(Definition {
        // Encode the variant index in the high bits so each (enum, variant) is
        // distinct from the enum and other variants. The actual identity that
        // matters for the LSP is name_span equality.
        id: DefId::Source {
            source,
            local: 0xC000_0000 | (local << 8) | (variant_index & 0xFF),
        },
        name: variant.name.clone(),
        kind: DefKind::Variant,
        name_span: variant.name_span,
        full_span: variant.name_span,
    })
}

/// Walk the arena and produce one Definition per visible declaration in `source`.
fn materialize_all_definitions(arena: &VirArena, source: SourceId) -> Vec<Definition> {
    let mut defs = Vec::new();

    // All items declared anywhere in this source.
    for (i, item) in arena.items.iter().enumerate() {
        if item.span.source != source {
            continue;
        }
        if let Some(def) = definition_from_item(arena, ItemId::from_index(i)) {
            defs.push(def);
        }
        // Enum variants are also definitions.
        if let ItemKind::Enum(e) = &item.kind {
            for (vi, variant) in e.variants.iter().enumerate() {
                if variant.name_span.source != source {
                    continue;
                }
                if let Some(def) = definition_from_variant(arena, ItemId::from_index(i), vi as u32)
                {
                    defs.push(def);
                }
            }
        }
    }

    // All bindings declared anywhere in this source.
    for (i, binding) in arena.bindings.iter().enumerate() {
        if binding.name_span.source != source {
            continue;
        }
        defs.push(definition_from_binding(arena, BindingId::from_index(i)));
    }

    defs
}

/// Find the innermost vx-ir scope that owns `offset` in `source`.
fn innermost_scope_at(arena: &VirArena, source: SourceId, offset: rowan::TextSize) -> ScopeId {
    let mut best: Option<(ScopeId, rowan::TextRange)> = None;
    for (i, scope) in arena.scopes.iter().enumerate() {
        let range = scope_text_range(arena, scope);
        if range.source != source {
            continue;
        }
        if !range.range.contains_inclusive(offset) {
            continue;
        }
        // Pick the smallest enclosing range.
        let candidate_len = range.range.len();
        if let Some((_, best_range)) = best {
            if candidate_len < best_range.len() {
                best = Some((ScopeId::from_index(i), range.range));
            }
        } else {
            best = Some((ScopeId::from_index(i), range.range));
        }
    }
    // Fallback to scope 0 (module root of first source unit) if nothing matched.
    best.map(|(id, _)| id).unwrap_or(ScopeId::from_index(0))
}

fn scope_text_range(arena: &VirArena, scope: &vx_ir::scope::Scope) -> SourceSpan {
    match scope.owner {
        ScopeOwner::Module(su_id) => arena[arena[su_id].top_level_body].span,
        ScopeOwner::Body(body_id) => arena[body_id].span,
        ScopeOwner::Block(expr_id) => arena[expr_id].span,
        ScopeOwner::MatchArm {
            match_expr,
            arm_index,
        } => {
            if let ExprKind::Match(m) = &arena[match_expr].kind
                && let Some(arm) = m.arms.get(arm_index as usize)
            {
                return arm.span;
            }
            arena[match_expr].span
        }
        ScopeOwner::Namespace(item_id) => arena[item_id].span,
    }
}

/// Enumerate definitions visible at `offset` by walking the scope chain.
/// Mirrors the legacy `ProgramScopes::visible_at` against vx-ir scopes.
fn visible_definitions_at(
    arena: &VirArena,
    from: ScopeId,
    offset: rowan::TextSize,
) -> Vec<Definition> {
    use std::collections::HashSet;

    let mut seen: HashSet<String> = HashSet::new();
    let mut result: Vec<Definition> = Vec::new();
    let mut scope_id = Some(from);

    while let Some(id) = scope_id {
        let scope = &arena[id];
        // Iterate entries in reverse so later bindings shadow earlier ones.
        for entry in scope.entries.iter().rev() {
            // For block scopes, only include declarations before the cursor.
            let name_span = match entry.target {
                ScopeEntryTarget::Binding(b) => arena[b].name_span,
                ScopeEntryTarget::Item(i) => match arena[i].name_span() {
                    Some(s) => s,
                    None => continue,
                },
            };
            if matches!(scope.owner, ScopeOwner::Block(_)) && name_span.range.start() >= offset {
                continue;
            }
            let name = entry.name(arena).to_string();
            if seen.insert(name.clone()) {
                let def = match entry.target {
                    ScopeEntryTarget::Binding(b) => Some(definition_from_binding(arena, b)),
                    ScopeEntryTarget::Item(i) => definition_from_item(arena, i),
                };
                if let Some(def) = def {
                    result.push(def);
                }
            }
        }
        for pv in &scope.promoted_variants {
            if seen.insert(pv.name.clone())
                && let Some(def) = definition_from_variant(arena, pv.enum_item, pv.variant_index)
            {
                result.push(def);
            }
        }
        scope_id = scope.parent;
    }
    result
}

/// Compute methods visible on a receiver of type `ty` at a given offset.
/// Mirrors the legacy `visible_members_for_receiver` but uses vx-ir directly.
fn visible_members_for_receiver(
    state: &DocumentState,
    _offset: rowan::TextSize,
    receiver_ty: &vx_types::Ty,
) -> Vec<Definition> {
    let Some(namespace_name) = namespace_name_for_receiver(receiver_ty, &state.types.env) else {
        return Vec::new();
    };
    // Find a namespace item by name across all source units.
    let mut members = Vec::new();
    for (i, item) in state.arena.items.iter().enumerate() {
        let ItemKind::Namespace(ns) = &item.kind else {
            continue;
        };
        if ns.name != namespace_name {
            continue;
        }
        let _ = i;
        for &child_id in &ns.items {
            if let Some(def) = definition_from_item(&state.arena, child_id) {
                members.push(def);
            }
        }
    }
    members
}

fn namespace_name_for_receiver(
    receiver_ty: &vx_types::Ty,
    env: &vx_types::TypeEnv,
) -> Option<String> {
    use vx_types::{Scalar, Ty};
    match receiver_ty {
        Ty::Scalar(Scalar::String) => Some("String".to_string()),
        Ty::Optional(_) => Some("Option".to_string()),
        Ty::Array(_) => Some("Array".to_string()),
        Ty::Map(_, _) => Some("Map".to_string()),
        Ty::Stream(_) => Some("Stream".to_string()),
        Ty::Struct(struct_ty) => Some(env.get(struct_ty.id).name().to_string()),
        Ty::Enum(enum_ty) => Some(env.get(enum_ty.id).name().to_string()),
        Ty::Newtype(newtype_ty) => Some(env.get(newtype_ty.id).name().to_string()),
        Ty::Alias(alias_ty) => Some(env.get(alias_ty.id).name().to_string()),
        Ty::Opaque(opaque_ty) => Some(env.get(opaque_ty.id).name().to_string()),
        _ => None,
    }
}
