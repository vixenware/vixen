//! Tests for `vx build --json` NDJSON output
//!
//! Verifies that:
//! - JSON mode emits valid NDJSON (one JSON object per line)
//! - Events include build_started and build_finished
//! - Exit codes follow the spec: 0=success, 1=build error

mod harness;
use harness::{TestEnv, create_hello_world};

/// Parse stdout lines, keeping only non-empty ones
fn json_lines(output: &harness::VxOutput) -> Vec<&str> {
    output
        .stdout
        .lines()
        .filter(|line| !line.is_empty())
        .collect()
}

/// Extract a u64 value for a given key from a JSON line.
///
/// Looks for `"key":NUMBER` and parses the number.
fn json_u64(line: &str, key: &str) -> u64 {
    let needle = format!("\"{}\":", key);
    let start = line
        .find(&needle)
        .unwrap_or_else(|| panic!("key {key:?} not found in: {line}"))
        + needle.len();
    let rest = &line[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end]
        .parse()
        .unwrap_or_else(|e| panic!("failed to parse {key:?} from {rest:?}: {e}"))
}

/// Find the build_finished event line from JSON output.
fn build_finished_line<'a>(lines: &[&'a str]) -> &'a str {
    lines
        .iter()
        .rfind(|l| l.contains("\"event\":\"build_finished\""))
        .expect("no build_finished event in output")
}

#[test]
fn json_output_success() {
    let env = TestEnv::new();
    create_hello_world(&env);

    let result = env.build().json().run();
    assert_eq!(result.exit_code, Some(0), "expected exit code 0 on success");

    let lines = json_lines(&result);
    assert!(
        !lines.is_empty(),
        "expected at least one JSON line on stdout"
    );

    // Every line must be valid JSON
    for (i, line) in lines.iter().enumerate() {
        assert!(
            line.starts_with('{') && line.ends_with('}'),
            "line {} is not a JSON object: {}",
            i,
            line
        );
    }

    // First event should be build_started
    assert!(
        lines[0].contains("\"event\":\"build_started\""),
        "first event should be build_started, got: {}",
        lines[0]
    );
    assert!(
        lines[0].contains("\"total_actions\""),
        "build_started should contain total_actions"
    );

    // Last event should be build_finished with success
    let last = lines.last().unwrap();
    assert!(
        last.contains("\"event\":\"build_finished\""),
        "last event should be build_finished, got: {last}"
    );
    assert!(
        last.contains("\"success\":true"),
        "build_finished should indicate success"
    );
    assert!(
        last.contains("\"duration_ms\""),
        "build_finished should contain duration_ms"
    );
    assert!(
        last.contains("\"total_actions\""),
        "build_finished should contain total_actions"
    );
    assert!(
        last.contains("\"cache_hits\""),
        "build_finished should contain cache_hits"
    );
}

#[test]
fn json_output_cache_hit() {
    let env = TestEnv::new();
    create_hello_world(&env);

    // First build
    let result1 = env.build().json().run();
    assert_eq!(result1.exit_code, Some(0));

    // Second build — nothing changed, must be a full cache hit
    let result2 = env.build().json().run();
    assert_eq!(result2.exit_code, Some(0));

    let lines = json_lines(&result2);
    let finished = build_finished_line(&lines);

    assert!(
        finished.contains("\"cached\":true"),
        "second build should report cached:true, got: {finished}"
    );

    let total_actions = json_u64(finished, "total_actions");
    let cache_hits = json_u64(finished, "cache_hits");
    let rebuilt = json_u64(finished, "rebuilt");

    assert!(
        total_actions > 0,
        "total_actions must be nonzero, got: {finished}"
    );
    assert_eq!(
        cache_hits, total_actions,
        "every action must be a cache hit: cache_hits={cache_hits}, total_actions={total_actions}"
    );
    assert_eq!(
        rebuilt, 0,
        "zero actions should be rebuilt on a no-op second build, got rebuilt={rebuilt}"
    );
}

#[test]
fn json_output_build_error() {
    let env = TestEnv::new();

    env.write_file(
        "Cargo.toml",
        r#"[package]
name = "broken"
version = "0.1.0"
edition = "2021"
"#,
    );
    env.write_file("src/main.rs", "fn main() { SYNTAX ERROR }\n");

    let result = env.build().json().run();
    assert_eq!(
        result.exit_code,
        Some(1),
        "expected exit code 1 on build error"
    );

    let lines = json_lines(&result);
    let last = lines.last().expect("expected build_finished event");
    assert!(
        last.contains("\"event\":\"build_finished\""),
        "last event should be build_finished"
    );
    assert!(
        last.contains("\"success\":false"),
        "build_finished should indicate failure"
    );
    assert!(
        last.contains("\"error\""),
        "build_finished should contain error field"
    );
}

#[test]
fn json_output_has_action_events() {
    let env = TestEnv::new();
    create_hello_world(&env);

    let result = env.build().json().run();
    result.assert_success();

    let lines = json_lines(&result);

    let has_action_started = lines
        .iter()
        .any(|l| l.contains("\"event\":\"action_started\""));
    let has_action_completed = lines
        .iter()
        .any(|l| l.contains("\"event\":\"action_completed\""));

    assert!(
        has_action_started,
        "expected at least one action_started event"
    );
    assert!(
        has_action_completed,
        "expected at least one action_completed event"
    );
}

#[test]
fn json_output_is_valid_ndjson() {
    let env = TestEnv::new();
    create_hello_world(&env);

    let result = env.build().json().run();
    result.assert_success();

    // stdout must contain only JSON objects, no TUI control sequences
    for (i, line) in result.stdout.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        assert!(
            line.starts_with('{'),
            "line {i} does not start with '{{': {line}"
        );
        assert!(
            line.ends_with('}'),
            "line {i} does not end with '}}': {line}"
        );
        // Must not contain ANSI escape sequences
        assert!(
            !line.contains("\x1b["),
            "line {i} contains ANSI escape sequence: {line}"
        );
    }
}
