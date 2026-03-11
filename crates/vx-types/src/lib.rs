//! Type representation for the Vixen language.
//!
//! This crate defines the data structures that represent Vixen's type system.
//! It is used by the type checker (vx-check) and evaluator (vx-eval).

mod display;

/// A unique identifier for a type definition (struct, enum, newtype, type alias).
///
/// These are assigned during type collection — the pass that walks the AST and
/// registers all type declarations before checking begins.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TypeId(pub u32);

/// A unique identifier for a type variable introduced during inference.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct InferVar(pub u32);

/// The core type representation.
///
/// Every type in a Vixen program is represented as a `Ty`. Types are structural
/// where possible (tuples, functions, optionals) and nominal where the language
/// requires it (structs, enums, newtypes, opaque declarations).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Ty {
    /// A scalar type: U64, I64, F64, Bool, String.
    Scalar(Scalar),

    /// The unit type: `()`. Also the zero-element tuple.
    Unit,

    /// A named struct type, identified by its TypeId.
    /// Generic parameters are substituted — `Pair[U64, String]` stores the
    /// TypeId of `Pair` with `type_args: [U64, String]`.
    Struct(StructTy),

    /// An enum type, identified by its TypeId.
    Enum(EnumTy),

    /// A newtype wrapper, identified by its TypeId.
    Newtype(NewtypeTy),

    /// A type alias target. Aliases are transparent — the checker resolves
    /// them immediately. This variant exists so we can track the alias TypeId
    /// for error messages.
    Alias(AliasTy),

    /// An opaque nominal type declared without a target, e.g. `type Blob;`.
    Opaque(OpaqueTy),

    /// A heterogeneous tuple: `(A, B, C)`.
    Tuple(TupleTy),

    /// A function type: `Fn(A, B) -> C` with optional capability tags.
    Function(FunctionTy),

    /// `Option[T]` — sugar `T?` desugars to this.
    Optional(Box<Ty>),

    /// `Array[T]`.
    Array(Box<Ty>),

    /// `Map[K, V]`.
    Map(Box<Ty>, Box<Ty>),

    /// `Stream[T]`.
    Stream(Box<Ty>),

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
    pub id: TypeId,
    pub type_args: Vec<Ty>,
}

/// An enum type with optional generic arguments.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct EnumTy {
    pub id: TypeId,
    pub type_args: Vec<Ty>,
}

/// A newtype with optional generic arguments.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct NewtypeTy {
    pub id: TypeId,
    pub type_args: Vec<Ty>,
}

/// A type alias with its concrete target.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct AliasTy {
    pub id: TypeId,
    pub type_args: Vec<Ty>,
}

/// An opaque nominal type with optional generic arguments.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct OpaqueTy {
    pub id: TypeId,
    pub type_args: Vec<Ty>,
}

/// A heterogeneous tuple type.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct TupleTy {
    pub elements: Vec<Ty>,
}

/// A function type: `Fn(params) #caps -> ret`.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct FunctionTy {
    pub params: Vec<Ty>,
    pub ret: Box<Ty>,
    pub capabilities: Vec<Capability>,
}

/// A capability tag on a function type (e.g. `#fs`, `#discover`).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Capability {
    pub name: String,
}

/// The definition of a named type, stored in the type environment.
///
/// `TypeId` indexes into a table of these. This is populated during the
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
    /// The TypeId of the parent enum, if this struct is an enum variant.
    pub parent_enum: Option<TypeId>,
    pub type_params: Vec<String>,
    pub fields: Vec<FieldDef>,
}

/// A field in a struct definition.
#[derive(Clone, Debug)]
pub struct FieldDef {
    pub name: String,
    pub ty: Ty,
    pub has_default: bool,
    /// Text range of the field name token in source (for go-to-definition).
    pub name_range: rowan::TextRange,
}

/// An enum definition with its variants and generic parameters.
#[derive(Clone, Debug)]
pub struct EnumDef {
    pub name: String,
    pub type_params: Vec<String>,
    /// Each variant is a struct — its TypeId points to a StructDef
    /// whose `parent_enum` is this enum's TypeId.
    pub variants: Vec<TypeId>,
}

/// A newtype definition.
#[derive(Clone, Debug)]
pub struct NewtypeDef {
    pub name: String,
    pub type_params: Vec<String>,
    pub inner: Ty,
}

/// A type alias definition: `type Name = A | B | C`.
#[derive(Clone, Debug)]
pub struct AliasDef {
    pub name: String,
    pub type_params: Vec<String>,
    pub target: Ty,
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
    pub alternatives: Vec<Ty>,
}

