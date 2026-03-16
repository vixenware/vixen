use super::*;

pub(super) fn capture_referenced_bindings(
    bindings: &HashMap<String, Value>,
    params: &[instr::Param],
    keyword_params: &[instr::KeywordParam],
    body: &instr::Block,
) -> Arc<[(Arc<str>, Value)]> {
    let mut visible_locals = params
        .iter()
        .map(|param| param.name.clone())
        .collect::<HashSet<_>>();
    let mut captured = BTreeSet::new();
    collect_keyword_param_capture_names(
        keyword_params,
        &mut visible_locals,
        bindings,
        &mut captured,
    );
    collect_block_capture_names(body, &mut visible_locals, bindings, &mut captured);
    Arc::from(
        captured
            .into_iter()
            .filter_map(|name| {
                Some((
                    Arc::<str>::from(name.as_str()),
                    bindings.get(&name)?.clone(),
                ))
            })
            .collect::<Vec<_>>(),
    )
}

pub(super) fn collect_keyword_param_capture_names(
    keyword_params: &[instr::KeywordParam],
    visible_locals: &mut HashSet<String>,
    bindings: &HashMap<String, Value>,
    captured: &mut BTreeSet<String>,
) {
    for param in keyword_params {
        if let Some(default) = &param.default {
            collect_expr_capture_names(default, visible_locals, bindings, captured);
        }
        visible_locals.insert(param.name.clone());
    }
}

pub(super) fn collect_block_capture_names(
    block: &instr::Block,
    visible_locals: &mut HashSet<String>,
    bindings: &HashMap<String, Value>,
    captured: &mut BTreeSet<String>,
) {
    let mut local_scope = visible_locals.clone();

    for stmt in &block.stmts {
        match stmt {
            instr::Stmt::Let { name, value } | instr::Stmt::Assign { name, value } => {
                collect_expr_capture_names(value, &local_scope, bindings, captured);
                local_scope.insert(name.clone());
            }
            instr::Stmt::LetPattern { pattern, value } => {
                collect_expr_capture_names(value, &local_scope, bindings, captured);
                collect_pattern_bindings(pattern, &mut local_scope);
            }
            instr::Stmt::Expr(expr) | instr::Stmt::Return(expr) => {
                collect_expr_capture_names(expr, &local_scope, bindings, captured);
            }
            instr::Stmt::FnDecl(def) => {
                local_scope.insert(def.name.clone());
                let mut fn_scope = local_scope.clone();
                for param in &def.params {
                    fn_scope.insert(param.name.clone());
                }
                collect_keyword_param_capture_names(
                    &def.keyword_params,
                    &mut fn_scope,
                    bindings,
                    captured,
                );
                collect_block_capture_names(&def.body, &mut fn_scope, bindings, captured);
            }
            instr::Stmt::StructDecl(decl) => {
                local_scope.insert(decl.name.clone());
            }
        }
    }
}

