//! Shared "source text → ExecutableProgram" compilation pipeline.
//!
//! Used by vx-rt and vx-daemon to compile Vixen source text through
//! the full VIR semantic pipeline.

use std::fmt;

use crate::exec_types::ExecutableProgram;
use vx_resolve::SourceId;

use crate::build::VirBuilder;
use crate::cap_check::check_capabilities_vir;
use crate::capture::analyze_captures;
use crate::demand::analyze_demand_for_entry;
use crate::item::ItemKind;
use crate::lower::{LowerError, LoweringOutput, lower_vir_program};
use crate::resolve::resolve_names;
use crate::type_check::type_check_vir;
use crate::type_collect::collect_types_vir;

#[derive(Debug)]
pub enum CompileError {
    Parse(Vec<vx_parser::SyntaxError>),
    Resolve(Vec<crate::resolve::ResolveError>),
    Type(Vec<crate::type_check::TypeError>),
    Capability(Vec<vx_resolve::CapError>),
    MissingEntry(String),
    Lowering(LowerError),
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "parse errors: {e:?}"),
            Self::Resolve(e) => write!(f, "resolve errors: {e:?}"),
            Self::Type(e) => write!(f, "type errors: {e:?}"),
            Self::Capability(e) => write!(f, "capability errors: {e:?}"),
            Self::MissingEntry(name) => write!(f, "entry function `{name}` not found"),
            Self::Lowering(e) => write!(f, "lowering failed: {e:?}"),
        }
    }
}

impl std::error::Error for CompileError {}

/// Compile Vixen source text to an executable program.
///
/// Runs the full pipeline: parse → VIR build (with stdlib) → type collection →
/// name resolution → type checking → capability checking → capture analysis →
/// entry lookup → demand analysis → lowering.
///
/// The returned `ExecutableProgram` has `program.entry` set to the body of
/// `function_name` in the primary source unit.
pub fn compile_source(
    source: &str,
    function_name: &str,
) -> Result<ExecutableProgram, CompileError> {
    // Parse user source — short-circuit on errors.
    let parsed = vx_parser::parse_source_file(source);
    if !parsed.errors().is_empty() {
        return Err(CompileError::Parse(parsed.errors().to_vec()));
    }

    // Parse stdlib sources (trusted — panic on errors).
    let mut sources: Vec<(SourceId, vx_parser::SourceFile)> = vec![(SourceId(0), parsed.tree())];
    for (i, stdlib) in vx_resolve::stdlib_sources().iter().enumerate() {
        let stdlib_parsed = vx_parser::parse_source_file(stdlib.text);
        assert!(
            stdlib_parsed.errors().is_empty(),
            "stdlib source {} has parse errors: {:?}",
            stdlib.logical_path,
            stdlib_parsed.errors()
        );
        sources.push((SourceId(i as u32 + 1), stdlib_parsed.tree()));
    }

    // Build VIR.
    let mut program = VirBuilder::build_program(&sources);

    // Semantic passes.
    let types = collect_types_vir(&mut program.arena);
    let resolve_errors = resolve_names(&mut program.arena);
    if !resolve_errors.is_empty() {
        return Err(CompileError::Resolve(resolve_errors));
    }
    let tc_result = type_check_vir(&mut program.arena, &types);
    if !tc_result.errors.is_empty() {
        return Err(CompileError::Type(tc_result.errors.clone()));
    }
    let cap_result = check_capabilities_vir(&program.arena, &types, &tc_result);
    if !cap_result.errors.is_empty() {
        return Err(CompileError::Capability(cap_result.errors));
    }
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
            if let ItemKind::Function(ref f) = program.arena[item_id].kind {
                if f.name == function_name {
                    return Some(f.body);
                }
            }
            None
        })
        .ok_or_else(|| CompileError::MissingEntry(function_name.to_owned()))?;

    // Demand analysis + lowering.
    let demand = analyze_demand_for_entry(&program, entry_body);
    let output = lower_vir_program(&program, &demand).map_err(CompileError::Lowering)?;
    Ok(output.program)
}

/// Like [`compile_source`], but also returns slot type information and
/// original body count for strict-lowering consumers (vx-jit).
pub fn compile_source_with_types(
    source: &str,
    function_name: &str,
) -> Result<LoweringOutput, CompileError> {
    // Parse user source — short-circuit on errors.
    let parsed = vx_parser::parse_source_file(source);
    if !parsed.errors().is_empty() {
        return Err(CompileError::Parse(parsed.errors().to_vec()));
    }

    // Parse stdlib sources (trusted — panic on errors).
    let mut sources: Vec<(SourceId, vx_parser::SourceFile)> = vec![(SourceId(0), parsed.tree())];
    for (i, stdlib) in vx_resolve::stdlib_sources().iter().enumerate() {
        let stdlib_parsed = vx_parser::parse_source_file(stdlib.text);
        assert!(
            stdlib_parsed.errors().is_empty(),
            "stdlib source {} has parse errors: {:?}",
            stdlib.logical_path,
            stdlib_parsed.errors()
        );
        sources.push((SourceId(i as u32 + 1), stdlib_parsed.tree()));
    }

    // Build VIR.
    let mut program = VirBuilder::build_program(&sources);

    // Semantic passes.
    let types = collect_types_vir(&mut program.arena);
    let resolve_errors = resolve_names(&mut program.arena);
    if !resolve_errors.is_empty() {
        return Err(CompileError::Resolve(resolve_errors));
    }
    let tc_result = type_check_vir(&mut program.arena, &types);
    if !tc_result.errors.is_empty() {
        return Err(CompileError::Type(tc_result.errors.clone()));
    }
    let cap_result = check_capabilities_vir(&program.arena, &types, &tc_result);
    if !cap_result.errors.is_empty() {
        return Err(CompileError::Capability(cap_result.errors));
    }
    analyze_captures(&mut program.arena);

    // Find entry body.
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
            if let ItemKind::Function(ref f) = program.arena[item_id].kind {
                if f.name == function_name {
                    return Some(f.body);
                }
            }
            None
        })
        .ok_or_else(|| CompileError::MissingEntry(function_name.to_owned()))?;

    // Demand analysis + lowering.
    let demand = analyze_demand_for_entry(&program, entry_body);
    lower_vir_program(&program, &demand).map_err(CompileError::Lowering)
}
