// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `SwizzleSite::install`'s refusals and chaining, and `owning_class`, against
//! classes registered here.
//!
//! The third-party patches the refusals exist for are applied through this
//! file's own `method_setImplementation` binding, so the module under test
//! cannot vouch for itself. These run on libtest workers, which is fine: class
//! registration is not gated by [`aterm_objc::MainThread`], and these classes
//! touch no AppKit state and hold no ivar with a destructor.

#![cfg(target_os = "macos")]
#![expect(
    clippy::disallowed_methods,
    reason = "aterm-objc's own runtime tests drive the bare `msg` cast; everything else uses checked_msg"
)]

use std::ffi::{CStr, c_void};
use std::sync::atomic::{AtomicU32, Ordering};

use aterm_objc::swizzle::{MethodFn, Swizzle, SwizzleError, SwizzleSite, owning_class};
use aterm_objc::{
    Bool, CGRect, ClassPtr, Encode, Id, NSRange, Sel, begin, class_of, method_encoding, method_imp,
    method_types, msg, ns_string, sel,
};

// The runtime, bound here so the third-party patches are independent of the
// module under test.
//
// SAFETY: every symbol below is libobjc's, loaded in every process on this
// platform.
unsafe extern "C" {
    fn class_getInstanceMethod(cls: ClassPtr, name: Sel) -> *const c_void;
    fn method_setImplementation(method: *const c_void, imp: *const c_void) -> *const c_void;
}

// Probe implementations: each records that it ran and returns a distinct value.

/// Process-global counters, read only for their value: libtest is parallel, so
/// the one test that counts calls has its own private pair below.
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

/// `-(int64_t)probeValue`: the original.
unsafe extern "C-unwind" fn original_probe(_this: Id, _cmd: Sel) -> i64 {
    ORIGINAL_RUNS.fetch_add(1, Ordering::Relaxed);
    1
}

/// `-(int64_t)probeValue`: the patch.
unsafe extern "C-unwind" fn patched_probe(_this: Id, _cmd: Sel) -> i64 {
    PATCHED_RUNS.fetch_add(1, Ordering::Relaxed);
    2
}

/// A second patch, for the displacement case.
unsafe extern "C-unwind" fn other_probe(_this: Id, _cmd: Sel) -> i64 {
    3
}

/// A patch with the wrong return width, the shape the encoding check refuses. Never installed by
/// a passing test.
unsafe extern "C-unwind" fn wrong_shape(_this: Id, _cmd: Sel) -> Bool {
    Bool::from(true)
}

type ProbeFn = unsafe extern "C-unwind" fn(Id, Sel) -> i64;
type WrongFn = unsafe extern "C-unwind" fn(Id, Sel) -> Bool;

/// Register a fresh `NSObject` subclass with `-probeValue`. The name must be
/// unique per test (`begin` panics on a duplicate).
fn register_probe_class(name: &'static CStr) -> ClassPtr {
    register_probe_class_with(name, original_probe)
}

/// As [`register_probe_class`], with the implementation named, so a test that
/// counts calls can own its counter.
fn register_probe_class_with(name: &'static CStr, imp: ProbeFn) -> ClassPtr {
    let mut b = begin(c"NSObject", name);
    // `ClassBuilder::register` asserts the ivar exists; `()` is the empty payload.
    b.add_rust_ivar::<()>();
    // SAFETY: `imp` has the `(Id, Sel) -> i64` prototype its encoding describes and cannot
    // unwind (one atomic, then a constant).
    unsafe {
        b.add_method(
            sel!(probeValue),
            imp as *const c_void,
            &method_encoding!(i64),
        );
    }
    b.register().class()
}

/// A fresh instance of `cls`, +1, leaked: these classes hold no ivar with a destructor, and the
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

// Part 1 — the owner: the class you name is not the class you patch.

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

