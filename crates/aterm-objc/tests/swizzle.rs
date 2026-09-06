// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SWIZZLE MEASUREMENTS, and the refusals.
//!
//! Every claim `crates/aterm-objc/src/swizzle.rs` makes about the runtime is
//! re-measured here **against libobjc directly**, not against the module under
//! test: this file declares its own `class_addMethod`, `class_replaceMethod`,
//! `method_setImplementation`, `class_getInstanceMethod` and
//! `method_getTypeEncoding`, exactly as `examples/objc_dispatch_drive.rs`
//! declares its own libdispatch surface and for the same reason. A measurement
//! that goes through the wrapper cannot catch the wrapper being wrong about the
//! runtime.
//!
//! # These tests run on a libtest WORKER thread, and that is fine here
//!
//! Class registration is deliberately ungated by [`aterm_objc::MainThread`] —
//! see its docs — and none of these classes touch AppKit state or hold a Rust
//! ivar with a destructor, so registering, messaging and swizzling them on a
//! worker satisfies `MainThread::new_unchecked`'s second form. The parts that
//! genuinely need the main thread — a real `NSApplication`, a real event, and
//! the `dladdr` question A2 asks — are in
//! `examples/objc_swizzle_drive.rs`, which owns a `fn main`.

#![cfg(target_os = "macos")]

use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicU32, Ordering};

use aterm_objc::swizzle::{MethodFn, Swizzle, SwizzleError, SwizzleSite, owning_class};
use aterm_objc::{
    Bool, CGRect, ClassPtr, Encode, Id, Imp, NSRange, Sel, begin, class_of, method_encoding,
    method_imp, method_types, msg, ns_string, sel,
};

// The runtime, bound HERE so the measurements are independent of the module
// under test.
//
// SAFETY: every symbol below is `libobjc`'s, and `libobjc` is loaded in every
// process on this platform — the same reason `objc_msgSend` needs no link
// attribute in `runtime.rs`.
unsafe extern "C" {
    fn class_getInstanceMethod(cls: ClassPtr, name: Sel) -> *const c_void;
    fn method_getTypeEncoding(method: *const c_void) -> *const c_char;
    fn method_getImplementation(method: *const c_void) -> *const c_void;
    fn method_setImplementation(method: *const c_void, imp: *const c_void) -> *const c_void;
    fn class_addMethod(cls: ClassPtr, name: Sel, imp: *const c_void, types: *const c_char) -> Bool;
    fn class_replaceMethod(
        cls: ClassPtr,
        name: Sel,
        imp: *const c_void,
        types: *const c_char,
    ) -> *const c_void;
    fn class_copyMethodList(cls: ClassPtr, out_count: *mut u32) -> *mut *const c_void;
    fn free(ptr: *mut c_void);
}

/// How many rows a class registered ITSELF — the containment question.
fn own_method_count(cls: ClassPtr) -> u32 {
    let mut n: u32 = 0;
    // SAFETY: `cls` is a live class object; the runtime writes the count and
    // hands back a malloc'd array the caller owns.
    let list = unsafe { class_copyMethodList(cls, &raw mut n) };
    if !list.is_null() {
        // SAFETY: the array came from `class_copyMethodList` and is the
        // caller's to free.
        unsafe { free(list.cast::<c_void>()) };
    }
    n
}

/// The registered encoding, read through the raw runtime.
fn raw_encoding(cls: ClassPtr, sel: Sel) -> Option<String> {
    // SAFETY: `cls` is live; both accessors answer NULL for a missing row.
    unsafe {
        let m = class_getInstanceMethod(cls, sel);
        if m.is_null() {
            return None;
        }
        let t = method_getTypeEncoding(m);
        if t.is_null() {
            return None;
        }
        Some(CStr::from_ptr(t).to_string_lossy().into_owned())
    }
}

// ---------------------------------------------------------------------------
// The probe implementations. Each records that it ran, and returns a value the
// caller can tell apart.
// ---------------------------------------------------------------------------

/// A COUNTER IS PROCESS-GLOBAL AND LIBTEST IS PARALLEL.
///
/// These two are read only for their VALUE by the tests that share
/// `original_probe`/`patched_probe`; the one test that needs a COUNT has its own
/// pair below. That is not tidiness — the first version of this file counted on
/// these and failed 4 runs in 5, because two other tests were sending
/// `-probeValue` to their own classes on other libtest threads and incrementing
/// the same words. A shared counter under a parallel harness is a flake, and
/// the fix is a private counter rather than a serialisation the file cannot
/// enforce.
static ORIGINAL_RUNS: AtomicU32 = AtomicU32::new(0);
static PATCHED_RUNS: AtomicU32 = AtomicU32::new(0);

/// The private pair, touched by exactly one test.
static SITE_ORIGINAL_RUNS: AtomicU32 = AtomicU32::new(0);
static SITE_PATCHED_RUNS: AtomicU32 = AtomicU32::new(0);

/// `-(int64_t)probeValue` — the original, for the site test alone.
unsafe extern "C-unwind" fn site_original_probe(_this: Id, _cmd: Sel) -> i64 {
    SITE_ORIGINAL_RUNS.fetch_add(1, Ordering::Relaxed);
    1
}

/// `-(int64_t)probeValue` — the patch, for the site test alone.
unsafe extern "C-unwind" fn site_patched_probe(_this: Id, _cmd: Sel) -> i64 {
    SITE_PATCHED_RUNS.fetch_add(1, Ordering::Relaxed);
    2
}

