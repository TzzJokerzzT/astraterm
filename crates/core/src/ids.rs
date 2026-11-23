//! Stable, cheap-to-copy identifiers for windows, tabs, panes, sessions and
//! profiles.
//!
//! Identifiers are monotonic process-local counters. They never encode layout
//! or process information, so they stay valid across layout changes and are
//! safe to move between threads.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Generates a newtype over `u64` with a process-local monotonic allocator.
macro_rules! id_type {
    ($name:ident, $counter:ident) => {
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u64);

        impl $name {
            /// Allocates the next identifier of this kind.
            pub fn next() -> Self {
                static $counter: AtomicU64 = AtomicU64::new(1);
                Self($counter.fetch_add(1, Ordering::Relaxed))
            }

            /// Builds an identifier from a previously stored numeric value.
            ///
            /// Used by workspace restoration, which persists identifiers.
            pub const fn from_raw(raw: u64) -> Self {
                Self(raw)
            }

            /// Returns the underlying numeric value.
            pub const fn raw(self) -> u64 {
                self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

id_type!(WindowId, WINDOW_COUNTER);
id_type!(TabId, TAB_COUNTER);
id_type!(PaneId, PANE_COUNTER);
id_type!(SessionId, SESSION_COUNTER);
id_type!(ProfileId, PROFILE_COUNTER);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_unique_and_monotonic() {
        let first = TabId::next();
        let second = TabId::next();
        assert!(second.raw() > first.raw());
        assert_ne!(first, second);
    }

    #[test]
    fn identifiers_round_trip_through_raw_values() {
        let id = PaneId::next();
        assert_eq!(PaneId::from_raw(id.raw()), id);
    }

    #[test]
    fn every_identifier_kind_allocates_non_zero_values() {
        assert!(TabId::next().raw() >= 1);
        assert!(PaneId::next().raw() >= 1);
        assert!(SessionId::next().raw() >= 1);
        assert!(WindowId::next().raw() >= 1);
        assert!(ProfileId::next().raw() >= 1);
    }
}
