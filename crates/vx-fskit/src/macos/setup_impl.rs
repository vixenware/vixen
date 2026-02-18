use camino::Utf8PathBuf;
use peeps::Command;

use super::probe::{self, HealthProbe};
use super::{FsKitResult, Log, VIXEN_APP_PATH, VIXEN_EXTENSION_IDENTIFIER, mount_path_candidates};

const PEEPS_CX: peeps::PeepsContext = peeps::PeepsContext::new(env!("CARGO_MANIFEST_DIR"));

/// Mount a VixenFS filesystem.
///
/// Checks prerequisites (app installed, codesign, extension registered+enabled),
/// cleans up broken mounts, mounts, and verifies via health.json.
pub async fn setup(sid: &str, mount_path: &Utf8PathBuf, debug: bool) -> FsKitResult {
    let mut log = Log::new();
    let candidates = mount_path_candidates(mount_path).await;

    // --- Prerequisites ---

    // Check extension first — if it's registered and enabled, the app is
    // necessarily installed and properly signed, so we can skip those checks.
    let extension_ok = match Command::new("pluginkit")
        .args(["-m", "-p", "com.apple.fskit.fsmodule"])
        .output_with_cx(PEEPS_CX)
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
        if peeps::fs::metadata(VIXEN_APP_PATH).await.is_err() {
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
            .output_with_cx(PEEPS_CX)
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
    if let Err(e) = peeps::fs::create_dir_all(mount_path.as_std_path()).await {
        log.fail(format!("mkdir -p {mount_path}: {e}"));
        return FsKitResult::failure(
            log.into_entries(),
            format!("failed to create mount point: {e}"),
            vec![],
        );
    }
    // Build the source URL
    let mut source = format!("vixen://session?sid={sid}");
    if debug {
        source.push_str("&debug=1");
    }
    if let Ok(addr) = std::env::var("PEEPS_DASHBOARD") {
        let trimmed = addr.trim();
        if !trimmed.is_empty() {
            source.push_str("&peeps_dashboard=");
            source.push_str(trimmed);
        }
    }

    let mount_output = match Command::new("/sbin/mount")
        .args(["-F", "-t", "vixenfs", &source, mount_path.as_str()])
        .output_with_cx(PEEPS_CX)
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

        // Exit code 69 (EX_UNAVAILABLE) with "File system named vixenfs not found"
        // means fskitd has a stale view of the extension — typically because the
        // extension binary was rebuilt or updated. Killing fskitd forces a rescan.
        let (summary, remediation) = if exit_code == Some(69) && stderr.contains("not found") {
            (
                "FSKit daemon has a stale view of the VixenFS extension \
                 (it was likely rebuilt). Kill fskitd to force a rescan."
                    .to_string(),
                vec!["sudo killall fskitd fskit_agent".to_string()],
            )
        } else if exit_code == Some(69) {
            (
                "The VixenFS filesystem type is not available. \
                 The FSKit daemon may need to be restarted."
                    .to_string(),
                vec!["sudo killall fskitd fskit_agent".to_string()],
            )
        } else {
            let first_line = stderr.lines().next().unwrap_or("").trim();
            let detail = if first_line.is_empty() {
                String::new()
            } else {
                format!(": {first_line}")
            };
            (
                format!("mount failed{detail}"),
                vec!["sudo killall fskitd fskit_agent".to_string()],
            )
        };

        log.fail(format!(
            "mount failed (exit code {})",
            exit_code.map_or("unknown".to_string(), |c| c.to_string())
        ));
        if !stderr.trim().is_empty() {
            for line in stderr.trim().lines() {
                log.info(format!("  {line}"));
            }
        }

        return FsKitResult::failure(log.into_entries(), summary, remediation);
    }

    // --- Verify via health.json ---

    // Retry a few times — the extension needs time to start up and connect.
    for attempt in 1..=5 {
        log.info(format!("health check attempt {attempt}/5"));
        let health = probe::probe(mount_path, &candidates, &mut log, true).await;
        match &health.health {
            HealthProbe::Ok(json) => match facet_json::from_str::<super::HealthPayload>(json) {
                Ok(payload) if payload.connected => {
                    log.ok("health.json: connected=true, mount is healthy");
                    return FsKitResult::success(log.into_entries());
                }
                Ok(_payload) => {
                    log.info("health.json: connected=false");
                    if attempt < 5 {
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        continue;
                    }
                    return FsKitResult::failure(
                        log.into_entries(),
                        "extension running but backend not connected",
                        vec![],
                    );
                }
                Err(e) => {
                    log.fail(format!("health.json: parse error: {e}"));
                    if attempt < 5 {
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        continue;
                    }
                    return FsKitResult::failure(
                        log.into_entries(),
                        format!("health.json parse error: {e}"),
                        vec!["sudo killall fskitd fskit_agent".to_string()],
                    );
                }
            },
            HealthProbe::Timeout => {
                log.fail("health.json: timed out (mount is zombied)");
                return FsKitResult::failure(
                    log.into_entries(),
                    "mount is zombied — /bin/cat health.json timed out",
                    vec!["sudo killall fskitd fskit_agent".to_string()],
                );
            }
            HealthProbe::Failed(_) | HealthProbe::NotAttempted => {
                if attempt < 5 {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    continue;
                }
                return FsKitResult::failure(
                    log.into_entries(),
                    "mount failed — could not read health.json after 5 attempts",
                    vec!["sudo killall fskitd fskit_agent".to_string()],
                );
            }
        }
    }

    // Should not reach here, but just in case
    FsKitResult::failure(
        log.into_entries(),
        "mount verification failed",
        vec!["sudo killall fskitd fskit_agent".to_string()],
    )
}
