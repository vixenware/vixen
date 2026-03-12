use std::fs;

use camino::{Utf8Path, Utf8PathBuf};
use vx_store_proto::chunking::{prepare_blob, store_prepared_blob};
use vx_store_proto::{Blake3Hash, BlobRef, CasNode, NodeHash, NodeId, NodeManifest, OutputEntry};

use super::{
    BlobValue, EvalRenderRequest, OutputBlobRef, PoisonError, PoisonKind, StoreClient, Value,
    exec_bridge_poison, hydrate_exec_value_from_store, output_blob_ref_from_blob_ref,
};

#[derive(Debug, Clone)]
enum TemplateNode {
    Text(String),
    Expr(Vec<String>),
    If {
        path: Vec<String>,
        body: Vec<TemplateNode>,
    },
    Each {
        path: Vec<String>,
        body: Vec<TemplateNode>,
    },
}

#[derive(Clone, Copy)]
struct RenderContext<'a> {
    root: &'a Value,
    current: Option<&'a Value>,
}

impl<'a> RenderContext<'a> {
    fn nested(self, current: &'a Value) -> Self {
        Self {
            root: self.root,
            current: Some(current),
        }
    }
}

pub(crate) async fn run_build_vx_render_via_store(
    store: &StoreClient,
    workspace_root: &Utf8Path,
    request: EvalRenderRequest,
) -> std::result::Result<Value, PoisonError> {
    let demand_chain = request.demand_chain.clone();
    let bindings_key = request.bindings.key().ok_or_else(|| {
        exec_bridge_poison(
            PoisonKind::TypeError,
            "render bindings must be fully resolved and keyable".to_string(),
            &demand_chain,
        )
    })?;
    let template_path = resolve_template_path(workspace_root, &request.template);
    let template_text = fs::read_to_string(&template_path).map_err(|error| {
        exec_bridge_poison(
            PoisonKind::UserFail,
            format!("failed to read render template {}: {error}", template_path),
            &demand_chain,
        )
    })?;
    let cache_key = render_cache_key(&template_text, &bindings_key);

    let rendered = if let Some(cached) = store.lookup(cache_key).await.map_err(|error| {
        exec_bridge_poison(PoisonKind::Internal, format!("{error:?}"), &demand_chain)
    })? {
        Value::Blob(
            load_rendered_blob_from_manifest(store, cached)
                .await
                .map_err(|message| {
                    exec_bridge_poison(PoisonKind::Internal, message, &demand_chain)
                })?,
        )
    } else {
        let template = parse_template(&template_text).map_err(|message| {
            exec_bridge_poison(
                PoisonKind::UserFail,
                format!(
                    "failed to parse render template {}: {message}",
                    template_path
                ),
                &demand_chain,
            )
        })?;
        let rendered = render_template(
            &template,
            RenderContext {
                root: &request.bindings,
                current: None,
            },
        )
        .map_err(|message| {
            exec_bridge_poison(
                PoisonKind::TypeError,
                format!("failed to render template {}: {message}", template_path),
                &demand_chain,
            )
        })?;
        let prepared = prepare_blob(rendered.as_bytes());
        let blob_ref = store_prepared_blob(store, &prepared, rendered.as_bytes())
            .await
            .map_err(|message| exec_bridge_poison(PoisonKind::Internal, message, &demand_chain))?;
        let manifest = NodeManifest {
            node_id: NodeId("render:blob".to_string()),
            cache_key,
            produced_at: String::new(),
            outputs: vec![OutputEntry {
                logical: "blob".to_string(),
                filename: template_path
                    .file_name()
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| "rendered".to_string()),
                blob: output_blob_ref_from_blob_ref(&blob_ref),
                executable: false,
            }],
        };
        let (node_bytes, manifest_hash) = CasNode::Manifest(manifest).to_bytes_and_hash();
        store
            .put_node(manifest_hash, node_bytes)
            .await
            .map_err(|error| {
                exec_bridge_poison(PoisonKind::Internal, format!("{error:?}"), &demand_chain)
            })?;
        publish_cache_entry(store, cache_key, manifest_hash)
            .await
            .map_err(|message| exec_bridge_poison(PoisonKind::Internal, message, &demand_chain))?;
        Value::Blob(BlobValue::Store {
            blob_ref,
            executable: false,
        })
    };

    hydrate_exec_value_from_store(store, rendered, &demand_chain).await
}

fn resolve_template_path(workspace_root: &Utf8Path, template: &str) -> Utf8PathBuf {
    let template = Utf8Path::new(template);
    if template.is_absolute() {
        template.to_path_buf()
    } else {
        workspace_root.join(template)
    }
}

