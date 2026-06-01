//! `TypeCollection` — the output of type collection on the VIR.
//!
//! Stores a populated [`TypeEnv`] together with side tables that map definition
//! sites to their types or function signatures, and that record the available
//! alternatives for sum-typed aliases.

use std::collections::HashMap;

use vx_types::{AliasDef, EnumDef, FnSig, Ty, TypeDef, TypeDefId, TypeEnv, TypeId};

use crate::def::{DefSite, Definition};

/// Result of type collection: a populated type environment plus lookup tables.
pub struct TypeCollection {
    /// The type environment containing all type definitions and interned types.
    pub env: TypeEnv,
    /// Map from definition sites to their [`TypeDefId`] (for structs, enums,
    /// newtypes, aliases, opaques).
    pub def_to_type: HashMap<DefSite, TypeDefId>,
    /// Map from definition sites to their function signature.
    pub def_to_fn_sig: HashMap<DefSite, FnSig>,
    /// Map from type names to their [`TypeDefId`] and kind (for resolving
    /// type annotations).
    pub name_to_id: HashMap<String, (TypeDefId, TypeKind)>,
    /// Full alternative sets for sum aliases like `ExecInput = String | Path | ...`.
    pub alias_sum_alternatives: HashMap<TypeDefId, Vec<TypeId>>,
    /// Cache of inferred error unions: a canonicalized (sorted, deduped, flattened)
    /// set of error alternatives → the synthetic anonymous alias minted for it.
    /// Populated by the [`error_union`](crate::error_union) pass; keeps `A | B`
    /// and `B | A` mapping to the same alias and keeps the fixpoint finite.
    pub error_union_cache: HashMap<Vec<TypeId>, TypeDefId>,
    /// Cache of the synthetic zero-variant "Never" enum used as the error type
    /// of a `?` over an expression that can never fail (`Result[inner, Never]`).
    pub never_enum: Option<TypeDefId>,
}

impl TypeCollection {
    pub fn type_id_for_def(&self, def: &Definition) -> Option<TypeDefId> {
        self.def_to_type.get(&def.site()).copied()
    }

    pub fn fn_sig_for_def(&self, def: &Definition) -> Option<&FnSig> {
        self.def_to_fn_sig.get(&def.site())
    }

    pub fn fn_sigs(&self) -> impl Iterator<Item = &FnSig> {
        self.def_to_fn_sig.values()
    }

    /// Intern the synthetic zero-variant "Never" enum (the error type of a `?`
    /// that can never fail). Cached in [`never_enum`](Self::never_enum) so all
    /// such `?`s share one `Result[inner, Never]` shape.
    pub fn never_ty(&mut self) -> TypeId {
        let def = *self.never_enum.get_or_insert_with(|| {
            self.env.define(
                "<never>",
                TypeDef::Enum(EnumDef {
                    name: "<never>".to_owned(),
                    type_params: vec![],
                    attrs: Default::default(),
                    variants: vec![],
                }),
            )
        });
        self.env.intern_enum(def, vec![])
    }

    /// Mint a [`TypeId`] for the union of error types `alts`, the inferred `E`
    /// of a `?` operator.
    ///
    /// Canonicalizes first: any alt that is itself a sum-alias is flattened to
    /// its alternatives, the result is deduped and sorted by `TypeId.0` (so
    /// `A | B == B | A` and the fixpoint terminates). Then:
    /// - 0 alts → `Never` (a zero-variant enum), so the `?` still yields a
    ///   uniform `Result[inner, Never]` with an `Err` arm.
    /// - 1 alt → that type directly (no alias wrapper).
    /// - ≥2 alts → a synthetic anonymous `TypeDef::Alias` whose
    ///   `alias_sum_alternatives` are the canonical set; cached by that set.
    pub fn mint_union(&mut self, alts: Vec<TypeId>) -> TypeId {
        let canon = self.canonicalize_alternatives(alts);
        match canon.len() {
            0 => self.never_ty(),
            1 => canon[0],
            _ => {
                if let Some(&def) = self.error_union_cache.get(&canon) {
                    return self.env.intern_alias(def, vec![]);
                }
                let def = self.env.define(
                    "<error union>",
                    TypeDef::Alias(AliasDef {
                        name: "<error union>".to_owned(),
                        type_params: vec![],
                        target: canon[0],
                    }),
                );
                self.alias_sum_alternatives.insert(def, canon.clone());
                self.error_union_cache.insert(canon, def);
                self.env.intern_alias(def, vec![])
            }
        }
    }

    /// Flatten any sum-alias alternatives, drop poison, dedup, and sort by
    /// `TypeId.0`. Shared by [`mint_union`](Self::mint_union) and the error-union
    /// pass when it needs the alternative *set* of a possibly-union type.
    pub fn canonicalize_alternatives(&self, alts: Vec<TypeId>) -> Vec<TypeId> {
        let mut flat: Vec<TypeId> = Vec::new();
        for alt in alts {
            self.push_flattened(alt, &mut flat);
        }
        flat.retain(|t| !self.env.get_ty(*t).is_poison());
        flat.sort_by_key(|t| t.0);
        flat.dedup();
        flat
    }

    /// Push `ty` into `out`, expanding it to its alternatives if it is a
    /// sum-alias (recursively, so unions-of-unions never nest).
    fn push_flattened(&self, ty: TypeId, out: &mut Vec<TypeId>) {
        if let Ty::Alias(alias_ty) = self.env.get_ty(ty)
            && let Some(alts) = self.alias_sum_alternatives.get(&alias_ty.def)
        {
            let alts = alts.clone();
            for alt in alts {
                self.push_flattened(alt, out);
            }
            return;
        }
        out.push(ty);
    }
}

#[derive(Clone, Copy)]
pub enum TypeKind {
    Struct,
    Enum,
    Newtype,
    Alias,
    Opaque,
}
