//! The engine's **semantic front-end** — the checked half.
//!
//! `vx-ir` produces the pure resolved front-end (parse → build → prelude →
//! type-collect → name-resolve); this composes it with the **type checker** and the
//! **error-union** pass — the phases that *force values* and therefore live with the
//! evaluator (see the wiki: "Checking is forcing; the checker lives in the engine").
//! The checked [`FrontEnd`] is what the demand-driven runtime, vx-lsp, and
//! vx-diagnostics consume; callers get the whole surface from this one module.

use std::time::Instant;

use vx_ir::capture::analyze_captures;
use vx_ir::item::ItemKind;

use crate::error_union::infer_error_unions;
use crate::type_check::type_check_vir;

// vx-ir owns the front-end *types* + the resolved (pre-check) pipeline; re-export
// them so the engine's `compile` is the single import site for the checked path.
pub use vx_ir::compile::{
    CompileError, CompileObserver, FrontEnd, NoopObserver, front_end_resolved,
    inject_implicit_stdlib_imports, inject_prelude,
};

/// The **semantic front-end**: source → a checked [`FrontEnd`]. Runs vx-ir's
/// resolved front-end, then type-checks and infers error unions here in the engine
/// (those passes force values). Pass a [`vx_package::PackageResolver`] to resolve
/// `@scope/name` packages (and their grammars) from disk.
pub fn semantic_front_end(
    source: &str,
    resolver: &dyn vx_package::PackageResolver,
    observer: &mut dyn CompileObserver,
) -> Result<FrontEnd, CompileError> {
    let (mut program, mut types) = front_end_resolved(source, resolver, observer)?;

    let t = Instant::now();
    let tc_result = type_check_vir(&mut program.arena, &mut types);
    if !tc_result.errors.is_empty() {
        return Err(CompileError::Type {
            errors: tc_result.errors.clone(),
            env: Box::new(types.env),
        });
    }
    // Replace the `?` operator's interim `Result[_, String]` with the real inferred
    // error union (a whole-call-graph fixpoint), re-type any `match` that observes a
    // `?`, and flag non-exhaustive `Ok`/`Err`.
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

/// Run the semantic front-end on `source`, returning the checked [`FrontEnd`] or
/// the first [`CompileError`]. `function_name` must name a function in the primary
/// unit (else [`CompileError::MissingEntry`]).
pub fn compile_source(source: &str, function_name: &str) -> Result<FrontEnd, CompileError> {
    compile_source_observed(source, function_name, &mut NoopObserver)
}

/// Like [`compile_source`], but reports each pipeline phase to `observer`.
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

/// [`compile_source_observed`] with an explicit package `resolver`, additionally
/// requiring that `function_name` names a function in the primary unit.
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
    use crate::type_check::type_check_vir;
    use vx_ir::build::VirBuilder;
    use vx_ir::resolve::resolve_names;
    use vx_ir::type_collect::collect_types_vir;
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

    fn semantically_analyze(source: &str) -> vx_ir::build::VirProgram {
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

    /// Run the compiler front-end (parse user + stdlib + import closure, build
    /// VIR, inject prelude, collect types, resolve names, type-check) over
    /// `source` with `resolver`, stopping before demand/lowering. Returns the
    /// program plus debug-formatted resolve and type errors.
    fn front_end(
        source: &str,
        resolver: &dyn vx_package::PackageResolver,
    ) -> (vx_ir::build::VirProgram, Vec<String>, Vec<String>) {
        use vx_ir::SourceUnitId;

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
            matches!(&e.kind, vx_ir::expr::ExprKind::StructLiteral(sl)
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
