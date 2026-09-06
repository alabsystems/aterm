// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE MODAL DRIVER: `aterm-gui`'s modal-alert subsystem and its window
//! capture, answering for real.
//!
//! # Why this exists beside the other four
//!
//! W13 ported `aterm-gui`'s last five `objc2` files. Four of those five are one
//! subsystem — `alert_keys.rs`, `menu.rs`'s `confirm`, `lib.rs`'s paste sheet
//! and `app_introspect.rs` — and the sixteenth adversarial pass named it "the
//! least-driven surface in the whole port". It was: `objc_live_class_audit`
//! asks whether declared classes are shaped right, `objc_ime_drive` drives
//! composition, `objc_toolbar_drive` the tab strip, `objc_window_drive` the
//! window. NONE of them touches an `NSAlert`, a sheet, a completion block, an
//! event monitor or a `NSBitmapImageRep`.
//!
//! `crates/aterm-objc/tests/gui_sent_prototypes.rs` is the other half of the
//! evidence and the cheaper half — it reads `method_getTypeEncoding` for all 40
//! selectors these files send. It cannot catch a correct send of the wrong
//! selector, a block whose ABI is right but whose ownership is wrong, an
//! identity predicate that answers for the wrong window, or a capture chain
//! that returns bytes which are not a PNG. That is this file.
//!
//! # What it drives, and what it drives it against
//!
//! Every send here is a raw typed `objc_msgSend` cast, DELIBERATELY NOT through
//! `aterm_objc::send::*` — a driver that reached for the same helpers the port
//! reaches for would agree with the port about a shape they both got wrong.
//! The blocks are the exception and the reason is the same in reverse: the
//! block ABI IS the thing under test, so `aterm_objc::RcBlock` is the subject
//! rather than the instrument.
//!
//! The constants are checked BEHAVIOURALLY where AppKit can be asked. The
//! driver never asserts that `NSAlertFirstButtonReturn` is 1000 because
//! `appkit.rs` says so; it clicks the first button and reads what AppKit
//! actually delivers. `crates/aterm-objc/tests/gui_appkit_constants.rs` is the
//! oracle for the values; this is the oracle for the meaning.
//!
//! # The claim in `alert_keys.rs` that nothing had ever checked
//!
//! Its documentation says the interceptor exists because "AppKit gives [the
//! first button] Return with an EMPTY modifier mask, which is why `alert_keys`
//! has to route ⌘Return itself". The whole subsystem rests on that, and it was
//! a sentence. Stage 2 asks AppKit.
//!
//! # Exit codes — the ladder gates on these, not on the prose
//!
//! * `0` — every stage that could run, ran, and agreed.
//! * `1` — a finding.
//! * `2` — NOT RUN: no event loop, no window server, or no window. Never a pass.

