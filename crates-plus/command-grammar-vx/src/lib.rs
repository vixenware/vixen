//! Load `command-model` grammars authored in Vixen (`.vx`) using only the Vixen
//! **parser** — no evaluator. Grammar files are declarative literals (`let`
//! bindings of `Grammar { … }` / `Flag { … }` struct + enum literals), so a
//! total lowering over the parsed AST is enough; the demand-driven engine is
//! never needed for the declarative core. Compatibility relations stay in the
//! introspectable tier (data / small expressions), so they lower here too; only
//! a genuinely Turing-complete reducer would ever need the real evaluator.

/// Smoke test that the pinned parser is wired in and produces a syntax tree.
/// (Real lowering to [`command_model::GrammarSet`] lands next.)
pub fn parse(src: &str) -> vx_parser::Parsed<vx_parser::SourceFile> {
    vx_parser::parse_source_file(src, vx_parser::source::SourceId(0))
}
