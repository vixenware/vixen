//! `command-model` — describe a CLI tool's argument grammar, parse a raw argv
//! into structure, and reduce it to a **canonical** form, then to the
//! **effective command**: the resolved identity a content-addressed cache keys
//! on. Also the basis for command linting and structured provenance.
//!
//! The crate is deliberately standalone: data in, data out, no dependencies.
//!
//! ## Status
//!
//! [`canonicalize`] folds an argv to a canonical form (cc compile + a link
//! slice: `-l`/`-L`, `-W`, and `-Wl,` as a **recursive** passthrough into an
//! `ld` grammar). [`effective_command`] adds the soundness pillar: the identity
//! is canonical argv **+ the declared env the tool consumes** (explicit only —
//! undeclared env is never read; there is no ambient inheritance) **+ the
//! path-kind inputs** the cache must hash (argv paths, env-valued paths, and
//! inputs bubbled up from passthroughs) **+ impurity marks**.
//!
//! An `MSVC`/`cl.exe` grammar additionally exercises the `/`-lead spellings:
//! DualLead (`/c` ≡ `-c`), ColonAttached (`/std:c++20`), and GluedNoDelim
//! (`/Foout.obj`).
//!
//! Still future work (see `docs/design/command-model.md`): full guarded
//! productions (arbitrary predicates; this has the mode-by-flag first cut),
//! command-level reducers + a coupling graph (the `-W` polarity
//! algebra, cross-flag aliasing), discovered-from-file-content inputs, the
//! caching-hazard neutralizers, and link-order semantics.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Separator for `PATH`-style env vars (slice: unix `:`; Windows `;` is TODO).
const ENV_PATH_SEP: char = ':';

/// How a flag and its value are written on the command line.
#[derive(Debug, Clone, Copy)]
pub enum Spelling {
    /// `-c`, `-fPIC` — the whole token is the flag; no value.
    Bare,
    /// `-O2`, `-DFOO`, `-lm` — value glued directly after the lead.
    Attached,
    /// `-o out`, `-I dir` — value is the following argv token.
    Separated,
    /// `-std=c99`, `/std:c++20` — value after a separator char (`=` or `:`).
    Suffixed { sep: char },
    /// `-Wl,a,b` — the value is a `sep`-joined sub-command in another tool's
    /// `grammar`; canonicalized recursively and re-joined.
    Passthrough {
        sep: &'static str,
        grammar: &'static Grammar,
    },
}

/// Identifier for a set of mutually-exclusive flags (last mention wins).
pub type GroupId = &'static str;

/// How repeated occurrences of a flag collapse during canonicalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// At most one meaningful occurrence; a later one overrides (and lints).
    Once,
    /// Mutually-exclusive set sharing a [`GroupId`]; the last mention wins.
    Group(GroupId),
    /// Repeatable; order is semantic (e.g. include search order); exact
    /// duplicates are dropped (and linted).
    DedupOrdered,
    /// Repeatable; order does not matter; canonical form is sorted + deduped.
    DedupUnordered,
    /// Repeatable; order is semantic and duplicates are kept — e.g. `-l`, where
    /// a repeat can be load-bearing for circular archives.
    OrderedKeepAll,
    /// Keyed values (`key` or `key=value`): dedup by key, last value per key
    /// wins, emitted sorted (e.g. preprocessor defines).
    KeyedLastWins,
    /// Positional input: never reordered, never deduped.
    Positional,
}

/// How the engine treats a value for mounting and caching — the universal,
/// domain-agnostic classification. Small and stable; it never grows with new
/// tools. What the value *means* to the tool (header dir, library, define, …) is
/// the open `role` string the grammar supplies, which the engine never
/// interprets — that's where domain knowledge lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Literal text — hashed as text, never mounted.
    Text,
    /// Input file/dir — mounted read-only, content-hashed into the key.
    Input { dir: bool },
    /// Output target — written by the command; not an input hash.
    Output { dir: bool },
    /// Read and rewritten in place (e.g. an `ar` archive).
    InOut,
}

/// The verb a flag performs on its conceptual knob — the polarity translation
/// must respect (you cannot map an "add to include path" onto a "remove from
/// include path", even though both carry role `"include"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// Assign / replace a value (the common case): `-O2`, `-o out`.
    Set,
    /// Add to a collection: `-I dir`, `-DFOO`, `-lfoo`.
    Add,
    /// Remove from a collection: `-UFOO`.
    Remove,
    /// Turn a knob on: `-Wfoo`, `-frtti`, `-c`.
    Enable,
    /// Turn a knob off: `-Wno-foo`, `-fno-rtti`.
    Disable,
}

/// An environment channel a tool reads. Declared by the grammar so it becomes
/// an *explicit*, keyed input — Vixen never inherits ambient env.
#[derive(Debug, Clone, Copy)]
pub struct EnvDecl {
    pub name: &'static str,
    pub kind: Kind,
    pub role: &'static str,
    /// The verb — an env channel carries a `(role, operation)` semantic just like
    /// a flag, so translation can move a semantic between flag and env.
    pub operation: Operation,
}

/// How many positional arguments a schema slot consumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arity {
    /// Exactly one.
    One,
    /// Zero or one.
    Optional,
    /// Any number (greedily takes the rest).
    Variadic,
}

/// A positional-argument slot — what a positional at this point *means*: its
/// name (for docs/explain), its [`Kind`] (the role it plays — input, output,
/// archive, …), how many it consumes, and its documentation. The ordered list
/// of these is a grammar's positional schema.
#[derive(Debug, Clone, Copy)]
pub struct Positional {
    pub name: &'static str,
    /// Universal cache/IO class.
    pub kind: Kind,
    /// Open domain tag, e.g. `"source"`, `"archive"`.
    pub role: &'static str,
    pub arity: Arity,
    pub doc: &'static str,
}

