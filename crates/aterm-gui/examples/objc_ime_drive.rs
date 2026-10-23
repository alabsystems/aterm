// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! End-to-end `NSTextInputClient` IME composition driver for `WinitView`.

/// Every stage agreed.
#[cfg(target_os = "macos")]
const PASS: i32 = 0;
/// At least one finding. See the transcript.
#[cfg(target_os = "macos")]
const FAIL: i32 = 1;
/// The drive could not execute here. Not a pass.
const NOT_RUN: i32 = 2;
/// Every stage agreed, but stage 8's control did not compose, so "no dead key"
#[cfg(target_os = "macos")]
const PASS_UNCONTROLLED: i32 = 3;

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "objc-ime-drive: NOT RUN — this drives \
         crates/aterm-window/src/platform_impl/macos/view.rs, which does not exist off macOS."
    );
    std::process::ExitCode::from(NOT_RUN as u8)
}

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(macos::run() as u8)
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_void;
    use std::time::{Duration, Instant};

    use aterm_objc::{Bool, CGRect, Id, NSRange, Obj, Sel, checked_msg, class, ns_string, sel};
    use aterm_window::application::ApplicationHandler;
    use aterm_window::event::{Ime, WindowEvent};
    use aterm_window::event_loop::ActiveEventLoop;
    use aterm_window::keyboard::{KeyCode, PhysicalKey};
    use aterm_window::platform::macos::{OptionAsAlt, WindowExtMacOS as _};
    use aterm_window::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
    use aterm_window::window::{Window, WindowId};

    use super::{FAIL, NOT_RUN, PASS, PASS_UNCONTROLLED};

    /// Long enough for macOS to launch its `NSApplication` and hand back a
    const BUDGET: Duration = Duration::from_secs(30);

    /// How long stage 7 waits for macOS to make the drive the active app before
    const FOCUS_WAIT: Duration = Duration::from_secs(10);

    /// `NSNotFound`, which is `NSIntegerMax`. `replacementRange:` is documented
    const NS_NOT_FOUND: usize = isize::MAX as usize;

    /// The whole `replacementRange:` an IME sends while composing.
    const NO_REPLACEMENT: NSRange = NSRange {
        location: NS_NOT_FOUND,
        length: 0,
    };

    /// Send `-setMarkedText:selectedRange:replacementRange:`.
    ///
    /// # Safety
    /// `view` must be a live `WinitView` and `string` a live `NSString` or `NSAttributedString`.
    unsafe fn set_marked_text(view: Id, string: Id, selected: NSRange, replacement: NSRange) {
        // SAFETY: `-setMarkedText:selectedRange:replacementRange:` is registered `v@:@{_NSRange=QQ}{_NSRange=QQ}`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, Id, NSRange, NSRange) = checked_msg();
            f(
                view,
                sel!(setMarkedText:selectedRange:replacementRange:),
                string,
                selected,
                replacement,
            );
        }
    }

    /// Send `-insertText:replacementRange:`.
    ///
    /// # Safety
    /// Same preconditions as [`set_marked_text`].
    unsafe fn insert_text(view: Id, string: Id, replacement: NSRange) {
        // SAFETY: `-insertText:replacementRange:` is registered `v@:@{_NSRange=QQ}`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, Id, NSRange) = checked_msg();
            f(
                view,
                sel!(insertText:replacementRange:),
                string,
                replacement,
            );
        }
    }

    /// Send `-hasMarkedText`.
    ///
    /// # Safety
    /// `view` must be a live `WinitView`.
    unsafe fn has_marked_text(view: Id) -> bool {
        // SAFETY: `-hasMarkedText` is registered `B@:`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> Bool = checked_msg();
            f(view, sel!(hasMarkedText)).as_bool()
        }
    }

    /// Send `-markedRange`.
    ///
    /// # Safety
    /// `view` must be a live `WinitView`.
    unsafe fn marked_range(view: Id) -> NSRange {
        // SAFETY: `-markedRange` is registered `{_NSRange=QQ}@:`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> NSRange = checked_msg();
            f(view, sel!(markedRange))
        }
    }

    /// Send a zero-argument `v@:` selector (`unmarkText`).
    ///
    /// # Safety
    /// `view` must be a live `WinitView` and `s` one of its `v@:` rows.
    unsafe fn send_void(view: Id, s: Sel) {
        // SAFETY: Caller guarantees `s` is a `v@:` row on a live receiver.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) = checked_msg();
            f(view, s);
        }
    }

    /// Send `-doCommandBySelector:`.
    ///
    /// # Safety
    /// `view` must be a live `WinitView`.
    unsafe fn do_command(view: Id, command: Sel) {
        // SAFETY: `-doCommandBySelector:` is registered `v@::` with a `SEL` argument.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, Sel) = checked_msg();
            f(view, sel!(doCommandBySelector:), command);
        }
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreateKeyboardEvent(
            source: *const c_void,
            keycode: u16,
            keydown: bool,
        ) -> *mut c_void;
        fn CGEventSetFlags(event: *mut c_void, flags: u64);
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: *const c_void);
    }

    /// One Option chord stages 7 and 8 press: its ANSI `keyCode`, the key winit
    #[derive(Clone, Copy, Debug)]
    /// Option key chord tested by stages 7 and 8.
    pub(super) struct Chord {
        code: u16,
        key: KeyCode,
        dead: bool,
    }

    /// U.S. layout Option dead keys plus Option+B control chord.
    pub(super) const CHORDS: [Chord; 6] = [
        Chord {
            code: 14,
            key: KeyCode::KeyE,
            dead: true,
        },
        Chord {
            code: 32,
            key: KeyCode::KeyU,
            dead: true,
        },
        Chord {
            code: 34,
            key: KeyCode::KeyI,
            dead: true,
        },
        Chord {
            code: 45,
            key: KeyCode::KeyN,
            dead: true,
        },
        Chord {
            code: 50,
            key: KeyCode::Backquote,
            dead: true,
        },
        Chord {
            code: 11,
            key: KeyCode::KeyB,
            dead: false,
        },
    ];

    /// An Option+`code` key-down `NSEvent` built as a key press is: a CoreGraphics
    fn option_key_down(code: u16) -> Option<Obj> {
        const CG_FLAG_ALTERNATE: u64 = 1 << 19;
        // SAFETY: CoreGraphics/NSEvent calls follow their ownership contracts; `cg` is released after `+eventWithCGEvent:`.
        unsafe {
            let cg = CGEventCreateKeyboardEvent(std::ptr::null(), code, true);
            if cg.is_null() {
                return None;
            }
            CGEventSetFlags(cg, CG_FLAG_ALTERNATE);
            let f: unsafe extern "C-unwind" fn(Id, Sel, *mut c_void) -> Id = checked_msg();
            let event = f(class(c"NSEvent").as_id(), sel!(eventWithCGEvent:), cg);
            CFRelease(cg);
            Obj::retain(event)
        }
    }

    /// Read `-charactersIgnoringModifiers` from a key `NSEvent`.
    ///
    /// # Safety
    /// `event` must be a live key `NSEvent`.
    unsafe fn chars_ignoring_modifiers(event: Id) -> Option<String> {
        // SAFETY: `-charactersIgnoringModifiers` is `@16@0:8` on a live key `NSEvent`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let s = f(event, sel!(charactersIgnoringModifiers));
            (!s.is_null()).then(|| aterm_objc::ns_string_to_rust(s))
        }
    }

    /// `+[NSApplication sharedApplication]`.
    fn ns_app() -> Id {
        // SAFETY: `+sharedApplication` is `@16#0:8` on `NSApplication`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            f(class(c"NSApplication").as_id(), sel!(sharedApplication))
        }
    }

    /// Whether the input method sees `view`'s keys: the app is active, the
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    struct Focus {
        active: bool,
        key: bool,
        current: bool,
    }

    impl Focus {
        fn sees_keys(self) -> bool {
            self.active && self.key && self.current
        }

        fn show(self) -> String {
            format!(
                "app active = {}, window key = {}, the view's input context is the current one = {}",
                self.active, self.key, self.current
            )
        }
    }

    /// Snapshot `view`'s [`Focus`].
    ///
    /// # Safety
    /// `view` must be a live `WinitView`.
    unsafe fn focus_of(view: Id) -> Focus {
        // SAFETY: Focus and input-context getters match their AppKit signatures on a live `view`.
        unsafe {
            let b: unsafe extern "C-unwind" fn(Id, Sel) -> Bool = checked_msg();
            let o: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let window = o(view, sel!(window));
            let ctx = o(view, sel!(inputContext));
            let current = o(
                class(c"NSTextInputContext").as_id(),
                sel!(currentInputContext),
            );
            Focus {
                active: b(ns_app(), sel!(isActive)).as_bool(),
                key: !window.is_null() && b(window, sel!(isKeyWindow)).as_bool(),
                current: !ctx.is_null() && current == ctx,
            }
        }
    }

    /// Send `-firstRectForCharacterRange:actualRange:`.
    ///
    /// # Safety
    /// `view` must be a live `WinitView` and `actual` must be null or valid for one `NSRange` write.
    unsafe fn first_rect(view: Id, range: NSRange, actual: *mut NSRange) -> CGRect {
        // SAFETY: `-firstRectForCharacterRange:actualRange:` matches its audited `CGRect`-returning signature.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, NSRange, *mut NSRange) -> CGRect =
                checked_msg();
            f(
                view,
                sel!(firstRectForCharacterRange:actualRange:),
                range,
                actual,
            )
        }
    }

    /// A +1 `NSAttributedString` over `s`, for the other half of
    fn attributed(s: &str) -> Option<Obj> {
        let inner = ns_string(s)?;
        // SAFETY: `+alloc` and `-initWithString:` follow +1 ownership on a live `NSString`.
        unsafe {
            let alloc: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let raw = alloc(class(c"NSAttributedString").as_id(), sel!(alloc));
            let init: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
            Obj::from_owned(init(raw, sel!(initWithString:), inner.id()))
        }
    }

    /// The transcript and the verdict.
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

        /// Assert one thing, printing it either way, so a green run is still a
        fn expect(&mut self, what: &str, ok: bool, detail: String) {
            if ok {
                println!("    ok   {what}: {detail}");
            } else {
                self.fail(format!("{what}: {detail}"));
            }
        }
    }

    /// Expectations a stage writes down to be checked one pump later.
    type Deferred = Box<dyn FnOnce(&mut Driver)>;

    struct Driver {
        window: Option<Window>,
        report: Report,
        /// Every `Ime` event winit has delivered since the last drain.
        ime: Vec<Ime>,
        /// The non-IME window events stage 9 needs, as short names.
        mouse: Vec<&'static str>,
        /// Every press of a key stages 7 and 8 send ([`CHORDS`]) that winit has
        keys: Vec<(KeyCode, Option<String>)>,
        /// Every other key press since then. The drive sends none, so these are a
        foreign: Vec<String>,
        /// When stage 7 asked macOS to make the drive the active app.
        focus_asked: Option<Instant>,
        /// The chords whose stage 7 press started a composition: `None` until it ran.
        control_composed: Option<Vec<KeyCode>>,
        /// Stage 8 ran without a control that composed (exit 3).
        uncontrolled: bool,
        done: bool,
        stage: usize,
        /// Expectations a stage queued for AFTER the next pump, because the
        pending: Option<Deferred>,
    }

    impl Driver {
        /// The `WinitView` AppKit is holding, or `None` if there is no window.
        fn view(&self) -> Option<Id> {
            let view = aterm_objc::HasNsView::ns_view(self.window.as_ref()?);
            (!view.is_null()).then_some(view)
        }

        /// Everything winit has queued, in order, cleared.
        fn drain(&mut self) -> Vec<Ime> {
            std::mem::take(&mut self.ime)
        }

        /// Prints, and clears, the key presses the drive did not send.
        fn show_foreign(&mut self) {
            let foreign = std::mem::take(&mut self.foreign);
            if !foreign.is_empty() {
                println!(
                    "    NOTE: key presses this drive did not send arrived while it held the \
                     keyboard (a person typing), left out of the checks: {foreign:?}"
                );
            }
        }
    }

    /// Pretty-print an `Ime` for the transcript without depending on its
    fn show(e: &Ime) -> String {
        match e {
            Ime::Enabled => "Enabled".to_owned(),
            Ime::Disabled => "Disabled".to_owned(),
            Ime::Preedit(s, r) => format!("Preedit({s:?}, {r:?})"),
            Ime::Commit(s) => format!("Commit({s:?})"),
        }
    }

    fn show_all(v: &[Ime]) -> String {
        v.iter().map(show).collect::<Vec<_>>().join(" -> ")
    }

    impl ApplicationHandler for Driver {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let attrs = Window::default_attributes()
                .with_title("objc-ime-drive")
                .with_inner_size(aterm_window::dpi::LogicalSize::new(480.0, 320.0))
                .with_visible(false);
            match el.create_window(attrs) {
                Ok(w) => {
                    w.set_ime_allowed(true);
                    self.window = Some(w);
                }
                Err(e) => {
                    self.report.blocked = Some(format!("no window could be created: {e}"));
                    self.done = true;
                    el.exit();
                }
            }
        }

        fn window_event(&mut self, _el: &ActiveEventLoop, _id: WindowId, e: WindowEvent) {
            match e {
                WindowEvent::Ime(ime) => self.ime.push(ime),
                WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                    let text = event.text.as_ref().map(|t| t.to_string());
                    let sent = CHORDS
                        .iter()
                        .find(|c| event.physical_key == PhysicalKey::Code(c.key));
                    if let Some(c) = sent {
                        self.keys.push((c.key, text));
                    } else {
                        self.foreign
                            .push(format!("{:?} {text:?}", event.physical_key));
                    }
                }
                WindowEvent::CursorMoved { .. } => self.mouse.push("CursorMoved"),
                WindowEvent::MouseInput { .. } => self.mouse.push("MouseInput"),
                _ => {}
            }
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
            if let Some(check) = self.pending.take() {
                check(self);
                return;
            }
            let stage = self.stage;
            self.stage += 1;
            match stage {
                0 => {}
                1 => self.stage_ascii(view),
                2 => self.stage_cjk(view),
                3 => self.stage_attributed(view),
                4 => self.stage_clamp(view),
                5 => self.stage_leave_preedit(view),
                6 => self.stage_candidate_rect(view),
                7 => {
                    if !self.focused_for_control(view) {
                        self.stage = 7;
                        return;
                    }
                    self.stage_dead_key(view);
                }
                8 => self.stage_option_as_alt(view),
                9 => self.stage_mouse_paths(view),
                _ => {
                    self.done = true;
                    el.exit();
                }
            }
        }
    }

    impl Driver {
        /// Stage 1: a whole composition in ASCII.
        fn stage_ascii(&mut self, view: Id) {
            println!("\n=== 1. A COMPOSITION, ASCII ===");
            let _ = self.drain();
            let Some(s) = ns_string("ab") else {
                self.report.fail("could not build an NSString".to_owned());
                return;
            };
            // SAFETY: `view` is a live `WinitView` and `s` is a live `NSString`.
            unsafe {
                set_marked_text(
                    view,
                    s.id(),
                    NSRange {
                        location: 2,
                        length: 0,
                    },
                    NO_REPLACEMENT,
                );
            }
            // SAFETY: `view` is a live `WinitView`.
            let (marked, range) = unsafe { (has_marked_text(view), marked_range(view)) };
            self.report.expect(
                "hasMarkedText after setMarkedText:",
                marked,
                format!("{marked}"),
            );
            self.report.expect(
                "markedRange covers the composition",
                range.location == 0 && range.length == 2,
                format!("{{{}, {}}}", range.location, range.length),
            );

            let Some(c) = ns_string("AB") else {
                self.report.fail("could not build an NSString".to_owned());
                return;
            };
            // SAFETY: `view` is a live `WinitView` and `c` is a live `NSString`.
            unsafe { insert_text(view, c.id(), NO_REPLACEMENT) };
            self.pending = Some(Box::new(|d: &mut Driver| {
                let got = d.drain();
                println!("    events: {}", show_all(&got));
                let want_enabled = matches!(got.first(), Some(Ime::Enabled));
                d.report.expect(
                    "the first composition enables IME",
                    want_enabled,
                    format!("{:?}", got.first().map(show)),
                );
                let preedit = got.iter().any(|e| {
                    matches!(e, Ime::Preedit(s, Some((a, b))) if s == "ab" && *a == 2 && *b == 2)
                });
                d.report.expect(
                    "preedit carries the text and a UTF-8 cursor range",
                    preedit,
                    "Preedit(\"ab\", Some((2, 2)))".to_owned(),
                );
                let commit = got.iter().any(|e| matches!(e, Ime::Commit(s) if s == "AB"));
                d.report
                    .expect("insertText: commits", commit, "Commit(\"AB\")".to_owned());
            }));
        }

        /// Stage 2: the same, in CJK. `selectedRange:` counts UTF-16 code units and
        fn stage_cjk(&mut self, view: Id) {
            println!("\n=== 2. A COMPOSITION, CJK (the UTF-16 -> UTF-8 conversion) ===");
            let _ = self.drain();
            let Some(s) = ns_string("にほ") else {
                self.report.fail("could not build an NSString".to_owned());
                return;
            };
            // SAFETY: `view` is a live `WinitView` and `s` is a live `NSString`.
            unsafe {
                set_marked_text(
                    view,
                    s.id(),
                    NSRange {
                        location: 2,
                        length: 0,
                    },
                    NO_REPLACEMENT,
                );
            }
            // SAFETY: `view` is a live `WinitView`.
            let range = unsafe { marked_range(view) };
            self.report.expect(
                "markedRange counts UTF-16 units, as NSTextInputClient does",
                range.location == 0 && range.length == 2,
                format!("{{{}, {}}}", range.location, range.length),
            );
            self.pending = Some(Box::new(|d: &mut Driver| {
                let got = d.drain();
                println!("    events: {}", show_all(&got));
                let ok = got.iter().any(|e| {
                    matches!(e, Ime::Preedit(s, Some((a, b))) if s == "にほ" && *a == 6 && *b == 6)
                });
                d.report.expect(
                    "the cursor range is converted to UTF-8 byte offsets",
                    ok,
                    "Preedit(\"にほ\", Some((6, 6))) — 2 UTF-16 units, 6 UTF-8 bytes".to_owned(),
                );
            }));
        }

        /// Stage 3: the `NSAttributedString` branch. `setMarkedText:`'s first
        fn stage_attributed(&mut self, view: Id) {
            println!("\n=== 3. setMarkedText: WITH AN NSAttributedString ===");
            let _ = self.drain();
            let Some(a) = attributed("한글") else {
                self.report
                    .fail("could not build an NSAttributedString".to_owned());
                return;
            };
            // SAFETY: `view` is a live `WinitView` and `a` is a live `NSAttributedString`.
            unsafe {
                set_marked_text(
                    view,
                    a.id(),
                    NSRange {
                        location: 2,
                        length: 0,
                    },
                    NO_REPLACEMENT,
                );
            }
            // SAFETY: `view` is a live `WinitView`.
            let (marked, range) = unsafe { (has_marked_text(view), marked_range(view)) };
            self.report.expect(
                "an attributed string marks text too",
                marked && range.length == 2,
                format!(
                    "hasMarkedText={marked}, markedRange.length={}",
                    range.length
                ),
            );
            self.pending = Some(Box::new(|d: &mut Driver| {
                let got = d.drain();
                println!("    events: {}", show_all(&got));
                let ok = got.iter().any(|e| {
                    matches!(e, Ime::Preedit(s, Some((a, b))) if s == "한글" && *a == 6 && *b == 6)
                });
                d.report.expect(
                    "the attributed branch reports the same preedit as the plain one",
                    ok,
                    "Preedit(\"한글\", Some((6, 6)))".to_owned(),
                );
            }));
        }

        /// Stage 4: the clamp, both halves.
        fn stage_clamp(&mut self, view: Id) {
            println!("\n=== 4. OUT-OF-BOUNDS AND OVERFLOWING SELECTED RANGES ===");
            let _ = self.drain();
            let Some(s) = ns_string("hi") else {
                self.report.fail("could not build an NSString".to_owned());
                return;
            };
            // SAFETY: `view` is a live `WinitView` and `s` is a live `NSString`.
            unsafe {
                set_marked_text(
                    view,
                    s.id(),
                    NSRange {
                        location: 9,
                        length: 0,
                    },
                    NO_REPLACEMENT,
                );
            }
            println!("    survived a selectedRange past the end of the string");
            // SAFETY: `view` is a live `WinitView` and `s` is a live `NSString`.
            unsafe {
                set_marked_text(
                    view,
                    s.id(),
                    NSRange {
                        location: usize::MAX,
                        length: 5,
                    },
                    NO_REPLACEMENT,
                );
            }
            println!("    survived a selectedRange whose location + length overflows usize");
            self.pending = Some(Box::new(|d: &mut Driver| {
                let got = d.drain();
                println!("    events: {}", show_all(&got));
                let clamped = got.iter().filter(|e| {
                    matches!(e, Ime::Preedit(s, Some((a, b))) if s == "hi" && *a == 2 && *b == 2)
                });
                d.report.expect(
                    "both out-of-range selections clamp to the string's length",
                    clamped.count() == 2,
                    "two Preedit(\"hi\", Some((2, 2))) — 9 and usize::MAX both clamp to 2"
                        .to_owned(),
                );
            }));
        }

        /// Stage 5: leaving preedit without committing.
        fn stage_leave_preedit(&mut self, view: Id) {
            println!("\n=== 5. doCommandBySelector: AND unmarkText ===");
            let _ = self.drain();
            let Some(s) = ns_string("xy") else {
                self.report.fail("could not build an NSString".to_owned());
                return;
            };
            // SAFETY: `view` is a live `WinitView` and `s` is a live `NSString`.
            unsafe {
                set_marked_text(
                    view,
                    s.id(),
                    NSRange {
                        location: 2,
                        length: 0,
                    },
                    NO_REPLACEMENT,
                );
            }
            // SAFETY: `view` is a live `WinitView` and `insertLineBreak:` is a valid selector.
            unsafe { do_command(view, sel!(insertLineBreak:)) };
            // SAFETY: `view` is a live `WinitView`.
            let after_command = unsafe { has_marked_text(view) };
            self.report.expect(
                "doCommandBySelector: leaves the marked text in place",
                after_command,
                format!("hasMarkedText={after_command}"),
            );

            // SAFETY: `-inputContext` is `@@:` on a live `NSResponder`.
            let ctx: Id = unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
                f(view, sel!(inputContext))
            };
            if ctx.is_null() {
                println!("    SKIPPED unmarkText: the view has no NSTextInputContext here");
            } else {
                // SAFETY: `view` is a live `WinitView` with a non-nil input context.
                unsafe { send_void(view, sel!(unmarkText)) };
                // SAFETY: `view` is a live `WinitView`.
                let after_unmark = unsafe { has_marked_text(view) };
                self.report.expect(
                    "unmarkText clears the composition",
                    !after_unmark,
                    format!("hasMarkedText={after_unmark}"),
                );
                // SAFETY: `view` is a live `WinitView`.
                let r = unsafe { marked_range(view) };
                self.report.expect(
                    "markedRange goes back to {NSNotFound, 0}",
                    r.location == NS_NOT_FOUND && r.length == 0,
                    format!("{{{:#x}, {}}}", r.location, r.length),
                );
            }
            self.pending = Some(Box::new(|d: &mut Driver| {
                let got = d.drain();
                println!("    events: {}", show_all(&got));
                let no_commit = !got.iter().any(|e| matches!(e, Ime::Commit(_)));
                d.report.expect(
                    "leaving preedit this way commits nothing",
                    no_commit,
                    "no Commit in the sequence".to_owned(),
                );
            }));
        }

        /// Stage 6: the candidate-window rectangle, the row whose encoding is
        fn stage_candidate_rect(&mut self, view: Id) {
            println!("\n=== 6. THE CANDIDATE-WINDOW RECTANGLE ===");
            let Some(w) = self.window.as_ref() else {
                return;
            };
            let scale = w.scale_factor();
            w.set_ime_cursor_area(
                aterm_window::dpi::LogicalPosition::new(40.0, 60.0),
                aterm_window::dpi::LogicalSize::new(10.0, 20.0),
            );
            // SAFETY: `view` is a live `WinitView` and `actualRange:` may be null.
            let rect = unsafe {
                first_rect(
                    view,
                    NSRange {
                        location: 0,
                        length: 1,
                    },
                    std::ptr::null_mut(),
                )
            };
            println!(
                "    firstRectForCharacterRange: = origin ({}, {}) size {}x{} (scale {scale})",
                rect.origin.x, rect.origin.y, rect.size.width, rect.size.height
            );
            self.report.expect(
                "the candidate rect carries the size set through set_ime_cursor_area",
                (rect.size.width - 10.0).abs() < 0.5 && (rect.size.height - 20.0).abs() < 0.5,
                format!("{}x{} against 10x20", rect.size.width, rect.size.height),
            );
            self.report.expect(
                "the candidate rect is finite and on a screen, not at a garbage address",
                rect.origin.x.is_finite()
                    && rect.origin.y.is_finite()
                    && rect.origin.x.abs() < 1.0e6
                    && rect.origin.y.abs() < 1.0e6,
                format!("({}, {})", rect.origin.x, rect.origin.y),
            );

            let mut actual = NSRange {
                location: 0xDEAD,
                length: 0xBEEF,
            };
            // SAFETY: `view` is a live `WinitView` and `actual` is writable for the call.
            let again = unsafe {
                first_rect(
                    view,
                    NSRange {
                        location: 0,
                        length: 1,
                    },
                    std::ptr::from_mut(&mut actual),
                )
            };
            self.report.expect(
                "a non-null actualRange: does not disturb the return",
                again == rect,
                format!("{:?} {:?}", again.origin, again.size),
            );
            self.report.expect(
                "the actualRange: out-parameter arrives as a real pointer and is left alone",
                actual.location == 0xDEAD && actual.length == 0xBEEF,
                format!("{{{:#x}, {:#x}}}", actual.location, actual.length),
            );
        }

        /// Stage 7's precondition: the input method sees only the keys of the
        fn focused_for_control(&mut self, view: Id) -> bool {
            let asked = *self.focus_asked.get_or_insert_with(|| {
                if let Some(window) = self.window.as_ref() {
                    window.set_visible(true);
                    window.focus_window();
                }
                Instant::now()
            });
            // SAFETY: `view` is a live `WinitView`.
            unsafe { focus_of(view) }.sees_keys() || asked.elapsed() > FOCUS_WAIT
        }

        /// Stage 7: real `NSEvent`s through `keyDown:`, the only path into
        fn stage_dead_key(&mut self, view: Id) {
            println!("\n=== 7. REAL KEY EVENTS (the dead-key path) ===");
            let _ = self.drain();
            self.keys.clear();
            self.foreign.clear();
            // SAFETY: `-inputContext` is `@@:` on a live `NSResponder`.
            let ctx: Id = unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
                f(view, sel!(inputContext))
            };
            if ctx.is_null() {
                println!("    SKIPPED: no NSTextInputContext, so interpretKeyEvents: goes nowhere");
                return;
            }
            // SAFETY: `view` is a live `WinitView`.
            let focus = unsafe { focus_of(view) };
            println!("    {}", focus.show());
            let mut composed = Vec::new();
            for chord in CHORDS {
                let Some(event) = option_key_down(chord.code) else {
                    self.report.fail(format!(
                        "+[NSEvent eventWithCGEvent:] built no Option+{:?} key event",
                        chord.key
                    ));
                    return;
                };
                // SAFETY: `keyDown:` is `v@:@` and `event` is a live `NSEvent`.
                unsafe {
                    let f: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
                    f(view, sel!(keyDown:), event.id());
                }
                // SAFETY: `view` is a live `WinitView`.
                let marked = unsafe { has_marked_text(view) };
                println!(
                    "    Option+{:?} (keyCode {}, built from a CGEvent): hasMarkedText = {marked}",
                    chord.key, chord.code
                );
                if marked {
                    composed.push(chord.key);
                    // SAFETY: `view` is a live `WinitView` with an active input context.
                    unsafe { send_void(view, sel!(unmarkText)) };
                }
            }
            println!(
                "    keyDown: returned {} times without aborting — the trampoline and its panic \
                 guard both held",
                CHORDS.len()
            );
            self.control_composed = Some(composed);
            self.pending = Some(Box::new(|d: &mut Driver| {
                let got = d.drain();
                let keys = std::mem::take(&mut d.keys);
                println!("    events: {}; key texts: {keys:?}", show_all(&got));
                d.show_foreign();
                let disabled_then_preedit = got
                    .windows(2)
                    .any(|w| matches!((&w[0], &w[1]), (Ime::Disabled, Ime::Preedit(..))));
                d.report.expect(
                    "no preedit arrives after IME reports itself disabled",
                    !disabled_then_preedit,
                    "no Disabled -> Preedit pair".to_owned(),
                );
                println!(
                    "    NOTE: whether a dead key composes here depends on the machine's \
                     keyboard layout, and on macOS making this drive the active app; the \
                     encoding-level claim this stage makes is that a real NSEvent reaches \
                     keyDown: through the registered v@:@ trampoline and interpretKeyEvents: \
                     runs."
                );
            }));
        }

        /// Stage 8: Option as Alt, which aterm sets in Meta mode (config
        fn stage_option_as_alt(&mut self, view: Id) {
            println!("\n=== 8. OPTION AS ALT: no Option chord is a dead key ===");
            let Some(window) = self.window.as_ref() else {
                self.report.fail("the window is gone".to_owned());
                return;
            };
            window.set_option_as_alt(OptionAsAlt::Both);
            self.report.expect(
                "the window keeps the Option-as-Alt setting",
                window.option_as_alt() == OptionAsAlt::Both,
                format!("{:?}", window.option_as_alt()),
            );
            let _ = self.drain();
            self.keys.clear();
            self.foreign.clear();
            // SAFETY: `view` is a live `WinitView`.
            let focus = unsafe { focus_of(view) };
            println!("    {}", focus.show());
            let mut bare = Vec::new();
            for chord in CHORDS {
                let Some(event) = option_key_down(chord.code) else {
                    self.report.fail(format!(
                        "+[NSEvent eventWithCGEvent:] built no Option+{:?} key event",
                        chord.key
                    ));
                    return;
                };
                // SAFETY: `event` is a live key `NSEvent`.
                let ignoring = unsafe { chars_ignoring_modifiers(event.id()) };
                // SAFETY: `keyDown:` is `v@:@` and `event` is a live `NSEvent`.
                unsafe {
                    let f: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
                    f(view, sel!(keyDown:), event.id());
                }
                // SAFETY: `view` is a live `WinitView`.
                let marked = unsafe { has_marked_text(view) };
                self.report.expect(
                    &format!(
                        "Option as Alt leaves no marked text after Option+{:?}",
                        chord.key
                    ),
                    !marked,
                    format!("hasMarkedText = {marked}"),
                );
                if marked {
                    // SAFETY: `view` is a live `WinitView` with an active input context.
                    unsafe { send_void(view, sel!(unmarkText)) };
                }
                bare.push((chord.key, ignoring));
            }
            let composed = self.control_composed.clone().unwrap_or_default();
            let dead: Vec<KeyCode> = CHORDS.iter().filter(|c| c.dead).map(|c| c.key).collect();
            let controlled = focus.sees_keys() && dead.iter().all(|k| composed.contains(k));
            self.pending = Some(Box::new(move |d: &mut Driver| {
                let got = d.drain();
                let keys = std::mem::take(&mut d.keys);
                println!("    events: {}; key texts: {keys:?}", show_all(&got));
                d.show_foreign();
                d.report.expect(
                    "no preedit arrives for an Option chord with Option as Alt",
                    !got.iter()
                        .any(|e| matches!(e, Ime::Preedit(s, _) if !s.is_empty())),
                    show_all(&got),
                );
                d.report.expect(
                    "each chord arrives as one key press whose text is the key's bare character",
                    bare.iter()
                        .all(|(_, b)| b.as_deref().is_some_and(|b| !b.is_empty()))
                        && keys == bare,
                    format!("{keys:?} against charactersIgnoringModifiers {bare:?}"),
                );
                if controlled {
                    println!(
                        "    CONTROLLED: stage 7's identical presses of {dead:?} each started a \
                         dead key, so the checks above judge what Option as Alt does to one"
                    );
                } else {
                    println!(
                        "    UNCONTROLLED: of the dead keys {dead:?}, stage 7 composed only \
                         {composed:?} (or the input method stopped seeing this view's keys), so \
                         this stage measured the keys' text, not that Option as Alt stops a \
                         dead key for each"
                    );
                    d.uncontrolled = true;
                }
                if let Some(window) = d.window.as_ref() {
                    window.set_option_as_alt(OptionAsAlt::None);
                }
            }));
        }

        /// Stage 9: the mouse paths through `update_modifiers`. Not an IME question,
        fn stage_mouse_paths(&mut self, view: Id) {
            println!("\n=== 9. THE MOUSE PATHS: a non-key event through update_modifiers ===");
            self.mouse.clear();
            let inside = aterm_objc::CGPoint { x: 100.0, y: 100.0 };
            let mut built = 0_usize;
            for (label, ty, imp) in [
                ("mouseMoved:", 5_usize, sel!(mouseMoved:)),
                ("mouseDragged:", 6, sel!(mouseDragged:)),
                ("mouseDown:", 1, sel!(mouseDown:)),
                ("mouseUp:", 2, sel!(mouseUp:)),
            ] {
                // SAFETY: `+mouseEventWithType:...` matches its AppKit class-method signature.
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
                        isize,
                        isize,
                        f32,
                    )
                        -> Id = checked_msg();
                    f(
                        class(c"NSEvent").as_id(),
                        sel!(
                            mouseEventWithType:location:modifierFlags:timestamp:windowNumber:context:eventNumber:clickCount:pressure:
                        ),
                        ty,
                        inside,
                        0,
                        0.0,
                        0,
                        Id::NIL,
                        0,
                        1,
                        1.0,
                    )
                };
                if event.is_null() {
                    self.report.fail(format!(
                        "+[NSEvent mouseEventWithType:…] answered nil for {label}"
                    ));
                    continue;
                }
                built += 1;
                // SAFETY: Each mouse selector is registered `v@:@` on `WinitView`.
                unsafe {
                    let f: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
                    f(view, imp, event);
                }
            }
            self.report.expect(
                "the four non-key mouse IMPs were entered and returned",
                built == 4,
                format!("{built} of 4, with no foreign exception raised"),
            );
            self.pending = Some(Box::new(|d: &mut Driver| {
                let saw = std::mem::take(&mut d.mouse);
                d.report.expect(
                    "CursorMoved arrived, so update_modifiers really ran with a mouse event",
                    saw.contains(&"CursorMoved"),
                    format!(
                        "{saw:?} — CursorMoved is on the FAR SIDE of `mouse_motion`'s \
                         outside-the-client-area early return"
                    ),
                );
                d.report.expect(
                    "MouseInput arrived, so `mouse_click` ran with one too",
                    saw.contains(&"MouseInput"),
                    format!("{saw:?}"),
                );
            }));
        }
    }

    /// Run all IME verification stages and return the process exit code.
    pub fn run() -> i32 {
        let mut el = match aterm_gui::quiet_driver_event_loop_builder::<()>().build() {
            Ok(el) => el,
            Err(e) => {
                eprintln!("objc-ime-drive: NOT RUN — no event loop: {e}");
                return NOT_RUN;
            }
        };
        let mut driver = Driver {
            window: None,
            report: Report::default(),
            ime: Vec::new(),
            mouse: Vec::new(),
            keys: Vec::new(),
            foreign: Vec::new(),
            focus_asked: None,
            control_composed: None,
            uncontrolled: false,
            done: false,
            stage: 0,
            pending: None,
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
                eprintln!("objc-ime-drive: NOT RUN — the stages did not finish within {BUDGET:?}");
                return NOT_RUN;
            }
        }
        drop(driver.window.take());

        if let Some(why) = driver.report.blocked {
            eprintln!("objc-ime-drive: NOT RUN — {why}");
            return NOT_RUN;
        }
        println!("\n=== VERDICT ===");
        if driver.report.findings.is_empty() && driver.uncontrolled {
            println!(
                "objc-ime-drive: OK, WEAKER — every stage that ran agreed, but stage 8's \
                 control did not compose for every dead key, so whether Option as Alt stops \
                 each dead key was not measured."
            );
            PASS_UNCONTROLLED
        } else if driver.report.findings.is_empty() {
            println!("objc-ime-drive: OK — every stage that ran agreed.");
            PASS
        } else {
            for f in &driver.report.findings {
                println!("  FAIL: {f}");
            }
            println!(
                "objc-ime-drive: {} FINDING(S)",
                driver.report.findings.len()
            );
            FAIL
        }
    }
}