pub(super) fn collect_expr_capture_names(
    expr: &instr::Expr,
    visible_locals: &HashSet<String>,
    bindings: &HashMap<String, Value>,
    captured: &mut BTreeSet<String>,
) {
    match expr {
        instr::Expr::Var(name) => {
            if !visible_locals.contains(name) && bindings.contains_key(name) {
                captured.insert(name.clone());
            }
        }
        instr::Expr::LitInt(_)
        | instr::Expr::LitFloat(_)
        | instr::Expr::LitBool(_)
        | instr::Expr::LitString(_)
        | instr::Expr::Import { .. }
        | instr::Expr::Unit => {}
        instr::Expr::StringTemplate { parts, .. } | instr::Expr::PathTemplate { parts, .. } => {
            for part in parts {
                if let instr::TemplatePart::Expr(expr) = part {
                    collect_expr_capture_names(expr, visible_locals, bindings, captured);
                }
            }
        }
        instr::Expr::Binary { left, right, .. } => {
            collect_expr_capture_names(left, visible_locals, bindings, captured);
            collect_expr_capture_names(right, visible_locals, bindings, captured);
        }
        instr::Expr::Unary { operand, .. }
        | instr::Expr::Fail {
            message: operand, ..
        }
        | instr::Expr::Fallible(operand) => {
            collect_expr_capture_names(operand, visible_locals, bindings, captured);
        }
        instr::Expr::Block(block) => {
            let mut nested_scope = visible_locals.clone();
            collect_block_capture_names(block, &mut nested_scope, bindings, captured);
        }
        instr::Expr::If {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            collect_expr_capture_names(cond, visible_locals, bindings, captured);
            let mut then_scope = visible_locals.clone();
            collect_block_capture_names(then_branch, &mut then_scope, bindings, captured);
            let mut else_scope = visible_locals.clone();
            collect_block_capture_names(else_branch, &mut else_scope, bindings, captured);
        }
        instr::Expr::Closure { def, .. } => {
            let mut closure_scope = visible_locals.clone();
            for param in &def.params {
                closure_scope.insert(param.name.clone());
            }
            collect_keyword_param_capture_names(
                &def.keyword_params,
                &mut closure_scope,
                bindings,
                captured,
            );
            collect_block_capture_names(&def.body, &mut closure_scope, bindings, captured);
        }
        instr::Expr::Call {
            func,
            args,
            kwargs,
            kwargs_expr,
            ..
        }
        | instr::Expr::PartialCall {
            func,
            args,
            kwargs,
            kwargs_expr,
            ..
        } => {
            collect_expr_capture_names(func, visible_locals, bindings, captured);
            for arg in args {
                collect_expr_capture_names(arg, visible_locals, bindings, captured);
            }
            for (_, arg) in kwargs {
                collect_expr_capture_names(arg, visible_locals, bindings, captured);
            }
            if let Some(kwargs_expr) = kwargs_expr {
                collect_expr_capture_names(kwargs_expr, visible_locals, bindings, captured);
            }
        }
        instr::Expr::StructLit { fields, .. } => {
            for (_, value) in fields {
                collect_expr_capture_names(value, visible_locals, bindings, captured);
            }
        }
        instr::Expr::Record(items) => {
            for item in items {
                match item {
                    instr::RecordItem::Field { value, .. } | instr::RecordItem::Spread(value) => {
                        collect_expr_capture_names(value, visible_locals, bindings, captured);
                    }
                }
            }
        }
        instr::Expr::Array(items) => {
            for item in items {
                match item {
                    instr::ArrayItem::Expr(expr) | instr::ArrayItem::Spread(expr) => {
                        collect_expr_capture_names(expr, visible_locals, bindings, captured);
                    }
                }
            }
        }
        instr::Expr::Map(entries) => {
            for entry in entries {
                match entry {
                    instr::MapItem::Entry { key, value } => {
                        collect_expr_capture_names(key, visible_locals, bindings, captured);
                        collect_expr_capture_names(value, visible_locals, bindings, captured);
                    }
                    instr::MapItem::Spread(expr) => {
                        collect_expr_capture_names(expr, visible_locals, bindings, captured);
                    }
                }
            }
        }
        instr::Expr::Field { receiver, .. } => {
            collect_expr_capture_names(receiver, visible_locals, bindings, captured);
        }
        instr::Expr::Tuple(elems) => {
            for elem in elems {
                collect_expr_capture_names(elem, visible_locals, bindings, captured);
            }
        }
    }
}

pub(super) fn builtin_namespace(name: &str) -> Option<BuiltinValue> {
    match name {
        "acquire" => Some(BuiltinValue::Function(BuiltinFunction::Acquire)),
        "stabilize" => Some(BuiltinValue::Function(BuiltinFunction::Stabilize)),
        "render" => Some(BuiltinValue::Function(BuiltinFunction::Render)),
        "exec" => Some(BuiltinValue::Function(BuiltinFunction::Exec)),
        "discover" => Some(BuiltinValue::Function(BuiltinFunction::Discover)),
        "executor_class" => Some(BuiltinValue::Function(BuiltinFunction::ExecutorClass)),
        "Blob" => Some(BuiltinValue::Namespace(BuiltinNamespace::Blob)),
        "Bytes" => Some(BuiltinValue::Namespace(BuiltinNamespace::Bytes)),
        "Tree" => Some(BuiltinValue::Namespace(BuiltinNamespace::Tree)),
        _ => None,
    }
}