fn render_cache_key(template_text: &str, bindings_key: &str) -> Blake3Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"render-cache-v1\n");
    hasher.update(b"template:");
    hasher.update(template_text.as_bytes());
    hasher.update(b"\n");
    hasher.update(b"bindings:");
    hasher.update(bindings_key.as_bytes());
    hasher.update(b"\n");
    Blake3Hash(*hasher.finalize().as_bytes())
}

fn parse_template(input: &str) -> Result<Vec<TemplateNode>, String> {
    let mut index = 0;
    parse_nodes(input, &mut index, None)
}

fn parse_nodes(
    input: &str,
    index: &mut usize,
    end_tag: Option<&str>,
) -> Result<Vec<TemplateNode>, String> {
    let mut nodes = Vec::new();

    while *index < input.len() {
        let Some(open_rel) = input[*index..].find("{{") else {
            if *index < input.len() {
                nodes.push(TemplateNode::Text(input[*index..].to_string()));
                *index = input.len();
            }
            return match end_tag {
                Some(tag) => Err(format!("unclosed template block '{tag}'")),
                None => Ok(nodes),
            };
        };

        let open = *index + open_rel;
        if open > *index {
            nodes.push(TemplateNode::Text(input[*index..open].to_string()));
        }

        let close = input[open + 2..]
            .find("}}")
            .ok_or_else(|| "unterminated template tag".to_string())?
            + open
            + 2;
        let tag = input[open + 2..close].trim();
        *index = close + 2;

        if let Some(stripped) = tag.strip_prefix('/') {
            let closing = stripped.trim();
            return match end_tag {
                Some(expected) if closing == expected => Ok(nodes),
                Some(expected) => Err(format!(
                    "unexpected closing tag '{closing}', expected '{expected}'"
                )),
                None => Err(format!("unexpected closing tag '{closing}'")),
            };
        }

        if let Some(stripped) = tag.strip_prefix("#if") {
            let path = parse_path(stripped.trim())?;
            let body = parse_nodes(input, index, Some("if"))?;
            nodes.push(TemplateNode::If { path, body });
            continue;
        }

        if let Some(stripped) = tag.strip_prefix("#each") {
            let path = parse_path(stripped.trim())?;
            let body = parse_nodes(input, index, Some("each"))?;
            nodes.push(TemplateNode::Each { path, body });
            continue;
        }

        nodes.push(TemplateNode::Expr(parse_path(tag)?));
    }

    match end_tag {
        Some(tag) => Err(format!("unclosed template block '{tag}'")),
        None => Ok(nodes),
    }
}

fn parse_path(raw: &str) -> Result<Vec<String>, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("empty template path".to_string());
    }
    raw.split('.')
        .map(|segment| {
            let segment = segment.trim();
            if segment.is_empty() {
                Err(format!("invalid template path '{raw}'"))
            } else {
                Ok(segment.to_string())
            }
        })
        .collect()
}

fn render_template(nodes: &[TemplateNode], context: RenderContext<'_>) -> Result<String, String> {
    let mut out = String::new();
    render_into(nodes, context, &mut out)?;
    Ok(out)
}

fn render_into(
    nodes: &[TemplateNode],
    context: RenderContext<'_>,
    out: &mut String,
) -> Result<(), String> {
    for node in nodes {
        match node {
            TemplateNode::Text(text) => out.push_str(text),
            TemplateNode::Expr(path) => {
                let value = lookup_path(context, path)?;
                out.push_str(&render_scalar(value, path.join("."))?);
            }
            TemplateNode::If { path, body } => {
                let value = lookup_path(context, path)?;
                match undiscovered(value) {
                    Value::Bool(true) => render_into(body, context, out)?,
                    Value::Bool(false) => {}
                    other => {
                        return Err(format!(
                            "template condition '{}' must be Bool, got {}",
                            path.join("."),
                            other.type_name()
                        ));
                    }
                }
            }
            TemplateNode::Each { path, body } => {
                let value = lookup_path(context, path)?;
                let Value::Array(items) = undiscovered(value) else {
                    return Err(format!(
                        "template loop '{}' must be Array, got {}",
                        path.join("."),
                        undiscovered(value).type_name()
                    ));
                };
                for item in items.iter() {
                    render_into(body, context.nested(item), out)?;
                }
            }
        }
    }
    Ok(())
}

fn lookup_path<'a>(context: RenderContext<'a>, path: &[String]) -> Result<&'a Value, String> {
    if path.first().is_some_and(|segment| segment == "this") {
        let value = context.current.unwrap_or(context.root);
        return lookup_from(value, &path[1..]);
    }

    if let Some(current) = context.current
        && let Ok(value) = lookup_from(current, path)
    {
        return Ok(value);
    }

    lookup_from(context.root, path)
}

