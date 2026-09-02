// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Weak references, and the ONE property that makes them a capability rather
//! than a wrapper: **the storage location is what is registered.**
//!
//! Every other handle in `aterm-objc` is a value. `Obj`, `Retained<T>` and
//! `RcBlock` all hold a word that means the same thing wherever it is stored,
//! so Rust's move — a `memcpy` of the bytes plus a `mem::forget` of the source
//! — is exactly right for them, and no test here needs to say so.
//!
//! A weak reference is not a value. `objc_initWeak(location, obj)` records
//! `location` — the ADDRESS — in the runtime's side table for `obj`, and on
//! dealloc the runtime writes `nil` through every address it holds. Move the
//! bytes and the runtime still holds the old address; the copy is never zeroed
//! and the old address is still a live write target.
//!
//! This file MEASURES that, on real Objective-C objects, rather than asserting
//! it from the ABI:
//!
//! * [`a_memcpy_moved_weak_slot_reads_a_dangling_pointer_and_the_runtime_nils_the_old_address`]
//!   performs the naive inline-slot move and reads BOTH slots after the target
//!   dies. This is the hazard, in five lines.
//! * [`objc_move_weak_is_what_a_move_would_have_to_be`] performs the sanctioned
//!   relocation and reads both slots again. Same shape, opposite answer.
//! * [`a_boxed_weak_survives_every_move_rust_can_perform`] moves a `WeakObj`
//!   six ways — let-binding, struct field, `Vec` reallocation, function
//!   argument, function return, `Box` — and shows the registered address never
//!   changes and the reference keeps working.
//! * [`the_slot_the_runtime_nils_is_the_boxed_one`] closes the loop: after the
//!   target dies, the address the runtime nil'd IS the address the moved handle
//!   is still using.
//! * [`the_wild_write_a_missing_destroy_performs`] and
//!   [`destroy_removes_the_registration_and_the_runtime_leaves_the_address_alone`]
//!   are the other end of the same property, at an address this file OWNS: a
//!   registration left behind is a write the runtime performs into memory the
//!   program has moved on from, and `objc_destroyWeak` is what withdraws it.
//!   They are a matched pair — the second is the guard, the first is the
//!   positive control that keeps the guard from going quiet.
//!
//! # Why `NSObject` and `NSMutableString`, and not AppKit
//!
//! libtest cannot host AppKit — there is no `NSApplication`, no run loop and no
//! window server in a test binary. Foundation is a real Objective-C runtime
//! with real reference counting and real weak tables, which is everything these
//! properties are about. The AppKit half is `objc_window_drive.rs`, which runs
//! the same two experiments against a live `NSView` inside a real event loop.

#![cfg(target_os = "macos")]

use aterm_objc::{Id, Obj, Sel, WeakObj, WeakSlot, autoreleasepool, class, msg, sel};

/// `[[NSObject alloc] init]`, +1 and owned.
///
/// The smallest object with a real refcount: it is not interned, not cached and
/// not autoreleased by anything, so `drop` really is the last release and
/// `dealloc` really does run.
fn new_object() -> Obj {
    // SAFETY: `+alloc` is `@16#0:8` and `-init` is `@16@0:8` on `NSObject`;
    // the pair yields a +1 reference this `Obj` adopts.
    unsafe {
        let alloc: unsafe extern "C" fn(Id, Sel) -> Id = msg();
        let init: unsafe extern "C" fn(Id, Sel) -> Id = msg();
        let raw = alloc(class(c"NSObject").as_id(), sel!(alloc));
        Obj::from_owned(init(raw, sel!(init))).expect("a fresh NSObject")
    }
}

/// `[[NSMutableString alloc] init]`, +1 — a second class, so the properties are
/// not accidentally about `NSObject`'s own `dealloc`.
fn new_mutable_string() -> Obj {
    // SAFETY: `+alloc`/`-init` on `NSMutableString`, same prototypes.
    unsafe {
        let alloc: unsafe extern "C" fn(Id, Sel) -> Id = msg();
        let init: unsafe extern "C" fn(Id, Sel) -> Id = msg();
        let raw = alloc(class(c"NSMutableString").as_id(), sel!(alloc));
        Obj::from_owned(init(raw, sel!(init))).expect("a fresh NSMutableString")
    }
}

