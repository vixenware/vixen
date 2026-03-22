use std::fs;
use std::future::Future;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eyre::{Result, bail, eyre};
use sha2::{Digest, Sha256};
use tar::Archive;
use tempfile::Builder;
use tokio::runtime::{Builder as RuntimeBuilder, Handle};
use vx_eval2::{
    CrossingPolicy, EvalError, EvalErrorKind, FunctionRef, Provenance, ReuseClass,
    RuntimeCallOutcome, RuntimeHooks, SourceSpan, Value as EvalValue, ValueRep,
};

pub(crate) enum EvalCliRuntime {
    Local(LocalEvalRuntime),
    CargoSketch(CargoSketchRuntime),
}

pub(crate) struct LocalEvalRuntime {
    root_dir: PathBuf,
}

pub(crate) struct CargoSketchRuntime {
    root_dir: PathBuf,
}

pub(crate) fn build_eval_runtime(
    mode: &str,
    source_dir: PathBuf,
    cwd: PathBuf,
) -> Result<EvalCliRuntime> {
    match mode {
        "local" => Ok(EvalCliRuntime::Local(LocalEvalRuntime {
            root_dir: source_dir,
        })),
        "cargo-sketch" | "cargo_sketch" => Ok(EvalCliRuntime::CargoSketch(CargoSketchRuntime {
            root_dir: cwd,
        })),
        other => bail!("unsupported eval runtime {other:?} (expected `local` or `cargo-sketch`)"),
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
        _place: &str,
        callee: &FunctionRef,
        args: &[EvalValue],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        match callee.name() {
            "read_file" => read_file_runtime_call(&self.root_dir, args, source, "read_file"),
            "Path" => runtime_path(args, source),
            "Tree" => self.runtime_tree(args, source),
            "Tree.file" => self.runtime_tree_file(args, source),
            "Http.get" => self.runtime_http_get(args, source),
            "acquire" => self.runtime_acquire(callee, args, source),
            _ => Ok(RuntimeCallOutcome::Unsupported),
        }
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
            ValueRep::Map(map) => {
                let tree_path = self.materialize_tree_map(&map.entries)?;
                runtime_external("Tree", tree_path.to_string_lossy().into_owned(), source)
            }
            _ => {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Tree() argument must evaluate to String, Path, or Map",
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
        let ValueRep::External(tree) = &args[0].rep else {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Tree.file() receiver must evaluate to Tree",
            ));
        };
        if tree.kind.as_ref() != "Tree" {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Tree.file() receiver must evaluate to Tree",
            ));
        }
        let rel_path = path_like_descriptor(&args[1], "Tree.file() path")?;
        let full_path = self.root_dir.join(tree.descriptor.as_ref()).join(rel_path);
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
        let contents = local_sparse_index_http_response(url.as_ref()).or_else(|| {
            block_on_runtime(vx_http::get_text(url.as_ref()))
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
            return Err(EvalError::new(
                EvalErrorKind::UserFail,
                format!("Http.get() failed for {url}"),
            ));
        };
        Ok(RuntimeCallOutcome::Ready(
            runtime_external("Blob", contents, source)
                .with_reuse(ReuseClass::Observed)
                .with_crossing(CrossingPolicy::ByValue),
        ))
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
        let bytes = block_on_runtime(vx_http::get_bytes(url.as_ref()))
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
        verify_sha256_digest(&bytes, &digest)?;
        match extract.as_deref() {
            Some("tar.gz") => {
                let extracted = self.extract_tar_gz(url.as_ref(), &digest, &bytes)?;
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
        .find(|(capture, _)| capture.as_ref() == name)
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
        .find(|(capture, _)| capture.as_ref() == name)
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