/// A function signature — the type-level description of a function declaration.
///
/// This is richer than `FunctionTy` because it includes parameter names,
/// keyword parameters with defaults, and generic type parameters.
#[derive(Clone, Debug)]
pub struct FnSig {
    pub name: String,
    pub type_params: Vec<String>,
    pub positional_params: Vec<ParamDef>,
    pub keyword_params: Vec<KeywordParamDef>,
    pub ret: Ty,
    pub capabilities: Vec<Capability>,
}

/// A positional parameter in a function signature.
#[derive(Clone, Debug)]
pub struct ParamDef {
    pub name: String,
    pub ty: Ty,
    pub doc: Option<String>,
}

/// A keyword parameter in a function signature — has a name and may have a default.
#[derive(Clone, Debug)]
pub struct KeywordParamDef {
    pub name: String,
    pub ty: Ty,
    pub has_default: bool,
    pub doc: Option<String>,
}

/// The type environment: a collection of all named type definitions.
///
/// `TypeId` values index into `defs`. The checker builds this during the
/// collection pass, then uses it throughout type checking.
#[derive(Clone, Debug, Default)]
pub struct TypeEnv {
    defs: Vec<TypeDef>,
    /// Maps type names to their TypeIds for lookup.
    name_to_id: std::collections::HashMap<String, Vec<TypeId>>,
}

impl TypeEnv {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new type definition, returning its TypeId.
    pub fn define(&mut self, name: &str, def: TypeDef) -> TypeId {
        let id = TypeId(self.defs.len() as u32);
        self.name_to_id.entry(name.to_owned()).or_default().push(id);
        self.defs.push(def);
        id
    }

    /// Look up a type definition by its TypeId.
    pub fn get(&self, id: TypeId) -> &TypeDef {
        &self.defs[id.0 as usize]
    }

    /// Look up type definitions by name. Returns all definitions with that
    /// name (there can be multiple if the name is used in different scopes,
    /// though typically there's one).
    pub fn lookup(&self, name: &str) -> &[TypeId] {
        self.name_to_id
            .get(name)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// The number of registered type definitions.
    pub fn len(&self) -> usize {
        self.defs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }
}

impl std::ops::Index<TypeId> for TypeEnv {
    type Output = TypeDef;

