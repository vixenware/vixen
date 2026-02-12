use std::collections::HashMap;
use tokio::process::Command;

use super::Log;

/// Information about a process with files open under the mount.
pub(crate) struct OpenProcess {
    pub pid: u32,
    pub command: String,
    pub path: String,
}

/// Find processes with files open under any of the path candidates.
///
/// Runs `lsof -n -P -Fpcn` and parses the structured output:
/// - `p<pid>` lines set the current PID
/// - `c<command>` lines set the current command name
/// - `n<path>` lines are file paths — matched against candidates
pub(crate) async fn find_open_processes(candidates: &[String], log: &mut Log) -> Vec<OpenProcess> {
    let output = match Command::new("lsof")
        .args(["-n", "-P", "-Fpcn"])
        .output()
        .await
    {
        Ok(output) => output,
        Err(e) => {
            log.fail(format!("lsof: failed to run: {e}"));
            return Vec::new();
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut current_pid: Option<u32> = None;
    let mut current_cmd: Option<String> = None;
    let mut found: HashMap<u32, (String, String)> = HashMap::new();

    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix('p') {
            current_pid = rest.parse().ok();
            current_cmd = None;
        } else if let Some(rest) = line.strip_prefix('c') {
            current_cmd = Some(rest.to_string());
        } else if let Some(path) = line.strip_prefix('n')
            && let (Some(pid), Some(cmd)) = (current_pid, current_cmd.as_deref())
            && candidates
                .iter()
                .any(|prefix| path.starts_with(prefix.as_str()))
        {
            found
                .entry(pid)
                .or_insert_with(|| (cmd.to_string(), path.to_string()));
        }
    }

    let result: Vec<OpenProcess> = found
        .into_iter()
        .map(|(pid, (command, path))| OpenProcess { pid, command, path })
        .collect();

    if result.is_empty() {
        log.info("lsof: no processes with files open under mount");
    } else {
        for p in &result {
            log.info(format!(
                "lsof: pid {} ({}) has files open under {}",
                p.pid, p.command, p.path
            ));
        }
    }

    result
}

/// SIGKILL processes and wait for them to die. Returns PIDs that survived
/// (uninterruptible sleep).
pub(crate) async fn kill_and_wait(processes: &[OpenProcess], log: &mut Log) -> Vec<u32> {
    if processes.is_empty() {
        return Vec::new();
    }

    log.info(format!("killing {} process(es)", processes.len()));

    let mut pids: Vec<u32> = Vec::new();
    for p in processes {
        log.info(format!(
            "SIGKILL pid {} ({}) — files open under {}",
            p.pid, p.command, p.path
        ));
        unsafe {
            libc::kill(p.pid as libc::pid_t, libc::SIGKILL);
        }
        pids.push(p.pid);
    }

    // Poll with kill(pid, 0) until they're all gone or we hit the deadline.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while !pids.is_empty() && tokio::time::Instant::now() < deadline {
        pids.retain(|&pid| {
            let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
            ret == 0 // keep if still alive
        });
        if !pids.is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    if pids.is_empty() {
        log.ok("all killed processes have exited");
    } else {
        for &pid in &pids {
            if let Some(p) = processes.iter().find(|p| p.pid == pid) {
                log.fail(format!(
                    "pid {} ({}) survived SIGKILL — uninterruptible sleep",
                    pid, p.command
                ));
            }
        }
    }

    pids
}
