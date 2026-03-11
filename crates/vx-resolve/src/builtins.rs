//! Builtin function and method signatures for the Vixen runtime surface.
//!
//! Parsed stdlib headers are the primary source of truth. A small fallback
//! table remains for runtime helpers that the std sketch does not yet cover.

use std::{
    collections::{BTreeSet, HashMap},
    sync::OnceLock,
};

use vx_parser::{
    AstNode, FnDecl, NamespaceDecl, SourceFile, SyntaxKind, TypePath, parse_source_file,
};
use vx_types::{
    Builtin, Capability, FnSig, FunctionTy, KeywordParamDef, ParamDef, Scalar, Ty, TypeParam,
};

use crate::cap_check::CapSet;

const STD_CORE: &str = include_str!("../../vx-stdlib/std/core.vx");
const STD_BUILD: &str = include_str!("../../vx-stdlib/std/build.vx");

/// A method signature on a builtin type.
#[derive(Clone, Debug)]
pub struct MethodSig {
    pub name: String,
    pub params: Vec<ParamDef>,
    pub ret: Ty,
    pub capabilities: Vec<Capability>,
}

#[derive(Default)]
struct BuiltinRegistry {
    fns: HashMap<String, Vec<FnSig>>,
    methods: HashMap<(Builtin, String), MethodSig>,
    scalar_methods: HashMap<(Scalar, String), MethodSig>,
    static_methods: HashMap<(String, String), Vec<FnSig>>,
    fn_names: Vec<String>,
    method_names: HashMap<Builtin, Vec<String>>,
    namespace_names: Vec<String>,
}

static REGISTRY: OnceLock<BuiltinRegistry> = OnceLock::new();

/// Derive a CapSet from a list of Capability values.
fn caps_from_capabilities(caps: &[Capability]) -> CapSet {
    let mut result = CapSet::EMPTY;
    for cap in caps {
        if let Some(c) = CapSet::from_name(&cap.name) {
            result = result.union(c);
        }
    }
    result
}

/// Get the capability requirements for a builtin free function.
pub fn builtin_fn_caps(name: &str) -> Option<CapSet> {
    builtin_fn_sig(name).map(|sig| caps_from_capabilities(&sig.capabilities))
}

/// Get the capability requirements for a builtin method.
pub fn builtin_method_caps(receiver: &Builtin, method_name: &str) -> Option<CapSet> {
    builtin_method(receiver, method_name).map(|sig| caps_from_capabilities(&sig.capabilities))
}

/// List all builtin free function names.
pub fn all_builtin_fn_names() -> Vec<String> {
    let mut names: BTreeSet<String> = registry().fn_names.iter().cloned().collect();
    names.extend(
        fallback_builtin_fn_names()
            .iter()
            .map(|name| (*name).to_string()),
    );
    names.into_iter().collect()
}

/// List all method names on a given builtin type.
pub fn builtin_methods_for(receiver: &Builtin) -> Vec<String> {
    let mut names: BTreeSet<String> = registry()
        .method_names
        .get(receiver)
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    names.extend(
        fallback_builtin_method_names(receiver)
            .iter()
            .map(|name| (*name).to_string()),
    );
    names.into_iter().collect()
}

/// Look up a builtin free function signature by name.
pub fn builtin_fn_sig(name: &str) -> Option<FnSig> {
    fallback_builtin_fn_sig(name).or_else(|| builtin_fn_sigs(name).into_iter().next())
}

/// Look up all builtin free-function signatures by name.
pub fn builtin_fn_sigs(name: &str) -> Vec<FnSig> {
    registry().fns.get(name).cloned().unwrap_or_default()
}

/// Look up a method signature on a builtin type.
pub fn builtin_method(receiver: &Builtin, method_name: &str) -> Option<MethodSig> {
    registry()
        .methods
        .get(&(*receiver, method_name.to_string()))
        .cloned()
        .or_else(|| fallback_builtin_method(receiver, method_name))
}

/// Look up a method signature on a scalar type.
pub fn builtin_scalar_method(receiver: &Scalar, method_name: &str) -> Option<MethodSig> {
    registry()
        .scalar_methods
        .get(&(*receiver, method_name.to_string()))
        .cloned()
}

/// Check whether a name resolves to a builtin namespace.
pub fn is_builtin_namespace(name: &str) -> bool {
    registry()
        .namespace_names
        .iter()
        .any(|namespace| namespace == name)
}

