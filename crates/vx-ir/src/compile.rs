//! The semantic front-end: source text → a checked VIR program + type env.
//!
//! Parse → VIR → prelude → type-collect → resolve → type-check
//! → capture analysis. The eager `ExecutableProgram` back-end is gone; the
//! demand-driven `vx-runtime` is the evaluator. vx-lsp and vx-diagnostics consume
//! the diagnostics this surfaces.

use std::fmt;
use std::time::{Duration, Instant};

use vx_parser::{AstNode, SourceId};
use vx_types::TypeEnv;

use crate::build::VirBuilder;
use crate::capture::analyze_captures;
use crate::diagnostic::IrDiagnostic;
use crate::error_union::infer_error_unions;
use crate::item::ItemKind;
use crate::resolve::resolve_names;
use crate::scope::{ScopeEntry, ScopeEntryTarget};
use crate::type_check::type_check_vir;
use crate::type_collect::collect_types_vir;
use crate::{ItemId, SourceUnitId};

#[derive(Debug)]
pub enum CompileError {
    Parse(Vec<vx_parser::ParseDiagnostic>),
    Resolve {
        errors: Vec<IrDiagnostic>,
        env: Box<TypeEnv>,
    },
    Type {
        errors: Vec<IrDiagnostic>,
        env: Box<TypeEnv>,
    },
    /// Diagnostics from the inferred-error-union pass (`?` exhaustiveness).
    ErrorUnion {
        errors: Vec<IrDiagnostic>,
        env: Box<TypeEnv>,
    },
    MissingEntry(String),
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "parse errors: {e:?}"),
            Self::Resolve { errors, .. } => write!(f, "resolve errors: {errors:?}"),
            Self::Type { errors, .. } => write!(f, "type errors: {errors:?}"),
            Self::ErrorUnion { errors, .. } => write!(f, "error-union errors: {errors:?}"),
            Self::MissingEntry(name) => write!(f, "entry function `{name}` not found"),
        }
    }
}

impl std::error::Error for CompileError {}

/// The output of the semantic front-end: a fully name-resolved, type-checked,
/// type-checked VIR program and its type environment. No lowering — the
/// demand-driven `vx-runtime` is the evaluator.
#[derive(Debug)]
pub struct FrontEnd {
    pub program: crate::build::VirProgram,
    pub env: TypeEnv,
}

/// Inject the **prelude** — the stdlib units' top-level items — into the module
/// scope of each `target` unit. The prelude is ambient (available un-imported)
/// everywhere, so a package module (e.g. `cc.vx`) sees `String`/`Array`/… just
/// like user code. Package *exports* are **not** injected — those are
/// import-scoped (bound where they're imported), only the prelude is global.
pub fn inject_prelude(
    program: &mut crate::build::VirProgram,
    stdlib_su_ids: &[SourceUnitId],
    targets: &[SourceUnitId],
) {
    let prelude_items: Vec<ItemId> = stdlib_su_ids
        .iter()
        .flat_map(|&su_id| program.arena[su_id].items.iter().copied())
        .collect();

    // The stdlib enums' promoted variants (`Option`'s `Some`/`None`, `Result`'s
    // `Ok`/`Err`, …) must be ambient too — bare `Ok(v)` resolves un-imported just
    // like the items above. (Promotion lands them in each declaring unit's own
    // module scope; the prelude carries them across to user units.)
    let prelude_variants: Vec<crate::scope::PromotedVariant> = stdlib_su_ids
        .iter()
        .flat_map(|&su_id| {
            let scope = program.arena[program.arena[su_id].top_level_body].scope;
            program.arena[scope].promoted_variants.clone()
        })
        .collect();

    // When the prelude exposes both an opaque type alias (`type Toml;`) and a
    // namespace with the same name (`namespace Toml { ... }`), the namespace
    // wins so `Toml.parse(...)` resolves to it.
    let namespace_names: std::collections::HashSet<String> = prelude_items
        .iter()
        .filter_map(|&id| match &program.arena[id].kind {
            ItemKind::Namespace(ns) => Some(ns.name.clone()),
            _ => None,
        })
        .collect();

    for &target in targets {
        let module_scope = program.arena[program.arena[target].top_level_body].scope;
        for &item_id in &prelude_items {
            if let ItemKind::TypeAlias(t) = &program.arena[item_id].kind
                && namespace_names.contains(&t.name)
            {
                continue;
            }
            program.arena[module_scope].entries.push(ScopeEntry {
                target: ScopeEntryTarget::Item(item_id),
                decl_order: 0,
            });
        }
        for pv in &prelude_variants {
            program.arena[module_scope]
                .promoted_variants
                .push(pv.clone());
        }
    }
}

