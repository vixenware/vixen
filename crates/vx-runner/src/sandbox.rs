//! Sandbox confinement for executed actions.
//!
//! On macOS this wraps the process in `sandbox-exec` under a generated **seatbelt**
//! (SBPL) profile: an allow-default base (so the process can start and run — dyld,
//! libsystem, the read of its binary), then confined — **no network**, and **writes
//! only under the action's output roots** (plus `/dev`). That bounds the write-set
//! the VFS flushes to exactly the declared output roots.
//!
//! Read confinement (restricting reads to the hashed read-set — the cache key's other
//! half) is the coupled cache+sandbox closure design, deliberately deferred (see the
//! wiki "Design cache key + sandbox together"). This cut bounds writes + network.
//!
//! **Canonicalize the roots.** macOS resolves symlinks (`/tmp` → `/private/tmp`,
//! and the FSKit mount path) before the sandbox checks a path, so a profile written
//! against an unresolved path never matches — the caller passes real (canonicalized)
//! paths.
//!
//! On non-macOS the process currently runs unsandboxed (Linux landlock-exec wiring is
//! separate); the write-set is still bounded by what the action writes into the VFS.

use moire::process::Command;

/// Build the command to run `program args…`, confined to writing only under
/// `write_roots` (canonicalized real paths) and denied network.
pub fn make_command(program: &str, args: &[String], write_roots: &[String]) -> Command {
    #[cfg(target_os = "macos")]
    {
        let profile = seatbelt_profile(write_roots);
        let mut cmd = Command::new("/usr/bin/sandbox-exec");
        cmd.arg("-p").arg(profile).arg(program).args(args);
        cmd
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = write_roots;
        let mut cmd = Command::new(program);
        cmd.args(args);
        cmd
    }
}

/// The seatbelt profile: allow-default, then deny network and confine writes to the
/// output roots (+ `/dev`).
#[cfg(target_os = "macos")]
fn seatbelt_profile(write_roots: &[String]) -> String {
    let mut profile = String::from(
        "(version 1)\n(allow default)\n(deny network*)\n(deny file-write*)\n(allow file-write*\n  (subpath \"/dev\")",
    );
    for root in write_roots {
        // SBPL string literal: escape backslash then double-quote.
        let escaped = root.replace('\\', "\\\\").replace('"', "\\\"");
        profile.push_str("\n  (subpath \"");
        profile.push_str(&escaped);
        profile.push('"');
        profile.push(')');
    }
    profile.push_str(")\n");
    profile
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// The seatbelt sandbox actually confines: the allowed output root is writable,
    /// a sibling path is not, and reads still work. Exercises the real
    /// `sandbox-exec` on the host (no VFS needed).
    #[test]
    fn seatbelt_confines_writes_to_the_output_root() {
        let dir = std::env::temp_dir().join(format!("vx-sbtest-{}", std::process::id()));
        let inp = dir.join("in");
        let out = dir.join("out");
        std::fs::create_dir_all(&inp).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(inp.join("x.txt"), b"input-data").unwrap();
        // Canonicalize — the sandbox checks the resolved path.
        let out_real = out.canonicalize().unwrap();
        let in_real = inp.canonicalize().unwrap();

        let write_roots = vec![out_real.to_string_lossy().into_owned()];
        let script = format!(
            "cat {in_x} && echo result > {out_y} && echo ok-write",
            in_x = in_real.join("x.txt").display(),
            out_y = out_real.join("y.txt").display(),
        );

        let run = |script: &str, roots: &[String]| -> std::process::Output {
            // moire's Command runs on moire's runtime; use a blocking std equivalent
            // shape via the same profile to keep the test synchronous + hermetic.
            let profile = seatbelt_profile(roots);
            std::process::Command::new("/usr/bin/sandbox-exec")
                .arg("-p")
                .arg(&profile)
                .arg("/bin/sh")
                .arg("-c")
                .arg(script)
                .output()
                .unwrap()
        };

        // Allowed: read the input + write into the output root.
        let ok = run(&script, &write_roots);
        assert!(
            ok.status.success(),
            "allowed write should succeed: {}",
            String::from_utf8_lossy(&ok.stderr)
        );
        let stdout = String::from_utf8_lossy(&ok.stdout);
        assert!(
            stdout.contains("input-data"),
            "read of the input worked: {stdout:?}"
        );
        assert!(
            stdout.contains("ok-write"),
            "the allowed write ran: {stdout:?}"
        );
        assert!(out_real.join("y.txt").exists(), "the allowed write landed");

        // Denied: writing outside the output root is blocked.
        let bad = run(
            &format!("echo nope > {}", in_real.join("hax.txt").display()),
            &write_roots,
        );
        assert!(
            !bad.status.success(),
            "writing outside the output root must be denied"
        );
        assert!(
            !in_real.join("hax.txt").exists(),
            "the denied write must not land"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
