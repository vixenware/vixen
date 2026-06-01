//! Type representation for the Vixen language.
//!
//! This crate defines the data structures that represent Vixen's type system.
//! It is used by the type checker and evaluator stack.
//!
//! ## Identifiers
//!
//! - [`TypeDefId`] indexes into the `defs` table of [`TypeEnv`] — i.e. nominal
//!   declarations like structs, enums, newtypes, aliases, and opaques.
//! - [`TypeId`] indexes into the `types` table of [`TypeEnv`] — i.e. every
//!   interned [`Ty`] (anonymous or nominal), so the entire type graph is
//!   addressable by id.

mod display;

use std::collections::HashMap;

use vx_parser::SourceSpan;

/// A unique identifier for a *type definition* — a struct, enum, newtype,
/// alias, or opaque declaration.
///
/// These are assigned during type collection — the pass that walks the AST
/// and registers all type declarations before checking begins.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TypeDefId(pub u32);

/// A unique identifier for a [`Ty`] interned in a [`TypeEnv`].
///
/// Every type (scalar, unit, struct instantiation, function, tuple, array,
/// inference variable, …) is interned and addressable by `TypeId`. Two
/// `TypeId`s compare equal iff they refer to the same structural type.
///
/// `Ord` is by interning order (the raw `u32`) — a stable, arbitrary total
/// order used to canonicalize inferred error unions deterministically.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct TypeId(pub u32);

/// A unique identifier for a type variable introduced during inference.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct InferVar(pub u32);

/// The core type representation.
///
/// Every type in a Vixen program is represented as a `Ty`. Types are structural
/// where possible (tuples, functions) and nominal where the language
/// requires it (structs, enums, newtypes, opaque declarations).
///
/// Sub-types are stored as [`TypeId`]s referring back into the [`TypeEnv`]
/// that interns this `Ty`. The `Ty` itself is therefore shallow — recursing
/// requires fetching child types from the env.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Ty {
    /// A scalar type: U64, I64, F64, Bool, String.
    Scalar(Scalar),

    /// The unit type: `()`. Also the zero-element tuple.
    Unit,

    /// A named struct type, identified by its [`TypeDefId`].
    /// Generic parameters are substituted — `Pair[U64, String]` stores the
    /// `TypeDefId` of `Pair` with `type_args: [U64, String]`.
    Struct(StructTy),

    /// An enum type, identified by its [`TypeDefId`].
    Enum(EnumTy),

    /// A newtype wrapper, identified by its [`TypeDefId`].
    Newtype(NewtypeTy),

    /// A type alias target. Aliases are transparent — the checker resolves
    /// them immediately. This variant exists so we can track the alias
    /// [`TypeDefId`] for error messages.
    Alias(AliasTy),

    /// An opaque nominal type declared without a target, e.g. `type Blob;`.
    Opaque(OpaqueTy),

    /// A heterogeneous tuple: `(A, B, C)`.
    Tuple(TupleTy),

    /// A function type: `Fn(A, B) -> C` with optional capability tags.
    Function(FunctionTy),

    /// `Array[T]`.
    Array(ArrayTy),

    /// `Map[K, V]`.
    Map(MapTy),

    /// `Stream[T]`.
    Stream(StreamTy),

    /// A type parameter reference: the N-th type parameter in scope.
    /// During checking, these are substituted with concrete types.
    Param(TypeParam),

    /// An inference variable — a placeholder the checker fills in.
    Infer(InferVar),

    /// A poisoned type — the result of a type error. Propagates silently
    /// so we don't cascade errors.
    Poison,
}

/// Scalar types built into the language.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Scalar {
    U64,
    I64,
    F64,
    Bool,
    String,
}

/// A reference to a generic type parameter.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct TypeParam {
    /// The name as written in source (e.g. "T", "K", "V").
    pub name: String,
    /// Index in the parameter list of the enclosing generic definition.
    pub index: u32,
}

/// A struct type with optional generic arguments.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct StructTy {
    pub def: TypeDefId,
    pub type_args: Vec<TypeId>,
}

/// An enum type with optional generic arguments.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct EnumTy {
    pub def: TypeDefId,
    pub type_args: Vec<TypeId>,
}

/// A newtype with optional generic arguments.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct NewtypeTy {
    pub def: TypeDefId,
    pub type_args: Vec<TypeId>,
}