/// Back-compat shim: inject every non-primary unit (all stdlib, no packages)
/// into the primary module scope. Used by callers/tests that compile a single
/// module with no package imports.
pub fn inject_implicit_stdlib_imports(program: &mut crate::build::VirProgram) {
    if program.source_units.is_empty() {
        return;
    }
    let primary = program.source_units[0];
    let stdlib: Vec<SourceUnitId> = program.source_units.iter().copied().skip(1).collect();
    inject_prelude(program, &stdlib, &[primary]);
}

/// Observes semantic-pipeline phases as they complete, with timing and
/// best-effort counts. The *emission seam* on the compiler side: a deeply-nested
/// phase reports through these callbacks without the compiler knowing how (or
/// whether) it is surfaced. All methods default to no-ops.
pub trait CompileObserver {
    fn parsed(&mut self, _elapsed: Duration, _cst_nodes: u64, _bytes: u64) {}
    fn name_resolved(&mut self, _elapsed: Duration) {}
    fn type_checked(&mut self, _elapsed: Duration) {}
}

/// A `CompileObserver` that ignores everything.
pub struct NoopObserver;
impl CompileObserver for NoopObserver {}

/// Run the semantic front-end on `source`, returning the checked [`FrontEnd`] or
/// the first [`CompileError`]. `function_name` must name a function in the primary
/// unit (else [`CompileError::MissingEntry`]).
pub fn compile_source(source: &str, function_name: &str) -> Result<FrontEnd, CompileError> {
    compile_source_observed(source, function_name, &mut NoopObserver)
}

/// Like [`compile_source`], but reports each pipeline phase to `observer` as it
/// completes. Uses bundled built-ins only for `import`s.
pub fn compile_source_observed(
    source: &str,
    function_name: &str,
    observer: &mut dyn CompileObserver,
) -> Result<FrontEnd, CompileError> {
    compile_source_observed_with_resolver(
        source,
        function_name,
        &vx_package::BundledResolver,
        observer,
    )
}

