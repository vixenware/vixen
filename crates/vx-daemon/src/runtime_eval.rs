use std::collections::{BTreeMap, HashMap};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;

use async_recursion::async_recursion;
use thiserror::Error;
use vox::Tx;
use vx_ir::compile::CompileError;
use vx_ir::exec_types::{
    BodyId, ExecutableClosure, ExecutableProgram, KernelOp, Operand, ResultSource,
    RuntimeBoundaryInputSpec, RuntimeBoundaryKind, RuntimeBoundaryPayload, RuntimeField,
    RuntimeMemberOp, SlotId,
};
use vx_parser::SourceId as ParserSourceId;
use vx_services::{
    BlobRef, DirectoryEntryKind, EvalProgressEvent, NodeHash, RuntimeValue, StoreClient,
    cas_tree::{MerkleTreeFile, build_merkle_tree},
    chunking::{prepare_blob, store_prepared_blob},
};
use vx_types::TypeEnv;
use vx_vfsd::{VfsdService, protocol::Vfsd};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum EvalValue {
    U64(u64),
    Bool(bool),
    String(String),
    Path(String),
    StringArray(Vec<String>),
    StringMap(BTreeMap<String, String>),
    Blob(Vec<u8>),
    Tree(NodeHash),
    SandboxTreeRoot(String),
    Pair(Box<EvalValue>, Box<EvalValue>),
    Array(Vec<EvalValue>),
    CallableAddConst(Box<EvalValue>),
    Host,
    Process {
        exit_code: u64,
        stdout: String,
        stderr: String,
    },
    ProcessStdout(String),
    ProcessStderr(String),
    Failure(String),
    Pending(String),
}

#[derive(Clone, Default)]
pub struct RuntimeEvalContext {
    pub store: Option<StoreClient>,
    pub vfsd: Option<VfsdService>,
}

#[derive(Debug, Error)]
pub enum RuntimeEvalError {
    #[error("compilation failed: {0}")]
    Compile(#[from] CompileError),
    #[error("arity mismatch in `{function}`: expected {expected} args, got {actual}")]
    ArityMismatch {
        function: String,
        expected: usize,
        actual: usize,
    },
    #[error("unsupported runtime boundary in `{function}`: {detail}")]
    UnsupportedBoundary { function: String, detail: String },
    #[error("unsupported operation in `{function}`: {detail}")]
    UnsupportedOperation { function: String, detail: String },
    #[error("missing slot s{slot} in `{function}`")]
    MissingSlot { function: String, slot: u32 },
    #[error("type mismatch in `{function}`: expected {expected}, found {found}")]
    TypeMismatch {
        function: String,
        expected: &'static str,
        found: &'static str,
    },
    #[error("negative integer literal {value} is not yet supported in `{function}`")]
    NegativeIntLiteral { function: String, value: i64 },
    #[error("function `{function}` produced a pending value: {label}")]
    PendingResult { function: String, label: String },
    #[error("function `{function}` failed: {message}")]
    FailureResult { function: String, message: String },
    #[error("function `{function}` returned an unsupported runtime value kind `{kind}`")]
    UnsupportedResultKind {
        function: String,
        kind: &'static str,
    },
}

/// Convert a [`RuntimeEvalError`] into a structured [`margin::Diagnostics`].
///
/// For [`RuntimeEvalError::Compile`], delegates to
/// [`vx_diagnostics::render_compile_error`] using the user's source text.
/// For runtime variants (which carry no source spans yet), produces a single
/// no-span report whose title summarizes the variant and whose note carries
/// the formatted [`Display`] message.
pub fn runtime_error_to_diagnostics(
    err: &RuntimeEvalError,
    user_source_text: &str,
    user_display_name: Option<&str>,
) -> margin::Diagnostics {
    use margin::{Diagnostics, Note, NoteKind, Report, Severity, Source, SourceId};

    match err {
        RuntimeEvalError::Compile(compile_err) => {
            // The TypeEnv is carried inside the relevant CompileError variants.
            // For variants that don't carry it (Parse / MissingEntry /
            // Lowering), an empty TypeEnv is fine — those code paths don't
            // format types.
            let fallback = TypeEnv::default();
            vx_diagnostics::render_compile_error(
                compile_err,
                user_source_text,
                ParserSourceId(0),
                user_display_name,
                &fallback,
            )
        }
        _ => {
            let title = match err {
                RuntimeEvalError::ArityMismatch { .. } => "arity mismatch",
                RuntimeEvalError::UnsupportedBoundary { .. } => "unsupported runtime boundary",
                RuntimeEvalError::UnsupportedOperation { .. } => "unsupported operation",
                RuntimeEvalError::MissingSlot { .. } => "missing slot",
                RuntimeEvalError::TypeMismatch { .. } => "runtime type mismatch",
                RuntimeEvalError::NegativeIntLiteral { .. } => "negative integer literal",
                RuntimeEvalError::PendingResult { .. } => "pending result",
                RuntimeEvalError::FailureResult { .. } => "explicit failure",
                RuntimeEvalError::UnsupportedResultKind { .. } => "unsupported result kind",
                RuntimeEvalError::Compile(_) => unreachable!(),
            };
            let report = Report {
                severity: Severity::Error,
                title: title.to_string(),
                annotations: Vec::new(),
                notes: vec![Note {
                    kind: NoteKind::Note,
                    text: err.to_string(),
                }],
                sections: Vec::new(),
            };
            let source = Source {
                id: SourceId(ParserSourceId(0).0.to_string()),
                name: user_display_name.unwrap_or("input.vx").to_string(),
                hyperlink: None,
                text: user_source_text.to_string(),
            };
            Diagnostics {
                sources: vec![source],
                reports: vec![report],
            }
        }
    }
}

/// Output from a successful evaluation.
pub struct EvalOutput {
    pub value: RuntimeValue,
}

pub struct EvalRequest<'a> {
    pub source: &'a str,
    pub function_name: &'a str,
    pub args: &'a [RuntimeValue],
}

pub async fn evaluate_source_text_function(
    context: &RuntimeEvalContext,
    request: EvalRequest<'_>,
    progress: Option<Tx<EvalProgressEvent>>,
) -> Result<EvalOutput, RuntimeEvalError> {
    let program = vx_ir::compile::compile_source(request.source, request.function_name)?;
    let body_id = program.entry;

    let eval_args = request
        .args
        .iter()
        .cloned()
        .map(eval_value_from_runtime)
        .collect::<Vec<_>>();
    let mut evaluator = ProgramEvaluator::new(context, &program, progress);
    let result = async {
        let value = evaluator
            .eval_body(request.function_name, body_id, &eval_args)
            .await?;
        let value = evaluator
            .finalize_value(request.function_name, value)
            .await?;
        runtime_value_from_eval(request.function_name, value)
    }
    .await;
    evaluator.cleanup_exec_prefixes().await;
    result.map(|value| EvalOutput { value })
}