/// `-(int64_t)probeValue` — the ORIGINAL.
unsafe extern "C-unwind" fn original_probe(_this: Id, _cmd: Sel) -> i64 {
    ORIGINAL_RUNS.fetch_add(1, Ordering::Relaxed);
    1
}

/// `-(int64_t)probeValue` — the PATCH.
unsafe extern "C-unwind" fn patched_probe(_this: Id, _cmd: Sel) -> i64 {
    PATCHED_RUNS.fetch_add(1, Ordering::Relaxed);
    2
}

/// A second patch, for the displacement case.
unsafe extern "C-unwind" fn other_probe(_this: Id, _cmd: Sel) -> i64 {
    3
}

/// A patch with the WRONG return width — the shape the encoding check exists to
/// refuse. Never installed by any passing test.
unsafe extern "C-unwind" fn wrong_shape(_this: Id, _cmd: Sel) -> Bool {
    Bool::from(true)
}

type ProbeFn = unsafe extern "C-unwind" fn(Id, Sel) -> i64;
type WrongFn = unsafe extern "C-unwind" fn(Id, Sel) -> Bool;

/// Register a fresh `NSObject` subclass with `-probeValue` on it.
///
/// The name must be unique per test: `objc_allocateClassPair` returns nil for a
/// name already in the process, and `begin` panics on that.
fn register_probe_class(name: &'static CStr) -> ClassPtr {
    register_probe_class_with(name, original_probe)
}

/// As [`register_probe_class`], with the implementation named — so a test that
/// counts CALLS can own its counter instead of sharing one with every other
/// test the harness is running at the same time.
fn register_probe_class_with(name: &'static CStr, imp: ProbeFn) -> ClassPtr {
    let mut b = begin(c"NSObject", name);
    // `ClassBuilder::register` asserts the ivar exists — every class this crate
    // mints carries exactly one, and `()` is the empty payload.
    b.add_rust_ivar::<()>();
    // SAFETY: `imp` is `extern "C"` with the `(Id, Sel) -> i64` prototype the
    // encoding beside it describes, and it cannot unwind — every one in this
    // file touches one atomic and returns a constant.
    unsafe {
        b.add_method(
            sel!(probeValue),
            imp as *const c_void,
            &method_encoding!(i64),
        );
    }
    b.register().class()
}

/// A fresh instance of `cls`, +1, leaked — these classes have no ivars and the
/// tests are about method tables, not lifetimes.
fn instance_of(cls: ClassPtr) -> Id {
    // SAFETY: `+alloc` then `-init` on an `NSObject` subclass is the canonical
    // +1 construction and both prototypes are `-(id)(id, SEL)`.
    unsafe {
        let alloc: unsafe extern "C-unwind" fn(Id, Sel) -> Id = msg();
        let init: unsafe extern "C-unwind" fn(Id, Sel) -> Id = msg();
        init(alloc(cls.as_id(), sel!(alloc)), sel!(init))
    }
}

/// Send `-probeValue`.
fn probe_value(obj: Id) -> i64 {
    // SAFETY: `-probeValue` is registered `q@:` on every class in this file and
    // `obj` is a live instance of one.
    unsafe {
        let send: unsafe extern "C-unwind" fn(Id, Sel) -> i64 = msg();
        send(obj, sel!(probeValue))
    }
}

// ---------------------------------------------------------------------------
// PART 1 — the three mutators, measured. These are the facts the module's
// "what a guard can and cannot prove" section rests on.
// ---------------------------------------------------------------------------

#[test]
fn method_set_implementation_leaves_the_encoding_byte_identical() {
    let cls = register_probe_class(c"AtermSwizzleEnc");
    let before = raw_encoding(cls, sel!(probeValue)).expect("a registered row has an encoding");

    // SAFETY: the row exists (just registered) and `patched_probe` has the same
    // prototype as the implementation it replaces.
    let previous = unsafe {
        let m = class_getInstanceMethod(cls, sel!(probeValue));
        method_setImplementation(m, patched_probe as *const c_void)
    };

    let after = raw_encoding(cls, sel!(probeValue)).expect("still registered");
    assert_eq!(
        before, after,
        "method_setImplementation takes no `types` argument, so it cannot move the encoding — \
         this is why no encoding check in this tree can SEE a swizzle"
    );
    assert!(
        std::ptr::eq(previous, original_probe as *const c_void),
        "the swap returns the implementation it displaced"
    );
}

#[test]
fn class_replace_method_ignores_a_lying_types_on_a_row_the_class_already_has() {
    let cls = register_probe_class(c"AtermSwizzleReplace");
    let before = raw_encoding(cls, sel!(probeValue)).expect("registered");

    // SAFETY: the row exists on `cls` directly; the types string is a
    // deliberate lie whose fate is the measurement.
    let previous = unsafe {
        class_replaceMethod(
            cls,
            sel!(probeValue),
            patched_probe as *const c_void,
            c"{_Lie=qqqq}@:B".as_ptr(),
        )
    };

    assert!(
        std::ptr::eq(previous, original_probe as *const c_void),
        "on an existing row `class_replaceMethod` behaves as a swap and returns the previous IMP"
    );
    assert_eq!(
        before,
        raw_encoding(cls, sel!(probeValue)).expect("registered"),
        "the lying `types` was IGNORED — the encoding is still what the class registered"
    );
}

