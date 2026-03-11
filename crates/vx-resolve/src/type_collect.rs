//! Type collection: walk the AST to build a [`TypeEnv`] from type declarations.
//!
//! This is a lightweight pass that registers struct, enum, newtype, and type
//! alias declarations in the type environment, and provides a mapping from
//! definition name ranges to their [`TypeId`].

use std::collections::HashMap;

use rowan::TextRange;
use vx_parser::{
    AstNode, EnumDecl, FnDecl, NewtypeDecl, SourceFile, StructDecl, SyntaxKind, TypeAliasDecl,
};
use vx_types::{
    AliasDef, EnumDef, FieldDef, FnSig, KeywordParamDef, NewtypeDef, ParamDef, StructDef, Ty,
    TypeDef, TypeEnv, TypeId,
};

/// Result of type collection: a populated type environment plus lookup tables.
pub struct TypeCollection {
    /// The type environment containing all type definitions.
    pub env: TypeEnv,
    /// Map from definition name ranges to their TypeId (for structs, enums, newtypes, aliases).
    pub range_to_type: HashMap<TextRange, TypeId>,
    /// Map from definition name ranges to their function signature.
    pub range_to_fn_sig: HashMap<TextRange, FnSig>,
    /// Map from type names to their TypeId and kind (for resolving type annotations).
    pub(crate) name_to_id: HashMap<String, (TypeId, TypeKind)>,
}

