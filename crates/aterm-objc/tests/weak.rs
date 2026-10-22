// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Weak references: the storage location, not the value, is what the runtime
//! registers.
//!
//! `objc_initWeak(location, obj)` records the address in `obj`'s side table,
//! and on dealloc the runtime writes `nil` through every recorded address. Move
//! the bytes and the copy is never zeroed while the old address stays a live
//! write target. These tests hold `WeakSlot` and `WeakObj` to that on real
//! objects: a `WeakObj` keeps one registered address across every kind of
//! Rust move, and dropping the handle unregisters before its box is freed.
//!
//! Foundation objects (`NSObject`, `NSMutableString`) have real refcounts and
//! weak tables; libtest cannot host AppKit, so `objc_window_drive.rs` repeats
//! the experiments against a live `NSView`.

#![cfg(target_os = "macos")]
#![expect(
    clippy::disallowed_methods,
    reason = "aterm-objc's own runtime tests drive the bare `msg` cast; everything else uses checked_msg"
)]

use aterm_objc::{Id, Obj, Sel, WeakObj, WeakSlot, autoreleasepool, class, msg, sel};

/// `[[NSObject alloc] init]`, +1 and owned. Not interned, cached or
/// autoreleased, so `drop` is the last release and `dealloc` runs.
fn new_object() -> Obj {
    // SAFETY: `+alloc` is `@16#0:8` and `-init` is `@16@0:8` on `NSObject`;
    // the pair yields a +1 reference this `Obj` adopts.
    unsafe {
        let alloc: unsafe extern "C-unwind" fn(Id, Sel) -> Id = msg();
        let init: unsafe extern "C-unwind" fn(Id, Sel) -> Id = msg();
        let raw = alloc(class(c"NSObject").as_id(), sel!(alloc));
        Obj::from_owned(init(raw, sel!(init))).expect("a fresh NSObject")
    }
}

/// `[[NSMutableString alloc] init]`, +1 — a second class, so the properties are
/// not accidentally about `NSObject`'s own `dealloc`.
fn new_mutable_string() -> Obj {
    // SAFETY: `+alloc`/`-init` on `NSMutableString`, same prototypes.
    unsafe {
        let alloc: unsafe extern "C-unwind" fn(Id, Sel) -> Id = msg();
        let init: unsafe extern "C-unwind" fn(Id, Sel) -> Id = msg();
        let raw = alloc(class(c"NSMutableString").as_id(), sel!(alloc));
        Obj::from_owned(init(raw, sel!(init))).expect("a fresh NSMutableString")
    }
}

// The safe handle

/// A container to move a `WeakObj` INTO, so the move is a real struct move and
/// not something the optimiser can elide as a no-op rebinding.
struct Holder {
    weak: WeakObj,
    /// Padding either side, so the field's offset inside `Holder` is non-zero
    /// and a `Holder` move genuinely relocates the handle's bytes.
    _before: [u64; 3],
}

/// Take a `WeakObj` by value and give it back — two more moves.
fn round_trip(w: WeakObj) -> WeakObj {
    w
}

/// Six real moves keep one registration: allocate, weak-store, move the
/// container, release the object, and the moved slot reads nil rather than a
/// dangling pointer.
#[test]
fn a_boxed_weak_survives_every_move_rust_can_perform() {
    let obj = new_object();
    let target = obj.id().addr();

    let weak = WeakObj::from_obj(&obj);
    let registered = weak.slot_addr();

    // 1. a plain rebinding.
    let weak = weak;
    // 2. into a struct field at a non-zero offset.
    let holder = Holder {
        weak,
        _before: [0; 3],
    };
    // 3. the whole struct into a Vec, which will reallocate under it.
    let mut v = Vec::with_capacity(1);
    v.push(holder);
    for _ in 0..8 {
        v.push(Holder {
            weak: WeakObj::empty(),
            _before: [0; 3],
        });
    }
    assert!(v.capacity() > 1, "the Vec grew, so its buffer moved");
    // 4. back out of the Vec.
    let holder = v.swap_remove(0);
    // 5. into a function and 6. back out of it.
    let weak = round_trip(holder.weak);
    // and onto the heap for good measure.
    let weak = *Box::new(weak);

    assert_eq!(
        weak.slot_addr(),
        registered,
        "THE PROPERTY: six moves of the HANDLE and the registered ADDRESS is \
         unchanged, because the slot never moved — only the box pointer did"
    );
    assert!(weak.is_live(), "the target is still alive");
    assert_eq!(
        weak.load().expect("live").id().addr(),
        target,
        "and the moved handle still resolves to the same object"
    );

    // Now kill it.
    drop(obj);

    assert!(
        !weak.is_live(),
        "after the last release the moved handle must report gone"
    );
    assert!(
        weak.load().is_none(),
        "and `load` must answer None, not a dangling pointer"
    );
    // SAFETY: a plain word read of the moved handle's slot; not dereferenced.
    let raw = unsafe { weak.peek() };
    assert!(
        raw.is_null(),
        "this slot reads nil ({raw:?}), because the address the runtime \
         registered is the address the handle still owns"
    );
}