pub(super) fn fn_ptr(def: &Arc<instr::FnDef>) -> usize {
    Arc::as_ptr(def) as usize
}

pub(super) fn stable_fn_repr(def: &instr::FnDef) -> String {
    let mut out = String::new();
    write_fn_def_repr(&mut out, def);
    out
}

pub(super) fn write_fn_def_repr(out: &mut String, def: &instr::FnDef) {
    let _ = write!(out, "fn {}(", def.name);
    for (index, param) in def.params.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&param.name);
    }
    if !def.keyword_params.is_empty() {
        out.push(';');
        for (index, param) in def.keyword_params.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(&param.name);
            if let Some(default) = &param.default {
                out.push('=');
                write_expr_repr(out, default);
            }
        }
    }
    out.push(')');
    write_block_repr(out, &def.body);
}

pub(super) fn write_block_repr(out: &mut String, block: &instr::Block) {
    out.push('{');
    for stmt in &block.stmts {
        write_stmt_repr(out, stmt);
        out.push(';');
    }
    out.push('}');
}

pub(super) fn write_stmt_repr(out: &mut String, stmt: &instr::Stmt) {
    match stmt {
        instr::Stmt::Let { name, value } => {
            let _ = write!(out, "let {name}=");
            write_expr_repr(out, value);
        }
        instr::Stmt::LetPattern { pattern, value } => {
            out.push_str("let_pattern ");
            write_pattern_repr(out, pattern);
            out.push('=');
            write_expr_repr(out, value);
        }
        instr::Stmt::Assign { name, value } => {
            let _ = write!(out, "assign {name}=");
            write_expr_repr(out, value);
        }
        instr::Stmt::Expr(expr) => write_expr_repr(out, expr),
        instr::Stmt::Return(expr) => {
            out.push_str("return ");
            write_expr_repr(out, expr);
        }
        instr::Stmt::FnDecl(def) => {
            out.push_str("fn_decl ");
            write_fn_def_repr(out, def);
        }
        instr::Stmt::StructDecl(decl) => {
            let _ = write!(out, "struct {}(", decl.name);
            for (index, field) in decl.fields.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(field);
            }
            out.push(')');
        }
    }
}

pub(super) fn write_pattern_repr(out: &mut String, pattern: &instr::Pattern) {
    match pattern {
        instr::Pattern::Binding(name) => {
            let _ = write!(out, "bind({name})");
        }
        instr::Pattern::Struct(fields) => {
            out.push_str("struct(");
            for (index, (name, nested)) in fields.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(name);
                if let Some(nested) = nested {
                    out.push(':');
                    write_pattern_repr(out, nested);
                }
            }
            out.push(')');
        }
    }
}

