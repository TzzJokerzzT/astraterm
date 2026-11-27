//! Test-only heap allocation counter.
//!
//! The row walk is supposed to stop allocating one string per cell (B19), and the
//! only honest way to assert that is to count allocations instead of timing them.
//! This module installs a counting global allocator for the crate's unit tests and
//! keeps the count in a thread local, so tests running in parallel never see each
//! other's allocations.
//!
//! Nothing here is compiled into the renderer: [`crate::test_alloc`] only exists
//! under `cfg(test)`, and `cfg(test)` is not set for a dependency.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    /// Allocations this thread has made since it was last reset.
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

/// Counts allocations in a thread local and forwards to the system allocator.
struct Counting;

impl Counting {
    /// Records one allocation. A thread that is starting up or shutting down may have
    /// no key yet; its allocations are then simply not counted.
    fn record() {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
    }
}

// SAFETY: every method forwards to `System`, which is a valid allocator, and the
// counter touches only a thread local `Cell<u64>` that never allocates itself.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::record();
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::record();
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::record();
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Allocations this thread has made since the last [`reset`].
pub fn allocations() -> u64 {
    ALLOCATIONS.try_with(|count| count.get()).unwrap_or(0)
}

/// Starts counting from zero on this thread.
pub fn reset() {
    let _ = ALLOCATIONS.try_with(|count| count.set(0));
}
