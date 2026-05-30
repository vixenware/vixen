//! Load `command-model` grammars authored in Vixen (`.vx`) using only the Vixen
//! **parser** — no evaluator. Grammar files are declarative literals (`let`
//! bindings of `Grammar { … }` built from struct + enum literals), so a total,
//! type-directed lowering over the parsed AST is enough; the demand-driven
//! engine is never needed for the declarative core. Compatibility relations stay
//! in the introspectable tier (data / small expressions), so they lower here
//! too; only a genuinely Turing-complete reducer would ever need the evaluator.

use command_model::{
    Arity, EnvDecl, Flag, Grammar, GrammarSet, Kind, Mode, Operation, Policy, Positional, Spelling,
    ValueDomain,
};
use std::fmt;
use vx_parser::{
    ArrayItem, AstNode, Expr, SourceFile, Stmt, StructLiteralExpr, SyntaxKind, SyntaxNode,
    parse_source_file, source::SourceId,
};

/// A grammar file failed to load: either it didn't parse, or its declarative
/// shape didn't match the grammar schema (unknown variant, missing field, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError {
    pub message: String,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for LoadError {}

type Result<T> = std::result::Result<T, LoadError>;

fn bail<T>(msg: impl Into<String>) -> Result<T> {
    Err(LoadError {
        message: msg.into(),
    })
}

/// Parse a `.vx` grammar source into a syntax tree. Thin wrapper used by tests.
pub fn parse(src: &str) -> vx_parser::Parsed<SourceFile> {
    parse_source_file(src, SourceId(0))
}

/// Load a `.vx` grammar module into a [`GrammarSet`]: every top-level
/// `let X = Grammar { … }` becomes a grammar (the `import` binding and anything
/// else is ignored). Frontmatter (a leading ```` ``` ```` fence) is stripped.
pub fn load_str(src: &str) -> Result<GrammarSet> {
    let body = strip_imports(strip_frontmatter(src));
    let parsed = parse_source_file(&body, SourceId(0));
    if !parsed.errors().is_empty() {
        return bail(format!(
            "{} parse error(s): {:?}",
            parsed.errors().len(),
            parsed.errors()
        ));
    }
    let file = parsed.tree();

    let mut grammars = Vec::new();
    for stmt in file.statements() {
        let Stmt::Let(let_stmt) = stmt else { continue };
        let Some(Expr::StructLiteral(lit)) = let_stmt.expr() else {
            continue;
        };
        if last_upper_ident(lit.syntax()).as_deref() == Some("Grammar") {
            grammars.push(lower_grammar(&lit)?);
        }
    }
    Ok(GrammarSet { grammars })
}

/// Strip a leading triple-backtick frontmatter fence, if present, returning the
/// Vixen body. (The pinned parser has no frontmatter support yet.)
fn strip_frontmatter(src: &str) -> &str {
    let t = src.trim_start();
    let Some(open_end) = t.strip_prefix("```").and(t.find('\n')) else {
        return src;
    };
    let Some(rel) = t[open_end..].find("```") else {
        return src;
    };
    let after_close = &t[open_end + rel + 3..];
    match after_close.find('\n') {
        Some(nl) => &after_close[nl + 1..],
        None => "",
    }
}

/// Remove a single import binding (`let … = import "…"`, one or two lines): the
/// pinned parser predates `import`, and the loader resolves grammar/type names
/// structurally, so the import is dead weight here. A future parser bump lets us
/// drop this.
fn strip_imports(body: &str) -> String {
    let Some(imp) = body.find("import \"") else {
        return body.to_string();
    };
    let start = body[..imp].rfind("let ").unwrap_or(imp);
    let end = body[imp..]
        .find('\n')
        .map(|n| imp + n + 1)
        .unwrap_or(body.len());
    let mut out = String::with_capacity(body.len());
    out.push_str(&body[..start]);
    out.push_str(&body[end..]);
    out
}

// ---- type-directed lowering -------------------------------------------------

fn lower_grammar(lit: &StructLiteralExpr) -> Result<Grammar> {
    Ok(Grammar {
        name: req_string(lit, "name")?,
        flags: struct_array(lit, "flags", lower_flag)?,
        env: struct_array(lit, "env", lower_env)?,
        positionals: struct_array(lit, "positionals", lower_positional)?,
        modes: struct_array(lit, "modes", lower_mode)?,
    })
}