/// A type alias with its concrete target.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct AliasTy {
    pub def: TypeDefId,
    pub type_args: Vec<TypeId>,
}

/// An opaque nominal type with optional generic arguments.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct OpaqueTy {
    pub def: TypeDefId,
    pub type_args: Vec<TypeId>,
}

/// A heterogeneous tuple type.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct TupleTy(pub Vec<TypeId>);

/// A function type: `Fn(params) #caps -> ret`.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct FunctionTy {
    pub params: Vec<TypeId>,
    pub ret: TypeId,
    pub capabilities: Vec<Capability>,
}

/// `Array[T]`.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ArrayTy(pub TypeId);

/// `Map[K, V]`.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct MapTy {
    pub key: TypeId,
    pub value: TypeId,
}

/// `Stream[T]`.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct StreamTy(pub TypeId);

/// A capability tag on a function type (e.g. `#fs`, `#discover`).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Capability {
    pub name: String,
    /// Span of the capability name token in source. May be a default span
    /// when constructed synthetically (e.g. from runtime-injected capabilities).
    pub name_span: SourceSpan,
}

/// The definition of a named type, stored in the type environment.
///
/// [`TypeDefId`] indexes into a table of these. This is populated during the
/// type collection pass that walks the AST before checking begins.
#[derive(Clone, Debug)]
pub enum TypeDef {
    Struct(StructDef),
    Enum(EnumDef),
    Newtype(NewtypeDef),
    Alias(AliasDef),
    Opaque(OpaqueDef),
}

/// A struct definition with its fields and generic parameters.
#[derive(Clone, Debug)]
pub struct StructDef {
    pub name: String,
    /// The [`TypeDefId`] of the parent enum, if this struct is an enum variant.
    pub parent_enum: Option<TypeDefId>,
    pub type_params: Vec<String>,
    pub attrs: DeserContainerAttrs,
    pub fields: Vec<FieldDef>,
}

/// A field in a struct definition.
#[derive(Clone, Debug)]
pub struct FieldDef {
    pub name: String,
    pub ty: TypeId,
    pub attrs: DeserFieldAttrs,
    pub has_default: bool,
    pub default_value: Option<FieldDefaultValue>,
    /// Span of the field name token in source (for go-to-definition and diagnostics).
    pub name_span: SourceSpan,
}

#[derive(Clone, Debug)]
pub enum FieldDefaultValue {
    Bool(bool),
    String(String),
    U64(u64),
    None,
    EmptyArray,
    EmptyMap,
}

/// An enum definition with its variants and generic parameters.
#[derive(Clone, Debug)]
pub struct EnumDef {
    pub name: String,
    pub type_params: Vec<String>,
    pub attrs: DeserContainerAttrs,
    /// Each variant is a struct — its [`TypeDefId`] points to a [`StructDef`]
    /// whose `parent_enum` is this enum's [`TypeDefId`].
    pub variants: Vec<TypeDefId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeserRenameRule {
    SnakeCase,
    CamelCase,
    PascalCase,
    KebabCase,
    ScreamingSnakeCase,
}

#[derive(Clone, Debug, Default)]
pub struct DeserContainerAttrs {
    pub rename: Option<String>,
    pub rename_all: Option<DeserRenameRule>,
    pub untagged: bool,
}

#[derive(Clone, Debug, Default)]
pub struct DeserFieldAttrs {
    pub rename: Option<String>,
    pub flatten: bool,
}

/// A newtype definition.
#[derive(Clone, Debug)]
pub struct NewtypeDef {
    pub name: String,
    pub type_params: Vec<String>,
    pub inner: TypeId,
}

/// A type alias definition: `type Name = A | B | C`.
#[derive(Clone, Debug)]
pub struct AliasDef {
    pub name: String,
    pub type_params: Vec<String>,
    pub target: TypeId,
}

/// An opaque nominal type definition: `type Name;`.
#[derive(Clone, Debug)]
pub struct OpaqueDef {
    pub name: String,
    pub type_params: Vec<String>,
}

/// A sum type — the target of a type alias like `type Entry = Blob | Tree`.
///
/// Sum types are not a Ty variant because they're always behind an alias.
/// The alias target stores the alternatives directly.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct SumTy {
    pub alternatives: Vec<TypeId>,
}

