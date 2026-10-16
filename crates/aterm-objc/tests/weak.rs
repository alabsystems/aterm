// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Weak references: the storage location, not the value, is what the runtime
//! registers.
//!
//! `objc_initWeak(location, obj)` records the address in `obj`'s side table,
//! and on dealloc the runtime writes `nil` through every recorded address. Move
//! the bytes and the copy is never zeroed while the old address stays a live
//! write target. These tests measure that on real objects:
//!
//! * [`a_memcpy_moved_weak_slot_reads_a_dangling_pointer_and_the_runtime_nils_the_old_address`]:
//!   the naive inline-slot move.
//! * [`objc_move_weak_is_what_a_move_would_have_to_be`]: the sanctioned
//!   relocation, with the opposite result.
//! * [`a_boxed_weak_survives_every_move_rust_can_perform`] and
//!   [`the_slot_the_runtime_nils_is_the_boxed_one`]: a `WeakObj` keeps one
//!   registered address across every kind of move.
//! * [`destroy_removes_the_registration_and_the_runtime_leaves_the_address_alone`]
//!   and its positive control [`the_wild_write_a_missing_destroy_performs`].
//!
//! Foundation objects (`NSObject`, `NSMutableString`) have real refcounts and
//! weak tables; libtest cannot host AppKit, so `objc_window_drive.rs` repeats
//! the experiments against a live `NSView`.

#![cfg(target_os = "macos")]

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

// ---------------------------------------------------------------------------
// The hazard
// ---------------------------------------------------------------------------

/// Why `WeakObj` boxes its slot: after a byte copy of a registered slot (what
/// Rust's move of an inline slot would emit), the runtime nils the original
/// address and the copy keeps the dead object's pointer.
#[test]
fn a_memcpy_moved_weak_slot_reads_a_dangling_pointer_and_the_runtime_nils_the_old_address() {
    let a = WeakSlot::uninit();
    let b = WeakSlot::uninit();

    let target_addr;
    {
        let obj = new_object();
        target_addr = obj.id().addr();

        // SAFETY: `a` is fresh, does not move for the rest of the function, and
        // `obj` is a live +1 this frame holds.
        unsafe { a.init(obj.id()) };

        // SAFETY: reading the registered slot's word; the pointer is live here
        // because `obj` still holds a strong reference.
        assert_eq!(
            unsafe { a.peek() }.addr(),
            target_addr,
            "a names the object"
        );

        // The naive move: a one-word `copy_nonoverlapping`, as a `memcpy` move
        // of `struct Weak { slot: Id }` would compile to.
        //
        // SAFETY: both slots are `#[repr(transparent)]` over one pointer word,
        // live and non-overlapping. This deliberately creates the unsound state
        // under test: `a` registered, `b` unregistered with a pointer the
        // runtime will never update.
        unsafe { std::ptr::copy_nonoverlapping(a.addr(), b.addr(), 1) };

        // SAFETY: same read; the copy is still live because `obj` is.
        assert_eq!(
            unsafe { b.peek() }.addr(),
            target_addr,
            "the memcpy'd slot holds the same word — which is exactly why it \
             looks correct right up until the object dies"
        );

        drop(obj); // the last release: `dealloc` runs here.
    }

    // SAFETY: reading two pointer words; neither is dereferenced, messaged or
    // retained (`b`'s is dangling).
    let after_a = unsafe { a.peek() };
    // SAFETY: as above.
    let after_b = unsafe { b.peek() };

    assert!(
        after_a.is_null(),
        "the runtime is supposed to nil the address it registered; it read {after_a:?}"
    );
    assert_eq!(
        after_b.addr(),
        target_addr,
        "THE FINDING: the memcpy'd slot still holds the dead object's address \
         ({target_addr:#x}). A type with the slot inline would hand this \
         pointer to the next caller of `load`, which would message freed \
         memory. It reads {after_b:?}"
    );
    assert!(
        !after_b.is_null(),
        "a memcpy'd weak slot is never nil'd, because the runtime never knew \
         about it"
    );

    // SAFETY: `a` is initialised and not destroyed; destroying it keeps the
    // runtime from holding this stack address. `b` was never registered, so it
    // is not destroyed.
    unsafe { a.destroy() };
}

