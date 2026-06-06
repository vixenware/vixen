//! Repair the installed VixenFS app-extension registration.
//!
//! This deliberately does not touch `/Volumes/Vixen`: it only reconciles
//! LaunchServices and PlugInKit records for the installed app bundle.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

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
    settle_containing_app(app_path)?;
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
    let app_key = normalize(app_path);
    let registrations = registered_containing_apps()?;
    let stale_paths: Vec<_> = registrations
        .iter()
        .filter(|path| normalize(path) != app_key)
        .cloned()
        .collect();

    unregister_apps(&stale_paths)?;
    register_app(app_path)
}

fn repair_extension(extension_path: &Path) -> Result<()> {
    let extension_key = normalize(extension_path);
    let registrations = registered_extensions()?;
    let stale_paths: Vec<_> = registrations
        .iter()
        .filter(|path| normalize(path) != extension_key)
        .cloned()
        .collect();
    let has_desired = registrations
        .iter()
        .any(|path| normalize(path) == extension_key);

    for stale_path in stale_paths {
        run(PLUGINKIT, [OsStr::new("-r"), stale_path.as_os_str()])
            .wrap_err_with(|| format!("unregistering stale VixenFS at {}", stale_path.display()))?;
    }

    if !has_desired {
        run(PLUGINKIT, [OsStr::new("-a"), extension_path.as_os_str()])
            .wrap_err_with(|| format!("registering VixenFS at {}", extension_path.display()))?;
    }

    Ok(())
}

fn settle_containing_app(app_path: &Path) -> Result<()> {
    let app_key = normalize(app_path);
    for attempt in 0..5 {
        let registrations = registered_containing_apps()?;
        let stale_paths: Vec<_> = registrations
            .iter()
            .filter(|path| normalize(path) != app_key)
            .cloned()
            .collect();
        let has_desired = registrations.iter().any(|path| normalize(path) == app_key);

        if stale_paths.is_empty() && has_desired {
            return Ok(());
        }

        unregister_apps(&stale_paths)?;
        if !has_desired {
            register_app(&app_path)?;
        }

        if attempt < 4 {
            thread::sleep(Duration::from_millis(200));
        }
    }

    let registrations = registered_containing_apps()?;
    let stale_paths: Vec<_> = registrations
        .iter()
        .filter(|path| normalize(path) != app_key)
        .collect();
    let has_desired = registrations.iter().any(|path| normalize(path) == app_key);
    if !stale_paths.is_empty() || !has_desired {
        bail!(
            "LaunchServices still has stale Vixen.app registrations: {}",
            registrations
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    Ok(())
}

fn unregister_apps(paths: &[PathBuf]) -> Result<()> {
    for path in paths {
        run(LSREGISTER, [OsStr::new("-u"), path.as_os_str()])
            .wrap_err_with(|| format!("unregistering stale Vixen.app at {}", path.display()))?;
    }
    Ok(())
}

fn register_app(app_path: &Path) -> Result<()> {
    run(
        LSREGISTER,
        [
            OsStr::new("-f"),
            OsStr::new("-R"),
            OsStr::new("-trusted"),
            app_path.as_os_str(),
        ],
    )
    .wrap_err_with(|| format!("registering Vixen.app at {}", app_path.display()))
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