fn eval_value_from_runtime(value: RuntimeValue) -> EvalValue {
    match value {
        RuntimeValue::U64(value) => EvalValue::U64(value),
        RuntimeValue::Bool(value) => EvalValue::Bool(value),
        RuntimeValue::String(value) => EvalValue::String(value),
        RuntimeValue::Path(value) => EvalValue::Path(value),
        RuntimeValue::StringArray(value) => EvalValue::StringArray(value),
        RuntimeValue::StringMap(value) => EvalValue::StringMap(value),
        RuntimeValue::Tree(hash) => EvalValue::Failure(format!(
            "tree runtime arguments are not yet supported: {}",
            hash.to_hex()
        )),
    }
}

fn runtime_value_from_eval(
    function_name: &str,
    value: EvalValue,
) -> Result<RuntimeValue, RuntimeEvalError> {
    match value {
        EvalValue::U64(value) => Ok(RuntimeValue::U64(value)),
        EvalValue::Bool(value) => Ok(RuntimeValue::Bool(value)),
        EvalValue::String(value) => Ok(RuntimeValue::String(value)),
        EvalValue::Path(value) => Ok(RuntimeValue::Path(value)),
        EvalValue::StringArray(value) => Ok(RuntimeValue::StringArray(value)),
        EvalValue::StringMap(value) => Ok(RuntimeValue::StringMap(value)),
        EvalValue::Tree(value) => Ok(RuntimeValue::Tree(value)),
        EvalValue::Pending(label) => Err(RuntimeEvalError::PendingResult {
            function: function_name.to_owned(),
            label,
        }),
        EvalValue::Failure(message) => Err(RuntimeEvalError::FailureResult {
            function: function_name.to_owned(),
            message,
        }),
        other => Err(RuntimeEvalError::UnsupportedResultKind {
            function: function_name.to_owned(),
            kind: other.kind_name(),
        }),
    }
}

/// The mount/writable/create-dirs description of a single exec boundary,
/// bundled so callers pass one value instead of three slices.
struct ExecBoundaryLayout<'a> {
    mounts: &'a [(vx_ir::exec_types::TreeFilePath, RuntimeBoundaryInputSpec)],
    writable_roots: &'a [vx_ir::exec_types::TreeFilePath],
    create_dirs: &'a [vx_ir::exec_types::TreeFilePath],
}

struct ProgramEvaluator<'a> {
    context: &'a RuntimeEvalContext,
    program: &'a ExecutableProgram,
    progress: Option<Tx<EvalProgressEvent>>,
    memo: HashMap<(BodyId, Vec<EvalValue>), EvalValue>,
    runtime_exec_prefixes: HashMap<vx_ir::exec_types::RuntimeNodeId, String>,
    next_exec_prefix: u64,
}

static NEXT_EXEC_PREFIX_ID: AtomicU64 = AtomicU64::new(0);

impl<'a> ProgramEvaluator<'a> {
    fn new(
        context: &'a RuntimeEvalContext,
        program: &'a ExecutableProgram,
        progress: Option<Tx<EvalProgressEvent>>,
    ) -> Self {
        Self {
            context,
            program,
            progress,
            memo: HashMap::new(),
            runtime_exec_prefixes: HashMap::new(),
            next_exec_prefix: NEXT_EXEC_PREFIX_ID.fetch_add(1, Ordering::Relaxed),
        }
    }

    async fn cleanup_exec_prefixes(&self) {
        let Some(vfsd) = self.context.vfsd.as_ref() else {
            return;
        };
        for prefix in self.runtime_exec_prefixes.values() {
            let _ = vfsd.rm_rf(format!("/{prefix}")).await;
        }
    }

    #[async_recursion]
    async fn eval_body(
        &mut self,
        function_name: &str,
        body_id: BodyId,
        args: &[EvalValue],
    ) -> Result<EvalValue, RuntimeEvalError> {
        let cache_key = (body_id, args.to_vec());
        if let Some(value) = self.memo.get(&cache_key) {
            return Ok(value.clone());
        }

        let body = self.program.bodies.get(&body_id).ok_or_else(|| {
            RuntimeEvalError::UnsupportedOperation {
                function: function_name.to_owned(),
                detail: format!("missing lowered body {}", body_id.0),
            }
        })?;
        if body.params.len() != args.len() {
            return Err(RuntimeEvalError::ArityMismatch {
                function: function_name.to_owned(),
                expected: body.params.len(),
                actual: args.len(),
            });
        }
        let mut slots = BTreeMap::new();
        for (slot, value) in body.params.iter().copied().zip(args.iter().cloned()) {
            slots.insert(slot, value);
        }
        let producers = body
            .ops
            .iter()
            .map(|op| (produced_slot(op), op))
            .collect::<HashMap<_, _>>();

        let value = self
            .read_result(
                function_name,
                body.result_source.clone(),
                &producers,
                &mut slots,
            )
            .await?;
        self.memo.insert(cache_key, value.clone());
        Ok(value)
    }