/// Collect all type declarations from a source file into a [`TypeEnv`].
pub fn collect_types(file: &SourceFile) -> TypeCollection {
    let mut env = TypeEnv::new();
    let mut range_to_type = HashMap::new();
    let mut range_to_fn_sig = HashMap::new();

    // Maps type names to their TypeId and kind for resolving type references.
    let mut name_to_id: HashMap<String, (TypeId, TypeKind)> = HashMap::new();

    // First pass: register all type declarations (so we can resolve forward references).
    for item in file.items() {
        let syntax = item.syntax();
        match syntax.kind() {
            SyntaxKind::STRUCT_DECL => {
                if let Some(decl) = StructDecl::cast(syntax.clone())
                    && let Some(name_token) = decl.name()
                {
                    let name = name_token.text().to_string();
                    let type_params = collect_type_params(syntax);
                    let id = env.define(
                        &name,
                        TypeDef::Struct(StructDef {
                            name: name.clone(),
                            parent_enum: None,
                            type_params,
                            fields: Vec::new(), // filled in second pass
                        }),
                    );
                    name_to_id.insert(name, (id, TypeKind::Struct));
                    range_to_type.insert(name_token.text_range(), id);
                }
            }
            SyntaxKind::ENUM_DECL => {
                if let Some(decl) = EnumDecl::cast(syntax.clone())
                    && let Some(name_token) = decl.name()
                {
                    let name = name_token.text().to_string();
                    let type_params = collect_type_params(syntax);
                    let id = env.define(
                        &name,
                        TypeDef::Enum(EnumDef {
                            name: name.clone(),
                            type_params,
                            variants: Vec::new(), // filled in second pass
                        }),
                    );
                    name_to_id.insert(name, (id, TypeKind::Enum));
                    range_to_type.insert(name_token.text_range(), id);
                }
            }
            SyntaxKind::NEWTYPE_DECL => {
                if let Some(decl) = NewtypeDecl::cast(syntax.clone())
                    && let Some(name_token) = decl.name()
                {
                    let name = name_token.text().to_string();
                    let type_params = collect_type_params(syntax);
                    let id = env.define(
                        &name,
                        TypeDef::Newtype(NewtypeDef {
                            name: name.clone(),
                            type_params,
                            inner: Ty::Poison, // filled in second pass
                        }),
                    );
                    name_to_id.insert(name, (id, TypeKind::Newtype));
                    range_to_type.insert(name_token.text_range(), id);
                }
            }
            SyntaxKind::TYPE_ALIAS_DECL => {
                if let Some(decl) = TypeAliasDecl::cast(syntax.clone())
                    && let Some(name_token) = decl.name()
                {
                    let name = name_token.text().to_string();
                    let type_params = collect_type_params(syntax);
                    let id = env.define(
                        &name,
                        TypeDef::Alias(AliasDef {
                            name: name.clone(),
                            type_params,
                            target: Ty::Poison, // filled in second pass
                        }),
                    );
                    name_to_id.insert(name, (id, TypeKind::Alias));
                    range_to_type.insert(name_token.text_range(), id);
                }
            }
            _ => {}
        }
    }

    // Second pass: fill in fields, variants, and type references.
    let resolver = TypeResolver::new(&name_to_id);

    for item in file.items() {
        let syntax = item.syntax();
        match syntax.kind() {
            SyntaxKind::STRUCT_DECL => {
                if let Some(decl) = StructDecl::cast(syntax.clone())
                    && let Some(name_token) = decl.name()
                {
                    let (id, _) = name_to_id[name_token.text()];
                    let type_params = collect_type_param_names(syntax);
                    let fields = collect_fields(&decl, &resolver, &type_params);
                    if let TypeDef::Struct(ref mut s) = env[id] {
                        s.fields = fields;
                    }
                }
            }
            SyntaxKind::ENUM_DECL => {
                if let Some(decl) = EnumDecl::cast(syntax.clone())
                    && let Some(name_token) = decl.name()
                {
                    let (enum_id, _) = name_to_id[name_token.text()];
                    let type_params = collect_type_param_names(syntax);
                    let mut variant_ids = Vec::new();

                    for variant in decl.variants() {
                        if let Some(vname_token) = variant.name() {
                            let vname = vname_token.text().to_string();
                            let fields: Vec<FieldDef> = variant
                                .fields()
                                .filter_map(|f| {
                                    let name_tok = f.name()?;
                                    let name = name_tok.text().to_string();
                                    let name_range = name_tok.text_range();
                                    let ty = f
                                        .slot_type()
                                        .and_then(|st| st.ty())
                                        .map(|t| resolver.resolve_type(&t, &type_params))
                                        .unwrap_or(Ty::Poison);
                                    Some(FieldDef {
                                        name,
                                        ty,
                                        has_default: false,
                                        name_range,
                                    })
                                })
                                .collect();

                            let vid = env.define(
                                &vname,
                                TypeDef::Struct(StructDef {
                                    name: vname.clone(),
                                    parent_enum: Some(enum_id),
                                    type_params: type_params.clone(),
                                    fields,
                                }),
                            );
                            variant_ids.push(vid);
                            range_to_type.insert(vname_token.text_range(), vid);
                        }
                    }

                    if let TypeDef::Enum(ref mut e) = env[enum_id] {
                        e.variants = variant_ids;
                    }
                }
            }
            SyntaxKind::NEWTYPE_DECL => {
                if let Some(decl) = NewtypeDecl::cast(syntax.clone())
                    && let Some(name_token) = decl.name()
                {
                    let (id, _) = name_to_id[name_token.text()];
                    let type_params = collect_type_param_names(syntax);
                    let inner = decl
                        .inner_type()
                        .map(|t| resolver.resolve_type(&t, &type_params))
                        .unwrap_or(Ty::Poison);
                    if let TypeDef::Newtype(ref mut n) = env[id] {
                        n.inner = inner;
                    }
                }
            }
            SyntaxKind::TYPE_ALIAS_DECL => {
                if let Some(decl) = TypeAliasDecl::cast(syntax.clone())
                    && let Some(name_token) = decl.name()
                {
                    let (id, _) = name_to_id[name_token.text()];
                    let type_params = collect_type_param_names(syntax);
                    let target = decl
                        .aliased_type()
                        .map(|t| resolver.resolve_type(&t, &type_params))
                        .unwrap_or(Ty::Poison);
                    if let TypeDef::Alias(ref mut a) = env[id] {
                        a.target = target;
                    }
                }
            }
            SyntaxKind::FN_DECL => {
                if let Some(decl) = FnDecl::cast(syntax.clone())
                    && let Some(name_token) = decl.name()
                {
                    let type_params = collect_type_param_names(syntax);
                    let sig = collect_fn_sig(&decl, &resolver, &type_params);
                    range_to_fn_sig.insert(name_token.text_range(), sig);
                }
            }
            _ => {}
        }
    }

    TypeCollection {
        env,
        range_to_type,
        range_to_fn_sig,
        name_to_id,
    }
}

impl TypeCollection {
    /// Resolve a type annotation AST node to a [`Ty`].
    pub(crate) fn resolve_type_annotation(&self, ty: &vx_parser::Type) -> Ty {
        let resolver = TypeResolver::new(&self.name_to_id);
        resolver.resolve_type(ty, &[])
    }
}

/// Extract type parameter names from a TYPE_PARAM_LIST child.
fn collect_type_params(node: &vx_parser::SyntaxNode) -> Vec<String> {
    collect_type_param_names(node)
}