/// A single flag in a tool grammar.
#[derive(Debug, Clone, Copy)]
pub struct Flag {
    /// Canonical lead spelling, e.g. `-O`, `-D`, `-I`, `-o`, `-c`, `-Wl,`.
    pub canonical: &'static str,
    /// Accepted alternate spellings that canonicalize to `canonical`.
    pub aliases: &'static [&'static str],
    pub spelling: Spelling,
    pub policy: Policy,
    /// Universal cache/IO class of the value.
    pub kind: Kind,
    /// Open domain tag — `"include"`, `"lib"`, `"define"`, … — engine-opaque,
    /// for explain/completion and as translate's cross-grammar key.
    pub role: &'static str,
    /// The verb (set/add/remove/enable/disable). Translation matches on
    /// `(role, operation, mode)` — role is the noun, operation the verb.
    pub operation: Operation,
    /// The modes this flag is legal in (by name); empty = all modes. Part of the
    /// `(role, operation, mode)` translation key.
    pub modes: &'static [&'static str],
}

impl Flag {
    /// Baseline for terse `..Flag::DEFAULT` construction: a bare, set-once,
    /// text-valued flag with no aliases and an empty role.
    pub const DEFAULT: Flag = Flag {
        canonical: "",
        aliases: &[],
        spelling: Spelling::Bare,
        policy: Policy::Once,
        kind: Kind::Text,
        role: "",
        operation: Operation::Set,
        modes: &[],
    };
}

/// A mode a command can be in. The default mode has no `selector`; a non-default
/// mode is entered when its `selector` flag is present. Mode names are a
/// grammar's own vocabulary — a C grammar may call its default mode `"link"`;
/// the model itself knows nothing domain-specific.
#[derive(Debug, Clone, Copy)]
pub struct Mode {
    pub name: &'static str,
    pub selector: Option<&'static str>,
}

/// A tool grammar: flags, env channels, the positional schema, and modes.
/// Anything not matching a flag is a positional input. (Richer guarded
/// productions — arbitrary predicates over parse state — remain future.)
#[derive(Debug, Clone, Copy)]
pub struct Grammar {
    pub flags: &'static [Flag],
    pub env: &'static [EnvDecl],
    /// The command's modes. The default mode has no selector; a non-default mode
    /// is entered by its selector flag. A flag is legal only in the modes it
    /// lists (all, if it lists none). Empty modes = a single unnamed mode.
    pub modes: &'static [Mode],
    /// The positional-argument schema: an ordered sequence of slots giving each
    /// positional its meaning (name, role, arity, doc). Resolved against the
    /// actual positionals by [`parse_command`].
    pub positionals: &'static [Positional],
}

/// One parsed flag occurrence, remembering its argv position for last-wins.
struct Occurrence {
    flag: usize,
    value: Option<String>,
    pos: usize,
}

struct Parsed {
    occurrences: Vec<Occurrence>,
    positionals: Vec<String>,
    unknown: Vec<String>,
}

impl Grammar {
    /// Match a token against the grammar. Returns
    /// `(flag_index, in_token_value, consumes_next)`. Exact (`Bare`/`Separated`)
    /// and `lead<sep>` (`Suffixed`) matches win over the longest prefix match
    /// (`Attached`/`Passthrough`).
    fn match_token(&self, token: &str) -> Option<(usize, Option<String>, bool)> {
        let mut best_prefix: Option<(usize, usize)> = None;
        for (i, flag) in self.flags.iter().enumerate() {
            for lead in std::iter::once(flag.canonical).chain(flag.aliases.iter().copied()) {
                match flag.spelling {
                    Spelling::Bare if token == lead => return Some((i, None, false)),
                    Spelling::Separated if token == lead => return Some((i, None, true)),
                    Spelling::Suffixed { sep } => {
                        if let Some(value) =
                            token.strip_prefix(lead).and_then(|r| r.strip_prefix(sep))
                        {
                            return Some((i, Some(value.to_string()), false));
                        }
                    }
                    Spelling::Attached | Spelling::Passthrough { .. }
                        if token.len() > lead.len()
                            && token.starts_with(lead)
                            && best_prefix.is_none_or(|(_, l)| lead.len() > l) =>
                    {
                        best_prefix = Some((i, lead.len()));
                    }
                    _ => {}
                }
            }
        }
        best_prefix.map(|(i, lead_len)| (i, Some(token[lead_len..].to_string()), false))
    }

    fn parse(&self, args: &[String]) -> Parsed {
        let mut occurrences = Vec::new();
        let mut positionals = Vec::new();
        let mut unknown = Vec::new();
        let mut i = 0;
        while i < args.len() {
            let token = &args[i];
            if let Some((flag, in_value, consumes_next)) = self.match_token(token) {
                let value = if consumes_next {
                    i += 1;
                    args.get(i).cloned()
                } else {
                    in_value
                };
                occurrences.push(Occurrence {
                    flag,
                    value,
                    pos: i,
                });
            } else if token.starts_with('-') && token != "-" {
                unknown.push(token.clone());
            } else {
                positionals.push(token.clone());
            }
            i += 1;
        }
        Parsed {
            occurrences,
            positionals,
            unknown,
        }
    }
}

/// A command reduced to its canonical argv form, plus the lints found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalCommand {
    pub program: String,
    pub args: Vec<String>,
    pub lints: Vec<Lint>,
}

/// A range of argv token indices (inclusive), relative to the args after the
/// program. For diagnostics and tooling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

/// The fully role-tagged parse of a command: every token resolved to the part
/// it plays. The structured view behind completion, hover-docs, validation, and
/// "explain this command".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommand {
    pub program: String,
    pub args: Vec<Arg>,
}

/// One token of a [`ParsedCommand`], with its span and resolved role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arg {
    pub span: Span,
    /// The token as written.
    pub spelling: String,
    pub role: ArgRole,
}

