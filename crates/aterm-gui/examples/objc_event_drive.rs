// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Drives every `NSEvent`-taking row of `WinitView` and `NSApplication::sendEvent:` with real events.

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
    use aterm_window::application::ApplicationHandler;
    use aterm_window::event::{ElementState, WindowEvent};
    use aterm_window::event_loop::ActiveEventLoop;
    use aterm_window::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
    use aterm_window::window::{Window, WindowId};

    use super::{ABORTED, FAIL, NOT_RUN, PASS};

    /// The fatal-signal trap: turn the abort a row's trampoline performs into
    /// `exit 3` with the row's name on stdout, instead of a crash dialog.
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
        // SAFETY: written only on the main thread between sends and read on the same thread.
        unsafe impl Sync for RowBuf {}

        static ROW: RowBuf = RowBuf(UnsafeCell::new([0; CAP]));
        static ROW_LEN: AtomicUsize = AtomicUsize::new(0);

        /// Records the row label about to be driven for fatal signal reporting.
        pub fn set_row(label: &str) {
            let bytes = label.as_bytes();
            let n = bytes.len().min(CAP);
            // SAFETY: main thread only; see `RowBuf`.
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
            // SAFETY: `write` and `_exit` are async-signal-safe and read the static row buffer.
            unsafe {
                let _ = write(1, HEAD.as_ptr().cast(), HEAD.len());
                let _ = write(1, digits.as_ptr().cast(), k);
                let _ = write(1, MID.as_ptr().cast(), MID.len());
                let _ = write(1, (*ROW.0.get()).as_ptr().cast(), n);
                let _ = write(1, b"\n".as_ptr().cast(), 1);
                _exit(super::ABORTED);
            }
        }

        /// Reports the hung row and exits with code 134.
        pub fn report_hang_and_exit() -> ! {
            const HEAD: &[u8] =
                b"\nobjc-event-drive: HUNG (row never returned within the budget) while driving: ";
            let n = ROW_LEN.load(Ordering::SeqCst);
            // SAFETY: raw writes of static bytes and the stuck main thread's row buffer before `_exit`.
            unsafe {
                let _ = write(1, HEAD.as_ptr().cast(), HEAD.len());
                let _ = write(1, (*ROW.0.get()).as_ptr().cast(), n);
                let _ = write(1, b"\n".as_ptr().cast(), 1);
                _exit(super::ABORTED);
            }
        }

        /// Installs `on_fatal` for abort and fault signals.
        pub fn install() {
            for sig in [SIGABRT, SIGSEGV, SIGBUS, SIGILL, SIGTRAP] {
                // SAFETY: `signal(2)` installs a plain C handler for fatal signals.
                unsafe {
                    signal(sig, on_fatal as extern "C" fn(c_int) as usize);
                }
            }
        }
    }

    /// Environment variable naming the re-executed control child ([`CONTROL_EXCEPTION`] or [`CONTROL_PANIC`]).
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

    /// Constructs a mouse `NSEvent` via `+mouseEventWithType:...`.
    /// # Safety
    /// `kind` must be a valid mouse event type accepted by the factory.
    unsafe fn mouse_event(kind: usize, loc: CGPoint, win: isize, clicks: isize) -> Id {
        // SAFETY: eleven-argument `objc_msgSend` matches the `+mouseEventWithType:...` prototype.
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

    /// Constructs an enter/exit `NSEvent` via `+enterExitEventWithType:...`.
    /// # Safety
    /// `kind` must be `MouseEntered`, `MouseExited`, or `CursorUpdate`.
    unsafe fn enter_exit_event(kind: usize, loc: CGPoint, win: isize) -> Id {
        // SAFETY: eleven-argument `objc_msgSend` matches the `+enterExitEventWithType:...` prototype.
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

    /// Constructs a key `NSEvent` via `+keyEventWithType:...`.
    /// # Safety
    /// `kind` must be `KeyDown`, `KeyUp`, or `FlagsChanged`, and `chars` a live `NSString`.
    unsafe fn key_event(kind: usize, win: isize, chars: Id, flags: usize, code: u16) -> Id {
        // SAFETY: twelve-argument `objc_msgSend` matches the `+keyEventWithType:...` prototype.
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

    /// Constructs a miscellaneous `NSEvent` via `+otherEventWithType:...`.
    /// # Safety
    /// `kind` must be `AppKitDefined`, `SystemDefined`, `ApplicationDefined`, or `Periodic`.
    unsafe fn other_event(kind: usize, win: isize) -> Id {
        // SAFETY: eleven-argument `objc_msgSend` matches the `+otherEventWithType:...` prototype.
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

    /// Wraps `cg` with `+[NSEvent eventWithCGEvent:]` and releases `cg`.
    /// # Safety
    /// `cg` must be a live caller-owned `CGEventRef` or null.
    unsafe fn cg_wrapped(cg: *mut c_void) -> Id {
        if cg.is_null() {
            return Id::NIL;
        }
        // SAFETY: `+eventWithCGEvent:` (`@@:^{__CGEvent=}`) retains `cg`, which `CFRelease` balances.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, *mut c_void) -> Id = checked_msg();
            let ev = f(class(c"NSEvent").as_id(), sel!(eventWithCGEvent:), cg);
            CFRelease(cg);
            ev
        }
    }

    /// Constructs a CG-backed mouse `NSEvent` at window location `loc`.
    /// # Safety
    /// `kind` must be a valid CoreGraphics mouse event type.
    unsafe fn cg_mouse(kind: usize, loc: CGPoint, button: u32) -> Id {
        // SAFETY: CoreGraphics extern prototypes match the framework headers.
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

    /// Constructs a CG-backed scroll-wheel `NSEvent`.
    unsafe fn cg_scroll(units: u32, wheel1: i32, wheel2: i32) -> Id {
        // SAFETY: CoreGraphics extern prototypes match the framework headers.
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

    /// Constructs a CG-backed key `NSEvent`, optionally retyped.
    unsafe fn cg_key(code: u16, down: bool, retype: Option<usize>) -> Id {
        // SAFETY: CoreGraphics extern prototypes match the framework headers.
        unsafe {
            let cg = CGEventCreateKeyboardEvent(std::ptr::null(), code, down);
            if let Some(kind) = retype {
                CGEventSetType(cg, kind as u32);
            }
            cg_wrapped(cg)
        }
    }

    /// Constructs a bare `CGEvent` retyped to `kind`.
    unsafe fn cg_typed(kind: usize) -> Id {
        // SAFETY: CoreGraphics extern prototypes match the framework headers.
        unsafe {
            let cg = CGEventCreate(std::ptr::null());
            CGEventSetType(cg, kind as u32);
            cg_wrapped(cg)
        }
    }

    /// Sends `-(void)s:(NSEvent *)arg` to `target`.
    /// # Safety
    /// `target` must be live and implement `s` with signature `v@:@`.
    unsafe fn send_v_id(target: Id, s: Sel, arg: Id) {
        // SAFETY: caller guarantees `target` implements `s` with `v@:@`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
            f(target, s, arg);
        }
    }

    /// Sends `-(BOOL)s:(NSEvent *)arg` to `target`.
    /// # Safety
    /// `target` must be live and implement `s` with signature `B@:@`.
    unsafe fn send_b_id(target: Id, s: Sel, arg: Id) -> Bool {
        // SAFETY: caller guarantees `target` implements `s` with `B@:@`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Bool = checked_msg();
            f(target, s, arg)
        }
    }

    /// Sends `-(id)s` to `target`.
    /// # Safety
    /// `target` must be live and implement `s` with signature `@@:`.
    unsafe fn send_id(target: Id, s: Sel) -> Id {
        // SAFETY: caller guarantees `target` implements `s` with `@@:`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            f(target, s)
        }
    }

    /// Sends `-(NSInteger)s` to `target`.
    /// # Safety
    /// `target` must be live and implement `s` with signature `q@:`.
    unsafe fn send_isize(target: Id, s: Sel) -> isize {
        // SAFETY: caller guarantees `target` implements `s` with `q@:`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> isize = checked_msg();
            f(target, s)
        }
    }

    /// Reads `-(NSUInteger)type` from `ev`.
    unsafe fn event_type(ev: Id) -> usize {
        // SAFETY: `-type` (`Q@:`) is valid on every `NSEvent`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
            f(ev, sel!(type))
        }
    }

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
                // SAFETY: `-keyCode` has signature `S@:`.
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

    /// Runs the negative-control child selected by [`CONTROL_ENV`].
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
                    let ev =
                        // SAFETY: `MOUSE_MOVED` is a valid mouse event type.
                        unsafe { mouse_event(MOUSE_MOVED, CGPoint { x: 10.0, y: 10.0 }, 0, 0) };
                    if ev.is_null() {
                        eprintln!("control: +[NSEvent mouseEventWithType:…] answered nil");
                        return NOT_RUN;
                    }
                    // SAFETY: `poke:` is registered `v@:@` on the live `obj` instance.
                    unsafe { send_v_id(obj.as_id(), sel!(poke:), ev) };
                    println!("control: SURVIVED the v0.72.0 send");
                    trap::set_row("control: -alive after the containment");
                    // SAFETY: `alive` is registered `q@:` on `obj`.
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
                    // SAFETY: `panicInside` is registered `v@:` on `obj`.
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
        /// `WindowEvent` names delivered by winit.
        events: Vec<String>,
        done: bool,
        stage: usize,
        pending: Option<Deferred>,
        /// Count of rows driven with an event.
        driven: usize,
        /// Rows not driven, with reasons.
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
            // SAFETY: `-window` is `@@:` on `NSView` and `-windowNumber` is `q@:` on `NSWindow`.
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
            // SAFETY: `row` is registered `v@:@` and `event` is a live `NSEvent`.
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
            // SAFETY: `row` is registered `B@:@` and `event` is a live `NSEvent`.
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
                .with_inner_size(aterm_window::dpi::LogicalSize::new(480.0, 320.0))
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
            if let Some(view) = self.view() {
                // SAFETY: `-window` (`@@:`) and `-setFrameOrigin:` (`v@:{CGPoint=dd}`) match AppKit.
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
            // SAFETY: mouse event types passed to the mouse event factories.
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
                let (a, b) =
                    // SAFETY: mouse event types passed to the mouse event factories.
                    unsafe { (mouse_event(kind, p, win, clicks), cg_mouse(kind, p, button)) };
                self.drive(view, row, name, a, &format!("type {kind}/factory"));
                self.drive(view, row, name, b, &format!("type {kind}/cg"));
            }
            // SAFETY: mouse and key event types match their factories.
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
            // SAFETY: enter/exit event types match their factories.
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
            // SAFETY: CG scroll constructor arguments are valid.
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
            // SAFETY: key event types match their factories.
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
            // SAFETY: `FLAGS_CHANGED` and `empty` match `key_event` preconditions.
            let up = unsafe { key_event(FLAGS_CHANGED, win, empty.id(), 0, VK_SHIFT) };
            self.drive(
                view,
                sel!(flagsChanged:),
                "flagsChanged:",
                up,
                "FlagsChanged/factory release",
            );
            trap::set_row("cancelOperation: <- (currentEvent as the run loop left it)");
            // SAFETY: `cancelOperation:` is registered `v@:@` and accepts a nil sender.
            unsafe { send_v_id(view, sel!(cancelOperation:), Id::NIL) };
            self.driven += 1;
            println!(
                "    ok   cancelOperation: <- (currentEvent as the run loop left it) returned"
            );
            self.pending = Some(Box::new(|d: &mut Driver| {
                let got = d.drain();
                let keys = Driver::count(&got, "KeyboardInput");
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
        /// the view.
        fn stage_send_event(&mut self, win: isize) {
            println!("\n=== 7. -[NSApplication sendEvent:] with every shape ===");
            let _ = self.drain();
            // SAFETY: `+sharedApplication` has signature `@@:`.
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
            // SAFETY: each event type is passed to the matching `NSEvent` factory.
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
                // SAFETY: `-sendEvent:` has signature `v@:@` on `NSApplication`.
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