/// Look up all static namespace member signatures by namespace and member name.
pub fn builtin_static_method_sigs(namespace: &str, method_name: &str) -> Vec<FnSig> {
    registry()
        .static_methods
        .get(&(namespace.to_string(), method_name.to_string()))
        .cloned()
        .unwrap_or_default()
}

fn registry() -> &'static BuiltinRegistry {
    REGISTRY.get_or_init(BuiltinRegistry::load)
}

impl BuiltinRegistry {
    fn load() -> Self {
        let mut registry = Self::default();
        registry.load_std_file("std/core.vx", STD_CORE);
        registry.load_std_file("std/build.vx", STD_BUILD);
        registry.rebuild_name_indexes();
        registry
    }

    fn load_std_file(&mut self, path: &str, source: &str) {
        let parsed = parse_source_file(source);
        assert!(
            parsed.errors().is_empty(),
            "{path} parse errors: {:?}",
            parsed.errors()
        );
        let file = parsed.tree();

        for item in file.items() {
            if let vx_parser::Item::FnDecl(fn_decl) = item {
                self.insert_fn(collect_fn_sig(&fn_decl, None));
            }
        }

        for namespace in top_level_namespaces(&file) {
            self.load_namespace(&namespace);
        }
    }

    fn load_namespace(&mut self, namespace: &NamespaceDecl) {
        let namespace_name = namespace_name(namespace);
        self.namespace_names.push(namespace_name.clone());
        let receiver = namespace_receiver(namespace);
        let Some(body) = namespace.body() else {
            return;
        };

        for item in body.items() {
            let vx_parser::Item::FnDecl(fn_decl) = item else {
                continue;
            };

            let Some(fn_name) = fn_decl.name().map(|name| name.text().to_string()) else {
                continue;
            };

            let Some(first_param) = fn_decl.parameters().next() else {
                let sig = collect_fn_sig(&fn_decl, receiver);
                self.insert_static_method(&namespace_name, sig.clone());
                if receiver.is_some() && fn_name == namespace_name {
                    self.insert_fn(sig);
                }
                continue;
            };
            let is_instance_method = first_param.name().is_some_and(|name| name.text() == "self");
            if is_instance_method {
                if let Some(receiver) = receiver {
                    let sig = collect_method_sig(&fn_decl, receiver);
                    self.methods
                        .entry((receiver, sig.name.clone()))
                        .or_insert(sig);
                } else if let Some(receiver) = namespace_scalar_receiver(&namespace_name) {
                    let sig = collect_method_sig_for_scalar(&fn_decl, receiver);
                    self.scalar_methods
                        .entry((receiver, sig.name.clone()))
                        .or_insert(sig);
                }
                continue;
            }

            let sig = collect_fn_sig(&fn_decl, receiver);
            self.insert_static_method(&namespace_name, sig.clone());
            if receiver.is_some() && fn_name == namespace_name {
                self.insert_fn(sig);
            }
        }
    }

    fn insert_fn(&mut self, sig: FnSig) {
        self.fns.entry(sig.name.clone()).or_default().push(sig);
    }

    fn insert_static_method(&mut self, namespace: &str, sig: FnSig) {
        self.static_methods
            .entry((namespace.to_string(), sig.name.clone()))
            .or_default()
            .push(sig);
    }

    fn rebuild_name_indexes(&mut self) {
        self.fn_names = self.fns.keys().cloned().collect();
        self.fn_names.sort();
        self.namespace_names.sort();
        self.namespace_names.dedup();

        for ((receiver, method_name), _) in &self.methods {
            self.method_names
                .entry(*receiver)
                .or_default()
                .push(method_name.clone());
        }

        for method_names in self.method_names.values_mut() {
            method_names.sort();
            method_names.dedup();
        }
    }
}

fn top_level_namespaces(file: &SourceFile) -> Vec<NamespaceDecl> {
    file.syntax()
        .children()
        .filter_map(NamespaceDecl::cast)
        .collect()
}

fn namespace_receiver(namespace: &NamespaceDecl) -> Option<Builtin> {
    namespace
        .target()
        .as_ref()
        .and_then(type_path_builtin)
        .or_else(|| {
            namespace
                .name()
                .and_then(|name| builtin_from_name(name.text()))
        })
}

