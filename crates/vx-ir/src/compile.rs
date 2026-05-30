//! Shared "source text → ExecutableProgram" compilation pipeline.
//!
//! Used by vx-rt and vx-daemon to compile Vixen source text through
//! the full VIR semantic pipeline.

use std::fmt;
use std::time::{Duration, Instant};

use crate::exec_types::ExecutableProgram;
use vx_parser::{AstNode, SourceId};
use vx_types::TypeEnv;

use crate::build::VirBuilder;
use crate::cap_check::check_capabilities_vir;
use crate::capture::analyze_captures;
use crate::demand::analyze_demand_for_entry;
use crate::diagnostic::IrDiagnostic;
use crate::item::ItemKind;
use crate::lower::{LowerError, lower_vir_program};
use crate::resolve::resolve_names;
use crate::scope::{ScopeEntry, ScopeEntryTarget};
use crate::type_check::type_check_vir;
use crate::type_collect::collect_types_vir;

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
    Capability {
        errors: Vec<IrDiagnostic>,
        env: Box<TypeEnv>,
    },
    MissingEntry(String),
    Lowering(LowerError),
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "parse errors: {e:?}"),
            Self::Resolve { errors, .. } => write!(f, "resolve errors: {errors:?}"),
            Self::Type { errors, .. } => write!(f, "type errors: {errors:?}"),
            Self::Capability { errors, .. } => write!(f, "capability errors: {errors:?}"),
            Self::MissingEntry(name) => write!(f, "entry function `{name}` not found"),
            Self::Lowering(e) => write!(f, "lowering failed: {e:?}"),
        }
    }
}

impl std::error::Error for CompileError {}

pub fn inject_implicit_stdlib_imports(program: &mut crate::build::VirProgram) {
    let Some(&primary_su) = program.source_units.first() else {
        return;
    };

    let primary_module_scope = program.arena[program.arena[primary_su].top_level_body].scope;
    let imported_items = program
        .source_units
        .iter()
        .copied()
        .skip(1)
        .flat_map(|su_id| program.arena[su_id].items.iter().copied())
        .collect::<Vec<_>>();

    // Collect namespace names from imports. When stdlib exposes both an opaque
    // type alias (e.g. `type Toml;`) and a namespace with the same name
    // (`namespace Toml { ... }`), the namespace should win in the user's scope
    // so that `Toml.parse(...)` resolves to the namespace.
    let namespace_names: std::collections::HashSet<String> = imported_items
        .iter()
        .filter_map(|&id| {
            if let ItemKind::Namespace(ns) = &program.arena[id].kind {
                Some(ns.name.clone())
            } else {
                None
            }
        })
        .collect();

    for item_id in imported_items {
        // Skip stdlib type aliases that are also exposed as a namespace.
        if let ItemKind::TypeAlias(t) = &program.arena[item_id].kind
            && namespace_names.contains(&t.name)
        {
            continue;
        }
        program.arena[primary_module_scope]
            .entries
            .push(ScopeEntry {
                target: ScopeEntryTarget::Item(item_id),
                // Module scopes are order-independent, but lower decl_order keeps
                // user-defined top-level items winning on ties and later lookups.
                decl_order: 0,
            });
    }
}

/// Compile Vixen source text to an executable program.
///
/// Runs the full pipeline: parse → VIR build (with stdlib) → type collection →
/// name resolution → type checking → capability checking → capture analysis →
/// entry lookup → demand analysis → lowering.
///
/// The returned `ExecutableProgram` has `program.entry` set to the body of
/// `function_name` in the primary source unit.
/// Observes compilation-pipeline phases as they complete, with timing and
/// best-effort counts. This is the *emission seam* on the compiler side: a
/// deeply-nested phase reports through these callbacks without the compiler
/// knowing anything about how (or whether) it is surfaced. All methods default
/// to no-ops, so non-observing callers pay nothing.
pub trait CompileObserver {
    fn parsed(&mut self, _elapsed: Duration, _cst_nodes: u64, _bytes: u64) {}
    fn name_resolved(&mut self, _elapsed: Duration) {}
    fn type_checked(&mut self, _elapsed: Duration) {}
    fn capability_checked(&mut self, _elapsed: Duration) {}
    fn demand_analyzed(&mut self, _elapsed: Duration) {}
    fn lowered(&mut self, _elapsed: Duration, _bodies: u64, _kernel_ops: u64) {}
}

