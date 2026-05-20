//! Type checking pass for VIR.
//!
//! Walks the VIR arena and populates `Expr.ty`, `Binding.ty`, and `Pattern.ty`
//! directly on nodes. This replaces the old span-keyed HashMap approach in
//! `vx-resolve/src/type_check.rs`.
//!
//! ## Semantics
//!
//! - `Some(ty)`: checked, type is `ty`.
//! - `Some(Ty::Poison)`: checked but failed.
//! - `None`: not yet checked (never left behind after a full run).

pub mod calls;
pub mod closures;
pub mod collections;
pub mod control_flow;
pub mod fields;
pub mod literals;
pub mod names;
pub mod operators;
pub mod patterns;
pub mod special;
pub mod subst;

use std::collections::HashMap;

use vx_resolve::SourceSpan;
use vx_types::{Capability, FnSig, ParamDef, Ty};

use crate::arena::VirArena;
use crate::expr::ExprKind;
use crate::item::ItemKind;
use crate::type_collect::resolve_type_ref;
use crate::{BindingId, BodyId, ExprId, ItemId, SourceUnitId, TypeRefId};

use vx_resolve::Definition;
use vx_resolve::type_collect::TypeCollection;

/// A type error found during expression typing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeError {
    pub span: SourceSpan,
    pub kind: TypeErrorKind,
}

/// The kind of type error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeErrorKind {
    /// Argument type doesn't match parameter type.
    ArgTypeMismatch {
        param_name: String,
        expected: Ty,
        found: Ty,
    },
    /// Binary operator does not apply to the operand types.
    BinaryOpMismatch { op: String, left: Ty, right: Ty },
    /// Method not found on type.
    NoSuchMethod {
        receiver_ty: Ty,
        method_name: String,
    },
    /// A bare expression appears in statement position without flowing into a value.
    UnusedExprStatement,
}

/// Result of the VIR type checking pass.
///
/// Expression types and binding types are written directly onto arena nodes.
/// This struct holds the side-table outputs.
#[derive(Debug)]
pub struct TypeCheckVirResult {
    /// Real member-definition targets for field/method references.
    pub member_defs: HashMap<SourceSpan, Definition>,
    /// Documentation inherited from callback slots for closure parameters.
    pub closure_param_docs: HashMap<BindingId, String>,
    /// Type errors found during checking.
    pub errors: Vec<TypeError>,
}

/// Run VIR type checking on a fully built and name-resolved arena.
///
/// Writes `Expr.ty`, `Binding.ty`, and `Pattern.ty` directly onto arena nodes.
/// Clears all types to `None` before starting.
pub fn type_check_vir(arena: &mut VirArena, types: &TypeCollection) -> TypeCheckVirResult {
    // Clear all types up front.
    for expr in &mut arena.exprs {
        expr.ty = None;
    }
    for binding in &mut arena.bindings {
        binding.ty = None;
    }
    for pattern in &mut arena.patterns {
        pattern.ty = None;
    }

    let mut checker = TypeChecker {
        arena,
        types,
        member_defs: HashMap::new(),
        closure_param_docs: HashMap::new(),
        errors: Vec::new(),
    };

    // Walk all source units.
    let su_ids: Vec<SourceUnitId> = (0..checker.arena.source_units.len())
        .map(SourceUnitId::from_index)
        .collect();
    for su_id in su_ids {
        let body_id = checker.arena[su_id].top_level_body;
        checker.check_body(body_id);
    }

    TypeCheckVirResult {
        member_defs: checker.member_defs,
        closure_param_docs: checker.closure_param_docs,
        errors: checker.errors,
    }
}

/// The type checker state.
pub(crate) struct TypeChecker<'a> {
    pub(crate) arena: &'a mut VirArena,
    pub(crate) types: &'a TypeCollection,
    pub(crate) member_defs: HashMap<SourceSpan, Definition>,
    pub(crate) closure_param_docs: HashMap<BindingId, String>,
    pub(crate) errors: Vec<TypeError>,
}

/// Method signature after binding `self` param and substituting type params.
#[derive(Clone)]
pub(crate) struct BoundMethodSig {
    pub(crate) params: Vec<ParamDef>,
    pub(crate) ret: Ty,
    pub(crate) capabilities: Vec<Capability>,
}

