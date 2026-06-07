//! Clean stale VixenFS app-extension registration state.
//!
//! This deliberately does not touch `/Volumes/Vixen`: it only removes stale
//! LaunchServices and PlugInKit records. It does not explicitly add/register
//! VixenFS; manual registration creates fresh FSKit identities that have not
//! been approved in System Settings.
//!
//! The root/launchd path only touches root LaunchServices state. PlugInKit is
//! session-facing and can fail from the management service context with
//! `match: Connection invalid`; the logged-in app process owns that cleanup.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use eyre::{Result, WrapErr, bail};

const APP_BUNDLE_IDENTIFIER: &str = "eu.bearcove.vixen";
const EXTENSION_BUNDLE_IDENTIFIER: &str = "eu.bearcove.vixenfs";
const FSKIT_EXTENSION_POINT: &str = "com.apple.fskit.fsmodule";
const INSTALLED_APP_PATH: &str = "/Applications/Vixen.app";
const PLUGINKIT: &str = "/usr/bin/pluginkit";
const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Versions/Current/Frameworks/LaunchServices.framework/Versions/Current/Support/lsregister";

/// Remove stale records that point away from `/Applications/Vixen.app`.
pub fn repair_installed_extension() -> Result<()> {
    let app_path = Path::new(INSTALLED_APP_PATH);
    let mode = launch_services_mode();
    tracing::info!(
        mode = ?mode,
        app_path = %app_path.display(),
        "cleaning installed extension registration state"
    );

    repair_containing_app(app_path, mode)?;
    let extension_path = bundled_extension_path(app_path)?;
    if should_repair_pluginkit(mode) {
        tracing::info!(
            extension_path = %extension_path.display(),
            "cleaning user-session PlugInKit state"
        );
        repair_extension(&extension_path)?;
        elect_extension_use()?;
    } else {
        tracing::info!("skipping PlugInKit repair outside the user session");
    }
    repair_launch_services_extension(&extension_path, mode)?;
    tracing::info!(mode = ?mode, "installed extension registration cleanup finished");
    Ok(())
}

/// Record the registration state visible to the current effective user.
///
/// This is intentionally diagnostic-only: it does not add/register anything, and it
/// does not unregister the installed `/Applications/Vixen.app` copy.
pub fn log_installed_extension_diagnostics() {
    let app_path = Path::new(INSTALLED_APP_PATH);
    let mode = launch_services_mode();
    let desired_extension = bundled_extension_path(app_path).ok();
    tracing::info!(
        mode = ?mode,
        uid = unsafe { libc::geteuid() },
        app_path = %app_path.display(),
        desired_extension = %desired_extension
            .as_deref()
            .map(Path::display)
            .map(|display| display.to_string())
            .unwrap_or_else(|| "<missing>".to_string()),
        "recording installed extension diagnostics"
    );

    match registered_extension_records() {
        Ok(records) => {
            tracing::info!(
                mode = ?mode,
                record_count = records.len(),
                records = ?records,
                "PlugInKit VixenFS diagnostic records"
            );
        }
        Err(err) => {
            tracing::warn!(
                mode = ?mode,
                error = %format!("{err:#}"),
                "PlugInKit VixenFS diagnostics unavailable"
            );
        }
    }

    match registered_launch_services_extension_records() {
        Ok(records) => {
            tracing::info!(
                mode = ?mode,
                record_count = records.len(),
                records = ?records,
                "LaunchServices VixenFS diagnostic plugin records"
            );
        }
        Err(err) => {
            tracing::warn!(
                mode = ?mode,
                error = %format!("{err:#}"),
                "LaunchServices VixenFS plugin diagnostics unavailable"
            );
        }
    }
}

fn bundled_extension_path(app_path: &Path) -> Result<PathBuf> {
    for directory in ["Extensions", "PlugIns"] {
        let path = app_path
            .join("Contents")
            .join(directory)
            .join("VixenFS.appex");
        if path.is_dir() {
            tracing::info!(extension_path = %path.display(), "found bundled VixenFS extension");
            return Ok(path);
        }
        tracing::debug!(extension_path = %path.display(), "bundled extension candidate missing");
    }
    bail!("VixenFS extension not found in {}", app_path.display())
}

fn repair_containing_app(app_path: &Path, mode: LaunchServicesMode) -> Result<()> {
    let registrations = registered_containing_apps()?;
    let plan = registration_plan(&registrations, app_path, mode);
    tracing::info!(
        mode = ?mode,
        registration_count = registrations.len(),
        stale_count = plan.stale_paths.len(),
        registrations = ?registrations,
        stale_paths = ?plan.stale_paths,
        "computed LaunchServices containing-app repair plan"
    );

    unregister_apps(&plan.stale_paths)?;
    Ok(())
}