/// A `CompileObserver` that ignores everything.
pub struct NoopObserver;
impl CompileObserver for NoopObserver {}

pub fn compile_source(
    source: &str,
    function_name: &str,
) -> Result<ExecutableProgram, CompileError> {
    compile_source_observed(source, function_name, &mut NoopObserver)
}

/// Like [`compile_source`], but reports each pipeline phase to `observer` as it
/// completes.
pub fn compile_source_observed(
    source: &str,
    function_name: &str,
    observer: &mut dyn CompileObserver,
) -> Result<ExecutableProgram, CompileError> {
    // Parse user source — short-circuit on errors.
    let t = Instant::now();
    let parsed = vx_parser::parse_source_file(source, SourceId(0));
    if !parsed.errors().is_empty() {
        return Err(CompileError::Parse(parsed.errors().to_vec()));
    }
    let cst_nodes = parsed.tree().syntax().descendants().count() as u64;
    observer.parsed(t.elapsed(), cst_nodes, source.len() as u64);

    // Parse stdlib sources (trusted — panic on errors).
    let mut sources: Vec<(SourceId, vx_parser::SourceFile)> = vec![(SourceId(0), parsed.tree())];
    for (i, stdlib) in vx_stdlib::stdlib_sources().iter().enumerate() {
        let stdlib_id = SourceId(i as u32 + 1);
        let stdlib_parsed = vx_parser::parse_source_file(stdlib.text, stdlib_id);
        assert!(
            stdlib_parsed.errors().is_empty(),
            "stdlib source {} has parse errors: {:?}",
            stdlib.logical_path,
            stdlib_parsed.errors()
        );
        sources.push((stdlib_id, stdlib_parsed.tree()));
    }

    // Build VIR + collect types + resolve names. (Bundled under name
    // resolution — VIR construction has no separate protocol phase.)
    let t = Instant::now();
    let mut program = VirBuilder::build_program(&sources);
    inject_implicit_stdlib_imports(&mut program);
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
    observer.type_checked(t.elapsed());

    let t = Instant::now();
    let cap_result = check_capabilities_vir(&program.arena, &types, &tc_result);
    if !cap_result.errors.is_empty() {
        return Err(CompileError::Capability {
            errors: cap_result.errors,
            env: Box::new(types.env),
        });
    }
    observer.capability_checked(t.elapsed());
    analyze_captures(&mut program.arena);

    // Find entry body by name in primary source unit (first match by item order).
    let primary_su = program
        .source_units
        .first()
        .copied()
        .expect("program must have at least one source unit");
    let su = &program.arena[primary_su];
    let entry_body = su
        .items
        .iter()
        .find_map(|&item_id| {
            if let ItemKind::Function(ref f) = program.arena[item_id].kind
                && f.name == function_name
            {
                return Some(f.body);
            }
            None
        })
        .ok_or_else(|| CompileError::MissingEntry(function_name.to_owned()))?;

    // Demand analysis + lowering.
    let t = Instant::now();
    let demand = analyze_demand_for_entry(&program, entry_body);
    observer.demand_analyzed(t.elapsed());

    let t = Instant::now();
    let program = lower_vir_program(&program, &demand).map_err(CompileError::Lowering)?;
    let kernel_ops: u64 = program.bodies.values().map(|b| b.ops.len() as u64).sum();
    observer.lowered(t.elapsed(), program.bodies.len() as u64, kernel_ops);
    Ok(program)
}

#[cfg(test)]
mod tests {
    use super::inject_implicit_stdlib_imports;
    use crate::build::VirBuilder;
    use crate::cap_check::check_capabilities_vir;
    use crate::exec_types::RuntimeBoundaryKind;
    use crate::resolve::resolve_names;
    use crate::type_check::type_check_vir;
    use crate::type_collect::collect_types_vir;
    use vx_parser::SourceId;
    use vx_stdlib::stdlib_sources;

    const SOURCE_DISCOVER_WHICH: &str = r#"
fn probe() #discover -> String (
  discover(executor_class("default"), (host) => host.which("sh"))
)
"#;

    const SOURCE_EXEC_STDOUT: &str = r#"
fn probe() #discover -> String (
  let sh = discover(executor_class("default"), (host) => host.which("sh"));
  exec [sh, "-c", "printf hello"] where {
    observer: (proc, _vfs) => proc.stdout.collect()
  }
)
"#;

    const SOURCE_EXEC_WRITABLE: &str = r#"