/// What part a token plays in the command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgRole {
    /// A recognized flag, with its canonical name, value, and role-kind.
    Flag {
        canonical: String,
        value: Option<String>,
        kind: Kind,
        role: String,
    },
    /// A positional resolved against the grammar's schema — it knows its meaning.
    Positional {
        name: String,
        kind: Kind,
        value: String,
    },
    /// A `-`-leading token that matched no flag.
    Unknown,
}

/// The resolved identity a content-addressed cache keys on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveCommand {
    pub program: String,
    /// Canonical argv (without the program token).
    pub args: Vec<String>,
    /// The declared env channels the tool reads that are present, sorted.
    pub env: Vec<(String, String)>,
    /// Path-kind inputs the cache must content-hash, deduped.
    pub inputs: Vec<Input>,
    /// Reasons this command cannot be soundly cached as-is.
    pub impurities: Vec<Impurity>,
    pub lints: Vec<Lint>,
}

/// A path the command depends on, tagged with its open domain role.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Input {
    pub role: String,
    pub path: String,
}

/// A reason the command's result cannot be soundly cached by output equality.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Impurity {
    /// A `-`-leading token matched no flag, so its effect is unknown.
    UnknownFlag { token: String },
    /// A flag was used in a mode it is not legal in.
    IllegalInMode { flag: String, mode: String },
}

/// One difference between two [`EffectiveCommand`]s — the structured answer to
/// "why did this rebuild?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Program {
        from: String,
        to: String,
    },
    ArgAdded(String),
    ArgRemoved(String),
    EnvChanged {
        name: String,
        from: Option<String>,
        to: Option<String>,
    },
    InputAdded(Input),
    InputRemoved(Input),
}

/// Something the canonicalizer found worth reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lint {
    /// A flag's value was discarded because a later one (same group) won.
    Overridden {
        flag: String,
        dropped: String,
        kept: String,
    },
    /// The same key was given twice, identically (e.g. a duplicate define).
    DuplicateKey { key: String },
    /// The same key was given two different values.
    ConflictingKey {
        key: String,
        first: String,
        second: String,
    },
    /// A repeatable, order-significant flag listed the same value twice.
    Duplicate { flag: String, value: String },
    /// A `-`-leading token matched no flag in the grammar.
    UnknownFlag { token: String },
    /// A flag was used in a mode it is not legal in.
    IllegalInMode { flag: String, mode: String },
}

impl fmt::Display for Lint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Lint::Overridden {
                flag,
                dropped,
                kept,
            } => {
                write!(f, "{flag}: `{dropped}` overridden by `{kept}`")
            }
            Lint::DuplicateKey { key } => write!(f, "duplicate `{key}`"),
            Lint::ConflictingKey { key, first, second } => {
                write!(f, "conflicting `{key}`: `{first}` then `{second}`")
            }
            Lint::Duplicate { flag, value } => write!(f, "duplicate {flag} `{value}`"),
            Lint::UnknownFlag { token } => write!(f, "unknown flag `{token}` (not in grammar)"),
            Lint::IllegalInMode { flag, mode } => {
                write!(f, "{flag} is not legal in mode `{mode}`")
            }
        }
    }
}

fn emit(flag: &Flag, value: Option<&str>) -> Vec<String> {
    match flag.spelling {
        Spelling::Bare => vec![flag.canonical.to_string()],
        Spelling::Attached | Spelling::Passthrough { .. } => {
            vec![format!("{}{}", flag.canonical, value.unwrap_or(""))]
        }
        Spelling::Separated => {
            vec![flag.canonical.to_string(), value.unwrap_or("").to_string()]
        }
        Spelling::Suffixed { sep } => {
            vec![format!("{}{}{}", flag.canonical, sep, value.unwrap_or(""))]
        }
    }
}

fn emit_one(flag: &Flag, value: Option<&str>) -> String {
    emit(flag, value).join(" ")
}

/// Whether a value of this kind is a cache input (mounted + content-hashed).
fn is_input(kind: Kind) -> bool {
    matches!(kind, Kind::Input { .. } | Kind::InOut)
}

fn dedup_inputs(inputs: &mut Vec<Input>) {
    let mut seen = BTreeSet::new();
    inputs.retain(|i| seen.insert((i.role.clone(), i.path.clone())));
}

/// Resolve each positional (in order) to its schema slot, or `None` if the
/// schema has no slot for it.
fn resolve_positionals(schema: &[Positional], count: usize) -> Vec<Option<&Positional>> {
    let mut out: Vec<Option<&Positional>> = Vec::with_capacity(count);
    let mut p = 0;
    for slot in schema {
        match slot.arity {
            Arity::One | Arity::Optional => {
                if p < count {
                    out.push(Some(slot));
                    p += 1;
                }
            }
            Arity::Variadic => {
                while p < count {
                    out.push(Some(slot));
                    p += 1;
                }
            }
        }
    }
    while out.len() < count {
        out.push(None);
    }
    out
}

/// Collect the inputs a flag's occurrences contribute, recursing into passthrough
/// sub-grammars so their inner paths bubble up. The input's role is the flag's
/// open domain tag.
fn collect_inputs(flag: &Flag, occs: &[&Occurrence], inputs: &mut Vec<Input>) {
    if let Spelling::Passthrough { sep, grammar } = flag.spelling {
        let mut items: Vec<String> = Vec::new();
        for o in occs {
            if let Some(value) = &o.value {
                items.extend(value.split(sep).map(str::to_string));
            }
        }
        inputs.extend(canon_pass(grammar, &items).inputs);
        return;
    }
    if is_input(flag.kind) {
        for o in occs {
            if let Some(value) = &o.value {
                inputs.push(Input {
                    role: flag.role.to_string(),
                    path: value.clone(),
                });
            }
        }
    }
}

