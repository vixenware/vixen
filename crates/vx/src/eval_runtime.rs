use std::fs;
use std::future::Future;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use eyre::{Result, bail, eyre};
use sha2::{Digest, Sha256};
use tar::Archive;
use tempfile::Builder;
use tokio::runtime::{Builder as RuntimeBuilder, Handle};
use vx_eval2::{
    Claim, CrossingPolicy, EvalError, EvalErrorKind, FunctionRef, Provenance, ReuseClass,
    RuntimeCallOutcome, RuntimeExecReady, RuntimeExecRequest, RuntimeHooks, SourceSpan,
    Value as EvalValue, ValueRep,
};
use vx_platform_exec::{PlatformExecRequest, run_platform_exec};

use crate::eval_progress::EvalProgressReporter;

pub(crate) enum EvalCliRuntime {
    Local(LocalEvalRuntime),
    CargoSketch(CargoSketchRuntime),
}

pub(crate) struct LocalEvalRuntime {
    root_dir: PathBuf,
}

pub(crate) struct CargoSketchRuntime {
    root_dir: PathBuf,
    progress: Option<EvalProgressReporter>,
    http_cache_dir: PathBuf,
}

const INTERNAL_MERGED_TREE_TYPE_NAME: &str = "__Eval2MergedTree";
const INTERNAL_MERGED_TREE_TREES_FIELD: &str = "trees";

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn build_eval_runtime(
    mode: &str,
    source_dir: PathBuf,
    cwd: PathBuf,
) -> Result<EvalCliRuntime> {
    build_eval_runtime_with_progress(mode, source_dir, cwd, None)
}

pub(crate) fn build_eval_runtime_with_progress(
    mode: &str,
    source_dir: PathBuf,
    cwd: PathBuf,
    progress: Option<EvalProgressReporter>,
) -> Result<EvalCliRuntime> {
    match mode {
        "local" => Ok(EvalCliRuntime::Local(LocalEvalRuntime {
            root_dir: source_dir,
        })),
        "cargo-sketch" | "cargo_sketch" | "build-sketch" | "build_sketch" => {
            Ok(EvalCliRuntime::CargoSketch(CargoSketchRuntime {
                root_dir: cwd,
                progress,
                http_cache_dir: eval_cache_root().join("http"),
            }))
        }
        other => bail!(
            "unsupported eval runtime {other:?} (expected `local`, `cargo-sketch`, or `build-sketch`)"
        ),
    }
}

impl RuntimeHooks for EvalCliRuntime {
    fn call(
        &self,
        place: &str,
        callee: &FunctionRef,
        args: &[EvalValue],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        match self {
            Self::Local(runtime) => runtime.call(place, callee, args, source),
            Self::CargoSketch(runtime) => runtime.call(place, callee, args, source),
        }
    }

    fn exec(
        &self,
        place: &str,
        request: &RuntimeExecRequest,
        source: &SourceSpan,
    ) -> Result<RuntimeExecReady, EvalError> {
        match self {
            Self::Local(_) => Err(EvalError::new(
                EvalErrorKind::Internal,
                "exec is not supported by the local eval runtime",
            )),
            Self::CargoSketch(runtime) => runtime.exec(place, request, source),
        }
    }
}

impl RuntimeHooks for LocalEvalRuntime {
    fn call(
        &self,
        _place: &str,
        callee: &FunctionRef,
        args: &[EvalValue],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        match callee.name() {
            "read_file" => read_file_runtime_call(&self.root_dir, args, source, "read_file"),
            _ => Ok(RuntimeCallOutcome::Unsupported),
        }
    }
}

impl RuntimeHooks for CargoSketchRuntime {
    fn call(
        &self,
        place: &str,
        callee: &FunctionRef,
        args: &[EvalValue],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        match callee.name() {
            "read_file" => read_file_runtime_call(&self.root_dir, args, source, "read_file"),
            "Path" => runtime_path(args, source),
            "Tree" => self.runtime_tree(args, source),
            "Tree.file" => self.runtime_tree_file(args, source),
            "Tree.subtree" => self.runtime_tree_subtree(args, source),
            "Host.os" | "host_os" => self.runtime_host_os(place, callee, args, source),
            "Host.arch" | "host_arch" => self.runtime_host_arch(place, callee, args, source),
            "Host.which" | "host_which" => self.runtime_host_which(place, callee, args, source),
            "Stream.collect" => self.runtime_stream_collect(callee, args, source),
            "Bytes.blob" => self.runtime_bytes_blob(callee, args, source),
            "Bytes.utf8" => self.runtime_bytes_utf8(callee, args, source),
            "Bytes.utf8_lossy" => self.runtime_bytes_utf8(callee, args, source),
            "Http.get" => self.runtime_http_get(args, source),
            "acquire" => self.runtime_acquire(callee, args, source),
            _ => Ok(RuntimeCallOutcome::Unsupported),
        }
    }