#[test]
fn class_add_method_refuses_a_row_the_class_already_has() {
    let cls = register_probe_class(c"AtermSwizzleAdd");
    let before = raw_encoding(cls, sel!(probeValue)).expect("registered");

    // SAFETY: as above; the return value is the measurement.
    let added = unsafe {
        class_addMethod(
            cls,
            sel!(probeValue),
            patched_probe as *const c_void,
            c"{_Lie=qqqq}@:B".as_ptr(),
        )
    };

    assert!(
        !added.as_bool(),
        "`class_addMethod` answers NO for a selector the class already has — which is why \
         `ClassBuilder::add_method` cannot be the swizzle primitive: it asserts on this NO, and \
         an existing row is the PREMISE of a swizzle"
    );
    assert_eq!(
        before,
        raw_encoding(cls, sel!(probeValue)).expect("registered"),
        "and it changed nothing"
    );
    assert_eq!(
        1,
        probe_value(instance_of(cls)),
        "the original implementation is still the one that runs"
    );
}

// ---------------------------------------------------------------------------
// PART 2 — the blast radius: the class you name is not the class you patch.
// ---------------------------------------------------------------------------

#[test]
fn a_swizzle_through_a_subclass_mutates_the_ancestors_row() {
    let parent = register_probe_class(c"AtermSwizzleParent");
    let child = {
        let mut b = begin(c"AtermSwizzleParent", c"AtermSwizzleChild");
        b.add_rust_ivar::<()>();
        b.register().class()
    };

    // SAFETY: both classes are live and registered.
    let (m_parent, m_child) = unsafe {
        (
            class_getInstanceMethod(parent, sel!(probeValue)),
            class_getInstanceMethod(child, sel!(probeValue)),
        )
    };
    assert!(
        std::ptr::eq(m_parent, m_child),
        "`class_getInstanceMethod` walks the chain and hands back the DEFINER's Method — the \
         same pointer, not a copy"
    );
    assert_eq!(
        0,
        own_method_count(child),
        "the subclass registered nothing of its own"
    );

    let parent_instance = instance_of(parent);
    assert_eq!(1, probe_value(parent_instance));

    // Patch through the CHILD's lookup.
    // SAFETY: the row exists and the prototype matches.
    unsafe { method_setImplementation(m_child, patched_probe as *const c_void) };

    assert_eq!(
        2,
        probe_value(parent_instance),
        "a swizzle applied through the SUBCLASS changed what a PARENT instance does — the patch \
         is process-wide and the call site cannot see it. `owning_class` is what reports it"
    );
    assert_eq!(
        0,
        own_method_count(child),
        "and nothing was added to the subclass"
    );
}

#[test]
fn class_replace_method_on_an_inherited_row_adds_and_returns_null() {
    let parent = register_probe_class(c"AtermSwizzleParent2");
    let child = {
        let mut b = begin(c"AtermSwizzleParent2", c"AtermSwizzleChild2");
        b.add_rust_ivar::<()>();
        b.register().class()
    };

    // SAFETY: `child` is live; the row is INHERITED, not its own.
    let previous = unsafe {
        class_replaceMethod(
            child,
            sel!(probeValue),
            patched_probe as *const c_void,
            c"q@:".as_ptr(),
        )
    };

    assert!(
        previous.is_null(),
        "on a row the class does not have DIRECTLY, `class_replaceMethod` ADDS one and returns \
         NULL — a caller storing that as `the original` would have stored nil and would call it. \
         This is why the contained form is not offered: its chaining rule is msg_super, which is \
         a different capability"
    );
    assert_eq!(1, own_method_count(child), "a row was added to the child");
    assert_eq!(
        1,
        probe_value(instance_of(parent)),
        "and the parent's row was left alone — the containment the other form does not have"
    );
    assert_eq!(2, probe_value(instance_of(child)));
}

#[test]
fn owning_class_finds_the_definer_not_the_class_asked() {
    let parent = register_probe_class(c"AtermSwizzleOwner");
    let child = {
        let mut b = begin(c"AtermSwizzleOwner", c"AtermSwizzleOwnerChild");
        b.add_rust_ivar::<()>();
        b.register().class()
    };

    // SAFETY: both classes are live.
    let owner = unsafe { owning_class(child, sel!(probeValue)) }.expect("the chain implements it");
    assert_eq!(parent, owner, "the row lives on the parent");

    // A row the child DOES define resolves to the child.
    let mut b = begin(c"AtermSwizzleOwner", c"AtermSwizzleOwnerOverride");
    b.add_rust_ivar::<()>();
    // SAFETY: prototype and encoding agree, and the trampoline cannot unwind.
    unsafe {
        b.add_method(
            sel!(probeValue),
            patched_probe as *const c_void,
            &method_encoding!(i64),
        );
    }
    let over = b.register().class();
    // SAFETY: `over` is live.
    assert_eq!(
        over,
        unsafe { owning_class(over, sel!(probeValue)) }.expect("it defines it")
    );

    // And a selector nothing implements has no owner.
    // SAFETY: `child` is live; the selector is interned and unimplemented.
    assert_eq!(None, unsafe {
        owning_class(child, sel!(atermNoSuchSelector))
    });
    // SAFETY: null is the documented input `owning_class` answers `None` for.
    assert_eq!(None, unsafe {
        owning_class(ClassPtr::NULL, sel!(probeValue))
    });
}