fn reduce_flag(flag: &Flag, occs: &[&Occurrence], lints: &mut Vec<Lint>) -> Vec<String> {
    // A passthrough value is itself a sub-command: split, canonicalize against
    // the sub-grammar, re-join. Items from repeated occurrences concatenate.
    if let Spelling::Passthrough { sep, grammar } = flag.spelling {
        let mut items: Vec<String> = Vec::new();
        for o in occs {
            if let Some(value) = &o.value {
                items.extend(value.split(sep).map(str::to_string));
            }
        }
        let sub = canon_pass(grammar, &items);
        lints.extend(sub.lints);
        return vec![format!("{}{}", flag.canonical, sub.args.join(sep))];
    }

    match flag.policy {
        Policy::Once | Policy::Group(_) => {
            let last = occs.iter().max_by_key(|o| o.pos).expect("non-empty");
            for o in occs {
                if o.pos != last.pos && o.value != last.value {
                    lints.push(Lint::Overridden {
                        flag: flag.canonical.to_string(),
                        dropped: emit_one(flag, o.value.as_deref()),
                        kept: emit_one(flag, last.value.as_deref()),
                    });
                }
            }
            emit(flag, last.value.as_deref())
        }
        Policy::DedupOrdered => {
            let mut seen: Vec<String> = Vec::new();
            let mut out = Vec::new();
            for o in occs {
                let value = o.value.clone().unwrap_or_default();
                if seen.contains(&value) {
                    lints.push(Lint::Duplicate {
                        flag: flag.canonical.to_string(),
                        value,
                    });
                } else {
                    out.extend(emit(flag, Some(&value)));
                    seen.push(value);
                }
            }
            out
        }
        Policy::DedupUnordered => {
            let set: BTreeSet<String> = occs
                .iter()
                .map(|o| o.value.clone().unwrap_or_default())
                .collect();
            set.iter().flat_map(|v| emit(flag, Some(v))).collect()
        }
        Policy::OrderedKeepAll => occs
            .iter()
            .flat_map(|o| emit(flag, o.value.as_deref()))
            .collect(),
        Policy::KeyedLastWins => {
            // Keyed values; last value per key wins; emit sorted by key.
            let mut map: BTreeMap<String, Option<String>> = BTreeMap::new();
            for o in occs {
                let raw = o.value.clone().unwrap_or_default();
                let (key, value) = match raw.split_once('=') {
                    Some((k, v)) => (k.to_string(), Some(v.to_string())),
                    None => (raw, None),
                };
                match map.get(&key) {
                    Some(prev) if *prev == value => {
                        lints.push(Lint::DuplicateKey { key: key.clone() })
                    }
                    Some(prev) => lints.push(Lint::ConflictingKey {
                        key: key.clone(),
                        first: prev.clone().unwrap_or_default(),
                        second: value.clone().unwrap_or_default(),
                    }),
                    None => {}
                }
                map.insert(key, value);
            }
            map.into_iter()
                .flat_map(|(name, value)| {
                    let payload = match value {
                        Some(v) => format!("{name}={v}"),
                        None => name,
                    };
                    emit(flag, Some(&payload))
                })
                .collect()
        }
        Policy::Positional => Vec::new(),
    }
}

/// The full canonicalization pass over a bare argument list: canonical argv,
/// lints, path inputs, and impurities. The basis for both public entry points
/// and the recursion through passthroughs.
struct Pass {
    args: Vec<String>,
    lints: Vec<Lint>,
    inputs: Vec<Input>,
    impurities: Vec<Impurity>,
}

/// The active mode for a set of present canonical flags: the non-default mode
/// whose selector is present, else the default (selector-less) mode, else none.
fn resolve_mode<'g>(grammar: &'g Grammar, present: &BTreeSet<&str>) -> Option<&'g str> {
    grammar
        .modes
        .iter()
        .find(|m| m.selector.is_some_and(|s| present.contains(&s)))
        .or_else(|| grammar.modes.iter().find(|m| m.selector.is_none()))
        .map(|m| m.name)
}

fn canon_pass(grammar: &Grammar, args: &[String]) -> Pass {
    let parsed = grammar.parse(args);
    let mut lints = Vec::new();
    let mut inputs: Vec<Input> = Vec::new();
    let mut impurities = Vec::new();

    let mut by_flag: BTreeMap<usize, Vec<&Occurrence>> = BTreeMap::new();
    for occ in &parsed.occurrences {
        by_flag.entry(occ.flag).or_default().push(occ);
    }

    // Mode guard: the active mode is the non-default mode whose selector flag is
    // present, else the default (selector-less) mode. A flag that lists modes is
    // legal only in those; using it elsewhere is an impurity.
    let present: BTreeSet<&str> = parsed
        .occurrences
        .iter()
        .map(|o| grammar.flags[o.flag].canonical)
        .collect();
    let active_mode = resolve_mode(grammar, &present);

    let mut flag_tokens: Vec<String> = Vec::new();
    let mut output_tokens: Vec<String> = Vec::new();
    for (idx, flag) in grammar.flags.iter().enumerate() {
        let Some(occs) = by_flag.get(&idx) else {
            continue;
        };
        let emitted = reduce_flag(flag, occs, &mut lints);
        collect_inputs(flag, occs, &mut inputs);
        if let Some(mode) = active_mode
            && !flag.modes.is_empty()
            && !flag.modes.contains(&mode)
        {
            lints.push(Lint::IllegalInMode {
                flag: flag.canonical.to_string(),
                mode: mode.to_string(),
            });
            impurities.push(Impurity::IllegalInMode {
                flag: flag.canonical.to_string(),
                mode: mode.to_string(),
            });
        }
        if matches!(flag.kind, Kind::Output { .. }) {
            output_tokens.extend(emitted);
        } else {
            flag_tokens.extend(emitted);
        }
    }

    let mut out = flag_tokens;
    let slots = resolve_positionals(grammar.positionals, parsed.positionals.len());
    for (p, slot) in parsed.positionals.iter().zip(&slots) {
        out.push(p.clone());
        if let Some(slot) = slot
            && is_input(slot.kind)
        {
            inputs.push(Input {
                role: slot.role.to_string(),
                path: p.clone(),
            });
        }
    }
    out.extend(output_tokens);
    for token in &parsed.unknown {
        lints.push(Lint::UnknownFlag {
            token: token.clone(),
        });
        impurities.push(Impurity::UnknownFlag {
            token: token.clone(),
        });
        out.push(token.clone());
    }

    dedup_inputs(&mut inputs);
    Pass {
        args: out,
        lints,
        inputs,
        impurities,
    }
}