    fn exec(
        &self,
        _place: &str,
        request: &RuntimeExecRequest,
        source: &SourceSpan,
    ) -> Result<RuntimeExecReady, EvalError> {
        self.runtime_exec(request, source)
    }
}

impl CargoSketchRuntime {
    fn runtime_tree(
        &self,
        args: &[EvalValue],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        if args.len() != 1 {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Tree() requires exactly one path-like or map argument",
            ));
        }
        let value = match &args[0].rep {
            ValueRep::String(path) => runtime_external("Tree", path.clone(), source),
            ValueRep::External(external) if external.kind.as_ref() == "Path" => {
                runtime_external("Tree", external.descriptor.clone(), source)
            }
            ValueRep::Array(items) => {
                let tree_path = self.materialize_tree_array(items)?;
                runtime_external("Tree", tree_path.to_string_lossy().into_owned(), source)
            }
            ValueRep::Map(map) => {
                let tree_path = self.materialize_tree_map(&map.entries)?;
                runtime_external("Tree", tree_path.to_string_lossy().into_owned(), source)
            }
            _ => {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Tree() argument must evaluate to String, Path, Array, or Map",
                ));
            }
        };
        Ok(RuntimeCallOutcome::Ready(value))
    }

    fn runtime_tree_file(
        &self,
        args: &[EvalValue],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        if args.len() != 2 {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Tree.file() requires receiver and one path-like argument",
            ));
        }
        let rel_path = path_like_descriptor(&args[1], "Tree.file() path")?;
        let full_path = self
            .materialize_tree_value(&args[0], "Tree.file() receiver")?
            .join(rel_path);
        let contents = fs::read_to_string(&full_path).map_err(|error| {
            EvalError::new(
                EvalErrorKind::UserFail,
                format!("Tree.file() failed for {}: {error}", full_path.display()),
            )
        })?;
        Ok(RuntimeCallOutcome::Ready(
            runtime_external("Blob", contents, source)
                .with_reuse(ReuseClass::Observed)
                .with_crossing(CrossingPolicy::ByValue),
        ))
    }

    fn runtime_tree_subtree(
        &self,
        args: &[EvalValue],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        if args.len() != 2 {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Tree.subtree() requires receiver and one path-like argument",
            ));
        }
        let rel_path = path_like_descriptor(&args[1], "Tree.subtree() path")?;
        let base = self.materialize_tree_value(&args[0], "Tree.subtree() receiver")?;
        let full_path = base.join(rel_path);
        Ok(RuntimeCallOutcome::Ready(runtime_external(
            "Tree",
            full_path.to_string_lossy().into_owned(),
            source,
        )))
    }

    fn runtime_host_os(
        &self,
        place: &str,
        _callee: &FunctionRef,
        args: &[EvalValue],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        if args.len() != 1 {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Host.os() requires receiver",
            ));
        }
        let _executor = host_executor_arg(&args[0], "Host.os")?;
        let os = canonical_host_os();
        Ok(RuntimeCallOutcome::Ready(
            EvalValue::string(os.clone())
                .with_provenance(Provenance::host_probe(source.clone(), place, "host_os"))
                .with_reuse(ReuseClass::Observed)
                .with_crossing(CrossingPolicy::ByValue)
                .with_claim(Claim::Os { value: os }),
        ))
    }

    fn runtime_host_arch(
        &self,
        place: &str,
        _callee: &FunctionRef,
        args: &[EvalValue],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        if args.len() != 1 {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Host.arch() requires receiver",
            ));
        }
        let _executor = host_executor_arg(&args[0], "Host.arch")?;
        let arch = canonical_host_arch();
        Ok(RuntimeCallOutcome::Ready(
            EvalValue::string(arch.clone())
                .with_provenance(Provenance::host_probe(source.clone(), place, "host_arch"))
                .with_reuse(ReuseClass::Observed)
                .with_crossing(CrossingPolicy::ByValue)
                .with_claim(Claim::Arch { value: arch }),
        ))
    }

    fn runtime_host_which(
        &self,
        place: &str,
        _callee: &FunctionRef,
        args: &[EvalValue],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        if args.len() != 2 {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Host.which() requires receiver and one string argument",
            ));
        }
        let _executor = host_executor_arg(&args[0], "Host.which")?;
        let ValueRep::String(name) = &args[1].rep else {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Host.which() argument must evaluate to String",
            ));
        };
        let resolved = resolve_host_tool(name.as_ref()).map_err(|error| {
            EvalError::new(
                EvalErrorKind::UserFail,
                format!("Host.which({name}) failed: {error}"),
            )
        })?;
        Ok(RuntimeCallOutcome::Ready(
            EvalValue::string(resolved.clone())
                .with_provenance(Provenance::host_probe(source.clone(), place, "host_which"))
                .with_reuse(ReuseClass::Observed)
                .with_crossing(CrossingPolicy::ByValue)
                .with_claim(Claim::Which {
                    name: name.to_string(),
                    expected_path: Some(resolved),
                }),
        ))
    }

    fn runtime_stream_collect(
        &self,
        _callee: &FunctionRef,
        args: &[EvalValue],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        if args.len() != 1 {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Stream.collect() requires receiver",
            ));
        }
        let ValueRep::External(stream) = &args[0].rep else {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Stream.collect() receiver must evaluate to Stream",
            ));
        };
        if !stream.kind.as_ref().starts_with("Stream[") {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Stream.collect() receiver must evaluate to Stream",
            ));
        }
        Ok(RuntimeCallOutcome::Ready(runtime_external(
            "Bytes",
            stream.descriptor.clone(),
            source,
        )))
    }

    fn runtime_bytes_blob(
        &self,
        _callee: &FunctionRef,
        args: &[EvalValue],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        if args.len() != 1 {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Bytes.blob() requires receiver",
            ));
        }
        let ValueRep::External(bytes) = &args[0].rep else {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Bytes.blob() receiver must evaluate to Bytes",
            ));
        };
        if bytes.kind.as_ref() != "Bytes" {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Bytes.blob() receiver must evaluate to Bytes",
            ));
        }
        let contents = fs::read_to_string(bytes.descriptor.as_ref()).map_err(|error| {
            EvalError::new(
                EvalErrorKind::UserFail,
                format!("Bytes.blob() failed for {}: {error}", bytes.descriptor),
            )
        })?;
        Ok(RuntimeCallOutcome::Ready(runtime_external(
            "Blob", contents, source,
        )))
    }

    fn runtime_bytes_utf8(
        &self,
        _callee: &FunctionRef,
        args: &[EvalValue],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        if args.len() != 1 {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Bytes.utf8() requires receiver",
            ));
        }
        let ValueRep::External(bytes) = &args[0].rep else {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Bytes.utf8() receiver must evaluate to Bytes",
            ));
        };
        if bytes.kind.as_ref() != "Bytes" {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Bytes.utf8() receiver must evaluate to Bytes",
            ));
        }
        let contents = fs::read_to_string(bytes.descriptor.as_ref()).map_err(|error| {
            EvalError::new(
                EvalErrorKind::UserFail,
                format!("Bytes.utf8() failed for {}: {error}", bytes.descriptor),
            )
        })?;
        Ok(RuntimeCallOutcome::Ready(
            EvalValue::string(contents).with_provenance(Provenance::unknown(source.clone())),
        ))
    }

    fn runtime_http_get(
        &self,
        args: &[EvalValue],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        if args.len() != 1 {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Http.get() requires exactly one string argument",
            ));
        }
        let ValueRep::String(url) = &args[0].rep else {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Http.get() argument must evaluate to String",
            ));
        };
        let local = local_sparse_index_http_response(url.as_ref());
        let (progress_label, progress_detail) = if local.is_some() {
            ("index", format!("{} (cache)", summarize_url_label(url)))
        } else {
            ("fetch", summarize_url_label(url).to_string())
        };
        let progress = self.progress.as_ref().map(|reporter| {
            reporter.begin_runtime_call(progress_label, Some(progress_detail), source)
        });
        let contents = local.or_else(|| {
            block_on_runtime(vx_http::get_text_cached(&self.http_cache_dir, url.as_ref()))
                .map_err(|error| {
                    EvalError::new(
                        EvalErrorKind::UserFail,
                        format!("Http.get() failed for {url}: {error}"),
                    )
                })
                .and_then(|result| {
                    result.map_err(|error| {
                        EvalError::new(
                            EvalErrorKind::UserFail,
                            format!("Http.get() failed for {url}: {error}"),
                        )
                    })
                })
                .ok()
        });
        let Some(contents) = contents else {
            let error = EvalError::new(
                EvalErrorKind::UserFail,
                format!("Http.get() failed for {url}"),
            );
            if let Some(progress) = progress {
                progress.finish_failed();
            }
            return Err(error);
        };
        let ready = RuntimeCallOutcome::Ready(
            runtime_external("Blob", contents, source)
                .with_reuse(ReuseClass::Observed)
                .with_crossing(CrossingPolicy::ByValue),
        );
        if let Some(progress) = progress {
            progress.finish_ready();
        }
        Ok(ready)
    }

    fn runtime_acquire(
        &self,
        callee: &FunctionRef,
        args: &[EvalValue],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        if args.len() != 1 {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "acquire() requires exactly one string argument",
            ));
        }
        let ValueRep::String(url) = &args[0].rep else {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "acquire() argument must evaluate to String",
            ));
        };
        let digest = capture_string(callee, "digest")?;
        let extract = capture_optional_string(callee, "extract")?;
        let progress = self.progress.as_ref().map(|reporter| {
            reporter.begin_runtime_call(
                "download",
                Some(summarize_url_label(url).to_string()),
                source,
            )
        });
        let transfer_started = Instant::now();
        let bytes = block_on_runtime(vx_http::get_bytes_cached(
            &self.http_cache_dir,
            url.as_ref(),
        ))
        .map_err(|error| {
            EvalError::new(
                EvalErrorKind::UserFail,
                format!("acquire() failed for {url}: {error}"),
            )
        })?
        .map_err(|error| {
            EvalError::new(
                EvalErrorKind::UserFail,
                format!("acquire() failed for {url}: {error}"),
            )
        })?;
        if let Some(progress) = progress.as_ref() {
            progress.report_phase("transfer", transfer_started.elapsed());
        }
        let verify_started = Instant::now();
        verify_sha256_digest(&bytes, &digest)?;
        if let Some(progress) = progress.as_ref() {
            progress.report_phase("verify", verify_started.elapsed());
        }
        let outcome = match extract.as_deref() {
            Some("tar.gz") => {
                let extract_started = Instant::now();
                let extracted = self.extract_tar_gz(url.as_ref(), &digest, &bytes)?;
                if let Some(progress) = progress.as_ref() {
                    progress.report_phase("extract", extract_started.elapsed());
                }
                Ok(RuntimeCallOutcome::Ready(runtime_external(
                    "Tree",
                    extracted.to_string_lossy().into_owned(),
                    source,
                )))
            }
            Some(other) => Err(EvalError::new(
                EvalErrorKind::UserFail,
                format!("acquire() does not support extract format {other:?} in vx eval yet"),
            )),
            None => Err(EvalError::new(
                EvalErrorKind::UserFail,
                "acquire() without extract is not supported in vx eval yet",
            )),
        };
        match outcome {
            Ok(ready) => {
                if let Some(progress) = progress {
                    progress.finish_ready();
                }
                Ok(ready)
            }
            Err(error) => {
                if let Some(progress) = progress {
                    progress.finish_failed();
                }
                Err(error)
            }
        }
    }

    fn materialize_tree_map(
        &self,
        entries: &[(EvalValue, EvalValue)],
    ) -> Result<PathBuf, EvalError> {
        let cache_root = eval_cache_root().join("trees");
        fs::create_dir_all(&cache_root).map_err(|error| {
            EvalError::new(
                EvalErrorKind::UserFail,
                format!(
                    "failed to create eval tree cache at {}: {error}",
                    cache_root.display()
                ),
            )
        })?;
        let root = Builder::new()
            .prefix("tree-")
            .tempdir_in(&cache_root)
            .map_err(|error| {
                EvalError::new(
                    EvalErrorKind::UserFail,
                    format!("failed to allocate eval tree directory: {error}"),
                )
            })?
            .keep();
        for (key, value) in entries {
            let rel = path_like_descriptor(key, "Tree() map key")?;
            let dest = root.join(rel);
            materialize_tree_entry(&self.root_dir, value, &dest)?;
        }
        Ok(root)
    }

    fn materialize_tree_array(&self, items: &[EvalValue]) -> Result<PathBuf, EvalError> {
        let root = alloc_eval_tempdir("trees")?;
        for item in items {
            materialize_tree_entry(&self.root_dir, item, &root)?;
        }
        Ok(root)
    }

    fn materialize_tree_value(
        &self,
        value: &EvalValue,
        context: &str,
    ) -> Result<PathBuf, EvalError> {
        match &value.rep {
            ValueRep::External(external) if external.kind.as_ref() == "Tree" => {
                Ok(runtime_tree_root(&self.root_dir, external))
            }
            ValueRep::Struct(_) if merged_tree_children(value).is_ok() => {
                let root = alloc_eval_tempdir("trees")?;
                materialize_tree_entry(&self.root_dir, value, &root)?;
                Ok(root)
            }
            _ => Err(EvalError::new(
                EvalErrorKind::TypeError,
                format!("{context} must evaluate to Tree"),
            )),
        }
    }

    fn extract_tar_gz(&self, url: &str, digest: &str, bytes: &[u8]) -> Result<PathBuf, EvalError> {
        let cache_root = eval_cache_root().join("acquire");
        fs::create_dir_all(&cache_root).map_err(|error| {
            EvalError::new(
                EvalErrorKind::UserFail,
                format!(
                    "failed to create eval acquire cache at {}: {error}",
                    cache_root.display()
                ),
            )
        })?;
        let dir = Builder::new()
            .prefix("acquire-")
            .tempdir_in(&cache_root)
            .map_err(|error| {
                EvalError::new(
                    EvalErrorKind::UserFail,
                    format!("failed to allocate acquire directory for {url}: {error}"),
                )
            })?
            .keep();
        let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
        let mut archive = Archive::new(decoder);
        archive.unpack(&dir).map_err(|error| {
            EvalError::new(
                EvalErrorKind::UserFail,
                format!("failed to extract acquire() archive for {url} ({digest}): {error}"),
            )
        })?;
        Ok(single_root_dir(&dir))
    }

    fn runtime_exec(
        &self,
        request: &RuntimeExecRequest,
        source: &SourceSpan,
    ) -> Result<RuntimeExecReady, EvalError> {
        let ValueRep::External(executor) = &request.executor.rep else {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "exec().executor must evaluate to ExecutorClass",
            ));
        };
        if executor.kind.as_ref() != "ExecutorClass" {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "exec().executor must evaluate to ExecutorClass",
            ));
        }
        if executor.descriptor.as_ref() != "default" {
            return Err(EvalError::new(
                EvalErrorKind::UserFail,
                format!(
                    "vx eval build runtime only supports executor_class(\"default\") for now, got {:?}",
                    executor.descriptor
                ),
            ));
        }
        if request.args.is_empty() {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "exec() requires at least one program argument",
            ));
        }

        let sandbox_root = alloc_eval_tempdir("exec")?;
        let setup_started = Instant::now();
        for (mount_path, mount_value) in &request.mounts {
            let rel = path_like_descriptor(mount_path, "exec().mounts key")?;
            let dest = sandbox_root.join(rel);
            materialize_tree_entry(&self.root_dir, mount_value, &dest)?;
        }
        for writable in &request.writable {
            fs::create_dir_all(
                sandbox_root.join(path_like_descriptor(writable, "exec().writable entry")?),
            )
            .map_err(io_error)?;
        }
        for dir in &request.create_dirs {
            fs::create_dir_all(
                sandbox_root.join(path_like_descriptor(dir, "exec().create_dirs entry")?),
            )
            .map_err(io_error)?;
        }

        let program = render_exec_value(&request.args[0], &sandbox_root, "exec program")?;
        let progress = self
            .progress
            .as_ref()
            .map(|reporter| reporter.begin_runtime_call("exec", Some(program.clone()), source));
        let mut args = Vec::with_capacity(request.args.len().saturating_sub(1));
        for arg in request.args.iter().skip(1) {
            args.push(render_exec_value(arg, &sandbox_root, "exec arg")?);
        }
        let mut env = Vec::with_capacity(request.env.len());
        for (name, value) in &request.env {
            env.push((
                name.as_ref().to_owned(),
                render_exec_value(value, &sandbox_root, "exec env value")?,
            ));
        }
        if let Some(progress) = progress.as_ref() {
            progress.report_phase("setup", setup_started.elapsed());
        }

        let run_started = Instant::now();
        let output = run_platform_exec(PlatformExecRequest {
            program: &program,
            args: &args,
            env: &env,
            sandbox_root: &sandbox_root,
            sandbox_exemptions: &[],
        })
        .map_err(|error| {
            EvalError::new(
                EvalErrorKind::UserFail,
                format!("failed to execute {program}: {error}"),
            )
        })?;
        if let Some(progress) = progress.as_ref() {
            progress.report_phase("run", run_started.elapsed());
        }

        let capture_started = Instant::now();
        let stdout_path = sandbox_root.join(".vx-stdout");
        fs::write(&stdout_path, &output.stdout).map_err(io_error)?;
        let stderr_path = sandbox_root.join(".vx-stderr");
        fs::write(&stderr_path, &output.stderr).map_err(io_error)?;
        if let Some(progress) = progress.as_ref() {
            progress.report_phase("capture", capture_started.elapsed());
        }

        let process = EvalValue::structure(
            "Process",
            [
                (
                    Arc::from("stdout"),
                    runtime_external(
                        "Stream[Bytes]",
                        stdout_path.to_string_lossy().into_owned(),
                        source,
                    ),
                ),
                (
                    Arc::from("stderr"),
                    runtime_external(
                        "Stream[Bytes]",
                        stderr_path.to_string_lossy().into_owned(),
                        source,
                    ),
                ),
                (
                    Arc::from("exit_code"),
                    EvalValue::u64(output.exit_code as u64)
                        .with_provenance(Provenance::unknown(source.clone())),
                ),
            ],
        )
        .with_provenance(Provenance::unknown(source.clone()));
        let vfs = runtime_external("Tree", sandbox_root.to_string_lossy().into_owned(), source);
        let ready = RuntimeExecReady {
            process,
            vfs,
            events: Vec::new(),
        };
        if let Some(progress) = progress {
            progress.finish_ready();
        }
        Ok(ready)
    }
}