/// The same experiment through `objc_moveWeak`: the new slot is registered and
/// reads nil after the target dies; the old one is inert.
#[test]
fn objc_move_weak_is_what_a_move_would_have_to_be() {
    let from = WeakSlot::uninit();
    let to = WeakSlot::uninit();

    {
        let obj = new_mutable_string();
        // SAFETY: `from` is freshly `uninit()`, immovable for this frame, and
        // `obj` is a live +1.
        unsafe { from.init(obj.id()) };
        // SAFETY: `to` is fresh, `from` initialised, both immovable locals.
        // Afterwards `from` is unregistered and must not be destroyed.
        unsafe { to.move_from(&from) };

        // SAFETY: reading the word while `obj` still holds a strong reference.
        assert_eq!(
            unsafe { to.peek() }.addr(),
            obj.id().addr(),
            "the relocation carried the target across"
        );
        // SAFETY: `to` is the registered slot now; the load is +1.
        let live = unsafe { Obj::from_owned(to.load_retained()) };
        assert!(live.is_some(), "the relocated slot still resolves");
        assert_eq!(
            live.expect("just checked").id().addr(),
            obj.id().addr(),
            "and to the same object"
        );

        drop(obj);
    }

    // SAFETY: two plain word reads, neither dereferenced.
    let after_to = unsafe { to.peek() };
    // SAFETY: as above. `from` was unregistered by `move_from`, so its word is
    // whatever `objc_moveWeak` left there — the assertion below is about `to`.
    let after_from = unsafe { from.peek() };

    assert!(
        after_to.is_null(),
        "the RELOCATED slot is the one the runtime nils; it read {after_to:?}"
    );
    let _ = after_from;

    // SAFETY: `to` is the initialised slot; `from` was unregistered by
    // `move_from` and must NOT be destroyed a second time.
    unsafe { to.destroy() };
}

// ---------------------------------------------------------------------------
// The safe handle
// ---------------------------------------------------------------------------

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
        "THE CONTRAST WITH THE MEMCPY TEST: this slot reads nil ({raw:?}), \
         because the address the runtime registered is the address the handle \
         still owns"
    );
}

/// The address the runtime nil'd is the same eight bytes the moved handle uses.
#[test]
fn the_slot_the_runtime_nils_is_the_boxed_one() {
    let obj = new_mutable_string();
    let weak = WeakObj::from_obj(&obj);
    let addr_before_moves = weak.slot_addr();

    let moved = round_trip(round_trip(weak));
    assert_eq!(moved.slot_addr(), addr_before_moves);

    // SAFETY: a word read while the object is still alive.
    assert_eq!(unsafe { moved.peek() }.addr(), obj.id().addr());

    drop(obj);

    // SAFETY: the same eight bytes, read again. The runtime wrote through this
    // exact address during `dealloc`.
    let word = unsafe { *moved.slot_addr() };
    assert!(
        word.is_null(),
        "the runtime's nil landed at {addr_before_moves:p}, which is the \
         address this handle has held since before any move"
    );
}

// ---------------------------------------------------------------------------
// Ordinary behaviour
// ---------------------------------------------------------------------------

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

/// A signature taking an `AutoreleasePool` returns nothing with a lifetime in it, checked across
/// this crate, `aterm-gui` and the winit fork.
///
/// A pool reference proves that some pool is open, not that it is the innermost one the runtime
/// autoreleases into, so it may gate an operation but never mint a lifetime. Rejected: a reference
/// (`&`), any lifetime token (`'`), and `impl`/`dyn` returns that can capture one. A
/// lifetime-carrying wrapper has no `&` in its return type:
///
/// ```text
/// pub struct Borrowed<'p> { id: Id, _pool: PhantomData<&'p AutoreleasePool> }
/// pub fn load_borrowed_v2<'p>(&self, _pool: &'p AutoreleasePool)
///     -> Option<Borrowed<'p>>
/// ```
///
/// Aliasing the pool type (`type Pool = AutoreleasePool;`) is rejected too, since the check
/// matches the type name. The non-empty assertion shows the parser still finds signatures
/// (currently only `autoreleasepool`).
#[test]
fn a_pool_parameter_never_mints_a_lifetime() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let trees = [
        "crates/aterm-objc/src",
        "crates/aterm-gui/src",
        "crates/aterm-window/src/platform_impl/macos",
    ];

    let mut scanned = 0usize;
    let mut gated = 0usize;
    let mut offenders = Vec::new();
    for tree in trees {
        let dir = root.join(tree);
        assert!(
            dir.is_dir(),
            "{tree} is not a directory — the scope is stale"
        );
        for file in rs_files(&dir) {
            let src = std::fs::read_to_string(&file).expect("readable");
            scanned += 1;
            for (line, sig) in fn_signatures(&src) {
                let Some((params, ret)) = split_signature(&sig) else {
                    continue;
                };
                if !params.contains("AutoreleasePool") {
                    continue;
                }
                gated += 1;
                // No lifetime may leave: `&`, a `PhantomData<&'p …>` wrapper, or
                // an `impl`/`dyn` return that captures one.
                if let Some(why) = leaks_a_lifetime(&ret) {
                    offenders.push(format!(
                        "{}:{line}: [{why}] {sig}",
                        file.strip_prefix(&root).unwrap_or(&file).display()
                    ));
                }
            }
            // The check matches the type name, so renaming the type is refused.
            for (line, text) in aliases_of_the_pool(&src) {
                offenders.push(format!(
                    "{}:{line}: [aliases AutoreleasePool, which hides every \
                     signature above from this check] {text}",
                    file.strip_prefix(&root).unwrap_or(&file).display()
                ));
            }
        }
    }

    assert!(scanned > 30, "the walk found only {scanned} files");
    assert!(
        gated > 0,
        "no signature takes an AutoreleasePool at all — the check is vacuous \
         and the rule it encodes has stopped being checkable"
    );
    assert!(
        offenders.is_empty(),
        "a pool parameter is minting a lifetime, which is `load_borrowed`'s \
         defect returning:\n  {}",
        offenders.join("\n  ")
    );
}