/// A function signature — the type-level description of a function declaration.
///
/// This is richer than [`FunctionTy`] because it includes parameter names,
/// keyword parameters with defaults, and generic type parameters.
#[derive(Clone, Debug)]
pub struct FnSig {
    pub name: String,
    /// Span of the function name token in source.
    pub name_span: SourceSpan,
    pub type_params: Vec<String>,
    pub positional_params: Vec<ParamDef>,
    pub keyword_params: Vec<KeywordParamDef>,
    pub ret: TypeId,
    pub capabilities: Vec<Capability>,
}

/// A positional parameter in a function signature.
#[derive(Clone, Debug)]
pub struct ParamDef {
    pub name: String,
    /// Span of the parameter name token in source.
    pub name_span: SourceSpan,
    pub ty: TypeId,
    pub doc: Option<String>,
}

/// A keyword parameter in a function signature — has a name and may have a default.
#[derive(Clone, Debug)]
pub struct KeywordParamDef {
    pub name: String,
    /// Span of the keyword parameter name token in source.
    pub name_span: SourceSpan,
    pub ty: TypeId,
    pub has_default: bool,
    pub doc: Option<String>,
}

/// The type environment: a collection of all type definitions and interned types.
///
/// [`TypeDefId`] values index into `defs`. [`TypeId`] values index into `types`.
/// The checker builds this during the collection pass, then uses it throughout
/// type checking.
#[derive(Clone, Debug, Default)]
pub struct TypeEnv {
    defs: Vec<TypeDef>,
    /// Maps type def names to their [`TypeDefId`]s for lookup.
    name_to_def: HashMap<String, Vec<TypeDefId>>,
    /// Interned [`Ty`] values. [`TypeId`] indexes into this.
    types: Vec<Ty>,
    /// Reverse index from `Ty` to its interned [`TypeId`].
    intern_table: HashMap<Ty, TypeId>,
}

impl TypeEnv {
    pub fn new() -> Self {
        Self::default()
    }

    // ----- Definitions -----

    /// Register a new type definition, returning its [`TypeDefId`].
    pub fn define(&mut self, name: &str, def: TypeDef) -> TypeDefId {
        let id = TypeDefId(self.defs.len() as u32);
        self.name_to_def
            .entry(name.to_owned())
            .or_default()
            .push(id);
        self.defs.push(def);
        id
    }

    /// Look up a type definition by its [`TypeDefId`].
    pub fn get_def(&self, id: TypeDefId) -> &TypeDef {
        &self.defs[id.0 as usize]
    }

    /// Mutable access to a type definition.
    pub fn def_mut(&mut self, id: TypeDefId) -> &mut TypeDef {
        &mut self.defs[id.0 as usize]
    }