fn read_file_runtime_call(
    root_dir: &Path,
    args: &[EvalValue],
    source: &SourceSpan,
    callee: &str,
) -> Result<RuntimeCallOutcome, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!("{callee}() requires exactly one string argument"),
        ));
    }
    let ValueRep::String(path) = &args[0].rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!("{callee}() argument must evaluate to String"),
        ));
    };
    let full_path = root_dir.join(path.as_ref());
    let contents = fs::read_to_string(&full_path).map_err(|error| {
        EvalError::new(
            EvalErrorKind::UserFail,
            format!("{callee}() failed for {}: {error}", full_path.display()),
        )
    })?;
    Ok(RuntimeCallOutcome::Ready(
        runtime_external("Blob", contents, source)
            .with_reuse(ReuseClass::Observed)
            .with_crossing(CrossingPolicy::ByValue),
    ))
}

fn runtime_path(args: &[EvalValue], source: &SourceSpan) -> Result<RuntimeCallOutcome, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "Path() requires exactly one string argument",
        ));
    }
    let ValueRep::String(path) = &args[0].rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "Path() argument must evaluate to String",
        ));
    };
    Ok(RuntimeCallOutcome::Ready(runtime_external(
        "Path",
        path.clone(),
        source,
    )))
}

fn runtime_external(
    kind: impl Into<Arc<str>>,
    descriptor: impl Into<Arc<str>>,
    source: &SourceSpan,
) -> EvalValue {
    EvalValue::external(kind, descriptor)
        .with_provenance(Provenance::unknown(source.clone()))
        .with_reuse(ReuseClass::Observed)
        .with_crossing(CrossingPolicy::ByValue)
}