/// Why `ret` carries a lifetime out, or `None` if it cannot.
fn leaks_a_lifetime(ret: &str) -> Option<&'static str> {
    if ret.contains('&') {
        return Some("returns a reference");
    }
    // A lifetime token: `'p`, `'_`, `'static`. `'static` cannot be told from
    // `'p` without resolving generics, and a pool-gated fn should return
    // neither.
    let b: Vec<char> = ret.chars().collect();
    for (i, c) in b.iter().enumerate() {
        if *c != '\'' {
            continue;
        }
        // Not a char literal: `'a'` has a closing quote two chars on.
        let is_char_lit = b.get(i + 2) == Some(&'\'');
        if !is_char_lit && b.get(i + 1).is_some_and(|n| n.is_alphabetic() || *n == '_') {
            return Some("returns a named or elided lifetime");
        }
    }
    if ret.contains("impl ") || ret.contains("dyn ") {
        return Some("returns impl/dyn, which can capture a lifetime unwritten");
    }
    None
}

/// `(1-based line, text)` for every alias of `AutoreleasePool` in `src`.
fn aliases_of_the_pool(src: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (i, raw) in src.lines().enumerate() {
        let line = raw.split("//").next().unwrap_or("");
        // Match the RHS however it is pathed (`AutoreleasePool`,
        // `crate::AutoreleasePool`, `aterm_objc::AutoreleasePool`).
        let aliased = (line.contains("type ")
            && line.contains('=')
            && line
                .split('=')
                .nth(1)
                .is_some_and(|r| r.contains("AutoreleasePool")))
            || (line.contains("use ") && line.contains("AutoreleasePool as "));
        if aliased {
            out.push((i + 1, line.trim().to_owned()));
        }
    }
    out
}

/// Every `*.rs` under `dir`, recursively.
fn rs_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).expect("readable dir").flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    out
}

/// `(1-based line, signature text)` for every `fn` in `src`, comments stripped.
///
/// A signature runs from `fn` to the `{`, `;` or `where` ending it, newlines
/// flattened so multi-line signatures are caught. Lines are split once
/// (re-splitting per `fn` is quadratic on large files).
fn fn_signatures(src: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = Vec::new();
    for (i, raw) in lines.iter().enumerate() {
        let line = raw.split("//").next().unwrap_or("");
        let Some(at) = line.find("fn ") else { continue };
        if at > 0
            && !line[..at]
                .chars()
                .all(|c| c.is_whitespace() || c.is_alphanumeric() || c == '_')
        {
            continue;
        }
        // Accumulate forward until the signature closes.
        let mut sig = String::new();
        let mut depth = 0i32;
        let mut started = false;
        for later in &lines[i..] {
            let text = later.split("//").next().unwrap_or("");
            for c in text.chars() {
                match c {
                    '(' | '<' | '[' => depth += 1,
                    ')' | '>' | ']' => depth -= 1,
                    '{' | ';' if depth <= 0 => {
                        started = true;
                    }
                    _ => {}
                }
                if started {
                    break;
                }
                sig.push(c);
            }
            if started {
                break;
            }
            sig.push(' ');
        }
        let sig = sig[sig.find("fn ").unwrap_or(0)..]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if !sig.is_empty() {
            out.push((i + 1, sig));
        }
    }
    out
}

