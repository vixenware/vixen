//! Name resolution: maps every name reference in the CST to its definition.

use rowan::TextRange;
use vx_parser::{AstNode, NameRefExpr, SourceFile, SyntaxKind, SyntaxNode};

use crate::scope::{Definition, ScopeTree};

/// A resolved name reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRef {
    /// Text range of the reference (the name token in the source).
    pub ref_range: TextRange,
    /// The name being referenced.
    pub name: String,
    /// The definition this reference resolves to, if found.
    pub target: Result<Definition, ResolveError>,
}

/// Why a name couldn't be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// No definition found for this name.
    Unresolved { name: String, range: TextRange },
}

/// The result of name resolution for a source file.
#[derive(Debug)]
pub struct NameResolution {
    /// The scope tree.
    pub scopes: ScopeTree,
    /// All name references and their resolution results.
    pub refs: Vec<ResolvedRef>,
    /// Just the errors.
    pub errors: Vec<ResolveError>,
}

/// Run name resolution on a parsed source file.
pub fn resolve(file: &SourceFile) -> NameResolution {
    let scopes = ScopeTree::build(file);
    let mut collector = RefCollector {
        scopes: &scopes,
        refs: Vec::new(),
        errors: Vec::new(),
    };
    collector.visit_node(file.syntax());
    let refs = collector.refs;
    let errors = collector.errors;
    NameResolution {
        scopes,
        refs,
        errors,
    }
}

struct RefCollector<'a> {
    scopes: &'a ScopeTree,
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
        let ref_range = token.text_range();

        // Dollar identifiers ($, $0, $1) are implicit closure parameters —
        // they don't resolve to named definitions in the scope tree
        if token.kind() == SyntaxKind::DOLLAR_IDENT {
            return;
        }

        let scope = self.scopes.scope_at(ref_range.start());
        let target = match self.scopes.lookup_at(&name, scope, ref_range.start()) {
            Some(def) => Ok(def.clone()),
            None => {
                let err = ResolveError::Unresolved {
                    name: name.clone(),
                    range: ref_range,
                };
                self.errors.push(err.clone());
                Err(err)
            }
        };

        self.refs.push(ResolvedRef {
            ref_range,
            name,
            target,
        });
    }
}