    /// Look up type definitions by name. Returns all definitions with that
    /// name (there can be multiple if the name is used in different scopes,
    /// though typically there's one).
    pub fn lookup_def(&self, name: &str) -> &[TypeDefId] {
        self.name_to_def
            .get(name)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// The number of registered type definitions.
    pub fn defs_len(&self) -> usize {
        self.defs.len()
    }

    pub fn defs_is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    // ----- Interned Tys -----

    /// Intern a [`Ty`], returning its [`TypeId`]. Already-interned types
    /// return their existing id.
    pub fn intern(&mut self, ty: Ty) -> TypeId {
        if let Some(&id) = self.intern_table.get(&ty) {
            return id;
        }
        let id = TypeId(self.types.len() as u32);
        self.types.push(ty.clone());
        self.intern_table.insert(ty, id);
        id
    }

    /// Look up the interned [`Ty`] behind a [`TypeId`].
    pub fn get_ty(&self, id: TypeId) -> &Ty {
        &self.types[id.0 as usize]
    }

    /// The number of interned types.
    pub fn types_len(&self) -> usize {
        self.types.len()
    }

    // ----- Convenience interners -----

    pub fn intern_scalar(&mut self, s: Scalar) -> TypeId {
        self.intern(Ty::Scalar(s))
    }

    pub fn intern_unit(&mut self) -> TypeId {
        self.intern(Ty::Unit)
    }

    pub fn intern_poison(&mut self) -> TypeId {
        self.intern(Ty::Poison)
    }

    pub fn intern_tuple(&mut self, elems: Vec<TypeId>) -> TypeId {
        self.intern(Ty::Tuple(TupleTy(elems)))
    }

    pub fn intern_array(&mut self, elem: TypeId) -> TypeId {
        self.intern(Ty::Array(ArrayTy(elem)))
    }

    pub fn intern_function(
        &mut self,
        params: Vec<TypeId>,
        ret: TypeId,
        caps: Vec<Capability>,
    ) -> TypeId {
        self.intern(Ty::Function(FunctionTy {
            params,
            ret,
            capabilities: caps,
        }))
    }

    pub fn intern_map(&mut self, key: TypeId, value: TypeId) -> TypeId {
        self.intern(Ty::Map(MapTy { key, value }))
    }

    pub fn intern_stream(&mut self, inner: TypeId) -> TypeId {
        self.intern(Ty::Stream(StreamTy(inner)))
    }

    pub fn intern_struct(&mut self, def: TypeDefId, type_args: Vec<TypeId>) -> TypeId {
        self.intern(Ty::Struct(StructTy { def, type_args }))
    }

    pub fn intern_enum(&mut self, def: TypeDefId, type_args: Vec<TypeId>) -> TypeId {
        self.intern(Ty::Enum(EnumTy { def, type_args }))
    }

    pub fn intern_newtype(&mut self, def: TypeDefId, type_args: Vec<TypeId>) -> TypeId {
        self.intern(Ty::Newtype(NewtypeTy { def, type_args }))
    }

    pub fn intern_alias(&mut self, def: TypeDefId, type_args: Vec<TypeId>) -> TypeId {
        self.intern(Ty::Alias(AliasTy { def, type_args }))
    }

    pub fn intern_opaque(&mut self, def: TypeDefId, type_args: Vec<TypeId>) -> TypeId {
        self.intern(Ty::Opaque(OpaqueTy { def, type_args }))
    }

    pub fn intern_param(&mut self, p: TypeParam) -> TypeId {
        self.intern(Ty::Param(p))
    }

    pub fn intern_infer(&mut self, v: InferVar) -> TypeId {
        self.intern(Ty::Infer(v))
    }
}

impl std::ops::Index<TypeDefId> for TypeEnv {
    type Output = TypeDef;

    fn index(&self, id: TypeDefId) -> &TypeDef {
        &self.defs[id.0 as usize]
    }
}

impl std::ops::IndexMut<TypeDefId> for TypeEnv {
    fn index_mut(&mut self, id: TypeDefId) -> &mut TypeDef {
        &mut self.defs[id.0 as usize]
    }
}

impl std::ops::Index<TypeId> for TypeEnv {
    type Output = Ty;

    fn index(&self, id: TypeId) -> &Ty {
        &self.types[id.0 as usize]
    }
}

impl TypeDef {
    /// The name of this type definition.
    pub fn name(&self) -> &str {
        match self {
            TypeDef::Struct(d) => &d.name,
            TypeDef::Enum(d) => &d.name,
            TypeDef::Newtype(d) => &d.name,
            TypeDef::Alias(d) => &d.name,
            TypeDef::Opaque(d) => &d.name,
        }
    }

    /// The generic type parameters of this definition.
    pub fn type_params(&self) -> &[String] {
        match self {
            TypeDef::Struct(d) => &d.type_params,
            TypeDef::Enum(d) => &d.type_params,
            TypeDef::Newtype(d) => &d.type_params,
            TypeDef::Alias(d) => &d.type_params,
            TypeDef::Opaque(d) => &d.type_params,
        }
    }
}

impl Ty {
    /// Returns true if this type (or any sub-type via `env`) contains any
    /// inference variables.
    pub fn has_infer_vars(&self, env: &TypeEnv) -> bool {
        match self {
            Ty::Infer(_) => true,
            Ty::Scalar(_) | Ty::Unit | Ty::Poison | Ty::Param(_) => false,
            Ty::Struct(s) => s.type_args.iter().any(|id| env[*id].has_infer_vars(env)),
            Ty::Enum(e) => e.type_args.iter().any(|id| env[*id].has_infer_vars(env)),
            Ty::Newtype(n) => n.type_args.iter().any(|id| env[*id].has_infer_vars(env)),
            Ty::Alias(a) => a.type_args.iter().any(|id| env[*id].has_infer_vars(env)),
            Ty::Opaque(o) => o.type_args.iter().any(|id| env[*id].has_infer_vars(env)),
            Ty::Tuple(t) => t.0.iter().any(|id| env[*id].has_infer_vars(env)),
            Ty::Function(f) => {
                f.params.iter().any(|id| env[*id].has_infer_vars(env))
                    || env[f.ret].has_infer_vars(env)
            }
            Ty::Array(ArrayTy(inner)) | Ty::Stream(StreamTy(inner)) => {
                env[*inner].has_infer_vars(env)
            }
            Ty::Map(m) => env[m.key].has_infer_vars(env) || env[m.value].has_infer_vars(env),
        }
    }