/// Assign parsed positionals to the grammar's schema slots, in order, tagging
/// each with its meaning. Leftover positionals (no slot) stay `Unknown`.
fn assign_positionals(schema: &[Positional], positionals: &[(usize, String)], out: &mut [Arg]) {
    let mut p = 0;
    for slot in schema {
        match slot.arity {
            Arity::One | Arity::Optional => {
                if let Some((idx, value)) = positionals.get(p) {
                    out[*idx].role = ArgRole::Positional {
                        name: slot.name.to_string(),
                        kind: slot.kind,
                        value: value.clone(),
                    };
                    p += 1;
                }
            }
            Arity::Variadic => {
                while let Some((idx, value)) = positionals.get(p) {
                    out[*idx].role = ArgRole::Positional {
                        name: slot.name.to_string(),
                        kind: slot.kind,
                        value: value.clone(),
                    };
                    p += 1;
                }
            }
        }
    }
}

/// Parse `argv` into the fully role-tagged [`ParsedCommand`]: every flag
/// resolved to its canonical name + value + kind, every positional resolved
/// against the grammar's schema to its meaning, every token carrying its span.
/// This is the structured view; [`canonicalize`] / [`effective_command`] reduce
/// it to a cache identity.
pub fn parse_command(grammar: &Grammar, argv: &[String]) -> ParsedCommand {
    let program = argv.first().cloned().unwrap_or_default();
    let args = argv.get(1..).unwrap_or(&[]);

    let mut out: Vec<Arg> = Vec::new();
    // (index into `out`, value) for each positional, assigned to schema slots
    // after the walk.
    let mut positionals: Vec<(usize, String)> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let start = i;
        let token = &args[i];
        let role = if let Some((flag_idx, in_value, consumes_next)) = grammar.match_token(token) {
            let flag = &grammar.flags[flag_idx];
            let value = if consumes_next {
                i += 1;
                args.get(i).cloned()
            } else {
                in_value
            };
            ArgRole::Flag {
                canonical: flag.canonical.to_string(),
                value,
                kind: flag.kind,
                role: flag.role.to_string(),
            }
        } else if token.starts_with('-') && token != "-" {
            ArgRole::Unknown
        } else {
            positionals.push((out.len(), token.clone()));
            ArgRole::Unknown
        };
        out.push(Arg {
            span: Span { start, end: i },
            spelling: token.clone(),
            role,
        });
        i += 1;
    }

    assign_positionals(grammar.positionals, &positionals, &mut out);
    ParsedCommand { program, args: out }
}

/// Parse `argv` against `grammar` and reduce it to its canonical argv form.
/// `argv[0]` is the program, preserved verbatim. For the full cache identity
/// (env + inputs + impurities), use [`effective_command`].
pub fn canonicalize(grammar: &Grammar, argv: &[String]) -> CanonicalCommand {
    let program = argv.first().cloned().unwrap_or_default();
    let pass = canon_pass(grammar, argv.get(1..).unwrap_or(&[]));
    CanonicalCommand {
        program,
        args: pass.args,
        lints: pass.lints,
    }
}

/// Resolve `argv` (and the explicit `env` the build provides) into the
/// effective command — the identity a content-addressed cache keys on.
///
/// Only env channels the grammar declares are consulted; everything else in
/// `env` is ignored. Vixen never inherits ambient host env, so this is the
/// whole env surface, made explicit and keyed.
pub fn effective_command(
    grammar: &Grammar,
    argv: &[String],
    env: &BTreeMap<String, String>,
) -> EffectiveCommand {
    let program = argv.first().cloned().unwrap_or_default();
    let mut pass = canon_pass(grammar, argv.get(1..).unwrap_or(&[]));

    let mut consumed_env: Vec<(String, String)> = Vec::new();
    for decl in grammar.env {
        let Some(value) = env.get(decl.name) else {
            continue;
        };
        consumed_env.push((decl.name.to_string(), value.clone()));
        if is_input(decl.kind) {
            for part in value.split(ENV_PATH_SEP).filter(|s| !s.is_empty()) {
                pass.inputs.push(Input {
                    role: decl.role.to_string(),
                    path: part.to_string(),
                });
            }
        }
    }
    consumed_env.sort();
    dedup_inputs(&mut pass.inputs);

    EffectiveCommand {
        program,
        args: pass.args,
        env: consumed_env,
        inputs: pass.inputs,
        impurities: pass.impurities,
        lints: pass.lints,
    }
}

/// Diff two effective commands — the structured "why did it rebuild?": exactly
/// which args, env channels, and inputs differ in the cache identity. An empty
/// result means the two commands share a cache key.
pub fn diff(a: &EffectiveCommand, b: &EffectiveCommand) -> Vec<Change> {
    let mut changes = Vec::new();

    if a.program != b.program {
        changes.push(Change::Program {
            from: a.program.clone(),
            to: b.program.clone(),
        });
    }

    // Args as a multiset: positive count = only in `a` (removed), negative = added.
    let mut counts: BTreeMap<&str, i64> = BTreeMap::new();
    for arg in &a.args {
        *counts.entry(arg.as_str()).or_default() += 1;
    }
    for arg in &b.args {
        *counts.entry(arg.as_str()).or_default() -= 1;
    }
    for (arg, count) in &counts {
        for _ in 0..count.unsigned_abs() {
            changes.push(if *count > 0 {
                Change::ArgRemoved(arg.to_string())
            } else {
                Change::ArgAdded(arg.to_string())
            });
        }
    }

    // Env: keyed comparison.
    let am: BTreeMap<&str, &str> = a
        .env
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let bm: BTreeMap<&str, &str> = b
        .env
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let keys: BTreeSet<&str> = am.keys().chain(bm.keys()).copied().collect();
    for k in keys {
        let (av, bv) = (am.get(k), bm.get(k));
        if av != bv {
            changes.push(Change::EnvChanged {
                name: k.to_string(),
                from: av.map(|s| s.to_string()),
                to: bv.map(|s| s.to_string()),
            });
        }
    }

    // Inputs are deduped sets.
    for input in &a.inputs {
        if !b.inputs.contains(input) {
            changes.push(Change::InputRemoved(input.clone()));
        }
    }
    for input in &b.inputs {
        if !a.inputs.contains(input) {
            changes.push(Change::InputAdded(input.clone()));
        }
    }

    changes
}