fn lower_flag(lit: &StructLiteralExpr) -> Result<Flag> {
    Ok(Flag {
        canonical: req_string(lit, "canonical")?,
        aliases: opt_string_array(lit, "aliases")?,
        spelling: opt_lower(lit, "spelling", lower_spelling)?.unwrap_or(Spelling::Bare),
        policy: opt_lower(lit, "policy", lower_policy)?.unwrap_or(Policy::Once),
        kind: opt_lower(lit, "kind", lower_kind)?.unwrap_or(Kind::Text),
        role: opt_string(lit, "role")?,
        operation: opt_lower(lit, "operation", lower_operation)?.unwrap_or(Operation::Set),
        modes: opt_string_array(lit, "modes")?,
        value: opt_lower(lit, "value", lower_value)?.unwrap_or(ValueDomain::Any),
        doc: opt_string(lit, "doc")?,
    })
}

fn lower_env(lit: &StructLiteralExpr) -> Result<EnvDecl> {
    Ok(EnvDecl {
        name: req_string(lit, "name")?,
        kind: opt_lower(lit, "kind", lower_kind)?.unwrap_or(Kind::Text),
        role: opt_string(lit, "role")?,
        operation: opt_lower(lit, "operation", lower_operation)?.unwrap_or(Operation::Set),
        doc: opt_string(lit, "doc")?,
    })
}

fn lower_positional(lit: &StructLiteralExpr) -> Result<Positional> {
    Ok(Positional {
        name: req_string(lit, "name")?,
        kind: opt_lower(lit, "kind", lower_kind)?.unwrap_or(Kind::Text),
        role: opt_string(lit, "role")?,
        arity: opt_lower(lit, "arity", lower_arity)?.unwrap_or(Arity::One),
        doc: opt_string(lit, "doc")?,
    })
}

fn lower_mode(lit: &StructLiteralExpr) -> Result<Mode> {
    Ok(Mode {
        name: req_string(lit, "name")?,
        selector: opt_string(lit, "selector")?,
    })
}

fn lower_spelling(expr: &Expr) -> Result<Spelling> {
    match variant_of(expr).as_deref() {
        Some("Bare") => Ok(Spelling::Bare),
        Some("Attached") => Ok(Spelling::Attached),
        Some("Separated") => Ok(Spelling::Separated),
        Some("Suffixed") => Ok(Spelling::Suffixed {
            sep: req_string(&as_struct(expr)?, "sep")?,
        }),
        Some("Passthrough") => {
            let s = as_struct(expr)?;
            Ok(Spelling::Passthrough {
                sep: req_string(&s, "sep")?,
                grammar: req_string(&s, "grammar")?,
            })
        }
        other => bail(format!("unknown Spelling variant: {other:?}")),
    }
}

fn lower_policy(expr: &Expr) -> Result<Policy> {
    match variant_of(expr).as_deref() {
        Some("Once") => Ok(Policy::Once),
        Some("Group") => Ok(Policy::Group {
            name: req_string(&as_struct(expr)?, "name")?,
        }),
        Some("DedupOrdered") => Ok(Policy::DedupOrdered),
        Some("DedupUnordered") => Ok(Policy::DedupUnordered),
        Some("OrderedKeepAll") => Ok(Policy::OrderedKeepAll),
        Some("KeyedLastWins") => Ok(Policy::KeyedLastWins),
        Some("Positional") => Ok(Policy::Positional),
        other => bail(format!("unknown Policy variant: {other:?}")),
    }
}

fn lower_kind(expr: &Expr) -> Result<Kind> {
    match variant_of(expr).as_deref() {
        Some("Text") => Ok(Kind::Text),
        Some("Input") => Ok(Kind::Input {
            dir: req_bool(&as_struct(expr)?, "dir")?,
        }),
        Some("Output") => Ok(Kind::Output {
            dir: req_bool(&as_struct(expr)?, "dir")?,
        }),
        Some("InOut") => Ok(Kind::InOut),
        other => bail(format!("unknown Kind variant: {other:?}")),
    }
}

fn lower_operation(expr: &Expr) -> Result<Operation> {
    match variant_of(expr).as_deref() {
        Some("Set") => Ok(Operation::Set),
        Some("Add") => Ok(Operation::Add),
        Some("Remove") => Ok(Operation::Remove),
        Some("Enable") => Ok(Operation::Enable),
        Some("Disable") => Ok(Operation::Disable),
        other => bail(format!("unknown Operation variant: {other:?}")),
    }
}

fn lower_value(expr: &Expr) -> Result<ValueDomain> {
    match variant_of(expr).as_deref() {
        Some("Any") => Ok(ValueDomain::Any),
        Some("OneOf") => Ok(ValueDomain::OneOf {
            values: req_string_array(&as_struct(expr)?, "values")?,
        }),
        Some("Path") => Ok(ValueDomain::Path {
            dir: req_bool(&as_struct(expr)?, "dir")?,
        }),
        Some("Named") => Ok(ValueDomain::Named {
            domain: req_string(&as_struct(expr)?, "domain")?,
        }),
        other => bail(format!("unknown ValueDomain variant: {other:?}")),
    }
}

