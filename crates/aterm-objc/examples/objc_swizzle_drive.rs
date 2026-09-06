// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SWIZZLE DRIVER: `aterm_objc::SwizzleSite` against the REAL
//! `-[NSApplication sendEvent:]`, with the A2 tooth drawn on it.
//!
//! # Why this is a driver and not a test
//!
//! Two reasons, and both are the reasons the other five drivers exist:
//!
//! * `+[NSApplication sharedApplication]` must be called on the process main
//!   thread. libtest runs every `#[test]` body on a worker, where
//!   `pthread_main_np()` answers 0.
//! * The subject is a **live framework class in a process that has AppKit
//!   loaded**. `tests/swizzle.rs` measures the runtime's behaviour on classes of
//!   its own making, which is the right place for the mutator measurements and
//!   the refusals; it cannot ask whether the encoding this fork derives from a
//!   Rust prototype matches the one Apple shipped, because it has no Apple row
//!   to compare against.
//!
//! # THE PROOF OBLIGATION
//!
//! `crates/aterm-objc/src/swizzle.rs` makes three claims about evidence, and
//! this file discharges the two that can be discharged here:
//!
//! * **No encoding check can SEE a swizzle.** Stage 3 reads the registered
//!   string before and after the install and requires it to be UNCHANGED. That
//!   is a stage whose PASSING is the bad news — it is the measurement that
//!   makes every encoding-shaped guard in this tree decoration on this target,
//!   and it is run so that the claim is a number rather than a paragraph.
//! * **The IMP and its IMAGE are the only live evidence.** Stage 3 also asks
//!   `dladdr` which Mach-O image the implementation is in, before and after: it
//!   must move from AppKit's framework to this executable. That is part **A2**
//!   of `crates/aterm-gui/examples/objc_live_class_audit.rs`, drawn here on the
//!   capability itself rather than on the port.
//! * **The prototype check is the one nothing else can make.** Stage 2 runs it
//!   against Apple's own `v24@0:8@16`, and stage 6 shows it REFUSING a
//!   prototype that is wrong in a way no other instrument in this repository
//!   would notice.
//!
//! Stage 5 then does the thing that matters: sends a real `NSEvent` through
//! `NSApp` and requires both halves of the chain to have run.
//!
//! # WHICH STAGES CAN FAIL — plant-verified, three plants, and one stage cannot
//!
//! A stage that cannot fail is worth exactly as much as the honesty about it,
//! so each was made to fail on purpose and the result recorded:
//!
//! | plant | what fails |
//! |---|---|
//! | the encoding comparison removed from `install` | stage 6 and both halves of 6b — 4 checks |
//! | the patch does not call `original()` | stage 5's chain, and 6b's — 2 checks |
//! | the install skipped entirely | stages 3, 5, 6 and 6b — 7 checks |
//!
//! **And the measurement the whole module is built around falls out of that
//! table.** `stage3: THE ENCODING IS UNCHANGED` reported `ok` in **all three
//! plants**, including the one where the swizzle never happened at all. That is
//! not a defective stage — it is the claim, arriving as a number: an encoding
//! check cannot distinguish a patched class from an unpatched one, so it is not
//! evidence about this target and must never be counted as any. The stage
//! beside it, the IMP's image, failed in exactly the arm where it should.
//!
//! # What this file does NOT prove, on the record
//!
//! That the `'static` bound stops a stack-allocated site. That is a
//! COMPILE-time refusal and its evidence is the `compile_fail` doctest on
//! [`aterm_objc::SwizzleSite::install`], whose error text was read rather than
//! assumed (`E0597`, "argument requires that `site` is borrowed for
//! `'static`"). A driver cannot run a program that does not compile.

fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(run() as u8)
}

const PASS: i32 = 0;
const FAIL: i32 = 1;
const NOT_RUN: i32 = 2;

#[cfg(not(target_os = "macos"))]
fn run() -> i32 {
    eprintln!("objc-swizzle-drive: NOT RUN — NSApplication is a macOS subject.");
    NOT_RUN
}