/// A minimal `ld` grammar — enough for the `-Wl,` passthroughs `libpng` emits.
pub const LD: Grammar = Grammar {
    modes: &[],
    positionals: &[Positional {
        name: "object",
        kind: Kind::Input { dir: false },
        role: "object",
        arity: Arity::Variadic,
        doc: "an object file to link",
    }],
    env: &[],
    flags: &[
        Flag {
            canonical: "--gc-sections",
            role: "gc-sections",
            operation: Operation::Enable,
            ..Flag::DEFAULT
        },
        Flag {
            canonical: "--as-needed",
            role: "as-needed",
            operation: Operation::Enable,
            ..Flag::DEFAULT
        },
        Flag {
            canonical: "--version-script",
            spelling: Spelling::Suffixed { sep: '=' },
            kind: Kind::Input { dir: false },
            role: "version-script",
            ..Flag::DEFAULT
        },
    ],
};

/// The `cc`/clang grammar — the compile invocation `libpng` drives, plus a slice
/// of linking. Gcc-style; MSVC and the full surface are future work.
pub const CC: Grammar = Grammar {
    modes: &[
        Mode {
            name: "link",
            selector: None,
        },
        Mode {
            name: "compile",
            selector: Some("-c"),
        },
    ],
    positionals: &[Positional {
        name: "input",
        kind: Kind::Input { dir: false },
        role: "source",
        arity: Arity::Variadic,
        doc: "a source (.c) or object (.o) file to compile or link",
    }],
    env: &[
        EnvDecl {
            name: "CPATH",
            kind: Kind::Input { dir: true },
            role: "include",
            operation: Operation::Add,
        },
        EnvDecl {
            name: "LIBRARY_PATH",
            kind: Kind::Input { dir: true },
            role: "lib-dir",
            operation: Operation::Add,
        },
        EnvDecl {
            name: "SOURCE_DATE_EPOCH",
            kind: Kind::Text,
            role: "source-date",
            operation: Operation::Set,
        },
    ],
    flags: &[
        Flag {
            canonical: "-c",
            role: "compile-only",
            operation: Operation::Enable,
            ..Flag::DEFAULT
        },
        Flag {
            canonical: "-fPIC",
            role: "pic",
            operation: Operation::Enable,
            ..Flag::DEFAULT
        },
        Flag {
            canonical: "-shared",
            role: "shared",
            operation: Operation::Enable,
            modes: &["link"],
            ..Flag::DEFAULT
        },
        Flag {
            canonical: "-O",
            spelling: Spelling::Attached,
            policy: Policy::Group("opt"),
            role: "optimization",
            ..Flag::DEFAULT
        },
        Flag {
            canonical: "-std",
            spelling: Spelling::Suffixed { sep: '=' },
            role: "std",
            ..Flag::DEFAULT
        },
        Flag {
            canonical: "-W",
            spelling: Spelling::Attached,
            policy: Policy::DedupUnordered,
            role: "warning",
            operation: Operation::Enable,
            ..Flag::DEFAULT
        },
        Flag {
            canonical: "-D",
            spelling: Spelling::Attached,
            policy: Policy::KeyedLastWins,
            role: "define",
            operation: Operation::Add,
            ..Flag::DEFAULT
        },
        Flag {
            canonical: "-I",
            spelling: Spelling::Separated,
            policy: Policy::DedupOrdered,
            kind: Kind::Input { dir: true },
            role: "include",
            operation: Operation::Add,
            ..Flag::DEFAULT
        },
        Flag {
            canonical: "-L",
            spelling: Spelling::Separated,
            policy: Policy::DedupOrdered,
            kind: Kind::Input { dir: true },
            role: "lib-dir",
            operation: Operation::Add,
            ..Flag::DEFAULT
        },
        Flag {
            canonical: "-l",
            spelling: Spelling::Attached,
            policy: Policy::OrderedKeepAll,
            role: "lib",
            operation: Operation::Add,
            modes: &["link"],
            ..Flag::DEFAULT
        },
        Flag {
            canonical: "-Wl,",
            spelling: Spelling::Passthrough {
                sep: ",",
                grammar: &LD,
            },
            policy: Policy::OrderedKeepAll,
            role: "linker-args",
            operation: Operation::Add,
            modes: &["link"],
            ..Flag::DEFAULT
        },
        Flag {
            canonical: "-o",
            spelling: Spelling::Separated,
            kind: Kind::Output { dir: false },
            role: "output",
            ..Flag::DEFAULT
        },
    ],
};

