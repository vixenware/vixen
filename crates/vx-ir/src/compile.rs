//! The **resolved** front-end: source text → a name-resolved VIR program + its
//! type collection.
//!
//! Parse → VIR → prelude → type-collect → resolve. It stops *before* type-checking:
//! checking (and the error-union pass) force values, so they live with the evaluator
//! in `vx-runtime` (see the wiki: "Checking is forcing; the checker lives in the
//! engine"). The engine's `vx_runtime::compile::semantic_front_end` composes this
//! resolved front-end with the checker to produce the checked [`FrontEnd`]. The
//! front-end *types* (`FrontEnd`, `CompileError`, `CompileObserver`) stay here, so
//! vx-lsp and vx-diagnostics share them.

use std::fmt;
use std::time::{Duration, Instant};

use vx_parser::{AstNode, SourceId};
use vx_types::TypeEnv;

use crate::build::VirBuilder;
use crate::diagnostic::IrDiagnostic;
use crate::item::ItemKind;
use crate::resolve::resolve_names;
use crate::scope::{ScopeEntry, ScopeEntryTarget};
use crate::type_collect::collect_types_vir;
use crate::type_collection::TypeCollection;
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

/// The **resolved front-end**: source → a fully built, prelude-injected,
/// type-collected, **name-resolved** VIR program + its [`TypeCollection`], with
/// grammars loaded — stopping *before* type-checking. Type-checking (and the
/// error-union pass) force values, so they live in the engine (`vx-runtime`); this
/// is the pure front-end the engine's `semantic_front_end` composes on top of. See
/// the wiki: "Checking is forcing; the checker lives in the engine".
pub fn front_end_resolved(
    source: &str,
    resolver: &dyn vx_package::PackageResolver,
    observer: &mut dyn CompileObserver,
) -> Result<(crate::build::VirProgram, TypeCollection), CompileError> {
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

    let types = collect_types_vir(&mut program.arena);
    let resolve_errors = resolve_names(&mut program.arena);
    if !resolve_errors.is_empty() {
        return Err(CompileError::Resolve {
            errors: resolve_errors,
            env: Box::new(types.env),
        });
    }
    observer.name_resolved(t.elapsed());

    Ok((program, types))
}
