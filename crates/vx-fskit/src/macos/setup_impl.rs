use camino::Utf8PathBuf;
use moire::process::Command;

use super::probe::{self, StatusProbe};
use super::{FsKitResult, Log, VIXEN_APP_PATH, VIXEN_EXTENSION_IDENTIFIER, mount_path_candidates};

const RESTART_FSKIT_COMMAND: &str = "sudo killall fskitd fskit_agent";
const RESTART_EXTENSIONKIT_COMMAND: &str = "killall extensionkitservice";
const RELAUNCH_VIXEN_COMMAND: &str = "open /Applications/Vixen.app";
const INSTALLED_APPEX_PATH: &str = "/Applications/Vixen.app/Contents/Extensions/VixenFS.appex";

#[derive(Debug)]
struct MountFailureDiagnosis {
    summary: String,
    remediation: Vec<String>,
    extra_log_lines: Vec<String>,
}

/// Mount a VixenFS filesystem.
///
/// Checks prerequisites (app installed, codesign, extension registered+enabled),
/// cleans up broken mounts, mounts, and verifies via status.json.
pub async fn setup(mount_path: &Utf8PathBuf, debug: bool) -> FsKitResult {
    let mut log = Log::new();
    let candidates = mount_path_candidates(mount_path).await;

    // --- Prerequisites ---

    // Check extension first — if it's registered and enabled, the app is
    // necessarily installed and properly signed, so we can skip those checks.
    let extension_ok = match Command::new("pluginkit")
        .args(["-m", "-p", "com.apple.fskit.fsmodule"])
        .output()
        .await
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            // Look for "+    eu.bearcove.vixenfs" — the + means enabled
            let found = stdout.lines().any(|line| {
                line.contains(VIXEN_EXTENSION_IDENTIFIER) && line.trim_start().starts_with('+')
            });
            if found {
                log.ok("FSKit extension registered and enabled");
                true
            } else {
                let registered = stdout
                    .lines()
                    .any(|line| line.contains(VIXEN_EXTENSION_IDENTIFIER));
                if registered {
                    log.fail("pluginkit: extension registered but NOT enabled");
                    return FsKitResult::failure(
                        log.into_entries(),
                        "VixenFS extension is registered but not enabled",
                        vec!["run `vx doctor` to enable the extension".to_string()],
                    );
                }
                false
            }
        }
        Err(e) => {
            log.fail(format!("pluginkit: failed to run: {e}"));
            false
        }
    };

    // Extension not registered — dig deeper to give a useful diagnostic.
    if !extension_ok {
        if moire::fs::metadata(VIXEN_APP_PATH).await.is_err() {
            log.fail(format!("{VIXEN_APP_PATH} not found"));
            return FsKitResult::failure(
                log.into_entries(),
                "Vixen.app is not installed",
                vec!["brew install --cask bearcove/tap/vixen".to_string()],
            );
        }
        log.ok("Vixen.app installed");

        // App exists but extension not registered — codesign might be broken
        match Command::new("codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(VIXEN_APP_PATH)
            .output()
            .await
        {
            Ok(output) if output.status.success() => {
                log.ok("codesign valid");
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                log.fail(format!("codesign: FAILED — {}", stderr.trim()));
                return FsKitResult::failure(
                    log.into_entries(),
                    "Vixen.app codesign is invalid",
                    vec!["brew reinstall --cask bearcove/tap/vixen".to_string()],
                );
            }
            Err(e) => {
                log.fail(format!("codesign: failed to run: {e}"));
            }
        }

        // App installed, codesign ok, but extension not registered
        log.fail("pluginkit: extension not registered");
        return FsKitResult::failure(
            log.into_entries(),
            "VixenFS extension is not registered",
            vec!["run `vx doctor` to launch Vixen.app and register the extension".to_string()],
        );
    }

    // --- Probe mount point ---

    let probe_result = probe::probe(mount_path, &candidates, &mut log, true).await;

    if probe_result.is_healthy() {
        log.ok("mount is already healthy");
        return FsKitResult::success(log.into_entries());
    }

    if probe_result.is_broken_mount() {
        log.info("broken mount detected, running teardown first");
        let teardown_result = super::teardown_impl::teardown(mount_path).await;
        // Merge teardown logs
        for entry in teardown_result.log {
            log.entries.push(entry);
        }
        if let Some(err) = teardown_result.error {
            log.fail(format!("teardown failed: {}", err.summary));
            return FsKitResult::failure(log.into_entries(), err.summary, err.remediation);
        }
    }

    // --- Mount ---

    // Create mount point directory
    if let Err(e) = moire::fs::create_dir_all(mount_path.as_std_path()).await {
        log.fail(format!("mkdir -p {mount_path}: {e}"));
        return FsKitResult::failure(
            log.into_entries(),
            format!("failed to create mount point: {e}"),
            vec![],
        );
    }
    // Build the source URL
    let mut source = "vixen://session".to_string();
    let mut query_items = Vec::new();
    if debug {
        query_items.push("debug=1".to_string());
    }
    if let Ok(addr) = std::env::var("moire_DASHBOARD") {
        let trimmed = addr.trim();
        if !trimmed.is_empty() {
            query_items.push(format!("moire_dashboard={trimmed}"));
        }
    }
    if !query_items.is_empty() {
        source.push('?');
        source.push_str(&query_items.join("&"));
    }

    let mount_output = match Command::new("/sbin/mount")
        .args(["-F", "-t", "vixenfs", &source, mount_path.as_str()])
        .output()
        .await
    {
        Ok(output) => output,
        Err(e) => {
            log.fail(format!("/sbin/mount: failed to run: {e}"));
            return FsKitResult::failure(
                log.into_entries(),
                format!("failed to run /sbin/mount: {e}"),
                vec!["sudo killall fskitd fskit_agent".to_string()],
            );
        }
    };

    if !mount_output.status.success() {
        let stderr = String::from_utf8_lossy(&mount_output.stderr);
        let exit_code = mount_output.status.code();
        let system_log_excerpt = if exit_code == Some(69) {
            recent_extensionkit_log_excerpt().await
        } else {
            None
        };
        let registration_lines = if exit_code == Some(69) {
            registration_debug_lines().await
        } else {
            Vec::new()
        };
        let diagnosis = diagnose_mount_failure(
            exit_code,
            &stderr,
            system_log_excerpt.as_deref(),
            &registration_lines,
        );

        log.fail(format!(
            "mount failed (exit code {})",
            exit_code.map_or("unknown".to_string(), |c| c.to_string())
        ));
        if !stderr.trim().is_empty() {
            for line in stderr.trim().lines() {
                log.info(format!("  {line}"));
            }
        }
        for line in &diagnosis.extra_log_lines {
            log.info(format!("  system log: {line}"));
        }

        return FsKitResult::failure(log.into_entries(), diagnosis.summary, diagnosis.remediation);
    }

    // --- Verify via status.json ---

    // Retry a few times — the extension needs time to start up and connect.
    for attempt in 1..=5 {
        log.info(format!("status check attempt {attempt}/5"));
        let status = probe::probe(mount_path, &candidates, &mut log, true).await;
        match &status.status {
            StatusProbe::Ok(json) => match facet_json::from_str::<super::StatusPayload>(json) {
                Ok(payload) => {
                    let has_app_group_path = payload
                        .app_group_path
                        .as_deref()
                        .map(str::trim)
                        .is_some_and(|path| !path.is_empty());
                    if has_app_group_path {
                        log.ok(format!(
                            "status.json: app_group_path present (connected={})",
                            payload.connected
                        ));
                        return FsKitResult::success(log.into_entries());
                    }

                    log.info("status.json: app_group_path missing");
                    if attempt < 5 {
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        continue;
                    }
                    return FsKitResult::failure(
                        log.into_entries(),
                        "mount failed — status.json missing app_group_path",
                        vec!["sudo killall fskitd fskit_agent".to_string()],
                    );
                }
                Err(e) => {
                    log.fail(format!("status.json: parse error: {e}"));
                    if attempt < 5 {
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        continue;
                    }
                    return FsKitResult::failure(
                        log.into_entries(),
                        format!("status.json parse error: {e}"),
                        vec!["sudo killall fskitd fskit_agent".to_string()],
                    );
                }
            },
            StatusProbe::Timeout => {
                log.fail("status.json: timed out (mount is zombied)");
                return FsKitResult::failure(
                    log.into_entries(),
                    "mount is zombied — /bin/cat status.json timed out",
                    vec!["sudo killall fskitd fskit_agent".to_string()],
                );
            }
            StatusProbe::Failed(_) | StatusProbe::NotAttempted => {
                if attempt < 5 {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    continue;
                }
                return FsKitResult::failure(
                    log.into_entries(),
                    "mount failed — could not read status.json after 5 attempts",
                    vec!["sudo killall fskitd fskit_agent".to_string()],
                );
            }
        }
    }

    // Should not reach here, but just in case
    FsKitResult::failure(
        log.into_entries(),
        "mount verification failed",
        vec![RESTART_FSKIT_COMMAND.to_string()],
    )
}