    fn index(&self, id: TypeId) -> &TypeDef {
        &self.defs[id.0 as usize]
    }
}

impl std::ops::IndexMut<TypeId> for TypeEnv {
    fn index_mut(&mut self, id: TypeId) -> &mut TypeDef {
        &mut self.defs[id.0 as usize]
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
    /// Returns true if this type contains any inference variables.
    pub fn has_infer_vars(&self) -> bool {
        match self {
            Ty::Infer(_) => true,
            Ty::Scalar(_) | Ty::Unit | Ty::Poison => false,
            Ty::Param(_) => false,
            Ty::Struct(s) => s.type_args.iter().any(Ty::has_infer_vars),
            Ty::Enum(e) => e.type_args.iter().any(Ty::has_infer_vars),
            Ty::Newtype(n) => n.type_args.iter().any(Ty::has_infer_vars),
            Ty::Alias(a) => a.type_args.iter().any(Ty::has_infer_vars),
            Ty::Opaque(o) => o.type_args.iter().any(Ty::has_infer_vars),
            Ty::Tuple(t) => t.elements.iter().any(Ty::has_infer_vars),
            Ty::Function(f) => f.params.iter().any(Ty::has_infer_vars) || f.ret.has_infer_vars(),
            Ty::Optional(inner) | Ty::Array(inner) | Ty::Stream(inner) => inner.has_infer_vars(),
            Ty::Map(k, v) => k.has_infer_vars() || v.has_infer_vars(),
        }
    }

    /// Returns true if this type contains any type parameters.
    pub fn has_params(&self) -> bool {
        match self {
            Ty::Param(_) => true,
            Ty::Scalar(_) | Ty::Unit | Ty::Poison | Ty::Infer(_) => false,
            Ty::Struct(s) => s.type_args.iter().any(Ty::has_params),
            Ty::Enum(e) => e.type_args.iter().any(Ty::has_params),
            Ty::Newtype(n) => n.type_args.iter().any(Ty::has_params),
            Ty::Alias(a) => a.type_args.iter().any(Ty::has_params),
            Ty::Opaque(o) => o.type_args.iter().any(Ty::has_params),
            Ty::Tuple(t) => t.elements.iter().any(Ty::has_params),
            Ty::Function(f) => f.params.iter().any(Ty::has_params) || f.ret.has_params(),
            Ty::Optional(inner) | Ty::Array(inner) | Ty::Stream(inner) => inner.has_params(),
            Ty::Map(k, v) => k.has_params() || v.has_params(),
        }
    }

    /// Substitute type parameters with concrete types.
    ///
    /// `substitution[i]` replaces `Param { index: i, .. }`.
    pub fn substitute(&self, substitution: &[Ty]) -> Ty {
        match self {
            Ty::Param(p) => substitution
                .get(p.index as usize)
                .cloned()
                .unwrap_or_else(|| self.clone()),
            Ty::Scalar(_) | Ty::Unit | Ty::Poison | Ty::Infer(_) => self.clone(),
            Ty::Struct(s) => Ty::Struct(StructTy {
                id: s.id,
                type_args: s
                    .type_args
                    .iter()
                    .map(|t| t.substitute(substitution))
                    .collect(),
            }),
            Ty::Enum(e) => Ty::Enum(EnumTy {
                id: e.id,
                type_args: e
                    .type_args
                    .iter()
                    .map(|t| t.substitute(substitution))
                    .collect(),
            }),
            Ty::Newtype(n) => Ty::Newtype(NewtypeTy {
                id: n.id,
                type_args: n
                    .type_args
                    .iter()
                    .map(|t| t.substitute(substitution))
                    .collect(),
            }),
            Ty::Alias(a) => Ty::Alias(AliasTy {
                id: a.id,
                type_args: a
                    .type_args
                    .iter()
                    .map(|t| t.substitute(substitution))
                    .collect(),
            }),
            Ty::Opaque(o) => Ty::Opaque(OpaqueTy {
                id: o.id,
                type_args: o
                    .type_args
                    .iter()
                    .map(|t| t.substitute(substitution))
                    .collect(),
            }),
            Ty::Tuple(t) => Ty::Tuple(TupleTy {
                elements: t
                    .elements
                    .iter()
                    .map(|t| t.substitute(substitution))
                    .collect(),
            }),
            Ty::Function(f) => Ty::Function(FunctionTy {
                params: f
                    .params
                    .iter()
                    .map(|t| t.substitute(substitution))
                    .collect(),
                ret: Box::new(f.ret.substitute(substitution)),
                capabilities: f.capabilities.clone(),
            }),
            Ty::Optional(inner) => Ty::Optional(Box::new(inner.substitute(substitution))),
            Ty::Array(inner) => Ty::Array(Box::new(inner.substitute(substitution))),
            Ty::Stream(inner) => Ty::Stream(Box::new(inner.substitute(substitution))),
            Ty::Map(k, v) => Ty::Map(
                Box::new(k.substitute(substitution)),
                Box::new(v.substitute(substitution)),
            ),
        }
    }

    /// Is this a poison type?
    pub fn is_poison(&self) -> bool {
        matches!(self, Ty::Poison)
    }

    /// Format this type using a TypeEnv for named type display.
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
            Ty::Struct(s) => {
                let name = self.env.get(s.id).name();
                write!(f, "{name}")?;
                if !s.type_args.is_empty() {
                    write!(f, "[")?;
                    for (i, arg) in s.type_args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", arg.display_with_env(self.env))?;
                    }
                    write!(f, "]")?;
                }
                Ok(())
            }
            Ty::Enum(e) => {
                let name = self.env.get(e.id).name();
                write!(f, "{name}")?;
                if !e.type_args.is_empty() {
                    write!(f, "[")?;
                    for (i, arg) in e.type_args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", arg.display_with_env(self.env))?;
                    }
                    write!(f, "]")?;
                }
                Ok(())
            }
            Ty::Newtype(n) => {
                let name = self.env.get(n.id).name();
                write!(f, "{name}")?;
                if !n.type_args.is_empty() {
                    write!(f, "[")?;
                    for (i, arg) in n.type_args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", arg.display_with_env(self.env))?;
                    }
                    write!(f, "]")?;
                }
                Ok(())
            }
            Ty::Alias(a) => {
                let name = self.env.get(a.id).name();
                write!(f, "{name}")?;
                if !a.type_args.is_empty() {
                    write!(f, "[")?;
                    for (i, arg) in a.type_args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", arg.display_with_env(self.env))?;
                    }
                    write!(f, "]")?;
                }
                Ok(())
            }
            Ty::Opaque(o) => {
                let name = self.env.get(o.id).name();
                write!(f, "{name}")?;
                if !o.type_args.is_empty() {
                    write!(f, "[")?;
                    for (i, arg) in o.type_args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", arg.display_with_env(self.env))?;
                    }
                    write!(f, "]")?;
                }
                Ok(())
            }
            Ty::Tuple(tuple) => {
                write!(f, "(")?;
                for (i, elem) in tuple.elements.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", elem.display_with_env(self.env))?;
                }
                if tuple.elements.len() == 1 {
                    write!(f, ",")?;
                }
                write!(f, ")")
            }
            Ty::Function(function) => {
                write!(f, "Fn(")?;
                for (i, param) in function.params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", param.display_with_env(self.env))?;
                }
                write!(f, ")")?;
                for cap in &function.capabilities {
                    write!(f, " {cap}")?;
                }
                write!(f, " -> {}", function.ret.display_with_env(self.env))
            }
            Ty::Optional(inner) => write!(f, "{}?", inner.display_with_env(self.env)),
            Ty::Array(inner) => write!(f, "Array[{}]", inner.display_with_env(self.env)),
            Ty::Map(key, value) => {
                write!(
                    f,
                    "Map[{}, {}]",
                    key.display_with_env(self.env),
                    value.display_with_env(self.env)
                )
            }
            Ty::Stream(inner) => write!(f, "Stream[{}]", inner.display_with_env(self.env)),
            Ty::Param(param) => write!(f, "{param}"),
            Ty::Infer(var) => write!(f, "{var}"),
            Ty::Poison => write!(f, "<poison>"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_display() {
        assert_eq!(Ty::Scalar(Scalar::U64).to_string(), "U64");
        assert_eq!(Ty::Scalar(Scalar::String).to_string(), "String");
    }

    #[test]
    fn unit_display() {
        assert_eq!(Ty::Unit.to_string(), "()");
    }

    #[test]
    fn tuple_display() {
        let single = Ty::Tuple(TupleTy {
            elements: vec![Ty::Scalar(Scalar::U64)],
        });
        assert_eq!(single.to_string(), "(U64,)");

        let pair = Ty::Tuple(TupleTy {
            elements: vec![Ty::Scalar(Scalar::U64), Ty::Scalar(Scalar::Bool)],
        });
        assert_eq!(pair.to_string(), "(U64, Bool)");
    }

    #[test]
    fn function_display() {
        let f = Ty::Function(FunctionTy {
            params: vec![Ty::Scalar(Scalar::U64), Ty::Scalar(Scalar::U64)],
            ret: Box::new(Ty::Scalar(Scalar::U64)),
            capabilities: vec![],
        });
        assert_eq!(f.to_string(), "Fn(U64, U64) -> U64");
    }

    #[test]
    fn function_with_caps_display() {
        let f = Ty::Function(FunctionTy {
            params: vec![Ty::Scalar(Scalar::String)],
            ret: Box::new(Ty::Scalar(Scalar::String)),
            capabilities: vec![Capability {
                name: "fs".to_owned(),
            }],
        });
        assert_eq!(f.to_string(), "Fn(String) #fs -> String");
    }

    #[test]
    fn optional_display() {
        let opt = Ty::Optional(Box::new(Ty::Scalar(Scalar::U64)));
        assert_eq!(opt.to_string(), "U64?");
    }

    #[test]
    fn collection_display() {
        assert_eq!(
            Ty::Array(Box::new(Ty::Scalar(Scalar::String))).to_string(),
            "Array[String]"
        );
        assert_eq!(
            Ty::Map(
                Box::new(Ty::Scalar(Scalar::String)),
                Box::new(Ty::Scalar(Scalar::U64))
            )
            .to_string(),
            "Map[String, U64]"
        );
        assert_eq!(
            Ty::Stream(Box::new(Ty::Scalar(Scalar::Bool))).to_string(),
            "Stream[Bool]"
        );
    }

    #[test]
    fn infer_var_display() {
        assert_eq!(Ty::Infer(InferVar(0)).to_string(), "?0");
        assert_eq!(Ty::Infer(InferVar(42)).to_string(), "?42");
    }

    #[test]
    fn opaque_display_with_env() {
        let mut env = TypeEnv::new();
        let id = env.define(
            "Token",
            TypeDef::Opaque(OpaqueDef {
                name: "Token".to_owned(),
                type_params: vec![],
            }),
        );
        let ty = Ty::Opaque(OpaqueTy {
            id,
            type_args: vec![],
        });
        assert_eq!(ty.display_with_env(&env).to_string(), "Token");
        assert_eq!(ty.to_string(), format!("<opaque#{}>", id.0));
    }

    #[test]
    fn function_display_with_env_is_recursive() {
        let mut env = TypeEnv::new();
        let tree_id = env.define(
            "Tree",
            TypeDef::Opaque(OpaqueDef {
                name: "Tree".to_owned(),
                type_params: vec![],
            }),
        );
        let exec_input_id = env.define(
            "ExecInput",
            TypeDef::Alias(AliasDef {
                name: "ExecInput".to_owned(),
                type_params: vec![],
                target: Ty::Scalar(Scalar::String),
            }),
        );
        let ty = Ty::Function(FunctionTy {
            params: vec![Ty::Array(Box::new(Ty::Alias(AliasTy {
                id: exec_input_id,
                type_args: vec![],
            })))],
            ret: Box::new(Ty::Opaque(OpaqueTy {
                id: tree_id,
                type_args: vec![],
            })),
            capabilities: vec![],
        });

        assert_eq!(
            ty.display_with_env(&env).to_string(),
            "Fn(Array[ExecInput]) -> Tree"
        );
    }

    #[test]
    fn type_env_define_and_lookup() {
        let mut env = TypeEnv::new();
        let point_id = env.define(
            "Point",
            TypeDef::Struct(StructDef {
                name: "Point".to_owned(),
                parent_enum: None,
                type_params: vec![],
                fields: vec![
                    FieldDef {
                        name: "x".to_owned(),
                        ty: Ty::Scalar(Scalar::F64),
                        has_default: false,
                        name_range: Default::default(),
                    },
                    FieldDef {
                        name: "y".to_owned(),
                        ty: Ty::Scalar(Scalar::F64),
                        has_default: false,
                        name_range: Default::default(),
                    },
                ],
            }),
        );

        assert_eq!(env.len(), 1);
        assert_eq!(env.get(point_id).name(), "Point");
        assert_eq!(env.lookup("Point"), &[point_id]);
        assert_eq!(env.lookup("Missing"), &[]);
    }

    #[test]
    fn substitute_replaces_params() {
        let t = TypeParam {
            name: "T".to_owned(),
            index: 0,
        };
        let ty = Ty::Array(Box::new(Ty::Param(t)));
        let substituted = ty.substitute(&[Ty::Scalar(Scalar::U64)]);
        assert_eq!(substituted, Ty::Array(Box::new(Ty::Scalar(Scalar::U64))));
    }

    #[test]
    fn substitute_leaves_unmatched_params() {
        let t = TypeParam {
            name: "T".to_owned(),
            index: 5,
        };
        let ty = Ty::Param(t.clone());
        let substituted = ty.substitute(&[Ty::Scalar(Scalar::U64)]);
        assert_eq!(substituted, Ty::Param(t));
    }

    #[test]
    fn has_infer_vars_detection() {
        assert!(!Ty::Scalar(Scalar::U64).has_infer_vars());
        assert!(Ty::Infer(InferVar(0)).has_infer_vars());
        assert!(Ty::Array(Box::new(Ty::Infer(InferVar(0)))).has_infer_vars());
        assert!(!Ty::Array(Box::new(Ty::Scalar(Scalar::U64))).has_infer_vars());
    }

    #[test]
    fn enum_with_variants() {
        let mut env = TypeEnv::new();

        // Register the enum first to get its ID
        let shape_id = env.define(
            "Shape",
            TypeDef::Enum(EnumDef {
                name: "Shape".to_owned(),
                type_params: vec![],
                variants: vec![], // filled in after variants are registered
            }),
        );

        let circle_id = env.define(
            "Circle",
            TypeDef::Struct(StructDef {
                name: "Circle".to_owned(),
                parent_enum: Some(shape_id),
                type_params: vec![],
                fields: vec![FieldDef {
                    name: "radius".to_owned(),
                    ty: Ty::Scalar(Scalar::F64),
                    has_default: false,
                    name_range: Default::default(),
                }],
            }),
        );

        let rect_id = env.define(
            "Rect",
            TypeDef::Struct(StructDef {
                name: "Rect".to_owned(),
                parent_enum: Some(shape_id),
                type_params: vec![],
                fields: vec![
                    FieldDef {
                        name: "width".to_owned(),
                        ty: Ty::Scalar(Scalar::F64),
                        has_default: false,
                        name_range: Default::default(),
                    },
                    FieldDef {
                        name: "height".to_owned(),
                        ty: Ty::Scalar(Scalar::F64),
                        has_default: false,
                        name_range: Default::default(),
                    },
                ],
            }),
        );

        // Verify the variant structs know their parent
        let TypeDef::Struct(circle_def) = env.get(circle_id) else {
            panic!("expected struct");
        };
        assert_eq!(circle_def.parent_enum, Some(shape_id));

        let TypeDef::Struct(rect_def) = env.get(rect_id) else {
            panic!("expected struct");
        };
        assert_eq!(rect_def.parent_enum, Some(shape_id));
    }
}