pub(super) fn write_expr_repr(out: &mut String, expr: &instr::Expr) {
    match expr {
        instr::Expr::LitInt(value) => {
            let _ = write!(out, "u64({value})");
        }
        instr::Expr::LitFloat(value) => {
            let _ = write!(out, "f64({value:?})");
        }
        instr::Expr::LitBool(value) => {
            let _ = write!(out, "bool({value})");
        }
        instr::Expr::LitString(value) => {
            let _ = write!(out, "string({value:?})");
        }
        instr::Expr::StringTemplate { parts, .. } => {
            out.push_str("string_template(");
            write_template_parts_repr(out, parts);
            out.push(')');
        }
        instr::Expr::PathTemplate { parts, .. } => {
            out.push_str("path_template(");
            write_template_parts_repr(out, parts);
            out.push(')');
        }
        instr::Expr::Import { path, .. } => {
            let _ = write!(out, "import({path:?})");
        }
        instr::Expr::Unit => out.push_str("unit"),
        instr::Expr::Var(name) => {
            let _ = write!(out, "var({name})");
        }
        instr::Expr::Binary {
            op, left, right, ..
        } => {
            let _ = write!(out, "binary({op:?},");
            write_expr_repr(out, left);
            out.push(',');
            write_expr_repr(out, right);
            out.push(')');
        }
        instr::Expr::Unary { op, operand, .. } => {
            let _ = write!(out, "unary({op:?},");
            write_expr_repr(out, operand);
            out.push(')');
        }
        instr::Expr::Block(block) => write_block_repr(out, block),
        instr::Expr::If {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            out.push_str("if(");
            write_expr_repr(out, cond);
            out.push(')');
            write_block_repr(out, then_branch);
            write_block_repr(out, else_branch);
        }
        instr::Expr::Closure { closure_id, def } => {
            let _ = write!(out, "closure({closure_id},");
            write_fn_def_repr(out, def);
            out.push(')');
        }
        instr::Expr::Call {
            func,
            args,
            kwargs,
            kwargs_expr,
            ..
        } => {
            out.push_str("call(");
            write_expr_repr(out, func);
            write_expr_list_repr(out, args);
            write_kwargs_repr(out, kwargs);
            if let Some(kwargs_expr) = kwargs_expr {
                out.push_str("kwargs_expr=");
                write_expr_repr(out, kwargs_expr);
            }
            out.push(')');
        }
        instr::Expr::PartialCall {
            func,
            args,
            kwargs,
            kwargs_expr,
            ..
        } => {
            out.push_str("partial_call(");
            write_expr_repr(out, func);
            write_expr_list_repr(out, args);
            write_kwargs_repr(out, kwargs);
            if let Some(kwargs_expr) = kwargs_expr {
                out.push_str("kwargs_expr=");
                write_expr_repr(out, kwargs_expr);
            }
            out.push(')');
        }
        instr::Expr::StructLit { name, fields } => {
            let _ = write!(out, "struct_lit({name}:");
            out.push('[');
            for (index, (field, value)) in fields.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(field);
                out.push('=');
                write_expr_repr(out, value);
            }
            out.push_str("])");
        }
        instr::Expr::Record(items) => {
            out.push_str("record(");
            write_record_items_repr(out, items);
            out.push(')');
        }
        instr::Expr::Array(items) => {
            out.push_str("array(");
            write_array_items_repr(out, items);
            out.push(')');
        }
        instr::Expr::Map(entries) => {
            out.push_str("map(");
            out.push('[');
            for (index, entry) in entries.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                match entry {
                    instr::MapItem::Entry { key, value } => {
                        write_expr_repr(out, key);
                        out.push_str("=>");
                        write_expr_repr(out, value);
                    }
                    instr::MapItem::Spread(expr) => {
                        out.push_str("..");
                        write_expr_repr(out, expr);
                    }
                }
            }
            out.push_str("])");
        }
        instr::Expr::Field {
            receiver, field, ..
        } => {
            out.push_str("field(");
            write_expr_repr(out, receiver);
            let _ = write!(out, ",{field})");
        }
        instr::Expr::Fail { message, .. } => {
            out.push_str("fail(");
            write_expr_repr(out, message);
            out.push(')');
        }
        instr::Expr::Fallible(expr) => {
            out.push_str("fallible(");
            write_expr_repr(out, expr);
            out.push(')');
        }
        instr::Expr::Tuple(items) => {
            out.push_str("tuple(");
            write_expr_list_repr(out, items);
            out.push(')');
        }
    }
}

pub(super) fn write_template_parts_repr(out: &mut String, parts: &[instr::TemplatePart]) {
    out.push('[');
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        match part {
            instr::TemplatePart::Text(text) => {
                let _ = write!(out, "text({text:?})");
            }
            instr::TemplatePart::Expr(expr) => {
                out.push_str("expr(");
                write_expr_repr(out, expr);
                out.push(')');
            }
        }
    }
    out.push(']');
}

pub(super) fn write_expr_list_repr(out: &mut String, exprs: &[instr::Expr]) {
    out.push('[');
    for (index, expr) in exprs.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        write_expr_repr(out, expr);
    }
    out.push(']');
}

pub(super) fn write_kwargs_repr(out: &mut String, kwargs: &[(String, instr::Expr)]) {
    out.push('[');
    for (index, (name, expr)) in kwargs.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(name);
        out.push('=');
        write_expr_repr(out, expr);
    }
    out.push(']');
}

pub(super) fn write_record_items_repr(out: &mut String, items: &[instr::RecordItem]) {
    out.push('[');
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        match item {
            instr::RecordItem::Field { name, value } => {
                out.push_str(name);
                out.push('=');
                write_expr_repr(out, value);
            }
            instr::RecordItem::Spread(expr) => {
                out.push_str("..");
                write_expr_repr(out, expr);
            }
        }
    }
    out.push(']');
}

