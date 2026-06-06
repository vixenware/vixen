//! Vixen Management — the single privileged component on macOS.
//!
//! macOS 15's only FSKit resource type is a **block device**, and `mount(2)` of one
//! needs root. So the CAS-VFS is mounted on a mock raw disk image (see [`mount`]), and
//! the user-level vixen daemon can no longer establish its own volume. Recovery is
//! different from ordinary unmounting: once FSKit is wedged, the unmount caller can
//! wedge too. So the privileged recovery button tries ordinary unmount in a sacrificial
//! child, escalates to terminating `fskitd` only if that fails to clear the mount, and
//! only then cleans up the backing image.
//!
//! The discipline from the wiki ("The FSKit doctor & kill-switch") is preserved: each
//! privileged capability is a **fixed, payload-less [`Button`]**. A caller fires one by
//! dropping a file into its launchd `QueueDirectories` queue ([`queue`]); nothing the
//! caller supplies decides *what* runs. There is no general "run as root" surface —
//! only this fixed set of buttons, each gated by filesystem permission on its queue
//! directory.

pub mod extension;
pub mod killswitch;
pub mod mount;
pub mod queue;
pub mod status;

/// The fixed privileged capabilities. Each maps to one queue directory; a file
/// appearing there *is* the request. Adding a button is a deliberate widening of the
/// privileged surface — there is no parameterized/general action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    /// Establish the block-backed VixenFS mount (provision device + mount).
    Mount,
    /// Recover the VixenFS mount by unwinding FSKit, then detach its mock backing image.
    Unmount,
    /// Recover/detach, then re-establish.
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
            Button::Mount => {
                extension::repair_installed_extension()?;
                mount::establish()
            }
            Button::Unmount => mount::unmount(),
            Button::Remount => {
                extension::repair_installed_extension()?;
                mount::unmount()?;
                mount::establish()
            }
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
