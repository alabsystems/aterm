// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! The event driver: every `NSEvent`-taking row of the ported `WinitView`, and the
//! `NSApplication` `sendEvent:` override, entered with a real `NSEvent` of every
//! type AppKit can deliver to it.
//!
//! It hunts a trampoline sending an accessor the event's type does not answer.
//! AppKit raises `NSInternalInconsistencyException` ("Invalid message sent to
//! event") for a key-only accessor such as `-keyCode` on a mouse event, and that
//! foreign exception cannot unwind through a Rust frame.
//!
//! # What it drives
//!
//! A real `NSWindow` and `WinitView` through winit's public API. For each
//! event-taking row, one `NSEvent` per type the row can receive, built by AppKit's
//! factories (`+mouseEventWithType:…`, `+keyEventWithType:…`,
//! `+enterExitEventWithType:…`, `+otherEventWithType:…`) and, where the type has
//! one, by CoreGraphics wrapped with `+eventWithCGEvent:` (`CGEventCreateMouseEvent`,
//! `CGEventCreateScrollWheelEvent2`, `CGEventCreateKeyboardEvent`, and a bare
//! `CGEventCreate` retyped with `CGEventSetType`). Hardware events are CG-backed and
//! AppKit's synthesised ones are not, so both shapes are sent, through
//! `objc_msgSend` as AppKit enters an IMP.
//!
//! A last stage sends each kind of event through `-[NSApplication sendEvent:]`
//! (`app.rs`'s override), the gesture family included: the view has no magnify,
//! smart-magnify, rotate or pressure rows, so `Gesture` (29), `Magnify` (30) and
//! `Rotate` (18), retyped bare `CGEvent`s (`docs/measured/nsevent-accessors/`), are
//! sent only there, and AppKit passes them up the responder chain past the view.
//!
//! It asserts survival plus winit's promised relations: `mouseMoved:` inside the
//! view yields `CursorMoved`, `mouseDown:`/`mouseUp:` `MouseInput`, `scrollWheel:`
//! `MouseWheel`, `mouseEntered:`/`mouseExited:` `CursorEntered`/`CursorLeft`, and
//! `keyDown:` `KeyboardInput`.
//!
//! # The two controls
//!
//! The first stage re-executes this binary as two children, one per half of the
//! trampoline's policy:
//!
//! * **Exception control.** A [`aterm_objc::declare_class!`] class whose `poke:`
//!   sends `-keyCode` to a `MouseMoved` event must be contained: exit `0`, one
//!   stderr line naming `poke:` and `NSInternalInconsistencyException`
//!   (`aterm_objc::exception`'s default sink), and a later send on the same object
//!   answering.
//! * **Panic control.** `panicInside` panics in Rust, which `@catch (id)` must not
//!   swallow: the child must abort, as exit `3` through the signal trap (or a raw
//!   `SIGABRT`), with `abort_on_unwind`'s message on stderr.
//!
//! # Exit contract (read by `aterm_verify::stages::OBJC_DRIVES`)
//!
//! | code | meaning |
//! |---|---|
//! | 0 | every row survived every event it can receive, the relations held, the exception control was contained and the panic control aborted |
//! | 1 | a relation failed, an event could not be built, or a control answered the wrong way (the exception control aborted or was not named; the panic control survived) |
//! | 2 | not run: no event loop, no window, no view, or a control could not be spawned. Never a pass. |
//! | 3 | the finding: a row aborted on an event it can receive |
//! | killed by a signal | the same finding, when the trap could not turn it into `3` |
//!
//! An uncaught abort makes macOS write a DiagnosticReport and show "quit
//! unexpectedly", so before driving anything the driver traps the fatal signals: the
//! handler writes the row being driven (a static buffer the main thread fills before
//! each send, read with async-signal-safe calls) and `_exit`s with `3`, which the
//! crash reporter does not see. `verify appkit` reads `3` as a failure, not
//! could-not-run: for this driver the abort is the finding.
#![allow(clippy::disallowed_types, reason = "an example driver, never shipped")]

