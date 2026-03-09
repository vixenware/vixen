//! Capability enforcement.
//!
//! Every evaluation task carries a `CapabilitySet` — a bitfield of what
//! operations it's allowed to perform. Checked synchronously at intrinsic
//! call sites.

/// A set of capabilities, represented as a bitfield.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CapabilitySet(u32);

impl CapabilitySet {
    /// No capabilities — pure computation only.
    pub const NONE: Self = Self(0);

    /// Filesystem access (reading files, walking directories).
    pub const FS: Self = Self(1 << 0);

    /// Discovery of volatile external state (registry queries, toolchain probing).
    pub const DISCOVER: Self = Self(1 << 1);

    /// Create a set with the given capabilities.
    pub fn new(bits: u32) -> Self {
        Self(bits)
    }

    /// Check if this set contains a specific capability.
    pub fn has(self, cap: CapabilitySet) -> bool {
        self.0 & cap.0 == cap.0
    }

    /// Union of two capability sets.
    pub fn union(self, other: CapabilitySet) -> Self {
        Self(self.0 | other.0)
    }

    /// Check if `self` is a subset of `mask` (all capabilities in self
    /// are allowed by mask).
    pub fn is_subset_of(self, mask: CapabilitySet) -> bool {
        self.0 & !mask.0 == 0
    }
}
