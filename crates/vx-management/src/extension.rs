//! Repair the installed VixenFS app-extension registration.
//!
//! This deliberately does not touch `/Volumes/Vixen`: it only removes stale
//! LaunchServices and PlugInKit records. It does not explicitly add/register
//! VixenFS; manual registration creates fresh FSKit identities that have not
//! been approved in System Settings.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use eyre::{Result, WrapErr, bail};

const APP_BUNDLE_IDENTIFIER: &str = "eu.bearcove.vixen";
const EXTENSION_BUNDLE_IDENTIFIER: &str = "eu.bearcove.vixenfs";
const INSTALLED_APP_PATH: &str = "/Applications/Vixen.app";
const PLUGINKIT: &str = "/usr/bin/pluginkit";
const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Versions/Current/Frameworks/LaunchServices.framework/Versions/Current/Support/lsregister";

/// Repair registration toward `/Applications/Vixen.app`.
pub fn repair_installed_extension() -> Result<()> {
    let app_path = Path::new(INSTALLED_APP_PATH);
    let extension_path = bundled_extension_path(app_path)?;

    repair_containing_app(app_path)?;
    repair_extension(&extension_path)?;
    Ok(())
}

fn bundled_extension_path(app_path: &Path) -> Result<PathBuf> {
    for directory in ["Extensions", "PlugIns"] {
        let path = app_path
            .join("Contents")
            .join(directory)
            .join("VixenFS.appex");
        if path.is_dir() {
            return Ok(path);
        }
    }
    bail!("VixenFS extension not found in {}", app_path.display())
}

fn repair_containing_app(app_path: &Path) -> Result<()> {
    let registrations = registered_containing_apps()?;
    let plan = registration_plan(&registrations, app_path, launch_services_mode());

    unregister_apps(&plan.stale_paths)?;
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

    for stale_path in stale_paths {
        run(PLUGINKIT, [OsStr::new("-r"), stale_path.as_os_str()])
            .wrap_err_with(|| format!("unregistering stale VixenFS at {}", stale_path.display()))?;
    }

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

fn registration_plan(
    registrations: &[PathBuf],
    desired_path: &Path,
    mode: LaunchServicesMode,
) -> RegistrationPlan {
    let desired_key = normalize(desired_path);
    let stale_paths = registrations
        .iter()
        .filter(|path| mode == LaunchServicesMode::Root || normalize(path) != desired_key)
        .cloned()
        .collect();
    RegistrationPlan { stale_paths }
}

fn unregister_apps(paths: &[PathBuf]) -> Result<()> {
    for path in paths {
        run(LSREGISTER, [OsStr::new("-u"), path.as_os_str()])
            .wrap_err_with(|| format!("unregistering stale Vixen.app at {}", path.display()))?;
    }
    Ok(())
}

fn registered_extensions() -> Result<Vec<PathBuf>> {
    let output = command_output(PLUGINKIT, ["-mAvvv", "-i", EXTENSION_BUNDLE_IDENTIFIER])?;
    let mut paths = BTreeSet::new();
    for line in output.lines().map(str::trim) {
        let Some(path) = line.strip_prefix("Path = ") else {
            continue;
        };
        paths.insert(PathBuf::from(path));
    }
    Ok(paths.into_iter().collect())
}

fn registered_containing_apps() -> Result<Vec<PathBuf>> {
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

    Ok(paths.into_iter().collect())
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
    let output = Command::new(program)
        .args(args)
        .output()
        .wrap_err_with(|| format!("running {program}"))?;
    if !output.status.success() {
        bail!(
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn command_output<I, S>(program: &str, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new(program)
        .args(args)
        .output()
        .wrap_err_with(|| format!("running {program}"))?;
    if !output.status.success() {
        bail!(
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn normalize(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        canonical
    } else {
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{LaunchServicesMode, registration_plan};

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
    fn root_registration_is_removed_even_when_it_points_at_installed_app() {
        let desired = Path::new("/Applications/Vixen.app");
        let plan = registration_plan(
            &[PathBuf::from("/Applications/Vixen.app")],
            desired,
            LaunchServicesMode::Root,
        );

        assert_eq!(
            plan.stale_paths,
            vec![PathBuf::from("/Applications/Vixen.app")]
        );
    }
}