    /// Returns true if this type (or any sub-type via `env`) contains any
    /// type parameters.
    pub fn has_params(&self, env: &TypeEnv) -> bool {
        match self {
            Ty::Param(_) => true,
            Ty::Scalar(_) | Ty::Unit | Ty::Poison | Ty::Infer(_) => false,
            Ty::Struct(s) => s.type_args.iter().any(|id| env[*id].has_params(env)),
            Ty::Enum(e) => e.type_args.iter().any(|id| env[*id].has_params(env)),
            Ty::Newtype(n) => n.type_args.iter().any(|id| env[*id].has_params(env)),
            Ty::Alias(a) => a.type_args.iter().any(|id| env[*id].has_params(env)),
            Ty::Opaque(o) => o.type_args.iter().any(|id| env[*id].has_params(env)),
            Ty::Tuple(t) => t.0.iter().any(|id| env[*id].has_params(env)),
            Ty::Function(f) => {
                f.params.iter().any(|id| env[*id].has_params(env)) || env[f.ret].has_params(env)
            }
            Ty::Array(ArrayTy(inner)) | Ty::Stream(StreamTy(inner)) => env[*inner].has_params(env),
            Ty::Map(m) => env[m.key].has_params(env) || env[m.value].has_params(env),
        }
    }

    /// Is this a poison type?
    pub fn is_poison(&self) -> bool {
        matches!(self, Ty::Poison)
    }

    /// Format this type using a [`TypeEnv`] for named type display.
    pub fn display_with_env<'a>(&'a self, env: &'a TypeEnv) -> TyDisplay<'a> {
        TyDisplay { ty: self, env }
    }
}

/// A helper for displaying types with access to the type environment.
pub struct TyDisplay<'a> {
    ty: &'a Ty,
    env: &'a TypeEnv,
}

impl<'a> std::fmt::Display for TyDisplay<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.ty {
            Ty::Scalar(s) => write!(f, "{s}"),
            Ty::Unit => write!(f, "()"),
            Ty::Struct(s) => write_named(f, self.env, s.def, &s.type_args),
            Ty::Enum(e) => write_named(f, self.env, e.def, &e.type_args),
            Ty::Newtype(n) => write_named(f, self.env, n.def, &n.type_args),
            Ty::Alias(a) => write_named(f, self.env, a.def, &a.type_args),
            Ty::Opaque(o) => write_named(f, self.env, o.def, &o.type_args),
            Ty::Tuple(tuple) => {
                write!(f, "(")?;
                for (i, elem_id) in tuple.0.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", self.env[*elem_id].display_with_env(self.env))?;
                }
                if tuple.0.len() == 1 {
                    write!(f, ",")?;
                }
                write!(f, ")")
            }
            Ty::Function(function) => {
                write!(f, "Fn(")?;
                for (i, param_id) in function.params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", self.env[*param_id].display_with_env(self.env))?;
                }
                write!(f, ")")?;
                for cap in &function.capabilities {
                    write!(f, " {cap}")?;
                }
                write!(
                    f,
                    " -> {}",
                    self.env[function.ret].display_with_env(self.env)
                )
            }
            Ty::Array(ArrayTy(inner)) => {
                write!(f, "Array[{}]", self.env[*inner].display_with_env(self.env))
            }
            Ty::Map(m) => write!(
                f,
                "Map[{}, {}]",
                self.env[m.key].display_with_env(self.env),
                self.env[m.value].display_with_env(self.env)
            ),
            Ty::Stream(StreamTy(inner)) => {
                write!(f, "Stream[{}]", self.env[*inner].display_with_env(self.env))
            }
            Ty::Param(param) => write!(f, "{param}"),
            Ty::Infer(var) => write!(f, "{var}"),
            Ty::Poison => write!(f, "<poison>"),
        }
    }
}

