//! `command-model` — describe a CLI tool's argument grammar, parse a raw argv
//! into structure, and reduce it to a **canonical** form. That canonical form
//! is the basis for content-addressed command caching, command linting, and
//! structured provenance.
//!
//! The crate is deliberately standalone: data in, data out, no dependencies.
//!
//! ## Status: first vertical slice
//!
//! This implements the `cc` compile invocation exercised by
//! `old-zoo/libpng.vx`. The structural pillars from the CLI-grammar study —
//! guarded productions, command-level reducers + a coupling graph, and the full
//! effective-command closure (env, expanded response/config/lock files,
//! discovered inputs) — are **not here yet**; see
//! `docs/design/command-model.md`. What *is* here is real — it parses,
//! canonicalizes, and lints a flat flag grammar, so two spellings of the same
//! compile collapse to one identity.

use std::collections::BTreeMap;
use std::fmt;

/// How a flag and its value are written on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spelling {
    /// `-c`, `-fPIC` — the whole token is the flag; no value.
    Bare,
    /// `-O2`, `-DFOO`, `-lm` — value glued directly after the lead.
    Attached,
    /// `-o out`, `-I dir` — value is the following argv token.
    Separated,
    /// `-std=c99` — value after `=`.
    Equals,
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
    /// duplicates are dropped.
    DedupOrdered,
    /// `-D`/`-U`: an ordered define/undef program. Slice handles `-D` only;
    /// `-U` interleaving is future work (the study's `OrderedDefineProgram`).
    DefineProgram,
    /// Positional input: never reordered, never deduped.
    Positional,
}

/// What a value slot *is* — drives sandbox mounting and cache-key treatment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Literal text — hashed as text, never mounted.
    Str,
    /// Input path — mounted read-only, content-hashed into the key.
    PathIn { dir: bool },
    /// Output path — the writable target; not an input hash.
    PathOut,
    /// Header search directory.
    Include,
    /// `NAME` or `NAME=VALUE` preprocessor define.
    Define,
}

/// A single flag in a tool grammar.
#[derive(Debug, Clone, Copy)]
pub struct Flag {
    /// Canonical lead spelling, e.g. `-O`, `-D`, `-I`, `-o`, `-c`.
    pub canonical: &'static str,
    /// Accepted alternate spellings that canonicalize to `canonical`.
    pub aliases: &'static [&'static str],
    pub spelling: Spelling,
    pub policy: Policy,
    pub kind: Kind,
}

/// A flat tool grammar: a set of flags. Anything not matching a flag is a
/// positional input. (The recursive, guarded-production form is future work.)
#[derive(Debug, Clone, Copy)]
pub struct Grammar {
    pub flags: &'static [Flag],
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
    /// and `lead=` (`Equals`) matches win over the longest `Attached` prefix.
    fn match_token(&self, token: &str) -> Option<(usize, Option<String>, bool)> {
        let mut best_attached: Option<(usize, usize)> = None;
        for (i, flag) in self.flags.iter().enumerate() {
            for lead in std::iter::once(flag.canonical).chain(flag.aliases.iter().copied()) {
                match flag.spelling {
                    Spelling::Bare if token == lead => return Some((i, None, false)),
                    Spelling::Separated if token == lead => return Some((i, None, true)),
                    Spelling::Equals => {
                        if let Some(value) =
                            token.strip_prefix(lead).and_then(|r| r.strip_prefix('='))
                        {
                            return Some((i, Some(value.to_string()), false));
                        }
                    }
                    Spelling::Attached
                        if token.len() > lead.len()
                            && token.starts_with(lead)
                            && best_attached.is_none_or(|(_, l)| lead.len() > l) =>
                    {
                        best_attached = Some((i, lead.len()));
                    }
                    _ => {}
                }
            }
        }
        best_attached.map(|(i, lead_len)| (i, Some(token[lead_len..].to_string()), false))
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

/// A command reduced to its canonical form, plus everything the canonicalizer
/// noticed on the way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalCommand {
    pub program: String,
    pub args: Vec<String>,
    pub lints: Vec<Lint>,
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
    /// The same define was given twice, identically.
    DuplicateDefine { name: String },
    /// The same define name was given two different values.
    ConflictingDefine {
        name: String,
        first: String,
        second: String,
    },
    /// The same include dir was listed more than once.
    DuplicateInclude { dir: String },
    /// A `-`-leading token matched no flag in the grammar.
    UnknownFlag { token: String },
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
            Lint::DuplicateDefine { name } => write!(f, "duplicate define `{name}`"),
            Lint::ConflictingDefine {
                name,
                first,
                second,
            } => {
                write!(f, "conflicting define `{name}`: `{first}` then `{second}`")
            }
            Lint::DuplicateInclude { dir } => write!(f, "duplicate include dir `{dir}`"),
            Lint::UnknownFlag { token } => write!(f, "unknown flag `{token}` (not in grammar)"),
        }
    }
}

