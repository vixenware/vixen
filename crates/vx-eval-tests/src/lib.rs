use std::collections::HashMap;

use facet::Facet;
use thiserror::Error;

pub mod eval;

/// Expected result value, tagged in styx as @u64"42", @string"hello", @bool"true"
#[derive(Debug, Facet, PartialEq)]
#[repr(u8)]
#[facet(rename_all = "kebab-case")]
pub enum ExpectedValue {
    U64(u64),
    Bool(bool),
    String(String),
}

/// Trace assertions — which functions were / weren't entered
#[derive(Debug, Default, Facet)]
pub struct TraceAssertions {
    pub entered: Option<Vec<String>>,
    pub not_entered: Option<Vec<String>>,
    pub entered_once: Option<Vec<String>>,
}

/// A test fixture parsed from a .styx file
#[derive(Debug, Facet)]
pub struct TestFixture {
    pub source: String,
    pub entry: String,
    pub expect: ExpectedValue,
    pub trace: Option<TraceAssertions>,
}

/// Actual result from the evaluator
#[derive(Debug, PartialEq)]
pub enum EvalResult {
    U64(u64),
    Bool(bool),
    String(String),
}

/// Trace recorded during evaluation
#[derive(Debug, Default)]
pub struct EvalTrace {
    /// How many times each function was entered
    pub enter_counts: HashMap<String, usize>,
}

impl EvalTrace {
    pub fn record_enter(&mut self, name: &str) {
        *self.enter_counts.entry(name.to_owned()).or_default() += 1;
    }

    pub fn was_entered(&self, name: &str) -> bool {
        self.enter_counts.get(name).copied().unwrap_or(0) > 0
    }

    pub fn enter_count(&self, name: &str) -> usize {
        self.enter_counts.get(name).copied().unwrap_or(0)
    }
}

#[derive(Debug, Error)]
pub enum FixtureError {
    #[error("parse error: {0}")]
    Parse(String),
    #[error("eval error: {0}")]
    Eval(String),
    #[error("expected {expected:?}, got {actual:?}")]
    ValueMismatch {
        expected: ExpectedValue,
        actual: EvalResult,
    },
    #[error("trace assertion failed: {0}")]
    TraceFailed(String),
}

/// Parse a .styx fixture file
pub fn parse_fixture(content: &str) -> Result<TestFixture, FixtureError> {
    facet_styx::from_str(content).map_err(|e| FixtureError::Parse(e.to_string()))
}

/// Run a parsed fixture: evaluate and check assertions
pub fn run_fixture(fixture: &TestFixture) -> Result<(), FixtureError> {
    let (result, trace) =
        eval::evaluate(&fixture.source, &fixture.entry).map_err(FixtureError::Eval)?;

    // Check expected value
    let matches = match (&fixture.expect, &result) {
        (ExpectedValue::U64(e), EvalResult::U64(a)) => e == a,
        (ExpectedValue::Bool(e), EvalResult::Bool(a)) => e == a,
        (ExpectedValue::String(e), EvalResult::String(a)) => e == a,
        _ => false,
    };
    if !matches {
        return Err(FixtureError::ValueMismatch {
            expected: fixture.expect.clone(),
            actual: result,
        });
    }

    // Check trace assertions
    if let Some(assertions) = &fixture.trace {
        if let Some(entered) = &assertions.entered {
            for name in entered {
                if !trace.was_entered(name) {
                    return Err(FixtureError::TraceFailed(format!(
                        "expected `{name}` to be entered, but it wasn't"
                    )));
                }
            }
        }
        if let Some(not_entered) = &assertions.not_entered {
            for name in not_entered {
                if trace.was_entered(name) {
                    return Err(FixtureError::TraceFailed(format!(
                        "expected `{name}` NOT to be entered, but it was (entered {} time(s))",
                        trace.enter_count(name)
                    )));
                }
            }
        }
        if let Some(entered_once) = &assertions.entered_once {
            for name in entered_once {
                let count = trace.enter_count(name);
                if count != 1 {
                    return Err(FixtureError::TraceFailed(format!(
                        "expected `{name}` to be entered exactly once, but it was entered {count} time(s)"
                    )));
                }
            }
        }
    }

    Ok(())
}

// Need PartialEq clone for error reporting
impl Clone for ExpectedValue {
    fn clone(&self) -> Self {
        match self {
            Self::U64(v) => Self::U64(*v),
            Self::Bool(v) => Self::Bool(*v),
            Self::String(v) => Self::String(v.clone()),
        }
    }
}