fn capture_string(callee: &FunctionRef, name: &str) -> Result<Arc<str>, EvalError> {
    let Some((_, value)) = callee
        .captures()
        .iter()
        .find(|(capture, _)| capture.as_str() == name)
    else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!("{}() requires keyword argument `{name}`", callee.name()),
        ));
    };
    let ValueRep::String(text) = &value.rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!("{}().{name} must evaluate to String", callee.name()),
        ));
    };
    Ok(text.clone())
}

fn capture_optional_string(
    callee: &FunctionRef,
    name: &str,
) -> Result<Option<Arc<str>>, EvalError> {
    let Some((_, value)) = callee
        .captures()
        .iter()
        .find(|(capture, _)| capture.as_str() == name)
    else {
        return Ok(None);
    };
    match &value.rep {
        ValueRep::String(text) => Ok(Some(text.clone())),
        ValueRep::Unit => Ok(None),
        ValueRep::Struct(struct_value) if struct_value.type_name.as_ref() == "None" => Ok(None),
        ValueRep::Struct(struct_value) if struct_value.type_name.as_ref() == "Some" => {
            let Some((_, inner)) = struct_value
                .fields
                .iter()
                .find(|(field, _)| field.as_ref() == "value")
            else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    format!("{}().{name} Some value is missing payload", callee.name()),
                ));
            };
            let ValueRep::String(text) = &inner.rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    format!(
                        "{}().{name} Some payload must evaluate to String",
                        callee.name()
                    ),
                ));
            };
            Ok(Some(text.clone()))
        }
        _ => Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!("{}().{name} must evaluate to String?", callee.name()),
        )),
    }
}