/// The **semantic front-end**: source → a fully resolved, type-checked,
/// type-checked VIR program + type env, with grammars loaded — *no* entry
/// check (that's [`compile_source_observed_with_resolver`]). This is the one
/// pipeline the demand-driven runtime (`vx-runtime::analyze`) drives; vx-lsp and
/// vx-diagnostics ride the same passes. Pass a [`vx_package::CompositeResolver`]
/// to resolve `@scope/name` packages (and their grammars) from disk.
pub fn semantic_front_end(
    source: &str,
    resolver: &dyn vx_package::PackageResolver,
    observer: &mut dyn CompileObserver,
) -> Result<FrontEnd, CompileError> {
    // Parse user source — short-circuit on errors.
    let t = Instant::now();
    let parsed = vx_parser::parse_source_file(source, SourceId(0));
    if !parsed.errors().is_empty() {
        return Err(CompileError::Parse(parsed.errors().to_vec()));
    }
    let cst_nodes = parsed.tree().syntax().descendants().count() as u64;
    observer.parsed(t.elapsed(), cst_nodes, source.len() as u64);

    let mut sources: Vec<(SourceId, vx_parser::SourceFile)> = vec![(SourceId(0), parsed.tree())];
    let mut next_id: u32 = 1;

    // The prelude: stdlib sources, parsed and bundled (trusted — panic on error).
    let mut stdlib_ids: Vec<SourceId> = Vec::new();
    for stdlib in vx_stdlib::stdlib_sources() {
        let id = SourceId(next_id);
        next_id += 1;
        let stdlib_parsed = vx_parser::parse_source_file(stdlib.text, id);
        assert!(
            stdlib_parsed.errors().is_empty(),
            "stdlib source {} has parse errors: {:?}",
            stdlib.logical_path,
            stdlib_parsed.errors()
        );
        sources.push((id, stdlib_parsed.tree()));
        stdlib_ids.push(id);
    }

    // The import closure: every package reachable from the user module via
    // `import`, parsed and added as a source unit. Unresolved imports surface
    // during name resolution (the spec simply won't be in the import map).
    let closure = vx_package::resolve_closure(source, resolver);
    let mut import_specs: Vec<(String, SourceId)> = Vec::new();
    for pkg in &closure.packages {
        let id = SourceId(next_id);
        next_id += 1;
        let pkg_parsed = vx_parser::parse_source_file(&pkg.main, id);
        if !pkg_parsed.errors().is_empty() {
            return Err(CompileError::Parse(pkg_parsed.errors().to_vec()));
        }
        sources.push((id, pkg_parsed.tree()));
        import_specs.push((pkg.id.clone(), id));
    }

    // Build VIR over the whole module graph.
    let t = Instant::now();
    let mut program = VirBuilder::build_program(&sources);

    // Record the import map (spec → source unit) for cross-module resolution.
    for (spec, sid) in &import_specs {
        if let Some((su_id, _)) = program.arena.source_unit_for(*sid) {
            program.arena.imports.insert(spec.clone(), su_id);
        }
    }

    // Load any grammar packages into GrammarSets (keyed by import spec) so the
    // CLI-literal check can validate `@grammar[ … ]` against them. A package
    // that defines no grammars yields an empty set — harmless.
    for pkg in &closure.packages {
        if let Ok(set) = command_grammar_vx::load_str(&pkg.main) {
            program.arena.grammars.insert(pkg.id.clone(), set);
        }
    }

    // The prelude is ambient in every non-stdlib unit (user module + packages);
    // package exports are import-scoped, not injected.
    let stdlib_su_ids: Vec<SourceUnitId> = stdlib_ids
        .iter()
        .filter_map(|&sid| program.arena.source_unit_for(sid).map(|(su, _)| su))
        .collect();
    let prelude_targets: Vec<SourceUnitId> = program
        .source_units
        .iter()
        .copied()
        .filter(|su| !stdlib_su_ids.contains(su))
        .collect();
    inject_prelude(&mut program, &stdlib_su_ids, &prelude_targets);

    let mut types = collect_types_vir(&mut program.arena);
    let resolve_errors = resolve_names(&mut program.arena);
    if !resolve_errors.is_empty() {
        return Err(CompileError::Resolve {
            errors: resolve_errors,
            env: Box::new(types.env),
        });
    }
    observer.name_resolved(t.elapsed());

    let t = Instant::now();
    let tc_result = type_check_vir(&mut program.arena, &mut types);
    if !tc_result.errors.is_empty() {
        return Err(CompileError::Type {
            errors: tc_result.errors.clone(),
            env: Box::new(types.env),
        });
    }
    // Replace the `?` operator's interim `Result[_, String]` with the real
    // inferred error union (a whole-call-graph fixpoint), re-type any `match`
    // that observes a `?`, and flag non-exhaustive `Ok`/`Err`. Runs here so the
    // demand-driven runtime, vx-lsp, and vx-diagnostics all see inferred unions.
    let eu_errors = infer_error_unions(&mut program.arena, &mut types, &tc_result);
    if !eu_errors.is_empty() {
        return Err(CompileError::ErrorUnion {
            errors: eu_errors,
            env: Box::new(types.env),
        });
    }
    observer.type_checked(t.elapsed());

    analyze_captures(&mut program.arena);

    Ok(FrontEnd {
        program,
        env: types.env,
    })
}