fn lower_arity(expr: &Expr) -> Result<Arity> {
    match variant_of(expr).as_deref() {
        Some("One") => Ok(Arity::One),
        Some("Optional") => Ok(Arity::Optional),
        Some("Variadic") => Ok(Arity::Variadic),
        other => bail(format!("unknown Arity variant: {other:?}")),
    }
}

// ---- AST helpers ------------------------------------------------------------

/// The variant/struct name of a literal: the **last** `UPPER_IDENT` token that
/// is a direct child (the head path, before any body) — `Grammar` → "Grammar",
/// `Policy.Group { … }` → "Group", `Spelling.Bare` → "Bare".
fn variant_of(expr: &Expr) -> Option<String> {
    last_upper_ident(expr.syntax())
}

fn last_upper_ident(node: &SyntaxNode) -> Option<String> {
    node.children_with_tokens()
        .filter_map(|c| c.into_token())
        .filter(|t| t.kind() == SyntaxKind::UPPER_IDENT)
        .last()
        .map(|t| t.text().to_string())
}

fn field(lit: &StructLiteralExpr, name: &str) -> Option<Expr> {
    lit.fields()
        .find(|f| f.name().is_some_and(|t| t.text() == name))
        .and_then(|f| f.expr())
}

fn as_struct(expr: &Expr) -> Result<StructLiteralExpr> {
    match expr {
        Expr::StructLiteral(s) => Ok(s.clone()),
        _ => bail("expected a struct/variant literal with a body"),
    }
}

fn as_string(expr: &Expr) -> Result<String> {
    if let Expr::Literal(lit) = expr
        && let Some(tok) = lit.token()
        && tok.kind() == SyntaxKind::STRING
    {
        let t = tok.text();
        return Ok(t[1..t.len() - 1].to_string());
    }
    bail("expected a string literal")
}

fn as_bool(expr: &Expr) -> Result<bool> {
    if let Expr::Literal(lit) = expr
        && let Some(tok) = lit.token()
    {
        match tok.kind() {
            SyntaxKind::TRUE_KW => return Ok(true),
            SyntaxKind::FALSE_KW => return Ok(false),
            _ => {}
        }
    }
    bail("expected a boolean literal")
}

fn as_exprs(expr: &Expr) -> Result<Vec<Expr>> {
    let Expr::Array(arr) = expr else {
        return bail("expected an array");
    };
    let mut out = Vec::new();
    for item in arr.items() {
        match item {
            ArrayItem::Expr(e) => out.push(e),
            ArrayItem::Spread(_) => return bail("spread is not supported in grammar arrays"),
        }
    }
    Ok(out)
}

fn req_field(lit: &StructLiteralExpr, name: &str) -> Result<Expr> {
    field(lit, name).ok_or_else(|| LoadError {
        message: format!("missing required field `{name}`"),
    })
}

fn req_string(lit: &StructLiteralExpr, name: &str) -> Result<String> {
    as_string(&req_field(lit, name)?)
}

fn opt_string(lit: &StructLiteralExpr, name: &str) -> Result<String> {
    match field(lit, name) {
        Some(e) => as_string(&e),
        None => Ok(String::new()),
    }
}

fn req_bool(lit: &StructLiteralExpr, name: &str) -> Result<bool> {
    as_bool(&req_field(lit, name)?)
}

fn req_string_array(lit: &StructLiteralExpr, name: &str) -> Result<Vec<String>> {
    as_exprs(&req_field(lit, name)?)?
        .iter()
        .map(as_string)
        .collect()
}

fn opt_string_array(lit: &StructLiteralExpr, name: &str) -> Result<Vec<String>> {
    match field(lit, name) {
        Some(e) => as_exprs(&e)?.iter().map(as_string).collect(),
        None => Ok(Vec::new()),
    }
}

/// Lower an optional field through `f` (a variant lowerer), or `None` if absent.
fn opt_lower<T>(
    lit: &StructLiteralExpr,
    name: &str,
    f: impl Fn(&Expr) -> Result<T>,
) -> Result<Option<T>> {
    match field(lit, name) {
        Some(e) => f(&e).map(Some),
        None => Ok(None),
    }
}

/// Lower an optional array-of-struct-literals field through `f`, or empty.
fn struct_array<T>(
    lit: &StructLiteralExpr,
    name: &str,
    f: impl Fn(&StructLiteralExpr) -> Result<T>,
) -> Result<Vec<T>> {
    let Some(e) = field(lit, name) else {
        return Ok(Vec::new());
    };
    as_exprs(&e)?.iter().map(|e| f(&as_struct(e)?)).collect()
}