fn path_like_descriptor(value: &EvalValue, context: &str) -> Result<PathBuf, EvalError> {
    match &value.rep {
        ValueRep::String(path) => Ok(PathBuf::from(path.as_ref())),
        ValueRep::External(external) if external.kind.as_ref() == "Path" => {
            Ok(PathBuf::from(external.descriptor.as_ref()))
        }
        _ => Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!("{context} must evaluate to String or Path"),
        )),
    }
}

fn merged_tree_children(value: &EvalValue) -> Result<&[EvalValue], EvalError> {
    let ValueRep::Struct(struct_value) = &value.rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "value must evaluate to merged Tree",
        ));
    };
    if struct_value.type_name.as_ref() != INTERNAL_MERGED_TREE_TYPE_NAME {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "value must evaluate to merged Tree",
        ));
    }
    let Some((_, children)) = struct_value
        .fields
        .iter()
        .find(|(name, _)| name.as_ref() == INTERNAL_MERGED_TREE_TREES_FIELD)
    else {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            "merged Tree value is missing trees",
        ));
    };
    let ValueRep::Array(items) = &children.rep else {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            "merged Tree children must evaluate to Array",
        ));
    };
    Ok(items)
}

fn materialize_tree_entry(
    root_dir: &Path,
    value: &EvalValue,
    dest: &Path,
) -> Result<(), EvalError> {
    match &value.rep {
        ValueRep::External(external) if external.kind.as_ref() == "Tree" => {
            let source = Path::new(external.descriptor.as_ref());
            let source = if source.is_absolute() {
                source.to_path_buf()
            } else {
                root_dir.join(source)
            };
            copy_tree_contents(&source, dest)
        }
        ValueRep::Struct(_) if merged_tree_children(value).is_ok() => {
            fs::create_dir_all(dest).map_err(io_error)?;
            for child in merged_tree_children(value)? {
                merge_tree_entry(root_dir, child, dest)?;
            }
            Ok(())
        }
        ValueRep::External(external) if external.kind.as_ref() == "Blob" => {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(io_error)?;
            }
            fs::write(dest, external.descriptor.as_ref()).map_err(io_error)
        }
        ValueRep::String(text) => {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(io_error)?;
            }
            fs::write(dest, text.as_ref()).map_err(io_error)
        }
        _ => Err(EvalError::new(
            EvalErrorKind::TypeError,
            "Tree() map values must evaluate to Tree, Blob, or String",
        )),
    }
}