/// [`semantic_front_end`] with the bundled resolver and no phase observation —
/// what the demand-driven runtime's `analyze` (and most callers) want.
pub fn semantic_front_end_bundled(source: &str) -> Result<FrontEnd, CompileError> {
    semantic_front_end(source, &vx_package::BundledResolver, &mut NoopObserver)
}

/// Like [`compile_source_observed`], with an explicit package `resolver`. Runs the
/// [`semantic_front_end`] and additionally requires that `function_name` names a
/// function in the primary unit (else [`CompileError::MissingEntry`]).
pub fn compile_source_observed_with_resolver(
    source: &str,
    function_name: &str,
    resolver: &dyn vx_package::PackageResolver,
    observer: &mut dyn CompileObserver,
) -> Result<FrontEnd, CompileError> {
    let fe = semantic_front_end(source, resolver, observer)?;
    let primary_su = fe
        .program
        .source_units
        .first()
        .copied()
        .expect("program must have at least one source unit");
    let entry_exists = fe.program.arena[primary_su].items.iter().any(|&item_id| {
        matches!(&fe.program.arena[item_id].kind, ItemKind::Function(f) if f.name == function_name)
    });
    if !entry_exists {
        return Err(CompileError::MissingEntry(function_name.to_owned()));
    }
    Ok(fe)
}

#[cfg(test)]
mod tests {
    use super::inject_implicit_stdlib_imports;
    use crate::build::VirBuilder;
    use crate::resolve::resolve_names;
    use crate::type_check::type_check_vir;
    use crate::type_collect::collect_types_vir;
    use vx_parser::SourceId;
    use vx_stdlib::stdlib_sources;

    const SOURCE_EXEC_STDOUT: &str = r#"
fn probe(cmd: Command) -> Bytes (
  exec(cmd) with {
    observer: (run) => run.ok.stdout
  }
)
"#;

    const SOURCE_EXEC_WRITABLE: &str = r#"
fn probe(cmd: Command) -> Tree (
  exec(cmd) with {
    writable: ["out"],
    create_dirs: ["out"],
    observer: (run) => run.ok.out
  }
)
"#;

    fn semantically_analyze(source: &str) -> crate::build::VirProgram {
        let parsed = vx_parser::parse_source_file(source, SourceId(0));
        assert!(parsed.errors().is_empty(), "source should parse");

        let mut sources = vec![(SourceId(0), parsed.tree())];
        for (i, stdlib) in stdlib_sources().iter().enumerate() {
            let stdlib_id = SourceId(i as u32 + 1);
            let stdlib_parsed = vx_parser::parse_source_file(stdlib.text, stdlib_id);
            assert!(stdlib_parsed.errors().is_empty(), "stdlib should parse");
            sources.push((stdlib_id, stdlib_parsed.tree()));
        }

        let mut program = VirBuilder::build_program(&sources);
        inject_implicit_stdlib_imports(&mut program);
        program
    }

    /// A resolver that serves the bundled `vixen:*` modules plus the real
    /// on-disk `@vixen/*` packages under the repo's `packages/` directory.
    fn packages_resolver() -> vx_package::CompositeResolver {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages");
        vx_package::CompositeResolver::new(root)
    }

    /// Build a user module that imports the cc grammar and uses `body` (a CLI
    /// literal source) inside `probe`. `out` is a `Path` in scope (so a `$out`
    /// hole at a path position type-checks).
    fn cc_user_module(body: &str) -> String {
        format!(
            "fn probe() -> Bool (\n  let {{ cc }} = import \"@vixen/grammar-cc\";\n  let out: Path = \"out\";\n  let _cmd = {body};\n  true\n)\n"
        )
    }

    fn cli_diags(tc_errors: &[String]) -> Vec<&String> {
        tc_errors.iter().filter(|e| e.starts_with("Cli ")).collect()
    }

