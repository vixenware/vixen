//! The runtime `Command` value — what a CLI literal (`@[ ${cmd} args… ]`) evaluates
//! to and `exec` consumes.
//!
//! `command-model` is **pure projection** (grammar + argv → classified I/O +
//! canonical form; no I/O, no hashing). This wraps its [`EffectiveCommand`] with the
//! two things the runtime also needs:
//!
//! - the literal **execution argv** — what actually runs (vs. the cache key's
//!   compat-normalized form), and
//! - the **outputs** — the dual of `inputs`, which `EffectiveCommand` omits (the
//!   cache key is over inputs); derived here from the parsed args' `Kind::Output`
//!   tokens.
//!
//! Identity (resolving input *paths* → content hashes) and transfer stay the
//! runtime's job; this is just the classified value, ready to dispatch.

use std::collections::BTreeMap;

use command_model::{ArgRole, EffectiveCommand, GrammarSet, Kind, parse_command};

/// A command ready to dispatch: command-model's [`EffectiveCommand`] (program, the
/// compat-normalized argv the cache key is built from, the resolved env, the
/// classified `inputs`, impurities, lints) plus the literal [`execution_argv`] that
/// runs and the grammar-classified [`outputs`].
///
/// [`execution_argv`]: Command::execution_argv
/// [`outputs`]: Command::outputs
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// The cache identity + classified inputs (command-model's projection).
    pub effective: EffectiveCommand,
    /// The literal args (holes substituted) — what the executor runs.
    pub execution_argv: Vec<String>,
    /// Output targets, classified by the grammar (`Kind::Output`) — the dual of
    /// `effective.inputs`. The sandbox's writable roots fall out of these.
    pub outputs: Vec<Output>,
}

/// An output target a command writes, tagged with its grammar role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub role: String,
    pub path: String,
}

/// Project a resolved command (`argv[0]` is the program) against the named grammar
/// into the runtime [`Command`]. `None` if the grammar isn't in the set.
/// command-model classifies; we add the execution argv and derive the outputs.
pub fn project(
    grammars: &GrammarSet,
    grammar: &str,
    argv: &[String],
    env: &BTreeMap<String, String>,
) -> Option<Command> {
    let g = grammars.get(grammar)?;
    let effective = grammars.effective_command(grammar, argv, env);
    // Outputs aren't in EffectiveCommand (the cache key is over inputs), so walk the
    // parsed args for `Kind::Output` tokens. `parse_command` takes the args after
    // the program (`argv[1..]`), matching `effective_command`'s own pass.
    let parsed = parse_command(g, argv.get(1..).unwrap_or(&[]));
    let outputs = parsed
        .args
        .iter()
        .filter_map(|a| match &a.role {
            ArgRole::Flag {
                kind: Kind::Output { .. },
                value: Some(path),
                role,
                ..
            } => Some(Output {
                role: role.clone(),
                path: path.clone(),
            }),
            ArgRole::Positional {
                kind: Kind::Output { .. },
                value,
                name,
            } => Some(Output {
                role: name.clone(),
                path: value.clone(),
            }),
            _ => None,
        })
        .collect();
    Some(Command {
        effective,
        execution_argv: argv.to_vec(),
        outputs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use command_model::examples;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|s| s.to_string()).collect()
    }

    /// Projecting a `cc` compile: command-model classifies the inputs (the `-I` dir
    /// and the source); we derive the output from the `-o` token's `Kind::Output`;
    /// and the output path never leaks into the inputs (outputs aren't input hashes).
    #[test]
    fn projects_a_cc_command_with_inputs_and_outputs() {
        let av = argv(&["cc", "-c", "-I", "src", "src/x.c", "-o", "x.o"]);
        let cmd = project(&examples(), "cc", &av, &env(&[])).expect("cc grammar present");

        assert!(
            cmd.effective.inputs.iter().any(|i| i.path == "src/x.c"),
            "the source is an input: {:?}",
            cmd.effective.inputs
        );
        assert!(
            cmd.effective.inputs.iter().any(|i| i.path == "src"),
            "the -I dir is an input"
        );
        assert!(
            cmd.outputs.iter().any(|o| o.path == "x.o"),
            "the -o target is an output: {:?}",
            cmd.outputs
        );
        assert!(
            !cmd.effective.inputs.iter().any(|i| i.path == "x.o"),
            "an output path is not an input"
        );
        assert_eq!(
            cmd.execution_argv, av,
            "execution argv is the literal command"
        );
    }

    /// An unknown grammar projects to `None`.
    #[test]
    fn unknown_grammar_is_none() {
        assert!(project(&examples(), "nope", &argv(&["nope"]), &env(&[])).is_none());
    }
}