fn merge_tree_entry(root_dir: &Path, value: &EvalValue, dest: &Path) -> Result<(), EvalError> {
    match &value.rep {
        ValueRep::External(external) if external.kind.as_ref() == "Tree" => {
            merge_tree_contents(&runtime_tree_root(root_dir, external), dest)
        }
        ValueRep::Struct(_) if merged_tree_children(value).is_ok() => {
            for child in merged_tree_children(value)? {
                merge_tree_entry(root_dir, child, dest)?;
            }
            Ok(())
        }
        _ => Err(EvalError::new(
            EvalErrorKind::TypeError,
            "Tree merge inputs must evaluate to Tree",
        )),
    }
}

fn copy_tree_contents(source: &Path, dest: &Path) -> Result<(), EvalError> {
    if source.is_file() {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(io_error)?;
        }
        fs::copy(source, dest).map_err(io_error)?;
        return Ok(());
    }
    fs::create_dir_all(dest).map_err(io_error)?;
    let entries = fs::read_dir(source).map_err(io_error)?;
    for entry in entries {
        let entry = entry.map_err(io_error)?;
        let file_type = entry.file_type().map_err(io_error)?;
        let child_dest = dest.join(entry.file_name());
        if file_type.is_dir() {
            copy_tree_contents(&entry.path(), &child_dest)?;
        } else if file_type.is_file() {
            if let Some(parent) = child_dest.parent() {
                fs::create_dir_all(parent).map_err(io_error)?;
            }
            fs::copy(entry.path(), child_dest).map_err(io_error)?;
        } else {
            return Err(EvalError::new(
                EvalErrorKind::UserFail,
                format!(
                    "unsupported filesystem entry in Tree copy: {}",
                    entry.path().display()
                ),
            ));
        }
    }
    Ok(())
}