pub(super) fn write_array_items_repr(out: &mut String, items: &[instr::ArrayItem]) {
    out.push('[');
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        match item {
            instr::ArrayItem::Expr(expr) => write_expr_repr(out, expr),
            instr::ArrayItem::Spread(expr) => {
                out.push_str("..");
                write_expr_repr(out, expr);
            }
        }
    }
    out.push(']');
}

pub(super) fn option_some(value: Value) -> Value {
    Value::Struct(StructValue {
        type_name: Arc::from("Some"),
        fields: Arc::from([(Arc::from("value"), value)]),
    })
}

pub(super) fn option_none() -> Value {
    Value::Struct(StructValue {
        type_name: Arc::from("None"),
        fields: Arc::from([]),
    })
}

pub fn verify_host_obligations(obligations: &[HostObservation]) -> Result<(), String> {
    for obligation in obligations {
        match obligation {
            HostObservation::Os { value } => {
                let actual = canonical_host_os();
                if &actual != value {
                    return Err(format!("host.os changed: expected {value}, got {actual}"));
                }
            }
            HostObservation::Arch { value } => {
                let actual = env::consts::ARCH;
                if actual != value {
                    return Err(format!("host.arch changed: expected {value}, got {actual}"));
                }
            }
            HostObservation::Which {
                name,
                expected_path,
            } => {
                let actual = resolve_host_which(name);
                match (expected_path.as_deref(), actual.as_deref()) {
                    (Some(expected), Some(actual)) if expected == actual => {}
                    (Some(expected), Some(actual)) => {
                        return Err(format!(
                            "host.which(\"{name}\") changed: expected {expected}, got {actual}"
                        ));
                    }
                    (Some(expected), None) => {
                        return Err(format!(
                            "host.which(\"{name}\") changed: expected {expected}, but it is now missing"
                        ));
                    }
                    (None, Some(actual)) => {
                        return Err(format!(
                            "host.which(\"{name}\") changed: expected missing, got {actual}"
                        ));
                    }
                    (None, None) => {}
                }
            }
            HostObservation::Read { path, content_hash } => {
                let actual = hash_host_file(path).map_err(|error| {
                    format!("host.read verification failed for {path}: {error}")
                })?;
                if actual != *content_hash {
                    return Err(format!(
                        "host.read(\"{path}\") changed: expected {content_hash}, got {actual}"
                    ));
                }
            }
            HostObservation::Hash { path, content_hash } => {
                let actual = hash_host_file(path).map_err(|error| {
                    format!("host.hash verification failed for {path}: {error}")
                })?;
                if actual != *content_hash {
                    return Err(format!(
                        "host.hash(\"{path}\") changed: expected {content_hash}, got {actual}"
                    ));
                }
            }
        }
    }

    Ok(())
}

pub(super) fn hash_host_file(path: &str) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

pub(super) fn canonical_host_os() -> String {
    match env::consts::OS {
        "macos" => "darwin".to_string(),
        other => other.to_string(),
    }
}

pub(super) fn host_lookup_candidate(name: &str) -> Option<&Path> {
    let candidate = Path::new(name);
    if candidate.is_absolute() || name.contains('/') || name.contains(std::path::MAIN_SEPARATOR) {
        Some(candidate)
    } else {
        None
    }
}

pub(super) fn resolve_host_which(name: &str) -> Option<String> {
    if host_lookup_candidate(name).is_some_and(host_path_is_executable) {
        return Some(name.to_string());
    }

    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let candidate = dir.join(name);
        if host_path_is_executable(&candidate) {
            return Some(candidate.to_string_lossy().to_string());
        }
    }

    None
}

pub(super) fn host_path_is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

pub(super) fn set_executable_if_needed(path: &str, executable: bool) -> std::io::Result<()> {
    if !executable {
        return Ok(());
    }

    #[cfg(unix)]
    {
        let metadata = fs::metadata(path)?;
        let mut permissions = metadata.permissions();
        permissions.set_mode(permissions.mode() | 0o111);
        fs::set_permissions(path, permissions)?;
    }

    Ok(())
}