fn repair_launch_services_extension(extension_path: &Path, mode: LaunchServicesMode) -> Result<()> {
    let records = registered_launch_services_extension_records()?;
    let stale_paths = launch_services_extension_stale_paths(&records, extension_path);
    let disabled_desired = launch_services_disabled_desired_extensions(&records, extension_path);
    tracing::info!(
        mode = ?mode,
        extension_path = %extension_path.display(),
        record_count = records.len(),
        stale_count = stale_paths.len(),
        disabled_desired_count = disabled_desired.len(),
        records = ?records,
        stale_paths = ?stale_paths,
        disabled_desired = ?disabled_desired,
        "computed LaunchServices VixenFS plugin repair plan"
    );

    unregister_launch_services_paths(&stale_paths, "stale VixenFS LaunchServices plugin record")?;
    if !disabled_desired.is_empty() {
        tracing::warn!(
            mode = ?mode,
            extension_path = %extension_path.display(),
            disabled_desired = ?disabled_desired,
            "installed VixenFS LaunchServices plugin record is link-disabled"
        );
    }

    Ok(())
}

fn repair_extension(extension_path: &Path) -> Result<()> {
    let extension_key = normalize(extension_path);
    let registrations = registered_extensions()?;
    let stale_paths: Vec<_> = registrations
        .iter()
        .filter(|path| normalize(path) != extension_key)
        .cloned()
        .collect();
    tracing::info!(
        extension_path = %extension_path.display(),
        registration_count = registrations.len(),
        stale_count = stale_paths.len(),
        registrations = ?registrations,
        stale_paths = ?stale_paths,
        "computed PlugInKit extension repair plan"
    );

    for stale_path in stale_paths {
        run(PLUGINKIT, [OsStr::new("-r"), stale_path.as_os_str()])
            .wrap_err_with(|| format!("unregistering stale VixenFS at {}", stale_path.display()))?;
    }

    Ok(())
}