    #[test]
    fn cli_literal_valid_command_checks_clean() {
        // A well-formed `@cc[…]` against the imported cc grammar: `-c` enters
        // compile mode, `main.c` is the source positional, `-o $out` writes the
        // output (the hole is a real, checked sub-expression). No diagnostics.
        let source = cc_user_module("@cc[ -c main.c -o $out ]");
        let (program, resolve_errors, tc_errors) = front_end(&source, &packages_resolver());
        assert!(
            resolve_errors.is_empty(),
            "should resolve (incl. the $out hole), got {resolve_errors:#?}"
        );
        assert!(
            tc_errors.is_empty(),
            "a valid command should check clean, got {tc_errors:#?}"
        );
        let found =
            program.arena.exprs.iter().any(
                |e| matches!(&e.kind, crate::expr::ExprKind::CliLiteral(c) if c.grammar == "cc"),
            );
        assert!(found, "expected a CliLiteral IR node for grammar `cc`");
    }

    #[test]
    fn cli_literal_reports_unknown_flag() {
        // `-Q` matches no flag in the cc grammar.
        let source = cc_user_module("@cc[ -Q ]");
        let (_program, _resolve, tc_errors) = front_end(&source, &packages_resolver());
        let diags = cli_diags(&tc_errors);
        assert!(
            diags
                .iter()
                .any(|d| d.contains("UnknownFlag") && d.contains("-Q")),
            "expected an UnknownFlag diagnostic for -Q, got {tc_errors:#?}"
        );
    }

    #[test]
    fn cli_literal_reports_value_not_allowed() {
        // `-std` is `OneOf {c99, c11, c17, c23}`; `cpp` is not a member.
        let source = cc_user_module("@cc[ -std=cpp ]");
        let (_program, _resolve, tc_errors) = front_end(&source, &packages_resolver());
        let diags = cli_diags(&tc_errors);
        assert!(
            diags
                .iter()
                .any(|d| d.contains("ValueNotAllowed") && d.contains("cpp")),
            "expected a ValueNotAllowed diagnostic for -std=cpp, got {tc_errors:#?}"
        );
    }

    #[test]
    fn cli_literal_reports_illegal_in_mode() {
        // `-c` selects compile mode; `-shared` is legal only in link mode.
        let source = cc_user_module("@cc[ -c -shared ]");
        let (_program, _resolve, tc_errors) = front_end(&source, &packages_resolver());
        let diags = cli_diags(&tc_errors);
        assert!(
            diags
                .iter()
                .any(|d| d.contains("IllegalInMode") && d.contains("-shared")),
            "expected an IllegalInMode diagnostic for -shared, got {tc_errors:#?}"
        );
    }

    #[test]
    fn cli_literal_hole_typed_by_position() {
        // `-o` is a path position; the grammar makes `$out` demand a `Path`.
        // A `String` bound to `out` and spliced into the `$out` hole is a
        // HoleType error — the CLI-hole check is exact, not a coercion site.
        let source = "fn probe() -> Bool (\n  let { cc } = import \"@vixen/grammar-cc\";\n  let out = \"a.out\";\n  let _cmd = @cc[ -o $out ];\n  true\n)\n";
        let (_program, resolve_errors, tc_errors) = front_end(source, &packages_resolver());
        assert!(
            resolve_errors.is_empty(),
            "should resolve, got {resolve_errors:#?}"
        );
        let diags = cli_diags(&tc_errors);
        assert!(
            diags.iter().any(|d| d.contains("HoleType")),
            "expected a HoleType diagnostic for a String at a path position, got {tc_errors:#?}"
        );
    }