    #[async_recursion]
    async fn eval_op(
        &mut self,
        function_name: &str,
        producers: &HashMap<SlotId, &KernelOp>,
        slots: &mut BTreeMap<SlotId, EvalValue>,
        op: &KernelOp,
    ) -> Result<EvalValue, RuntimeEvalError> {
        match op {
            KernelOp::AssignConstInt { value, .. } => self.int_to_value(function_name, *value),
            KernelOp::AssignConstString { value, .. } => Ok(EvalValue::String(value.clone())),
            KernelOp::AssignConstBool { value, .. } => Ok(EvalValue::Bool(*value)),
            KernelOp::MakeBlob { value, .. } => {
                let value = self
                    .resolve_operand(function_name, producers, slots, value)
                    .await?;
                Ok(EvalValue::Blob(
                    self.expect_string(function_name, &value)?
                        .as_bytes()
                        .to_vec(),
                ))
            }
            KernelOp::AssignAlias { value, .. } => {
                self.resolve_operand(function_name, producers, slots, value)
                    .await
            }
            KernelOp::AssignLazyPoison { label, .. } => Ok(EvalValue::Pending(label.clone())),
            KernelOp::AllocThunk { .. } => Ok(EvalValue::Pending(
                "thunk (not yet supported in eager evaluator)".to_owned(),
            )),
            KernelOp::MakeStruct { .. } => Ok(EvalValue::Pending(
                "struct (not yet supported in eager evaluator)".to_owned(),
            )),
            KernelOp::ProjectNamedField { value, field, .. } => {
                let value = self
                    .resolve_operand(function_name, producers, slots, value)
                    .await?;
                self.eval_project_named_field(function_name, value, field)
            }
            KernelOp::AssignCallableAddConst { addend, .. } => {
                let addend = self
                    .resolve_operand(function_name, producers, slots, addend)
                    .await?;
                Ok(EvalValue::CallableAddConst(Box::new(addend)))
            }
            KernelOp::ApplyCallable { callable, arg, .. } => {
                let callable = self
                    .resolve_operand(function_name, producers, slots, callable)
                    .await?;
                let arg = self
                    .resolve_operand(function_name, producers, slots, arg)
                    .await?;
                match callable {
                    EvalValue::CallableAddConst(addend) => Ok(EvalValue::U64(
                        self.expect_u64(function_name, &arg)?
                            + self.expect_u64(function_name, &addend)?,
                    )),
                    other => Err(RuntimeEvalError::TypeMismatch {
                        function: function_name.to_owned(),
                        expected: "callable",
                        found: other.kind_name(),
                    }),
                }
            }
            KernelOp::MakePair { first, second, .. } => {
                let first = self
                    .resolve_operand(function_name, producers, slots, first)
                    .await?;
                let second = self
                    .resolve_operand(function_name, producers, slots, second)
                    .await?;
                Ok(EvalValue::Pair(Box::new(first), Box::new(second)))
            }
            KernelOp::ProjectIndex { pair, index, .. } => {
                let pair = self
                    .resolve_operand(function_name, producers, slots, pair)
                    .await?;
                match pair {
                    EvalValue::Pair(first, second) => match index {
                        0 => Ok(*first),
                        1 => Ok(*second),
                        other => Err(RuntimeEvalError::UnsupportedOperation {
                            function: function_name.to_owned(),
                            detail: format!("pair index {other} out of bounds"),
                        }),
                    },
                    other => Err(RuntimeEvalError::TypeMismatch {
                        function: function_name.to_owned(),
                        expected: "pair",
                        found: other.kind_name(),
                    }),
                }
            }
            KernelOp::Add { lhs, rhs, .. } => {
                let lhs = self
                    .resolve_operand(function_name, producers, slots, lhs)
                    .await?;
                let rhs = self
                    .resolve_operand(function_name, producers, slots, rhs)
                    .await?;
                Ok(EvalValue::U64(
                    self.expect_u64(function_name, &lhs)?
                        .wrapping_add(self.expect_u64(function_name, &rhs)?),
                ))
            }
            KernelOp::Equal { lhs, rhs, .. } => {
                let lhs = self
                    .resolve_operand(function_name, producers, slots, lhs)
                    .await?;
                let rhs = self
                    .resolve_operand(function_name, producers, slots, rhs)
                    .await?;
                Ok(EvalValue::Bool(lhs == rhs))
            }
            KernelOp::NotEqual { lhs, rhs, .. } => {
                let lhs = self
                    .resolve_operand(function_name, producers, slots, lhs)
                    .await?;
                let rhs = self
                    .resolve_operand(function_name, producers, slots, rhs)
                    .await?;
                Ok(EvalValue::Bool(lhs != rhs))
            }
            KernelOp::Sub { lhs, rhs, .. } => {
                let lhs = self
                    .resolve_operand(function_name, producers, slots, lhs)
                    .await?;
                let rhs = self
                    .resolve_operand(function_name, producers, slots, rhs)
                    .await?;
                Ok(EvalValue::U64(
                    self.expect_u64(function_name, &lhs)?
                        .wrapping_sub(self.expect_u64(function_name, &rhs)?),
                ))
            }
            KernelOp::MakeArray { items, .. } => {
                let mut resolved = Vec::with_capacity(items.len());
                for item in items {
                    resolved.push(
                        self.resolve_operand(function_name, producers, slots, item)
                            .await?,
                    );
                }
                Ok(EvalValue::Array(resolved))
            }
            KernelOp::BuildTree { entries, .. } => {
                let mut files = Vec::with_capacity(entries.len());
                for (path, operand) in entries {
                    let value = self
                        .resolve_operand(function_name, producers, slots, operand)
                        .await?;
                    let blob = match value {
                        EvalValue::Blob(bytes) => bytes,
                        other => {
                            return Err(RuntimeEvalError::TypeMismatch {
                                function: function_name.to_owned(),
                                expected: "blob",
                                found: other.kind_name(),
                            });
                        }
                    };
                    let store: StoreClient = self.store_client(function_name).await?;
                    let prepared = prepare_blob(&blob);
                    let blob_ref = store_prepared_blob(&store, &prepared, &blob)
                        .await
                        .map_err(|error| RuntimeEvalError::UnsupportedBoundary {
                            function: function_name.to_owned(),
                            detail: format!(
                                "failed to store blob for tree entry `{}`: {error}",
                                path.0
                            ),
                        })?;
                    let kind = match blob_ref {
                        BlobRef::Small { chunk, size } => DirectoryEntryKind::SmallFile {
                            content_hash: vx_services::ContentHash(chunk.0),
                            size,
                            executable: false,
                        },
                        BlobRef::Large {
                            content_hash,
                            blob_node,
                            size,
                        } => DirectoryEntryKind::LargeFile {
                            content_hash,
                            blob_node,
                            size,
                            executable: false,
                        },
                    };
                    files.push(MerkleTreeFile {
                        path: path.0.clone(),
                        kind,
                    });
                }
                let store: StoreClient = self.store_client(function_name).await?;
                let (tree, _) = build_merkle_tree(&store, files).await.map_err(|error| {
                    RuntimeEvalError::UnsupportedBoundary {
                        function: function_name.to_owned(),
                        detail: format!("failed to build tree: {error}"),
                    }
                })?;
                Ok(EvalValue::Tree(tree))
            }
            KernelOp::ConcatStrings { parts, .. } => {
                let mut out = String::new();
                for part in parts {
                    let value = self
                        .resolve_operand(function_name, producers, slots, part)
                        .await?;
                    match value {
                        EvalValue::String(value) => out.push_str(&value),
                        EvalValue::U64(value) => out.push_str(&value.to_string()),
                        EvalValue::Bool(value) => out.push_str(&value.to_string()),
                        other => {
                            return Err(RuntimeEvalError::TypeMismatch {
                                function: function_name.to_owned(),
                                expected: "string-coercible",
                                found: other.kind_name(),
                            });
                        }
                    }
                }
                Ok(EvalValue::String(out))
            }
            KernelOp::StringLen { value, .. } => {
                let value = self
                    .resolve_operand(function_name, producers, slots, value)
                    .await?;
                let value = self.expect_string(function_name, &value)?;
                Ok(EvalValue::U64(value.len() as u64))
            }
            KernelOp::StringSlice {
                value, start, end, ..
            } => {
                let value = self
                    .resolve_operand(function_name, producers, slots, value)
                    .await?;
                let value = self.expect_string(function_name, &value)?;
                let start = self
                    .resolve_operand(function_name, producers, slots, start)
                    .await?;
                let end = self
                    .resolve_operand(function_name, producers, slots, end)
                    .await?;
                let start = (self.expect_u64(function_name, &start)? as usize).min(value.len());
                let end = (self.expect_u64(function_name, &end)? as usize).min(value.len());
                let start = start.min(end);
                let slice = value.get(start..end).ok_or_else(|| {
                    RuntimeEvalError::UnsupportedOperation {
                        function: function_name.to_owned(),
                        detail: format!("string slice {start}..{end} must lie on utf-8 boundaries"),
                    }
                })?;
                Ok(EvalValue::String(slice.to_owned()))
            }
            KernelOp::Select {
                condition,
                then_value,
                else_value,
                ..
            } => {
                let condition = self
                    .resolve_operand(function_name, producers, slots, condition)
                    .await?;
                if self.expect_bool(function_name, &condition)? {
                    self.resolve_operand(function_name, producers, slots, then_value)
                        .await
                } else {
                    self.resolve_operand(function_name, producers, slots, else_value)
                        .await
                }
            }
            KernelOp::AssignFailure { message, .. } => {
                let value = self
                    .resolve_operand(function_name, producers, slots, message)
                    .await?;
                Ok(EvalValue::Failure(
                    self.expect_string(function_name, &value)?.to_owned(),
                ))
            }
            KernelOp::CallBody { callee, args, .. } => {
                let mut resolved = Vec::with_capacity(args.len());
                for arg in args {
                    resolved.push(
                        self.resolve_operand(function_name, producers, slots, arg)
                            .await?,
                    );
                }
                self.eval_body(function_name, *callee, &resolved).await
            }
            KernelOp::ProjectField { value, field, .. } => {
                let value = self
                    .resolve_operand(function_name, producers, slots, value)
                    .await?;
                self.eval_project_field(function_name, value, *field)
            }
            KernelOp::CallMember {
                receiver,
                method,
                args,
                ..
            } => {
                let receiver = self
                    .resolve_operand(function_name, producers, slots, receiver)
                    .await?;
                let mut resolved = Vec::with_capacity(args.len());
                for arg in args {
                    resolved.push(
                        self.resolve_operand(function_name, producers, slots, arg)
                            .await?,
                    );
                }
                self.eval_member_call(function_name, receiver, *method, resolved)
            }
            KernelOp::AwaitRuntime {
                node,
                positional_inputs,
                mounts,
                writable_roots,
                create_dirs,
                coarse_mounts,
                ..
            } => {
                self.eval_runtime_boundary(
                    function_name,
                    *node,
                    positional_inputs,
                    mounts,
                    writable_roots,
                    create_dirs,
                    coarse_mounts,
                    producers,
                    slots,
                )
                .await
            }
            KernelOp::AssignSandboxTreeRoot { node, .. } => {
                let Some(prefix) = self.runtime_exec_prefixes.get(node).cloned() else {
                    return Err(RuntimeEvalError::UnsupportedBoundary {
                        function: function_name.to_owned(),
                        detail: format!(
                            "sandbox tree root requested before exec node {} completed",
                            node.0
                        ),
                    });
                };
                Ok(EvalValue::SandboxTreeRoot(prefix))
            }
            KernelOp::AssignHostLocal { .. }
            | KernelOp::AllocClosure { .. }
            | KernelOp::ApplyClosure { .. } => Err(RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: format!("op {op:?}"),
            }),
        }
    }

    #[async_recursion]
    async fn read_result(
        &mut self,
        function_name: &str,
        result_source: ResultSource,
        producers: &HashMap<SlotId, &KernelOp>,
        slots: &mut BTreeMap<SlotId, EvalValue>,
    ) -> Result<EvalValue, RuntimeEvalError> {
        match result_source {
            ResultSource::Slot(slot) => self.read_slot(function_name, producers, slots, slot).await,
            ResultSource::PendingRuntimeBoundary => Err(RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: "pending runtime boundary result".to_owned(),
            }),
        }
    }

    #[async_recursion]
    async fn resolve_operand(
        &mut self,
        function_name: &str,
        producers: &HashMap<SlotId, &KernelOp>,
        slots: &mut BTreeMap<SlotId, EvalValue>,
        operand: &Operand,
    ) -> Result<EvalValue, RuntimeEvalError> {
        match operand {
            Operand::Slot(slot) => self.read_slot(function_name, producers, slots, *slot).await,
            Operand::Int(value) => self.int_to_value(function_name, *value),
            Operand::String(value) => Ok(EvalValue::String(value.clone())),
        }
    }

    #[async_recursion]
    async fn read_slot(
        &mut self,
        function_name: &str,
        producers: &HashMap<SlotId, &KernelOp>,
        slots: &mut BTreeMap<SlotId, EvalValue>,
        slot: SlotId,
    ) -> Result<EvalValue, RuntimeEvalError> {
        if let Some(value) = slots.get(&slot).cloned() {
            return Ok(value);
        }
        let op = producers
            .get(&slot)
            .copied()
            .ok_or_else(|| RuntimeEvalError::MissingSlot {
                function: function_name.to_owned(),
                slot: slot.0,
            })?;
        let value = self.eval_op(function_name, producers, slots, op).await?;
        slots.insert(slot, value.clone());
        Ok(value)
    }

    #[async_recursion]
    async fn eval_closure(
        &mut self,
        function_name: &str,
        closure: &ExecutableClosure,
        args: &[EvalValue],
    ) -> Result<EvalValue, RuntimeEvalError> {
        if closure.params.len() != args.len() {
            return Err(RuntimeEvalError::ArityMismatch {
                function: function_name.to_owned(),
                expected: closure.params.len(),
                actual: args.len(),
            });
        }

        let mut slots = BTreeMap::new();
        for (slot, value) in closure.params.iter().copied().zip(args.iter().cloned()) {
            slots.insert(slot, value);
        }
        let producers = closure
            .ops
            .iter()
            .map(|op| (produced_slot(op), op))
            .collect::<HashMap<_, _>>();
        self.read_result(
            function_name,
            closure.result_source.clone(),
            &producers,
            &mut slots,
        )
        .await
    }

    fn eval_member_call(
        &mut self,
        function_name: &str,
        receiver: EvalValue,
        method: RuntimeMemberOp,
        args: Vec<EvalValue>,
    ) -> Result<EvalValue, RuntimeEvalError> {
        match (receiver, method) {
            (EvalValue::ProcessStdout(stdout), RuntimeMemberOp::Collect) => {
                let [] = args.as_slice() else {
                    return Err(RuntimeEvalError::UnsupportedBoundary {
                        function: function_name.to_owned(),
                        detail: "stdout.collect expects no arguments".to_owned(),
                    });
                };
                Ok(EvalValue::String(stdout))
            }
            (EvalValue::ProcessStderr(stderr), RuntimeMemberOp::Collect) => {
                let [] = args.as_slice() else {
                    return Err(RuntimeEvalError::UnsupportedBoundary {
                        function: function_name.to_owned(),
                        detail: "stderr.collect expects no arguments".to_owned(),
                    });
                };
                Ok(EvalValue::String(stderr))
            }
            (EvalValue::Host, RuntimeMemberOp::Which) => {
                let [name] = args.as_slice() else {
                    return Err(RuntimeEvalError::UnsupportedBoundary {
                        function: function_name.to_owned(),
                        detail: "host.which expects exactly one argument".to_owned(),
                    });
                };
                let name = self.expect_string(function_name, name)?;
                let path = find_host_tool(name).ok_or_else(|| RuntimeEvalError::FailureResult {
                    function: function_name.to_owned(),
                    message: format!("host.which could not find `{name}`"),
                })?;
                Ok(EvalValue::String(path))
            }
            (receiver, method) => Err(RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: format!(
                    "unsupported member call {method:?} on {}",
                    receiver.kind_name()
                ),
            }),
        }
    }

    fn eval_project_field(
        &mut self,
        function_name: &str,
        value: EvalValue,
        field: RuntimeField,
    ) -> Result<EvalValue, RuntimeEvalError> {
        match (value, field) {
            (EvalValue::Process { exit_code, .. }, RuntimeField::ExitCode) => {
                Ok(EvalValue::U64(exit_code))
            }
            (EvalValue::Process { stdout, .. }, RuntimeField::Stdout) => {
                Ok(EvalValue::ProcessStdout(stdout))
            }
            (EvalValue::Process { stderr, .. }, RuntimeField::Stderr) => {
                Ok(EvalValue::ProcessStderr(stderr))
            }
            (value, field) => Err(RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: format!(
                    "unsupported field projection {field:?} on {}",
                    value.kind_name()
                ),
            }),
        }
    }

    fn eval_project_named_field(
        &mut self,
        function_name: &str,
        value: EvalValue,
        field: &str,
    ) -> Result<EvalValue, RuntimeEvalError> {
        match (value, field) {
            (EvalValue::Process { exit_code, .. }, "exit_code") => Ok(EvalValue::U64(exit_code)),
            (EvalValue::Process { stdout, .. }, "stdout") => Ok(EvalValue::ProcessStdout(stdout)),
            (EvalValue::Process { stderr, .. }, "stderr") => Ok(EvalValue::ProcessStderr(stderr)),
            (value, field) => Err(RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: format!(
                    "unsupported named field projection `{field}` on {}",
                    value.kind_name()
                ),
            }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn eval_runtime_boundary(
        &mut self,
        function_name: &str,
        node: vx_ir::exec_types::RuntimeNodeId,
        positional_inputs: &[RuntimeBoundaryInputSpec],
        mounts: &[(vx_ir::exec_types::TreeFilePath, RuntimeBoundaryInputSpec)],
        writable_roots: &[vx_ir::exec_types::TreeFilePath],
        create_dirs: &[vx_ir::exec_types::TreeFilePath],
        coarse_mounts: &[RuntimeBoundaryInputSpec],
        producers: &HashMap<SlotId, &KernelOp>,
        slots: &mut BTreeMap<SlotId, EvalValue>,
    ) -> Result<EvalValue, RuntimeEvalError> {
        if !coarse_mounts.is_empty() {
            return Err(RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: "coarse_mounts are not yet supported in local runtime eval".to_owned(),
            });
        }

        let runtime_node = self.program.runtime_nodes.get(&node).ok_or_else(|| {
            RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: format!("missing runtime node {}", node.0),
            }
        })?;

        match (&runtime_node.kind, &runtime_node.payload) {
            (RuntimeBoundaryKind::Exec, RuntimeBoundaryPayload::Exec(_payload)) => {
                let input = positional_inputs.first().ok_or_else(|| {
                    RuntimeEvalError::UnsupportedBoundary {
                        function: function_name.to_owned(),
                        detail: "exec boundary is missing argv input".to_owned(),
                    }
                })?;
                let argv =
                    if mounts.is_empty() && writable_roots.is_empty() && create_dirs.is_empty() {
                        self.resolve_exec_argv(function_name, input, producers, slots)
                            .await?
                    } else {
                        let (access_path, prefix) = self
                            .prepare_local_exec_vfs(
                                function_name,
                                node,
                                ExecBoundaryLayout {
                                    mounts,
                                    writable_roots,
                                    create_dirs,
                                },
                                producers,
                                slots,
                            )
                            .await?;
                        self.resolve_exec_argv_for_prefix(
                            function_name,
                            input,
                            producers,
                            slots,
                            &access_path,
                            &prefix,
                        )
                        .await?
                    };
                if argv.is_empty() {
                    return Err(RuntimeEvalError::UnsupportedBoundary {
                        function: function_name.to_owned(),
                        detail: "exec argv must not be empty".to_owned(),
                    });
                }
                let process = self.run_local_exec(function_name, &argv).await?;
                let observer = positional_inputs.iter().find_map(|input| match input {
                    RuntimeBoundaryInputSpec::Closure { closure, .. } => Some(closure),
                    _ => None,
                });
                if let Some(observer) = observer {
                    let vfs_root = self
                        .runtime_exec_prefixes
                        .get(&node)
                        .cloned()
                        .map(EvalValue::SandboxTreeRoot)
                        .unwrap_or_else(|| {
                            EvalValue::Pending(
                                "exec observer vfs root is unavailable in local runtime eval"
                                    .to_owned(),
                            )
                        });
                    self.eval_closure(function_name, observer, &[process, vfs_root])
                        .await
                } else {
                    Ok(process)
                }
            }
            (RuntimeBoundaryKind::Discover, RuntimeBoundaryPayload::Discover(_payload)) => {
                let closure = positional_inputs
                    .iter()
                    .find_map(|input| match input {
                        RuntimeBoundaryInputSpec::Closure { closure, .. } => Some(closure),
                        _ => None,
                    })
                    .ok_or_else(|| RuntimeEvalError::UnsupportedBoundary {
                        function: function_name.to_owned(),
                        detail: "discover boundary is missing probe closure".to_owned(),
                    })?;
                self.eval_closure(function_name, closure, &[EvalValue::Host])
                    .await
            }
            (kind, payload) => Err(RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: format!("runtime boundary {kind:?} with payload {payload:?}"),
            }),
        }
    }

    async fn resolve_exec_argv(
        &mut self,
        function_name: &str,
        input: &RuntimeBoundaryInputSpec,
        producers: &HashMap<SlotId, &KernelOp>,
        slots: &mut BTreeMap<SlotId, EvalValue>,
    ) -> Result<Vec<String>, RuntimeEvalError> {
        let RuntimeBoundaryInputSpec::Array(items) = input else {
            return Err(RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: format!("exec argv must be an array input, got {input:?}"),
            });
        };
        let mut argv = Vec::with_capacity(items.len());
        for item in items {
            argv.push(
                self.resolve_runtime_string_input(function_name, item, producers, slots)
                    .await?,
            );
        }
        Ok(argv)
    }

    async fn resolve_exec_argv_for_prefix(
        &mut self,
        function_name: &str,
        input: &RuntimeBoundaryInputSpec,
        producers: &HashMap<SlotId, &KernelOp>,
        slots: &mut BTreeMap<SlotId, EvalValue>,
        access_path: &str,
        prefix: &str,
    ) -> Result<Vec<String>, RuntimeEvalError> {
        let RuntimeBoundaryInputSpec::Array(items) = input else {
            return Err(RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: format!("exec argv must be an array input, got {input:?}"),
            });
        };
        let mut argv = Vec::with_capacity(items.len());
        for item in items {
            argv.push(
                self.resolve_exec_string_input_for_prefix(
                    function_name,
                    item,
                    producers,
                    slots,
                    access_path,
                    prefix,
                )
                .await?,
            );
        }
        Ok(argv)
    }

    async fn resolve_runtime_string_input(
        &mut self,
        function_name: &str,
        input: &RuntimeBoundaryInputSpec,
        producers: &HashMap<SlotId, &KernelOp>,
        slots: &mut BTreeMap<SlotId, EvalValue>,
    ) -> Result<String, RuntimeEvalError> {
        match input {
            RuntimeBoundaryInputSpec::Binding { slot, .. } => {
                let value = self
                    .read_slot(function_name, producers, slots, *slot)
                    .await?;
                Ok(self.expect_string(function_name, &value)?.to_owned())
            }
            RuntimeBoundaryInputSpec::IntLiteral(value) => Ok(value.to_string()),
            RuntimeBoundaryInputSpec::StringLiteral(value) => Ok(value.clone()),
            RuntimeBoundaryInputSpec::PathLiteral(path) => Ok(path.0.clone()),
            other => Err(RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: format!("unsupported runtime string input {other:?}"),
            }),
        }
    }

    async fn resolve_exec_string_input_for_prefix(
        &mut self,
        function_name: &str,
        input: &RuntimeBoundaryInputSpec,
        producers: &HashMap<SlotId, &KernelOp>,
        slots: &mut BTreeMap<SlotId, EvalValue>,
        access_path: &str,
        prefix: &str,
    ) -> Result<String, RuntimeEvalError> {
        match input {
            RuntimeBoundaryInputSpec::PathLiteral(path) => {
                Ok(format!("{access_path}/{prefix}/{}", path.0))
            }
            other => {
                self.resolve_runtime_string_input(function_name, other, producers, slots)
                    .await
            }
        }
    }

    async fn store_client(&self, function_name: &str) -> Result<StoreClient, RuntimeEvalError> {
        let Some(store) = self.context.store.clone() else {
            return Err(RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: "tree construction requires a configured store".to_owned(),
            });
        };
        Ok(store)
    }

    async fn finalize_value(
        &self,
        function_name: &str,
        value: EvalValue,
    ) -> Result<EvalValue, RuntimeEvalError> {
        match value {
            EvalValue::SandboxTreeRoot(prefix) => {
                let Some(vfsd) = self.context.vfsd.as_ref() else {
                    return Err(RuntimeEvalError::UnsupportedBoundary {
                        function: function_name.to_owned(),
                        detail: "sandbox tree result requires a hosted vfsd".to_owned(),
                    });
                };
                let hash = vfsd.flush(format!("/{prefix}")).await.map_err(|error| {
                    RuntimeEvalError::UnsupportedBoundary {
                        function: function_name.to_owned(),
                        detail: format!("failed to flush sandbox tree `{prefix}`: {error}"),
                    }
                })?;
                let _ = vfsd.rm_rf(format!("/{prefix}")).await;
                Ok(EvalValue::Tree(NodeHash(hash)))
            }
            other => Ok(other),
        }
    }

    async fn prepare_local_exec_vfs(
        &mut self,
        function_name: &str,
        node: vx_ir::exec_types::RuntimeNodeId,
        layout: ExecBoundaryLayout<'_>,
        producers: &HashMap<SlotId, &KernelOp>,
        slots: &mut BTreeMap<SlotId, EvalValue>,
    ) -> Result<(String, String), RuntimeEvalError> {
        let ExecBoundaryLayout {
            mounts,
            writable_roots,
            create_dirs,
        } = layout;
        let Some(vfsd) = self.context.vfsd.as_ref() else {
            return Err(RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: "exec mounts/writable roots require a hosted vfsd".to_owned(),
            });
        };

        let prefix = format!("eval-{:016x}", self.next_exec_prefix);
        self.next_exec_prefix += 1;
        self.emit_phase(format!("preparing exec sandbox `{prefix}`"))
            .await;

        for writable_root in writable_roots {
            self.emit_phase(format!("creating writable root `{}`", writable_root.0))
                .await;
            vfsd.mkdir(format!("/{prefix}/{}", writable_root.0))
                .await
                .map_err(|error| RuntimeEvalError::UnsupportedBoundary {
                    function: function_name.to_owned(),
                    detail: format!(
                        "failed to create writable root `{}`: {error}",
                        writable_root.0
                    ),
                })?;
        }

        for dir in create_dirs {
            self.emit_phase(format!("creating sandbox dir `{}`", dir.0))
                .await;
            vfsd.mkdir(format!("/{prefix}/{}", dir.0))
                .await
                .map_err(|error| RuntimeEvalError::UnsupportedBoundary {
                    function: function_name.to_owned(),
                    detail: format!("failed to create sandbox dir `{}`: {error}", dir.0),
                })?;
        }

        for (mount_path, input) in mounts {
            let tree = self
                .resolve_runtime_tree_input(function_name, input, producers, slots)
                .await?;
            self.emit_phase(format!(
                "mounting tree {} at `{}`",
                tree.to_hex(),
                mount_path.0
            ))
            .await;
            vfsd.mount(format!("/{prefix}/{}", mount_path.0), tree.0)
                .await
                .map_err(|error| RuntimeEvalError::UnsupportedBoundary {
                    function: function_name.to_owned(),
                    detail: format!("failed to mount `{}`: {error}", mount_path.0),
                })?;
        }

        self.runtime_exec_prefixes.insert(node, prefix.clone());
        let access_path =
            vfsd.access_path()
                .await
                .map_err(|error| RuntimeEvalError::UnsupportedBoundary {
                    function: function_name.to_owned(),
                    detail: format!("failed to query vfs access path: {error}"),
                })?;
        Ok((access_path, prefix))
    }

    async fn resolve_runtime_tree_input(
        &mut self,
        function_name: &str,
        input: &RuntimeBoundaryInputSpec,
        producers: &HashMap<SlotId, &KernelOp>,
        slots: &mut BTreeMap<SlotId, EvalValue>,
    ) -> Result<NodeHash, RuntimeEvalError> {
        match input {
            RuntimeBoundaryInputSpec::Binding { slot, .. } => {
                let value = self
                    .read_slot(function_name, producers, slots, *slot)
                    .await?;
                match value {
                    EvalValue::Tree(hash) => Ok(hash),
                    other => Err(RuntimeEvalError::TypeMismatch {
                        function: function_name.to_owned(),
                        expected: "tree",
                        found: other.kind_name(),
                    }),
                }
            }
            other => Err(RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: format!("unsupported runtime tree input {other:?}"),
            }),
        }
    }

    async fn run_local_exec(
        &mut self,
        function_name: &str,
        argv: &[String],
    ) -> Result<EvalValue, RuntimeEvalError> {
        self.emit_phase(format!("spawning `{}`", argv.join(" ")))
            .await;
        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: format!("local exec failed: {error}"),
            })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: "local exec stdout was not piped".to_owned(),
            })?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: "local exec stderr was not piped".to_owned(),
            })?;
        let (chunk_tx, mut chunk_rx) = mpsc::unbounded_channel();
        let stdout_task = tokio::spawn(read_exec_stream(
            stdout,
            ExecStream::Stdout,
            chunk_tx.clone(),
        ));
        let stderr_task = tokio::spawn(read_exec_stream(stderr, ExecStream::Stderr, chunk_tx));
        let mut wait = Box::pin(child.wait());
        let mut status = None;
        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();

        loop {
            tokio::select! {
                result = &mut wait, if status.is_none() => {
                    let exit = result.map_err(|error| RuntimeEvalError::UnsupportedBoundary {
                        function: function_name.to_owned(),
                        detail: format!("local exec wait failed: {error}"),
                    })?;
                    status = Some(exit);
                }
                maybe_chunk = chunk_rx.recv() => {
                    let Some((stream, chunk)) = maybe_chunk else {
                        if status.is_some() {
                            break;
                        }
                        continue;
                    };
                    match stream {
                        ExecStream::Stdout => {
                            stdout_buf.extend_from_slice(&chunk);
                            self.emit_stdout(String::from_utf8_lossy(&chunk).into_owned()).await;
                        }
                        ExecStream::Stderr => {
                            stderr_buf.extend_from_slice(&chunk);
                            self.emit_stderr(String::from_utf8_lossy(&chunk).into_owned()).await;
                        }
                    }
                }
            }
        }

        stdout_task
            .await
            .map_err(|error| RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: format!("stdout reader task failed: {error}"),
            })?;
        stderr_task
            .await
            .map_err(|error| RuntimeEvalError::UnsupportedBoundary {
                function: function_name.to_owned(),
                detail: format!("stderr reader task failed: {error}"),
            })?;
        let status = status.expect("process status should be observed before readers close");
        self.emit_phase(format!(
            "process exited with code {}",
            status.code().unwrap_or(1)
        ))
        .await;
        Ok(EvalValue::Process {
            exit_code: status.code().unwrap_or(1) as u64,
            stdout: String::from_utf8_lossy(&stdout_buf).into_owned(),
            stderr: String::from_utf8_lossy(&stderr_buf).into_owned(),
        })
    }

    async fn emit_phase(&self, message: String) {
        if let Some(progress) = self.progress.as_ref() {
            let _ = progress.send(format!("phase: {message}\n")).await;
        }
    }

    async fn emit_stdout(&self, chunk: String) {
        if let Some(progress) = self.progress.as_ref() {
            let _ = progress.send(chunk).await;
        }
    }

    async fn emit_stderr(&self, chunk: String) {
        if let Some(progress) = self.progress.as_ref() {
            let _ = progress.send(chunk).await;
        }
    }

    fn int_to_value(&self, function_name: &str, value: i64) -> Result<EvalValue, RuntimeEvalError> {
        u64::try_from(value)
            .map(EvalValue::U64)
            .map_err(|_| RuntimeEvalError::NegativeIntLiteral {
                function: function_name.to_owned(),
                value,
            })
    }

    fn expect_u64(&self, function_name: &str, value: &EvalValue) -> Result<u64, RuntimeEvalError> {
        match value {
            EvalValue::U64(value) => Ok(*value),
            other => Err(RuntimeEvalError::TypeMismatch {
                function: function_name.to_owned(),
                expected: "u64",
                found: other.kind_name(),
            }),
        }
    }

    fn expect_bool(
        &self,
        function_name: &str,
        value: &EvalValue,
    ) -> Result<bool, RuntimeEvalError> {
        match value {
            EvalValue::Bool(value) => Ok(*value),
            other => Err(RuntimeEvalError::TypeMismatch {
                function: function_name.to_owned(),
                expected: "bool",
                found: other.kind_name(),
            }),
        }
    }

    fn expect_string<'b>(
        &self,
        function_name: &str,
        value: &'b EvalValue,
    ) -> Result<&'b str, RuntimeEvalError> {
        match value {
            EvalValue::String(value) => Ok(value),
            other => Err(RuntimeEvalError::TypeMismatch {
                function: function_name.to_owned(),
                expected: "string",
                found: other.kind_name(),
            }),
        }
    }
}