fn probe() #discover -> String (
  let sh = discover(executor_class("default"), (host) => host.which("sh"));
  exec [sh, "-c", "printf hello > out/greeting.txt"] where {
    writable: [@out],
    create_dirs: [@out],
    observer: (proc, _vfs) => proc.stdout.collect()
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

    const SOURCE_CLI_LITERAL: &str = r#"
fn probe() -> String (
  let out = "x.o";
  let cmd = @cc[ -c main.c -o $out ];
  out
)
"#;

    #[test]
    fn cli_literal_builds_resolves_and_type_checks() {
        // Phase A: the @cc[…] literal is a real IR expression whose holes ($out)
        // resolve and type-check like any sub-expression. (The grammar check is a
        // later phase; the literal's own type is poison for now.)
        let mut program = semantically_analyze(SOURCE_CLI_LITERAL);
        let mut types = collect_types_vir(&mut program.arena);
        let resolve_errors = resolve_names(&mut program.arena);
        assert!(
            resolve_errors.is_empty(),
            "the literal's holes should resolve, got {resolve_errors:?}"
        );
        let tc_result = type_check_vir(&mut program.arena, &mut types);
        assert!(
            tc_result.errors.is_empty(),
            "the literal + holes should type-check, got {:?}",
            tc_result.errors
        );

        // The literal lowered to a real CliLiteral node with its grammar name.
        let found =
            program.arena.exprs.iter().any(
                |e| matches!(&e.kind, crate::expr::ExprKind::CliLiteral(c) if c.grammar == "cc"),
            );
        assert!(found, "expected a CliLiteral IR node for grammar `cc`");
    }

    #[test]
    fn semantic_pipeline_resolves_discover_and_executor_class_from_stdlib() {
        let mut program = semantically_analyze(SOURCE_DISCOVER_WHICH);
        let mut types = collect_types_vir(&mut program.arena);
        let resolve_errors = resolve_names(&mut program.arena);
        assert!(
            resolve_errors.is_empty(),
            "expected stdlib names to resolve, got {resolve_errors:?}"
        );
        let tc_result = type_check_vir(&mut program.arena, &mut types);
        assert!(
            tc_result.errors.is_empty(),
            "expected discover source to type-check, got {:?}",
            tc_result.errors
        );
        let cap_result = check_capabilities_vir(&program.arena, &types, &tc_result);
        assert!(
            cap_result.errors.is_empty(),
            "expected discover source to pass capability checks, got {:?}",
            cap_result.errors
        );

        let lowered = super::compile_source(SOURCE_DISCOVER_WHICH, "probe")
            .expect("discover source should now compile through lowering");
        assert!(
            lowered
                .runtime_nodes
                .values()
                .any(|node| node.kind == RuntimeBoundaryKind::Discover),
            "expected discover runtime boundary in lowered program"
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
        let cap_result = check_capabilities_vir(&program.arena, &types, &tc_result);
        assert!(
            cap_result.errors.is_empty(),
            "expected exec source to pass capability checks, got {:?}",
            cap_result.errors
        );

        let lowered = super::compile_source(SOURCE_EXEC_STDOUT, "probe")
            .expect("exec source should now compile through lowering");
        assert!(
            lowered
                .runtime_nodes
                .values()
                .any(|node| node.kind == RuntimeBoundaryKind::Exec),
            "expected exec runtime boundary in lowered program"
        );
    }

    #[test]
    fn semantic_pipeline_resolves_exec_writable_keywords_from_stdlib() {
        let lowered = super::compile_source(SOURCE_EXEC_WRITABLE, "probe")
            .expect("exec source with writable keywords should compile through lowering");
        assert!(
            lowered
                .runtime_nodes
                .values()
                .any(|node| node.kind == RuntimeBoundaryKind::Exec),
            "expected exec runtime boundary in lowered program"
        );
    }

    #[test]
    fn semantic_pipeline_eventually_lowers_runtime_boundary_kinds_when_supported() {
        let lowered = super::compile_source(
            r#"
fn probe() #discover -> String (
  discover(executor_class("default"), (host) => host.which("sh"))
)
"#,
            "probe",
        );

        if let Ok(program) = lowered {
            assert!(
                program
                    .runtime_nodes
                    .values()
                    .any(|node| node.kind == RuntimeBoundaryKind::Discover),
                "expected discover runtime boundary in lowered program"
            );
        }
    }
}