fn lookup_from<'a>(value: &'a Value, path: &[String]) -> Result<&'a Value, String> {
    let mut value = undiscovered(value);
    for segment in path {
        value = lookup_field(value, segment)?;
    }
    Ok(value)
}

fn lookup_field<'a>(value: &'a Value, segment: &str) -> Result<&'a Value, String> {
    match undiscovered(value) {
        Value::Struct(struct_value) => struct_value
            .fields
            .iter()
            .find_map(|(name, value)| (name.as_ref() == segment).then_some(value))
            .ok_or_else(|| format!("missing template field '{segment}'")),
        Value::Map(map) => map
            .entries
            .iter()
            .find_map(|(key, value)| match undiscovered(key) {
                Value::String(name) if name.as_ref() == segment => Some(value),
                _ => None,
            })
            .ok_or_else(|| format!("missing template field '{segment}'")),
        other => Err(format!(
            "template field '{segment}' requires Struct or Map, got {}",
            other.type_name()
        )),
    }
}

fn render_scalar(value: &Value, path: String) -> Result<String, String> {
    match undiscovered(value) {
        Value::String(text) => Ok(text.to_string()),
        Value::Path(path_value) => Ok(path_value.as_str().to_string()),
        Value::Bool(_) | Value::U64(_) | Value::I64(_) | Value::F64(_) | Value::Unit => {
            Ok(undiscovered(value).to_string())
        }
        other => Err(format!(
            "template expression '{path}' must be scalar, got {}",
            other.type_name()
        )),
    }
}

fn undiscovered(mut value: &Value) -> &Value {
    while let Value::Discovered(discovered) = value {
        value = discovered.value.as_ref();
    }
    value
}

async fn load_rendered_blob_from_manifest(
    store: &StoreClient,
    manifest_hash: NodeHash,
) -> Result<BlobValue, String> {
    let node_bytes = store
        .get_node(manifest_hash)
        .await
        .map_err(|error| format!("failed to load render manifest: {error:?}"))?
        .ok_or_else(|| format!("render manifest {manifest_hash} not found"))?;
    let node = CasNode::from_bytes(&node_bytes)
        .map_err(|error| format!("failed to decode render manifest {manifest_hash}: {error}"))?;
    let CasNode::Manifest(manifest) = node else {
        return Err(format!(
            "render cache entry {manifest_hash} is not a blob manifest"
        ));
    };
    let [output] = manifest.outputs.as_slice() else {
        return Err(format!(
            "render manifest {manifest_hash} must contain exactly one output"
        ));
    };
    Ok(BlobValue::Store {
        blob_ref: blob_ref_from_output_blob_ref(&output.blob),
        executable: output.executable,
    })
}

fn blob_ref_from_output_blob_ref(blob_ref: &OutputBlobRef) -> BlobRef {
    match blob_ref {
        OutputBlobRef::Small { chunk, size } => BlobRef::Small {
            chunk: *chunk,
            size: *size,
        },
        OutputBlobRef::Large {
            blob_node,
            content_hash,
            size,
        } => BlobRef::Large {
            blob_node: *blob_node,
            content_hash: *content_hash,
            size: *size,
        },
    }
}

async fn publish_cache_entry(
    store: &StoreClient,
    cache_key: Blake3Hash,
    node_hash: NodeHash,
) -> Result<(), String> {
    let publish = store
        .publish(cache_key, node_hash)
        .await
        .map_err(|error| format!("failed to publish render cache entry: {error:?}"))?;
    if publish.success {
        Ok(())
    } else {
        Err(publish
            .error
            .unwrap_or_else(|| "failed to publish render cache entry".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use vx_eval::{MapValue, Value};

    use super::{RenderContext, parse_template, render_template};

    #[test]
    fn template_parser_renders_conditionals_and_loops() {
        let template = parse_template(
            "name={{NAME}}\n{{#if ENABLED}}enabled\n{{/if}}{{#each FLAGS}}- {{this}}\n{{/each}}",
        )
        .expect("template should parse");
        let bindings = Value::Map(MapValue {
            entries: Arc::from([
                (
                    Value::String(Arc::from("NAME")),
                    Value::String(Arc::from("libpng")),
                ),
                (Value::String(Arc::from("ENABLED")), Value::Bool(true)),
                (
                    Value::String(Arc::from("FLAGS")),
                    Value::Array(Arc::from([
                        Value::String(Arc::from("PNG_SHARED")),
                        Value::String(Arc::from("PNG_STATIC")),
                    ])),
                ),
            ]),
        });

        let rendered = render_template(
            &template,
            RenderContext {
                root: &bindings,
                current: None,
            },
        )
        .expect("template should render");

        assert_eq!(
            rendered,
            "name=libpng\nenabled\n- PNG_SHARED\n- PNG_STATIC\n"
        );
    }
}