// Part 2 — the encoding derived from the prototype.

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
fn install_refuses_a_prototype_the_registered_encoding_does_not_match() {
    static SITE: SwizzleSite<WrongFn> = SwizzleSite::new();
    let cls = register_probe_class(c"AtermSwizzleMismatch");

    // SAFETY: `wrong_shape` cannot unwind; the install must refuse it (it returns `BOOL` where
    // the row is `q`).
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

    // SAFETY: the patch cannot unwind; the selector is not implemented, which is the refusal
    // under test.
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

// Part 3 — the site: install, chain, idempotence, and the refusals that stop a
// chain being built from someone else's implementation.

#[test]
fn install_replaces_publishes_the_original_and_reports_the_owner() {
    static SITE: SwizzleSite<ProbeFn> = SwizzleSite::new();
    let parent = register_probe_class_with(c"AtermSwizzleSite", site_original_probe);
    let child = {
        let mut b = begin(c"AtermSwizzleSite", c"AtermSwizzleSiteChild");
        b.add_rust_ivar::<()>();
        b.register().class()
    };

    // Ask for the child; the row lives on the parent.
    // SAFETY: `site_patched_probe` has the row's `(Id, Sel) -> i64` prototype
    // (re-checked by the install) and cannot unwind.
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
    // SAFETY: the install verified this prototype. An IMP entered directly is
    // not a message send, so the receiver must be live — it is.
    let chained = unsafe { original(instance_of(parent), sel!(probeValue)) };
    assert_eq!(1, chained);
    assert_eq!(1, SITE_ORIGINAL_RUNS.load(Ordering::Relaxed));

    // The IMP the runtime will call is ours (the question the live auditor's
    // part A2 asks of AppKit).
    // SAFETY: `parent` is a live class.
    let live = unsafe { method_imp(parent, sel!(probeValue)) }.expect("registered");
    assert!(std::ptr::eq(live, site_patched_probe as *const c_void));

    // The encoding did not move, which is why part A2 resolves the IMP instead.
    // SAFETY: `parent` is live.
    assert_eq!(
        Some("q@:".to_owned()),
        unsafe { method_types(parent, sel!(probeValue)) },
        "no encoding check in this tree can tell this class was patched"
    );

    // Idempotent re-entry.
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

    // A third party swizzles on top, through the raw runtime.
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

    // Someone else installs our implementation without the site, so the
    // original is gone and the runtime cannot give it back.
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

// Part 4 — one site asked about two rows, and KVO swapping an object's class
// under the patch.

/// One site asked about a second row is refused, not answered with the first
/// row's original. Deciding "already installed?" from the row alone would
/// report `A`'s original for `B` (same encoding, so nothing else could see it).
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

    // A different site patches B's row to the same implementation.
    // SAFETY: as above.
    let second = unsafe { OTHER.install(b, sel!(probeValue), patched_probe) }
        .expect("the second install lands");
    assert_eq!(second.previous(), (other_probe as ProbeFn).imp());

    // Ask the first site about B's row, which now holds `patched_probe`.
    // SAFETY: as above.
    let asked = unsafe { SITE.install(b, sel!(probeValue), patched_probe) };
    match asked {
        Err(SwizzleError::WrongRow { claimed, asked }) => {
            assert_eq!(claimed, (a, sel!(probeValue)));
            assert_eq!(asked, (b, sel!(probeValue)));
        }
        other => panic!("expected WrongRow, got {other:?}"),
    }

    // The same site asked about a different selector on its own class.
    // SAFETY: as above.
    let by_selector = unsafe { SITE.install(a, sel!(description), patched_probe) };
    assert!(
        matches!(by_selector, Err(SwizzleError::WrongRow { .. }))
            || matches!(by_selector, Err(SwizzleError::EncodingMismatch { .. })),
        "a different selector on the claimed class must not be answered as \
         this site's row: {by_selector:?}"
    );

    // The refusal wrote nothing.
    assert_eq!(
        SITE.original().map(|f| f.imp()),
        Some((original_probe as ProbeFn).imp())
    );
    assert_eq!(probe_value(instance_of(b)), 2, "B still runs the patch");
}

/// KVO swapping the class under a patched object changes nothing that matters.
///
/// `-addObserver:forKeyPath:…` creates `NSKVONotifying_X`, repoints the
/// instance's isa, and overrides `-class` to hide it. This pins why
/// `override_send_event` uses `class_of` (`object_getClass`) rather than a
/// `-class` send.
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
        // SAFETY: each has the prototype its encoding describes, and none can unwind.
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

    // Swizzle first, then let KVO in on top.
    // SAFETY: `patched_probe` has the registered prototype and cannot unwind.
    let done = unsafe { SITE.install(cls, sel!(probeValue), patched_probe) }.expect("install");
    assert_eq!(done.owner(), cls);
    assert_eq!(probe_value(obj), 2);

    // SAFETY: `-addObserver:forKeyPath:options:context:` is
    // `v40@0:8@16@24Q32^v40` on `NSObject`; `obj` observing itself is legal and
    // makes the runtime build the notifying subclass.
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

    // 1. `-class` lies and `object_getClass` does not.
    // SAFETY: `-class` is `#16@0:8` on `NSObject` and `obj` is live.
    let lied = unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> ClassPtr = msg();
        f(obj, sel!(class))
    };
    assert_eq!(lied, cls, "-class hides the KVO subclass");
    assert_ne!(after, cls, "object_getClass does not");

    // 2. `owning_class` walks through the notifying subclass to the definer.
    // SAFETY: `after` is a live class object.
    assert_eq!(unsafe { owning_class(after, sel!(probeValue)) }, Some(cls));

    // 3. The patch still runs through the swapped isa.
    assert_eq!(probe_value(obj), 2);

    // 4. Re-installing through the notifying subclass is the idempotent path
    //    (the row identity is the owner's) and must not publish our own
    //    implementation as the original.
    // SAFETY: as above.
    let again = unsafe { SITE.install(after, sel!(probeValue), patched_probe) };
    assert!(
        matches!(again, Ok(Swizzle::AlreadyInstalled { .. })),
        "a KVO subclass resolves to the claimed row: {again:?}"
    );
    assert_eq!(
        SITE.original().map(|f| f.imp()),
        Some((original_probe as ProbeFn).imp())
    );
}
