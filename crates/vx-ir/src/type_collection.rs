//! `TypeCollection` — the output of type collection on the VIR.
//!
//! Stores a populated [`TypeEnv`] together with side tables that map definition
//! sites to their types or function signatures, and that record the available
//! alternatives for sum-typed aliases.

use std::collections::HashMap;

use vx_types::{FnSig, TypeDefId, TypeEnv, TypeId};

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
}

#[derive(Clone, Copy)]
pub enum TypeKind {
    Struct,
    Enum,
    Newtype,
    Alias,
    Opaque,
}