    #[test]
    fn cli_literal_unknown_grammar_is_an_error() {
        // `@nope[…]` with no grammar named `nope` in scope is a hard error.
        let source = "fn probe() -> Bool (\n  let _cmd = @nope[ -x ];\n  true\n)\n";
        let (_program, _resolve, tc_errors) = front_end(source, &vx_package::BundledResolver);
        let diags = cli_diags(&tc_errors);
        assert!(
            diags
                .iter()
                .any(|d| d.contains("UnknownGrammar") && d.contains("nope")),
            "expected an UnknownGrammar diagnostic, got {tc_errors:#?}"
        );
    }

    /// Run the compiler front-end (parse user + stdlib + import closure, build
    /// VIR, inject prelude, collect types, resolve names, type-check) over
    /// `source` with `resolver`, stopping before demand/lowering. Returns the
    /// program plus debug-formatted resolve and type errors.
    fn front_end(
        source: &str,
        resolver: &dyn vx_package::PackageResolver,
    ) -> (crate::build::VirProgram, Vec<String>, Vec<String>) {
        use crate::SourceUnitId;

        let parsed = vx_parser::parse_source_file(source, SourceId(0));
        assert!(parsed.errors().is_empty(), "user source should parse");
        let mut sources = vec![(SourceId(0), parsed.tree())];
        let mut next_id: u32 = 1;

        let mut stdlib_ids = Vec::new();
        for stdlib in stdlib_sources() {
            let id = SourceId(next_id);
            next_id += 1;
            let p = vx_parser::parse_source_file(stdlib.text, id);
            assert!(p.errors().is_empty(), "stdlib should parse");
            sources.push((id, p.tree()));
            stdlib_ids.push(id);
        }

        let closure = vx_package::resolve_closure(source, resolver);
        assert!(
            closure.errors.is_empty(),
            "import closure should resolve cleanly, got {:?}",
            closure.errors
        );
        let mut import_specs = Vec::new();
        for pkg in &closure.packages {
            let id = SourceId(next_id);
            next_id += 1;
            let p = vx_parser::parse_source_file(&pkg.main, id);
            assert!(
                p.errors().is_empty(),
                "package {} should parse, got {:?}",
                pkg.id,
                p.errors()
            );
            sources.push((id, p.tree()));
            import_specs.push((pkg.id.clone(), id));
        }

        let mut program = VirBuilder::build_program(&sources);
        for (spec, sid) in &import_specs {
            if let Some((su_id, _)) = program.arena.source_unit_for(*sid) {
                program.arena.imports.insert(spec.clone(), su_id);
            }
        }
        for pkg in &closure.packages {
            if let Ok(set) = command_grammar_vx::load_str(&pkg.main) {
                program.arena.grammars.insert(pkg.id.clone(), set);
            }
        }
        let stdlib_su_ids: Vec<SourceUnitId> = stdlib_ids
            .iter()
            .filter_map(|&sid| program.arena.source_unit_for(sid).map(|(su, _)| su))
            .collect();
        let prelude_targets: Vec<SourceUnitId> = program
            .source_units
            .iter()
            .copied()
            .filter(|su| !stdlib_su_ids.contains(su))
            .collect();
        super::inject_prelude(&mut program, &stdlib_su_ids, &prelude_targets);

        let mut types = collect_types_vir(&mut program.arena);
        let resolve_errors = resolve_names(&mut program.arena)
            .iter()
            .map(|e| format!("{e:?}"))
            .collect();
        let tc_result = type_check_vir(&mut program.arena, &mut types);
        let mut tc_errors: Vec<String> =
            tc_result.errors.iter().map(|e| format!("{e:?}")).collect();
        let eu_errors = super::infer_error_unions(&mut program.arena, &mut types, &tc_result);
        tc_errors.extend(eu_errors.iter().map(|e| format!("{e:?}")));
        (program, resolve_errors, tc_errors)
    }