fn emit(flag: &Flag, value: Option<&str>) -> Vec<String> {
    match flag.spelling {
        Spelling::Bare => vec![flag.canonical.to_string()],
        Spelling::Attached => vec![format!("{}{}", flag.canonical, value.unwrap_or(""))],
        Spelling::Separated => {
            vec![flag.canonical.to_string(), value.unwrap_or("").to_string()]
        }
        Spelling::Equals => vec![format!("{}={}", flag.canonical, value.unwrap_or(""))],
    }
}

fn emit_one(flag: &Flag, value: Option<&str>) -> String {
    emit(flag, value).join(" ")
}

fn reduce_flag(flag: &Flag, occs: &[&Occurrence], lints: &mut Vec<Lint>) -> Vec<String> {
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
                    lints.push(Lint::DuplicateInclude { dir: value });
                } else {
                    out.extend(emit(flag, Some(&value)));
                    seen.push(value);
                }
            }
            out
        }
        Policy::DefineProgram => {
            // Slice: -D only. Last value per name wins; emit sorted by name.
            let mut map: BTreeMap<String, Option<String>> = BTreeMap::new();
            for o in occs {
                let raw = o.value.clone().unwrap_or_default();
                let (name, value) = match raw.split_once('=') {
                    Some((n, v)) => (n.to_string(), Some(v.to_string())),
                    None => (raw, None),
                };
                match map.get(&name) {
                    Some(prev) if *prev == value => {
                        lints.push(Lint::DuplicateDefine { name: name.clone() })
                    }
                    Some(prev) => lints.push(Lint::ConflictingDefine {
                        name: name.clone(),
                        first: prev.clone().unwrap_or_default(),
                        second: value.clone().unwrap_or_default(),
                    }),
                    None => {}
                }
                map.insert(name, value);
            }
            let mut out = Vec::new();
            for (name, value) in map {
                let payload = match value {
                    Some(v) => format!("{name}={v}"),
                    None => name,
                };
                out.extend(emit(flag, Some(&payload)));
            }
            out
        }
        Policy::Positional => Vec::new(),
    }
}

/// Parse `argv` against `grammar` and reduce it to its canonical form.
///
/// `argv[0]` is the program and is preserved verbatim; the rest is
/// canonicalized. Emission order is fixed (flags in grammar-declaration order,
/// then positional inputs, then outputs), so two argvs that mean the same thing
/// produce the same `args` — the property a content-addressed cache key needs.
pub fn canonicalize(grammar: &Grammar, argv: &[String]) -> CanonicalCommand {
    let program = argv.first().cloned().unwrap_or_default();
    let args = argv.get(1..).unwrap_or(&[]);
    let parsed = grammar.parse(args);
    let mut lints = Vec::new();

    let mut by_flag: BTreeMap<usize, Vec<&Occurrence>> = BTreeMap::new();
    for occ in &parsed.occurrences {
        by_flag.entry(occ.flag).or_default().push(occ);
    }

    let mut flag_tokens: Vec<String> = Vec::new();
    let mut output_tokens: Vec<String> = Vec::new();
    for (idx, flag) in grammar.flags.iter().enumerate() {
        let Some(occs) = by_flag.get(&idx) else {
            continue;
        };
        let emitted = reduce_flag(flag, occs, &mut lints);
        if matches!(flag.kind, Kind::PathOut) {
            output_tokens.extend(emitted);
        } else {
            flag_tokens.extend(emitted);
        }
    }

    let mut out = flag_tokens;
    out.extend(parsed.positionals.iter().cloned());
    out.extend(output_tokens);
    for token in &parsed.unknown {
        lints.push(Lint::UnknownFlag {
            token: token.clone(),
        });
        out.push(token.clone());
    }

    CanonicalCommand {
        program,
        args: out,
        lints,
    }
}

/// The `cc`/clang compile-invocation grammar — the slice the `libpng` build
/// exercises. Gcc-style; MSVC and the full flag surface are future work.
pub const CC: Grammar = Grammar {
    flags: &[
        Flag {
            canonical: "-c",
            aliases: &[],
            spelling: Spelling::Bare,
            policy: Policy::Once,
            kind: Kind::Str,
        },
        Flag {
            canonical: "-fPIC",
            aliases: &[],
            spelling: Spelling::Bare,
            policy: Policy::Once,
            kind: Kind::Str,
        },
        Flag {
            canonical: "-O",
            aliases: &[],
            spelling: Spelling::Attached,
            policy: Policy::Group("opt"),
            kind: Kind::Str,
        },
        Flag {
            canonical: "-std",
            aliases: &[],
            spelling: Spelling::Equals,
            policy: Policy::Once,
            kind: Kind::Str,
        },
        Flag {
            canonical: "-D",
            aliases: &[],
            spelling: Spelling::Attached,
            policy: Policy::DefineProgram,
            kind: Kind::Define,
        },
        Flag {
            canonical: "-I",
            aliases: &[],
            spelling: Spelling::Separated,
            policy: Policy::DedupOrdered,
            kind: Kind::Include,
        },
        Flag {
            canonical: "-o",
            aliases: &[],
            spelling: Spelling::Separated,
            policy: Policy::Once,
            kind: Kind::PathOut,
        },
    ],
};