fn merge_tree_contents(source: &Path, dest: &Path) -> Result<(), EvalError> {
    if source.is_file() {
        if dest.exists() {
            return Err(EvalError::new(
                EvalErrorKind::UserFail,
                format!("tree merge conflict at {}", dest.display()),
            ));
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(io_error)?;
        }
        fs::copy(source, dest).map_err(io_error)?;
        return Ok(());
    }
    fs::create_dir_all(dest).map_err(io_error)?;
    let entries = fs::read_dir(source).map_err(io_error)?;
    for entry in entries {
        let entry = entry.map_err(io_error)?;
        let file_type = entry.file_type().map_err(io_error)?;
        let child_dest = dest.join(entry.file_name());
        if file_type.is_dir() {
            merge_tree_contents(&entry.path(), &child_dest)?;
        } else if file_type.is_file() {
            if child_dest.exists() {
                return Err(EvalError::new(
                    EvalErrorKind::UserFail,
                    format!("tree merge conflict at {}", child_dest.display()),
                ));
            }
            if let Some(parent) = child_dest.parent() {
                fs::create_dir_all(parent).map_err(io_error)?;
            }
            fs::copy(entry.path(), child_dest).map_err(io_error)?;
        } else {
            return Err(EvalError::new(
                EvalErrorKind::UserFail,
                format!(
                    "unsupported filesystem entry in Tree merge: {}",
                    entry.path().display()
                ),
            ));
        }
    }
    Ok(())
}

fn runtime_tree_root(root_dir: &Path, tree: &vx_eval2::ExternalValue) -> PathBuf {
    let source = Path::new(tree.descriptor.as_ref());
    if source.is_absolute() {
        source.to_path_buf()
    } else {
        root_dir.join(source)
    }
}

fn host_executor_arg(value: &EvalValue, context: &str) -> Result<Arc<str>, EvalError> {
    let ValueRep::External(external) = &value.rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!("{context}() receiver must evaluate to Host"),
        ));
    };
    if external.kind.as_ref() != "Host" {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!("{context}() receiver must evaluate to Host"),
        ));
    }
    if external.descriptor.as_ref() != "default" {
        return Err(EvalError::new(
            EvalErrorKind::UserFail,
            format!("{context}() only supports executor_class(\"default\") in vx eval for now"),
        ));
    }
    Ok(external.descriptor.clone())
}

fn canonical_host_os() -> String {
    match std::env::consts::OS {
        "macos" => "darwin".to_string(),
        other => other.to_string(),
    }
}

fn canonical_host_arch() -> String {
    std::env::consts::ARCH.to_string()
}

fn resolve_host_tool(name: &str) -> Result<String> {
    if name == "rustc"
        && let Some(path) = rustup_which("rustc")?
    {
        return Ok(path);
    }
    path_search_tool(name)?
        .ok_or_else(|| eyre!("could not find {name} on PATH"))
        .map(|path| path.to_string_lossy().into_owned())
}

fn rustup_which(name: &str) -> Result<Option<String>> {
    let Ok(output) = Command::new("rustup").args(["which", name]).output() else {
        return Ok(None);
    };
    if !output.status.success() {
        return Ok(None);
    }
    let path = String::from_utf8(output.stdout)
        .map_err(|error| eyre!("rustup which {name} produced non-utf8 output: {error}"))?;
    let trimmed = path.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

fn path_search_tool(name: &str) -> Result<Option<PathBuf>> {
    let direct = PathBuf::from(name);
    if direct.components().count() > 1 || direct.is_absolute() {
        return Ok(direct
            .exists()
            .then_some(fs::canonicalize(&direct).unwrap_or(direct)));
    }
    let Some(path_var) = std::env::var_os("PATH") else {
        return Ok(None);
    };
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(Some(fs::canonicalize(&candidate).unwrap_or(candidate)));
        }
    }
    Ok(None)
}