    #[test]
    fn enum_variant_struct_construction_resolves_and_types() {
        // `Enum.Variant { field: value }` (struct-payload variant construction)
        // resolves to the enum + variant index and types as the enum.
        let src = "enum Kind { struct Text, struct Input { dir: Bool } }\nfn p() -> Kind ( Kind.Input { dir: false } )\n";
        let mut program = semantically_analyze(src);
        let mut types = collect_types_vir(&mut program.arena);
        let resolve_errors = resolve_names(&mut program.arena);
        assert!(
            resolve_errors.is_empty(),
            "variant construction should resolve, got {resolve_errors:?}"
        );
        let tc = type_check_vir(&mut program.arena, &mut types);
        assert!(
            tc.errors.is_empty(),
            "variant construction should type-check, got {:?}",
            tc.errors
        );
        // The literal resolved to an enum item + a variant index.
        let ok = program.arena.exprs.iter().any(|e| {
            matches!(&e.kind, crate::expr::ExprKind::StructLiteral(sl)
                if sl.variant.as_deref() == Some("Input") && sl.resolved_variant == Some(1))
        });
        assert!(ok, "expected resolved Kind.Input variant (index 1)");
    }

    #[test]
    fn whole_module_import_is_a_record_value() {
        // `let m = import "…"` binds the whole module as a value; `m.Export`
        // projects an export. A module is a record — both destructure and
        // field access work. Here we project a type export and a struct export.
        let source = r#"
fn probe() -> Bool (
  let m = import "vixen:command-grammar";
  let _grammar = m.Grammar;
  let _spelling = m.Spelling;
  true
)
"#;
        let (program, resolve_errors, tc_errors) = front_end(source, &vx_package::BundledResolver);
        assert!(
            resolve_errors.is_empty(),
            "whole-module import should resolve, got {resolve_errors:#?}"
        );
        assert!(
            tc_errors.is_empty(),
            "module-record field access should type-check, got {tc_errors:#?}"
        );
        // The `m` binding carries whole-module provenance (export == None).
        let has_whole_module = program.arena.bindings.iter().any(|b| {
            b.name == "m"
                && matches!(&b.import_origin, Some(o) if o.export.is_none()
                    && o.spec == "vixen:command-grammar")
        });
        assert!(has_whole_module, "expected a whole-module binding for `m`");
    }

    #[test]
    fn grammar_cc_package_genuinely_compiles() {
        // The real @vixen/grammar-cc package imports `vixen:command-grammar`,
        // destructures 11 of its types, and builds Grammar/Flag/Spelling.Bare/
        // Policy.Group{…} literals against them. Compiling it as the entry
        // module proves cross-module name resolution + type-checking: the
        // imported `Grammar` struct name and `Spelling`/`Policy`/`Kind` enums
        // resolve to command-grammar's items, across the unit boundary.
        let source = include_str!("../../../packages/@vixen/grammar-cc/main.vx");
        let (_program, resolve_errors, tc_errors) = front_end(source, &vx_package::BundledResolver);
        assert!(
            resolve_errors.is_empty(),
            "grammar-cc names should resolve across the import, got {resolve_errors:#?}"
        );
        assert!(
            tc_errors.is_empty(),
            "grammar-cc should type-check against the imported vocabulary, got {tc_errors:#?}"
        );
    }

    #[test]
    fn semantic_pipeline_resolves_exec_and_observer_from_stdlib() {
        let mut program = semantically_analyze(SOURCE_EXEC_STDOUT);
        let mut types = collect_types_vir(&mut program.arena);
        let resolve_errors = resolve_names(&mut program.arena);
        assert!(
            resolve_errors.is_empty(),
            "expected stdlib names to resolve, got {resolve_errors:?}"
        );
        let tc_result = type_check_vir(&mut program.arena, &mut types);
        assert!(
            tc_result.errors.is_empty(),
            "expected exec source to type-check, got {:?}",
            tc_result.errors
        );
        super::compile_source(SOURCE_EXEC_STDOUT, "probe")
            .expect("exec source should compile through the front-end");
    }

    #[test]
    fn semantic_pipeline_resolves_exec_writable_keywords_from_stdlib() {
        super::compile_source(SOURCE_EXEC_WRITABLE, "probe")
            .expect("exec source with writable keywords should compile through the front-end");
    }
}