// Ordinary behaviour

/// `load` is +1, and the +1 is what keeps the object alive across the drop of
/// every other strong reference.
#[test]
fn load_hands_back_a_plus_one_that_outlives_the_original_owner() {
    let weak;
    let held;
    {
        let obj = new_object();
        weak = WeakObj::from_obj(&obj);
        held = weak.load().expect("live while `obj` is held");
        assert_eq!(held.id().addr(), obj.id().addr());
        drop(obj);
    }
    // `held` owns a +1, so the object survives `obj`'s drop and the weak
    // reference still resolves.
    assert!(
        weak.is_live(),
        "the +1 from `load` is a strong reference and keeps the target alive"
    );
    let addr = held.id().addr();
    drop(held);
    assert!(!weak.is_live(), "and releasing it is the last release");
    assert_ne!(addr, 0);
}

/// The raw +0 load (`objc_loadWeak`, `unsafe`, no safe wrapper) returns the same
/// object, kept alive by the pool open at the call (the innermost one, pushed
/// by this frame).
#[test]
fn the_raw_plus_zero_load_answers_inside_the_pool_that_is_open() {
    let obj = new_object();
    let slot = WeakSlot::uninit();
    // SAFETY: `slot` is freshly `uninit()`, is a local that does not move
    // before `destroy` below, and `obj` holds a live +1.
    unsafe { slot.init(obj.id()) };

    let seen = autoreleasepool(|_pool| {
        // SAFETY: `slot` is initialised and unmoved; the +0 answer is read
        // inside the pool that is innermost right now, so it is alive here.
        let id = unsafe { slot.load_autoreleased() };
        assert_eq!(id.addr(), obj.id().addr());
        id.addr()
    });
    assert_eq!(seen, obj.id().addr());

    drop(obj);
    let gone = autoreleasepool(|_pool| {
        // SAFETY: as above; a dead target answers nil rather than a corpse.
        unsafe { slot.load_autoreleased() }.is_null()
    });
    assert!(gone, "a dead target answers nil through the +0 load too");

    // SAFETY: `slot` was initialised above and has not moved; leaving it
    // registered would be the wild write this file's other tests measure.
    unsafe { slot.destroy() };
}

/// `objc_storeWeak` re-points a slot without moving it.
#[test]
fn store_repoints_the_same_slot_at_a_new_target() {
    let first = new_object();
    let second = new_mutable_string();

    let mut weak = WeakObj::from_obj(&first);
    let addr = weak.slot_addr();
    assert_eq!(weak.load().expect("live").id().addr(), first.id().addr());

    // SAFETY: `second` is a live +1 reference held by this frame.
    unsafe { weak.store(second.id()) };

    assert_eq!(
        weak.slot_addr(),
        addr,
        "a store does not relocate the registration"
    );
    assert_eq!(weak.load().expect("live").id().addr(), second.id().addr());

    // The first object is no longer named by this slot, so its death must not
    // touch it.
    drop(first);
    assert_eq!(
        weak.load().expect("still live").id().addr(),
        second.id().addr(),
        "the old target's dealloc must not nil a slot it no longer owns"
    );

    drop(second);
    assert!(weak.load().is_none());

    // And a store back to nil empties it.
    // SAFETY: nil is an explicitly legal `objc_storeWeak` value.
    unsafe { weak.store(Id::NIL) };
    assert!(weak.load().is_none());
}