/// The `cl.exe`/clang-cl grammar — proves the MSVC spellings: DualLead
/// (`/c` ≡ `-c`, via aliases), ColonAttached (`/std:c++20`, a
/// `Suffixed { sep: ':' }`), and GluedNoDelim (`/Foout.obj`, `/Iinc`, which fall
/// out of longest-prefix matching). Slice; `/link` mode-switching awaits guarded
/// productions.
pub const CL: Grammar = Grammar {
    modes: &[
        Mode {
            name: "link",
            selector: None,
        },
        Mode {
            name: "compile",
            selector: Some("/c"),
        },
    ],
    positionals: &[Positional {
        name: "input",
        kind: Kind::Input { dir: false },
        role: "source",
        arity: Arity::Variadic,
        doc: "a source or object file to compile or link",
    }],
    env: &[
        EnvDecl {
            name: "INCLUDE",
            kind: Kind::Input { dir: true },
            role: "include",
            operation: Operation::Add,
        },
        EnvDecl {
            name: "LIB",
            kind: Kind::Input { dir: true },
            role: "lib-dir",
            operation: Operation::Add,
        },
    ],
    flags: &[
        Flag {
            canonical: "/c",
            aliases: &["-c"],
            role: "compile-only",
            operation: Operation::Enable,
            ..Flag::DEFAULT
        },
        Flag {
            canonical: "/nologo",
            aliases: &["-nologo"],
            role: "nologo",
            operation: Operation::Enable,
            ..Flag::DEFAULT
        },
        Flag {
            canonical: "/EHsc",
            aliases: &["-EHsc"],
            role: "exceptions",
            operation: Operation::Enable,
            ..Flag::DEFAULT
        },
        Flag {
            canonical: "/O",
            aliases: &["-O"],
            spelling: Spelling::Attached,
            policy: Policy::Group("opt"),
            role: "optimization",
            ..Flag::DEFAULT
        },
        Flag {
            canonical: "/W",
            aliases: &["-W"],
            spelling: Spelling::Attached,
            policy: Policy::Group("warn"),
            role: "warning",
            operation: Operation::Enable,
            ..Flag::DEFAULT
        },
        Flag {
            canonical: "/std",
            aliases: &["-std"],
            spelling: Spelling::Suffixed { sep: ':' },
            role: "std",
            ..Flag::DEFAULT
        },
        Flag {
            canonical: "/D",
            aliases: &["-D"],
            spelling: Spelling::Attached,
            policy: Policy::KeyedLastWins,
            role: "define",
            operation: Operation::Add,
            ..Flag::DEFAULT
        },
        Flag {
            canonical: "/I",
            aliases: &["-I"],
            spelling: Spelling::Attached,
            policy: Policy::DedupOrdered,
            kind: Kind::Input { dir: true },
            role: "include",
            operation: Operation::Add,
            modes: &[],
        },
        Flag {
            canonical: "/Fo",
            aliases: &["-Fo"],
            spelling: Spelling::Attached,
            kind: Kind::Output { dir: false },
            role: "output",
            modes: &["compile"],
            ..Flag::DEFAULT
        },
        Flag {
            canonical: "/Fe",
            aliases: &["-Fe"],
            spelling: Spelling::Attached,
            kind: Kind::Output { dir: false },
            role: "output",
            modes: &["link"],
            ..Flag::DEFAULT
        },
    ],
};

/// A tiny `ar` grammar — its point is positional *meaning*: the operation, then
/// the archive, then the members. Shows fixed-then-variadic positional
/// resolution, where each positional knows the distinct role it plays.
pub const AR: Grammar = Grammar {
    modes: &[],
    flags: &[],
    env: &[],
    positionals: &[
        Positional {
            name: "operation",
            kind: Kind::Text,
            role: "operation",
            arity: Arity::One,
            doc: "the operation + modifiers, e.g. `rcs`",
        },
        Positional {
            name: "archive",
            kind: Kind::InOut,
            role: "archive",
            arity: Arity::One,
            doc: "the .a archive to create or update",
        },
        Positional {
            name: "member",
            kind: Kind::Input { dir: false },
            role: "object",
            arity: Arity::Variadic,
            doc: "an object file to add to the archive",
        },
    ],
};

/// A human-readable breakdown of a command — one item per token, each explaining
/// what it is and the part it plays. The basis for "explain this command" and
/// LSP hover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Explanation {
    pub program: String,
    pub items: Vec<ExplainItem>,
}

/// One explained token of a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplainItem {
    pub span: Span,
    pub spelling: String,
    pub summary: String,
}

impl fmt::Display for Explanation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.program)?;
        for item in &self.items {
            writeln!(f, "  {:<16} {}", item.spelling, item.summary)?;
        }
        Ok(())
    }
}

/// A short human description of a value's cache/IO class.
fn describe_io(kind: Kind) -> &'static str {
    match kind {
        Kind::Text => "literal",
        Kind::Input { dir: true } => "input directory",
        Kind::Input { dir: false } => "input file",
        Kind::Output { dir: true } => "output directory",
        Kind::Output { dir: false } => "output path",
        Kind::InOut => "modified in place",
    }
}

/// Explain a command: parse it and describe every token by the part it plays.
/// Flags are described from their role-kind; positionals from their schema slot
/// (name + doc). When flags gain `doc`, that takes precedence here.
pub fn explain(grammar: &Grammar, argv: &[String]) -> Explanation {
    let parsed = parse_command(grammar, argv);
    let items = parsed
        .args
        .iter()
        .map(|arg| {
            let summary = match &arg.role {
                ArgRole::Flag {
                    canonical,
                    value,
                    kind,
                    role,
                } => {
                    let io = describe_io(*kind);
                    let what = if role.is_empty() {
                        io.to_string()
                    } else {
                        format!("{role} ({io})")
                    };
                    match value {
                        Some(v) => format!("{canonical}: {what} = {v}"),
                        None => format!("{canonical}: {what}"),
                    }
                }
                ArgRole::Positional { name, kind, value } => {
                    let doc = grammar
                        .positionals
                        .iter()
                        .find(|p| p.name == name.as_str())
                        .map(|p| p.doc)
                        .unwrap_or("");
                    format!("{value}: {name} — {doc} ({})", describe_io(*kind))
                }
                ArgRole::Unknown => {
                    "unrecognized — impure (not understood by the grammar)".to_string()
                }
            };
            ExplainItem {
                span: arg.span,
                spelling: arg.spelling.clone(),
                summary,
            }
        })
        .collect();
    Explanation {
        program: parsed.program,
        items,
    }
}