fn alloc_eval_tempdir(prefix: &str) -> Result<PathBuf, EvalError> {
    let cache_root = eval_cache_root().join(prefix);
    fs::create_dir_all(&cache_root).map_err(|error| {
        EvalError::new(
            EvalErrorKind::UserFail,
            format!(
                "failed to create eval {prefix} cache at {}: {error}",
                cache_root.display()
            ),
        )
    })?;
    Builder::new()
        .prefix(&format!("{prefix}-"))
        .tempdir_in(&cache_root)
        .map(|dir| dir.keep())
        .map_err(|error| {
            EvalError::new(
                EvalErrorKind::UserFail,
                format!("failed to allocate eval {prefix} directory: {error}"),
            )
        })
}

fn render_exec_value(
    value: &EvalValue,
    sandbox_root: &Path,
    context: &str,
) -> Result<String, EvalError> {
    match &value.rep {
        ValueRep::String(text) => Ok(text.to_string()),
        ValueRep::External(external) if external.kind.as_ref() == "Path" => Ok(sandbox_root
            .join(external.descriptor.as_ref())
            .to_string_lossy()
            .into_owned()),
        ValueRep::External(external) if external.kind.as_ref() == "ExecPath" => Ok(sandbox_root
            .join(external.descriptor.as_ref())
            .to_string_lossy()
            .into_owned()),
        _ => Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!("{context} must evaluate to String or Path"),
        )),
    }
}

fn io_error(error: std::io::Error) -> EvalError {
    EvalError::new(EvalErrorKind::UserFail, error.to_string())
}

fn verify_sha256_digest(bytes: &[u8], digest: &str) -> Result<(), EvalError> {
    let Some(expected_hex) = digest.strip_prefix("sha256:") else {
        return Err(EvalError::new(
            EvalErrorKind::UserFail,
            format!("unsupported acquire digest {digest:?} (expected sha256:...)"),
        ));
    };
    let actual = hex::encode(Sha256::digest(bytes));
    if actual == expected_hex {
        Ok(())
    } else {
        Err(EvalError::new(
            EvalErrorKind::UserFail,
            format!("acquire digest mismatch: expected {digest}, got sha256:{actual}"),
        ))
    }
}

fn single_root_dir(root: &Path) -> PathBuf {
    let Ok(entries) = fs::read_dir(root) else {
        return root.to_path_buf();
    };
    let mut visible = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name() != ".DS_Store")
        .collect::<Vec<_>>();
    if visible.len() != 1 {
        return root.to_path_buf();
    }
    let only = visible.pop().expect("single visible entry");
    if only
        .file_type()
        .map(|file_type| file_type.is_dir())
        .unwrap_or(false)
    {
        only.path()
    } else {
        root.to_path_buf()
    }
}

fn block_on_runtime<F>(future: F) -> Result<F::Output>
where
    F: Future,
{
    match Handle::try_current() {
        Ok(handle) => Ok(tokio::task::block_in_place(|| handle.block_on(future))),
        Err(_) => {
            let runtime = RuntimeBuilder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| eyre!("failed to build temporary tokio runtime: {error}"))?;
            Ok(runtime.block_on(future))
        }
    }
}

fn eval_cache_root() -> PathBuf {
    PathBuf::from(vx_io::vx_home().join("eval-cache").as_str())
}

fn local_crates_io_sparse_index_root() -> Option<PathBuf> {
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))?;
    let index_root = cargo_home.join("registry/index");
    fs::read_dir(index_root)
        .ok()?
        .filter_map(Result::ok)
        .find_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            name.starts_with("index.crates.io-").then_some(path)
        })
}

fn sparse_cache_entry_to_jsonl(bytes: &[u8]) -> Option<String> {
    let mut entries = Vec::new();
    let mut start = 0usize;
    let mut index = 0usize;
    while index <= bytes.len() {
        if index == bytes.len() || bytes[index] == 0 {
            if start < index && bytes[start] == b'{' {
                entries.push(String::from_utf8(bytes[start..index].to_vec()).ok()?);
            }
            start = index.saturating_add(1);
        }
        index += 1;
    }
    (!entries.is_empty()).then(|| entries.join("\n"))
}

fn local_sparse_index_http_response(url: &str) -> Option<String> {
    let root = local_crates_io_sparse_index_root()?;
    let rel = url.strip_prefix("https://index.crates.io/")?;
    if rel == "config.json" {
        return fs::read_to_string(root.join("config.json")).ok();
    }
    let bytes = fs::read(root.join(".cache").join(rel)).ok()?;
    sparse_cache_entry_to_jsonl(&bytes)
}

fn summarize_url_label(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}