impl<'a> TypeChecker<'a> {
    pub(crate) fn push_error(&mut self, error: TypeError) {
        if !self.errors.contains(&error) {
            self.errors.push(error);
        }
    }

    /// Infer the type of an expression, write it onto the node, and return it.
    pub(crate) fn infer_expr(&mut self, expr_id: ExprId) -> Ty {
        let kind = self.arena[expr_id].kind.clone();

        let ty = match kind {
            ExprKind::Literal(ref lit) => literals::infer_literal(lit),
            ExprKind::PathLiteral(_) => literals::infer_path_literal(self),
            ExprKind::InterpolatedString(ref is) => literals::infer_interpolated_string(self, is),
            ExprKind::NameRef(ref nr) => names::infer_name_ref(self, expr_id, nr),
            ExprKind::Block(ref block) => control_flow::infer_block(self, block),
            ExprKind::If(ref if_) => control_flow::infer_if(self, if_),
            ExprKind::Match(ref m) => control_flow::infer_match(self, m),
            ExprKind::Closure(ref c) => closures::infer_closure(self, c),
            ExprKind::Call(ref call) => calls::infer_call(self, expr_id, call),
            ExprKind::Where(ref w) => calls::infer_where(self, w),
            ExprKind::Invoke(ref inv) => calls::infer_invoke(self, inv),
            ExprKind::PartialApp(ref pa) => calls::infer_partial_app(self, pa),
            ExprKind::Field(ref f) => fields::infer_field(self, expr_id, f),
            ExprKind::Binary(ref bin) => operators::infer_binary(self, bin),
            ExprKind::Prefix(ref pre) => operators::infer_prefix(self, pre),
            ExprKind::Array(ref arr) => collections::infer_array(self, arr),
            ExprKind::Tuple(ref t) => collections::infer_tuple(self, t),
            ExprKind::Record(ref r) => collections::infer_record(self, r),
            ExprKind::Map(ref m) => collections::infer_map(self, m),
            ExprKind::StructLiteral(ref sl) => collections::infer_struct_literal(self, sl),
            ExprKind::With(ref w) => special::infer_with(self, w),
            ExprKind::Fallible(ref f) => special::infer_fallible(self, f),
            ExprKind::Fail(ref f) => special::infer_fail(self, f),
            ExprKind::Range(_) => special::infer_range(self, expr_id),
            ExprKind::TypeOp(ref top) => special::infer_type_op(self, top),
            ExprKind::Import(_) => Ty::Poison,
            ExprKind::Index(ref idx) => {
                self.infer_expr(idx.receiver);
                self.infer_expr(idx.index);
                Ty::Poison
            }
            ExprKind::Error => Ty::Poison,
        };

        // Always write the type. Some(Poison) = checked but failed.
        self.arena[expr_id].ty = Some(ty.clone());
        ty
    }

    /// Check a body: infer its root expression.
    pub(crate) fn check_body(&mut self, body_id: BodyId) {
        let root_expr = self.arena[body_id].root_expr;
        self.infer_expr(root_expr);
    }

    /// Check an item declaration.
    pub(crate) fn check_item(&mut self, item_id: ItemId) {
        let kind = self.arena[item_id].kind.clone();
        match kind {
            ItemKind::Function(f) => {
                self.check_body(f.body);
            }
            ItemKind::Namespace(ns) => {
                for &child_id in &ns.items {
                    self.check_item(child_id);
                }
            }
            // Struct, Enum, Newtype, TypeAlias: handled by type collection, not type checker.
            ItemKind::Struct(_)
            | ItemKind::Enum(_)
            | ItemKind::Newtype(_)
            | ItemKind::TypeAlias(_) => {}
            // Explicitly a no-op.
            ItemKind::Error => {}
        }
    }

