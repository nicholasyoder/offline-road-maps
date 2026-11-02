//! Allocations counted per thread, for the bench (phase 5 step 5.2): built with
//! `--features count-allocs`, the app's allocator counts each thread's allocations, so the
//! UI thread's per frame can be told from the workers'. Without the feature, nothing is
//! counted and [`this_thread`] is `None`.

/// Allocations (count, bytes asked for) made on this thread so far.
#[cfg(feature = "count-allocs")]
pub fn this_thread() -> Option<(u64, u64)> {
    counting::COUNT.try_with(std::cell::Cell::get).ok()
}

#[cfg(not(feature = "count-allocs"))]
pub fn this_thread() -> Option<(u64, u64)> {
    None
}

#[cfg(feature = "count-allocs")]
#[allow(unsafe_code)]
mod counting {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        // Const-initialized with no destructor: reading it never allocates.
        pub static COUNT: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
    }

    fn note(bytes: usize) {
        // The slot can be gone while the thread exits; that allocation is not counted.
        let _ = COUNT.try_with(|c| {
            let (n, b) = c.get();
            c.set((n + 1, b + bytes as u64));
        });
    }

    struct Counting;

    // SAFETY: every call is passed on to the system allocator unchanged.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            note(l.size());
            unsafe { System.alloc(l) }
        }

        unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
            note(l.size());
            unsafe { System.alloc_zeroed(l) }
        }

        unsafe fn realloc(&self, p: *mut u8, l: Layout, size: usize) -> *mut u8 {
            note(size);
            unsafe { System.realloc(p, l, size) }
        }

        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            unsafe { System.dealloc(p, l) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: Counting = Counting;
}