// ---------------------------------------------------------------------------
// PART 3 — the encoding derived from the PROTOTYPE, which is the check nothing
// else in this tree can make.
// ---------------------------------------------------------------------------

#[test]
fn method_fn_derives_the_encoding_from_the_prototype() {
    assert_eq!("q@:", <ProbeFn as MethodFn>::method_encoding());
    assert_eq!(
        <Bool as Encode>::ENCODING.to_owned() + "@:",
        <WrongFn as MethodFn>::method_encoding(),
        "the `BOOL` letter is the target's, not a literal — `B` on arm64, `c` on the x86_64 \
         compat slice"
    );

    type SendEvent = unsafe extern "C-unwind" fn(Id, Sel, Id);
    assert_eq!(
        "v@:@",
        <SendEvent as MethodFn>::method_encoding(),
        "this is the one `override_send_event` needs, and it is what the live \
         `-[NSApplication sendEvent:]` row strips to"
    );

    // The shape a wide selector takes, so the derivation is exercised past
    // arity one.
    type Wide = unsafe extern "C-unwind" fn(Id, Sel, CGRect, NSRange, *mut NSRange, Bool) -> CGRect;
    assert_eq!(
        format!(
            "{r}@:{r}{n}^{n}{b}",
            r = <CGRect as Encode>::ENCODING,
            n = <NSRange as Encode>::ENCODING,
            b = <Bool as Encode>::ENCODING,
        ),
        <Wide as MethodFn>::method_encoding()
    );
}

#[test]
fn imp_carries_the_encoding_clang_emits() {
    assert_eq!(
        "^?",
        <Imp as Encode>::ENCODING,
        "MEASURED: `@encode(IMP)` is a pointer to an UNKNOWN type, not the `^v` a `void *` gets \
         and not the `@` an object gets"
    );
    assert_eq!(size_of::<Imp>(), size_of::<*const c_void>());
    assert_eq!(
        size_of::<Option<Imp>>(),
        size_of::<Imp>(),
        "non-null by construction, so the option is one word"
    );
}

#[test]
fn install_refuses_a_prototype_the_registered_encoding_does_not_match() {
    static SITE: SwizzleSite<WrongFn> = SwizzleSite::new();
    let cls = register_probe_class(c"AtermSwizzleMismatch");

    // SAFETY: `wrong_shape` is `extern "C"`, does not unwind, and is the very
    // thing the install is expected to REFUSE — it returns `BOOL` where the row
    // is registered `q`.
    let outcome = unsafe { SITE.install(cls, sel!(probeValue), wrong_shape) };

    match outcome {
        Err(SwizzleError::EncodingMismatch {
            registered,
            expected,
        }) => {
            assert_eq!("q@:", registered);
            assert_eq!(<WrongFn as MethodFn>::method_encoding(), expected);
        }
        other => panic!("expected an encoding refusal, got {other:?}"),
    }
    assert_eq!(
        1,
        probe_value(instance_of(cls)),
        "and NOTHING was written — the original still runs"
    );
    assert!(SITE.original().is_none(), "and the site is still empty");
}

#[test]
fn install_refuses_a_selector_nothing_implements_and_a_null_class() {
    static SITE: SwizzleSite<ProbeFn> = SwizzleSite::new();
    let cls = register_probe_class(c"AtermSwizzleAbsent");

    // SAFETY: the prototype is `extern "C"` and non-unwinding; the selector is
    // simply not implemented, which is the refusal being measured.
    assert_eq!(
        Err(SwizzleError::NoSuchMethod),
        unsafe { SITE.install(cls, sel!(atermNoSuchSelector), patched_probe) },
        "an existing implementation is the PREMISE of a swizzle; `class_addMethod` is the call \
         for the other case and `ClassBuilder` is where it lives"
    );
    // SAFETY: as above, against the null class a failed lookup returns.
    assert_eq!(Err(SwizzleError::NullClass), unsafe {
        SITE.install(ClassPtr::NULL, sel!(probeValue), patched_probe)
    });
}

// ---------------------------------------------------------------------------
// PART 4 — the site: install, chain, idempotence, and the two refusals that
// stop a chain being built out of somebody else's implementation.
// ---------------------------------------------------------------------------