/// Split a flattened signature into `(parameter list, return type)`.
fn split_signature(sig: &str) -> Option<(String, String)> {
    let open = sig.find('(')?;
    let mut depth = 0i32;
    let mut close = None;
    for (k, c) in sig.char_indices().skip(open) {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(k);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let params = sig[open + 1..close].to_string();
    let ret = sig[close + 1..]
        .split_once("->")
        .map(|(_, r)| r.trim().to_string())
        .unwrap_or_default();
    Some((params, ret))
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

/// An empty weak reference is legal, loads as `None`, and can be filled later —
/// the state a `Default`-shaped ivar needs.
#[test]
fn an_empty_weak_is_legal_and_can_be_filled() {
    let mut weak = WeakObj::empty();
    assert!(weak.load().is_none());
    assert!(!weak.is_live());

    let obj = new_object();
    // SAFETY: `obj` is a live +1 held by this frame; the slot was initialised
    // with nil, which `objc_storeWeak` accepts as an initialised slot.
    unsafe { weak.store(obj.id()) };
    assert_eq!(weak.load().expect("live").id().addr(), obj.id().addr());
    drop(obj);
    assert!(weak.load().is_none());
}

/// A weak reference does not keep its target alive.
#[test]
fn a_weak_reference_does_not_retain() {
    let obj = new_object();
    // SAFETY: `-retainCount` is `q16@0:8`. Only a difference between two reads
    // is asserted.
    let count = |id: Id| unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> isize = msg();
        f(id, sel!(retainCount))
    };

    let before = count(obj.id());
    let w1 = WeakObj::from_obj(&obj);
    let w2 = w1.clone_weak();
    let after = count(obj.id());
    assert_eq!(
        before, after,
        "two weak references changed the retain count from {before} to \
         {after}; a weak reference that retains is a strong reference with a \
         misleading name"
    );

    // And the strong load DOES move it.
    let strong = w1.load().expect("live");
    assert_eq!(
        count(obj.id()),
        before + 1,
        "`load` is documented +1; if this is unchanged the load is handing out \
         borrowed pointers"
    );
    drop(strong);
    drop(w2);
    assert_eq!(count(obj.id()), before);
}

/// `objc_destroyWeak` removes the address: register a local slot, destroy it,
/// fill it with a sentinel, kill the target, and the sentinel survives.
///
/// The sentinel is the target's own pointer, which the runtime's consistency
/// check accepts (see [`the_wild_write_a_missing_destroy_performs`]); any other
/// value would make the check vacuous.
#[test]
fn destroy_removes_the_registration_and_the_runtime_leaves_the_address_alone() {
    let slot = WeakSlot::uninit();
    let obj = new_object();
    let sentinel = obj.id();

    // SAFETY: `slot` is freshly `uninit()`, is a local that does not move for
    // the rest of this function, and `obj` is a live +1 this frame holds.
    unsafe { slot.init(sentinel) };
    // SAFETY: `slot` is initialised and is not used as an initialised slot
    // again; after this the runtime holds no pointer into these bytes.
    unsafe { slot.destroy() };

    // Re-fill the now-unregistered storage by hand.
    // SAFETY: `addr()` is a valid, aligned pointer to this local's pointer
    // word, which nothing aliases now the registration is gone.
    unsafe { *slot.addr() = sentinel };

    drop(obj);

    // SAFETY: a plain word read of a local; the value is a dangling pointer and
    // is only compared, never dereferenced.
    let after = unsafe { slot.peek() };
    assert_eq!(
        after.addr(),
        sentinel.addr(),
        "the runtime wrote through an address it had been told to forget: the \
         slot at {:p} was destroyed before the target died, so `dealloc` must \
         not have touched it. It reads {after:?}",
        slot.addr()
    );
}

/// Without the destroy, the runtime zeroes the still-registered slot when the
/// target dies. The positive control for the test above.
#[test]
fn the_wild_write_a_missing_destroy_performs() {
    let slot = WeakSlot::uninit();
    {
        let obj = new_object();
        // SAFETY: `slot` is freshly `uninit()` and immovable for this function;
        // `obj` is a live +1.
        unsafe { slot.init(obj.id()) };
        drop(obj);
    }
    // Deliberately NOT destroyed before the target died.
    // SAFETY: a plain word read of a local.
    let after = unsafe { slot.peek() };
    assert!(
        after.is_null(),
        "the runtime is expected to write nil through every registered address \
         on dealloc; it left {after:?}, which would mean the two tests either \
         side of this one are measuring nothing"
    );
    // SAFETY: `slot` is still an initialised (now nil) registration and is
    // destroyed exactly once, before this frame dies.
    unsafe { slot.destroy() };
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

// ---------------------------------------------------------------------------
// The quarantining allocator
// ---------------------------------------------------------------------------

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