/// A semantic the target grammar expresses through no mechanism — surfaced,
/// never silently dropped. `token` is the source spelling, for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Untranslatable {
    pub token: String,
}

/// One item of a command's **semantic IR** — the spelling-independent meaning a
/// command is lowered to before being re-emitted into another grammar. A
/// `Semantic` is the `(role, operation, value)` of a flag; a `Positional` is a
/// raw value that carries over by position. Order is preserved, so re-emission
/// keeps the source layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrItem {
    /// A flag's meaning, independent of how any tool spells it.
    Semantic {
        role: String,
        operation: Operation,
        value: Option<String>,
        /// The source token, kept only for diagnostics (e.g. `Untranslatable`).
        source_token: String,
    },
    /// A positional value, passed through by position.
    Positional { value: String },
}

/// A command lowered to its semantic IR: an ordered list of meaning-bearing
/// items, the command's active `mode` (so a mode-split target like MSVC's
/// `/Fo`÷`/Fe` for `output` can be disambiguated), and any tokens too opaque to
/// even lift (unrecognized flags).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandIr {
    pub items: Vec<IrItem>,
    pub mode: Option<String>,
    pub untranslatable: Vec<Untranslatable>,
}

/// The holistic result of emitting a [`CommandIr`] into a grammar: the target's
/// argv **and** the env it expresses semantics through. A semantic with no
/// target flag may still have a target env channel — `cc -L dir` becomes MSVC's
/// `LIB=dir` — so the result is the whole `(argv, env)`, not just args. Genuine
/// gaps land in `untranslatable`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslatedCommand {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub untranslatable: Vec<Untranslatable>,
}

/// Lower a command to its semantic IR against the grammar it was written for:
/// each recognized flag becomes a `(role, operation, value)` `Semantic`,
/// positionals carry through by position, and the command's active mode is
/// resolved. Unrecognized (`-`-leading) tokens cannot be lifted and are recorded
/// in `untranslatable`.
pub fn lift(grammar: &Grammar, argv: &[String]) -> CommandIr {
    let parsed = parse_command(grammar, argv);
    let present: BTreeSet<&str> = parsed
        .args
        .iter()
        .filter_map(|a| match &a.role {
            ArgRole::Flag { canonical, .. } => Some(canonical.as_str()),
            _ => None,
        })
        .collect();
    let mode = resolve_mode(grammar, &present).map(str::to_string);

    let mut items = Vec::new();
    let mut untranslatable = Vec::new();
    for arg in &parsed.args {
        match &arg.role {
            ArgRole::Flag {
                canonical,
                value,
                role,
                ..
            } => {
                let operation = grammar
                    .flags
                    .iter()
                    .find(|f| f.canonical == canonical.as_str())
                    .map(|f| f.operation)
                    .unwrap_or(Operation::Set);
                items.push(IrItem::Semantic {
                    role: role.clone(),
                    operation,
                    value: value.clone(),
                    source_token: arg.spelling.clone(),
                });
            }
            ArgRole::Positional { value, .. } => {
                items.push(IrItem::Positional {
                    value: value.clone(),
                });
            }
            ArgRole::Unknown => untranslatable.push(Untranslatable {
                token: arg.spelling.clone(),
            }),
        }
    }
    CommandIr {
        items,
        mode,
        untranslatable,
    }
}

/// Emit a semantic IR into a target grammar **holistically**: each semantic is
/// realized through whichever of the target's mechanisms expresses it — a flag
/// (matched by `(role, operation)` and legal in the command's mode), or, failing
/// that, an env channel (`cc -L dir` → MSVC `LIB=dir`). Positionals carry
/// through by position. A semantic the target expresses through neither is
/// surfaced in `untranslatable`.
pub fn emit_into(ir: &CommandIr, to: &Grammar) -> TranslatedCommand {
    let mode = ir.mode.as_deref();
    let mut args = Vec::new();
    let mut env_acc: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut untranslatable = ir.untranslatable.clone();

    for item in &ir.items {
        match item {
            IrItem::Semantic {
                role,
                operation,
                value,
                source_token,
            } => {
                let mut emitted = false;
                if !role.is_empty() {
                    // Prefer a flag legal in this mode; else an env channel.
                    if let Some(flag) = to.flags.iter().find(|t| {
                        t.role == role.as_str()
                            && t.operation == *operation
                            && (t.modes.is_empty() || mode.is_some_and(|m| t.modes.contains(&m)))
                    }) {
                        args.extend(emit(flag, value.as_deref()));
                        emitted = true;
                    } else if let Some(channel) = to
                        .env
                        .iter()
                        .find(|e| e.role == role.as_str() && e.operation == *operation)
                    {
                        env_acc
                            .entry(channel.name)
                            .or_default()
                            .push(value.clone().unwrap_or_default());
                        emitted = true;
                    }
                }
                if !emitted {
                    untranslatable.push(Untranslatable {
                        token: source_token.clone(),
                    });
                }
            }
            IrItem::Positional { value } => args.push(value.clone()),
        }
    }

    let sep = ENV_PATH_SEP.to_string();
    let env = env_acc
        .into_iter()
        .map(|(name, vals)| (name.to_string(), vals.join(&sep)))
        .collect();
    TranslatedCommand {
        args,
        env,
        untranslatable,
    }
}

/// Translate a command from one tool's grammar to another's by lowering it to
/// the semantic IR ([`lift`]) and re-emitting it holistically ([`emit_into`]).
/// Spelling, mechanism (flag vs env), and mode-split targets are all resolved by
/// the target grammar: `gcc -I inc` becomes `/Iinc`, `gcc -L dir` becomes the
/// `LIB` env channel, `gcc -o` in compile mode becomes `/Fo` (not `/Fe`). Gaps
/// the target cannot express land in [`TranslatedCommand::untranslatable`].
pub fn translate(from: &Grammar, argv: &[String], to: &Grammar) -> TranslatedCommand {
    emit_into(&lift(from, argv), to)
}
