use std::collections::HashMap;
use std::sync::Mutex;

use rowan::TextRange;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};
use vx_parser::{self as vx, AstNode, FnDecl, Parsed, SourceFile, SyntaxKind};
use vx_resolve::NameResolution;

pub struct VxLanguageServer {
    client: Client,
    documents: Mutex<HashMap<Url, DocumentState>>,
}

struct DocumentState {
    text: String,
    parsed: Parsed<SourceFile>,
    line_index: vx::LineIndex,
    resolution: NameResolution,
    types: vx_resolve::TypeCollection,
    cap_check: vx_resolve::CapCheckResult,
    type_check: vx_resolve::TypeCheckResult,
}

impl DocumentState {
    fn new(text: String) -> Self {
        let parsed = vx::parse_source_file(&text);
        let line_index = vx::LineIndex::new(&text);
        let resolution = vx_resolve::resolve(&parsed.tree());
        let types = vx_resolve::collect_types(&parsed.tree());
        let cap_check = vx_resolve::check_capabilities(&parsed.tree(), &resolution.scopes, &types);
        let type_check = vx_resolve::type_check(&parsed.tree(), &resolution, &types);
        Self {
            text,
            parsed,
            line_index,
            resolution,
            types,
            cap_check,
            type_check,
        }
    }

    /// Find the definition under the cursor and the range that should be
    /// treated as the origin span for editor features.
    fn definition_with_origin_at(
        &self,
        offset: rowan::TextSize,
    ) -> Option<(&vx_resolve::Definition, TextRange)> {
        // Check if cursor is on a reference
        if let Some(resolved) = self
            .resolution
            .refs
            .iter()
            .find(|r| r.ref_range.contains_inclusive(offset))
        {
            return Some((resolved.target.as_ref().ok()?, resolved.ref_range));
        }

        // Check if cursor is on a definition site
        let def = self
            .resolution
            .scopes
            .all_definitions()
            .find(|d| d.name_range.contains_inclusive(offset))?;
        Some((def, def.name_range))
    }

    /// Find the definition under the cursor. Works whether the cursor is on
    /// a definition site or on a reference that resolves to one.
    fn definition_at(&self, offset: rowan::TextSize) -> Option<&vx_resolve::Definition> {
        Some(self.definition_with_origin_at(offset)?.0)
    }

