//! Name resolution: maps every name reference in the CST to its definition.

use vx_parser::{AstNode, BinaryExpr, NameRefExpr, SourceFile, SyntaxKind, SyntaxNode};

use crate::scope::Definition;
use crate::{ImportGraph, ProgramScopes, SourceFileRef, SourceId, SourceSpan};

/// A resolved name reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRef {
    /// Text range of the reference (the name token in the source).
    pub span: SourceSpan,
    /// The name being referenced.
    pub name: String,
    /// The definition this reference resolves to, if found.
    pub target: Result<Definition, ResolveError>,
}

/// Why a name couldn't be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// No definition found for this name.
    Unresolved { name: String, span: SourceSpan },
}

/// The result of name resolution for a source file.
#[derive(Debug)]
pub struct NameResolution {
    /// Which source file these references were resolved from.
    pub source: SourceId,
    /// The scope tree.
    pub scopes: ProgramScopes,
    /// All name references and their resolution results.
    pub refs: Vec<ResolvedRef>,
    /// Just the errors.
    pub errors: Vec<ResolveError>,
}

/// Run name resolution on a parsed source file.
pub fn resolve(file: &SourceFile) -> NameResolution {
    resolve_in_program(
        SourceFileRef {
            id: SourceId(0),
            file: file.clone(),
        },
        &[SourceFileRef {
            id: SourceId(0),
            file: file.clone(),
        }],
        ImportGraph::new(),
    )
}

pub fn resolve_in_program(
    source: SourceFileRef,
    sources: &[SourceFileRef],
    imports: ImportGraph,
) -> NameResolution {
    let scopes = ProgramScopes::build(sources, imports);
    let mut collector = RefCollector {
        scopes: &scopes,
        source: source.id,
        refs: Vec::new(),
        errors: Vec::new(),
    };
    collector.visit_node(source.file.syntax());
    let refs = collector.refs;
    let errors = collector.errors;
    NameResolution {
        source: source.id,
        scopes,
        refs,
        errors,
    }
}

struct RefCollector<'a> {
    scopes: &'a ProgramScopes,
    source: SourceId,
    refs: Vec<ResolvedRef>,
    errors: Vec<ResolveError>,
}

impl<'a> RefCollector<'a> {
    fn visit_node(&mut self, node: &SyntaxNode) {
        // r[expr.ident] — resolve name references
        if let Some(name_ref) = NameRefExpr::cast(node.clone()) {
            self.resolve_name_ref(&name_ref);
        }

        for child in node.children() {
            self.visit_node(&child);
        }
    }

    fn resolve_name_ref(&mut self, name_ref: &NameRefExpr) {
        let Some(token) = name_ref.token() else {
            return;
        };

        let name = token.text().to_string();
        let ref_span = SourceSpan::new(self.source, token.text_range());

        // Dollar identifiers ($, $0, $1) and pipe placeholders (`_` in a
        // `|>` RHS) are implicit bindings — they don't resolve to named
        // definitions in the scope tree.
        if token.kind() == SyntaxKind::DOLLAR_IDENT || is_pipe_placeholder(name_ref) {
            return;
        }

        let Some(scope) = self.scopes.scope_at(self.source, ref_span.start()) else {
            return;
        };
        let target = match self
            .scopes
            .lookup_at(self.source, &name, scope, ref_span.start())
        {
            Some(def) => Ok(def.clone()),
            None => {
                let err = ResolveError::Unresolved {
                    name: name.clone(),
                    span: ref_span,
                };
                self.errors.push(err.clone());
                Err(err)
            }
        };

        self.refs.push(ResolvedRef {
            span: ref_span,
            name,
            target,
        });
    }
}

fn is_pipe_placeholder(name_ref: &NameRefExpr) -> bool {
    let Some(token) = name_ref.token() else {
        return false;
    };
    if token.kind() != SyntaxKind::UNDERSCORE {
        return false;
    }

    let placeholder_range = name_ref.syntax().text_range();
    let mut current = name_ref.syntax().clone();
    while let Some(parent) = current.parent() {
        if let Some(binary) = BinaryExpr::cast(parent.clone())
            && binary_operator_kind(&binary) == Some(SyntaxKind::PIPE_GT)
            && binary
                .rhs()
                .is_some_and(|rhs| rhs.syntax().text_range().contains_range(placeholder_range))
        {
            return true;
        }
        current = parent;
    }

    false
}

fn binary_operator_kind(binary: &BinaryExpr) -> Option<SyntaxKind> {
    binary
        .syntax()
        .children_with_tokens()
        .filter_map(|child| child.into_token())
        .find_map(|token| (token.kind() == SyntaxKind::PIPE_GT).then_some(SyntaxKind::PIPE_GT))
}