#[test]
fn install_replaces_publishes_the_original_and_reports_the_owner() {
    static SITE: SwizzleSite<ProbeFn> = SwizzleSite::new();
    let parent = register_probe_class_with(c"AtermSwizzleSite", site_original_probe);
    let child = {
        let mut b = begin(c"AtermSwizzleSite", c"AtermSwizzleSiteChild");
        b.add_rust_ivar::<()>();
        b.register().class()
    };

    // Ask for the CHILD, deliberately: the row lives on the parent.
    // SAFETY: `patched_probe` has the `(Id, Sel) -> i64` prototype the row is
    // registered with — which the install itself re-checks — and cannot unwind.
    let done = unsafe { SITE.install(child, sel!(probeValue), site_patched_probe) }
        .expect("the row exists and the prototype matches");

    match done {
        Swizzle::Replaced { owner, previous } => {
            assert_eq!(
                parent, owner,
                "the install reports where the row ACTUALLY lives, which is not the class it was \
                 handed"
            );
            assert_eq!(previous.as_ptr(), site_original_probe as *const c_void);
        }
        other => panic!("expected a replacement, got {other:?}"),
    }

    // The runtime now calls ours, on an instance of the PARENT.
    assert_eq!(2, probe_value(instance_of(parent)));
    assert_eq!(1, SITE_PATCHED_RUNS.load(Ordering::Relaxed));
    assert_eq!(0, SITE_ORIGINAL_RUNS.load(Ordering::Relaxed));

    // And the chain is typed: no transmute at the call site.
    let original = SITE.original().expect("the site published the original");
    // SAFETY: the install verified this prototype against the encoding the
    // runtime holds. An IMP entered directly is NOT a message send, so the
    // receiver must be a live object — this one is.
    let chained = unsafe { original(instance_of(parent), sel!(probeValue)) };
    assert_eq!(1, chained);
    assert_eq!(1, SITE_ORIGINAL_RUNS.load(Ordering::Relaxed));

    // The IMP the runtime will call is ours, read back through the crate's own
    // accessor — the same question part A2 of the live auditor asks of AppKit.
    // SAFETY: `parent` is a live class.
    let live = unsafe { method_imp(parent, sel!(probeValue)) }.expect("registered");
    assert!(std::ptr::eq(live, site_patched_probe as *const c_void));

    // And the encoding did not move, which is the whole reason A2 is needed.
    // SAFETY: `parent` is live.
    assert_eq!(
        Some("q@:".to_owned()),
        unsafe { method_types(parent, sel!(probeValue)) },
        "no encoding check in this tree can tell this class was patched"
    );

    // IDEMPOTENT re-entry — the case `override_send_event` relies on when it is
    // called twice.
    // SAFETY: as the first install.
    let again = unsafe { SITE.install(child, sel!(probeValue), site_patched_probe) }
        .expect("a second install of the same implementation is a no-op");
    match again {
        Swizzle::AlreadyInstalled { owner, previous } => {
            assert_eq!(parent, owner);
            assert_eq!(
                previous.as_ptr(),
                site_original_probe as *const c_void,
                "the ORIGINAL is still the original — a second install must not publish OUR \
                 implementation as the thing to chain to, which is the loop that would make"
            );
        }
        other => panic!("expected an idempotent re-entry, got {other:?}"),
    }
}

#[test]
fn install_refuses_once_a_third_party_has_displaced_it() {
    static SITE: SwizzleSite<ProbeFn> = SwizzleSite::new();
    let cls = register_probe_class(c"AtermSwizzleDisplaced");

    // SAFETY: prototype matches the registered row and cannot unwind.
    unsafe { SITE.install(cls, sel!(probeValue), patched_probe) }.expect("the first install");

    // A THIRD PARTY swizzles on top, through the raw runtime.
    // SAFETY: the row exists and `other_probe` has its prototype.
    unsafe {
        let m = class_getInstanceMethod(cls, sel!(probeValue));
        method_setImplementation(m, other_probe as *const c_void);
    }

    // SAFETY: as above.
    let outcome = unsafe { SITE.install(cls, sel!(probeValue), patched_probe) };
    match outcome {
        Err(SwizzleError::Displaced { expected, found }) => {
            assert_eq!(expected.as_ptr(), patched_probe as *const c_void);
            assert_eq!(found.as_ptr(), other_probe as *const c_void);
        }
        other => panic!("expected a displacement refusal, got {other:?}"),
    }
    assert_eq!(
        3,
        probe_value(instance_of(cls)),
        "and nothing was written: re-installing would publish the third party's implementation \
         as `the original` and build a chain that skips it"
    );
    assert_eq!(
        SITE.original().map(|f| f as *const c_void),
        Some(original_probe as *const c_void),
        "the site still holds the implementation IT displaced"
    );
}

#[test]
fn install_refuses_when_the_row_is_already_patched_by_somebody_else() {
    static SITE: SwizzleSite<ProbeFn> = SwizzleSite::new();
    let cls = register_probe_class(c"AtermSwizzleOrphan");

    // Somebody else installs OUR implementation, without going through the
    // site — so the original is gone and the runtime cannot give it back.
    // SAFETY: the row exists and the prototype matches.
    unsafe {
        let m = class_getInstanceMethod(cls, sel!(probeValue));
        method_setImplementation(m, patched_probe as *const c_void);
    }

    // SAFETY: as above.
    assert_eq!(
        Err(SwizzleError::OrphanedInstall),
        unsafe { SITE.install(cls, sel!(probeValue), patched_probe) },
        "the row already holds this implementation and the site has nothing to chain to; \
         answering `AlreadyInstalled` here would hand the caller an original it does not have"
    );
}

#[test]
fn the_swizzle_is_confined_to_the_class_pair_it_was_applied_to() {
    // The sibling classes each test registers are independent: a patch on one
    // must not reach another. This is the control for every test above, and it
    // is the reason each of them registers its OWN class rather than sharing.
    let a = register_probe_class(c"AtermSwizzleIsolationA");
    let b = register_probe_class(c"AtermSwizzleIsolationB");
    // SAFETY: the row exists on `a` and the prototype matches.
    unsafe {
        let m = class_getInstanceMethod(a, sel!(probeValue));
        method_setImplementation(m, patched_probe as *const c_void);
    }
    assert_eq!(2, probe_value(instance_of(a)));
    assert_eq!(1, probe_value(instance_of(b)));
    // SAFETY: both classes are live.
    unsafe {
        assert!(std::ptr::eq(
            method_getImplementation(class_getInstanceMethod(b, sel!(probeValue))),
            original_probe as *const c_void
        ));
    }
}