async fn recent_extensionkit_log_excerpt() -> Option<String> {
    let output = Command::new("/usr/bin/log")
        .args([
            "show",
            "--last",
            "20s",
            "--style",
            "compact",
            "--predicate",
            &format!(
                "process == \"mount\" OR process == \"fskitd\" OR process == \"fskit_agent\" \
                 OR process == \"extensionkitservice\" OR eventMessage CONTAINS \"{VIXEN_EXTENSION_IDENTIFIER}\""
            ),
        ])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn diagnose_mount_failure(
    exit_code: Option<i32>,
    stderr: &str,
    system_log_excerpt: Option<&str>,
    registration_lines: &[String],
) -> MountFailureDiagnosis {
    if exit_code == Some(69) && stderr.contains("not found") {
        if let Some(extensionkit_failure) =
            diagnose_extensionkit_failure(system_log_excerpt, registration_lines)
        {
            return extensionkit_failure;
        }

        return MountFailureDiagnosis {
            summary: "FSKit daemon has a stale view of the VixenFS extension \
                      (it was likely rebuilt). Kill fskitd to force a rescan."
                .to_string(),
            remediation: vec![RESTART_FSKIT_COMMAND.to_string()],
            extra_log_lines: registration_lines.to_vec(),
        };
    }

    if exit_code == Some(69) {
        return MountFailureDiagnosis {
            summary: "The VixenFS filesystem type is not available. \
                      The FSKit daemon may need to be restarted."
                .to_string(),
            remediation: vec![RESTART_FSKIT_COMMAND.to_string()],
            extra_log_lines: registration_lines.to_vec(),
        };
    }

    let first_line = stderr.lines().next().unwrap_or("").trim();
    let detail = if first_line.is_empty() {
        String::new()
    } else {
        format!(": {first_line}")
    };
    MountFailureDiagnosis {
        summary: format!("mount failed{detail}"),
        remediation: vec![RESTART_FSKIT_COMMAND.to_string()],
        extra_log_lines: registration_lines.to_vec(),
    }
}

fn diagnose_extensionkit_failure(
    system_log_excerpt: Option<&str>,
    registration_lines: &[String],
) -> Option<MountFailureDiagnosis> {
    let excerpt = system_log_excerpt?;
    let matched_lines = excerpt
        .lines()
        .filter(|line| {
            (line.contains("extensionkitservice")
                && line.contains("Failed to initialize _EXExtensionIdentity"))
                || (line.contains("fskit_agent")
                    && line.contains("Failed to create extensionProcess for extension")
                    && line.contains(VIXEN_EXTENSION_IDENTIFIER))
        })
        .map(str::trim)
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let matched_lines = matched_lines
        .into_iter()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();

    if matched_lines.is_empty() {
        return None;
    }

    Some(MountFailureDiagnosis {
        summary: "ExtensionKit could not launch the VixenFS extension process. \
                  Restart ExtensionKit and FSKit, then relaunch Vixen.app."
            .to_string(),
        remediation: vec![
            RESTART_EXTENSIONKIT_COMMAND.to_string(),
            RESTART_FSKIT_COMMAND.to_string(),
            RELAUNCH_VIXEN_COMMAND.to_string(),
        ],
        extra_log_lines: matched_lines
            .into_iter()
            .chain(registration_lines.iter().cloned())
            .collect(),
    })
}

async fn registration_debug_lines() -> Vec<String> {
    let output = match Command::new("/usr/bin/pluginkit")
        .args(["-mAvvv", "-i", VIXEN_EXTENSION_IDENTIFIER])
        .output()
        .await
    {
        Ok(output) => output,
        Err(error) => {
            return vec![format!("pluginkit registration query failed: {error}")];
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let appex_paths = stdout
        .lines()
        .filter_map(extract_appex_path)
        .collect::<Vec<_>>();

    let mut lines = vec![format!("pluginkit -mAvvv -i {VIXEN_EXTENSION_IDENTIFIER}")];
    if appex_paths.is_empty() {
        lines.push("no appex paths found in pluginkit registration output".to_string());
        return lines;
    }

    for path in &appex_paths {
        lines.push(format!("registered appex: {path}"));
    }
    if appex_paths.iter().any(|path| path == INSTALLED_APPEX_PATH) {
        lines.push("installed app registration is present".to_string());
    } else {
        lines.push("installed app registration is missing".to_string());
    }
    for path in appex_paths
        .iter()
        .filter(|path| path.contains("/DerivedData/"))
    {
        lines.push(format!("derived data appex is registered: {path}"));
    }

    lines
}

fn extract_appex_path(line: &str) -> Option<String> {
    let start = line.find('/')?;
    let path = &line[start..];
    let end = path.find(".appex")?;
    Some(path[..end + ".appex".len()].to_owned())
}

#[cfg(test)]
mod tests {
    use super::{
        RELAUNCH_VIXEN_COMMAND, RESTART_EXTENSIONKIT_COMMAND, RESTART_FSKIT_COMMAND,
        diagnose_mount_failure, extract_appex_path,
    };

    #[test]
    fn diagnose_mount_failure_prefers_extensionkit_identity_failure() {
        let log_excerpt = r#"
2026-03-12 21:29:25.521 E  extensionkitservice[8164:59d63d6] [com.apple.extensionkit:default] Failed to initialize _EXExtensionIdentity: Error Domain=com.apple.extensionKit.errorDomain Code=5
2026-03-12 21:29:25.523 E  fskit_agent[37787:59d57a1] [com.apple.extensionkit:default] Failed to create extensionProcess for extension 'eu.bearcove.vixenfs' error: Error Domain=com.apple.extensionKit.errorDomain Code=2
"#;

        let diagnosis = diagnose_mount_failure(
            Some(69),
            "mount: File system named vixenfs not found",
            Some(log_excerpt),
            &[],
        );

        assert_eq!(
            diagnosis.summary,
            "ExtensionKit could not launch the VixenFS extension process. Restart ExtensionKit and FSKit, then relaunch Vixen.app."
        );
        assert_eq!(
            diagnosis.remediation,
            vec![
                RESTART_EXTENSIONKIT_COMMAND.to_string(),
                RESTART_FSKIT_COMMAND.to_string(),
                RELAUNCH_VIXEN_COMMAND.to_string(),
            ]
        );
        assert_eq!(diagnosis.extra_log_lines.len(), 2);
    }

    #[test]
    fn diagnose_mount_failure_falls_back_to_stale_view() {
        let diagnosis = diagnose_mount_failure(
            Some(69),
            "mount: File system named vixenfs not found",
            None,
            &[],
        );

        assert_eq!(
            diagnosis.summary,
            "FSKit daemon has a stale view of the VixenFS extension (it was likely rebuilt). Kill fskitd to force a rescan."
        );
        assert_eq!(
            diagnosis.remediation,
            vec![RESTART_FSKIT_COMMAND.to_string()]
        );
        assert!(diagnosis.extra_log_lines.is_empty());
    }

    #[test]
    fn diagnose_mount_failure_surfaces_registration_lines() {
        let registration_lines = vec![
            "registered appex: /Users/amos/Library/Developer/Xcode/DerivedData/foo/VixenFS.appex"
                .to_string(),
            "installed app registration is missing".to_string(),
        ];

        let diagnosis = diagnose_mount_failure(
            Some(69),
            "mount: Unable to invoke task",
            None,
            &registration_lines,
        );

        assert_eq!(diagnosis.extra_log_lines, registration_lines);
    }

    #[test]
    fn extract_appex_path_parses_registration_line() {
        let line =
            "    path = /Applications/Vixen.app/Contents/Extensions/VixenFS.appex uuid = ABC";
        assert_eq!(
            extract_appex_path(line).as_deref(),
            Some("/Applications/Vixen.app/Contents/Extensions/VixenFS.appex")
        );
    }
}
