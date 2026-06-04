//! Type checking pass for VIR.
//!
//! Walks the VIR arena and populates `Expr.ty`, `Binding.ty`, and `Pattern.ty`
//! directly on nodes. This replaces the old span-keyed HashMap approach in
//! the legacy span-keyed type check.
//!
//! ## Semantics
//!
//! - `Some(ty)`: checked, type is `ty`.
//! - `Some(Ty::Poison)`: checked but failed.
//! - `None`: not yet checked (never left behind after a full run).

pub mod calls;
pub mod cli;
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

use vx_parser::SourceSpan;
use vx_types::{ArrayTy, FnSig, MapTy, ParamDef, Ty, TypeId};

use vx_ir::arena::VirArena;
use vx_ir::def::Definition;
use vx_ir::diagnostic::IrDiagnostic;
use vx_ir::expr::{ArrayElement, CoerceExpr, ExprKind, MapElement, MapKey};
use vx_ir::item::ItemKind;
use vx_ir::type_collect::resolve_type_ref;
use vx_ir::type_collection::TypeCollection;
use vx_ir::{BindingId, BodyId, ExprId, ItemId, SourceUnitId, TypeRefId};

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
    /// Diagnostics produced during checking.
    pub errors: Vec<IrDiagnostic>,
}

/// Run VIR type checking on a fully built and name-resolved arena.
///
/// Writes `Expr.ty`, `Binding.ty`, and `Pattern.ty` directly onto arena nodes.
/// Clears all types to `None` before starting.
pub fn type_check_vir(arena: &mut VirArena, types: &mut TypeCollection) -> TypeCheckVirResult {
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

    // Build the coercion table: scan every item for `#[coerce]`-marked unary
    // functions `fn(A) -> B` and record `(A, B) -> fn item`. The checker inserts
    // an `ExprKind::Coerce` wherever a value of type `A` appears in a checking
    // position expecting `B`. We build this before constructing the checker
    // because it borrows `arena` + `types` immutably.
    let mut coercions: HashMap<(TypeId, TypeId), ItemId> = HashMap::new();
    for i in 0..arena.items.len() {
        let item_id = ItemId::from_index(i);
        let ItemKind::Function(f) = &arena[item_id].kind else {
            continue;
        };
        let is_coerce = f
            .attributes
            .iter()
            .flat_map(|a| &a.entries)
            .any(|e| e.key == "coerce");
        if !is_coerce {
            continue;
        }
        // The signature is keyed by the function's name span. A coercion fn must
        // take exactly one positional param and no keyword params; malformed ones
        // are skipped (and ignored).
        let Some(sig) = types.def_to_fn_sig.get(&f.name_span) else {
            continue;
        };
        if sig.positional_params.len() != 1 || !sig.keyword_params.is_empty() {
            continue;
        }
        let source_ty = sig.positional_params[0].ty;
        let target_ty = sig.ret;
        coercions.insert((source_ty, target_ty), item_id);
    }

    let mut checker = TypeChecker {
        arena,
        types,
        coercions,
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
    pub(crate) types: &'a mut TypeCollection,
    /// Implicit coercions declared by `#[coerce]` functions: `(source_ty,
    /// target_ty) -> coerce fn item`. Built once in [`type_check_vir`].
    pub(crate) coercions: std::collections::HashMap<(vx_types::TypeId, vx_types::TypeId), ItemId>,
    pub(crate) member_defs: HashMap<SourceSpan, Definition>,
    pub(crate) closure_param_docs: HashMap<BindingId, String>,
    pub(crate) errors: Vec<IrDiagnostic>,
}

/// Method signature after binding `self` param and substituting type params.
#[derive(Clone)]
pub(crate) struct BoundMethodSig {
    pub(crate) params: Vec<ParamDef>,
    pub(crate) ret: TypeId,
}

impl<'a> TypeChecker<'a> {
    pub(crate) fn push_diagnostic(&mut self, d: IrDiagnostic) {
        if !self.errors.contains(&d) {
            self.errors.push(d);
        }
    }

    pub(crate) fn poison(&mut self) -> TypeId {
        self.types.env.intern_poison()
    }

    pub(crate) fn unit(&mut self) -> TypeId {
        self.types.env.intern_unit()
    }

    /// Infer the type of an expression, write it onto the node, and return it.
    pub(crate) fn infer_expr(&mut self, expr_id: ExprId) -> TypeId {
        let kind = self.arena[expr_id].kind.clone();

        let ty = match kind {
            ExprKind::Literal(ref lit) => literals::infer_literal(self, lit),
            ExprKind::InterpolatedString(ref is) => literals::infer_interpolated_string(self, is),
            ExprKind::CliLiteral(ref c) => cli::infer_cli_literal(self, expr_id, c),
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
            ExprKind::Coerce(ref c) => {
                // A Coerce node is created post-inference by `coerce_to`, so its
                // target type is already set; this arm is defensive. Infer the
                // inner, then report the node's recorded target type (falling back
                // to the coerce fn's return type, then poison).
                let coerce_fn = c.coerce_fn;
                self.infer_expr(c.expr);
                self.arena[expr_id]
                    .ty
                    .or_else(|| {
                        let kind = self.arena[coerce_fn].kind.clone();
                        if let ItemKind::Function(f) = kind {
                            self.types.def_to_fn_sig.get(&f.name_span).map(|s| s.ret)
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| self.poison())
            }
            ExprKind::With(ref w) => special::infer_with(self, w),
            ExprKind::Fallible(ref f) => special::infer_fallible(self, f),
            ExprKind::Fail(ref f) => special::infer_fail(self, f),
            // r[expr.bomb] — bottom type, fits any position.
            ExprKind::Bomb => self.poison(),
            ExprKind::Range(_) => special::infer_range(self, expr_id),
            ExprKind::TypeOp(ref top) => special::infer_type_op(self, top),
            ExprKind::Import(_) => self.poison(),
            ExprKind::Index(ref idx) => {
                self.infer_expr(idx.receiver);
                self.infer_expr(idx.index);
                self.poison()
            }
            ExprKind::Error => self.poison(),
        };

        // Always write the type. Some(Poison) = checked but failed.
        self.arena[expr_id].ty = Some(ty);
        ty
    }

    /// Check a body: infer its root expression. If the body declares a return
    /// type, that's a checking-mode position for the body's value expression —
    /// coerce it (so a `fn f() -> Path ( "x" )` inserts the String→Path coercion).
    pub(crate) fn check_body(&mut self, body_id: BodyId) {
        let root_expr = self.arena[body_id].root_expr;
        self.infer_expr(root_expr);

        let Some(return_tr) = self.arena[body_id].return_ty else {
            return;
        };
        let return_ty = self.resolve_type_ref_id(return_tr);
        if self.types.env.get_ty(return_ty).is_poison() {
            return;
        }
        // Coerce the value-bearing expression: the tail of the body block, or the
        // root expression itself when the body is a single expression.
        if let Some(value_expr) = self.body_value_expr(root_expr) {
            self.coerce_to(value_expr, return_ty);
        }
    }

    /// The value-bearing expression of a body root: the tail of a block (if it has
    /// one), else the root itself. Used to find the checking-mode position for a
    /// declared return type.
    fn body_value_expr(&self, root_expr: ExprId) -> Option<ExprId> {
        match &self.arena[root_expr].kind {
            ExprKind::Block(block) => block.tail,
            _ => Some(root_expr),
        }
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

    /// Look up a nominal type by name (e.g. "Path" → Ty::Struct/Enum/etc),
    /// returning its interned TypeId.
    pub(crate) fn nominal_ty(&mut self, name: &str) -> TypeId {
        let Some(&id) = self.types.env.lookup_def(name).first() else {
            return self.poison();
        };
        match self.types.env.get_def(id) {
            vx_types::TypeDef::Struct(_) => self.types.env.intern_struct(id, vec![]),
            vx_types::TypeDef::Enum(_) => self.types.env.intern_enum(id, vec![]),
            vx_types::TypeDef::Newtype(_) => self.types.env.intern_newtype(id, vec![]),
            vx_types::TypeDef::Alias(_) => self.types.env.intern_alias(id, vec![]),
            vx_types::TypeDef::Opaque(_) => self.types.env.intern_opaque(id, vec![]),
        }
    }

    /// Resolve an alias `ty` to its target (substituted), if it is one.
    pub(crate) fn alias_target(&mut self, ty: TypeId) -> Option<TypeId> {
        let Ty::Alias(alias_ty) = self.types.env.get_ty(ty).clone() else {
            return None;
        };
        let vx_types::TypeDef::Alias(alias_def) = self.types.env.get_def(alias_ty.def) else {
            return None;
        };
        let target = alias_def.target;
        let args = alias_ty.type_args;
        Some(subst::substitute_type_params(
            target,
            &args,
            &mut self.types.env,
        ))
    }

    pub(crate) fn alias_sum_alternatives(&mut self, ty: TypeId) -> Option<Vec<TypeId>> {
        let Ty::Alias(alias_ty) = self.types.env.get_ty(ty).clone() else {
            return None;
        };
        let alternatives = self
            .types
            .alias_sum_alternatives
            .get(&alias_ty.def)?
            .clone();
        let args = alias_ty.type_args;
        Some(
            alternatives
                .into_iter()
                .map(|alt| subst::substitute_type_params(alt, &args, &mut self.types.env))
                .collect(),
        )
    }

    pub(crate) fn types_compatible(&mut self, actual: TypeId, expected: TypeId) -> bool {
        let actual_ty = self.types.env.get_ty(actual);
        let expected_ty = self.types.env.get_ty(expected);
        if actual_ty.is_poison() || expected_ty.is_poison() {
            return true;
        }
        if actual == expected {
            return true;
        }
        if let Some(alternatives) = self.alias_sum_alternatives(expected) {
            return alternatives
                .iter()
                .any(|alt| self.types_compatible(actual, *alt));
        }
        if let Some(alternatives) = self.alias_sum_alternatives(actual) {
            return alternatives
                .iter()
                .any(|alt| self.types_compatible(*alt, expected));
        }
        if let Some(target) = self.alias_target(expected) {
            return self.types_compatible(actual, target);
        }
        if let Some(target) = self.alias_target(actual) {
            return self.types_compatible(target, expected);
        }

        let actual_ty = self.types.env.get_ty(actual).clone();
        let expected_ty = self.types.env.get_ty(expected).clone();
        match (actual_ty, expected_ty) {
            (Ty::Array(a), Ty::Array(e)) => self.types_compatible(a.0, e.0),
            (Ty::Map(a), Ty::Map(e)) => {
                self.types_compatible(a.key, e.key) && self.types_compatible(a.value, e.value)
            }
            (Ty::Stream(a), Ty::Stream(e)) => self.types_compatible(a.0, e.0),
            (Ty::Tuple(at), Ty::Tuple(et)) => {
                at.0.len() == et.0.len()
                    && at
                        .0
                        .iter()
                        .zip(et.0.iter())
                        .all(|(a, e)| self.types_compatible(*a, *e))
            }
            (Ty::Function(af), Ty::Function(ef)) => {
                af.params.len() == ef.params.len()
                    && af
                        .params
                        .iter()
                        .zip(ef.params.iter())
                        .all(|(a, e)| self.types_compatible(*a, *e))
                    && self.types_compatible(af.ret, ef.ret)
            }
            (a, e) => matches!(
                (a, e),
                (Ty::Infer(_), _) | (_, Ty::Infer(_)) | (Ty::Param(_), _) | (_, Ty::Param(_))
            ),
        }
    }

    /// In checking mode: if `expr_id`'s inferred type isn't compatible with
    /// `expected` but a `#[coerce]` fn from it to `expected` exists, wrap the node
    /// in an `ExprKind::Coerce` (in place) and return true. Returns true if already
    /// compatible (no wrap). Returns false if neither compatible nor coercible.
    pub(crate) fn coerce_to(&mut self, expr_id: ExprId, expected: TypeId) -> bool {
        let actual = match self.arena[expr_id].ty {
            Some(ty) => ty,
            None => self.infer_expr(expr_id),
        };
        // Don't cascade errors through poison on either side.
        if self.types.env.get_ty(actual).is_poison() || self.types.env.get_ty(expected).is_poison()
        {
            return true;
        }
        if self.types_compatible(actual, expected) {
            return true;
        }
        if let Some(&coerce_fn) = self.coercions.get(&(actual, expected)) {
            // Wrap in place so the parent's `ExprId` reference stays valid: clone
            // the current node into a fresh `inner_id`, then overwrite this node as
            // a Coerce wrapping the inner, typed at the target.
            let inner = self.arena[expr_id].clone();
            let inner_id = self.arena.alloc_expr(inner);
            self.arena[expr_id].kind = ExprKind::Coerce(CoerceExpr {
                expr: inner_id,
                coerce_fn,
            });
            self.arena[expr_id].ty = Some(expected);
            return true;
        }
        // Structural coercion of collection literals: an array/map literal in a
        // checking position coerces element-wise into the expected collection
        // type (so `["out.txt"]` coerces to `Array[Path]` via the String→Path
        // scalar coercion on each element). Resolve aliases on `expected` first so
        // an alias for `Array[T]`/`Map[K, V]` is handled too.
        let resolved_expected = {
            let mut ty = expected;
            while let Some(target) = self.alias_target(ty) {
                ty = target;
            }
            ty
        };
        let kind = self.arena[expr_id].kind.clone();
        match (self.types.env.get_ty(resolved_expected).clone(), kind) {
            (Ty::Array(ArrayTy(elem)), ExprKind::Array(arr)) => {
                let all_ok = arr.elements.iter().all(|el| match el {
                    ArrayElement::Expr(e) => self.coerce_to(*e, elem),
                    // A spread contributes a whole `Array[elem]`.
                    ArrayElement::Spread(e) => self.coerce_to(*e, expected),
                });
                if all_ok {
                    self.arena[expr_id].ty = Some(expected);
                    return true;
                }
                false
            }
            (Ty::Map(MapTy { key, value }), ExprKind::Map(m)) => {
                let all_ok = m.entries.iter().all(|el| match el {
                    MapElement::Entry(entry) => {
                        // A `Name` key is already a `String` with no sub-expression
                        // to coerce; only `Expr` keys carry a coercible node.
                        let key_ok = match &entry.key {
                            MapKey::Expr(k) => self.coerce_to(*k, key),
                            MapKey::Name { .. } => true,
                        };
                        let value_ok = self.coerce_to(entry.value, value);
                        key_ok && value_ok
                    }
                    MapElement::Spread(e) => self.coerce_to(*e, expected),
                });
                if all_ok {
                    self.arena[expr_id].ty = Some(expected);
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    /// Resolve a VIR TypeRefId to a Ty using the type collection's name_to_id.
    pub(crate) fn resolve_type_ref_id(&mut self, tr_id: TypeRefId) -> TypeId {
        let name_to_id = self.types.name_to_id.clone();
        resolve_type_ref(
            self.arena,
            tr_id,
            &name_to_id,
            &[],
            None,
            &mut self.types.env,
        )
    }

    pub(crate) fn function_ty_from_sig(&mut self, sig: &FnSig) -> TypeId {
        let params: Vec<TypeId> = sig.positional_params.iter().map(|p| p.ty).collect();
        let ret = sig.ret;
        self.types.env.intern_function(params, ret)
    }
}