// ---------------------------------------------------------------------------
// THE HAZARD, MEASURED
// ---------------------------------------------------------------------------

/// The whole reason `WeakObj` boxes its slot.
///
/// Two slots. `a` is registered with the runtime; `b` receives a byte-for-byte
/// copy of `a`'s contents — which is precisely what Rust's move of a type with
/// the slot INLINE would emit, and Rust has no move constructor with which to
/// do anything else. Then the target dies.
///
/// If a weak reference were a value, `b` would behave like `a`. It does not:
/// the runtime nils `a` because `a`'s ADDRESS is what it recorded, and `b` is
/// left holding the dead object's pointer, non-nil, forever.
#[test]
fn a_memcpy_moved_weak_slot_reads_a_dangling_pointer_and_the_runtime_nils_the_old_address() {
    let a = WeakSlot::uninit();
    let b = WeakSlot::uninit();

    let target_addr;
    {
        let obj = new_object();
        target_addr = obj.id().addr();

        // SAFETY: `a` is freshly `uninit()`, does not move for the rest of this
        // function (it is a local that is borrowed to the end), and `obj` is a
        // live +1 reference this frame holds.
        unsafe { a.init(obj.id()) };

        // SAFETY: reading the registered slot's word; the pointer is live here
        // because `obj` still holds a strong reference.
        assert_eq!(
            unsafe { a.peek() }.addr(),
            target_addr,
            "a names the object"
        );

        // THE NAIVE MOVE. `copy_nonoverlapping` of one pointer word is exactly
        // what `memcpy`-moving a `struct Weak { slot: Id }` compiles to; the
        // runtime is not told, because there is nothing in Rust that could tell
        // it.
        //
        // SAFETY: both slots are `#[repr(transparent)]` over one pointer word,
        // both are live locals, and they do not overlap. This deliberately
        // leaves the program in the UNSOUND state the test exists to measure:
        // `a` is registered and about to go out of scope, `b` is unregistered
        // and holds a pointer the runtime will never update.
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

    // SAFETY: reading two pointer WORDS. Neither is dereferenced, messaged or
    // retained — `b`'s is a dangling pointer to freed memory and reading its
    // value is the only thing this test does with it.
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

    // SAFETY: `a` is initialised and has not been destroyed; destroying it now
    // is what keeps the runtime from holding this stack address after the frame
    // dies. `b` is deliberately NOT destroyed — it was never registered, and
    // calling `objc_destroyWeak` on an unregistered slot is not sanctioned.
    unsafe { a.destroy() };
}

/// The same experiment with the runtime told, which is the whole difference.
///
/// `objc_moveWeak` registers the new address and unregisters the old one under
/// the weak lock, so after the target dies it is the NEW slot that reads nil
/// and the old one that is inert.
#[test]
fn objc_move_weak_is_what_a_move_would_have_to_be() {
    let from = WeakSlot::uninit();
    let to = WeakSlot::uninit();

    {
        let obj = new_mutable_string();
        // SAFETY: `from` is freshly `uninit()`, immovable for this frame, and
        // `obj` is a live +1.
        unsafe { from.init(obj.id()) };
        // SAFETY: `to` is freshly `uninit()`, `from` is initialised, and both
        // are immovable locals. After this `from` is UNREGISTERED and must not
        // be destroyed.
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
// THE SAFE HANDLE
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

/// Six real moves, one registration.
///
/// The task's proof obligation, in its own words: *allocate, weak-store, move
/// the container, release the object, and show the moved slot reads nil rather
/// than a dangling pointer.*
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

/// The two halves joined: the address the runtime nil'd is the address the
/// moved handle uses.
///
/// The previous test shows the handle survives moves and the slot reads nil.
/// This one shows they are the SAME eight bytes, which is what rules out "some
/// other slot happened to be nil".
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
// THE ORDINARY BEHAVIOUR
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
    // `obj` is gone but `held` owns a +1, so the object is NOT deallocated and
    // the weak reference still resolves. If `load` returned +0 this would be a
    // use-after-free that no test could see.
    assert!(
        weak.is_live(),
        "the +1 from `load` is a strong reference and keeps the target alive"
    );
    let addr = held.id().addr();
    drop(held);
    assert!(!weak.is_live(), "and releasing it is the last release");
    assert_ne!(addr, 0);
}

/// The RAW +0 load still works — and there is no safe wrapper over it.
///
/// `WeakObj::load_borrowed` used to be tested here, taking a
/// `&AutoreleasePool` and handing back a borrow tied to it. **It was UNSOUND**
/// (`weak.rs`'s module docs carry the counterexample and its SIGSEGV) and it is
/// deleted. `objc_loadWeak` itself is a real runtime entry point and stays
/// bound as `unsafe`, so what this test can honestly check is what the ABI
/// does: the pointer that comes back is the same object, and it is kept alive
/// by whichever pool was open AT THE CALL — which is the pool this frame just
/// pushed, because this frame is the innermost one.
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

/// THE RULE THE WITHDRAWAL LEAVES BEHIND, enforced against the source.
///
/// A `&AutoreleasePool` parameter proves that *a* pool is open. It does NOT
/// prove that *that* pool is the one the runtime will autorelease into —
/// `objc_loadWeak` and every `+0` framework return land in the innermost pool
/// ON THE THREAD, which is a runtime fact no parameter can name. So a pool
/// reference may GATE an operation and must never appear in the returned
/// lifetime. `load_borrowed` broke exactly this rule and looked entirely
/// reasonable doing it, which is why the rule is a test and not a comment.
///
/// # THE RULE IS "NO LIFETIME", NOT "NO `&`" — and the first spelling of it was
/// # blind, which the THIRTEENTH pass proved with a run
///
/// This check used to reject only a return type containing `&`. That is not the
/// rule; it is one spelling of the rule, and `load_borrowed` has another that
/// the check could not see. MEASURED, not argued:
///
/// ```text
/// pub struct Borrowed<'p> { id: Id, _pool: PhantomData<&'p AutoreleasePool> }
/// pub fn load_borrowed_v2<'p>(&self, _pool: &'p AutoreleasePool)
///     -> Option<Borrowed<'p>>
/// ```
///
/// No `&` appears in that return type, so the old check passed it — and it is
/// `load_borrowed` in every way that mattered. Fired as the W9 counterexample:
/// after the inner pool pops the weak slot reads `id(nil)` (the object IS
/// deallocated) while safe code still holds `0x10504dce0`, and
/// `-retainCount` on that borrow **SIGSEGVs (signal 11)**. Same defect, same
/// crash, guard green. That is P3-1's lesson — a guard armed at the wrong
/// spelling is not a guard — recurring inside F1's own fix.
///
/// So the rule enforced here is the one that was always meant: a signature
/// taking an `AutoreleasePool` must return something with **no lifetime in it
/// at all**. Four spellings are rejected, and each is a way to carry `'p` out:
/// a reference (`&`), any named or elided lifetime token (`'`), and `impl` /
/// `dyn` returns, which can capture a lifetime without writing one.
///
/// A second hole is closed with it: the check matched the TYPE NAME, so
/// `type Pool = AutoreleasePool;` would have hidden every signature from it.
/// Aliasing the type is now rejected outright, in the same trees.
///
/// It is PLANT-VERIFIED in all three shapes — the elided reference
/// (`-> Option<&Obj>`, split across four lines), the lifetime-carrying wrapper
/// above, and the alias — each failing this test naming the file and line.
///
/// Today exactly ONE signature in the three trees is gated — `autoreleasepool`
/// itself, whose `R` is not a reference — so the non-empty assertion below is
/// a canary that the parser still finds signatures at all, not a claim that the
/// tree is full of pool arguments.
#[test]
fn a_pool_parameter_never_mints_a_lifetime() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let trees = [
        "crates/aterm-objc/src",
        "crates/aterm-gui/src",
        "vendor/winit/src/platform_impl/macos",
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
                // NO LIFETIME MAY LEAVE. `&` is only the first spelling; a
                // wrapper carrying `PhantomData<&'p …>`, and an `impl`/`dyn`
                // return that captures one without writing it, are the others.
                if let Some(why) = leaks_a_lifetime(&ret) {
                    offenders.push(format!(
                        "{}:{line}: [{why}] {sig}",
                        file.strip_prefix(&root).unwrap_or(&file).display()
                    ));
                }
            }
            // A guard that matches a TYPE NAME is defeated by renaming the
            // type, so the rename is refused rather than chased.
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
///
/// Each arm is a way `load_borrowed`'s defect can be spelled; the `&` arm is
/// the only one the first version of this check saw.
fn leaks_a_lifetime(ret: &str) -> Option<&'static str> {
    if ret.contains('&') {
        return Some("returns a reference");
    }
    // A lifetime token: `'p`, `'_`, `'static`. `'static` is harmless in
    // itself but cannot be told from `'p` without resolving the signature's
    // generics, and a pool-gated function has no business returning either.
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
        // Match the RHS however it is PATHED — `= AutoreleasePool`,
        // `= crate::AutoreleasePool`, `= aterm_objc::AutoreleasePool`. The
        // first version of this line required the bare name and was defeated
        // by `crate::`, which is how the plant that found it was written.
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
/// The signature runs from `fn` to the `{`, `;` or `where` that ends it, with
/// newlines flattened — multi-line signatures are exactly the ones a
/// line-oriented grep would miss.
fn fn_signatures(src: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (i, raw) in src.lines().enumerate() {
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
        for later in src.lines().skip(i) {
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

/// THE SECOND MEASUREMENT BEHIND THE WITHDRAWAL: the `+0` load is not cheaper.
///
/// `load_borrowed` was documented as "the cheap load: no retain/release pair
/// survives the call". `objc4` defines `objc_loadWeak(location)` as
/// `objc_autorelease(objc_loadWeakRetained(location))` — the same retain, plus
/// an autorelease, for the +1 load's single release — so the claim was wrong
/// before soundness ever came into it.
///
/// This test ASSERTS only what is stable (both forms answer the same object)
/// and PRINTS the timings, because a wall-clock threshold in libtest is a flake
/// waiting to happen. Read it with:
///
/// ```text
/// cargo test --release -p aterm-objc --test weak -- --nocapture the_plus_zero_load
/// ```
///
/// Measured on m21 (`--release`, 2×10⁶ loads per arm, four runs): `load()`
/// 4.7–5.0 ns/op, the +0 load with the pool amortised 1,000 deep 8.0–8.1, and
/// the only sound shape — the loader pushing its own pool — 8.8–9.0.
#[test]
fn the_plus_zero_load_is_not_the_cheaper_one() {
    const N: usize = 200_000;
    const CHUNK: usize = 1_000;

    let obj = new_object();
    let slot = WeakSlot::uninit();
    // SAFETY: freshly `uninit()`, immovable until `destroy` below, `obj` live.
    unsafe { slot.init(obj.id()) };
    let weak = WeakObj::from_obj(&obj);
    let want = obj.id().addr();

    let t = std::time::Instant::now();
    for _ in 0..N {
        assert_eq!(weak.load().expect("live").id().addr(), want);
    }
    let retained = t.elapsed();

    let t = std::time::Instant::now();
    for _ in 0..(N / CHUNK) {
        autoreleasepool(|_p| {
            for _ in 0..CHUNK {
                // SAFETY: `slot` is initialised and unmoved; the answer is read
                // inside the pool that is innermost at the call.
                assert_eq!(unsafe { slot.load_autoreleased() }.addr(), want);
            }
        });
    }
    let pooled = t.elapsed();

    let t = std::time::Instant::now();
    for _ in 0..N {
        autoreleasepool(|_p| {
            // SAFETY: as above.
            assert_eq!(unsafe { slot.load_autoreleased() }.addr(), want);
        });
    }
    let scoped = t.elapsed();

    let ns = |d: std::time::Duration| d.as_nanos() as f64 / N as f64;
    println!(
        "load() +1        {:>7.2} ns/op\n\
         +0, pool/{CHUNK}   {:>7.2} ns/op\n\
         +0, pool per load {:>6.2} ns/op",
        ns(retained),
        ns(pooled),
        ns(scoped)
    );

    // SAFETY: initialised above and unmoved.
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

    // The FIRST object is no longer named by this slot, so its death must not
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

/// A weak reference must not keep its target alive. If `WeakObj::new` retained,
/// every test above would still pass and every cycle it exists to break would
/// still leak.
#[test]
fn a_weak_reference_does_not_retain() {
    let obj = new_object();
    // SAFETY: `-retainCount` is `q16@0:8`. It is a debugging read, not a
    // decision input — the assertion below is about a DIFFERENCE, which is the
    // only thing this number can honestly support.
    let count = |id: Id| unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> isize = msg();
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

/// `objc_destroyWeak` really does remove the address, measured at an address
/// this test OWNS.
///
/// The deterministic half of the unregistration proof, and the one that gates:
/// the slot is a local, so no allocator has to cooperate for the experiment to
/// mean anything. Register it, destroy it, overwrite it with a sentinel, then
/// kill the target. If the destroy did not take, the runtime writes `nil`
/// through an address it no longer has any right to.
///
/// The sentinel is the TARGET'S OWN POINTER, which is what the slot legitimately
/// held a moment ago and what the runtime's own consistency check accepts — see
/// [`the_wild_write_a_missing_destroy_performs`] for why any other value makes
/// the check vacuous.
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

    // Re-fill the (now unregistered) storage by hand.
    // SAFETY: `addr()` is a valid, aligned pointer to this local's own pointer
    // word, which nothing else aliases now that the registration is gone.
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

/// And the same experiment WITHOUT the destroy — the wild write itself.
///
/// This is the failure mode `WeakObj::drop` exists to prevent, performed
/// deliberately on storage this test owns so it can be watched. The slot stays
/// registered, the storage is then treated as if it belonged to something else,
/// and the runtime zeroes it when the target dies.
///
/// It is the POSITIVE control for the test above: if this one ever stops
/// showing a nil, the negative result up there stops being evidence, because
/// the runtime would have stopped writing through registered addresses at all.
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

/// `WeakObj::drop` unregisters the address BEFORE the box is freed.
///
/// The heap half of the unregistration proof. The slot lives in a `Box`, so
/// once the handle drops, the address belongs to the allocator and then to
/// whatever it hands out next. A registration that outlived `Drop` is not a
/// leak, it is a **wild write** the runtime performs into live application
/// data, at a time nothing in the program chooses.
///
/// # The memory is QUARANTINED, not raced for
///
/// [`Quarantining`] is this binary's global allocator, and for the one
/// `dealloc` inside `drop(weak)` it LEAKS the block instead of returning it.
/// The eight bytes therefore stay allocated and owned by this test, so the
/// experiment reads an address that is genuinely still live rather than one the
/// allocator might or might not have handed back.
///
/// That is the fourth shape of this test and the first one that is a gate. The
/// three before it all tried to win a race with the allocator, and the record
/// is kept because each failed differently and each looked fine first:
///
/// 1. It filled the freed slot with an ARBITRARY non-zero word. With `Drop`'s
///    `destroy` deleted it still PASSED, because the runtime saw the mismatch
///    and declined the write:
///
///    ```text
///    objc[94981]: __weak variable at 0x1028c82a0 holds 0x1c3f instead of
///    0x1028c0f80. This is probably incorrect use of objc_storeWeak() and
///    objc_loadWeak().
///    ```
///
///    A guard armed at a shape the defect does not produce is not a guard. The
///    sentinel below is the TARGET'S OWN POINTER, which is what a live weak
///    slot for `obj` contains and what that consistency check accepts.
/// 2. It freed ONE slot and allocated in BULK. Reliable under
///    `--test-threads=1`, and `the allocator never reused the freed slot` four
///    runs in five in parallel. Freeing SIXTY-FOUR slots first made it strictly
///    worse — `none of 4096 fresh boxes reused any of the 64 freed slots`,
///    every run — because `objc_destroyWeak` and the runtime's own weak-table
///    bookkeeping allocate in the same size class and take the blocks first.
/// 3. It moved to a child process for a quiet allocator, and then to a
///    drop-then-allocate-immediately recipe that measured 256/256 in a binary
///    of its own. Together they still flaked one run in two. macOS's tiny
///    magazines are per-CPU, and nothing a test can do makes winning that race
///    a property.
///
/// The lesson is the shape of the evidence, not the allocator: an experiment
/// whose precondition is luck reports "could not measure" exactly as often as
/// the luck runs out, and a gate that says that is not reporting on the code.
#[test]
fn dropping_the_handle_unregisters_before_the_box_is_freed() {
    let obj = new_object();
    let target = obj.id();

    let weak = WeakObj::from_obj(&obj);
    // The address is EXPOSED before the box dies, and re-derived from the
    // integer afterwards. The block is still allocated — the quarantine saw to
    // that — but the `Box`'s own provenance ended with it, and
    // `with_exposed_provenance_mut` is the sanctioned way to build a fresh
    // pointer to memory whose address crossed out of Rust's model. It is the
    // same tool this crate's ivar access uses, for the same reason.
    let slot = weak.slot_addr().expose_provenance();

    quarantine(true);
    drop(weak);
    quarantine(false);

    // The block is now this test's, permanently. Fill it as a squatter would.
    let slot: *mut Id = std::ptr::with_exposed_provenance_mut(slot);
    // SAFETY: the quarantine leaked this block rather than freeing it, so the
    // eight bytes are still allocated, still pointer-aligned, and reachable
    // through no other pointer — `weak` is gone and nothing else was handed
    // this address.
    unsafe { *slot = target };

    // The moment of truth: if the registration outlived `Drop`, the runtime
    // writes nil through this address, which the program has moved on from.
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
// THE QUARANTINING ALLOCATOR
// ---------------------------------------------------------------------------

/// This binary's global allocator: [`std::alloc::System`], except that a thread
/// which has armed [`quarantine`] LEAKS its deallocations instead of returning
/// them.
///
/// One test needs to read the bytes a `Box` released, and reading freed memory
/// is not something a test may do. Leaking the block makes the read legitimate:
/// the memory is still allocated, so the only thing that changed is that
/// nothing else can be given it.
///
/// The flag is THREAD-LOCAL, so arming it in one test cannot quarantine the
/// twelve others running beside it, and it is `const`-initialised so that
/// reading it inside `dealloc` cannot itself allocate.
struct Quarantining;

thread_local! {
    /// Armed only around the one `drop` that matters. `const` init: a lazily
    /// initialised TLS would allocate on first touch, and the first touch is
    /// inside `dealloc`.
    static QUARANTINE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Arm or disarm the quarantine for THIS thread.
fn quarantine(on: bool) {
    QUARANTINE.with(|q| q.set(on));
}

// SAFETY: every method forwards to `System`, which is a correct global
// allocator, with one deviation: `dealloc` may decline to free a block. Never
// freeing an allocation is permitted — it is a leak, not unsoundness — and no
// block is ever freed twice, handed back while live, or returned to a different
// allocator than the one that produced it.
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
        // SAFETY: `ptr`/`layout` come from this allocator's `alloc`, which
        // forwards to `System`, so `System` is the right allocator to return
        // them to.
        unsafe { std::alloc::System.dealloc(ptr, layout) };
    }

    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        // SAFETY: contract passed through unchanged.
        unsafe { std::alloc::System.alloc_zeroed(layout) }
    }

    // `realloc` is deliberately NOT forwarded to `System::realloc`: the default
    // implementation is alloc + copy + `self.dealloc`, which is what routes a
    // growing `Vec` through the quarantine too. `System::realloc` would call
    // `free` behind this type's back.
}

#[global_allocator]
static ALLOC: Quarantining = Quarantining;
