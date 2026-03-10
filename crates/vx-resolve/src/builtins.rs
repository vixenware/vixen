//! Builtin function and method signatures for the Vixen runtime surface.
//!
//! These are the functions and methods that exist in the Vixen runtime
//! but aren't declared in user source code.

use vx_types::{Builtin, Capability, FnSig, KeywordParamDef, ParamDef, Ty};

/// A method signature on a builtin type.
pub struct MethodSig {
    pub name: &'static str,
    pub params: Vec<ParamDef>,
    pub ret: Ty,
}

/// Look up a builtin free function signature by name.
pub fn builtin_fn_sig(name: &str) -> Option<FnSig> {
    match name {
        // r[eval.entry] — build(entry: Fn(Path, Array[String], Map[String, String]) #fs -> T) -> T
        "build" => Some(FnSig {
            name: "build".into(),
            type_params: vec![],
            positional_params: vec![ParamDef {
                name: "entry".into(),
                ty: Ty::Function(vx_types::FunctionTy {
                    params: vec![
                        Ty::Builtin(Builtin::Path),
                        Ty::Array(Box::new(Ty::Scalar(vx_types::Scalar::String))),
                        Ty::Map(
                            Box::new(Ty::Scalar(vx_types::Scalar::String)),
                            Box::new(Ty::Scalar(vx_types::Scalar::String)),
                        ),
                    ],
                    ret: Box::new(Ty::Unit),
                    capabilities: vec![Capability { name: "fs".into() }],
                }),
            }],
            keyword_params: vec![],
            ret: Ty::Unit,
            capabilities: vec![],
        }),

        // r[stabilize.signature] — stabilize(witness: Fn() #fs -> W, resolve: Fn(W) #discover -> T) -> T
        "stabilize" => Some(FnSig {
            name: "stabilize".into(),
            type_params: vec![],
            positional_params: vec![
                ParamDef {
                    name: "witness".into(),
                    ty: Ty::Function(vx_types::FunctionTy {
                        params: vec![],
                        ret: Box::new(Ty::Unit),
                        capabilities: vec![Capability { name: "fs".into() }],
                    }),
                },
                ParamDef {
                    name: "resolve".into(),
                    ty: Ty::Function(vx_types::FunctionTy {
                        params: vec![Ty::Unit],
                        ret: Box::new(Ty::Unit),
                        capabilities: vec![Capability {
                            name: "discover".into(),
                        }],
                    }),
                },
            ],
            keyword_params: vec![],
            ret: Ty::Unit,
            capabilities: vec![],
        }),

        // r[exec.signature]
        "exec" => Some(FnSig {
            name: "exec".into(),
            type_params: vec![],
            positional_params: vec![ParamDef {
                name: "command".into(),
                ty: Ty::Array(Box::new(Ty::Scalar(vx_types::Scalar::String))),
            }],
            keyword_params: vec![
                KeywordParamDef {
                    name: "observer".into(),
                    ty: Ty::Function(vx_types::FunctionTy {
                        params: vec![Ty::Builtin(Builtin::Process), Ty::Builtin(Builtin::Tree)],
                        ret: Box::new(Ty::Stream(Box::new(Ty::Unit))),
                        capabilities: vec![Capability {
                            name: "sandbox".into(),
                        }],
                    }),
                    has_default: true,
                },
                KeywordParamDef {
                    name: "verifier".into(),
                    ty: Ty::Function(vx_types::FunctionTy {
                        params: vec![Ty::Builtin(Builtin::Host)],
                        ret: Box::new(Ty::Unit),
                        capabilities: vec![Capability {
                            name: "host".into(),
                        }],
                    }),
                    has_default: true,
                },
            ],
            ret: Ty::Unit,
            capabilities: vec![],
        }),

        // r[exec.class] — executor_class(name: String) -> ExecutorClass
        "executor_class" => Some(FnSig {
            name: "executor_class".into(),
            type_params: vec![],
            positional_params: vec![ParamDef {
                name: "name".into(),
                ty: Ty::Scalar(vx_types::Scalar::String),
            }],
            keyword_params: vec![],
            ret: Ty::Builtin(Builtin::ExecutorClass),
            capabilities: vec![],
        }),

        // r[exec.discover] — discover_exec(class: String, probe: Array[String]) -> DiscoveryProcess
        "discover_exec" => Some(FnSig {
            name: "discover_exec".into(),
            type_params: vec![],
            positional_params: vec![
                ParamDef {
                    name: "class".into(),
                    ty: Ty::Scalar(vx_types::Scalar::String),
                },
                ParamDef {
                    name: "probe".into(),
                    ty: Ty::Array(Box::new(Ty::Scalar(vx_types::Scalar::String))),
                },
            ],
            keyword_params: vec![],
            ret: Ty::Builtin(Builtin::DiscoveryProcess),
            capabilities: vec![Capability {
                name: "discover".into(),
            }],
        }),

        // r[cap.kinds] — fs operations
        "read_file" => Some(FnSig {
            name: "read_file".into(),
            type_params: vec![],
            positional_params: vec![ParamDef {
                name: "path".into(),
                ty: Ty::Scalar(vx_types::Scalar::String),
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
                ty: Ty::Scalar(vx_types::Scalar::String),
            }],
            keyword_params: vec![],
            ret: Ty::Array(Box::new(Ty::Scalar(vx_types::Scalar::String))),
            capabilities: vec![Capability { name: "fs".into() }],
        }),

        "glob" => Some(FnSig {
            name: "glob".into(),
            type_params: vec![],
            positional_params: vec![ParamDef {
                name: "pattern".into(),
                ty: Ty::Scalar(vx_types::Scalar::String),
            }],
            keyword_params: vec![],
            ret: Ty::Array(Box::new(Ty::Scalar(vx_types::Scalar::String))),
            capabilities: vec![Capability { name: "fs".into() }],
        }),

        "exists" => Some(FnSig {
            name: "exists".into(),
            type_params: vec![],
            positional_params: vec![ParamDef {
                name: "path".into(),
                ty: Ty::Scalar(vx_types::Scalar::String),
            }],
            keyword_params: vec![],
            ret: Ty::Scalar(vx_types::Scalar::Bool),
            capabilities: vec![Capability { name: "fs".into() }],
        }),

        "metadata" => Some(FnSig {
            name: "metadata".into(),
            type_params: vec![],
            positional_params: vec![ParamDef {
                name: "path".into(),
                ty: Ty::Scalar(vx_types::Scalar::String),
            }],
            keyword_params: vec![],
            ret: Ty::Map(
                Box::new(Ty::Scalar(vx_types::Scalar::String)),
                Box::new(Ty::Scalar(vx_types::Scalar::String)),
            ),
            capabilities: vec![Capability { name: "fs".into() }],
        }),

        _ => None,
    }
}

