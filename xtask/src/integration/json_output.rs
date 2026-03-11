use super::ScenarioFuture;
use super::context::{BuildOptions, CommandOutput, IntegrationContext};
use super::scaffold::create_hello_world;

fn json_lines(output: &CommandOutput) -> Vec<&str> {
    output
        .stdout
        .lines()
        .filter(|line| !line.is_empty())
        .collect()
}

fn json_u64(line: &str, key: &str) -> u64 {
    let needle = format!("\"{key}\":");
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
        .unwrap_or_else(|error| panic!("failed to parse {key:?} from {rest:?}: {error}"))
}

fn build_finished_line<'a>(lines: &[&'a str]) -> &'a str {
    lines
        .iter()
        .rfind(|line| line.contains("\"event\":\"build_finished\""))
        .expect("no build_finished event in output")
}

pub(crate) fn json_output_success(context: &mut IntegrationContext) -> ScenarioFuture<'_> {
    Box::pin(async move {
        create_hello_world(context)?;

        let result = context.run_vx_build(BuildOptions::default().json()).await?;
        result.assert_success("vx build --json")?;

        let lines = json_lines(&result);
        assert!(
            !lines.is_empty(),
            "expected at least one JSON line on stdout"
        );

        for (index, line) in lines.iter().enumerate() {
            assert!(
                line.starts_with('{') && line.ends_with('}'),
                "line {} is not a JSON object: {}",
                index,
                line
            );
        }

        assert!(
            lines[0].contains("\"event\":\"build_started\""),
            "first event should be build_started, got: {}",
            lines[0]
        );
        assert!(
            lines[0].contains("\"total_actions\""),
            "build_started should contain total_actions"
        );

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

        Ok(())
    })
}

pub(crate) fn json_output_cache_hit(context: &mut IntegrationContext) -> ScenarioFuture<'_> {
    Box::pin(async move {
        create_hello_world(context)?;

        context
            .run_vx_build(BuildOptions::default().json())
            .await?
            .assert_success("vx build --json")?;

        let result = context.run_vx_build(BuildOptions::default().json()).await?;
        result.assert_success("vx build --json")?;

        let lines = json_lines(&result);
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

        Ok(())
    })
}

pub(crate) fn json_output_build_error(context: &mut IntegrationContext) -> ScenarioFuture<'_> {
    Box::pin(async move {
        context.write_project_file(
            "Cargo.toml",
            r#"[package]
name = "broken"
version = "0.1.0"
edition = "2021"
"#,
        )?;
        context.write_project_file("src/main.rs", "fn main() { SYNTAX ERROR }\n")?;

        let result = context.run_vx_build(BuildOptions::default().json()).await?;
        result.assert_failure("vx build --json")?;

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

        Ok(())
    })
}

pub(crate) fn json_output_has_action_events(
    context: &mut IntegrationContext,
) -> ScenarioFuture<'_> {
    Box::pin(async move {
        create_hello_world(context)?;

        let result = context.run_vx_build(BuildOptions::default().json()).await?;
        result.assert_success("vx build --json")?;

        let lines = json_lines(&result);
        let has_action_started = lines
            .iter()
            .any(|line| line.contains("\"event\":\"action_started\""));
        let has_action_completed = lines
            .iter()
            .any(|line| line.contains("\"event\":\"action_completed\""));

        assert!(
            has_action_started,
            "expected at least one action_started event"
        );
        assert!(
            has_action_completed,
            "expected at least one action_completed event"
        );

        Ok(())
    })
}

pub(crate) fn json_output_is_valid_ndjson(context: &mut IntegrationContext) -> ScenarioFuture<'_> {
    Box::pin(async move {
        create_hello_world(context)?;

        let result = context.run_vx_build(BuildOptions::default().json()).await?;
        result.assert_success("vx build --json")?;

        for (index, line) in result.stdout.lines().enumerate() {
            if line.is_empty() {
                continue;
            }
            assert!(
                line.starts_with('{'),
                "line {index} does not start with '{{': {line}"
            );
            assert!(
                line.ends_with('}'),
                "line {index} does not end with '}}': {line}"
            );
            assert!(
                !line.contains("\x1b["),
                "line {index} contains ANSI escape sequence: {line}"
            );
        }

        Ok(())
    })
}
