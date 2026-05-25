//! Capability tracking data types.
//!
//! r[cap.model]
//!
//! These types describe the capability fingerprint of closures and the
//! capability errors that can be reported by capability checking. The actual
//! capability checking pass lives in [`crate::cap_check`].

use rowan::TextRange;

/// A set of capabilities, represented as a bitset.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct CapSet(u8);

impl CapSet {
    pub const EMPTY: CapSet = CapSet(0);
    pub const FS: CapSet = CapSet(1 << 0);
    pub const DISCOVER: CapSet = CapSet(1 << 1);
    pub const SANDBOX: CapSet = CapSet(1 << 2);
    pub const HOST: CapSet = CapSet(1 << 3);

    pub fn from_name(name: &str) -> Option<CapSet> {
        match name {
            "fs" => Some(Self::FS),
            "discover" => Some(Self::DISCOVER),
            "sandbox" => Some(Self::SANDBOX),
            "host" => Some(Self::HOST),
            _ => None,
        }
    }

    pub fn union(self, other: CapSet) -> CapSet {
        CapSet(self.0 | other.0)
    }

    /// Returns the excess capabilities (bits in `self` but not in `mask`).
    pub fn satisfies(self, mask: CapSet) -> CapSet {
        CapSet(self.0 & !mask.0)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn names(self) -> Vec<&'static str> {
        let mut result = Vec::new();
        if self.0 & Self::FS.0 != 0 {
            result.push("fs");
        }
        if self.0 & Self::DISCOVER.0 != 0 {
            result.push("discover");
        }
        if self.0 & Self::SANDBOX.0 != 0 {
            result.push("sandbox");
        }
        if self.0 & Self::HOST.0 != 0 {
            result.push("host");
        }
        result
    }
}

impl std::fmt::Display for CapSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names = self.names();
        if names.is_empty() {
            write!(f, "(none)")
        } else {
            for (i, name) in names.iter().enumerate() {
                if i > 0 {
                    write!(f, " ")?;
                }
                write!(f, "#{name}")?;
            }
            Ok(())
        }
    }
}

/// A capability error found during checking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapError {
    pub range: TextRange,
    pub kind: CapErrorKind,
}

/// The kind of capability error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapErrorKind {
    /// A closure's inferred capabilities exceed the mask of the callback slot.
    CapabilityViolation {
        slot_name: String,
        allowed: CapSet,
        found: CapSet,
        excess: CapSet,
    },
    /// An unknown capability tag was used.
    UnknownCapability { name: String },
}

/// Result of capability checking.
#[derive(Debug)]
pub struct CapCheckResult {
    pub errors: Vec<CapError>,
}