fn collect_type_param_names(node: &vx_parser::SyntaxNode) -> Vec<String> {
    node.children()
        .find(|c| c.kind() == SyntaxKind::TYPE_PARAM_LIST)
        .map(|list| {
            list.children_with_tokens()
                .filter_map(|child| child.into_token())
                .filter(|t| t.kind() == SyntaxKind::UPPER_IDENT)
                .map(|t| t.text().to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn collect_fields(
    decl: &StructDecl,
    resolver: &TypeResolver<'_>,
    type_params: &[String],
) -> Vec<FieldDef> {
    decl.fields()
        .filter_map(|f| {
            let name_tok = f.name()?;
            let name = name_tok.text().to_string();
            let name_range = name_tok.text_range();
            let ty = f
                .slot_type()
                .and_then(|st| st.ty())
                .map(|t| resolver.resolve_type(&t, type_params))
                .unwrap_or(Ty::Poison);
            // Check if there's an expression after the type (default value)
            let has_default = f
                .syntax()
                .children()
                .any(|c| vx_parser::Expr::cast(c).is_some());
            Some(FieldDef {
                name,
                ty,
                has_default,
                name_range,
            })
        })
        .collect()
}

fn collect_fn_sig(decl: &FnDecl, resolver: &TypeResolver<'_>, type_params: &[String]) -> FnSig {
    let name = decl
        .name()
        .map(|t| t.text().to_string())
        .unwrap_or_default();

    let positional_params: Vec<ParamDef> = decl
        .parameters()
        .filter_map(|p| {
            let pname = p.name()?.text().to_string();
            let ty = p
                .slot_type()
                .and_then(|st| st.ty())
                .map(|t| resolver.resolve_type(&t, type_params))
                .unwrap_or(Ty::Poison);
            Some(ParamDef {
                name: pname,
                ty,
                doc: vx_parser::doc_comment(p.syntax()),
            })
        })
        .collect();

    let keyword_params: Vec<KeywordParamDef> = decl
        .keyword_parameters()
        .filter_map(|kp| {
            let kname = kp.name()?.text().to_string();
            let ty = kp
                .slot_type()
                .and_then(|st| st.ty())
                .map(|t| resolver.resolve_type(&t, type_params))
                .unwrap_or(Ty::Poison);
            let has_default = kp
                .syntax()
                .children()
                .any(|c| vx_parser::Expr::cast(c).is_some());
            Some(KeywordParamDef {
                name: kname,
                ty,
                has_default,
                doc: vx_parser::doc_comment(kp.syntax()),
            })
        })
        .collect();

    let ret = decl
        .return_type()
        .and_then(|rt| rt.ty())
        .map(|t| resolver.resolve_type(&t, type_params))
        .unwrap_or(Ty::Unit);

    let capabilities = collect_capabilities(decl);

    FnSig {
        name,
        type_params: type_params.to_vec(),
        positional_params,
        keyword_params,
        ret,
        capabilities,
    }
}

fn collect_capabilities(decl: &FnDecl) -> Vec<vx_types::Capability> {
    let Some(cap_list) = decl.capability_list() else {
        return Vec::new();
    };
    cap_list
        .tags()
        .filter_map(|tag| {
            Some(vx_types::Capability {
                name: tag.name()?.text().to_string(),
            })
        })
        .collect()
}

#[derive(Clone, Copy)]
pub(crate) enum TypeKind {
    Struct,
    Enum,
    Newtype,
    Alias,
}

/// Resolves AST type annotations to [`Ty`] values.
pub(crate) struct TypeResolver<'a> {
    name_to_id: &'a HashMap<String, (TypeId, TypeKind)>,
}

impl<'a> TypeResolver<'a> {
    fn new(name_to_id: &'a HashMap<String, (TypeId, TypeKind)>) -> Self {
        Self { name_to_id }
    }

    fn resolve_type(&self, ty: &vx_parser::Type, type_params: &[String]) -> Ty {
        use vx_parser::Type;
        match ty {
            Type::Path(path) => self.resolve_type_path(path, type_params),
            Type::Optional(opt) => {
                let inner = opt
                    .inner()
                    .map(|t| self.resolve_type(&t, type_params))
                    .unwrap_or(Ty::Poison);
                // The parser wraps ALL type_expr in OPTIONAL_TYPE, even without `?`.
                // Only make it Optional if there's actually a QUESTION token.
                let has_question = opt
                    .syntax()
                    .children_with_tokens()
                    .any(|c| c.kind() == SyntaxKind::QUESTION);
                if has_question {
                    Ty::Optional(Box::new(inner))
                } else {
                    inner
                }
            }
            Type::Tuple(tup) => {
                let elements: Vec<Ty> = tup
                    .items()
                    .map(|t| self.resolve_type(&t, type_params))
                    .collect();
                Ty::Tuple(vx_types::TupleTy { elements })
            }
            Type::Function(func) => {
                let params: Vec<Ty> = func
                    .parameter_types()
                    .map(|t| self.resolve_type(&t, type_params))
                    .collect();
                let ret = func
                    .return_type()
                    .map(|t| self.resolve_type(&t, type_params))
                    .unwrap_or(Ty::Unit);
                let capabilities = func
                    .capability_list()
                    .map(|cl| {
                        cl.tags()
                            .filter_map(|tag| {
                                Some(vx_types::Capability {
                                    name: tag.name()?.text().to_string(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Ty::Function(vx_types::FunctionTy {
                    params,
                    ret: Box::new(ret),
                    capabilities,
                })
            }
            Type::Paren(p) => p
                .inner()
                .map(|t| self.resolve_type(&t, type_params))
                .unwrap_or(Ty::Poison),
            Type::Sum(sum) => {
                // Sum types are just used in type aliases, resolve each variant
                let alts: Vec<Ty> = sum
                    .variants()
                    .map(|t| self.resolve_type(&t, type_params))
                    .collect();
                if alts.len() == 1 {
                    alts.into_iter().next().unwrap()
                } else {
                    // There's no direct Ty variant for sum types; they're stored
                    // as AliasDef targets. For now, return the first alternative.
                    alts.into_iter().next().unwrap_or(Ty::Poison)
                }
            }
        }
    }

    fn resolve_type_path(&self, path: &vx_parser::TypePath, type_params: &[String]) -> Ty {
        let segments: Vec<String> = path.segments().map(|s| s.text().to_string()).collect();
        let name = match segments.first() {
            Some(n) => n,
            None => return Ty::Poison,
        };

        // Check if it's a type parameter
        if let Some(index) = type_params.iter().position(|p| p == name) {
            return Ty::Param(vx_types::TypeParam {
                name: name.clone(),
                index: index as u32,
            });
        }

        // Check built-in scalar types
        match name.as_str() {
            "U64" => return Ty::Scalar(vx_types::Scalar::U64),
            "I64" => return Ty::Scalar(vx_types::Scalar::I64),
            "F64" => return Ty::Scalar(vx_types::Scalar::F64),
            "Bool" => return Ty::Scalar(vx_types::Scalar::Bool),
            "String" => return Ty::Scalar(vx_types::Scalar::String),
            "Unit" => return Ty::Unit,
            "Path" => return Ty::Builtin(vx_types::Builtin::Path),
            "Blob" => return Ty::Builtin(vx_types::Builtin::Blob),
            "Tree" => return Ty::Builtin(vx_types::Builtin::Tree),
            "ExecPath" => return Ty::Builtin(vx_types::Builtin::ExecPath),
            "Process" => return Ty::Builtin(vx_types::Builtin::Process),
            "Host" => return Ty::Builtin(vx_types::Builtin::Host),
            "ExecutorClass" => return Ty::Builtin(vx_types::Builtin::ExecutorClass),
            _ => {}
        }

        // Resolve type args
        let type_args: Vec<Ty> = path
            .type_args()
            .map(|args| {
                args.types()
                    .map(|t| self.resolve_type(&t, type_params))
                    .collect()
            })
            .unwrap_or_default();

        // Check special built-in generic types
        match name.as_str() {
            "Option" => {
                let inner = type_args.into_iter().next().unwrap_or(Ty::Poison);
                return Ty::Optional(Box::new(inner));
            }
            "Array" => {
                let inner = type_args.into_iter().next().unwrap_or(Ty::Poison);
                return Ty::Array(Box::new(inner));
            }
            "Map" => {
                let mut iter = type_args.into_iter();
                let key = iter.next().unwrap_or(Ty::Poison);
                let val = iter.next().unwrap_or(Ty::Poison);
                return Ty::Map(Box::new(key), Box::new(val));
            }
            "Stream" => {
                let inner = type_args.into_iter().next().unwrap_or(Ty::Poison);
                return Ty::Stream(Box::new(inner));
            }
            _ => {}
        }

        // Look up in the type environment
        if let Some(&(id, kind)) = self.name_to_id.get(name) {
            return match kind {
                TypeKind::Struct => Ty::Struct(vx_types::StructTy { id, type_args }),
                TypeKind::Enum => Ty::Enum(vx_types::EnumTy { id, type_args }),
                TypeKind::Newtype => Ty::Newtype(vx_types::NewtypeTy { id, type_args }),
                TypeKind::Alias => Ty::Alias(vx_types::AliasTy { id, type_args }),
            };
        }

        Ty::Poison
    }
}