#[test]
fn concurrent_installs_into_one_site_never_build_a_chain_that_calls_itself() {
    // THE RACE THE `original_imp()` READ CANNOT CLOSE, and the reason the site
    // is claimed with a compare-exchange rather than a store.
    //
    // Without the CAS, two threads read the same empty slot, both publish, and
    // the loser's swap displaces the WINNER'S implementation — so the site ends
    // up holding an implementation a thread installed rather than the one it
    // displaced. That is a chain that calls itself: unbounded recursion inside
    // whatever dispatched to it. `install` takes `&'static self` exactly so it
    // can be called from anywhere, so "nobody would do that" is not a property
    // this type may assume.
    //
    // # THE TEST IS PROBABILISTIC AND THE NUMBER IS MEASURED
    //
    // The window is a handful of instructions. Measured on this box with the
    // CAS replaced by a plain store:
    //
    // * without a barrier, 8 threads, ONE round: **0 failures in 20 runs** —
    //   the threads serialise, the first finishes before the last is spawned,
    //   and the test is decoration.
    // * with the barrier below, 16 threads, 12 rounds: **6 failures in 60
    //   runs of the test binary — 10%**.
    //
    // The real implementation is **0 in 60** in the same arrangement, run the
    // same way.
    //
    // **SO THIS IS A WEAK DETECTOR AND IT SAYS SO.** One green run of this test
    // is worth about a tenth of a refutation, and a regression that removed the
    // CAS would reach `main` nine times in ten. It is kept because the
    // alternative is not a better test, it is NO test: the invariant would then
    // be an argument in a doc comment with nothing in the tree able to
    // contradict it, which is precisely the shape this campaign has been caught
    // by. What carries the weight is the reasoning in `install`, and what this
    // file adds is that the reasoning has been shown capable of being wrong.
    const THREADS: usize = 16;
    const ROUNDS: usize = 12;
    static SITES: [SwizzleSite<ProbeFn>; ROUNDS] = [const { SwizzleSite::new() }; ROUNDS];
    const NAMES: [&CStr; ROUNDS] = [
        c"AtermSwizzleRace0",
        c"AtermSwizzleRace1",
        c"AtermSwizzleRace2",
        c"AtermSwizzleRace3",
        c"AtermSwizzleRace4",
        c"AtermSwizzleRace5",
        c"AtermSwizzleRace6",
        c"AtermSwizzleRace7",
        c"AtermSwizzleRace8",
        c"AtermSwizzleRace9",
        c"AtermSwizzleRace10",
        c"AtermSwizzleRace11",
    ];

    for round in 0..ROUNDS {
        let cls = register_probe_class(NAMES[round]);
        // `ClassPtr` is `!Send` — it wraps a `*mut c_void` and, unlike `Sel`,
        // this crate has never written the `unsafe impl` that would say a class
        // object is immortal and thread-safe to hand around. So the ADDRESS
        // crosses and the pointer is rebuilt on the far side. Widening an
        // existing public type's auto traits is a decision of its own and not
        // one to take inside a test.
        let addr = cls.expose_provenance();
        // A BARRIER, because without one the threads serialise and the plant
        // above is not caught at all. Released together, they reach the
        // read-then-write inside `install` at the same moment.
        let gate = std::sync::Arc::new(std::sync::Barrier::new(THREADS));
        let site = &SITES[round];
        let mut threads = Vec::new();
        for _ in 0..THREADS {
            let gate = std::sync::Arc::clone(&gate);
            threads.push(std::thread::spawn(move || {
                let cls = ClassPtr::from_ptr(std::ptr::with_exposed_provenance_mut(addr));
                gate.wait();
                // SAFETY: the prototype matches the registered row — which the
                // install re-checks — and cannot unwind.
                let outcome = unsafe { site.install(cls, sel!(probeValue), patched_probe) };
                // `Swizzle` carries a `ClassPtr` and is therefore `!Send` too,
                // so the VERDICT crosses rather than the value. A label is all
                // this test needs, and reducing here keeps the thread's return
                // type from being the second thing that has to be widened.
                match outcome {
                    Ok(Swizzle::Replaced { .. }) => "replaced",
                    Ok(Swizzle::AlreadyInstalled { .. }) => "already",
                    Err(SwizzleError::Contended) => "contended",
                    Err(SwizzleError::Displaced { .. }) => "displaced",
                    Err(SwizzleError::OrphanedInstall) => "orphaned",
                    Err(SwizzleError::Raced { .. }) => "raced",
                    Err(_) => "refused",
                }
            }));
        }
        let outcomes: Vec<&'static str> = threads
            .into_iter()
            .map(|t| t.join().expect("no thread panicked"))
            .collect();

        assert_eq!(
            1,
            outcomes.iter().filter(|o| **o == "replaced").count(),
            "round {round}: exactly one install may write the row; the rest are \
             AlreadyInstalled, Contended or Displaced — got {outcomes:?}"
        );
        assert!(
            !outcomes.contains(&"refused")
                && !outcomes.contains(&"orphaned")
                && !outcomes.contains(&"raced"),
            "round {round}: no thread may see a refusal that means the row or the site was \
             corrupted mid-install — got {outcomes:?}"
        );

        let original = site.original().expect("the winner published one");
        assert_eq!(
            original as *const c_void, original_probe as *const c_void,
            "round {round}: THE ASSERTION THAT MATTERS — the site holds the implementation it \
             DISPLACED, never one a racing thread installed. The other spelling of this bug is a \
             chain that calls itself"
        );
        assert_eq!(2, probe_value(instance_of(cls)));
    }
}

