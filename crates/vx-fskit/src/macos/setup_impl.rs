use camino::Utf8PathBuf;
use tokio::process::Command;

use super::probe::{self, HealthProbe};
use super::{FsKitResult, Log, VIXEN_APP_PATH, VIXEN_EXTENSION_IDENTIFIER, mount_path_candidates};

/// Mount a VixenFS filesystem.
///
/// Checks prerequisites (app installed, codesign, extension registered+enabled),
/// cleans up broken mounts, mounts, and verifies via health.json.
pub async fn setup(sid: &str, mount_path: &Utf8PathBuf, debug: bool) -> FsKitResult {
    let mut log = Log::new();
    let candidates = mount_path_candidates(mount_path).await;

    log.record(format!(
        "setup: sid={sid}, mount_path={mount_path}, debug={debug}"
    ));

    // --- Prerequisites ---

    // Is Vixen.app installed?
    if tokio::fs::metadata(VIXEN_APP_PATH).await.is_err() {
        log.record(format!("{VIXEN_APP_PATH} not found"));
        return FsKitResult::failure(
            log.into_entries(),
            "Vixen.app is not installed",
            vec!["brew install --cask bearcove/tap/vixen".to_string()],
        );
    }
    log.record(format!("{VIXEN_APP_PATH} exists"));

    // Is codesign valid?
    match Command::new("codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(VIXEN_APP_PATH)
        .output()
        .await
    {
        Ok(output) if output.status.success() => {
            log.record("codesign: valid");
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            log.record(format!("codesign: FAILED — {}", stderr.trim()));
            return FsKitResult::failure(
                log.into_entries(),
                "Vixen.app codesign is invalid",
                vec!["brew reinstall --cask bearcove/tap/vixen".to_string()],
            );
        }
        Err(e) => {
            log.record(format!("codesign: failed to run: {e}"));
            // Non-fatal — codesign might not be available in CI
        }
    }

    // Is extension registered and enabled?
    match Command::new("pluginkit")
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
                log.record("pluginkit: extension registered and enabled");
            } else {
                let registered = stdout
                    .lines()
                    .any(|line| line.contains(VIXEN_EXTENSION_IDENTIFIER));
                if registered {
                    log.record("pluginkit: extension registered but NOT enabled");
                    return FsKitResult::failure(
                        log.into_entries(),
                        "VixenFS extension is registered but not enabled",
                        vec!["run `vx doctor` to enable the extension".to_string()],
                    );
                } else {
                    log.record("pluginkit: extension not registered");
                    return FsKitResult::failure(
                        log.into_entries(),
                        "VixenFS extension is not registered",
                        vec![
                            "run `vx doctor` to launch Vixen.app and register the extension"
                                .to_string(),
                        ],
                    );
                }
            }
        }
        Err(e) => {
            log.record(format!("pluginkit: failed to run: {e}"));
            // Non-fatal — might not be available
        }
    }

    // --- Probe mount point ---

    let probe_result = probe::probe(mount_path, &candidates, &mut log, true).await;

    if probe_result.is_healthy() {
        log.record("mount is already healthy");
        return FsKitResult::success(log.into_entries());
    }

    if probe_result.is_broken_mount() {
        log.record("broken mount detected, running teardown first");
        let teardown_result = super::teardown_impl::teardown(mount_path).await;
        // Merge teardown logs
        for entry in teardown_result.log {
            log.entries.push(entry);
        }
        if let Some(err) = teardown_result.error {
            log.record(format!("teardown failed: {}", err.summary));
            return FsKitResult::failure(log.into_entries(), err.summary, err.remediation);
        }
    }

    // --- Mount ---

    // Create mount point directory
    if let Err(e) = tokio::fs::create_dir_all(mount_path.as_std_path()).await {
        log.record(format!("mkdir -p {mount_path}: {e}"));
        return FsKitResult::failure(
            log.into_entries(),
            format!("failed to create mount point: {e}"),
            vec![],
        );
    }
    log.record(format!("mkdir -p {mount_path}: ok"));

    // Build the source URL
    let mut source = format!("vixen://session?sid={sid}");
    if debug {
        source.push_str("&debug=1");
    }

    log.record(format!(
        "mounting: /sbin/mount -F -t vixenfs {source} {mount_path}"
    ));

    let mount_output = match Command::new("/sbin/mount")
        .args(["-F", "-t", "vixenfs", &source, mount_path.as_str()])
        .output()
        .await
    {
        Ok(output) => output,
        Err(e) => {
            log.record(format!("/sbin/mount: failed to run: {e}"));
            return FsKitResult::failure(
                log.into_entries(),
                format!("failed to run /sbin/mount: {e}"),
                vec!["sudo killall fskitd fskit_agent".to_string()],
            );
        }
    };

    if !mount_output.status.success() {
        let stderr = String::from_utf8_lossy(&mount_output.stderr);
        log.record(format!(
            "/sbin/mount: failed (exit={}): {}",
            mount_output.status,
            stderr.trim()
        ));
        return FsKitResult::failure(
            log.into_entries(),
            format!("mount failed: {}", stderr.trim()),
            vec!["sudo killall fskitd fskit_agent".to_string()],
        );
    }
    log.record("/sbin/mount: ok");

    // --- Verify via health.json ---

    // Retry a few times — the extension needs time to start up and connect.
    for attempt in 1..=5 {
        log.record(format!("health check attempt {attempt}/5"));
        let health = probe::probe(mount_path, &candidates, &mut log, true).await;
        match &health.health {
            HealthProbe::Ok(json) => match facet_json::from_str::<super::HealthPayload>(json) {
                Ok(payload) if payload.connected => {
                    log.record("health.json: connected=true, mount is healthy");
                    return FsKitResult::success(log.into_entries());
                }
                Ok(_payload) => {
                    log.record("health.json: connected=false");
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
                    log.record(format!("health.json: parse error: {e}"));
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
                log.record("health.json: timed out (mount is zombied)");
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
