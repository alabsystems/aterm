// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a DECLARED class owns, and for how long: a sixteen-byte aligned ivar,
//! dropped exactly once, and the objects its methods return (+0 and owned by
//! the pool, unless the method leaks a +1 on purpose). The return is counted
//! by the runtime's own retain count, never only a drop counter.

#![cfg(target_os = "macos")]
#![expect(
    clippy::disallowed_methods,
    reason = "the subject is the send itself: these tests cast the bare `msg` by hand"
)]

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use aterm_objc::{ClassType, Id, Sel, autoreleasepool, class_of, declare_class, msg, sel};

/// The [`MainThread`](aterm_objc::MainThread) witness, minted unchecked:
/// libtest runs every test on a worker, so `MainThread::new()` answers `None`.
/// Every class here touches no AppKit state and is created and released on the
/// same worker, which is `new_unchecked`'s second obligation.
fn mtm() -> aterm_objc::MainThread {
    // SAFETY: see this function's doc comment — the class has no main-thread
    // affinity and its ivars are born and dropped on this one worker.
    unsafe { aterm_objc::MainThread::new_unchecked() }
}

// The +0 object return, by retain count.

struct OwnerIvars {
    calls: Cell<i64>,
    drops: Arc<AtomicUsize>,
}
impl Drop for OwnerIvars {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

declare_class! {
    /// Returns a fresh instance of itself both ways: +0 (autoreleased, what a
    /// non-`new`/`copy` method owes) and +1 (the leak, kept for contrast).
    struct Owner: NSObject {
        const NAME: &str = "ATermObjcReturnOwnerProbe";
        type Ivars = OwnerIvars;

        /// The +0 return an object-returning declared method owes, checked
        /// against the runtime's own retain count rather than a drop counter.
        @sel(makeChild)
        fn make_child(&self) -> Id {
            match Owner::alloc_init(mtm(), OwnerIvars {
                calls: Cell::new(0),
                drops: Arc::clone(&self.ivars().drops),
            }) {
                Some(c) => c.autorelease(),
                None => Id::NIL,
            }
        }

        /// The +1 form, which leaks; kept so the test can show the difference.
        @sel(makeLeakedChild)
        fn make_leaked_child(&self) -> Id {
            match Owner::alloc_init(mtm(), OwnerIvars {
                calls: Cell::new(0),
                drops: Arc::clone(&self.ivars().drops),
            }) {
                Some(c) => c.into_raw(),
                None => Id::NIL,
            }
        }

        @sel(count)
        fn count(&self) -> i64 {
            self.ivars().calls.get()
        }
    }
}

fn owner(drops: &Arc<AtomicUsize>) -> aterm_objc::Retained<Owner> {
    Owner::alloc_init(
        mtm(),
        OwnerIvars {
            calls: Cell::new(0),
            drops: Arc::clone(drops),
        },
    )
    .expect("+alloc/-init")
}

#[test]
fn an_autoreleased_return_is_at_plus_one_owned_by_the_pool() {
    let drops = Arc::new(AtomicUsize::new(0));
    let obj = owner(&drops);
    // SAFETY: `-retainCount` is `-(NSUInteger)`, `-makeChild` is `-(id)`, both
    // on live instances of the declaring class.
    unsafe {
        let rc: unsafe extern "C-unwind" fn(Id, Sel) -> usize = msg();
        assert_eq!(rc(obj.as_id(), sel!(retainCount)), 1, "alloc/init is +1");

        autoreleasepool(|_| {
            let child = {
                let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = msg();
                f(obj.as_id(), sel!(makeChild))
            };
            assert!(!child.is_null());
            assert_eq!(class_of(child), Owner::class());
            assert_eq!(
                rc(child, sel!(retainCount)),
                1,
                "an autoreleased return must be at +1 held BY THE POOL — 2 means \
                 over-retained, and a freed object means under-retained"
            );
            assert_eq!(drops.load(Ordering::SeqCst), 0);
        });
        assert_eq!(
            drops.load(Ordering::SeqCst),
            1,
            "the pool did not release the autoreleased return exactly once"
        );

        // The +1 form, for contrast: it survives the pool, which is the leak.
        let leaked = autoreleasepool(|_| {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = msg();
            f(obj.as_id(), sel!(makeLeakedChild))
        });
        assert_eq!(
            drops.load(Ordering::SeqCst),
            1,
            "a `into_raw` return is NOT released by the pool — this is the leak \
             D5 names, kept here so the difference is measured"
        );
        // Clean it up so the test leaks nothing.
        let rel: unsafe extern "C-unwind" fn(Id, Sel) = msg();
        rel(leaked, sel!(release));
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }
}

// A sixteen-byte aligned ivar.

/// An ivar that needs 16-byte alignment (`class_addIvar` takes it as log2 4).
#[repr(align(16))]
struct AlignedIvars {
    marker: u64,
    drops: Arc<AtomicUsize>,
}
impl Drop for AlignedIvars {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

declare_class! {
    struct Aligned: NSObject {
        const NAME: &str = "ATermObjcAlignedIvarProbe";
        type Ivars = AlignedIvars;

        @sel(marker)
        fn marker(&self) -> u64 {
            let ivars = self.ivars();
            assert_eq!(
                std::ptr::from_ref(ivars).addr() % 16,
                0,
                "the runtime placed a 16-aligned ivar at a misaligned offset"
            );
            ivars.marker
        }
    }
}

#[test]
fn a_sixteen_byte_aligned_ivar_lands_aligned_and_drops_once() {
    let drops = Arc::new(AtomicUsize::new(0));
    {
        let obj = Aligned::alloc_init(
            mtm(),
            AlignedIvars {
                marker: 0xDEAD_BEEF,
                drops: Arc::clone(&drops),
            },
        )
        .expect("+alloc/-init");
        // SAFETY: `-marker` is `-(unsigned long long)` on a live instance.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> u64 = msg();
            assert_eq!(f(obj.as_id(), sel!(marker)), 0xDEAD_BEEF);
        }
    }
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