/// The RECEIVER type of the probe row, deliberately wrong and deliberately
/// indistinguishable: `Sel` and `Id` are both one pointer word, and this
/// prototype's *third* position takes a `Sel` where the row's argument is an
/// object. It is not reachable through `-probeValue`, which takes no argument;
/// it exists only for the encoding comparison below.
type WrongObjectFn = unsafe extern "C-unwind" fn(Id, Sel, *const c_void) -> i64;
/// The same arity and the same widths, with `^v` where the row has `^v`.
type SameShapeFn = unsafe extern "C-unwind" fn(Id, Sel, *mut c_void) -> i64;

#[test]
fn the_encoding_check_is_blind_to_a_disagreement_that_does_not_move_a_register() {
    // A CHECK IS WORTH WHAT ITS BLINDEST SPELLING IS WORTH, and this test is
    // that spelling. Its PASSING is the bad news: `@encode` is a shape language,
    // not a type language, so two prototypes that differ in meaning but agree
    // in shape are the same string to it.
    assert_eq!(
        <WrongObjectFn as MethodFn>::method_encoding(),
        <SameShapeFn as MethodFn>::method_encoding(),
        "`^v` covers both const and mut `void *` — a const/mut disagreement is invisible here, \
         and correctly so: it moves no register"
    );

    // And the one that matters more, because this crate has no bindings and
    // every object is an `Id`: `@` erases the CLASS.
    type TakesEvent = unsafe extern "C-unwind" fn(Id, Sel, Id);
    type TakesString = unsafe extern "C-unwind" fn(Id, Sel, Id);
    assert_eq!(
        <TakesEvent as MethodFn>::method_encoding(),
        <TakesString as MethodFn>::method_encoding(),
        "a green install says the SHAPE is right; it says nothing about which object arrives"
    );

    // What it DOES catch is everything that moves a register or changes a
    // width — the disagreements that corrupt the ABI, which is what the check
    // is for.
    type Wider = unsafe extern "C-unwind" fn(Id, Sel, Id, Id) -> i64;
    type NarrowerReturn = unsafe extern "C-unwind" fn(Id, Sel, *mut c_void) -> Bool;
    type StructByValue = unsafe extern "C-unwind" fn(Id, Sel, CGRect) -> i64;
    let baseline = <SameShapeFn as MethodFn>::method_encoding();
    for (name, other) in [
        ("one more argument", <Wider as MethodFn>::method_encoding()),
        (
            "a narrower return",
            <NarrowerReturn as MethodFn>::method_encoding(),
        ),
        (
            "a struct by value where a pointer was",
            <StructByValue as MethodFn>::method_encoding(),
        ),
    ] {
        assert_ne!(baseline, other, "{name} must be visible to the check");
    }
}

// ---------------------------------------------------------------------------
// PART 5 — W15. What one site can be asked about two rows, and what survives
// a runtime that swaps an object's class underneath the patch.
// ---------------------------------------------------------------------------

/// ONE SITE, TWO ROWS: the answer must be a REFUSAL, not a number.
///
/// # The defect this is the regression test for
///
/// A site used to record only the IMP it displaced. "Am I already installed?"
/// was then answered by comparing the ROW's current implementation against the
/// one being installed — `current == new_imp` — which is a property of the row
/// and says nothing about which row this site claimed. Measured before the fix,
/// with the two classes below:
///
/// ```text
/// SITE.install(B) -> Ok(AlreadyInstalled { owner: Class(W15_A9_B),
///                                          previous: Imp(<A's original>) })
/// ```
///
/// The reported `owner` is `B` and the `previous` is a value from `A`'s row.
/// A caller chaining through it would enter `A`'s implementation with a `B`
/// receiver, from a call whose SAFETY comment says "the implementation this
/// site displaced" — and NOTHING else in this tree could see it, because the
/// two rows have the same encoding by construction.
#[test]
fn one_site_asked_about_a_second_row_refuses_instead_of_answering() {
    static SITE: SwizzleSite<ProbeFn> = SwizzleSite::new();
    static OTHER: SwizzleSite<ProbeFn> = SwizzleSite::new();

    let a = register_probe_class_with(c"W15SiteRowA", original_probe);
    let b = register_probe_class_with(c"W15SiteRowB", other_probe);

    // SAFETY: `patched_probe` has the registered prototype and cannot unwind.
    let first = unsafe { SITE.install(a, sel!(probeValue), patched_probe) }
        .expect("the first install lands");
    assert!(matches!(first, Swizzle::Replaced { .. }));
    assert_eq!(first.previous(), (original_probe as ProbeFn).imp());
    assert_eq!(
        SITE.installed_row(),
        Some((a, sel!(probeValue))),
        "a site claims a ROW, and can say which"
    );

    // A DIFFERENT site patches B's row to the same implementation. Ordinary,
    // legal, and the state in which the old rule gave a wrong answer.
    // SAFETY: as above.
    let second = unsafe { OTHER.install(b, sel!(probeValue), patched_probe) }
        .expect("the second install lands");
    assert_eq!(second.previous(), (other_probe as ProbeFn).imp());

    // Now ask the FIRST site about B's row. The row's current implementation
    // IS `patched_probe`, so the old `current == new_imp` test fired and
    // answered `AlreadyInstalled` with A's original.
    // SAFETY: as above.
    let asked = unsafe { SITE.install(b, sel!(probeValue), patched_probe) };
    match asked {
        Err(SwizzleError::WrongRow { claimed, asked }) => {
            assert_eq!(claimed, (a, sel!(probeValue)));
            assert_eq!(asked, (b, sel!(probeValue)));
        }
        other => panic!("expected WrongRow, got {other:?}"),
    }

    // …and the same site asked about a different SELECTOR on its own class is
    // the same defect one spelling out, so it is checked too.
    // SAFETY: as above.
    let by_selector = unsafe { SITE.install(a, sel!(description), patched_probe) };
    assert!(
        matches!(by_selector, Err(SwizzleError::WrongRow { .. }))
            || matches!(by_selector, Err(SwizzleError::EncodingMismatch { .. })),
        "a different selector on the claimed class must not be answered as \
         this site's row: {by_selector:?}"
    );

    // NOTHING WAS WRITTEN by the refusal.
    assert_eq!(SITE.original_imp(), Some((original_probe as ProbeFn).imp()));
    assert_eq!(probe_value(instance_of(b)), 2, "B still runs the patch");
}