#[cfg(target_os = "macos")]
const PASS: i32 = 0;
#[cfg(target_os = "macos")]
const FAIL: i32 = 1;
const NOT_RUN: i32 = 2;
/// A row aborted; the driver's own signal trap turned the abort into this exit.
#[cfg(target_os = "macos")]
const ABORTED: i32 = 3;

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "objc-event-drive: NOT RUN — this drives \
         crates/aterm-window/src/platform_impl/macos/view.rs, which does not exist off macOS."
    );
    std::process::ExitCode::from(NOT_RUN as u8)
}

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    if std::env::var_os(macos::CONTROL_ENV).is_some() {
        return std::process::ExitCode::from(macos::control_child() as u8);
    }
    std::process::ExitCode::from(macos::run() as u8)
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_void;
    use std::time::{Duration, Instant};

    use aterm_objc::{
        Bool, CGPoint, CGRect, Id, MainThread, Sel, autoreleasepool, checked_msg, class, ns_string,
        sel,
    };
    use winit::application::ApplicationHandler;
    use winit::event::{ElementState, WindowEvent};
    use winit::event_loop::ActiveEventLoop;
    use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
    use winit::window::{Window, WindowId};

    use super::{ABORTED, FAIL, NOT_RUN, PASS};

    /// The fatal-signal trap: turn the abort a row's trampoline performs into
    /// `exit 3` with the row's name on stdout, instead of a crash dialog.
    ///
    /// Everything the handler touches is async-signal-safe: `write(2)`,
    /// `_exit(2)`, and a static byte buffer the main thread filled before the
    /// send. The signal is delivered to the faulting thread, which is the main
    /// thread — the only thread that ever writes the buffer — so there is no
    /// race to guard.
    mod trap {
        use std::cell::UnsafeCell;
        use std::ffi::{c_int, c_void};
        use std::sync::atomic::{AtomicUsize, Ordering};

        unsafe extern "C" {
            fn signal(sig: c_int, handler: usize) -> usize;
            fn _exit(code: c_int) -> !;
            fn write(fd: c_int, buf: *const c_void, n: usize) -> isize;
        }

        const SIGILL: c_int = 4;
        const SIGTRAP: c_int = 5;
        const SIGABRT: c_int = 6;
        const SIGBUS: c_int = 10;
        const SIGSEGV: c_int = 11;

        const CAP: usize = 192;

        struct RowBuf(UnsafeCell<[u8; CAP]>);
        // SAFETY: written only by the main thread between sends, read only by
        // a handler running on that same thread.
        unsafe impl Sync for RowBuf {}

        static ROW: RowBuf = RowBuf(UnsafeCell::new([0; CAP]));
        static ROW_LEN: AtomicUsize = AtomicUsize::new(0);

        /// Name the row about to be driven, for the handler to report.
        pub fn set_row(label: &str) {
            let bytes = label.as_bytes();
            let n = bytes.len().min(CAP);
            // SAFETY: main thread only, see `RowBuf`.
            unsafe {
                let buf = &mut *ROW.0.get();
                buf[..n].copy_from_slice(&bytes[..n]);
            }
            ROW_LEN.store(n, Ordering::SeqCst);
        }

        extern "C" fn on_fatal(sig: c_int) {
            const HEAD: &[u8] = b"\nobjc-event-drive: ABORTED (signal ";
            const MID: &[u8] = b") while driving: ";
            let mut digits = [0u8; 4];
            let mut k = 0;
            let mut v = if sig < 0 { 0 } else { sig as u32 };
            if v == 0 {
                digits[0] = b'0';
                k = 1;
            }
            while v > 0 && k < digits.len() {
                digits[k] = b'0' + (v % 10) as u8;
                v /= 10;
                k += 1;
            }
            digits[..k].reverse();
            let n = ROW_LEN.load(Ordering::SeqCst);
            // SAFETY: the calls are async-signal-safe; the buffer is the
            // static filled by `set_row` on this thread.
            unsafe {
                let _ = write(1, HEAD.as_ptr().cast(), HEAD.len());
                let _ = write(1, digits.as_ptr().cast(), k);
                let _ = write(1, MID.as_ptr().cast(), MID.len());
                let _ = write(1, (*ROW.0.get()).as_ptr().cast(), n);
                let _ = write(1, b"\n".as_ptr().cast(), 1);
                _exit(super::ABORTED);
            }
        }

        /// Name the row that never returned and leave with the abort code. Called
        /// from the hang watchdog thread, not a signal handler, but with the same raw
        /// writes, so it works whatever state the stuck main thread is in.
        pub fn report_hang_and_exit() -> ! {
            const HEAD: &[u8] =
                b"\nobjc-event-drive: HUNG (row never returned within the budget) while driving: ";
            let n = ROW_LEN.load(Ordering::SeqCst);
            // SAFETY: raw writes of static bytes and the row buffer, then
            // `_exit`; the buffer is only ever written by the main thread,
            // which is the thread that is stuck.
            unsafe {
                let _ = write(1, HEAD.as_ptr().cast(), HEAD.len());
                let _ = write(1, (*ROW.0.get()).as_ptr().cast(), n);
                let _ = write(1, b"\n".as_ptr().cast(), 1);
                _exit(super::ABORTED);
            }
        }

        /// Install `on_fatal` for every signal an abort or a fault raises.
        pub fn install() {
            for sig in [SIGABRT, SIGSEGV, SIGBUS, SIGILL, SIGTRAP] {
                // SAFETY: `signal(2)` with a plain C handler; BSD semantics
                // keep it installed after delivery.
                unsafe {
                    signal(sig, on_fatal as extern "C" fn(c_int) as usize);
                }
            }
        }
    }

    /// Set in the environment of the re-executed child that runs a control:
    /// [`CONTROL_EXCEPTION`] or [`CONTROL_PANIC`].
    pub const CONTROL_ENV: &str = "ATERM_OBJC_EVENT_DRIVE_CONTROL";
    /// The exception control: a wrong-type send, which must be contained.
    const CONTROL_EXCEPTION: &str = "exception";
    /// The panic control: a Rust panic, which must still abort.
    const CONTROL_PANIC: &str = "panic";

    /// Long enough for macOS to launch its `NSApplication`, hand back a window
    /// and run every stage; past it the drive reports NOT RUN.
    const BUDGET: Duration = Duration::from_secs(60);
    /// How long the control child may take to abort before it is a hang.
    const CONTROL_WATCHDOG: Duration = Duration::from_secs(20);
    /// `SIGABRT`.
    const SIGABRT: i32 = 6;

    // `NSEventType` (NSEvent.h). The CG types AppKit maps 1:1 share the
    // numbers, which is why one table serves both factories.
    const LEFT_MOUSE_DOWN: usize = 1;
    const LEFT_MOUSE_UP: usize = 2;
    const RIGHT_MOUSE_DOWN: usize = 3;
    const RIGHT_MOUSE_UP: usize = 4;
    const MOUSE_MOVED: usize = 5;
    const LEFT_MOUSE_DRAGGED: usize = 6;
    const RIGHT_MOUSE_DRAGGED: usize = 7;
    const MOUSE_ENTERED: usize = 8;
    const MOUSE_EXITED: usize = 9;
    const KEY_DOWN: usize = 10;
    const KEY_UP: usize = 11;
    const FLAGS_CHANGED: usize = 12;
    const APPKIT_DEFINED: usize = 13;
    const SYSTEM_DEFINED: usize = 14;
    const APPLICATION_DEFINED: usize = 15;
    const PERIODIC: usize = 16;
    const CURSOR_UPDATE: usize = 17;
    const ROTATE: usize = 18;
    const OTHER_MOUSE_DOWN: usize = 25;
    const OTHER_MOUSE_UP: usize = 26;
    const OTHER_MOUSE_DRAGGED: usize = 27;
    const GESTURE: usize = 29;
    const MAGNIFY: usize = 30;

    /// `NSEventModifierFlagShift`.
    const NS_SHIFT: usize = 1 << 17;
    /// The `kVK_Shift` virtual key.
    const VK_SHIFT: u16 = 56;
    /// The `kVK_ANSI_A` virtual key.
    const VK_A: u16 = 0;

    /// `CGMouseButton`.
    const CG_BUTTON_LEFT: u32 = 0;
    const CG_BUTTON_RIGHT: u32 = 1;
    const CG_BUTTON_CENTER: u32 = 2;
    /// `CGScrollEventUnit`.
    const CG_SCROLL_UNIT_PIXEL: u32 = 0;
    const CG_SCROLL_UNIT_LINE: u32 = 1;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreate(source: *const c_void) -> *mut c_void;
        fn CGEventCreateMouseEvent(
            source: *const c_void,
            mouse_type: u32,
            position: CGPoint,
            button: u32,
        ) -> *mut c_void;
        fn CGEventCreateScrollWheelEvent2(
            source: *const c_void,
            units: u32,
            wheel_count: u32,
            wheel1: i32,
            wheel2: i32,
            wheel3: i32,
        ) -> *mut c_void;
        fn CGEventCreateKeyboardEvent(
            source: *const c_void,
            keycode: u16,
            keydown: bool,
        ) -> *mut c_void;
        fn CGEventSetType(event: *mut c_void, kind: u32);
        fn CGMainDisplayID() -> u32;
        fn CGDisplayBounds(display: u32) -> CGRect;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: *const c_void);
    }

    // ------------------------------------------------------------- factories
    //
    // AppKit's own constructors as typed `objc_msgSend` casts; every result is
    // autoreleased.

    /// `+[NSEvent mouseEventWithType:location:modifierFlags:timestamp:
    /// windowNumber:context:eventNumber:clickCount:pressure:]`.
    ///
    /// # Safety
    /// `kind` must be one of the mouse types the factory accepts (the
    /// button, moved and dragged families).
    unsafe fn mouse_event(kind: usize, loc: CGPoint, win: isize, clicks: isize) -> Id {
        // SAFETY: eleven parameters counting the class and `_cmd`, inside
        // `MsgFn`'s sixteen; the prototype is the header's, `context:` is
        // documented nil on modern macOS.
        unsafe {
            let f: unsafe extern "C-unwind" fn(
                Id,
                Sel,
                usize,
                CGPoint,
                usize,
                f64,
                isize,
                Id,
                isize,
                isize,
                f32,
            ) -> Id = checked_msg();
            f(
                class(c"NSEvent").as_id(),
                sel!(
                    mouseEventWithType:location:modifierFlags:timestamp:windowNumber:context:eventNumber:clickCount:pressure:
                ),
                kind,
                loc,
                0,
                0.0,
                win,
                Id::NIL,
                1,
                clicks,
                if clicks > 0 { 1.0 } else { 0.0 },
            )
        }
    }

    /// `+[NSEvent enterExitEventWithType:location:modifierFlags:timestamp:
    /// windowNumber:context:eventNumber:trackingNumber:userData:]`.
    ///
    /// # Safety
    /// `kind` must be `MouseEntered`, `MouseExited` or `CursorUpdate`.
    unsafe fn enter_exit_event(kind: usize, loc: CGPoint, win: isize) -> Id {
        // SAFETY: as `mouse_event`; `userData:` is `void *` and NULL is the
        // documented "none".
        unsafe {
            let f: unsafe extern "C-unwind" fn(
                Id,
                Sel,
                usize,
                CGPoint,
                usize,
                f64,
                isize,
                Id,
                isize,
                isize,
                *mut c_void,
            ) -> Id = checked_msg();
            f(
                class(c"NSEvent").as_id(),
                sel!(
                    enterExitEventWithType:location:modifierFlags:timestamp:windowNumber:context:eventNumber:trackingNumber:userData:
                ),
                kind,
                loc,
                0,
                0.0,
                win,
                Id::NIL,
                1,
                0,
                std::ptr::null_mut(),
            )
        }
    }

    /// `+[NSEvent keyEventWithType:location:modifierFlags:timestamp:
    /// windowNumber:context:characters:charactersIgnoringModifiers:isARepeat:
    /// keyCode:]`.
    ///
    /// # Safety
    /// `kind` must be `KeyDown`, `KeyUp` or `FlagsChanged`; `chars` a live
    /// `NSString` (empty for `FlagsChanged`, as AppKit passes).
    unsafe fn key_event(kind: usize, win: isize, chars: Id, flags: usize, code: u16) -> Id {
        // SAFETY: twelve parameters counting the class and `_cmd`, inside
        // `MsgFn`'s sixteen — the same cast `objc_ime_drive` makes.
        unsafe {
            let f: unsafe extern "C-unwind" fn(
                Id,
                Sel,
                usize,
                CGPoint,
                usize,
                f64,
                isize,
                Id,
                Id,
                Id,
                Bool,
                u16,
            ) -> Id = checked_msg();
            f(
                class(c"NSEvent").as_id(),
                sel!(
                    keyEventWithType:location:modifierFlags:timestamp:windowNumber:context:characters:charactersIgnoringModifiers:isARepeat:keyCode:
                ),
                kind,
                CGPoint { x: 0.0, y: 0.0 },
                flags,
                0.0,
                win,
                Id::NIL,
                chars,
                chars,
                Bool::NO,
                code,
            )
        }
    }

    /// `+[NSEvent otherEventWithType:location:modifierFlags:timestamp:
    /// windowNumber:context:subtype:data1:data2:]`.
    ///
    /// # Safety
    /// `kind` must be `AppKitDefined`, `SystemDefined`, `ApplicationDefined`
    /// or `Periodic`.
    unsafe fn other_event(kind: usize, win: isize) -> Id {
        // SAFETY: eleven parameters counting the class and `_cmd`; `subtype:`
        // is a `short` in the header, hence `i16`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(
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
            ) -> Id = checked_msg();
            f(
                class(c"NSEvent").as_id(),
                sel!(otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:),
                kind,
                CGPoint { x: 0.0, y: 0.0 },
                0,
                0.0,
                win,
                Id::NIL,
                0,
                0,
                0,
            )
        }
    }

    /// `+[NSEvent eventWithCGEvent:]`, then release the `CGEvent` (the
    /// `NSEvent` holds its own reference). Nil if the factory refuses.
    ///
    /// # Safety
    /// `cg` must be a live `CGEventRef` this caller owns.
    unsafe fn cg_wrapped(cg: *mut c_void) -> Id {
        if cg.is_null() {
            return Id::NIL;
        }
        // SAFETY: `+eventWithCGEvent:` is `@@:^{__CGEvent=}`; the result is
        // autoreleased and retains the CGEvent, so releasing ours is the
        // documented balance.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, *mut c_void) -> Id = checked_msg();
            let ev = f(class(c"NSEvent").as_id(), sel!(eventWithCGEvent:), cg);
            CFRelease(cg);
            ev
        }
    }

    /// A CG-backed mouse event of the given type at the window point `loc`.
    ///
    /// CoreGraphics locations are global with a top-left origin; AppKit flips them to
    /// bottom-left screen coordinates, and for a windowless event `-locationInWindow`
    /// answers that screen point. With the window pinned to the screen origin
    /// (`resumed`), window point `(x, y)` is CG point `(x, height - y)`.
    ///
    /// # Safety
    /// `kind` must be a CG mouse type.
    unsafe fn cg_mouse(kind: usize, loc: CGPoint, button: u32) -> Id {
        // SAFETY: the CoreGraphics prototypes above are the headers'.
        unsafe {
            let height = CGDisplayBounds(CGMainDisplayID()).size.height;
            let global = CGPoint {
                x: loc.x,
                y: height - loc.y,
            };
            cg_wrapped(CGEventCreateMouseEvent(
                std::ptr::null(),
                kind as u32,
                global,
                button,
            ))
        }
    }

    /// A CG-backed scroll-wheel event.
    unsafe fn cg_scroll(units: u32, wheel1: i32, wheel2: i32) -> Id {
        // SAFETY: as `cg_mouse`.
        unsafe {
            cg_wrapped(CGEventCreateScrollWheelEvent2(
                std::ptr::null(),
                units,
                2,
                wheel1,
                wheel2,
                0,
            ))
        }
    }

    /// A CG-backed key event, optionally retyped (`FlagsChanged` has no
    /// CG constructor of its own; AppKit accepts a keyboard event retyped).
    unsafe fn cg_key(code: u16, down: bool, retype: Option<usize>) -> Id {
        // SAFETY: as `cg_mouse`.
        unsafe {
            let cg = CGEventCreateKeyboardEvent(std::ptr::null(), code, down);
            if let Some(kind) = retype {
                CGEventSetType(cg, kind as u32);
            }
            cg_wrapped(cg)
        }
    }

    /// A bare `CGEvent` retyped: the gesture family and the CG enter/exit shapes.
    /// Nil when `+eventWithCGEvent:` refuses the type; the caller says so.
    unsafe fn cg_typed(kind: usize) -> Id {
        // SAFETY: as `cg_mouse`.
        unsafe {
            let cg = CGEventCreate(std::ptr::null());
            CGEventSetType(cg, kind as u32);
            cg_wrapped(cg)
        }
    }

    // ----------------------------------------------------------------- sends

    /// `-(void)row:(NSEvent *)event`, entered exactly as AppKit enters it.
    ///
    /// # Safety
    /// `target` must be live and must implement `s` with that prototype.
    unsafe fn send_v_id(target: Id, s: Sel, arg: Id) {
        // SAFETY: the caller pins the prototype.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
            f(target, s, arg);
        }
    }

    /// `-(BOOL)row:(NSEvent *)event`.
    ///
    /// # Safety
    /// As `send_v_id`.
    unsafe fn send_b_id(target: Id, s: Sel, arg: Id) -> Bool {
        // SAFETY: the caller pins the prototype.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Bool = checked_msg();
            f(target, s, arg)
        }
    }

    /// `-(id)sel` on a live object.
    ///
    /// # Safety
    /// As `send_v_id`.
    unsafe fn send_id(target: Id, s: Sel) -> Id {
        // SAFETY: the caller pins the prototype.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            f(target, s)
        }
    }

    /// `-(NSInteger)sel` on a live object.
    ///
    /// # Safety
    /// As `send_v_id`.
    unsafe fn send_isize(target: Id, s: Sel) -> isize {
        // SAFETY: the caller pins the prototype.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> isize = checked_msg();
            f(target, s)
        }
    }

    /// `-(NSUInteger)type` of an event, for the transcript.
    unsafe fn event_type(ev: Id) -> usize {
        // SAFETY: `-type` is `Q@:` and valid for every event.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
            f(ev, sel!(type))
        }
    }

    // --------------------------------------------------------------- control

    aterm_objc::declare_class! {
        /// The controls' class: `poke:` sends a key-only accessor to whatever event
        /// arrived, inside the same `declare_class!` trampoline the view's rows run
        /// under; `panicInside` panics in Rust there; `alive` is the later send that
        /// proves the object and runtime survive a containment.
        struct EventDriveControl: NSObject {
            const NAME: &str = "ATermObjcEventDriveControl";
            type Ivars = ();

            @sel(poke:)
            fn poke(&self, event: Id) {
                // SAFETY: `-keyCode` is `S@:`; whether the receiver ACCEPTS it
                // is the whole question, and AppKit answers by raising.
                let code: u16 = unsafe {
                    let f: unsafe extern "C-unwind" fn(Id, Sel) -> u16 = checked_msg();
                    f(event, sel!(keyCode))
                };
                println!("control: -keyCode on a MouseMoved event answered {code}");
            }

            @sel(panicInside)
            fn panic_inside(&self) {
                panic!("control: a Rust panic inside the trampoline");
            }

            @sel(alive)
            fn alive(&self) -> i64 {
                42
            }
        }
    }

    /// The child half of the controls, chosen by [`CONTROL_ENV`]'s value. The
    /// exception control makes the wrong-type send, then a second send on the same
    /// object, reports both on stdout and exits `0` (the containment line is on
    /// stderr). The panic control's report is never read: the process must abort first.
    pub fn control_child() -> i32 {
        let mode = std::env::var(CONTROL_ENV).unwrap_or_default();
        trap::install();
        let Some(mtm) = MainThread::new() else {
            eprintln!("control: not on the main thread");
            return NOT_RUN;
        };
        autoreleasepool(|_| {
            let Some(obj) = EventDriveControl::alloc_init(mtm, ()) else {
                eprintln!("control: the class did not instantiate");
                return NOT_RUN;
            };
            match mode.as_str() {
                CONTROL_EXCEPTION => {
                    trap::set_row(
                        "control: -keyCode on a MouseMoved event inside ATermObjcEventDriveControl poke:",
                    );
                    // SAFETY: a mouse type into the mouse factory.
                    let ev =
                        unsafe { mouse_event(MOUSE_MOVED, CGPoint { x: 10.0, y: 10.0 }, 0, 0) };
                    if ev.is_null() {
                        eprintln!("control: +[NSEvent mouseEventWithType:…] answered nil");
                        return NOT_RUN;
                    }
                    // SAFETY: `poke:` is registered `v@:@` by the declaration
                    // above and `obj` is the live instance.
                    unsafe { send_v_id(obj.as_id(), sel!(poke:), ev) };
                    println!("control: SURVIVED the v0.72.0 send");
                    trap::set_row("control: -alive after the containment");
                    // SAFETY: `alive` is registered `q@:`.
                    let later = unsafe { send_isize(obj.as_id(), sel!(alive)) };
                    println!("control: a later send on the same object answered {later}");
                    println!(
                        "control: {} containment(s) counted",
                        aterm_objc::contained_count()
                    );
                    if later == 42 { PASS } else { FAIL }
                }
                CONTROL_PANIC => {
                    trap::set_row(
                        "control: a Rust panic inside ATermObjcEventDriveControl panicInside",
                    );
                    // SAFETY: `panicInside` is registered `v@:`.
                    unsafe { aterm_objc::send::send_v(obj.as_id(), sel!(panicInside)) };
                    println!("control: SURVIVED the Rust panic");
                    PASS
                }
                other => {
                    eprintln!("control: unknown mode {other:?}");
                    NOT_RUN
                }
            }
        })
    }

    /// What a control child did: its signal, or its exit code with both
    /// output streams.
    enum ControlOutcome {
        Signalled(i32, String),
        Exited(Option<i32>, String, String),
        Hung,
        CouldNotSpawn(String),
    }

    fn run_control(mode: &str) -> ControlOutcome {
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => return ControlOutcome::CouldNotSpawn(format!("current_exe: {e}")),
        };
        let mut child = match std::process::Command::new(exe)
            .env(CONTROL_ENV, mode)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return ControlOutcome::CouldNotSpawn(format!("spawn: {e}")),
        };
        // Drain both pipes on threads so a chatty child (the containment line
        // carries a call stack) can never block on a full pipe.
        let drain = |stream: Option<std::process::ChildStdout>| {
            std::thread::spawn(move || {
                let mut text = String::new();
                if let Some(mut s) = stream {
                    use std::io::Read as _;
                    let _ = s.read_to_string(&mut text);
                }
                text
            })
        };
        let stdout = drain(child.stdout.take());
        let stderr = {
            let stream = child.stderr.take();
            std::thread::spawn(move || {
                let mut text = String::new();
                if let Some(mut s) = stream {
                    use std::io::Read as _;
                    let _ = s.read_to_string(&mut text);
                }
                text
            })
        };
        let deadline = Instant::now() + CONTROL_WATCHDOG;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    use std::os::unix::process::ExitStatusExt as _;
                    let out = stdout.join().unwrap_or_default();
                    let err = stderr.join().unwrap_or_default();
                    if let Some(sig) = status.signal() {
                        return ControlOutcome::Signalled(sig, err);
                    }
                    return ControlOutcome::Exited(status.code(), out, err);
                }
                Ok(None) => {}
                Err(e) => return ControlOutcome::CouldNotSpawn(format!("try_wait: {e}")),
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return ControlOutcome::Hung;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    // ---------------------------------------------------------------- driver

    #[derive(Default)]
    struct Report {
        findings: Vec<String>,
        blocked: Option<String>,
    }

    impl Report {
        fn fail(&mut self, what: String) {
            println!("    FINDING: {what}");
            self.findings.push(what);
        }

        fn expect(&mut self, what: &str, ok: bool, detail: String) {
            if ok {
                println!("    ok   {what}: {detail}");
            } else {
                self.fail(format!("{what}: {detail}"));
            }
        }
    }

    type Deferred = Box<dyn FnOnce(&mut Driver)>;

    struct Driver {
        window: Option<Window>,
        report: Report,
        /// Every `WindowEvent` winit delivered, by name.
        events: Vec<String>,
        done: bool,
        stage: usize,
        pending: Option<Deferred>,
        /// Rows actually entered with an event.
        driven: usize,
        /// Rows this file could not drive, each with the reason.
        not_driven: Vec<String>,
    }

    impl Driver {
        fn view(&self) -> Option<Id> {
            let view = aterm_objc::HasNsView::ns_view(self.window.as_ref()?);
            (!view.is_null()).then_some(view)
        }

        /// The `NSWindow` holding the view, and its number — the number is
        /// what the factories take so `-[NSEvent window]` resolves.
        fn window_number(&self) -> Option<isize> {
            let view = self.view()?;
            // SAFETY: `-window` is `@@:` on `NSView`; `-windowNumber` is `q@:`
            // on `NSWindow`.
            let win = unsafe { send_id(view, sel!(window)) };
            if win.is_null() {
                return None;
            }
            Some(unsafe { send_isize(win, sel!(windowNumber)) })
        }

        fn drain(&mut self) -> Vec<String> {
            std::mem::take(&mut self.events)
        }

        fn count(events: &[String], name: &str) -> usize {
            events.iter().filter(|e| e.as_str() == name).count()
        }

        /// Enter one row with one event and log the survival. A nil event means the
        /// shape could not be built, which is logged as NOT DRIVEN and fails the run.
        fn drive(&mut self, view: Id, row: Sel, row_name: &str, event: Id, shape: &str) {
            if event.is_null() {
                let why = format!("{row_name} <- {shape}: the event could not be built");
                println!("    NOT DRIVEN {why}");
                self.not_driven.push(why);
                return;
            }
            trap::set_row(&format!("{row_name} <- {shape}"));
            // SAFETY: every row driven here is registered `v@:@` (audited by
            // `objc_live_class_audit`) and `event` is a live autoreleased
            // NSEvent built above.
            let kind = unsafe { event_type(event) };
            unsafe { send_v_id(view, row, event) };
            self.driven += 1;
            println!("    ok   {row_name} <- {shape} (type {kind}) returned");
        }

        /// As [`Self::drive`], for the two `BOOL`-returning rows.
        fn drive_bool(&mut self, view: Id, row: Sel, row_name: &str, event: Id, shape: &str) {
            if event.is_null() {
                let why = format!("{row_name} <- {shape}: the event could not be built");
                println!("    NOT DRIVEN {why}");
                self.not_driven.push(why);
                return;
            }
            trap::set_row(&format!("{row_name} <- {shape}"));
            // SAFETY: both rows are registered `B@:@`.
            let kind = unsafe { event_type(event) };
            let answer = unsafe { send_b_id(view, row, event) };
            self.driven += 1;
            println!(
                "    ok   {row_name} <- {shape} (type {kind}) answered {}",
                answer.as_bool()
            );
        }
    }

    impl ApplicationHandler for Driver {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let attrs = Window::default_attributes()
                .with_title("objc-event-drive")
                .with_inner_size(winit::dpi::LogicalSize::new(480.0, 320.0))
                .with_visible(false);
            match el.create_window(attrs) {
                Ok(w) => self.window = Some(w),
                Err(e) => {
                    self.report.blocked = Some(format!("no window could be created: {e}"));
                    self.done = true;
                    el.exit();
                    return;
                }
            }
            // A CG-backed event has no window, so `-locationInWindow` answers screen
            // coordinates, which `mouse_motion` treats as window ones. Pinning the
            // window to the screen origin makes the two coincide over the view, so the
            // CG shape lands in-frame like the factory shape.
            if let Some(view) = self.view() {
                // SAFETY: `-window` is `@@:` on NSView; `-setFrameOrigin:` is
                // `v@:{CGPoint=dd}` on NSWindow.
                unsafe {
                    let win = send_id(view, sel!(window));
                    if !win.is_null() {
                        let f: unsafe extern "C-unwind" fn(Id, Sel, CGPoint) = checked_msg();
                        f(win, sel!(setFrameOrigin:), CGPoint { x: 0.0, y: 0.0 });
                    }
                }
            }
        }

        fn window_event(&mut self, _el: &ActiveEventLoop, _id: WindowId, e: WindowEvent) {
            let name = match &e {
                WindowEvent::CursorMoved { .. } => "CursorMoved",
                WindowEvent::CursorEntered { .. } => "CursorEntered",
                WindowEvent::CursorLeft { .. } => "CursorLeft",
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                } => "MouseInput(Pressed)",
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    ..
                } => "MouseInput(Released)",
                WindowEvent::MouseWheel { .. } => "MouseWheel",
                WindowEvent::KeyboardInput { .. } => "KeyboardInput",
                WindowEvent::ModifiersChanged(_) => "ModifiersChanged",
                WindowEvent::Ime(_) => "Ime",
                WindowEvent::Resized(_) => "Resized",
                WindowEvent::Moved(_) => "Moved",
                WindowEvent::Focused(_) => "Focused",
                WindowEvent::RedrawRequested => "RedrawRequested",
                _ => "other",
            };
            self.events.push(name.to_owned());
        }

        fn about_to_wait(&mut self, el: &ActiveEventLoop) {
            if self.done {
                return;
            }
            let Some(view) = self.view() else {
                self.report.blocked = Some("the window has no AppKit view".to_owned());
                self.done = true;
                el.exit();
                return;
            };
            let Some(win) = self.window_number() else {
                self.report.blocked = Some("the view has no NSWindow".to_owned());
                self.done = true;
                el.exit();
                return;
            };
            if let Some(check) = self.pending.take() {
                check(self);
                return;
            }
            // One stage per wait, so a stage's winit events are delivered by
            // the pump before the next stage's `pending` reads them.
            let stage = self.stage;
            self.stage += 1;
            autoreleasepool(|_| match stage {
                0 => {}
                1 => self.stage_control(),
                2 => self.stage_mouse_moved(view, win),
                3 => self.stage_buttons(view, win),
                4 => self.stage_tracking(view, win),
                5 => self.stage_scroll(view),
                6 => self.stage_keys(view, win),
                7 => self.stage_send_event(win),
                _ => {
                    self.done = true;
                    el.exit();
                }
            });
        }
    }

    impl Driver {
        fn stage_control(&mut self) {
            println!(
                "\n=== 1a. THE EXCEPTION CONTROL: the v0.72.0 send inside a trampoline must be \
                 CONTAINED ==="
            );
            match run_control(CONTROL_EXCEPTION) {
                ControlOutcome::Signalled(sig, err) => {
                    self.report.fail(format!(
                        "the exception control died by signal {sig}: the v0.72.0 send was NOT \
                         contained (the pre-containment behaviour); stderr {:?}",
                        err.trim()
                    ));
                }
                ControlOutcome::Exited(code, out, err) => {
                    self.report.expect(
                        "the exception control exited 0",
                        code == Some(PASS),
                        format!("exit {code:?}; stdout {:?}", out.trim()),
                    );
                    let line = err
                        .lines()
                        .find(|l| l.starts_with("aterm-objc: contained "))
                        .unwrap_or("");
                    self.report.expect(
                        "the containment line on stderr names poke: and NSInternalInconsistencyException",
                        line.contains("`poke:`") && line.contains("NSInternalInconsistencyException"),
                        if line.is_empty() {
                            format!("no `aterm-objc: contained` line; stderr {:?}", err.trim())
                        } else {
                            let head: String = line.chars().take(220).collect();
                            format!("{head}…")
                        },
                    );
                    self.report.expect(
                        "the containment line carries the exception's reason and a call stack",
                        line.contains("Invalid message sent to event")
                            && line.contains("| stack: "),
                        String::new(),
                    );
                    self.report.expect(
                        "a later send on the same object answered after the containment",
                        out.contains("a later send on the same object answered 42"),
                        format!("stdout {:?}", out.trim()),
                    );
                    let lines = err
                        .lines()
                        .filter(|l| l.starts_with("aterm-objc: contained "))
                        .count();
                    self.report.expect(
                        "exactly ONE containment line for the one raise",
                        lines == 1,
                        format!(
                            "{lines} `aterm-objc: contained` line(s); stderr {:?}",
                            err.trim()
                        ),
                    );
                    self.report.expect(
                        "the child counted exactly one containment",
                        out.contains("control: 1 containment(s) counted"),
                        format!("stdout {:?}", out.trim()),
                    );
                }
                ControlOutcome::Hung => {
                    self.report.fail("the exception control hung".to_owned());
                }
                ControlOutcome::CouldNotSpawn(why) => {
                    self.report.blocked =
                        Some(format!("the exception control could not run: {why}"));
                    return;
                }
            }

            println!(
                "\n=== 1b. THE PANIC CONTROL: a Rust panic inside a trampoline must still ABORT ==="
            );
            const PANIC_MESSAGE: &str =
                "aterm-objc: panic escaped Objective-C method `panic_inside`; aborting";
            match run_control(CONTROL_PANIC) {
                ControlOutcome::Signalled(sig, err) => {
                    self.report.expect(
                        "the panic control died by SIGABRT",
                        sig == SIGABRT,
                        format!("signal {sig} (the trap did not turn it into exit 3, but the abort is the same evidence)"),
                    );
                    self.report.expect(
                        "abort_on_unwind's message is on stderr, unchanged",
                        err.contains(PANIC_MESSAGE),
                        format!("stderr {:?}", err.trim()),
                    );
                }
                ControlOutcome::Exited(Some(code), out, err) if code == ABORTED => {
                    // The trap turns any fatal signal after `set_row` into 3, a fault in
                    // class registration or the factory included, so require the trap's
                    // line to name signal 6 and the control row.
                    let last = out.trim().lines().last().unwrap_or("").to_owned();
                    self.report.expect(
                        "the panic control aborted (SIGABRT) inside the trampoline",
                        last.contains("(signal 6) while driving: control:"),
                        format!("exit {code} via the driver's signal trap: {last}"),
                    );
                    self.report.expect(
                        "abort_on_unwind's message is on stderr, unchanged",
                        err.contains(PANIC_MESSAGE),
                        format!("stderr {:?}", err.trim()),
                    );
                    self.report.expect(
                        "a Rust panic is not reported as a containment",
                        !err.contains("aterm-objc: contained "),
                        String::new(),
                    );
                }
                ControlOutcome::Exited(code, out, err) => {
                    self.report.fail(format!(
                        "the panic control SURVIVED a Rust panic inside a declare_class! \
                         trampoline (exit {code:?}, stdout {:?}, stderr {:?}) — @catch(id) \
                         swallowed a Rust panic, or the guard is gone; re-read every \
                         expectation in this file",
                        out.trim(),
                        err.trim()
                    ));
                }
                ControlOutcome::Hung => {
                    self.report
                        .fail("the panic control hung instead of aborting".to_owned());
                }
                ControlOutcome::CouldNotSpawn(why) => {
                    self.report.blocked = Some(format!("the panic control could not run: {why}"));
                }
            }
        }

        /// `mouseMoved:`, first and on its own.
        fn stage_mouse_moved(&mut self, view: Id, win: isize) {
            println!("\n=== 2. mouseMoved: — the v0.72.0 row ===");
            let _ = self.drain();
            let inside = CGPoint { x: 10.0, y: 10.0 };
            let outside = CGPoint { x: -50.0, y: -50.0 };
            // SAFETY: mouse types into the mouse factories.
            let (a, b, c, d) = unsafe {
                (
                    mouse_event(MOUSE_MOVED, inside, win, 0),
                    cg_mouse(MOUSE_MOVED, inside, CG_BUTTON_LEFT),
                    mouse_event(MOUSE_MOVED, outside, win, 0),
                    cg_mouse(MOUSE_MOVED, outside, CG_BUTTON_LEFT),
                )
            };
            self.drive(
                view,
                sel!(mouseMoved:),
                "mouseMoved:",
                a,
                "MouseMoved/factory inside",
            );
            self.drive(
                view,
                sel!(mouseMoved:),
                "mouseMoved:",
                b,
                "MouseMoved/cg inside",
            );
            self.drive(
                view,
                sel!(mouseMoved:),
                "mouseMoved:",
                c,
                "MouseMoved/factory outside",
            );
            self.drive(
                view,
                sel!(mouseMoved:),
                "mouseMoved:",
                d,
                "MouseMoved/cg outside",
            );
            self.pending = Some(Box::new(|d: &mut Driver| {
                let got = d.drain();
                let moved = Driver::count(&got, "CursorMoved");
                // Two in-frame sends (factory + CG); losing either shape is a
                // finding, so the count is exact.
                d.report.expect(
                    "both in-frame mouseMoved: shapes yield CursorMoved",
                    moved == 2,
                    format!("{moved} CursorMoved (want 2) in {got:?}"),
                );
            }));
        }

        /// The nine button rows, each with its own type, both shapes.
        fn stage_buttons(&mut self, view: Id, win: isize) {
            println!("\n=== 3. the button and drag rows ===");
            let _ = self.drain();
            let p = CGPoint { x: 20.0, y: 20.0 };
            let rows: [(Sel, &str, usize, u32, isize); 9] = [
                (
                    sel!(mouseDown:),
                    "mouseDown:",
                    LEFT_MOUSE_DOWN,
                    CG_BUTTON_LEFT,
                    1,
                ),
                (sel!(mouseUp:), "mouseUp:", LEFT_MOUSE_UP, CG_BUTTON_LEFT, 1),
                (
                    sel!(mouseDragged:),
                    "mouseDragged:",
                    LEFT_MOUSE_DRAGGED,
                    CG_BUTTON_LEFT,
                    1,
                ),
                (
                    sel!(rightMouseDown:),
                    "rightMouseDown:",
                    RIGHT_MOUSE_DOWN,
                    CG_BUTTON_RIGHT,
                    1,
                ),
                (
                    sel!(rightMouseUp:),
                    "rightMouseUp:",
                    RIGHT_MOUSE_UP,
                    CG_BUTTON_RIGHT,
                    1,
                ),
                (
                    sel!(rightMouseDragged:),
                    "rightMouseDragged:",
                    RIGHT_MOUSE_DRAGGED,
                    CG_BUTTON_RIGHT,
                    1,
                ),
                (
                    sel!(otherMouseDown:),
                    "otherMouseDown:",
                    OTHER_MOUSE_DOWN,
                    CG_BUTTON_CENTER,
                    1,
                ),
                (
                    sel!(otherMouseUp:),
                    "otherMouseUp:",
                    OTHER_MOUSE_UP,
                    CG_BUTTON_CENTER,
                    1,
                ),
                (
                    sel!(otherMouseDragged:),
                    "otherMouseDragged:",
                    OTHER_MOUSE_DRAGGED,
                    CG_BUTTON_CENTER,
                    1,
                ),
            ];
            for (row, name, kind, button, clicks) in rows {
                // SAFETY: mouse types into the mouse factories.
                let (a, b) =
                    unsafe { (mouse_event(kind, p, win, clicks), cg_mouse(kind, p, button)) };
                self.drive(view, row, name, a, &format!("type {kind}/factory"));
                self.drive(view, row, name, b, &format!("type {kind}/cg"));
            }
            // The two BOOL rows that take an event and ignore it.
            // SAFETY: as above.
            let (down, key) = unsafe {
                (
                    mouse_event(LEFT_MOUSE_DOWN, p, win, 1),
                    ns_string("a").map_or(Id::NIL, |s| key_event(KEY_DOWN, win, s.id(), 0, VK_A)),
                )
            };
            self.drive_bool(
                view,
                sel!(acceptsFirstMouse:),
                "acceptsFirstMouse:",
                down,
                "LeftMouseDown/factory",
            );
            self.drive_bool(
                view,
                sel!(_wantsKeyDownForEvent:),
                "_wantsKeyDownForEvent:",
                key,
                "KeyDown/factory",
            );
            self.pending = Some(Box::new(|d: &mut Driver| {
                let got = d.drain();
                let pressed = Driver::count(&got, "MouseInput(Pressed)");
                let released = Driver::count(&got, "MouseInput(Released)");
                // Three down rows x two shapes, three up rows x two shapes:
                // exact, so a shape that stops reaching its row is a finding.
                d.report.expect(
                    "the down rows yield MouseInput(Pressed), both shapes",
                    pressed == 6,
                    format!("{pressed} pressed (want 6), {released} released in {got:?}"),
                );
                d.report.expect(
                    "the up rows yield MouseInput(Released), both shapes",
                    released == 6,
                    format!("{released} released (want 6)"),
                );
            }));
        }

        fn stage_tracking(&mut self, view: Id, win: isize) {
            println!("\n=== 4. mouseEntered: / mouseExited: ===");
            let _ = self.drain();
            let p = CGPoint { x: 5.0, y: 5.0 };
            // SAFETY: enter/exit types into the enter/exit factory; the CG
            // shape is a retyped bare event, which is what a tracking event
            // AppKit synthesises from hardware looks like underneath.
            let (a, b, c, d) = unsafe {
                (
                    enter_exit_event(MOUSE_ENTERED, p, win),
                    cg_typed(MOUSE_ENTERED),
                    enter_exit_event(MOUSE_EXITED, p, win),
                    cg_typed(MOUSE_EXITED),
                )
            };
            self.drive(
                view,
                sel!(mouseEntered:),
                "mouseEntered:",
                a,
                "MouseEntered/factory",
            );
            self.drive(
                view,
                sel!(mouseEntered:),
                "mouseEntered:",
                b,
                "MouseEntered/cg",
            );
            self.drive(
                view,
                sel!(mouseExited:),
                "mouseExited:",
                c,
                "MouseExited/factory",
            );
            self.drive(
                view,
                sel!(mouseExited:),
                "mouseExited:",
                d,
                "MouseExited/cg",
            );
            self.pending = Some(Box::new(|d: &mut Driver| {
                let got = d.drain();
                d.report.expect(
                    "mouseEntered: yields CursorEntered and mouseExited: yields CursorLeft",
                    Driver::count(&got, "CursorEntered") >= 1
                        && Driver::count(&got, "CursorLeft") >= 1,
                    format!("{got:?}"),
                );
            }));
        }

        fn stage_scroll(&mut self, view: Id) {
            println!("\n=== 5. scrollWheel: — pixel and line units ===");
            let _ = self.drain();
            // SAFETY: the CG scroll constructor; there is no AppKit factory
            // for scroll events, so the CG shape is the only real one.
            let (a, b) = unsafe {
                (
                    cg_scroll(CG_SCROLL_UNIT_PIXEL, 3, 4),
                    cg_scroll(CG_SCROLL_UNIT_LINE, 1, 0),
                )
            };
            self.drive(
                view,
                sel!(scrollWheel:),
                "scrollWheel:",
                a,
                "ScrollWheel/cg pixel",
            );
            self.drive(
                view,
                sel!(scrollWheel:),
                "scrollWheel:",
                b,
                "ScrollWheel/cg line",
            );
            self.pending = Some(Box::new(|d: &mut Driver| {
                let got = d.drain();
                let n = Driver::count(&got, "MouseWheel");
                d.report.expect(
                    "scrollWheel: yields MouseWheel",
                    n >= 1,
                    format!("{n} MouseWheel in {got:?}"),
                );
            }));
        }

        fn stage_keys(&mut self, view: Id, win: isize) {
            println!("\n=== 6. keyDown: / keyUp: / flagsChanged: ===");
            let _ = self.drain();
            let Some(a) = ns_string("a") else {
                self.report.fail("could not build an NSString".to_owned());
                return;
            };
            let Some(empty) = ns_string("") else {
                self.report
                    .fail("could not build an empty NSString".to_owned());
                return;
            };
            // SAFETY: key types into the key factories.
            let (kd, ku, fc, cg_kd, cg_ku, cg_fc) = unsafe {
                (
                    key_event(KEY_DOWN, win, a.id(), 0, VK_A),
                    key_event(KEY_UP, win, a.id(), 0, VK_A),
                    key_event(FLAGS_CHANGED, win, empty.id(), NS_SHIFT, VK_SHIFT),
                    cg_key(VK_A, true, None),
                    cg_key(VK_A, false, None),
                    cg_key(VK_SHIFT, true, Some(FLAGS_CHANGED)),
                )
            };
            self.drive(view, sel!(keyDown:), "keyDown:", kd, "KeyDown/factory");
            self.drive(view, sel!(keyUp:), "keyUp:", ku, "KeyUp/factory");
            self.drive(
                view,
                sel!(flagsChanged:),
                "flagsChanged:",
                fc,
                "FlagsChanged/factory",
            );
            self.drive(view, sel!(keyDown:), "keyDown:", cg_kd, "KeyDown/cg");
            self.drive(view, sel!(keyUp:), "keyUp:", cg_ku, "KeyUp/cg");
            self.drive(
                view,
                sel!(flagsChanged:),
                "flagsChanged:",
                cg_fc,
                "FlagsChanged/cg",
            );
            // Release the shift the factory event pressed, so the modifier
            // state does not leak into the sendEvent: stage.
            // SAFETY: as above.
            let up = unsafe { key_event(FLAGS_CHANGED, win, empty.id(), 0, VK_SHIFT) };
            self.drive(
                view,
                sel!(flagsChanged:),
                "flagsChanged:",
                up,
                "FlagsChanged/factory release",
            );
            // `cancelOperation:` reads `-[NSApplication currentEvent]`, whatever the run
            // loop last dequeued: as from a Touch Bar or accessibility action, not a key
            // event and possibly nil. The row must return, not send a key-only accessor.
            trap::set_row("cancelOperation: <- (currentEvent as the run loop left it)");
            // SAFETY: `cancelOperation:` is registered `v@:@`; its argument
            // is the sender, which the row ignores.
            unsafe { send_v_id(view, sel!(cancelOperation:), Id::NIL) };
            self.driven += 1;
            println!(
                "    ok   cancelOperation: <- (currentEvent as the run loop left it) returned"
            );
            self.pending = Some(Box::new(|d: &mut Driver| {
                let got = d.drain();
                let keys = Driver::count(&got, "KeyboardInput");
                // keyDown:/keyUp: x factory and CG = four key events at least
                // (flagsChanged: may add more through the modifier path).
                d.report.expect(
                    "keyDown:/keyUp: yield KeyboardInput, both shapes",
                    keys >= 4,
                    format!("{keys} KeyboardInput (want >= 4) in {got:?}"),
                );
                d.report.expect(
                    "flagsChanged: yields ModifiersChanged",
                    Driver::count(&got, "ModifiersChanged") >= 1,
                    format!("{got:?}"),
                );
            }));
        }

        /// Every shape through `-[NSApplication sendEvent:]` — `app.rs`'s
        /// override, and behind it AppKit's own routing into the window and
        /// the view. Survival is the assertion; what arrives is printed.
        fn stage_send_event(&mut self, win: isize) {
            println!("\n=== 7. -[NSApplication sendEvent:] with every shape ===");
            let _ = self.drain();
            // SAFETY: `+sharedApplication` is `@@:`.
            let app = unsafe { send_id(class(c"NSApplication").as_id(), sel!(sharedApplication)) };
            if app.is_null() {
                self.report.fail("no NSApplication".to_owned());
                return;
            }
            let p = CGPoint { x: 30.0, y: 30.0 };
            let Some(a) = ns_string("a") else {
                self.report.fail("could not build an NSString".to_owned());
                return;
            };
            // SAFETY: each type into the factory that accepts it; nil from a
            // refusing factory is reported as NOT DRIVEN below.
            let shapes: Vec<(&str, Id)> = unsafe {
                vec![
                    ("MouseMoved/factory", mouse_event(MOUSE_MOVED, p, win, 0)),
                    ("MouseMoved/cg", cg_mouse(MOUSE_MOVED, p, CG_BUTTON_LEFT)),
                    (
                        "LeftMouseDown/factory",
                        mouse_event(LEFT_MOUSE_DOWN, p, win, 1),
                    ),
                    (
                        "LeftMouseDragged/factory",
                        mouse_event(LEFT_MOUSE_DRAGGED, p, win, 1),
                    ),
                    ("LeftMouseUp/factory", mouse_event(LEFT_MOUSE_UP, p, win, 1)),
                    (
                        "RightMouseDown/factory",
                        mouse_event(RIGHT_MOUSE_DOWN, p, win, 1),
                    ),
                    (
                        "RightMouseUp/factory",
                        mouse_event(RIGHT_MOUSE_UP, p, win, 1),
                    ),
                    (
                        "OtherMouseDown/factory",
                        mouse_event(OTHER_MOUSE_DOWN, p, win, 1),
                    ),
                    (
                        "OtherMouseUp/factory",
                        mouse_event(OTHER_MOUSE_UP, p, win, 1),
                    ),
                    (
                        "MouseEntered/factory",
                        enter_exit_event(MOUSE_ENTERED, p, win),
                    ),
                    (
                        "MouseExited/factory",
                        enter_exit_event(MOUSE_EXITED, p, win),
                    ),
                    (
                        "CursorUpdate/factory",
                        enter_exit_event(CURSOR_UPDATE, p, win),
                    ),
                    ("ScrollWheel/cg", cg_scroll(CG_SCROLL_UNIT_PIXEL, 2, 2)),
                    ("KeyDown/factory", key_event(KEY_DOWN, win, a.id(), 0, VK_A)),
                    ("KeyUp/factory", key_event(KEY_UP, win, a.id(), 0, VK_A)),
                    (
                        "FlagsChanged/factory",
                        key_event(FLAGS_CHANGED, win, a.id(), 0, VK_SHIFT),
                    ),
                    ("AppKitDefined/factory", other_event(APPKIT_DEFINED, win)),
                    ("SystemDefined/factory", other_event(SYSTEM_DEFINED, win)),
                    (
                        "ApplicationDefined/factory",
                        other_event(APPLICATION_DEFINED, win),
                    ),
                    ("Periodic/factory", other_event(PERIODIC, win)),
                    ("Gesture/cg", cg_typed(GESTURE)),
                    ("Magnify/cg", cg_typed(MAGNIFY)),
                    ("Rotate/cg", cg_typed(ROTATE)),
                ]
            };
            for (shape, ev) in shapes {
                if ev.is_null() {
                    let why = format!("sendEvent: <- {shape}: the event could not be built");
                    println!("    NOT DRIVEN {why}");
                    self.not_driven.push(why);
                    continue;
                }
                trap::set_row(&format!("sendEvent: <- {shape}"));
                // SAFETY: `-sendEvent:` is `v@:@` on NSApplication; the
                // override installed by `app.rs` has the same prototype.
                let kind = unsafe { event_type(ev) };
                unsafe { send_v_id(app, sel!(sendEvent:), ev) };
                self.driven += 1;
                println!("    ok   sendEvent: <- {shape} (type {kind}) returned");
            }
            self.pending = Some(Box::new(|d: &mut Driver| {
                let got = d.drain();
                println!("    window events after sendEvent: {got:?}");
            }));
        }
    }

    /// A row that never returns (a deadlock in a trampoline, a modal tracking loop
    /// entered from a button row) would sit inside `about_to_wait` forever, since
    /// `BUDGET` is consulted only between pumps; this thread names the row through the
    /// trap's buffer and exits 3.
    fn arm_hang_watchdog() {
        std::thread::Builder::new()
            .name("objc-event-drive-hang".into())
            .spawn(|| {
                std::thread::sleep(BUDGET + Duration::from_secs(5));
                trap::report_hang_and_exit();
            })
            .ok();
    }

    pub fn run() -> i32 {
        trap::install();
        trap::set_row("(before any row)");
        arm_hang_watchdog();
        // The quiet builder (Accessory, no launch activation): `verify appkit` runs
        // this drive beside a developer who is typing somewhere else.
        let mut el = match aterm_gui::quiet_driver_event_loop_builder::<()>().build() {
            Ok(el) => el,
            Err(e) => {
                eprintln!("objc-event-drive: NOT RUN — no event loop: {e}");
                return NOT_RUN;
            }
        };
        let mut driver = Driver {
            window: None,
            report: Report::default(),
            events: Vec::new(),
            done: false,
            stage: 0,
            pending: None,
            driven: 0,
            not_driven: Vec::new(),
        };
        let started = Instant::now();
        loop {
            if let PumpStatus::Exit(_) =
                el.pump_app_events(Some(Duration::from_millis(8)), &mut driver)
            {
                break;
            }
            if driver.done {
                break;
            }
            if started.elapsed() > BUDGET {
                eprintln!(
                    "objc-event-drive: NOT RUN — the stages did not finish within {BUDGET:?}"
                );
                return NOT_RUN;
            }
        }

        drop(driver.window.take());

        if let Some(why) = driver.report.blocked {
            eprintln!("objc-event-drive: NOT RUN — {why}");
            return NOT_RUN;
        }
        println!("\n=== VERDICT ===");
        println!("  {} row entries driven with a real NSEvent", driver.driven);
        for why in &driver.not_driven {
            println!("  NOT DRIVEN: {why}");
        }
        // Every shape this file builds is expected to be constructible; one that
        // is not means AppKit changed and this file must be re-read.
        if !driver.not_driven.is_empty() {
            driver.report.fail(format!(
                "{} entries were not driven; every row is expected to be",
                driver.not_driven.len()
            ));
        }
        if driver.report.findings.is_empty() {
            println!(
                "objc-event-drive: OK — every row survived every event it can receive, and the \
                 relations held."
            );
            PASS
        } else {
            for f in &driver.report.findings {
                println!("  FAIL: {f}");
            }
            println!(
                "objc-event-drive: {} FINDING(S)",
                driver.report.findings.len()
            );
            FAIL
        }
    }
}
