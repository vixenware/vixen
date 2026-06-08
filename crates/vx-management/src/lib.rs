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
//! privileged capability is a **fixed, payload-less [`Button`]**. The launchd queue is
//! only valid for operations that work correctly from the system bootstrap. Ordinary
//! FSKit mount must be run by the installed helper as an elevated process in the active
//! login session, matching the manual `sudo vixen-management mount` path.

pub mod extension;
pub mod killswitch;
pub mod mount;
pub mod probe;
pub mod queue;
pub mod repair;
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
    /// Legacy queue remount request. The launchd mount path is disabled until
    /// proven; app-visible repair runs the direct helper mount instead.
    Remount,
    /// Terminate the system `fskitd` — the wedge kill-switch.
    KillFskitd,
}

impl Button {
    /// Every button, in a stable order (the daemon scans them on launch).
    pub const ALL: [Button; 4] = [
        Button::Mount,
        Button::Unmount,
        Button::Remount,
        Button::KillFskitd,
    ];

    /// The queue-directory basename for this button.
    pub fn queue_name(self) -> &'static str {
        match self {
            Button::Mount => "mount",
            Button::Unmount => "unmount",
            Button::Remount => "remount",
            Button::KillFskitd => "kill-fskitd",
        }
    }

    /// Parse a button from its queue/CLI name.
    pub fn from_name(name: &str) -> Option<Button> {
        Button::ALL.into_iter().find(|b| b.queue_name() == name)
    }

    /// Execute the capability. **Requires root** (mount/unmount/kill).
    pub fn execute(self) -> eyre::Result<()> {
        match self {
            Button::Mount => mount::establish(),
            Button::Unmount => mount::unmount(),
            Button::Remount => repair::repair_vixenfs(),
            Button::KillFskitd => killswitch::terminate_fskitd().map(|_| ()),
        }
    }

    /// Execute the capability from the system launchd job.
    pub fn execute_from_launchd(self) -> eyre::Result<()> {
        match self {
            Button::Mount | Button::Remount => eyre::bail!(
                "mount requests from the launchd queue are disabled; run vixen-management mount as an elevated process in the active login session"
            ),
            Button::Unmount => mount::unmount(),
            Button::KillFskitd => killswitch::terminate_fskitd().map(|_| ()),
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