    /// Find all reference ranges that point to a given definition.
    fn references_to(&self, def: &vx_resolve::Definition) -> Vec<TextRange> {
        self.resolution
            .refs
            .iter()
            .filter_map(|r| {
                let target = r.target.as_ref().ok()?;
                if target.name_range == def.name_range {
                    Some(r.ref_range)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Extract a declaration snippet for hover display.
    /// Takes the first line of the full declaration range.
    fn declaration_snippet(&self, def: &vx_resolve::Definition) -> String {
        let start = u32::from(def.full_range.start()) as usize;
        let end = u32::from(def.full_range.end()) as usize;
        let slice = &self.text[start..end];
        let first_line = slice.lines().next().unwrap_or(slice);
        first_line.trim().to_string()
    }

    /// Extract the doc comment attached to a definition's syntax node.
    fn doc_comment_for(&self, def: &vx_resolve::Definition) -> Option<String> {
        let node = self.declaration_node_for(def)?;
        vx::doc_comment(&node)
    }

    fn documentation_for(&self, def: &vx_resolve::Definition) -> Option<String> {
        self.doc_comment_for(def).or_else(|| {
            self.type_check
                .closure_param_docs
                .get(&def.name_range)
                .cloned()
        })
    }

    fn declaration_node_for(&self, def: &vx_resolve::Definition) -> Option<vx::SyntaxNode> {
        let expected_kind = match def.kind {
            vx_resolve::DefKind::Function => SyntaxKind::FN_DECL,
            vx_resolve::DefKind::Struct => SyntaxKind::STRUCT_DECL,
            vx_resolve::DefKind::Enum => SyntaxKind::ENUM_DECL,
            vx_resolve::DefKind::Newtype => SyntaxKind::NEWTYPE_DECL,
            vx_resolve::DefKind::TypeAlias => SyntaxKind::TYPE_ALIAS_DECL,
            vx_resolve::DefKind::Binding
            | vx_resolve::DefKind::Parameter
            | vx_resolve::DefKind::Variant
            | vx_resolve::DefKind::Field
            | vx_resolve::DefKind::Builtin => return None,
        };

        self.parsed
            .tree()
            .syntax()
            .descendants()
            .find(|node| node.kind() == expected_kind && node.text_range() == def.full_range)
    }

    fn inferred_def_type(&self, def: &vx_resolve::Definition) -> Option<vx_types::Ty> {
        if let Some(ty) = self.type_check.binding_types.get(&def.name_range) {
            return Some(ty.clone());
        }

        if let Some(ty) = self.type_check.closure_param_types.get(&def.name_range) {
            return Some(ty.clone());
        }

        for resolved_ref in &self.resolution.refs {
            if let Ok(ref target) = resolved_ref.target {
                if target.name_range == def.name_range {
                    if let Some(ty) = self.type_check.expr_types.get(&resolved_ref.ref_range) {
                        return Some(ty.clone());
                    }
                }
            }
        }

        self.type_check.expr_types.get(&def.name_range).cloned()
    }

    fn to_lsp_range(&self, range: TextRange) -> Range {
        let start = self.line_index.position(range.start());
        let end = self.line_index.position(range.end());
        Range {
            start: Position::new(start.line, start.col),
            end: Position::new(end.line, end.col),
        }
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
                linked_editing_range_provider: Some(LinkedEditingRangeServerCapabilities::Simple(
                    true,
                )),
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
        let state = DocumentState::new(text);
        self.publish_diagnostics(&uri, &state).await;
        self.documents.lock().unwrap().insert(uri, state);
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        if let Some(change) = params.content_changes.into_iter().last() {
            let state = DocumentState::new(change.text);
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
            let vx_ranges = vx::folding_ranges(&state.parsed.tree().syntax());
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
            let tokens = vx::semantic_tokens(&state.parsed.tree().syntax());
            // Collect ranges of builtin references for modifier tagging
            let builtin_ranges: std::collections::HashSet<rowan::TextRange> = state
                .resolution
                .refs
                .iter()
                .filter(|r| {
                    r.target
                        .as_ref()
                        .is_ok_and(|d| d.kind == vx_resolve::DefKind::Builtin)
                })
                .map(|r| r.ref_range)
                .collect();
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

            // Find a resolved ref whose range contains the cursor
            if let Some(resolved) = state
                .resolution
                .refs
                .iter()
                .find(|r| r.ref_range.contains_inclusive(offset))
            {
                let def = resolved.target.as_ref().ok()?;
                let range = state.to_lsp_range(def.name_range);
                return Some(GotoDefinitionResponse::Scalar(Location {
                    uri: uri.clone(),
                    range,
                }));
            }

            // Try field access: jump to struct field definition
            let root = state.parsed.tree().syntax().clone();
            let token = root.token_at_offset(offset).left_biased()?;
            if let Some(field_node) = token.parent().and_then(vx::FieldExpr::cast) {
                if let Some(field_name_tok) = field_node.field_name() {
                    if field_name_tok.text_range().contains_inclusive(offset) {
                        let field_name = field_name_tok.text();
                        if let Some(receiver_expr) = field_node.receiver() {
                            let receiver_ty = state
                                .type_check
                                .expr_types
                                .get(&receiver_expr.syntax().text_range());
                            if let Some(vx_types::Ty::Struct(struct_ty)) = receiver_ty {
                                if let vx_types::TypeDef::Struct(sdef) =
                                    &state.types.env[struct_ty.id]
                                {
                                    if let Some(fdef) =
                                        sdef.fields.iter().find(|f| f.name == field_name)
                                    {
                                        return Some(GotoDefinitionResponse::Scalar(Location {
                                            uri: uri.clone(),
                                            range: state.to_lsp_range(fdef.name_range),
                                        }));
                                    }
                                }
                            }
                        }
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
                .references_to(def)
                .into_iter()
                .map(|range| Location {
                    uri: uri.clone(),
                    range: state.to_lsp_range(range),
                })
                .collect();

            if include_declaration {
                locations.push(Location {
                    uri: uri.clone(),
                    range: state.to_lsp_range(def.name_range),
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
            Some(PrepareRenameResponse::RangeWithPlaceholder {
                range: state.to_lsp_range(def.name_range),
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

            // Collect all edits: the definition site + all references
            let mut edits: Vec<TextEdit> = state
                .references_to(def)
                .into_iter()
                .map(|range| TextEdit {
                    range: state.to_lsp_range(range),
                    new_text: new_name.clone(),
                })
                .collect();

            // Also rename the definition itself
            edits.push(TextEdit {
                range: state.to_lsp_range(def.name_range),
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
                // Special handling for builtin functions
                if def.kind == vx_resolve::DefKind::Builtin {
                    if let Some(sig) = vx_resolve::builtins::builtin_fn_sig(&def.name) {
                        let ret_display = sig.ret.display_with_env(&state.types.env);
                        let params: Vec<String> = sig
                            .positional_params
                            .iter()
                            .map(|p| format!("{}: {}", p.name, p.ty))
                            .chain(sig.keyword_params.iter().map(|p| {
                                if p.has_default {
                                    format!("{}: {} = ...", p.name, p.ty)
                                } else {
                                    format!("{}: {}", p.name, p.ty)
                                }
                            }))
                            .collect();
                        let caps_str = if sig.capabilities.is_empty() {
                            String::new()
                        } else {
                            let caps: Vec<_> = sig
                                .capabilities
                                .iter()
                                .map(|c| format!("#{}", c.name))
                                .collect();
                            format!(" {}", caps.join(" "))
                        };
                        let snippet = format!(
                            "fn {}({}){} -> {}",
                            def.name,
                            params.join(", "),
                            caps_str,
                            ret_display
                        );
                        let markdown = format!("```vx\n{snippet}\n```\n---\n*builtin function*");
                        return Some(Hover {
                            contents: HoverContents::Markup(MarkupContent {
                                kind: MarkupKind::Markdown,
                                value: markdown,
                            }),
                            range: Some(state.to_lsp_range(hover_range)),
                        });
                    }
                }

                let kind_label = def_kind_label(def.kind);
                let snippet = state.declaration_snippet(def);
                let doc = state.documentation_for(def);

                let mut markdown = format!("```vx\n{snippet}\n```\n---\n*{kind_label}*");

                // Show type signature for functions
                if let Some(sig) = state.types.range_to_fn_sig.get(&def.name_range) {
                    markdown.push_str(&format!("\n\nReturns `{}`", sig.ret));
                }

                // Show inferred type for parameters and bindings from the type checker
                if matches!(
                    def.kind,
                    vx_resolve::DefKind::Parameter | vx_resolve::DefKind::Binding
                ) {
                    if let Some(ty) = state.inferred_def_type(def) {
                        let display = ty.display_with_env(&state.types.env);
                        markdown.push_str(&format!("\n\nType: `{display}`"));
                    }
                }

                // Show fields for structs
                if let Some(&type_id) = state.types.range_to_type.get(&def.name_range) {
                    match &state.types.env[type_id] {
                        vx_types::TypeDef::Struct(s) if !s.fields.is_empty() => {
                            markdown.push_str("\n\n**Fields:**\n");
                            for f in &s.fields {
                                markdown.push_str(&format!("- `{}`: `{}`\n", f.name, f.ty));
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
                    range: Some(state.to_lsp_range(hover_range)),
                });
            }

            // Try method name hover: hovering on `.method_name` in `receiver.method_name(...)`
            let root = state.parsed.tree().syntax().clone();
            let token = root.token_at_offset(offset).left_biased()?;

            if let Some(field_node) = token.parent().and_then(vx::FieldExpr::cast) {
                if let Some(field_name_tok) = field_node.field_name() {
                    if field_name_tok.text_range().contains_inclusive(offset) {
                        // Infer receiver type from type_check
                        if let Some(receiver_expr) = field_node.receiver() {
                            let receiver_ty = state
                                .type_check
                                .expr_types
                                .get(&receiver_expr.syntax().text_range());
                            if let Some(vx_types::Ty::Builtin(builtin)) = receiver_ty {
                                let method_name = field_name_tok.text();
                                if let Some(method_sig) =
                                    vx_resolve::builtins::builtin_method(builtin, method_name)
                                {
                                    let params: Vec<String> = method_sig
                                        .params
                                        .iter()
                                        .map(|p| {
                                            format!(
                                                "{}: {}",
                                                p.name,
                                                p.ty.display_with_env(&state.types.env)
                                            )
                                        })
                                        .collect();
                                    let ret_display =
                                        method_sig.ret.display_with_env(&state.types.env);
                                    let caps_str = if method_sig.capabilities.is_empty() {
                                        String::new()
                                    } else {
                                        let caps: Vec<_> = method_sig
                                            .capabilities
                                            .iter()
                                            .map(|c| format!("#{}", c.name))
                                            .collect();
                                        format!(" {}", caps.join(" "))
                                    };
                                    let snippet = format!(
                                        "fn {}({}){} -> {}",
                                        method_name,
                                        params.join(", "),
                                        caps_str,
                                        ret_display
                                    );
                                    let markdown = format!(
                                        "```vx\n{snippet}\n```\n---\n*method on `{builtin}`*"
                                    );
                                    return Some(Hover {
                                        contents: HoverContents::Markup(MarkupContent {
                                            kind: MarkupKind::Markdown,
                                            value: markdown,
                                        }),
                                        range: Some(
                                            state.to_lsp_range(field_name_tok.text_range()),
                                        ),
                                    });
                                }
                            }
                        }
                    }
                }
            }

            // Walk up from the token to find the smallest expression with a type
            let mut node = token.parent();
            while let Some(n) = node {
                if let Some(expr) = vx::Expr::cast(n.clone()) {
                    if let Some(ty) = state.type_check.expr_types.get(&expr.syntax().text_range()) {
                        let display = ty.display_with_env(&state.types.env);
                        let markdown = format!("```vx\n{display}\n```");
                        return Some(Hover {
                            contents: HoverContents::Markup(MarkupContent {
                                kind: MarkupKind::Markdown,
                                value: markdown,
                            }),
                            range: Some(state.to_lsp_range(expr.syntax().text_range())),
                        });
                    }
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
                .resolution
                .scopes
                .visible_at(offset)
                .into_iter()
                .map(|def| {
                    let documentation = state.documentation_for(def).map(|doc| {
                        Documentation::MarkupContent(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: doc,
                        })
                    });
                    CompletionItem {
                        label: def.name.clone(),
                        kind: Some(def_kind_to_completion(def.kind)),
                        detail: Some(state.declaration_snippet(def)),
                        documentation,
                        ..Default::default()
                    }
                })
                .collect();

            // Add builtin function completions
            items.extend(builtin_completions(&state.types.env));

            // Add keyword snippets
            items.extend(keyword_completions());

            Some(CompletionResponse::Array(items))
        });

        Ok(result.flatten())
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let result = self.with_document(&params.text_document.uri, |state| {
            let formatted = vx::format(&state.parsed.tree().syntax());
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
                let receiver_expr = field_expr.receiver()?;
                let method_name_tok = field_expr.field_name()?;
                let method_name = method_name_tok.text().to_string();

                // Get receiver type from type checker
                let receiver_ty = state
                    .type_check
                    .expr_types
                    .get(&receiver_expr.syntax().text_range());
                if let Some(vx_types::Ty::Builtin(builtin)) = receiver_ty {
                    if let Some(method_sig) =
                        vx_resolve::builtins::builtin_method(builtin, &method_name)
                    {
                        label_parts.push(format!("fn {method_name}("));
                        for (i, p) in method_sig.params.iter().enumerate() {
                            if i > 0 {
                                label_parts.push(", ".to_string());
                            }
                            let param_label =
                                format!("{}: {}", p.name, p.ty.display_with_env(&state.types.env));
                            params.push(ParameterInformation {
                                label: ParameterLabel::Simple(param_label.clone()),
                                documentation: None,
                            });
                            label_parts.push(param_label);
                        }
                        label_parts.push(")".to_string());
                        let ret_display = method_sig.ret.display_with_env(&state.types.env);
                        label_parts.push(format!(" -> {ret_display}"));
                    }
                }
            }

            // Try simple name call: name(args) or builtin(args)
            if params.is_empty() {
                let callee_token = match callee_expr {
                    vx::Expr::NameRef(ref nr) => nr.token()?,
                    _ if !label_parts.is_empty() => {
                        // Already handled above (method call)
                        // Fall through to build the result
                        // This branch shouldn't actually be reached since params would be populated
                        return None;
                    }
                    _ => return None,
                };

                let callee_range = callee_token.text_range();
                let callee_name_str = callee_token.text().to_string();
                let resolved = state
                    .resolution
                    .refs
                    .iter()
                    .find(|r| r.ref_range == callee_range);
                let def = resolved.and_then(|r| r.target.as_ref().ok());

                if let Some(def) = def
                    && def.kind == vx_resolve::DefKind::Function
                {
                    // User-declared function — use FnDecl from definition site
                    let def_token = root.token_at_offset(def.full_range.start()).right_biased();
                    let fn_node =
                        def_token.and_then(|t| t.parent_ancestors().find_map(FnDecl::cast));

                    if let Some(fn_node) = fn_node {
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

                        doc = state.documentation_for(def);
                    } else {
                        return None;
                    }
                } else if let Some(sig) = vx_resolve::builtins::builtin_fn_sig(&callee_name_str) {
                    // Builtin function — use typed signature
                    label_parts.push(format!("fn {}(", sig.name));

                    for (i, p) in sig.positional_params.iter().enumerate() {
                        if i > 0 {
                            label_parts.push(", ".to_string());
                        }
                        let param_label =
                            format!("{}: {}", p.name, p.ty.display_with_env(&state.types.env));
                        params.push(ParameterInformation {
                            label: ParameterLabel::Simple(param_label.clone()),
                            documentation: None,
                        });
                        label_parts.push(param_label);
                    }

                    if !sig.keyword_params.is_empty() {
                        if !sig.positional_params.is_empty() {
                            label_parts.push("; ".to_string());
                        }
                        for (i, kp) in sig.keyword_params.iter().enumerate() {
                            if i > 0 {
                                label_parts.push(", ".to_string());
                            }
                            let param_label = format!(
                                "{}: {}",
                                kp.name,
                                kp.ty.display_with_env(&state.types.env)
                            );
                            params.push(ParameterInformation {
                                label: ParameterLabel::Simple(param_label.clone()),
                                documentation: None,
                            });
                            label_parts.push(param_label);
                        }
                    }

                    label_parts.push(")".to_string());

                    if !sig.ret.is_poison() {
                        let ret_display = sig.ret.display_with_env(&state.types.env);
                        label_parts.push(format!(" -> {ret_display}"));
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
                if diag.source.as_deref() == Some("vx-resolve")
                    && diag.message.starts_with("unresolved name")
                {
                    if let Some(name) = diag.message.split('`').nth(1) {
                        // Compute offset for the diagnostic start
                        let offset = state.line_index.offset(vx::Position {
                            line: diag.range.start.line,
                            col: diag.range.start.character,
                        });
                        if let Some(offset) = offset {
                            let visible = state.resolution.scopes.visible_at(offset);
                            // Find closest name by edit distance
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

            let mut highlights = vec![DocumentHighlight {
                range: state.to_lsp_range(def.name_range),
                kind: Some(DocumentHighlightKind::WRITE),
            }];

            for ref_range in state.references_to(def) {
                highlights.push(DocumentHighlight {
                    range: state.to_lsp_range(ref_range),
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
            if def.kind != vx_resolve::DefKind::Function {
                return None;
            }

            Some(vec![make_call_hierarchy_item(uri, def, state)])
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

            // Find all call sites that reference this function
            let call_refs: Vec<_> = state
                .resolution
                .refs
                .iter()
                .filter(|r| {
                    r.target
                        .as_ref()
                        .ok()
                        .is_some_and(|d| d.name_range == target_def.name_range)
                })
                .filter(|r| {
                    // Only include refs that are the root callee of an application chain.
                    let token = root.token_at_offset(r.ref_range.start()).right_biased();
                    token.is_some_and(|t| is_root_callee_ref(&t))
                })
                .collect();

            // Group by enclosing function
            let mut by_caller: HashMap<rowan::TextRange, (vx_resolve::Definition, Vec<TextRange>)> =
                HashMap::new();
            for r in &call_refs {
                let token = root.token_at_offset(r.ref_range.start()).right_biased()?;
                // Walk up to find the enclosing FN_DECL
                let fn_node = token.parent_ancestors().find_map(FnDecl::cast)?;
                let caller_name_token = fn_node.name()?;
                let caller_name_range = caller_name_token.text_range();

                // Find the definition for this enclosing function
                let caller_def = state
                    .resolution
                    .scopes
                    .all_definitions()
                    .find(|d| d.name_range == caller_name_range)?;

                by_caller
                    .entry(caller_name_range)
                    .or_insert_with(|| (caller_def.clone(), Vec::new()))
                    .1
                    .push(r.ref_range);
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
            let mut by_target: HashMap<rowan::TextRange, (vx_resolve::Definition, Vec<TextRange>)> =
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

                // Resolve the callee
                let resolved = state
                    .resolution
                    .refs
                    .iter()
                    .find(|r| r.ref_range == callee_range)?;
                let def = resolved.target.as_ref().ok()?;
                if def.kind != vx_resolve::DefKind::Function {
                    continue;
                }

                by_target
                    .entry(def.name_range)
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
            for def in state.resolution.scopes.all_definitions() {
                // Skip local bindings/parameters — only top-level-ish symbols
                if matches!(
                    def.kind,
                    vx_resolve::DefKind::Binding | vx_resolve::DefKind::Parameter
                ) {
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
                            uri: uri.clone(),
                            range: state.to_lsp_range(def.name_range),
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
        params: LinkedEditingRangeParams,
    ) -> Result<Option<LinkedEditingRanges>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let result = self.with_document(uri, |state| {
            let offset = state.line_index.offset(vx::Position {
                line: pos.line,
                col: pos.character,
            })?;

            let def = state.definition_at(offset)?;

            let mut ranges = vec![state.to_lsp_range(def.name_range)];
            for ref_range in state.references_to(def) {
                ranges.push(state.to_lsp_range(ref_range));
            }

            Some(LinkedEditingRanges {
                ranges,
                word_pattern: None,
            })
        });

        Ok(result.flatten())
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

        // Unresolved name diagnostics from resolution
        for err in &state.resolution.errors {
            match err {
                vx_resolve::ResolveError::Unresolved { name, range } => {
                    diagnostics.push(Diagnostic {
                        range: state.to_lsp_range(*range),
                        severity: Some(DiagnosticSeverity::ERROR),
                        source: Some("vx-resolve".to_string()),
                        message: format!("unresolved name `{name}`"),
                        ..Default::default()
                    });
                }
            }
        }

        // Capability checking diagnostics
        for err in &state.cap_check.errors {
            let message = match &err.kind {
                vx_resolve::CapErrorKind::CapabilityViolation {
                    slot_name,
                    allowed,
                    excess,
                    ..
                } => {
                    format!(
                        "capability violation in `{slot_name}`: requires {excess} \
                         but slot only allows {allowed}"
                    )
                }
                vx_resolve::CapErrorKind::UnknownCapability { name } => {
                    format!("unknown capability `#{name}`")
                }
            };
            diagnostics.push(Diagnostic {
                range: state.to_lsp_range(err.range),
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("vx-cap".to_string()),
                message,
                ..Default::default()
            });
        }

        // Unused binding/parameter warnings
        {
            use std::collections::HashSet;
            // Collect all definition name_ranges that have at least one reference
            let referenced: HashSet<_> = state
                .resolution
                .refs
                .iter()
                .filter_map(|r| r.target.as_ref().ok())
                .map(|def| def.name_range)
                .collect();

            for def in state.resolution.scopes.all_definitions() {
                if !matches!(
                    def.kind,
                    vx_resolve::DefKind::Binding | vx_resolve::DefKind::Parameter
                ) {
                    continue;
                }
                // Skip names starting with _ (conventional unused marker)
                if def.name.starts_with('_') {
                    continue;
                }
                if !referenced.contains(&def.name_range) {
                    diagnostics.push(Diagnostic {
                        range: state.to_lsp_range(def.name_range),
                        severity: Some(DiagnosticSeverity::HINT),
                        source: Some("vx-lint".to_string()),
                        message: format!(
                            "unused {}: `{}`",
                            if def.kind == vx_resolve::DefKind::Binding {
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
            let message = match &err.kind {
                vx_resolve::TypeErrorKind::ArgTypeMismatch {
                    param_name,
                    expected,
                    found,
                } => {
                    format!(
                        "type mismatch for `{param_name}`: expected `{expected}`, found `{found}`"
                    )
                }
                vx_resolve::TypeErrorKind::NoSuchMethod {
                    receiver_ty,
                    method_name,
                } => {
                    format!("no method `{method_name}` on type `{receiver_ty}`")
                }
            };
            diagnostics.push(Diagnostic {
                range: state.to_lsp_range(err.range),
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("vx-type".to_string()),
                message,
                ..Default::default()
            });
        }

        self.client
            .publish_diagnostics(uri.clone(), diagnostics, None)
            .await;
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

fn def_kind_label(kind: vx_resolve::DefKind) -> &'static str {
    match kind {
        vx_resolve::DefKind::Function => "function",
        vx_resolve::DefKind::Struct => "struct",
        vx_resolve::DefKind::Enum => "enum",
        vx_resolve::DefKind::Newtype => "newtype",
        vx_resolve::DefKind::TypeAlias => "type alias",
        vx_resolve::DefKind::Binding => "binding",
        vx_resolve::DefKind::Parameter => "parameter",
        vx_resolve::DefKind::Variant => "variant",
        vx_resolve::DefKind::Field => "field",
        vx_resolve::DefKind::Builtin => "builtin",
    }
}

fn def_kind_to_completion(kind: vx_resolve::DefKind) -> CompletionItemKind {
    match kind {
        vx_resolve::DefKind::Function => CompletionItemKind::FUNCTION,
        vx_resolve::DefKind::Struct => CompletionItemKind::STRUCT,
        vx_resolve::DefKind::Enum => CompletionItemKind::ENUM,
        vx_resolve::DefKind::Newtype => CompletionItemKind::CLASS,
        vx_resolve::DefKind::TypeAlias => CompletionItemKind::TYPE_PARAMETER,
        vx_resolve::DefKind::Binding => CompletionItemKind::VARIABLE,
        vx_resolve::DefKind::Parameter => CompletionItemKind::VARIABLE,
        vx_resolve::DefKind::Variant => CompletionItemKind::ENUM_MEMBER,
        vx_resolve::DefKind::Field => CompletionItemKind::FIELD,
        vx_resolve::DefKind::Builtin => CompletionItemKind::FUNCTION,
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
    def: &vx_resolve::Definition,
    state: &DocumentState,
) -> CallHierarchyItem {
    CallHierarchyItem {
        name: def.name.clone(),
        kind: def_kind_to_symbol(def.kind),
        tags: None,
        detail: None,
        uri: uri.clone(),
        range: state.to_lsp_range(def.full_range),
        selection_range: state.to_lsp_range(def.name_range),
        data: None,
    }
}

fn def_kind_to_symbol(kind: vx_resolve::DefKind) -> SymbolKind {
    match kind {
        vx_resolve::DefKind::Function => SymbolKind::FUNCTION,
        vx_resolve::DefKind::Struct => SymbolKind::STRUCT,
        vx_resolve::DefKind::Enum => SymbolKind::ENUM,
        vx_resolve::DefKind::Newtype => SymbolKind::CLASS,
        vx_resolve::DefKind::TypeAlias => SymbolKind::TYPE_PARAMETER,
        vx_resolve::DefKind::Binding => SymbolKind::VARIABLE,
        vx_resolve::DefKind::Parameter => SymbolKind::VARIABLE,
        vx_resolve::DefKind::Variant => SymbolKind::ENUM_MEMBER,
        vx_resolve::DefKind::Field => SymbolKind::FIELD,
        vx_resolve::DefKind::Builtin => SymbolKind::FUNCTION,
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

    // Walk up to find a FIELD_EXPR or look for a DOT token
    let dot_token = if token.kind() == SyntaxKind::DOT {
        token
    } else {
        // Maybe we're on the field name after the dot — look for DOT sibling
        return None;
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

            // Try to resolve this name
            let resolved = state
                .resolution
                .refs
                .iter()
                .find(|r| r.ref_range == name_range)?;
            let def = resolved.target.as_ref().ok()?;

            // If it's an enum, offer variants
            if def.kind == vx_resolve::DefKind::Enum {
                let type_id = state.types.range_to_type.get(&def.name_range)?;
                if let vx_types::TypeDef::Enum(e) = &state.types.env[*type_id] {
                    let items: Vec<CompletionItem> = e
                        .variants
                        .iter()
                        .filter_map(|&vid| {
                            let vdef = &state.types.env[vid];
                            let vname = vdef.name().to_string();
                            Some(CompletionItem {
                                label: vname,
                                kind: Some(CompletionItemKind::ENUM_MEMBER),
                                ..Default::default()
                            })
                        })
                        .collect();
                    return Some(items);
                }
            }

            // If it's a binding or parameter, offer completions based on inferred type
            if matches!(
                def.kind,
                vx_resolve::DefKind::Binding | vx_resolve::DefKind::Parameter
            ) {
                if let Some(ty) = state.inferred_def_type(def) {
                    if let Some(items) = completions_for_type(&ty, state) {
                        return Some(items);
                    }
                }
                // Fall back to function signature matching
                if let Some(fields) = resolve_binding_fields(def, state) {
                    return Some(fields);
                }
            }

            // If it's a function, offer completions on the return type
            if def.kind == vx_resolve::DefKind::Function {
                if let Some(sig) = state.types.range_to_fn_sig.get(&def.name_range) {
                    if let Some(items) = completions_for_type(&sig.ret, state) {
                        return Some(items);
                    }
                }
            }

            None
        }
        _ => {
            // For any other expression, try the type checker's expr_types
            let expr_range = receiver_expr.syntax().text_range();
            if let Some(ty) = state.type_check.expr_types.get(&expr_range) {
                return completions_for_type(ty, state);
            }
            None
        }
    }
}

/// Try to determine the type of a binding and return its struct fields as completions.
fn resolve_binding_fields(
    def: &vx_resolve::Definition,
    state: &DocumentState,
) -> Option<Vec<CompletionItem>> {
    // Check if this binding is a function parameter with a type annotation
    for sig in state.types.range_to_fn_sig.values() {
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

/// Given a Ty, return completions for dot access — struct fields or builtin methods.
fn completions_for_type(ty: &vx_types::Ty, state: &DocumentState) -> Option<Vec<CompletionItem>> {
    match ty {
        vx_types::Ty::Builtin(builtin) => {
            let method_names = vx_resolve::builtins::builtin_methods_for(builtin);
            if method_names.is_empty() {
                return None;
            }
            let items: Vec<CompletionItem> = method_names
                .iter()
                .filter_map(|name| {
                    let sig = vx_resolve::builtins::builtin_method(builtin, name)?;
                    let detail = if sig.params.is_empty() {
                        format!("{}", sig.ret)
                    } else {
                        let params: Vec<_> =
                            sig.params.iter().map(|p| format!("{}", p.ty)).collect();
                        format!("({}) -> {}", params.join(", "), sig.ret)
                    };
                    Some(CompletionItem {
                        label: name.to_string(),
                        kind: Some(CompletionItemKind::METHOD),
                        detail: Some(detail),
                        ..Default::default()
                    })
                })
                .collect();
            if items.is_empty() { None } else { Some(items) }
        }
        vx_types::Ty::Optional(inner) => completions_for_type(inner, state),
        _ => struct_fields_from_ty(ty, state),
    }
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
                        detail: Some(format!("{}", f.ty)),
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

fn builtin_completions(env: &vx_types::TypeEnv) -> Vec<CompletionItem> {
    vx_resolve::builtins::all_builtin_fn_names()
        .into_iter()
        .filter_map(|name| {
            let sig = vx_resolve::builtins::builtin_fn_sig(&name)?;
            let ret_display = sig.ret.display_with_env(env);
            let detail = format!("fn {name}(...) -> {ret_display}");
            Some(CompletionItem {
                label: name,
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some(detail),
                ..Default::default()
            })
        })
        .collect()
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
fn closest_name(target: &str, visible: &[&vx_resolve::Definition]) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for def in visible {
        let dist = levenshtein(target, &def.name);
        if dist > 0 && dist <= 2 {
            if best.as_ref().is_none_or(|(d, _)| dist < *d) {
                best = Some((dist, def.name.clone()));
            }
        }
    }
    // Also check builtins
    for name in vx_resolve::builtins::all_builtin_fn_names() {
        let dist = levenshtein(target, &name);
        if dist > 0 && dist <= 2 {
            if best.as_ref().is_none_or(|(d, _)| dist < *d) {
                best = Some((dist, name));
            }
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
    for i in 0..=m {
        dp[i][0] = i;
    }
    for j in 0..=n {
        dp[0][j] = j;
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

fn collect_inlay_hints(state: &DocumentState) -> Vec<InlayHint> {
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

        // Resolve to definition or builtin
        let callee_range = callee_token.text_range();
        let callee_name = callee_token.text().to_string();
        let resolved = state
            .resolution
            .refs
            .iter()
            .find(|r| r.ref_range == callee_range);
        let def = resolved.and_then(|r| r.target.as_ref().ok());

        // Collect parameter names — either from user FnDecl or builtin sig
        let param_names: Vec<String>;
        if let Some(d) = def
            && d.kind == vx_resolve::DefKind::Function
        {
            let def_token = match root.token_at_offset(d.full_range.start()).right_biased() {
                Some(t) => t,
                None => continue,
            };
            let fn_node = match def_token.parent_ancestors().find_map(FnDecl::cast) {
                Some(f) => f,
                None => continue,
            };
            param_names = fn_node
                .parameters()
                .filter_map(|p| p.name().map(|t| t.text().to_string()))
                .collect();
        } else if let Some(sig) = vx_resolve::builtins::builtin_fn_sig(&callee_name) {
            param_names = sig
                .positional_params
                .iter()
                .map(|p| p.name.clone())
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

        // Look up the inferred type for this binding
        let Some(ty) = state.type_check.binding_types.get(&name_tok.text_range()) else {
            continue;
        };

        let pos = state.line_index.position(name_tok.text_range().end());
        hints.push(InlayHint {
            position: Position::new(pos.line, pos.col),
            label: InlayHintLabel::String(format!(": {ty}")),
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

#[cfg(test)]
mod tests {
    use rowan::TextSize;
    use tower_lsp::lsp_types::Range;
    use tower_lsp::lsp_types::{InlayHintLabel, Position};
    use vx_parser::LineIndex;

    use super::{DocumentState, collect_inlay_hints};

    fn hint_position(text: &str, needle: &str) -> Position {
        let byte_offset = text
            .find(needle)
            .unwrap_or_else(|| panic!("missing `{needle}`"));
        let line_index = LineIndex::new(text);
        let pos = line_index.position(TextSize::new(byte_offset as u32));
        Position::new(pos.line, pos.col)
    }

    fn text_range(text: &str, needle: &str, len: usize) -> Range {
        let byte_offset = text
            .find(needle)
            .unwrap_or_else(|| panic!("missing `{needle}`"));
        let line_index = LineIndex::new(text);
        let start = line_index.position(TextSize::new(byte_offset as u32));
        let end = line_index.position(TextSize::new((byte_offset + len) as u32));
        Range {
            start: Position::new(start.line, start.col),
            end: Position::new(end.line, end.col),
        }
    }

    fn definition_named<'a>(state: &'a DocumentState, name: &str) -> &'a vx_resolve::Definition {
        state
            .resolution
            .scopes
            .all_definitions()
            .find(|def| def.name == name)
            .unwrap_or_else(|| panic!("missing definition `{name}`"))
    }

    #[test]
    fn parameter_hints_anchor_on_effective_argument_tokens() {
        let text = r#"
fn discover_like(class: String, probe: Fn(String) -> String) {}
fn run(args: Array[String]) {}
fn executor_class(label: String) {}

fn demo(class: String, cc: String) {
  discover_like(class, (host) => host);
  run [cc, "--version"];
  executor_class("default");
}
"#;

        let state = DocumentState::new(text.to_string());
        let hints = collect_inlay_hints(&state)
            .into_iter()
            .filter_map(|hint| match hint.label {
                InlayHintLabel::String(label) => Some((label, hint.position)),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(hints.contains(&(String::from("probe:"), hint_position(text, "(host)"))));
        assert!(hints.contains(&(String::from("args:"), hint_position(text, "[cc"))));
        assert!(hints.contains(&(String::from("label:"), hint_position(text, "\"default\""))));
    }

    #[test]
    fn definition_with_origin_at_uses_reference_range_for_hovered_usages() {
        let text = r#"
fn demo() {
  let class: String = "default";
  discover(class);
}
"#;

        let state = DocumentState::new(text.to_string());
        let usage_offset = TextSize::new(text.rfind("class").unwrap() as u32);
        let (def, origin_range) = state.definition_with_origin_at(usage_offset).unwrap();

        assert_eq!(def.name, "class");
        assert_eq!(
            state.to_lsp_range(origin_range),
            text_range(text, "class);", 5)
        );
        assert_eq!(
            state.to_lsp_range(def.name_range),
            text_range(text, "class:", 5)
        );
    }

    #[test]
    fn doc_comment_for_ignores_plain_comments() {
        let text = r#"
// Minimal cc-version build sketch for the current
// resolver/evaluator lane.
fn build(ws: Path) -> Tree {
  Tree(ws)
}
"#;

        let state = DocumentState::new(text.to_string());
        let def = definition_named(&state, "build");

        assert_eq!(state.doc_comment_for(def), None);
    }

    #[test]
    fn doc_comment_for_returns_markdown_prose() {
        let text = r#"
/// Build the output tree.
///
/// Uses `ws` as the input workspace.
fn build(ws: Path) -> Tree {
  Tree(ws)
}
"#;

        let state = DocumentState::new(text.to_string());
        let def = definition_named(&state, "build");

        assert_eq!(
            state.doc_comment_for(def).as_deref(),
            Some("Build the output tree.\n\nUses `ws` as the input workspace.")
        );
    }

    #[test]
    fn inferred_def_type_uses_closure_parameter_types() {
        let text = r#"
fn build() {
  exec(["echo"]) where {
    observer: (proc, tree) => {
      proc.exit_code
    },
  };
}
"#;

        let state = DocumentState::new(text.to_string());
        let proc = definition_named(&state, "proc");
        let tree = definition_named(&state, "tree");

        assert_eq!(
            state
                .inferred_def_type(proc)
                .map(|ty| ty.display_with_env(&state.types.env).to_string()),
            Some(String::from("Process"))
        );
        assert_eq!(
            state
                .inferred_def_type(tree)
                .map(|ty| ty.display_with_env(&state.types.env).to_string()),
            Some(String::from("Tree"))
        );
    }

    #[test]
    fn documentation_for_uses_callback_slot_docs() {
        let text = r#"
fn build() {
  exec(["echo"]) where {
    observer: (proc, tree) => {
      proc.exit_code
    },
  };
}
"#;

        let state = DocumentState::new(text.to_string());
        let proc = definition_named(&state, "proc");
        let tree = definition_named(&state, "tree");

        let proc_doc = state.documentation_for(proc).unwrap();
        let tree_doc = state.documentation_for(tree).unwrap();

        assert!(proc_doc.contains("`observer` callback slot."));
        assert!(proc_doc.contains("Executor-side observer evaluated with sandbox authority."));
        assert_eq!(proc_doc, tree_doc);
    }
}