/// Look up a method signature on a builtin type.
pub fn builtin_method(receiver: &Builtin, method_name: &str) -> Option<MethodSig> {
    match (receiver, method_name) {
        // Host methods — r[exec.verifier.host]
        (Builtin::Host, "read") => Some(MethodSig {
            name: "read",
            params: vec![ParamDef {
                name: "path".into(),
                ty: Ty::Scalar(vx_types::Scalar::String),
            }],
            ret: Ty::Builtin(Builtin::Blob),
        }),
        (Builtin::Host, "hash") => Some(MethodSig {
            name: "hash",
            params: vec![ParamDef {
                name: "path".into(),
                ty: Ty::Scalar(vx_types::Scalar::String),
            }],
            ret: Ty::Scalar(vx_types::Scalar::String),
        }),
        (Builtin::Host, "which") => Some(MethodSig {
            name: "which",
            params: vec![ParamDef {
                name: "name".into(),
                ty: Ty::Scalar(vx_types::Scalar::String),
            }],
            ret: Ty::Builtin(Builtin::ExecPath),
        }),

        // Process methods
        (Builtin::Process, "exit_code") => Some(MethodSig {
            name: "exit_code",
            params: vec![],
            ret: Ty::Scalar(vx_types::Scalar::U64),
        }),
        (Builtin::Process, "stdout") => Some(MethodSig {
            name: "stdout",
            params: vec![],
            ret: Ty::Builtin(Builtin::Blob),
        }),
        (Builtin::Process, "stderr") => Some(MethodSig {
            name: "stderr",
            params: vec![],
            ret: Ty::Builtin(Builtin::Blob),
        }),

        // Blob methods
        (Builtin::Blob, "to_string") => Some(MethodSig {
            name: "to_string",
            params: vec![],
            ret: Ty::Scalar(vx_types::Scalar::String),
        }),

        // Tree methods
        (Builtin::Tree, "file") => Some(MethodSig {
            name: "file",
            params: vec![ParamDef {
                name: "path".into(),
                ty: Ty::Scalar(vx_types::Scalar::String),
            }],
            ret: Ty::Builtin(Builtin::Blob),
        }),

        _ => None,
    }
}