/// KVO SWAPS THE CLASS UNDER A PATCHED OBJECT, and none of it matters.
///
/// Asked by W15 because it is the one thing in a live application that changes
/// an object's class after the fork has swizzled: `-addObserver:forKeyPath:…`
/// makes `NSKVONotifying_X`, points the instance's isa at it, and overrides
/// `-class` to LIE about that. All four consequences are measured rather than
/// argued, because three of them are the reason `crate::class_of` is the
/// accessor `override_send_event` uses and that choice had no test.
#[test]
fn a_kvo_isa_swap_leaves_the_swizzle_and_its_owner_where_they_were() {
    static SITE: SwizzleSite<ProbeFn> = SwizzleSite::new();

    // A class with a property KVO will observe. `-probeValue` is the swizzle
    // target; `-value`/`-setValue:` are what KVO needs to isa-swap at all.
    unsafe extern "C-unwind" fn get_value(_this: Id, _cmd: Sel) -> i64 {
        41
    }
    unsafe extern "C-unwind" fn set_value(_this: Id, _cmd: Sel, _v: i64) {}

    let cls = {
        let mut b = begin(c"NSObject", c"W15KvoProbe");
        b.add_rust_ivar::<()>();
        // SAFETY: both are `extern "C"` at the prototypes their encodings
        // describe, and neither can unwind.
        unsafe {
            b.add_method(
                sel!(probeValue),
                original_probe as *const c_void,
                &method_encoding!(i64),
            );
            b.add_method(
                sel!(value),
                get_value as *const c_void,
                &method_encoding!(i64),
            );
            b.add_method(
                sel!(setValue:),
                set_value as *const c_void,
                &method_encoding!(() ; i64),
            );
        }
        b.register().class()
    };
    let obj = instance_of(cls);

    // SWIZZLE FIRST, then let KVO in on top of it.
    // SAFETY: `patched_probe` has the registered prototype and cannot unwind.
    let done = unsafe { SITE.install(cls, sel!(probeValue), patched_probe) }.expect("install");
    assert_eq!(done.owner(), cls);
    assert_eq!(probe_value(obj), 2);

    // SAFETY: `-addObserver:forKeyPath:options:context:` is
    // `v40@0:8@16@24Q32^v40` on `NSObject`; `obj` observing itself is legal and
    // is the cheapest way to make the runtime build the notifying subclass.
    let before = unsafe { class_of(obj) };
    unsafe {
        let add: unsafe extern "C-unwind" fn(Id, Sel, Id, Id, usize, *const c_void) = msg();
        add(
            obj,
            sel!(addObserver:forKeyPath:options:context:),
            obj,
            ns_string("value").expect("a literal is valid UTF-8").id(),
            0,
            std::ptr::null(),
        );
    }
    // SAFETY: `obj` is live.
    let after = unsafe { class_of(obj) };
    assert_ne!(
        before, after,
        "KVO must have installed a notifying subclass"
    );

    // 1. `-class` LIES and `object_getClass` does not. This is why
    //    `override_send_event` reaches for `class_of` and not for a send, and
    //    it had no test until now.
    // SAFETY: `-class` is `#16@0:8` on `NSObject` and `obj` is live.
    let lied = unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> ClassPtr = msg();
        f(obj, sel!(class))
    };
    assert_eq!(lied, cls, "-class hides the KVO subclass");
    assert_ne!(after, cls, "object_getClass does not");

    // 2. `owning_class` walks THROUGH the notifying subclass to the definer, so
    //    a re-install through a KVO'd object still names the row the fork
    //    asserts on.
    // SAFETY: `after` is a live class object.
    assert_eq!(unsafe { owning_class(after, sel!(probeValue)) }, Some(cls));

    // 3. The patch still runs through the swapped isa.
    assert_eq!(probe_value(obj), 2);

    // 4. Re-installing THROUGH the notifying subclass is the idempotent path —
    //    the row identity is the OWNER's, not the class named — and it must not
    //    publish our own implementation as the thing to chain to.
    // SAFETY: as above.
    let again = unsafe { SITE.install(after, sel!(probeValue), patched_probe) };
    assert!(
        matches!(again, Ok(Swizzle::AlreadyInstalled { .. })),
        "a KVO subclass resolves to the claimed row: {again:?}"
    );
    assert_eq!(SITE.original_imp(), Some((original_probe as ProbeFn).imp()));
}