// r[impl fn.identity]
// r[impl fn.identity.overloads]
// r[impl fn.identity.scc]
// r[impl fn.identity.stability]
// r[impl eval.memoization]
pub(super) fn compute_function_identities(
    functions: &[Arc<instr::FnDef>],
    module_id: &str,
) -> HashMap<usize, Arc<str>> {
    let top_level_names = functions
        .iter()
        .map(|function| function.name.clone())
        .collect::<HashSet<_>>();
    let overloads = functions.iter().enumerate().fold(
        BTreeMap::<String, Vec<usize>>::new(),
        |mut overloads, (index, function)| {
            overloads
                .entry(function.name.clone())
                .or_default()
                .push(index);
            overloads
        },
    );

    let edges = functions
        .iter()
        .map(|function| {
            collect_global_callee_names(function, &top_level_names)
                .into_iter()
                .flat_map(|name| overloads.get(&name).into_iter().flatten().copied())
                .collect::<BTreeSet<_>>()
        })
        .collect::<Vec<_>>();

    let (component_of, components) = strongly_connected_components(&edges);
    let mut component_identities = vec![None; components.len()];

    for component_index in 0..components.len() {
        component_identity(
            component_index,
            &components,
            &component_of,
            &edges,
            functions,
            &mut component_identities,
        );
    }

    functions
        .iter()
        .enumerate()
        .map(|(index, function)| {
            (
                fn_ptr(function),
                Arc::from(format!(
                    "{module_id}::{}",
                    component_identities[component_of[index]]
                        .clone()
                        .expect("component identity should be computed")
                )),
            )
        })
        .collect()
}

pub(super) fn component_identity(
    component_index: usize,
    components: &[Vec<usize>],
    component_of: &[usize],
    edges: &[BTreeSet<usize>],
    functions: &[Arc<instr::FnDef>],
    component_identities: &mut [Option<Arc<str>>],
) -> Arc<str> {
    if let Some(identity) = &component_identities[component_index] {
        return identity.clone();
    }

    let mut members = components[component_index]
        .iter()
        .map(|&index| stable_fn_repr(&functions[index]))
        .collect::<Vec<_>>();
    members.sort();

    let mut callees = components[component_index]
        .iter()
        .flat_map(|&index| edges[index].iter().copied())
        .map(|callee| component_of[callee])
        .filter(|&callee_component| callee_component != component_index)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|callee_component| {
            component_identity(
                callee_component,
                components,
                component_of,
                edges,
                functions,
                component_identities,
            )
            .to_string()
        })
        .collect::<Vec<_>>();
    callees.sort();

    let identity: Arc<str> = Arc::from(format!(
        "component:[{}]->[{}]",
        members.join("|"),
        callees.join("|")
    ));
    component_identities[component_index] = Some(identity.clone());
    identity
}

pub(super) fn strongly_connected_components(
    edges: &[BTreeSet<usize>],
) -> (Vec<usize>, Vec<Vec<usize>>) {
    struct Tarjan<'a> {
        edges: &'a [BTreeSet<usize>],
        index: usize,
        stack: Vec<usize>,
        on_stack: Vec<bool>,
        indices: Vec<Option<usize>>,
        lowlinks: Vec<usize>,
        component_of: Vec<usize>,
        components: Vec<Vec<usize>>,
    }

    impl<'a> Tarjan<'a> {
        fn visit(&mut self, node: usize) {
            let node_index = self.index;
            self.indices[node] = Some(node_index);
            self.lowlinks[node] = node_index;
            self.index += 1;
            self.stack.push(node);
            self.on_stack[node] = true;

            for &next in &self.edges[node] {
                if self.indices[next].is_none() {
                    self.visit(next);
                    self.lowlinks[node] = self.lowlinks[node].min(self.lowlinks[next]);
                } else if self.on_stack[next] {
                    self.lowlinks[node] =
                        self.lowlinks[node].min(self.indices[next].expect("known index"));
                }
            }

            if self.lowlinks[node] == node_index {
                let component_index = self.components.len();
                let mut component = Vec::new();
                loop {
                    let member = self.stack.pop().expect("stack should not be empty");
                    self.on_stack[member] = false;
                    self.component_of[member] = component_index;
                    component.push(member);
                    if member == node {
                        break;
                    }
                }
                self.components.push(component);
            }
        }
    }

    let mut tarjan = Tarjan {
        edges,
        index: 0,
        stack: Vec::new(),
        on_stack: vec![false; edges.len()],
        indices: vec![None; edges.len()],
        lowlinks: vec![0; edges.len()],
        component_of: vec![0; edges.len()],
        components: Vec::new(),
    };

    for node in 0..edges.len() {
        if tarjan.indices[node].is_none() {
            tarjan.visit(node);
        }
    }

    (tarjan.component_of, tarjan.components)
}