#[cfg(target_os = "macos")]
fn run() -> i32 {
    macos::drive()
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{CStr, c_char, c_void};
    use std::sync::atomic::{AtomicU32, Ordering};

    use aterm_objc::swizzle::{MethodFn, Swizzle, SwizzleError, SwizzleSite, owning_class};
    use aterm_objc::{
        Bool, CGPoint, ClassPtr, Encode, Id, Obj, Sel, begin, class, class_name, method_imp,
        method_types, msg, sel,
    };

    use super::{FAIL, NOT_RUN, PASS};

    // AppKit, for `NSApplication` and `NSEvent`. `aterm-objc`'s library half
    // links Foundation only — it has no bindings and needs no framework beyond
    // the one `ns_string` reaches for. This EXAMPLE needs the real thing, so it
    // carries the link directive itself. A system framework already present in
    // every aterm process is not a dependency in the sense the crate's
    // zero-dependency rule is about; nothing is added to `Cargo.toml`.
    //
    // SAFETY: the block declares no symbol. Its only job is the link line.
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}

    /// `dladdr`, for the question part A2 asks: which IMAGE is this code in?
    ///
    /// Declared here rather than reached for through a crate: the loader is
    /// libSystem's and this is the only caller.
    #[repr(C)]
    struct DlInfo {
        dli_fname: *const c_char,
        dli_fbase: *mut c_void,
        dli_sname: *const c_char,
        dli_saddr: *mut c_void,
    }

    // SAFETY: `dladdr` is libSystem's, loaded in every process on this
    // platform.
    unsafe extern "C" {
        fn dladdr(addr: *const c_void, info: *mut DlInfo) -> i32;
    }

    /// The Mach-O image a code address lives in, if the loader knows.
    fn image_of(addr: *const c_void) -> Option<String> {
        let mut info = DlInfo {
            dli_fname: std::ptr::null(),
            dli_fbase: std::ptr::null_mut(),
            dli_sname: std::ptr::null(),
            dli_saddr: std::ptr::null_mut(),
        };
        // SAFETY: `addr` is a code address the runtime handed out and is never
        // dereferenced; `dladdr` reads the loader's own tables and writes the
        // struct.
        let ok = unsafe { dladdr(addr, &raw mut info) };
        if ok == 0 || info.dli_fname.is_null() {
            return None;
        }
        // SAFETY: a non-null, NUL-terminated path the loader owns.
        Some(
            unsafe { CStr::from_ptr(info.dli_fname) }
                .to_string_lossy()
                .into_owned(),
        )
    }

    fn is_framework_image(image: &str) -> bool {
        image.contains("/System/Library/") || image.contains(".framework/")
    }

    /// Whether this thread is the process main thread, asked of Foundation
    /// directly rather than through the crate's own witness — a stage that used
    /// the type under test could not catch it being wrong.
    fn is_main_thread() -> bool {
        // SAFETY: `+[NSThread isMainThread]` is a side-effect-free class-method
        // `BOOL` query and the cast is exactly `-(BOOL)(id, SEL)`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> Bool = msg();
            f(class(c"NSThread").as_id(), sel!(isMainThread)).as_bool()
        }
    }

    // -----------------------------------------------------------------------
    // THE PATCH. Exactly `override_send_event`'s shape: the fork's behaviour,
    // then the chain.
    // -----------------------------------------------------------------------

    /// `-(void)sendEvent:(NSEvent *)event`, as the runtime enters it.
    type SendEvent = unsafe extern "C-unwind" fn(Id, Sel, Id);

    static SEND_EVENT: SwizzleSite<SendEvent> = SwizzleSite::new();
    static PATCH_RUNS: AtomicU32 = AtomicU32::new(0);
    static CHAIN_RUNS: AtomicU32 = AtomicU32::new(0);

    /// The fork's `-sendEvent:`.
    ///
    /// Unwinding out of an Objective-C frame is undefined behaviour, so this
    /// catches and aborts exactly as `declare_class!`'s trampolines do — the
    /// obligation `SwizzleSite::install`'s `# Safety` names and the one the
    /// encoding check cannot discharge.
    unsafe extern "C-unwind" fn patched_send_event(this: Id, cmd: Sel, event: Id) {
        let guard = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            PATCH_RUNS.fetch_add(1, Ordering::Relaxed);
            if let Some(original) = SEND_EVENT.original() {
                CHAIN_RUNS.fetch_add(1, Ordering::Relaxed);
                // SAFETY: the install verified this prototype against the
                // encoding the runtime holds for the row, so the IMP has this
                // shape. Entering an IMP directly is NOT a message send — nil
                // is not short-circuited — and `this` is the live `NSApp` the
                // runtime just dispatched to.
                unsafe { original(this, cmd, event) };
            }
        }));
        if guard.is_err() {
            aterm_objc::abort_on_unwind("sendEvent:");
        }
    }

    /// A prototype that is WRONG in a way only the encoding check can see: a
    /// `BOOL` return where AppKit registered `void`. Never installed.
    unsafe extern "C-unwind" fn wrong_shape(_this: Id, _cmd: Sel, _event: Id) -> Bool {
        Bool::from(false)
    }
    type WrongSendEvent = unsafe extern "C-unwind" fn(Id, Sel, Id) -> Bool;

    // -----------------------------------------------------------------------

    pub fn drive() -> i32 {
        let mut failures: Vec<String> = Vec::new();
        let mut check = |ok: bool, what: &str| {
            if ok {
                eprintln!("  ok   {what}");
            } else {
                eprintln!("  FAIL {what}");
                failures.push(what.to_owned());
            }
        };

        if !is_main_thread() {
            eprintln!("objc-swizzle-drive: NOT RUN — main() is not on the main thread.");
            return NOT_RUN;
        }

        let app_class = class(c"NSApplication");
        if app_class.is_null() {
            eprintln!("objc-swizzle-drive: NOT RUN — AppKit is not loaded in this process.");
            return NOT_RUN;
        }

        // -- STAGE 1: the premise, on the REAL class ----------------------
        eprintln!("stage 1: the row exists, and it does NOT live where it was asked for");
        // SAFETY: `app_class` is the live `NSApplication` class object.
        let owner = unsafe { owning_class(app_class, sel!(sendEvent:)) };
        match owner {
            Some(o) => {
                // SAFETY: `o` is a live class object.
                let name = unsafe { class_name(o) }.to_string_lossy().into_owned();
                eprintln!("       -sendEvent: is defined on {name}");
                check(
                    name == "NSApplication",
                    "stage1: the row's owner is NSApplication itself",
                );
            }
            None => check(false, "stage1: NSApplication has a -sendEvent: at all"),
        }
        // A subclass that does NOT override: the blast radius, on Apple's class
        // rather than on one of ours.
        let mut sub = begin(c"NSApplication", c"AtermSwizzleDriveApp");
        sub.add_rust_ivar::<()>();
        let sub_class = sub.register().class();
        // SAFETY: `sub_class` was just registered and is live.
        let sub_owner = unsafe { owning_class(sub_class, sel!(sendEvent:)) };
        check(
            sub_owner == Some(app_class),
            "stage1: a subclass that does not override resolves to NSApplication — patching \
             through it would change every application object in the process",
        );

        // -- STAGE 2: the check nothing else in this tree can make ---------
        eprintln!("stage 2: Apple's registered encoding against one derived from the Rust type");
        // SAFETY: `app_class` is live.
        let registered = unsafe { method_types(app_class, sel!(sendEvent:)) };
        let stripped = registered
            .as_deref()
            .map(aterm_objc::strip_method_offsets)
            .unwrap_or_default();
        let derived = <SendEvent as MethodFn>::method_encoding();
        eprintln!("       registered {registered:?} -> stripped {stripped:?}");
        eprintln!(
            "       derived from `unsafe extern \"C-unwind\" fn(Id, Sel, Id)` -> {derived:?}"
        );
        check(
            stripped == derived,
            "stage2: the two readings agree, and they do NOT move together — one is AppKit's \
             string, the other is generated from this fork's types",
        );

        // -- STAGE 3: the install, and the ONLY evidence it happened -------
        eprintln!(
            "stage 3: install, then the two questions — encoding (blind) and image (the tooth)"
        );
        // SAFETY: `app_class` is live.
        let before_imp = unsafe { method_imp(app_class, sel!(sendEvent:)) };
        let before_image = before_imp.and_then(image_of);
        eprintln!("       before: IMP {before_imp:?} in {before_image:?}");
        check(
            before_image.as_deref().is_some_and(is_framework_image),
            "stage3: before the install, the implementation is AppKit's own",
        );

        // SAFETY: `patched_send_event` is `extern "C"` with the prototype the
        // install re-checks against the registered encoding, and it catches and
        // aborts rather than unwinding out of the Objective-C frame.
        let outcome =
            unsafe { SEND_EVENT.install(app_class, sel!(sendEvent:), patched_send_event) };
        match outcome {
            Ok(Swizzle::Replaced { owner, previous }) => {
                // SAFETY: `owner` is a live class object.
                let name = unsafe { class_name(owner) }.to_string_lossy().into_owned();
                eprintln!("       replaced on {name}, displacing {previous:?}");
                check(
                    name == "NSApplication",
                    "stage3: the owner reported is real",
                );
                check(
                    image_of(previous.as_ptr())
                        .as_deref()
                        .is_some_and(is_framework_image),
                    "stage3: the displaced implementation is AppKit's",
                );
            }
            other => {
                check(false, "stage3: the install replaced the row");
                eprintln!("       got {other:?}");
            }
        }

        // SAFETY: `app_class` is live.
        let after = unsafe { method_types(app_class, sel!(sendEvent:)) };
        check(
            after == registered,
            "stage3: THE ENCODING IS UNCHANGED — this stage PASSING is the bad news: it is the \
             measurement that makes every encoding-shaped guard in this tree blind on this target",
        );
        // SAFETY: `app_class` is live.
        let after_imp = unsafe { method_imp(app_class, sel!(sendEvent:)) };
        let after_image = after_imp.and_then(image_of);
        eprintln!("       after:  IMP {after_imp:?} in {after_image:?}");
        check(
            after_image
                .as_deref()
                .is_some_and(|i| !is_framework_image(i)),
            "stage3: THE TOOTH — the runtime will now call code in this executable, not in a \
             framework. This is the only live evidence the swizzle happened",
        );
        check(
            after_imp.is_some_and(|p| std::ptr::eq(p, patched_send_event as *const c_void)),
            "stage3: and it is OUR function, by address",
        );

        // -- STAGE 4: the shared application, built for real ---------------
        eprintln!("stage 4: +[NSApplication sharedApplication] on the patched class");
        // SAFETY: `+sharedApplication` is `@#:` and is the documented accessor
        // for the one global application object; this is the main thread.
        let app: Id = unsafe {
            let send: unsafe extern "C-unwind" fn(Id, Sel) -> Id = msg();
            send(app_class.as_id(), sel!(sharedApplication))
        };
        if app.is_null() {
            eprintln!("objc-swizzle-drive: NOT RUN — there is no shared NSApplication.");
            return NOT_RUN;
        }
        check(true, "stage4: NSApp exists");

        // -- STAGE 5: a REAL event through the REAL dispatch ---------------
        eprintln!("stage 5: -[NSApp sendEvent:] must enter our IMP and chain to AppKit's");
        PATCH_RUNS.store(0, Ordering::Relaxed);
        CHAIN_RUNS.store(0, Ordering::Relaxed);
        match dummy_event() {
            Some(event) => {
                // SAFETY: `-sendEvent:` is `v@:@`, `app` is the live NSApp and
                // `event` a live `NSEvent` this frame holds a +1 on. This is an
                // ordinary message send, so it goes through the swizzled row
                // the way AppKit's own dispatch does.
                unsafe {
                    let send: unsafe extern "C-unwind" fn(Id, Sel, Id) = msg();
                    send(app, sel!(sendEvent:), event.id());
                }
                check(
                    PATCH_RUNS.load(Ordering::Relaxed) == 1,
                    "stage5: the runtime entered the fork's implementation",
                );
                check(
                    CHAIN_RUNS.load(Ordering::Relaxed) == 1,
                    "stage5: and it chained to the implementation the site published",
                );
            }
            None => check(false, "stage5: a dummy NSEvent could be constructed"),
        }

        // -- STAGE 6: the refusals, on the LIVE row ------------------------
        eprintln!("stage 6: the encoding check refuses a prototype nothing else could catch");
        static WRONG: SwizzleSite<WrongSendEvent> = SwizzleSite::new();
        // SAFETY: `wrong_shape` is `extern "C"` and does not unwind; it is the
        // thing the install is expected to refuse.
        let refusal = unsafe { WRONG.install(app_class, sel!(sendEvent:), wrong_shape) };
        match refusal {
            Err(SwizzleError::EncodingMismatch {
                registered,
                expected,
            }) => {
                eprintln!("       refused: runtime {registered:?} vs derived {expected:?}");
                check(
                    registered == stripped
                        && expected == <WrongSendEvent as MethodFn>::method_encoding(),
                    "stage6: refused, with both readings reported",
                );
            }
            other => {
                check(false, "stage6: a wrong-return-width prototype was REFUSED");
                eprintln!("       got {other:?}");
            }
        }
        // SAFETY: `app_class` is live.
        check(
            unsafe { method_imp(app_class, sel!(sendEvent:)) }
                .is_some_and(|p| std::ptr::eq(p, patched_send_event as *const c_void)),
            "stage6: and the refusal wrote NOTHING — our implementation is still installed",
        );

        eprintln!("stage 6b: a second install of the same implementation is a no-op");
        // SAFETY: identical to the first install.
        let again = unsafe { SEND_EVENT.install(app_class, sel!(sendEvent:), patched_send_event) };
        match again {
            Ok(Swizzle::AlreadyInstalled { previous, .. }) => {
                check(
                    image_of(previous.as_ptr())
                        .as_deref()
                        .is_some_and(is_framework_image),
                    "stage6b: the ORIGINAL is still AppKit's — a second install must not publish \
                     OUR implementation as the thing to chain to, which is the loop that would \
                     make",
                );
            }
            other => {
                check(
                    false,
                    "stage6b: the second install reported AlreadyInstalled",
                );
                eprintln!("       got {other:?}");
            }
        }
        // And the chain still works after the idempotent re-entry.
        PATCH_RUNS.store(0, Ordering::Relaxed);
        CHAIN_RUNS.store(0, Ordering::Relaxed);
        if let Some(event) = dummy_event() {
            // SAFETY: as stage 5.
            unsafe {
                let send: unsafe extern "C-unwind" fn(Id, Sel, Id) = msg();
                send(app, sel!(sendEvent:), event.id());
            }
        }
        check(
            PATCH_RUNS.load(Ordering::Relaxed) == 1 && CHAIN_RUNS.load(Ordering::Relaxed) == 1,
            "stage6b: and the chain is intact after it",
        );

        if failures.is_empty() {
            eprintln!("objc-swizzle-drive: PASS");
            PASS
        } else {
            eprintln!("objc-swizzle-drive: FAIL ({} stage(s))", failures.len());
            for f in &failures {
                eprintln!("  - {f}");
            }
            FAIL
        }
    }

    /// An application-defined `NSEvent`, +1.
    ///
    /// The same constructor `vendor/winit`'s `event.rs` uses, with the same
    /// arguments; it answers a +0 AUTORELEASED event, so the result is retained
    /// into an [`Obj`] rather than wrapped.
    fn dummy_event() -> Option<Obj> {
        /// `NSEventTypeApplicationDefined`.
        const APPLICATION_DEFINED: usize = 15;
        /// `NSEventSubtypeWindowExposed`.
        const WINDOW_EXPOSED: i16 = 0;

        // SAFETY: the receiver is the live `NSEvent` class object and the cast
        // is the prototype `method_getTypeEncoding` reports for
        // `+otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:`
        // — note `subtype:` is a SIGNED short.
        let raw = unsafe {
            let send: unsafe extern "C-unwind" fn(
                Id,
                Sel,
                usize,
                CGPoint,
                usize,
                f64,
                isize,
                Id,
                i16,
                isize,
                isize,
            ) -> Id = msg();
            send(
                class(c"NSEvent").as_id(),
                sel!(
                    otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:
                ),
                APPLICATION_DEFINED,
                CGPoint { x: 0.0, y: 0.0 },
                0,
                0.0,
                0,
                Id::NIL,
                WINDOW_EXPOSED,
                0,
                0,
            )
        };
        // SAFETY: the constructor answers a +0 autoreleased event or nil;
        // `Obj::retain` takes the +1 this frame needs and answers `None` for
        // nil.
        unsafe { Obj::retain(raw) }
    }

    /// Kept so the `Encode` bound is visibly the same one the rest of the crate
    /// uses; the compiler would otherwise let this import rot.
    const _: () = {
        assert!(<Id as Encode>::ENCODING.as_bytes()[0] == b'@');
        assert!(<ClassPtr as Encode>::ENCODING.as_bytes()[0] == b'#');
    };
}