fn write_named(
    f: &mut std::fmt::Formatter<'_>,
    env: &TypeEnv,
    def: TypeDefId,
    type_args: &[TypeId],
) -> std::fmt::Result {
    let name = env.get_def(def).name();
    write!(f, "{name}")?;
    if !type_args.is_empty() {
        write!(f, "[")?;
        for (i, arg) in type_args.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}", env[*arg].display_with_env(env))?;
        }
        write!(f, "]")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_display() {
        let mut env = TypeEnv::new();
        let u = env.intern_scalar(Scalar::U64);
        let s = env.intern_scalar(Scalar::String);
        assert_eq!(env.get_ty(u).display_with_env(&env).to_string(), "U64");
        assert_eq!(env.get_ty(s).display_with_env(&env).to_string(), "String");
    }

    #[test]
    fn unit_display() {
        let mut env = TypeEnv::new();
        let u = env.intern_unit();
        assert_eq!(env.get_ty(u).display_with_env(&env).to_string(), "()");
    }

    #[test]
    fn tuple_display() {
        let mut env = TypeEnv::new();
        let u64_id = env.intern_scalar(Scalar::U64);
        let bool_id = env.intern_scalar(Scalar::Bool);

        let single_id = env.intern_tuple(vec![u64_id]);
        assert_eq!(
            env.get_ty(single_id).display_with_env(&env).to_string(),
            "(U64,)"
        );

        let pair_id = env.intern_tuple(vec![u64_id, bool_id]);
        assert_eq!(
            env.get_ty(pair_id).display_with_env(&env).to_string(),
            "(U64, Bool)"
        );
    }

    #[test]
    fn function_display() {
        let mut env = TypeEnv::new();
        let u64_id = env.intern_scalar(Scalar::U64);
        let f_id = env.intern_function(vec![u64_id, u64_id], u64_id, vec![]);
        assert_eq!(
            env.get_ty(f_id).display_with_env(&env).to_string(),
            "Fn(U64, U64) -> U64"
        );
    }

    #[test]
    fn function_with_caps_display() {
        let mut env = TypeEnv::new();
        let s = env.intern_scalar(Scalar::String);
        let f_id = env.intern_function(
            vec![s],
            s,
            vec![Capability {
                name: "fs".to_owned(),
                name_span: Default::default(),
            }],
        );
        assert_eq!(
            env.get_ty(f_id).display_with_env(&env).to_string(),
            "Fn(String) #fs -> String"
        );
    }

    #[test]
    fn collection_display() {
        let mut env = TypeEnv::new();
        let s = env.intern_scalar(Scalar::String);
        let u = env.intern_scalar(Scalar::U64);
        let b = env.intern_scalar(Scalar::Bool);
        let arr = env.intern_array(s);
        let map = env.intern_map(s, u);
        let stream = env.intern_stream(b);
        assert_eq!(
            env.get_ty(arr).display_with_env(&env).to_string(),
            "Array[String]"
        );
        assert_eq!(
            env.get_ty(map).display_with_env(&env).to_string(),
            "Map[String, U64]"
        );
        assert_eq!(
            env.get_ty(stream).display_with_env(&env).to_string(),
            "Stream[Bool]"
        );
    }

    #[test]
    fn infer_var_display() {
        assert_eq!(InferVar(0).to_string(), "?0");
        assert_eq!(InferVar(42).to_string(), "?42");
    }

    #[test]
    fn opaque_display_with_env() {
        let mut env = TypeEnv::new();
        let def = env.define(
            "Token",
            TypeDef::Opaque(OpaqueDef {
                name: "Token".to_owned(),
                type_params: vec![],
            }),
        );
        let id = env.intern_opaque(def, vec![]);
        assert_eq!(env.get_ty(id).display_with_env(&env).to_string(), "Token");
    }

    #[test]
    fn function_display_with_env_is_recursive() {
        let mut env = TypeEnv::new();
        let tree_def = env.define(
            "Tree",
            TypeDef::Opaque(OpaqueDef {
                name: "Tree".to_owned(),
                type_params: vec![],
            }),
        );
        let string_id = env.intern_scalar(Scalar::String);
        let exec_input_def = env.define(
            "ExecInput",
            TypeDef::Alias(AliasDef {
                name: "ExecInput".to_owned(),
                type_params: vec![],
                target: string_id,
            }),
        );
        let exec_input_id = env.intern_alias(exec_input_def, vec![]);
        let arr_id = env.intern_array(exec_input_id);
        let tree_id = env.intern_opaque(tree_def, vec![]);
        let fn_id = env.intern_function(vec![arr_id], tree_id, vec![]);
        assert_eq!(
            env.get_ty(fn_id).display_with_env(&env).to_string(),
            "Fn(Array[ExecInput]) -> Tree"
        );
    }

    #[test]
    fn type_env_define_and_lookup() {
        let mut env = TypeEnv::new();
        let f64_id = env.intern_scalar(Scalar::F64);
        let point_id = env.define(
            "Point",
            TypeDef::Struct(StructDef {
                name: "Point".to_owned(),
                parent_enum: None,
                type_params: vec![],
                attrs: DeserContainerAttrs::default(),
                fields: vec![
                    FieldDef {
                        name: "x".to_owned(),
                        ty: f64_id,
                        attrs: DeserFieldAttrs::default(),
                        has_default: false,
                        default_value: None,
                        name_span: Default::default(),
                    },
                    FieldDef {
                        name: "y".to_owned(),
                        ty: f64_id,
                        attrs: DeserFieldAttrs::default(),
                        has_default: false,
                        default_value: None,
                        name_span: Default::default(),
                    },
                ],
            }),
        );

        assert_eq!(env.defs_len(), 1);
        assert_eq!(env.get_def(point_id).name(), "Point");
        assert_eq!(env.lookup_def("Point"), &[point_id]);
        assert_eq!(env.lookup_def("Missing"), &[]);
    }

    #[test]
    fn intern_dedupes() {
        let mut env = TypeEnv::new();
        let a = env.intern_scalar(Scalar::U64);
        let b = env.intern_scalar(Scalar::U64);
        assert_eq!(a, b);
        let c = env.intern_scalar(Scalar::I64);
        assert_ne!(a, c);
    }

    #[test]
    fn has_infer_vars_detection() {
        let mut env = TypeEnv::new();
        let u = env.intern_scalar(Scalar::U64);
        let i = env.intern_infer(InferVar(0));
        let arr_i = env.intern_array(i);
        let arr_u = env.intern_array(u);

        assert!(!env.get_ty(u).has_infer_vars(&env));
        assert!(env.get_ty(i).has_infer_vars(&env));
        assert!(env.get_ty(arr_i).has_infer_vars(&env));
        assert!(!env.get_ty(arr_u).has_infer_vars(&env));
    }

    #[test]
    fn enum_with_variants() {
        let mut env = TypeEnv::new();
        let f64_id = env.intern_scalar(Scalar::F64);

        // Register the enum first to get its ID
        let shape_id = env.define(
            "Shape",
            TypeDef::Enum(EnumDef {
                name: "Shape".to_owned(),
                type_params: vec![],
                attrs: DeserContainerAttrs::default(),
                variants: vec![], // filled in after variants are registered
            }),
        );

        let circle_id = env.define(
            "Circle",
            TypeDef::Struct(StructDef {
                name: "Circle".to_owned(),
                parent_enum: Some(shape_id),
                type_params: vec![],
                attrs: DeserContainerAttrs::default(),
                fields: vec![FieldDef {
                    name: "radius".to_owned(),
                    ty: f64_id,
                    attrs: DeserFieldAttrs::default(),
                    has_default: false,
                    default_value: None,
                    name_span: Default::default(),
                }],
            }),
        );

        let rect_id = env.define(
            "Rect",
            TypeDef::Struct(StructDef {
                name: "Rect".to_owned(),
                parent_enum: Some(shape_id),
                type_params: vec![],
                attrs: DeserContainerAttrs::default(),
                fields: vec![
                    FieldDef {
                        name: "width".to_owned(),
                        ty: f64_id,
                        attrs: DeserFieldAttrs::default(),
                        has_default: false,
                        default_value: None,
                        name_span: Default::default(),
                    },
                    FieldDef {
                        name: "height".to_owned(),
                        ty: f64_id,
                        attrs: DeserFieldAttrs::default(),
                        has_default: false,
                        default_value: None,
                        name_span: Default::default(),
                    },
                ],
            }),
        );

        // Verify the variant structs know their parent
        let TypeDef::Struct(circle_def) = env.get_def(circle_id) else {
            panic!("expected struct");
        };
        assert_eq!(circle_def.parent_enum, Some(shape_id));

        let TypeDef::Struct(rect_def) = env.get_def(rect_id) else {
            panic!("expected struct");
        };
        assert_eq!(rect_def.parent_enum, Some(shape_id));
    }
}