/// `objc_copyWeak` — a second registration at a second address, both valid,
/// both nil'd.
#[test]
fn clone_weak_registers_a_second_address_and_both_are_nild() {
    let obj = new_object();
    let a = WeakObj::from_obj(&obj);
    let b = a.clone_weak();

    assert_ne!(
        a.slot_addr(),
        b.slot_addr(),
        "a copy is a SECOND slot; if it shared one, destroying either would \
         unregister both"
    );
    assert_eq!(
        a.load().expect("live").id().addr(),
        b.load().expect("live").id().addr()
    );

    // Dropping one must not disturb the other — the failure mode a shared slot
    // would have.
    drop(b);
    assert!(
        a.is_live(),
        "destroying the copy left the original registered"
    );

    let c = a.clone_weak();
    drop(obj);
    assert!(!a.is_live(), "both registrations are nil'd on dealloc");
    assert!(!c.is_live());
}

/// `WeakObj::drop` unregisters the address before the box is freed; otherwise
/// the runtime would later write `nil` into whatever the allocator handed out.
///
/// [`Quarantining`], this binary's global allocator, leaks the one `dealloc`
/// inside `drop(weak)`, so the eight bytes stay allocated and owned by this
/// test. Racing the allocator for reuse is not reliable (per-CPU magazines,
/// and the runtime's weak table allocates in the same size class).
///
/// The sentinel is the target's own pointer: libobjc declines to write through
/// a weak slot holding any other value, which would make the test vacuous.
#[test]
fn dropping_the_handle_unregisters_before_the_box_is_freed() {
    let obj = new_object();
    let target = obj.id();

    let weak = WeakObj::from_obj(&obj);
    // Expose the address before the box dies and re-derive it afterwards: the
    // block is still allocated (quarantined), but the `Box`'s provenance
    // ended, so `with_exposed_provenance_mut` builds a fresh pointer.
    let slot = weak.slot_addr().expose_provenance();

    quarantine(true);
    drop(weak);
    quarantine(false);

    // The block is now this test's, permanently. Fill it as a squatter would.
    let slot: *mut Id = std::ptr::with_exposed_provenance_mut(slot);
    // SAFETY: the quarantine leaked this block, so the eight bytes are still
    // allocated, pointer-aligned, and reachable through no other pointer.
    unsafe { *slot = target };

    // If the registration outlived `Drop`, the runtime writes nil through this address now.
    drop(obj);

    // SAFETY: as above — the block is leaked and therefore still live.
    let after = unsafe { *slot };
    assert_eq!(
        after.addr(),
        target.addr(),
        "the runtime wrote through {slot:p} after the handle that registered \
         it was dropped. `WeakObj::drop` did not call `objc_destroyWeak`, so \
         the weak table still held memory the program had released — in a real \
         process that is a nil written into whatever the allocator handed out \
         next. It reads {after:?}"
    );
}

// The quarantining allocator

/// This binary's global allocator: [`std::alloc::System`], except that a thread
/// which has armed [`quarantine`] leaks its deallocations, so a test can read
/// the bytes a `Box` released while they are still allocated.
///
/// The flag is thread-local, so other tests are unaffected.
struct Quarantining;

thread_local! {
    /// Armed only around the one `drop` that matters. `const` init, since a
    /// lazily initialised TLS would allocate on first touch inside `dealloc`.
    static QUARANTINE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Arm or disarm the quarantine for this thread.
fn quarantine(on: bool) {
    QUARANTINE.with(|q| q.set(on));
}

// SAFETY: every method forwards to `System`, except that `dealloc` may decline
// to free a block. Leaking is permitted; no block is freed twice, handed back
// while live, or returned to a different allocator.
unsafe impl std::alloc::GlobalAlloc for Quarantining {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract, which is
        // passed through unchanged.
        unsafe { std::alloc::System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        // `try_with`, not `with`: TLS is unavailable during thread teardown,
        // and a `dealloc` there must still free rather than panic.
        if QUARANTINE.try_with(std::cell::Cell::get).unwrap_or(false) {
            return; // leaked on purpose; see the type docs.
        }
        // SAFETY: `ptr`/`layout` came from this allocator's `alloc`, which
        // forwards to `System`.
        unsafe { std::alloc::System.dealloc(ptr, layout) };
    }

    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        // SAFETY: contract passed through unchanged.
        unsafe { std::alloc::System.alloc_zeroed(layout) }
    }

    // `realloc` uses the default (alloc + copy + `self.dealloc`) so a growing
    // `Vec` goes through the quarantine too; `System::realloc` would `free`
    // behind this type's back.
}

#[global_allocator]
static ALLOC: Quarantining = Quarantining;