fn elect_extension_use() -> Result<()> {
    tracing::info!(
        extension_bundle_identifier = EXTENSION_BUNDLE_IDENTIFIER,
        extension_point = FSKIT_EXTENSION_POINT,
        "requesting PlugInKit use election for VixenFS"
    );
    run(
        PLUGINKIT,
        [
            OsStr::new("-e"),
            OsStr::new("use"),
            OsStr::new("-p"),
            OsStr::new(FSKIT_EXTENSION_POINT),
            OsStr::new("-i"),
            OsStr::new(EXTENSION_BUNDLE_IDENTIFIER),
        ],
    )
    .wrap_err("requesting PlugInKit use election for VixenFS")?;
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct RegistrationPlan {
    stale_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchServicesMode {
    User,
    Root,
}

fn launch_services_mode() -> LaunchServicesMode {
    if unsafe { libc::geteuid() } == 0 {
        LaunchServicesMode::Root
    } else {
        LaunchServicesMode::User
    }
}

fn should_repair_pluginkit(mode: LaunchServicesMode) -> bool {
    mode == LaunchServicesMode::User
}

fn registration_plan(
    registrations: &[PathBuf],
    desired_path: &Path,
    _mode: LaunchServicesMode,
) -> RegistrationPlan {
    let desired_key = normalize(desired_path);
    let stale_paths = registrations
        .iter()
        .filter(|path| normalize(path) != desired_key)
        .cloned()
        .collect();
    RegistrationPlan { stale_paths }
}

fn unregister_apps(paths: &[PathBuf]) -> Result<()> {
    unregister_launch_services_paths(paths, "stale Vixen LaunchServices app record")
}

fn unregister_launch_services_paths(paths: &[PathBuf], label: &str) -> Result<()> {
    for path in paths {
        tracing::warn!(path = %path.display(), label, "unregistering LaunchServices record");
        run(LSREGISTER, [OsStr::new("-u"), path.as_os_str()])
            .wrap_err_with(|| format!("unregistering {label} at {}", path.display()))?;
    }
    Ok(())
}

fn registered_extensions() -> Result<Vec<PathBuf>> {
    let records = registered_extension_records()?;
    let mut paths = BTreeSet::new();
    for record in records {
        if let Some(path) = record.path {
            paths.insert(path);
        }
    }
    let paths = paths.into_iter().collect::<Vec<_>>();
    tracing::info!(paths = ?paths, "PlugInKit VixenFS records");
    Ok(paths)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlugInKitRecord {
    version: Option<String>,
    uuid: Option<String>,
    path: Option<PathBuf>,
    parent_bundle: Option<PathBuf>,
}

fn registered_extension_records() -> Result<Vec<PlugInKitRecord>> {
    tracing::debug!("querying PlugInKit for VixenFS records");
    let output = command_output(PLUGINKIT, ["-mAvvv", "-i", EXTENSION_BUNDLE_IDENTIFIER])?;
    let mut records = Vec::new();
    let mut current: Option<PlugInKitRecord> = None;

    for line in output.lines().map(str::trim) {
        if line.contains(&format!("{EXTENSION_BUNDLE_IDENTIFIER}(")) {
            if let Some(record) = current.take() {
                records.push(record);
            }
            current = Some(PlugInKitRecord {
                version: version_from_pluginkit_header(line),
                uuid: None,
                path: None,
                parent_bundle: None,
            });
            continue;
        }

        let Some(record) = current.as_mut() else {
            continue;
        };
        if let Some(path) = line.strip_prefix("Path = ") {
            record.path = Some(PathBuf::from(path));
        } else if let Some(uuid) = line.strip_prefix("UUID = ") {
            record.uuid = Some(uuid.to_string());
        } else if let Some(parent_bundle) = line.strip_prefix("Parent Bundle = ") {
            record.parent_bundle = Some(PathBuf::from(parent_bundle));
        }
    }
    if let Some(record) = current {
        records.push(record);
    }

    tracing::info!(records = ?records, "PlugInKit VixenFS records");
    Ok(records)
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct LaunchServicesPluginRecord {
    path: Option<PathBuf>,
    identifier: Option<String>,
    plugin_id: Option<String>,
    plugin_identifier: Option<String>,
    uuid: Option<String>,
    link_enabled: Option<bool>,
}

impl LaunchServicesPluginRecord {
    fn is_vixenfs(&self) -> bool {
        self.identifier.as_deref() == Some(EXTENSION_BUNDLE_IDENTIFIER)
            || self.plugin_id.as_deref() == Some(EXTENSION_BUNDLE_IDENTIFIER)
            || self.plugin_identifier.as_deref() == Some(EXTENSION_BUNDLE_IDENTIFIER)
    }
}

fn registered_launch_services_extension_records() -> Result<Vec<LaunchServicesPluginRecord>> {
    tracing::debug!("querying LaunchServices for VixenFS plugin records");
    let output = command_output(LSREGISTER, ["-dump"])?;
    let records = parse_launch_services_extension_records(&output);
    tracing::info!(records = ?records, "LaunchServices VixenFS plugin records");
    Ok(records)
}

fn parse_launch_services_extension_records(output: &str) -> Vec<LaunchServicesPluginRecord> {
    let mut records = Vec::new();
    let mut current = LaunchServicesPluginRecord::default();

    for line in output.lines().map(str::trim) {
        if line.starts_with("----") {
            finish_launch_services_plugin_record(&mut records, std::mem::take(&mut current));
            continue;
        }

        if let Some(value) = line.strip_prefix("path:") {
            current.path = Some(PathBuf::from(launch_services_field(value)));
        } else if let Some(value) = line.strip_prefix("identifier:") {
            current.identifier = Some(launch_services_field(value));
        } else if let Some(value) = line.strip_prefix("plugin id:") {
            current.plugin_id = Some(launch_services_field(value));
        } else if let Some(value) = line.strip_prefix("pluginIdentifier:") {
            current.plugin_identifier = Some(launch_services_field(value));
        } else if let Some(value) = line.strip_prefix("plugin uuid:") {
            current.uuid = Some(launch_services_field(value));
        } else if let Some((key, value)) = line.split_once('=')
            && key.trim().trim_matches('"') == "_LSIsLinkEnabled"
        {
            current.link_enabled = parse_launch_services_bool(value);
        }
    }
    finish_launch_services_plugin_record(&mut records, current);

    records
}

fn finish_launch_services_plugin_record(
    records: &mut Vec<LaunchServicesPluginRecord>,
    record: LaunchServicesPluginRecord,
) {
    if record.is_vixenfs() {
        records.push(record);
    }
}

fn parse_launch_services_bool(value: &str) -> Option<bool> {
    match value.trim().trim_end_matches(';').trim() {
        "1" | "true" | "TRUE" | "YES" => Some(true),
        "0" | "false" | "FALSE" | "NO" => Some(false),
        _ => None,
    }
}

fn launch_services_extension_stale_paths(
    records: &[LaunchServicesPluginRecord],
    desired_path: &Path,
) -> Vec<PathBuf> {
    let desired_key = normalize(desired_path);
    records
        .iter()
        .filter_map(|record| record.path.as_ref())
        .filter(|path| normalize(path) != desired_key)
        .cloned()
        .collect()
}

fn launch_services_disabled_desired_extensions(
    records: &[LaunchServicesPluginRecord],
    desired_path: &Path,
) -> Vec<LaunchServicesPluginRecord> {
    let desired_key = normalize(desired_path);
    records
        .iter()
        .filter(|record| record.link_enabled == Some(false))
        .filter(|record| {
            record
                .path
                .as_deref()
                .is_some_and(|path| normalize(path) == desired_key)
        })
        .cloned()
        .collect()
}

fn version_from_pluginkit_header(line: &str) -> Option<String> {
    let (_, rest) = line.split_once(&format!("{EXTENSION_BUNDLE_IDENTIFIER}("))?;
    let (version, _) = rest.split_once(')')?;
    Some(version.to_string())
}

fn registered_containing_apps() -> Result<Vec<PathBuf>> {
    tracing::debug!("querying LaunchServices for Vixen app records");
    let output = command_output(LSREGISTER, ["-dump"])?;
    let mut paths = BTreeSet::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_identifier: Option<String> = None;

    for line in output.lines().map(str::trim) {
        if line.starts_with("----") {
            finish_record(&mut paths, current_path.take(), current_identifier.take());
            continue;
        }

        if let Some(value) = line.strip_prefix("path:") {
            current_path = Some(PathBuf::from(launch_services_field(value)));
        } else if let Some(value) = line.strip_prefix("identifier:") {
            current_identifier = Some(launch_services_field(value));
        }
    }
    finish_record(&mut paths, current_path, current_identifier);

    let paths = paths.into_iter().collect::<Vec<_>>();
    tracing::info!(paths = ?paths, "LaunchServices Vixen app records");
    Ok(paths)
}

fn finish_record(
    paths: &mut BTreeSet<PathBuf>,
    current_path: Option<PathBuf>,
    current_identifier: Option<String>,
) {
    if current_identifier.as_deref() != Some(APP_BUNDLE_IDENTIFIER) {
        return;
    }
    let Some(path) = current_path else {
        return;
    };
    if path.file_name().and_then(|name| name.to_str()) != Some("Vixen.app") {
        return;
    }
    paths.insert(path);
}

fn launch_services_field(value: &str) -> String {
    let value = value.trim();
    if let Some((prefix, _)) = value.rsplit_once(" (0x") {
        prefix.to_string()
    } else {
        value.to_string()
    }
}

fn run<I, S>(program: &str, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args = args
        .into_iter()
        .map(|arg| arg.as_ref().to_os_string())
        .collect::<Vec<_>>();
    tracing::debug!(program, args = ?args, "running management helper command");
    let output = Command::new(program)
        .args(&args)
        .output()
        .wrap_err_with(|| format!("running {program}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    tracing::info!(
        program,
        args = ?args,
        status = ?output.status.code(),
        stdout_len = stdout.len(),
        stderr = %stderr.trim(),
        "management helper command finished"
    );
    if !output.status.success() {
        bail!("{program} failed: {}", stderr.trim());
    }
    Ok(())
}

fn command_output<I, S>(program: &str, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args = args
        .into_iter()
        .map(|arg| arg.as_ref().to_os_string())
        .collect::<Vec<_>>();
    tracing::debug!(program, args = ?args, "running management helper command for output");
    let output = Command::new(program)
        .args(&args)
        .output()
        .wrap_err_with(|| format!("running {program}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    tracing::info!(
        program,
        args = ?args,
        status = ?output.status.code(),
        stdout_len = stdout.len(),
        stderr = %stderr.trim(),
        "management helper command output finished"
    );
    if !output.status.success() {
        bail!("{program} failed: {}", stderr.trim());
    }
    Ok(stdout.into_owned())
}

fn normalize(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        tracing::trace!(path = %path.display(), canonical = %canonical.display(), "normalized path");
        canonical
    } else {
        tracing::trace!(path = %path.display(), "path normalization kept original");
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        LaunchServicesMode, launch_services_disabled_desired_extensions,
        launch_services_extension_stale_paths, parse_launch_services_extension_records,
        registration_plan, should_repair_pluginkit,
    };

    #[test]
    fn clean_registration_is_noop() {
        let desired = Path::new("/Applications/Vixen.app");
        let plan = registration_plan(
            &[PathBuf::from("/Applications/Vixen.app")],
            desired,
            LaunchServicesMode::User,
        );

        assert!(plan.stale_paths.is_empty());
    }

    #[test]
    fn missing_registration_is_left_to_automatic_discovery() {
        let desired = Path::new("/Applications/Vixen.app");
        let plan = registration_plan(&[], desired, LaunchServicesMode::User);

        assert!(plan.stale_paths.is_empty());
    }

    #[test]
    fn stale_registration_is_removed_without_duplicate_desired_register() {
        let desired = Path::new("/Applications/Vixen.app");
        let plan = registration_plan(
            &[
                PathBuf::from("/tmp/dmg/Vixen.app"),
                PathBuf::from("/Applications/Vixen.app"),
            ],
            desired,
            LaunchServicesMode::User,
        );

        assert_eq!(plan.stale_paths, vec![PathBuf::from("/tmp/dmg/Vixen.app")]);
    }

    #[test]
    fn root_registration_keeps_installed_app_record() {
        let desired = Path::new("/Applications/Vixen.app");
        let plan = registration_plan(
            &[PathBuf::from("/Applications/Vixen.app")],
            desired,
            LaunchServicesMode::Root,
        );

        assert!(plan.stale_paths.is_empty());
    }

    #[test]
    fn pluginkit_cleanup_is_user_session_only() {
        assert!(should_repair_pluginkit(LaunchServicesMode::User));
        assert!(!should_repair_pluginkit(LaunchServicesMode::Root));
    }

    #[test]
    fn parses_launch_services_vixenfs_plugin_record() {
        let records = parse_launch_services_extension_records(
            r#"
--------------------------------------------------------------------------------
path:                       /Applications/Vixen.app/Contents/Extensions/VixenFS.appex (0x117dc)
identifier:                 eu.bearcove.vixenfs (0x117f0)
plugin id:                  eu.bearcove.vixenfs (0x11804)
pluginIdentifier:           eu.bearcove.vixenfs
plugin uuid:                B0BDC0FE-6744-4404-BD69-853408611EE3
infoDictionary: {
    "_LSIsLinkEnabled" = 0;
}
--------------------------------------------------------------------------------
path:                       /Applications/Vixen.app (0x11c00)
identifier:                 eu.bearcove.vixen (0x11c14)
"#,
        );

        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(
            record.path,
            Some(PathBuf::from(
                "/Applications/Vixen.app/Contents/Extensions/VixenFS.appex"
            ))
        );
        assert_eq!(
            record.uuid.as_deref(),
            Some("B0BDC0FE-6744-4404-BD69-853408611EE3")
        );
        assert_eq!(record.link_enabled, Some(false));
    }

    #[test]
    fn stale_launch_services_plugin_paths_are_removed() {
        let records = parse_launch_services_extension_records(
            r#"
--------------------------------------------------------------------------------
path:                       /Volumes/dmg.Old/Vixen.app/Contents/Extensions/VixenFS.appex (0x117dc)
pluginIdentifier:           eu.bearcove.vixenfs
--------------------------------------------------------------------------------
path:                       /Applications/Vixen.app/Contents/Extensions/VixenFS.appex (0x11804)
pluginIdentifier:           eu.bearcove.vixenfs
"#,
        );

        let stale_paths = launch_services_extension_stale_paths(
            &records,
            Path::new("/Applications/Vixen.app/Contents/Extensions/VixenFS.appex"),
        );

        assert_eq!(
            stale_paths,
            vec![PathBuf::from(
                "/Volumes/dmg.Old/Vixen.app/Contents/Extensions/VixenFS.appex"
            )]
        );
    }

    #[test]
    fn disabled_installed_launch_services_plugin_record_is_reported() {
        let records = parse_launch_services_extension_records(
            r#"
--------------------------------------------------------------------------------
path:                       /Applications/Vixen.app/Contents/Extensions/VixenFS.appex (0x11804)
pluginIdentifier:           eu.bearcove.vixenfs
infoDictionary: {
    "_LSIsLinkEnabled" = 0;
}
"#,
        );

        let disabled = launch_services_disabled_desired_extensions(
            &records,
            Path::new("/Applications/Vixen.app/Contents/Extensions/VixenFS.appex"),
        );

        assert_eq!(disabled.len(), 1);
        assert_eq!(disabled[0].link_enabled, Some(false));
    }
}