/// Every stage agreed.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const PASS: i32 = 0;
/// At least one finding. See the transcript.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const FAIL: i32 = 1;
/// Nothing was driven. NEVER reported as a pass.
const NOT_RUN: i32 = 2;

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "objc-alert-drive: NOT RUN — this drives aterm-gui's macOS modal-alert \
         subsystem, which does not exist off macOS."
    );
    std::process::ExitCode::from(NOT_RUN as u8)
}

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(macos::run() as u8)
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use aterm_objc::{Bool, Id, Obj, RcBlock, Sel, class, msg, ns_string, sel};
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, EventLoop};
    use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::{Window, WindowId};

    use super::{FAIL, NOT_RUN, PASS};

    /// Long enough for macOS to launch, hand back a window and run every stage.
    const BUDGET: Duration = Duration::from_secs(90);

    /// `NSEventMaskKeyDown`. Spelled here rather than imported: `appkit.rs`'s
    /// table is `pub(crate)`, and a driver that shared the port's constant
    /// could not disagree with it.
    const NS_EVENT_MASK_KEY_DOWN: usize = 1 << 10;
    /// `NSBitmapImageFileTypePNG`.
    const NS_BITMAP_IMAGE_FILE_TYPE_PNG: usize = 4;
    /// `NSColorRenderingIntentPerceptual`.
    const NS_COLOR_RENDERING_INTENT_PERCEPTUAL: isize = 3;

    // ------------------------------------------------------------------ sends

    /// `-(id)sel` — every +0 object getter this driver reads.
    ///
    /// # Safety
    /// `recv` must be live and implement `sel` with this prototype.
    unsafe fn get(recv: Id, s: Sel) -> Id {
        // SAFETY: the caller pins the prototype; `@16@0:8`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = msg();
            f(recv, s)
        }
    }

    /// `-(NSUInteger)sel`.
    ///
    /// # Safety
    /// As [`get`].
    unsafe fn get_usize(recv: Id, s: Sel) -> usize {
        // SAFETY: the caller pins the prototype; `Q16@0:8`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = msg();
            f(recv, s)
        }
    }

    /// `-(id)sel:(id)`.
    ///
    /// # Safety
    /// As [`get`], with `a` live or nil.
    unsafe fn get_id(recv: Id, s: Sel, a: Id) -> Id {
        // SAFETY: the caller pins the prototype; `@24@0:8@16`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = msg();
            f(recv, s, a)
        }
    }

    /// `-(void)sel:(id)`.
    ///
    /// # Safety
    /// As [`get_id`].
    unsafe fn put_id(recv: Id, s: Sel, a: Id) {
        // SAFETY: the caller pins the prototype; `v24@0:8@16`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, Id) = msg();
            f(recv, s, a);
        }
    }

    /// `-(id)sel:(NSUInteger)`.
    ///
    /// # Safety
    /// As [`get`].
    unsafe fn get_at(recv: Id, s: Sel, i: usize) -> Id {
        // SAFETY: the caller pins the prototype; `@24@0:8Q16`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Id = msg();
            f(recv, s, i)
        }
    }

    /// An `NSString`'s bytes as a Rust `String`, through `-UTF8String`.
    ///
    /// Deliberately not `appkit::nsstring_to_rust`: the driver reads the text
    /// its own way so a port bug in the conversion cannot hide behind itself.
    ///
    /// # Safety
    /// `s` must be nil or a live `NSString`.
    unsafe fn text(s: Id) -> String {
        if s.is_null() {
            return String::new();
        }
        // SAFETY: `-UTF8String` is `r*16@0:8` and answers an interior pointer
        // valid until the receiver is autoreleased; it is copied here at once.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> *const std::ffi::c_char = msg();
            let p = f(s, sel!(UTF8String));
            if p.is_null() {
                return String::new();
            }
            std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }

    /// The class name of a live object, for identity reporting.
    ///
    /// # Safety
    /// `o` must be nil or live.
    unsafe fn class_name(o: Id) -> String {
        if o.is_null() {
            return "nil".to_owned();
        }
        // SAFETY: `-class` answers the live class; `class_getName` is a C call
        // on it.
        unsafe {
            let cls = aterm_objc::class_of(o);
            aterm_objc::class_name(cls).to_string_lossy().into_owned()
        }
    }

    // ------------------------------------------------------------------ report

    #[derive(Default)]
    struct Report {
        findings: Vec<String>,
        blocked: Option<String>,
        ran: usize,
    }

    impl Report {
        fn check(&mut self, ok: bool, what: &str) {
            if ok {
                println!("    ok   {what}");
            } else {
                println!("    FAIL {what}");
                self.findings.push(what.to_owned());
            }
        }
        fn note(&mut self, what: &str) {
            println!("    --   {what}");
        }
    }

    // ------------------------------------------------------------------ driver

    struct Driver {
        window: Option<Window>,
        report: Report,
        stage: usize,
        done: bool,
        /// The alert under drive, and the two buttons it handed back.
        alert: Option<Obj>,
        accept: Option<Obj>,
        cancel: Option<Obj>,
        panel: Option<Obj>,
        /// What the sheet's completion block was handed, if it ran.
        answered: Rc<RefCell<Option<isize>>>,
        /// The `RcBlock` AppKit copied — dropped early on purpose, to prove the
        /// copy is what keeps the handler alive.
        completion_dropped: bool,
        /// The monitor token, while installed.
        monitor: Option<Obj>,
        /// Events the installed monitor saw.
        monitor_saw: Rc<RefCell<Vec<u16>>>,
    }

    impl Driver {
        /// The window's `NSView` and `NSWindow`, reached without the port.
        fn view_and_window(&self) -> Option<(Id, Id)> {
            let handle = self.window.as_ref()?.window_handle().ok()?;
            let RawWindowHandle::AppKit(h) = handle.as_raw() else {
                return None;
            };
            let view = Id::from_ptr(h.ns_view.as_ptr());
            // SAFETY: `view` is winit's live content view.
            let window = unsafe { get(view, sel!(window)) };
            if window.is_null() {
                return None;
            }
            Some((view, window))
        }
    }

    impl ApplicationHandler for Driver {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let attrs = Window::default_attributes()
                .with_title("objc-alert-drive")
                .with_inner_size(winit::dpi::LogicalSize::new(520.0, 360.0))
                .with_visible(true);
            match el.create_window(attrs) {
                Ok(w) => self.window = Some(w),
                Err(e) => {
                    self.report.blocked = Some(format!("no window could be created: {e}"));
                    self.done = true;
                    el.exit();
                }
            }
        }

        fn window_event(&mut self, _el: &ActiveEventLoop, _id: WindowId, _e: WindowEvent) {}

        fn about_to_wait(&mut self, el: &ActiveEventLoop) {
            if self.done {
                return;
            }
            let Some((view, window)) = self.view_and_window() else {
                self.report.blocked = Some("the window has no AppKit view or window".to_owned());
                self.done = true;
                el.exit();
                return;
            };
            // One stage per wait, so AppKit's own work between stages (the
            // sheet animation, the completion block) is delivered by the pump
            // before the next stage reads for it.
            self.stage += 1;
            self.report.ran += 1;
            match self.stage {
                1 => self.stage_alert_and_buttons(),
                2 => self.stage_first_button_is_bare_return(),
                3 => self.stage_block_abi_directly(),
                4 => self.stage_sheet_attaches(window),
                5 => self.stage_monitor_install(window),
                6 => self.stage_monitor_sees_a_key(window),
                7 => self.stage_monitor_removed(window),
                8 => self.stage_click_accept(),
                9 => self.stage_sheet_detached_and_answered(window),
                10 => self.stage_menu_bar_read(),
                11 => self.stage_chrome_capture(view, window),
                _ => {
                    self.done = true;
                    el.exit();
                }
            }
        }
    }

    impl Driver {
        /// `+[NSAlert new]`, the two buttons, and the panel — the construction
        /// half of `present_multiline_paste_sheet` and `menu::confirm`.
        fn stage_alert_and_buttons(&mut self) {
            println!("\n[1] NSAlert — +new, addButtonWithTitle: order, -window");
            let (Some(paste), Some(cancel_title), Some(message)) = (
                ns_string("Paste"),
                ns_string("Cancel"),
                ns_string("Paste multiple lines?"),
            ) else {
                self.report
                    .check(false, "NSString construction failed for the alert's text");
                return;
            };
            // SAFETY: `+[NSAlert new]` is `@16@0:8` on the metaclass and +1.
            let alert = unsafe { Obj::from_owned(get(class(c"NSAlert").as_id(), sel!(new))) };
            let Some(alert) = alert else {
                self.report.check(false, "+[NSAlert new] answered nil");
                return;
            };
            // SAFETY: the setters are `v24@0:8@16` and COPY their argument, so
            // the +1 strings may drop at the end of this stage.
            unsafe {
                put_id(alert.id(), sel!(setMessageText:), message.id());
            }
            // SAFETY: `-addButtonWithTitle:` is `@24@0:8@16` and +0.
            let (accept, cancel) = unsafe {
                (
                    Obj::retain(get_id(alert.id(), sel!(addButtonWithTitle:), paste.id())),
                    Obj::retain(get_id(
                        alert.id(),
                        sel!(addButtonWithTitle:),
                        cancel_title.id(),
                    )),
                )
            };
            let (Some(accept), Some(cancel)) = (accept, cancel) else {
                self.report
                    .check(false, "-addButtonWithTitle: answered nil for a button");
                return;
            };
            // SAFETY: `-buttons` is `@16@0:8` and answers the NSArray AppKit
            // keeps; `-count`/`-objectAtIndex:` on it are the census's rows.
            let (n, first, second) = unsafe {
                let buttons = get(alert.id(), sel!(buttons));
                let n = if buttons.is_null() {
                    0
                } else {
                    get_usize(buttons, sel!(count))
                };
                (
                    n,
                    if n > 0 {
                        get_at(buttons, sel!(objectAtIndex:), 0)
                    } else {
                        Id::NIL
                    },
                    if n > 1 {
                        get_at(buttons, sel!(objectAtIndex:), 1)
                    } else {
                        Id::NIL
                    },
                )
            };
            self.report.check(
                n == 2,
                &format!("the alert holds {n} button(s) after two additions (want 2)"),
            );
            self.report.check(
                first == accept.id() && second == cancel.id(),
                "the buttons come back in the order they were added — the whole \
                 reason `addButtonWithTitle:` order decides which is default",
            );
            // SAFETY: `-title` on a live NSButton, `-window` on the alert.
            let (t0, t1, panel) = unsafe {
                (
                    text(get(first, sel!(title))),
                    text(get(second, sel!(title))),
                    Obj::retain(get(alert.id(), sel!(window))),
                )
            };
            self.report.check(
                t0 == "Paste" && t1 == "Cancel",
                &format!("their titles read {t0:?} and {t1:?}"),
            );
            let Some(panel) = panel else {
                self.report.check(false, "-[NSAlert window] answered nil");
                return;
            };
            // SAFETY: `panel` is the live alert panel.
            let cls = unsafe { class_name(panel.id()) };
            self.report.check(
                cls.contains("Panel") || cls.contains("Window"),
                &format!("-[NSAlert window] is a {cls}"),
            );
            self.alert = Some(alert);
            self.accept = Some(accept);
            self.cancel = Some(cancel);
            self.panel = Some(panel);
        }

        /// THE SENTENCE `alert_keys.rs` RESTS ON, asked of AppKit.
        fn stage_first_button_is_bare_return(&mut self) {
            println!("\n[2] the default button's key equivalent — the premise of the interceptor");
            let Some(accept) = self.accept.as_ref().map(Obj::id) else {
                self.report.note("no alert: stage 1 did not complete");
                return;
            };
            let Some(cancel) = self.cancel.as_ref().map(Obj::id) else {
                return;
            };
            // SAFETY: `-keyEquivalent` is `@16@0:8` and
            // `-keyEquivalentModifierMask` is `Q16@0:8`, both on a live NSButton.
            let (ka, ma, kc, mc) = unsafe {
                (
                    text(get(accept, sel!(keyEquivalent))),
                    get_usize(accept, sel!(keyEquivalentModifierMask)),
                    text(get(cancel, sel!(keyEquivalent))),
                    get_usize(cancel, sel!(keyEquivalentModifierMask)),
                )
            };
            self.report.check(
                ka == "\r",
                &format!("the FIRST button's key equivalent is {ka:?} (want \"\\r\")"),
            );
            self.report.check(
                ma == 0,
                &format!(
                    "…with an EMPTY modifier mask ({ma:#x}). This is exactly why \
                     `alert_keys` must route Command-Return itself: AppKit's stock \
                     equivalent does not match a Command-modified Return"
                ),
            );
            self.report.check(
                kc == "\u{1b}",
                &format!("the SECOND button's key equivalent is {kc:?} (want Escape)"),
            );
            self.report
                .note(&format!("cancel's modifier mask is {mc:#x}"));
        }

        /// The BLOCK ABI, invoked directly through its own function pointer.
        ///
        /// This is the deterministic half of the interceptor's evidence: it does
        /// not depend on AppKit choosing to dispatch anything. A heap block is
        /// `{ void *isa; int flags; int reserved; void (*invoke)(void *, ...);
        /// struct descriptor *; }` — and the two `int`s SHARE one eight-byte
        /// slot, so `invoke` is at offset 16, the THIRD pointer-sized field and
        /// not the fourth. Reading the fourth is a crash, measured: the first
        /// spelling of this stage read `add(3)` and took the process down with
        /// a bad function pointer, which is a fair summary of what an ABI this
        /// file exists to check gets wrong. Calling `invoke` with the block as
        /// the first argument is precisely what AppKit does.
        fn stage_block_abi_directly(&mut self) {
            println!(
                "\n[3] RcBlock — the (NSEvent*) -> NSEvent* prototype, invoked as AppKit does"
            );
            let seen: Rc<RefCell<Vec<Id>>> = Rc::new(RefCell::new(Vec::new()));
            let recorder = Rc::clone(&seen);
            let swallow_marker = Id::from_ptr(0x1234_usize as *mut c_void);
            let closure = move |event: Id| -> Id {
                recorder.borrow_mut().push(event);
                if event == swallow_marker {
                    Id::NIL
                } else {
                    event
                }
            };
            // SAFETY: the prototype is `(id) -> id`, which is what a local
            // monitor's handler is; `Id` is `Encode`-`"@"` on both sides.
            let Some(block) = (unsafe { RcBlock::new1(closure) }) else {
                self.report.check(false, "RcBlock::new1 could not allocate");
                return;
            };
            // SAFETY: `block.as_ptr()` is a live heap block; `invoke` is at the
            // fourth pointer slot of the standard `Block_layout`, and its
            // prototype for a one-argument block is `(block, arg) -> ret`.
            let (pass, swallowed) = unsafe {
                let raw = block.as_ptr().cast::<*const c_void>();
                let invoke = *raw.add(2);
                let f: extern "C-unwind" fn(*mut c_void, Id) -> Id = std::mem::transmute(invoke);
                let a = Id::from_ptr(0x4321_usize as *mut c_void);
                (f(block.as_ptr(), a), f(block.as_ptr(), swallow_marker))
            };
            self.report.check(
                pass.addr() == 0x4321,
                &format!("a pass-through returns the event unchanged (read {pass:?})"),
            );
            self.report.check(
                swallowed.is_null(),
                &format!("a swallow returns nil (read {swallowed:?})"),
            );
            self.report.check(
                seen.borrow().len() == 2,
                &format!(
                    "the Rust closure ran {} time(s) — the capture really crossed \
                     the C boundary",
                    seen.borrow().len()
                ),
            );
        }

        /// `-beginSheetModalForWindow:completionHandler:` and the identity
        /// predicate `PasteConfirm::is_attached` and `confirmation_owns_event`
        /// both stand on.
        fn stage_sheet_attaches(&mut self, window: Id) {
            println!("\n[4] the sheet attaches — attachedSheet is the alert's own panel");
            let (Some(alert), Some(panel)) = (self.alert.as_ref(), self.panel.as_ref()) else {
                self.report.note("no alert: stage 1 did not complete");
                return;
            };
            let answered = Rc::clone(&self.answered);
            let completion = move |response: isize| {
                *answered.borrow_mut() = Some(response);
            };
            // SAFETY: the prototype is `(NSModalResponse) -> void` and
            // `NSModalResponse` is `NSInteger`.
            let Some(handler) = (unsafe { RcBlock::new1(completion) }) else {
                self.report.check(false, "RcBlock::new1 could not allocate");
                return;
            };
            // SAFETY: `-beginSheetModalForWindow:completionHandler:` is
            // `v32@0:8@16@?24` — the last argument is a BLOCK, which is why it
            // is spelled as a raw pointer here.
            unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel, Id, *mut c_void) = msg();
                f(
                    alert.id(),
                    sel!(beginSheetModalForWindow:completionHandler:),
                    window,
                    handler.as_ptr(),
                );
            }
            // DROPPED ON PURPOSE, one line after the call. `aterm-objc`'s
            // `RcBlock` doc claims AppKit COPIES the block, so the two real
            // sites are correct even though they drop before the handler fires.
            // If that were wrong, stage 9 would read a use-after-free rather
            // than an answer.
            drop(handler);
            self.completion_dropped = true;
            // SAFETY: `-attachedSheet` is `@16@0:8` on a live NSWindow.
            let attached = unsafe { get(window, sel!(attachedSheet)) };
            self.report.check(
                attached == panel.id(),
                &format!(
                    "-[NSWindow attachedSheet] is the alert's own panel \
                     (read {attached:?}, want {:?})",
                    panel.id()
                ),
            );
            // SAFETY: `-isVisible` is `B16@0:8`.
            let visible = unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel) -> Bool = msg();
                f(panel.id(), sel!(isVisible)).as_bool()
            };
            self.report
                .note(&format!("the panel reports isVisible = {visible}"));
        }

        /// `+addLocalMonitorForEventsMatchingMask:handler:` — the install, the
        /// token, and its ownership.
        fn stage_monitor_install(&mut self, _window: Id) {
            println!("\n[5] the key interceptor installs and hands back a token");
            let saw = Rc::clone(&self.monitor_saw);
            let handler = move |event: Id| -> Id {
                if event.is_null() {
                    return event;
                }
                // SAFETY: a local monitor is handed a live keyDown event.
                let code = unsafe {
                    let f: unsafe extern "C-unwind" fn(Id, Sel) -> u16 = msg();
                    f(event, sel!(keyCode))
                };
                saw.borrow_mut().push(code);
                // THE INTERCEPTOR'S OTHER HALF. `alert_keys.rs` answers nil for
                // the chord it handles itself, and AppKit reads a nil from a
                // local monitor as "consumed": the event goes no further. The
                // driver does the same for the Return stage 6 sends, so the
                // sheet's default button — whose key equivalent is that bare
                // Return (stage 2) — never sees it, and stage 8's click is the
                // only thing that can end the sheet. Everything else passes.
                if code == 36 { Id::NIL } else { event }
            };
            // SAFETY: the prototype is `(NSEvent *) -> NSEvent *`.
            let Some(block) = (unsafe { RcBlock::new1(handler) }) else {
                self.report.check(false, "RcBlock::new1 could not allocate");
                return;
            };
            // SAFETY: `+addLocalMonitorForEventsMatchingMask:handler:` is
            // `@32@0:8Q16@?24` on the metaclass; the result is +0 and
            // autoreleased, so it is RETAINED rather than adopted.
            let token = unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel, usize, *mut c_void) -> Id = msg();
                Obj::retain(f(
                    class(c"NSEvent").as_id(),
                    sel!(addLocalMonitorForEventsMatchingMask:handler:),
                    NS_EVENT_MASK_KEY_DOWN,
                    block.as_ptr(),
                ))
            };
            drop(block);
            match token {
                Some(t) => {
                    // SAFETY: `t` is the live token AppKit returned.
                    let cls = unsafe { class_name(t.id()) };
                    self.report
                        .check(true, &format!("the monitor token is a live {cls}"));
                    self.monitor = Some(t);
                }
                None => self.report.check(
                    false,
                    "+addLocalMonitorForEventsMatchingMask:handler: answered nil",
                ),
            }
        }

        /// A REAL keyDown, sent through `-[NSApplication sendEvent:]` the way
        /// AppKit's own run loop sends one, reaching the installed monitor —
        /// and consumed there.
        ///
        /// SENT, NOT POSTED, and the difference is the whole determinism of
        /// stages 6 to 8. The first macOS run of this driver (2026-09-05)
        /// posted the event with `-postEvent:atStart:` and left the pump between
        /// stages to dispatch it; the pump did so one stage LATE: stage 7 read
        /// an empty monitor and removed it, and the Return then reached the
        /// sheet through AppKit's key-equivalent path and answered it before
        /// stage 8 could click. Which pump AppKit picks is not this driver's to
        /// control, so nothing here is left to a pump: `-sendEvent:` is the
        /// call AppKit's `-run` loop makes with every dequeued event and the
        /// call inside which local monitors are invoked (measured below — the
        /// block has run by the time the send returns), so the monitor's answer
        /// is read on the next line and no event is left in the queue for a
        /// later pump to deliver.
        fn stage_monitor_sees_a_key(&mut self, window: Id) {
            println!(
                "\n[6] a keyDown through -[NSApplication sendEvent:] reaches the monitor, \
                 and its nil consumes it"
            );
            if self.monitor.is_none() {
                self.report.note("no monitor: stage 5 did not complete");
                return;
            }
            // SAFETY: `-windowNumber` is `q16@0:8`.
            let number = unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel) -> isize = msg();
                f(window, sel!(windowNumber))
            };
            let (Some(chars), Some(bare)) = (ns_string("\r"), ns_string("\r")) else {
                self.report.check(false, "NSString construction failed");
                return;
            };
            // SAFETY: `+[NSEvent keyEventWithType:location:modifierFlags:
            // timestamp:windowNumber:context:characters:charactersIgnoringModifiers:
            // isARepeat:keyCode:]` is
            // `@96@0:8Q16{CGPoint=dd}24Q40d48q56@64@72@80B88S92` — a class
            // method whose `context:` is the always-nil NSGraphicsContext the
            // hardware-key injector also passes.
            let event = unsafe {
                #[allow(clippy::type_complexity)]
                let f: unsafe extern "C-unwind" fn(
                    Id,
                    Sel,
                    usize,
                    aterm_objc::CGPoint,
                    usize,
                    f64,
                    isize,
                    Id,
                    Id,
                    Id,
                    Bool,
                    u16,
                ) -> Id = msg();
                f(
                    class(c"NSEvent").as_id(),
                    sel!(
                        keyEventWithType:location:modifierFlags:timestamp:windowNumber:context:characters:charactersIgnoringModifiers:isARepeat:keyCode:
                    ),
                    10, // NSEventTypeKeyDown
                    aterm_objc::CGPoint { x: 0.0, y: 0.0 },
                    0,
                    0.0,
                    number,
                    Id::NIL,
                    chars.id(),
                    bare.id(),
                    Bool::NO,
                    36, // kVK_Return
                )
            };
            if event.is_null() {
                self.report
                    .check(false, "+[NSEvent keyEventWithType:…] answered nil");
                return;
            }
            // The receiver is the SWIZZLED `-[NSApplication sendEvent:]` — the
            // fork's `SwizzleSite` override with the containment inside it and
            // the displaced AppKit implementation chained behind — so this is
            // also one real keyDown through that path, from a process that
            // need not be active or own the key window for the monitor to run.
            //
            // SAFETY: `+[NSApplication sharedApplication]` is `@16@0:8`, and
            // `-sendEvent:` is `v24@0:8@16`, sent with the live event just
            // built, on the main thread, from inside the pump.
            unsafe {
                let app = get(class(c"NSApplication").as_id(), sel!(sharedApplication));
                put_id(app, sel!(sendEvent:), event);
            }
            let saw = self.monitor_saw.borrow().clone();
            self.report.check(
                saw.contains(&36),
                &format!(
                    "the monitor ran INSIDE -sendEvent: and was handed key codes \
                     {saw:?} (want a 36) — synchronous, so no pump decides it"
                ),
            );
            self.report.check(
                self.answered.borrow().is_none(),
                "the handler's nil consumed the Return: the sheet's default button \
                 never saw it, and the completion has not run",
            );
        }

        /// `+removeMonitor:` — the `Drop` half of `ConfirmKeyWatch`.
        fn stage_monitor_removed(&mut self, _window: Id) {
            println!("\n[7] the monitor saw the key, and removeMonitor: ends it");
            let saw = self.monitor_saw.borrow().clone();
            // A CHECK, not a note. Stage 6's send was synchronous, so a monitor
            // that has not seen the Return by now never will; the reading no
            // longer depends on key focus or on which pump AppKit dispatched
            // in (it depended on both until 2026-09-05, when this stage read an
            // empty monitor on a box without focus and the pump after it
            // delivered the Return to the sheet instead).
            self.report.check(
                saw.contains(&36),
                &format!(
                    "the monitor was handed key codes {saw:?} (want a 36), one pump \
                     after the send and before its removal"
                ),
            );
            let Some(token) = self.monitor.take() else {
                return;
            };
            // SAFETY: `+[NSEvent removeMonitor:]` is `v24@0:8@16` — a CLASS
            // method, which is why the receiver is NSEvent and not the token —
            // with the exact token AppKit returned, removed once.
            unsafe {
                put_id(class(c"NSEvent").as_id(), sel!(removeMonitor:), token.id());
            }
            self.report
                .check(true, "removeMonitor: accepted the token without raising");
            drop(token);
        }

        /// `-performClick:` — what the interceptor does instead of plumbing a
        /// response code of its own.
        fn stage_click_accept(&mut self) {
            println!("\n[8] performClick: on the first button ends the sheet");
            let Some(accept) = self.accept.as_ref().map(Obj::id) else {
                self.report.note("no alert: stage 1 did not complete");
                return;
            };
            // ONE ROAD. The sheet must still be up here: it is modeless to
            // this thread (the whole reason `PasteConfirmed` is an event), and
            // the only key event this process has seen was consumed by the
            // monitor in stage 6, synchronously, with nothing left queued. A
            // completion that has already run would mean the Return escaped —
            // the nondeterminism the first macOS run measured, and this check
            // is what turns it into a named finding instead of a second road.
            self.report.check(
                self.answered.borrow().is_none(),
                "the completion has NOT run yet — the sheet is modeless to this \
                 thread, and the monitor's nil kept stage 6's Return from it",
            );
            // SAFETY: `-performClick:` is `v24@0:8@16`; `Id::NIL` is the
            // conventional nil sender.
            unsafe {
                put_id(accept, sel!(performClick:), Id::NIL);
            }
            self.report
                .note("clicked; the pump between stages delivers the completion");
        }

        /// The answer AppKit actually delivered, and the sheet coming off.
        fn stage_sheet_detached_and_answered(&mut self, window: Id) {
            println!("\n[9] the completion block ran, on a block AppKit had COPIED");
            let answer = *self.answered.borrow();
            self.report.check(
                self.completion_dropped,
                "the RcBlock was dropped immediately after the sheet call",
            );
            match answer {
                Some(r) => {
                    self.report.check(
                        r == 1000,
                        &format!(
                            "clicking the FIRST button delivered response {r} — \
                             NSAlertFirstButtonReturn, read from AppKit rather \
                             than from the port's constant table"
                        ),
                    );
                }
                None => self.report.check(
                    false,
                    "the completion block never ran: either the click did not \
                     end the sheet, or the copied block did not survive the \
                     RcBlock's drop",
                ),
            }
            // SAFETY: `-attachedSheet` on the live NSWindow.
            let attached = unsafe { get(window, sel!(attachedSheet)) };
            self.report.check(
                attached.is_null(),
                &format!(
                    "-[NSWindow attachedSheet] is nil once the sheet ended \
                     (read {attached:?}) — the predicate `PasteConfirm::is_attached` \
                     uses to discard a stale entry"
                ),
            );
        }

        /// `read_native_chrome`'s menu-bar half: the NSArray walk that replaced
        /// `objc2`'s iterators.
        fn stage_menu_bar_read(&mut self) {
            println!("\n[10] the menu bar, read through count/objectAtIndex:");
            // SAFETY: `+sharedApplication` and `-mainMenu` are `@16@0:8`.
            let (app, main) = unsafe {
                let app = get(class(c"NSApplication").as_id(), sel!(sharedApplication));
                let main = if app.is_null() {
                    Id::NIL
                } else {
                    get(app, sel!(mainMenu))
                };
                (app, main)
            };
            self.report
                .check(!app.is_null(), "+[NSApplication sharedApplication] is live");
            if main.is_null() {
                self.report.note(
                    "this process has no main menu (winit installs one only for a \
                     bundled app); the walk below has nothing to read",
                );
                return;
            }
            // SAFETY: `-itemArray` is `@16@0:8`; `-count`/`-objectAtIndex:`
            // and `-title`/`-submenu` are the census's rows.
            let (n, titles) = unsafe {
                let tops = get(main, sel!(itemArray));
                let n = if tops.is_null() {
                    0
                } else {
                    get_usize(tops, sel!(count))
                };
                let mut titles = Vec::new();
                for i in 0..n {
                    let top = get_at(tops, sel!(objectAtIndex:), i);
                    if top.is_null() {
                        continue;
                    }
                    titles.push(text(get(top, sel!(title))));
                }
                (n, titles)
            };
            self.report.check(
                titles.len() == n,
                &format!("the walk read {} of {n} top-level item(s)", titles.len()),
            );
            // THE TITLE IS AN NSString, checked. `read_native_chrome` formats
            // this value with `{title:?}` and the port's note says that is
            // byte-identical to `objc2`'s `Debug` on its `NSString`. That is
            // only true if what comes back IS one.
            //
            // A reader who sees `["NSMenuItem"]` below should not go looking for
            // a bug: `-[[NSMenuItem alloc] init] title]` really is the constant
            // string `@"NSMenuItem"`, which is AppKit's own default and was
            // measured with clang rather than guessed. winit installs exactly
            // one bare item, so that is the honest reading.
            // SAFETY: `-isKindOfClass:` is `B24@0:8#16` on any live object.
            let all_strings = unsafe {
                let tops = get(main, sel!(itemArray));
                let mut ok = n > 0;
                for i in 0..n {
                    let top = get_at(tops, sel!(objectAtIndex:), i);
                    if top.is_null() {
                        continue;
                    }
                    let t = get(top, sel!(title));
                    let f: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Bool = msg();
                    ok &= !t.is_null()
                        && f(t, sel!(isKindOfClass:), class(c"NSString").as_id()).as_bool();
                }
                ok
            };
            self.report.check(
                all_strings,
                "every title the walk read is an NSString — the premise of the \
                 `{title:?}` the port formats",
            );
            self.report.note(&format!("menu titles: {titles:?}"));
        }

        /// `native_chrome_overlay_of`'s capture chain, end to end — the three
        /// send shapes W13 ADDED to `aterm-objc` and nothing else exercises.
        fn stage_chrome_capture(&mut self, _view: Id, window: Id) {
            println!("\n[11] the chrome capture — NSBitmapImageRep to PNG bytes");
            // SAFETY: `-[NSWindow contentView]` is `@16@0:8`; the capture reads
            // the titlebar container, but any live NSView proves the chain.
            let target = unsafe {
                let close = {
                    let f: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Id = msg();
                    f(window, sel!(standardWindowButton:), 0)
                };
                if close.is_null() {
                    get(window, sel!(contentView))
                } else {
                    close
                }
            };
            if target.is_null() {
                self.report.check(
                    false,
                    "the window has neither a close button nor a content view",
                );
                return;
            }
            // SAFETY: `-bounds` is a 32-byte struct return.
            let bounds = unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel) -> aterm_objc::CGRect = msg();
                f(target, sel!(bounds))
            };
            self.report.check(
                bounds.size.width > 0.0 && bounds.size.height > 0.0,
                &format!(
                    "the view's bounds are {}x{}",
                    bounds.size.width, bounds.size.height
                ),
            );
            // SAFETY: `-bitmapImageRepForCachingDisplayInRect:` is
            // `@48@0:8{CGRect=…}16` and +0; `-cacheDisplayInRect:
            // toBitmapImageRep:` is `v56@0:8{CGRect=…}16@48`.
            let bitmap = unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel, aterm_objc::CGRect) -> Id = msg();
                let rep = f(target, sel!(bitmapImageRepForCachingDisplayInRect:), bounds);
                if !rep.is_null() {
                    let g: unsafe extern "C-unwind" fn(Id, Sel, aterm_objc::CGRect, Id) = msg();
                    g(
                        target,
                        sel!(cacheDisplayInRect:toBitmapImageRep:),
                        bounds,
                        rep,
                    );
                }
                rep
            };
            if bitmap.is_null() {
                self.report.check(
                    false,
                    "-bitmapImageRepForCachingDisplayInRect: answered nil",
                );
                return;
            }
            // SAFETY: `+[NSColorSpace sRGBColorSpace]` is `@16@0:8`;
            // `-bitmapImageRepByConvertingToColorSpace:renderingIntent:` is
            // `@32@0:8@16q24` — the intent is a SIGNED NSInteger, which is the
            // shape `send_id_id_isize` was added for.
            let converted = unsafe {
                let srgb = get(class(c"NSColorSpace").as_id(), sel!(sRGBColorSpace));
                if srgb.is_null() {
                    Id::NIL
                } else {
                    let f: unsafe extern "C-unwind" fn(Id, Sel, Id, isize) -> Id = msg();
                    f(
                        bitmap,
                        sel!(bitmapImageRepByConvertingToColorSpace:renderingIntent:),
                        srgb,
                        NS_COLOR_RENDERING_INTENT_PERCEPTUAL,
                    )
                }
            };
            self.report.check(
                !converted.is_null(),
                "the rep converted to sRGB through the SIGNED rendering intent",
            );
            if converted.is_null() {
                return;
            }
            // SAFETY: `+[NSDictionary new]` is `@16@0:8` and +1;
            // `-representationUsingType:properties:` is `@32@0:8Q16@24` — the
            // file type is an UNSIGNED NSUInteger, the near-twin of the row
            // above and the shape `send_id_usize_id` was added for.
            let png = unsafe {
                let props = Obj::from_owned(get(class(c"NSDictionary").as_id(), sel!(new)));
                let Some(props) = props else {
                    return;
                };
                let f: unsafe extern "C-unwind" fn(Id, Sel, usize, Id) -> Id = msg();
                f(
                    converted,
                    sel!(representationUsingType:properties:),
                    NS_BITMAP_IMAGE_FILE_TYPE_PNG,
                    props.id(),
                )
            };
            if png.is_null() {
                self.report
                    .check(false, "-representationUsingType:properties: answered nil");
                return;
            }
            // SAFETY: `-[NSData length]` is `Q16@0:8`; `-getBytes:length:` is
            // `v32@0:8^v16Q24` and WRITES through the pointer — the shape
            // `send_v_ptr_usize` was added for.
            let bytes = unsafe {
                let len = get_usize(png, sel!(length));
                let mut buf = vec![0_u8; len];
                if len != 0 {
                    let f: unsafe extern "C-unwind" fn(Id, Sel, *mut c_void, usize) = msg();
                    f(png, sel!(getBytes:length:), buf.as_mut_ptr().cast(), len);
                }
                buf
            };
            self.report.check(
                bytes.len() > 8,
                &format!("the PNG representation is {} byte(s)", bytes.len()),
            );
            self.report.check(
                bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]),
                &format!(
                    "…and it really is a PNG (first 8 bytes {:02x?}) — which is \
                     what proves `getBytes:length:` wrote through the pointer \
                     rather than leaving the Vec's zeroes",
                    &bytes[..8.min(bytes.len())]
                ),
            );
            self.report.check(
                bytes.iter().any(|b| *b != 0),
                "the buffer is not all zeroes",
            );
        }
    }

    /// Drive the loop until every stage has run, then report.
    pub fn run() -> i32 {
        let mut el = match EventLoop::new() {
            Ok(el) => el,
            Err(e) => {
                eprintln!("objc-alert-drive: NOT RUN — no event loop: {e}");
                return NOT_RUN;
            }
        };
        let mut driver = Driver {
            window: None,
            report: Report::default(),
            stage: 0,
            done: false,
            alert: None,
            accept: None,
            cancel: None,
            panel: None,
            answered: Rc::new(RefCell::new(None)),
            completion_dropped: false,
            monitor: None,
            monitor_saw: Rc::new(RefCell::new(Vec::new())),
        };
        let started = Instant::now();
        loop {
            if let PumpStatus::Exit(_) =
                el.pump_app_events(Some(Duration::from_millis(16)), &mut driver)
            {
                break;
            }
            if driver.done {
                break;
            }
            if started.elapsed() > BUDGET {
                eprintln!(
                    "objc-alert-drive: NOT RUN — the stages did not finish within {BUDGET:?}"
                );
                return NOT_RUN;
            }
        }

        drop(driver.window.take());

        if let Some(why) = driver.report.blocked {
            eprintln!("objc-alert-drive: NOT RUN — {why}");
            return NOT_RUN;
        }
        println!("\n=== VERDICT ===");
        println!("  {} stage(s) ran", driver.report.ran);
        if driver.report.findings.is_empty() {
            println!("objc-alert-drive: OK — every stage that ran agreed.");
            PASS
        } else {
            for f in &driver.report.findings {
                println!("  FAIL: {f}");
            }
            println!(
                "objc-alert-drive: {} FINDING(S)",
                driver.report.findings.len()
            );
            FAIL
        }
    }
}
