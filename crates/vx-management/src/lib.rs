//! Vixen Management — the single privileged component on macOS.
//!
//! macOS 15's only FSKit resource type is a **block device**, and `mount(2)` of one
//! needs root. So the CAS-VFS is mounted on a mock raw disk image (see [`mount`]), and
//! the user-level vixen daemon can no longer establish its own volume. Registration
//! repair stays in the logged-in user session; these privileged buttons only touch the
//! fixed mount/recovery surface. Recovery is different from ordinary unmounting: once
//! FSKit is wedged, the unmount caller can wedge too. So the privileged recovery button
//! tries ordinary unmount in a sacrificial child, escalates to terminating `fskitd` only
//! if that fails to clear the mount, and only then cleans up the backing image.
//!
//! The discipline from the wiki ("The FSKit doctor & kill-switch") is preserved: each
//! privileged capability is a **fixed, payload-less [`Button`]**. The root service owns
//! provisioning, recovery, and cleanup. Ordinary FSKit mount is the one privileged
//! operation whose final `/sbin/mount` process must run in the active console user's
//! launchd bootstrap; [`mount`] keeps that boundary at the leaf command.

pub mod extension;
pub mod killswitch;
pub mod mount;
pub mod probe;
pub mod queue;
pub mod repair_status;
pub mod session;
pub mod status;
pub mod uninstall;

/// The fixed privileged capabilities. Each maps to one queue directory; a file
/// appearing there *is* the request. Adding a button is a deliberate widening of the
/// privileged surface — there is no parameterized/general action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    /// Establish the block-backed VixenFS mount (provision device + mount).
    Mount,
    /// Recover the VixenFS mount by unwinding FSKit, then detach its mock backing image.
    Unmount,
    /// Recover the VixenFS mount, then establish it again.
    Remount,
    /// Terminate the system `fskitd` — the wedge kill-switch.
    KillFskitd,
    /// Remove Vixen-owned privileged state and the installed app bundle.
    Uninstall,
}

impl Button {
    /// Every button, in a stable order (the daemon scans them on launch).
    pub const ALL: [Button; 5] = [
        Button::Mount,
        Button::Unmount,
        Button::Remount,
        Button::KillFskitd,
        Button::Uninstall,
    ];

    /// The queue-directory basename for this button.
    pub fn queue_name(self) -> &'static str {
        match self {
            Button::Mount => "mount",
            Button::Unmount => "unmount",
            Button::Remount => "remount",
            Button::KillFskitd => "kill-fskitd",
            Button::Uninstall => "uninstall",
        }
    }

    /// Parse a button from its queue/CLI name.
    pub fn from_name(name: &str) -> Option<Button> {
        Button::ALL.into_iter().find(|b| b.queue_name() == name)
    }

    /// Execute the capability. **Requires root** (mount/unmount/kill).
    /// Used for direct CLI invocations (e.g. `sudo vixen-management mount`) where the
    /// calling process is already in the user's GUI session, so `launchctl asuser` works.
    pub fn execute(self) -> eyre::Result<()> {
        match self {
            Button::Mount => mount::establish(None),
            Button::Unmount => mount::unmount(),
            Button::Remount => mount::remount(None),
            Button::KillFskitd => killswitch::terminate_fskitd().map(|_| ()),
            Button::Uninstall => uninstall::uninstall_vixenfs(),
        }
    }

    /// Execute the capability from the system launchd job.
    /// `caller_pid` is the PID extracted from the queue trigger file — the Vixen app
    /// process running in the user's GUI session. For mount, we use `launchctl bsexec`
    /// into that PID so `/sbin/mount` runs in the login session where `fskit_agent` lives.
    pub fn execute_from_launchd(self, caller_pid: Option<u32>) -> eyre::Result<()> {
        match self {
            Button::Mount => mount::establish(caller_pid),
            Button::Unmount => mount::unmount(),
            Button::Remount => mount::remount(caller_pid),
            Button::KillFskitd => killswitch::terminate_fskitd().map(|_| ()),
            Button::Uninstall => uninstall::uninstall_vixenfs(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_names_round_trip() {
        for b in Button::ALL {
            assert_eq!(Button::from_name(b.queue_name()), Some(b));
        }
        assert_eq!(Button::from_name("nope"), None);
    }
}