fn namespace_name(namespace: &NamespaceDecl) -> String {
    namespace
        .name()
        .map(|name| name.text().to_string())
        .unwrap_or_default()
}

fn namespace_scalar_receiver(name: &str) -> Option<Scalar> {
    match name {
        "String" => Some(Scalar::String),
        _ => None,
    }
}

fn collect_fn_sig(decl: &FnDecl, self_ty: Option<Builtin>) -> FnSig {
    let type_params = collect_type_param_names(decl.syntax());
    let lowerer = TypeLowerer {
        type_params: &type_params,
        self_ty,
        self_scalar: None,
    };

    let positional_params = decl
        .parameters()
        .filter_map(|param| {
            let name = param.name()?.text().to_string();
            let ty = param
                .slot_type()
                .and_then(|slot| slot.ty())
                .map(|ty| lowerer.lower(&ty))
                .unwrap_or(Ty::Poison);
            Some(ParamDef { name, ty })
        })
        .collect();

    let keyword_params = decl
        .keyword_parameters()
        .filter_map(|param| {
            let name = param.name()?.text().to_string();
            let ty = param
                .slot_type()
                .and_then(|slot| slot.ty())
                .map(|ty| lowerer.lower(&ty))
                .unwrap_or(Ty::Poison);
            let has_default = param
                .syntax()
                .children()
                .any(|child| vx_parser::Expr::cast(child).is_some());
            Some(KeywordParamDef {
                name,
                ty,
                has_default,
            })
        })
        .collect();

    let ret = decl
        .return_type()
        .and_then(|ret| ret.ty())
        .map(|ty| lowerer.lower(&ty))
        .unwrap_or(Ty::Unit);

    let capabilities = collect_capabilities(decl);

    FnSig {
        name: decl
            .name()
            .map(|name| name.text().to_string())
            .unwrap_or_default(),
        type_params,
        positional_params,
        keyword_params,
        ret,
        capabilities,
    }
}

fn collect_method_sig(decl: &FnDecl, receiver: Builtin) -> MethodSig {
    let mut sig = collect_fn_sig(decl, Some(receiver));
    if !sig.positional_params.is_empty() {
        sig.positional_params.remove(0);
    }
    let capabilities = if receiver == Builtin::Host && sig.capabilities.is_empty() {
        vec![Capability {
            name: "host".into(),
        }]
    } else {
        sig.capabilities
    };
    MethodSig {
        name: sig.name,
        params: sig.positional_params,
        ret: sig.ret,
        capabilities,
    }
}

fn collect_method_sig_for_scalar(decl: &FnDecl, receiver: Scalar) -> MethodSig {
    let mut sig = collect_fn_sig_for_scalar(decl, receiver);
    if !sig.positional_params.is_empty() {
        sig.positional_params.remove(0);
    }
    MethodSig {
        name: sig.name,
        params: sig.positional_params,
        ret: sig.ret,
        capabilities: sig.capabilities,
    }
}

fn collect_fn_sig_for_scalar(decl: &FnDecl, self_scalar: Scalar) -> FnSig {
    let type_params = collect_type_param_names(decl.syntax());
    let lowerer = TypeLowerer {
        type_params: &type_params,
        self_ty: None,
        self_scalar: Some(self_scalar),
    };

    let positional_params = decl
        .parameters()
        .filter_map(|param| {
            let name = param.name()?.text().to_string();
            let ty = param
                .slot_type()
                .and_then(|slot| slot.ty())
                .map(|ty| lowerer.lower(&ty))
                .unwrap_or(Ty::Poison);
            Some(ParamDef { name, ty })
        })
        .collect();

    let keyword_params = decl
        .keyword_parameters()
        .filter_map(|param| {
            let name = param.name()?.text().to_string();
            let ty = param
                .slot_type()
                .and_then(|slot| slot.ty())
                .map(|ty| lowerer.lower(&ty))
                .unwrap_or(Ty::Poison);
            let has_default = param
                .syntax()
                .children()
                .any(|child| vx_parser::Expr::cast(child).is_some());
            Some(KeywordParamDef {
                name,
                ty,
                has_default,
            })
        })
        .collect();

    let ret = decl
        .return_type()
        .and_then(|ret| ret.ty())
        .map(|ty| lowerer.lower(&ty))
        .unwrap_or(Ty::Unit);

    let capabilities = collect_capabilities(decl);

    FnSig {
        name: decl
            .name()
            .map(|name| name.text().to_string())
            .unwrap_or_default(),
        type_params,
        positional_params,
        keyword_params,
        ret,
        capabilities,
    }
}

