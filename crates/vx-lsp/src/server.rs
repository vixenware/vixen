use std::collections::HashMap;
use std::sync::Mutex;

use rowan::TextRange;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};
use vx_parser::{self as vx, AstNode, Parsed, SourceFile};
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
}

impl DocumentState {
    fn new(text: String) -> Self {
        let parsed = vx::parse_source_file(&text);
        let line_index = vx::LineIndex::new(&text);
        let resolution = vx_resolve::resolve(&parsed.tree());
        Self {
            text,
            parsed,
            line_index,
            resolution,
        }
    }

    /// Find the definition under the cursor. Works whether the cursor is on
    /// a definition site or on a reference that resolves to one.
    fn definition_at(&self, offset: rowan::TextSize) -> Option<&vx_resolve::Definition> {
        // Check if cursor is on a reference
        if let Some(resolved) = self
            .resolution
            .refs
            .iter()
            .find(|r| r.ref_range.contains_inclusive(offset))
        {
            return resolved.target.as_ref().ok();
        }

        // Check if cursor is on a definition site
        self.resolution
            .scopes
            .all_definitions()
            .find(|d| d.name_range.contains_inclusive(offset))
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
        // Take up to the first line break, or the first `{`/`=` for multiline decls
        let first_line = slice.lines().next().unwrap_or(slice);
        first_line.trim().to_string()
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
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: Default::default(),
                })),
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
                                token_modifiers: vec![],
                            },
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            range: None,
                            ..Default::default()
                        },
                    ),
                ),
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
            encode_semantic_tokens(&tokens, &state.line_index)
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
            let resolved = state
                .resolution
                .refs
                .iter()
                .find(|r| r.ref_range.contains_inclusive(offset))?;

            let def = resolved.target.as_ref().ok()?;
            let range = state.to_lsp_range(def.name_range);
            Some(GotoDefinitionResponse::Scalar(Location {
                uri: uri.clone(),
                range,
            }))
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

            let def = state.definition_at(offset)?;
            let kind_label = def_kind_label(def.kind);
            let snippet = state.declaration_snippet(def);

            let markdown = format!("```vx\n{snippet}\n```\n---\n*{kind_label}*");

            Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: markdown,
                }),
                range: Some(state.to_lsp_range(def.name_range)),
            })
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

            let visible = state.resolution.scopes.visible_at(offset);
            let items: Vec<CompletionItem> = visible
                .into_iter()
                .map(|def| CompletionItem {
                    label: def.name.clone(),
                    kind: Some(def_kind_to_completion(def.kind)),
                    detail: Some(state.declaration_snippet(def)),
                    ..Default::default()
                })
                .collect();

            Some(CompletionResponse::Array(items))
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
    }
}

/// Encode semantic tokens as LSP delta-encoded 5-tuples.
fn encode_semantic_tokens(
    tokens: &[vx::SemanticToken],
    line_index: &vx::LineIndex,
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

        result.push(tower_lsp::lsp_types::SemanticToken {
            delta_line,
            delta_start,
            length: len,
            token_type: token.token_type as u32,
            token_modifiers_bitset: 0,
        });

        prev_line = start.line;
        prev_col = start.col;
    }

    result
}
