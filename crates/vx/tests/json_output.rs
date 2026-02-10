//! Tests for `vx build --json` NDJSON output
//!
//! Verifies that:
//! - JSON mode emits valid NDJSON (one JSON object per line)
//! - Events include build_started and build_finished
//! - Exit codes follow the spec: 0=success, 1=build error

#[allow(dead_code)]
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

    // Second build — should be cached
    let result2 = env.build().json().run();
    assert_eq!(result2.exit_code, Some(0));

    let lines = json_lines(&result2);
    let last = lines.last().expect("expected build_finished event");
    assert!(
        last.contains("\"event\":\"build_finished\""),
        "last event should be build_finished"
    );
    assert!(
        last.contains("\"cached\":true"),
        "second build should report cached:true, got: {last}"
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