pub(super) fn collect_global_callee_names(
    function: &instr::FnDef,
    top_level_names: &HashSet<String>,
) -> BTreeSet<String> {
    let mut visible_locals = function
        .params
        .iter()
        .map(|param| param.name.clone())
        .collect::<HashSet<_>>();
    let mut deps = BTreeSet::new();
    collect_keyword_param_callee_names(
        &function.keyword_params,
        &mut visible_locals,
        top_level_names,
        &mut deps,
    );
    collect_block_callee_names(
        &function.body,
        &mut visible_locals,
        top_level_names,
        &mut deps,
    );
    deps
}

pub(super) fn collect_keyword_param_callee_names(
    keyword_params: &[instr::KeywordParam],
    visible_locals: &mut HashSet<String>,
    top_level_names: &HashSet<String>,
    deps: &mut BTreeSet<String>,
) {
    for param in keyword_params {
        if let Some(default) = &param.default {
            collect_expr_callee_names(default, visible_locals, top_level_names, deps);
        }
        visible_locals.insert(param.name.clone());
    }
}

pub(super) fn collect_block_callee_names(
    block: &instr::Block,
    visible_locals: &mut HashSet<String>,
    top_level_names: &HashSet<String>,
    deps: &mut BTreeSet<String>,
) {
    let mut local_scope = visible_locals.clone();

    for stmt in &block.stmts {
        match stmt {
            instr::Stmt::Let { name, value } | instr::Stmt::Assign { name, value } => {
                collect_expr_callee_names(value, &local_scope, top_level_names, deps);
                local_scope.insert(name.clone());
            }
            instr::Stmt::LetPattern { pattern, value } => {
                collect_expr_callee_names(value, &local_scope, top_level_names, deps);
                collect_pattern_bindings(pattern, &mut local_scope);
            }
            instr::Stmt::Expr(expr) | instr::Stmt::Return(expr) => {
                collect_expr_callee_names(expr, &local_scope, top_level_names, deps);
            }
            instr::Stmt::FnDecl(def) => {
                local_scope.insert(def.name.clone());
                let mut fn_scope = local_scope.clone();
                for param in &def.params {
                    fn_scope.insert(param.name.clone());
                }
                collect_keyword_param_callee_names(
                    &def.keyword_params,
                    &mut fn_scope,
                    top_level_names,
                    deps,
                );
                collect_block_callee_names(&def.body, &mut fn_scope, top_level_names, deps);
            }
            instr::Stmt::StructDecl(_) => {}
        }
    }
}

