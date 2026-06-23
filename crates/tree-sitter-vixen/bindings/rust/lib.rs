//! This crate provides Vixen language support for the [tree-sitter][] parsing library.
//!
//! [tree-sitter]: https://tree-sitter.github.io/

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_vixen() -> *const ();
}

/// Get the tree-sitter language function for this grammar.
pub const fn language() -> LanguageFn {
    unsafe { LanguageFn::from_raw(tree_sitter_vixen) }
}

/// The content of the [`node-types.json`][] file for this grammar.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");

/// The Arborium highlights query for Vixen.
pub const HIGHLIGHTS_QUERY: &str = include_str!("../../queries/highlights.scm");

/// The Arborium injections query for Vixen.
pub const INJECTIONS_QUERY: &str = include_str!("../../queries/injections.scm");

/// Vixen does not currently ship a locals query.
pub const LOCALS_QUERY: &str = "";

#[cfg(test)]
mod tests {
    #[test]
    fn test_can_load_grammar() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter::Language::new(super::language()))
            .expect("Error loading Vixen grammar");
    }
}