fn collect_type_param_names(node: &vx_parser::SyntaxNode) -> Vec<String> {
    node.children()
        .find(|child| child.kind() == SyntaxKind::TYPE_PARAM_LIST)
        .map(|list| {
            list.children_with_tokens()
                .filter_map(|child| child.into_token())
                .filter(|token| token.kind() == SyntaxKind::UPPER_IDENT)
                .map(|token| token.text().to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn collect_capabilities(decl: &FnDecl) -> Vec<Capability> {
    let Some(cap_list) = decl.capability_list() else {
        return Vec::new();
    };
    cap_list
        .tags()
        .filter_map(|tag| {
            Some(Capability {
                name: tag.name()?.text().to_string(),
            })
        })
        .collect()
}

struct TypeLowerer<'a> {
    type_params: &'a [String],
    self_ty: Option<Builtin>,
    self_scalar: Option<Scalar>,
}

impl TypeLowerer<'_> {
    fn lower(&self, ty: &vx_parser::Type) -> Ty {
        use vx_parser::Type;

        match ty {
            Type::Path(path) => self.lower_type_path(path),
            Type::Optional(optional) => {
                let inner = optional
                    .inner()
                    .map(|inner| self.lower(&inner))
                    .unwrap_or(Ty::Poison);
                let has_question = optional
                    .syntax()
                    .children_with_tokens()
                    .any(|child| child.kind() == SyntaxKind::QUESTION);
                if has_question {
                    Ty::Optional(Box::new(inner))
                } else {
                    inner
                }
            }
            Type::Tuple(tuple) => Ty::Tuple(vx_types::TupleTy {
                elements: tuple.items().map(|item| self.lower(&item)).collect(),
            }),
            Type::Function(function) => {
                let params = function
                    .parameter_types()
                    .map(|param| self.lower(&param))
                    .collect();
                let ret = function
                    .return_type()
                    .map(|ret| self.lower(&ret))
                    .unwrap_or(Ty::Unit);
                let capabilities = function
                    .capability_list()
                    .map(|cap_list| {
                        cap_list
                            .tags()
                            .filter_map(|tag| {
                                Some(Capability {
                                    name: tag.name()?.text().to_string(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                Ty::Function(FunctionTy {
                    params,
                    ret: Box::new(ret),
                    capabilities,
                })
            }
            Type::Paren(paren) => paren
                .inner()
                .map(|inner| self.lower(&inner))
                .unwrap_or(Ty::Poison),
            Type::Sum(sum) => sum
                .variants()
                .next()
                .map(|variant| self.lower(&variant))
                .unwrap_or(Ty::Poison),
        }
    }

    fn lower_type_path(&self, path: &TypePath) -> Ty {
        let segments: Vec<String> = path
            .segments()
            .map(|segment| segment.text().to_string())
            .collect();
        let Some(name) = segments.first() else {
            return Ty::Poison;
        };

        if name == "Self"
            && let Some(self_ty) = self.self_ty
        {
            return Ty::Builtin(self_ty);
        }
        if name == "Self"
            && let Some(self_scalar) = self.self_scalar
        {
            return Ty::Scalar(self_scalar);
        }

        if let Some(index) = self.type_params.iter().position(|param| param == name) {
            return Ty::Param(TypeParam {
                name: name.clone(),
                index: index as u32,
            });
        }

        match name.as_str() {
            "U64" => return Ty::Scalar(Scalar::U64),
            "I64" => return Ty::Scalar(Scalar::I64),
            "F64" => return Ty::Scalar(Scalar::F64),
            "Bool" => return Ty::Scalar(Scalar::Bool),
            "String" => return Ty::Scalar(Scalar::String),
            "Unit" => return Ty::Unit,
            _ => {}
        }

        if let Some(builtin) = builtin_from_name(name) {
            return Ty::Builtin(builtin);
        }

        let type_args: Vec<Ty> = path
            .type_args()
            .map(|args| args.types().map(|ty| self.lower(&ty)).collect())
            .unwrap_or_default();

        match name.as_str() {
            "Option" => Ty::Optional(Box::new(type_args.into_iter().next().unwrap_or(Ty::Poison))),
            "Array" => Ty::Array(Box::new(type_args.into_iter().next().unwrap_or(Ty::Poison))),
            "Map" => {
                let mut args = type_args.into_iter();
                Ty::Map(
                    Box::new(args.next().unwrap_or(Ty::Poison)),
                    Box::new(args.next().unwrap_or(Ty::Poison)),
                )
            }
            "Stream" => Ty::Stream(Box::new(type_args.into_iter().next().unwrap_or(Ty::Poison))),
            _ => Ty::Poison,
        }
    }
}

fn type_path_builtin(path: &TypePath) -> Option<Builtin> {
    let name = path.segments().next()?;
    builtin_from_name(name.text())
}

fn builtin_from_name(name: &str) -> Option<Builtin> {
    match name {
        "Path" => Some(Builtin::Path),
        "Blob" => Some(Builtin::Blob),
        "Tree" => Some(Builtin::Tree),
        "ExecPath" => Some(Builtin::ExecPath),
        "Process" => Some(Builtin::Process),
        "Host" => Some(Builtin::Host),
        "ExecutorClass" => Some(Builtin::ExecutorClass),
        _ => None,
    }
}

fn fallback_builtin_fn_names() -> &'static [&'static str] {
    &[
        "build",
        "read_file",
        "read_dir",
        "glob",
        "exists",
        "metadata",
    ]
}

fn fallback_builtin_method_names(receiver: &Builtin) -> &'static [&'static str] {
    match receiver {
        Builtin::Process => &["exit_code", "stdout", "stderr"],
        _ => &[],
    }
}

fn fallback_builtin_fn_sig(name: &str) -> Option<FnSig> {
    match name {
        // r[eval.entry] — build(entry: Fn(Path, Array[String], Map[String, String]) #fs #discover -> T) -> T
        "build" => Some(FnSig {
            name: "build".into(),
            type_params: vec!["T".into()],
            positional_params: vec![ParamDef {
                name: "entry".into(),
                ty: Ty::Function(FunctionTy {
                    params: vec![
                        Ty::Builtin(Builtin::Path),
                        Ty::Array(Box::new(Ty::Scalar(Scalar::String))),
                        Ty::Map(
                            Box::new(Ty::Scalar(Scalar::String)),
                            Box::new(Ty::Scalar(Scalar::String)),
                        ),
                    ],
                    ret: Box::new(Ty::Param(TypeParam {
                        name: "T".into(),
                        index: 0,
                    })),
                    capabilities: vec![
                        Capability { name: "fs".into() },
                        Capability {
                            name: "discover".into(),
                        },
                    ],
                }),
            }],
            keyword_params: vec![],
            ret: Ty::Param(TypeParam {
                name: "T".into(),
                index: 0,
            }),
            capabilities: vec![],
        }),
        "read_file" => Some(FnSig {
            name: "read_file".into(),
            type_params: vec![],
            positional_params: vec![ParamDef {
                name: "path".into(),
                ty: Ty::Scalar(Scalar::String),
            }],
            keyword_params: vec![],
            ret: Ty::Builtin(Builtin::Blob),
            capabilities: vec![Capability { name: "fs".into() }],
        }),
        "read_dir" => Some(FnSig {
            name: "read_dir".into(),
            type_params: vec![],
            positional_params: vec![ParamDef {
                name: "path".into(),
                ty: Ty::Scalar(Scalar::String),
            }],
            keyword_params: vec![],
            ret: Ty::Array(Box::new(Ty::Scalar(Scalar::String))),
            capabilities: vec![Capability { name: "fs".into() }],
        }),
        "glob" => Some(FnSig {
            name: "glob".into(),
            type_params: vec![],
            positional_params: vec![ParamDef {
                name: "pattern".into(),
                ty: Ty::Scalar(Scalar::String),
            }],
            keyword_params: vec![],
            ret: Ty::Array(Box::new(Ty::Scalar(Scalar::String))),
            capabilities: vec![Capability { name: "fs".into() }],
        }),
        "exists" => Some(FnSig {
            name: "exists".into(),
            type_params: vec![],
            positional_params: vec![ParamDef {
                name: "path".into(),
                ty: Ty::Scalar(Scalar::String),
            }],
            keyword_params: vec![],
            ret: Ty::Scalar(Scalar::Bool),
            capabilities: vec![Capability { name: "fs".into() }],
        }),
        "metadata" => Some(FnSig {
            name: "metadata".into(),
            type_params: vec![],
            positional_params: vec![ParamDef {
                name: "path".into(),
                ty: Ty::Scalar(Scalar::String),
            }],
            keyword_params: vec![],
            ret: Ty::Map(
                Box::new(Ty::Scalar(Scalar::String)),
                Box::new(Ty::Scalar(Scalar::String)),
            ),
            capabilities: vec![Capability { name: "fs".into() }],
        }),
        _ => None,
    }
}

fn fallback_builtin_method(receiver: &Builtin, method_name: &str) -> Option<MethodSig> {
    match (receiver, method_name) {
        (Builtin::Process, "exit_code") => Some(MethodSig {
            name: "exit_code".into(),
            params: vec![],
            ret: Ty::Scalar(Scalar::U64),
            capabilities: vec![],
        }),
        (Builtin::Process, "stdout") => Some(MethodSig {
            name: "stdout".into(),
            params: vec![],
            ret: Ty::Builtin(Builtin::Blob),
            capabilities: vec![],
        }),
        (Builtin::Process, "stderr") => Some(MethodSig {
            name: "stderr".into(),
            params: vec![],
            ret: Ty::Builtin(Builtin::Blob),
            capabilities: vec![],
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use vx_types::Scalar;

    use super::{
        builtin_fn_sig, builtin_fn_sigs, builtin_method, builtin_scalar_method,
        builtin_static_method_sigs, is_builtin_namespace,
    };

    #[test]
    fn stdlib_host_methods_are_loaded() {
        let which = builtin_method(&vx_types::Builtin::Host, "which").expect("host.which");
        assert_eq!(which.ret, vx_types::Ty::Scalar(Scalar::String));
        assert_eq!(which.capabilities[0].name, "host");

        let hash = builtin_method(&vx_types::Builtin::Host, "hash").expect("host.hash");
        assert_eq!(hash.ret, vx_types::Ty::Builtin(vx_types::Builtin::Blob));
        assert_eq!(hash.capabilities[0].name, "host");
    }

    #[test]
    fn stdlib_tree_methods_are_loaded() {
        let merge = builtin_method(&vx_types::Builtin::Tree, "merge").expect("tree.merge");
        assert_eq!(merge.ret, vx_types::Ty::Builtin(vx_types::Builtin::Tree));
    }

    #[test]
    fn stdlib_string_methods_are_loaded() {
        let split_once =
            builtin_scalar_method(&Scalar::String, "split_once").expect("string.split_once");
        assert!(matches!(split_once.ret, vx_types::Ty::Optional(_)));

        let len = builtin_scalar_method(&Scalar::String, "len").expect("string.len");
        assert_eq!(len.ret, vx_types::Ty::Scalar(Scalar::U64));
    }

    #[test]
    fn stdlib_tree_constructors_are_loaded() {
        let constructors = builtin_fn_sigs("Tree");
        assert_eq!(constructors.len(), 3);
        assert!(
            constructors
                .iter()
                .any(|sig| sig.ret == vx_types::Ty::Builtin(vx_types::Builtin::Tree))
        );
        assert!(builtin_fn_sig("Tree").is_some());
    }

    #[test]
    fn stdlib_static_namespace_methods_are_loaded() {
        assert!(is_builtin_namespace("Toml"));
        assert!(is_builtin_namespace("Http"));

        let toml_parse = builtin_static_method_sigs("Toml", "parse");
        assert_eq!(toml_parse.len(), 1);
        assert!(matches!(toml_parse[0].ret, vx_types::Ty::Param(_)));

        let http_get = builtin_static_method_sigs("Http", "get");
        assert_eq!(http_get.len(), 1);
        assert_eq!(
            http_get[0].ret,
            vx_types::Ty::Builtin(vx_types::Builtin::Blob)
        );
    }

    #[test]
    fn stdlib_free_functions_are_loaded() {
        let discover = builtin_fn_sig("discover").expect("discover");
        assert_eq!(discover.positional_params.len(), 2);
        assert_eq!(discover.capabilities[0].name, "discover");
    }

    #[test]
    fn fallback_free_functions_remain_available() {
        let read_file = builtin_fn_sig("read_file").expect("read_file");
        assert_eq!(
            read_file.ret,
            vx_types::Ty::Builtin(vx_types::Builtin::Blob)
        );
    }
}