pub(super) fn collect_expr_callee_names(
    expr: &instr::Expr,
    visible_locals: &HashSet<String>,
    top_level_names: &HashSet<String>,
    deps: &mut BTreeSet<String>,
) {
    match expr {
        instr::Expr::LitInt(_)
        | instr::Expr::LitFloat(_)
        | instr::Expr::LitBool(_)
        | instr::Expr::LitString(_)
        | instr::Expr::Import { .. }
        | instr::Expr::Unit
        | instr::Expr::Var(_) => {}
        instr::Expr::StringTemplate { parts, .. } | instr::Expr::PathTemplate { parts, .. } => {
            for part in parts {
                if let instr::TemplatePart::Expr(expr) = part {
                    collect_expr_callee_names(expr, visible_locals, top_level_names, deps);
                }
            }
        }
        instr::Expr::Binary { left, right, .. } => {
            collect_expr_callee_names(left, visible_locals, top_level_names, deps);
            collect_expr_callee_names(right, visible_locals, top_level_names, deps);
        }
        instr::Expr::Unary { operand, .. }
        | instr::Expr::Fail {
            message: operand, ..
        }
        | instr::Expr::Fallible(operand) => {
            collect_expr_callee_names(operand, visible_locals, top_level_names, deps);
        }
        instr::Expr::Block(block) => {
            let mut nested_scope = visible_locals.clone();
            collect_block_callee_names(block, &mut nested_scope, top_level_names, deps);
        }
        instr::Expr::If {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            collect_expr_callee_names(cond, visible_locals, top_level_names, deps);
            let mut then_scope = visible_locals.clone();
            collect_block_callee_names(then_branch, &mut then_scope, top_level_names, deps);
            let mut else_scope = visible_locals.clone();
            collect_block_callee_names(else_branch, &mut else_scope, top_level_names, deps);
        }
        instr::Expr::Closure { def, .. } => {
            let mut closure_scope = visible_locals.clone();
            for param in &def.params {
                closure_scope.insert(param.name.clone());
            }
            collect_keyword_param_callee_names(
                &def.keyword_params,
                &mut closure_scope,
                top_level_names,
                deps,
            );
            collect_block_callee_names(&def.body, &mut closure_scope, top_level_names, deps);
        }
        instr::Expr::Call {
            func,
            args,
            kwargs,
            kwargs_expr,
            ..
        }
        | instr::Expr::PartialCall {
            func,
            args,
            kwargs,
            kwargs_expr,
            ..
        } => {
            if let instr::Expr::Var(name) = func.as_ref()
                && !visible_locals.contains(name)
                && top_level_names.contains(name)
            {
                deps.insert(name.clone());
            }
            collect_expr_callee_names(func, visible_locals, top_level_names, deps);
            for arg in args {
                collect_expr_callee_names(arg, visible_locals, top_level_names, deps);
            }
            for (_, arg) in kwargs {
                collect_expr_callee_names(arg, visible_locals, top_level_names, deps);
            }
            if let Some(kwargs_expr) = kwargs_expr {
                collect_expr_callee_names(kwargs_expr, visible_locals, top_level_names, deps);
            }
        }
        instr::Expr::StructLit { fields, .. } => {
            for (_, value) in fields {
                collect_expr_callee_names(value, visible_locals, top_level_names, deps);
            }
        }
        instr::Expr::Record(items) => {
            for item in items {
                match item {
                    instr::RecordItem::Field { value, .. } | instr::RecordItem::Spread(value) => {
                        collect_expr_callee_names(value, visible_locals, top_level_names, deps);
                    }
                }
            }
        }
        instr::Expr::Array(items) => {
            for item in items {
                match item {
                    instr::ArrayItem::Expr(expr) | instr::ArrayItem::Spread(expr) => {
                        collect_expr_callee_names(expr, visible_locals, top_level_names, deps);
                    }
                }
            }
        }
        instr::Expr::Map(entries) => {
            for entry in entries {
                match entry {
                    instr::MapItem::Entry { key, value } => {
                        collect_expr_callee_names(key, visible_locals, top_level_names, deps);
                        collect_expr_callee_names(value, visible_locals, top_level_names, deps);
                    }
                    instr::MapItem::Spread(expr) => {
                        collect_expr_callee_names(expr, visible_locals, top_level_names, deps);
                    }
                }
            }
        }
        instr::Expr::Field { receiver, .. } => {
            collect_expr_callee_names(receiver, visible_locals, top_level_names, deps);
        }
        instr::Expr::Tuple(elems) => {
            for elem in elems {
                collect_expr_callee_names(elem, visible_locals, top_level_names, deps);
            }
        }
    }
}

pub(super) fn collect_pattern_bindings(
    pattern: &instr::Pattern,
    visible_locals: &mut HashSet<String>,
) {
    match pattern {
        instr::Pattern::Binding(name) => {
            visible_locals.insert(name.clone());
        }
        instr::Pattern::Struct(fields) => {
            for (name, nested) in fields {
                if let Some(nested) = nested {
                    collect_pattern_bindings(nested, visible_locals);
                } else {
                    visible_locals.insert(name.clone());
                }
            }
        }
    }
}