    /// Look up a nominal type by name (e.g. "Path" → Ty::Struct/Enum/etc).
    pub(crate) fn nominal_ty(&self, name: &str) -> Ty {
        let Some(&id) = self.types.env.lookup(name).first() else {
            return Ty::Poison;
        };
        match self.types.env.get(id) {
            vx_types::TypeDef::Struct(_) => Ty::Struct(vx_types::StructTy {
                id,
                type_args: vec![],
            }),
            vx_types::TypeDef::Enum(_) => Ty::Enum(vx_types::EnumTy {
                id,
                type_args: vec![],
            }),
            vx_types::TypeDef::Newtype(_) => Ty::Newtype(vx_types::NewtypeTy {
                id,
                type_args: vec![],
            }),
            vx_types::TypeDef::Alias(_) => Ty::Alias(vx_types::AliasTy {
                id,
                type_args: vec![],
            }),
            vx_types::TypeDef::Opaque(_) => Ty::Opaque(vx_types::OpaqueTy {
                id,
                type_args: vec![],
            }),
        }
    }

    pub(crate) fn alias_target(&self, ty: &Ty) -> Option<Ty> {
        let Ty::Alias(alias_ty) = ty else {
            return None;
        };
        let vx_types::TypeDef::Alias(alias_def) = self.types.env.get(alias_ty.id) else {
            return None;
        };
        Some(subst::substitute_type_params(
            &alias_def.target,
            &alias_ty.type_args,
        ))
    }

    pub(crate) fn alias_sum_alternatives(&self, ty: &Ty) -> Option<Vec<Ty>> {
        let Ty::Alias(alias_ty) = ty else {
            return None;
        };
        let alternatives = self.types.alias_sum_alternatives.get(&alias_ty.id)?;
        Some(
            alternatives
                .iter()
                .map(|alt| subst::substitute_type_params(alt, &alias_ty.type_args))
                .collect(),
        )
    }

    pub(crate) fn types_compatible(&self, actual: &Ty, expected: &Ty) -> bool {
        if actual.is_poison() || expected.is_poison() {
            return true;
        }
        if actual == expected {
            return true;
        }
        if let Some(alternatives) = self.alias_sum_alternatives(expected) {
            return alternatives
                .iter()
                .any(|alt| self.types_compatible(actual, alt));
        }
        if let Some(alternatives) = self.alias_sum_alternatives(actual) {
            return alternatives
                .iter()
                .any(|alt| self.types_compatible(alt, expected));
        }
        if let Some(target) = self.alias_target(expected) {
            return self.types_compatible(actual, &target);
        }
        if let Some(target) = self.alias_target(actual) {
            return self.types_compatible(&target, expected);
        }

        match (actual, expected) {
            (Ty::Optional(actual_inner), Ty::Optional(expected_inner)) => {
                self.types_compatible(actual_inner, expected_inner)
            }
            (Ty::Array(actual_inner), Ty::Array(expected_inner)) => {
                self.types_compatible(actual_inner, expected_inner)
            }
            (Ty::Map(ak, av), Ty::Map(ek, ev)) => {
                self.types_compatible(ak, ek) && self.types_compatible(av, ev)
            }
            (Ty::Stream(actual_inner), Ty::Stream(expected_inner)) => {
                self.types_compatible(actual_inner, expected_inner)
            }
            (Ty::Tuple(at), Ty::Tuple(et)) => {
                at.elements.len() == et.elements.len()
                    && at
                        .elements
                        .iter()
                        .zip(et.elements.iter())
                        .all(|(a, e)| self.types_compatible(a, e))
            }
            (Ty::Function(af), Ty::Function(ef)) => {
                af.params.len() == ef.params.len()
                    && af
                        .params
                        .iter()
                        .zip(ef.params.iter())
                        .all(|(a, e)| self.types_compatible(a, e))
                    && self.types_compatible(&af.ret, &ef.ret)
            }
            _ => matches!(
                (actual, expected),
                (Ty::Infer(_), _) | (_, Ty::Infer(_)) | (Ty::Param(_), _) | (_, Ty::Param(_))
            ),
        }
    }

    /// Resolve a VIR TypeRefId to a Ty using the type collection's name_to_id.
    pub(crate) fn resolve_type_ref_id(&self, tr_id: TypeRefId) -> Ty {
        resolve_type_ref(self.arena, tr_id, &self.types.name_to_id, &[], None)
    }

    pub(crate) fn function_ty_from_sig(&self, sig: &FnSig) -> Ty {
        Ty::Function(vx_types::FunctionTy {
            params: sig.positional_params.iter().map(|p| p.ty.clone()).collect(),
            ret: Box::new(sig.ret.clone()),
            capabilities: sig.capabilities.clone(),
        })
    }
}