#[derive(Clone, Copy)]
enum ExecStream {
    Stdout,
    Stderr,
}

async fn read_exec_stream<R>(
    mut reader: R,
    stream: ExecStream,
    chunk_tx: mpsc::UnboundedSender<(ExecStream, Vec<u8>)>,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buffer = vec![0_u8; 8 * 1024];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(len) => {
                if chunk_tx.send((stream, buffer[..len].to_vec())).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

impl EvalValue {
    const fn kind_name(&self) -> &'static str {
        match self {
            Self::U64(_) => "u64",
            Self::Bool(_) => "bool",
            Self::String(_) => "string",
            Self::Path(_) => "path",
            Self::StringArray(_) => "string-array",
            Self::StringMap(_) => "string-map",
            Self::Blob(_) => "blob",
            Self::Tree(_) => "tree",
            Self::SandboxTreeRoot(_) => "sandbox-tree-root",
            Self::Pair(_, _) => "pair",
            Self::Array(_) => "array",
            Self::CallableAddConst(_) => "callable",
            Self::Host => "host",
            Self::Process { .. } => "process",
            Self::ProcessStdout(_) => "process-stdout",
            Self::ProcessStderr(_) => "process-stderr",
            Self::Failure(_) => "failure",
            Self::Pending(_) => "pending",
        }
    }
}

fn produced_slot(op: &KernelOp) -> SlotId {
    match op {
        KernelOp::AssignHostLocal { dst, .. }
        | KernelOp::AssignSandboxTreeRoot { dst, .. }
        | KernelOp::AssignConstInt { dst, .. }
        | KernelOp::AssignConstString { dst, .. }
        | KernelOp::MakeBlob { dst, .. }
        | KernelOp::AssignAlias { dst, .. }
        | KernelOp::AssignLazyPoison { dst, .. }
        | KernelOp::AllocThunk { dst, .. }
        | KernelOp::AssignCallableAddConst { dst, .. }
        | KernelOp::ApplyCallable { dst, .. }
        | KernelOp::AllocClosure { dst, .. }
        | KernelOp::ApplyClosure { dst, .. }
        | KernelOp::MakePair { dst, .. }
        | KernelOp::MakeStruct { dst, .. }
        | KernelOp::ProjectIndex { dst, .. }
        | KernelOp::ProjectNamedField { dst, .. }
        | KernelOp::Add { dst, .. }
        | KernelOp::AssignConstBool { dst, .. }
        | KernelOp::Equal { dst, .. }
        | KernelOp::NotEqual { dst, .. }
        | KernelOp::Sub { dst, .. }
        | KernelOp::ProjectField { dst, .. }
        | KernelOp::CallMember { dst, .. }
        | KernelOp::BuildTree { dst, .. }
        | KernelOp::MakeArray { dst, .. }
        | KernelOp::ConcatStrings { dst, .. }
        | KernelOp::StringLen { dst, .. }
        | KernelOp::StringSlice { dst, .. }
        | KernelOp::Select { dst, .. }
        | KernelOp::AssignFailure { dst, .. }
        | KernelOp::CallBody { dst, .. }
        | KernelOp::AwaitRuntime { dst, .. } => *dst,
    }
}

fn find_host_tool(name: &str) -> Option<String> {
    use std::path::{Path, PathBuf};

    fn candidate_if_file(path: PathBuf) -> Option<String> {
        path.is_file().then(|| path.to_string_lossy().into_owned())
    }

    if Path::new(name).components().count() > 1 {
        return candidate_if_file(PathBuf::from(name));
    }

    let path_var = std::env::var_os("PATH")?;

    #[cfg(windows)]
    let exts = std::env::var_os("PATHEXT")
        .map(|value| {
            std::env::split_paths(&value)
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| vec![".EXE".to_owned(), ".BAT".to_owned(), ".CMD".to_owned()]);

    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if let Some(path) = candidate_if_file(candidate) {
            return Some(path);
        }
        #[cfg(windows)]
        for ext in &exts {
            if let Some(path) = candidate_if_file(dir.join(format!("{name}{ext}"))) {
                return Some(path);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use vx_services::RuntimeValue;

    use super::{EvalRequest, RuntimeEvalContext, RuntimeEvalError, evaluate_source_text_function};

    const SOURCE_NON_STRICT_CALL: &str = r#"
fn greet(name: String) -> String (
  `hello {name}`
)

fn build(name: String) -> String (
  greet(name)
)
"#;

    const SOURCE_NON_STRICT_STRING_HELPERS: &str = r#"
fn measure(name: String) -> U64 (
  name.len()
)

fn prefix(name: String) -> String (
  name.slice(0..2)
)

fn build_len(name: String) -> U64 (
  measure(name)
)

fn build_slice(name: String) -> String (
  prefix(name)
)
"#;

    const SOURCE_DISCOVER_WHICH: &str = r#"
fn probe() #discover -> String (
  discover(executor_class("default"), (host) => host.which("sh"))
)
"#;

    const SOURCE_EXEC_STDOUT: &str = r#"
fn probe() #discover -> String (
  let sh = discover(executor_class("default"), (host) => host.which("sh"));
  exec [sh, "-c", "printf hello"] where {
    observer: (proc, _vfs) => proc.stdout.collect()
  }
)
"#;

    const SOURCE_EXEC_WRITABLE: &str = r#"
fn probe() #discover -> String (
  let sh = discover(executor_class("default"), (host) => host.which("sh"));
  exec [sh, "-c", "printf hello > out/greeting.txt"] where {
    writable: [@out],
    create_dirs: [@out],
    observer: (proc, _vfs) => proc.stdout.collect()
  }
)
"#;

    #[tokio::test]
    async fn evaluates_non_strict_top_level_body_call() {
        let output = evaluate_source_text_function(
            &RuntimeEvalContext::default(),
            EvalRequest {
                source: SOURCE_NON_STRICT_CALL,
                function_name: "build",
                args: &[RuntimeValue::String("serde".to_owned())],
            },
            None,
        )
        .await
        .expect("build should evaluate");

        assert_eq!(output.value, RuntimeValue::String("hello serde".to_owned()));
    }

    #[tokio::test]
    async fn evaluates_non_strict_string_len_via_local_call() {
        let output = evaluate_source_text_function(
            &RuntimeEvalContext::default(),
            EvalRequest {
                source: SOURCE_NON_STRICT_STRING_HELPERS,
                function_name: "build_len",
                args: &[RuntimeValue::String("serde".to_owned())],
            },
            None,
        )
        .await
        .expect("build_len should evaluate");

        assert_eq!(output.value, RuntimeValue::U64(5));
    }

    #[tokio::test]
    #[ignore = "VIR pipeline does not yet support range syntax (0..2) — work item 018"]
    async fn evaluates_non_strict_string_slice_via_local_call() {
        let output = evaluate_source_text_function(
            &RuntimeEvalContext::default(),
            EvalRequest {
                source: SOURCE_NON_STRICT_STRING_HELPERS,
                function_name: "build_slice",
                args: &[RuntimeValue::String("serde".to_owned())],
            },
            None,
        )
        .await
        .expect("build_slice should evaluate");

        assert_eq!(output.value, RuntimeValue::String("se".to_owned()));
    }

    #[tokio::test]
    async fn evaluates_local_discover_host_which() {
        let output = evaluate_source_text_function(
            &RuntimeEvalContext::default(),
            EvalRequest {
                source: SOURCE_DISCOVER_WHICH,
                function_name: "probe",
                args: &[],
            },
            None,
        )
        .await
        .expect("probe should evaluate");

        match output.value {
            RuntimeValue::String(path) => {
                assert!(path.starts_with('/'), "expected absolute path, got {path}");
                assert!(path.ends_with("sh"), "expected shell path, got {path}");
            }
            other => panic!("expected string path, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn evaluates_local_exec_stdout_observer() {
        let output = evaluate_source_text_function(
            &RuntimeEvalContext::default(),
            EvalRequest {
                source: SOURCE_EXEC_STDOUT,
                function_name: "probe",
                args: &[],
            },
            None,
        )
        .await
        .expect("probe should evaluate");

        assert_eq!(output.value, RuntimeValue::String("hello".to_owned()));
    }

    #[tokio::test]
    async fn exec_writable_keywords_need_hosted_vfsd() {
        let result = evaluate_source_text_function(
            &RuntimeEvalContext::default(),
            EvalRequest {
                source: SOURCE_EXEC_WRITABLE,
                function_name: "probe",
                args: &[],
            },
            None,
        )
        .await;

        match result {
            Ok(_) => panic!("writable exec should require a hosted vfsd"),
            Err(RuntimeEvalError::UnsupportedBoundary { detail, .. }) => {
                assert!(
                    detail.contains("hosted vfsd"),
                    "unexpected error detail: {detail}"
                );
            }
            Err(other) => panic!("expected hosted-vfsd boundary error, got {other:?}"),
        }
    }
}
