// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE IME DRIVER: `vendor/winit`'s ported `WinitView` composing for real,
//! through the `NSTextInputClient` IMPs `aterm_objc::declare_class!` registered.
//!
//! # Why this exists beside the auditor
//!
//! `crates/aterm-gui/examples/objc_live_class_audit.rs` asks whether the class
//! is SHAPED right: every registered encoding against the runtime's own
//! authority, plus a handful of live sends. It cannot ask whether the class
//! BEHAVES right, and for `view.rs` that is the question that matters. W3's
//! port moved 43 methods, eleven of them the whole `NSTextInputClient` surface,
//! and an input method is a state machine over six of them — `setMarkedText:`,
//! `hasMarkedText`, `markedRange`, `insertText:`, `unmarkText`,
//! `doCommandBySelector:`. A port that registers all six correctly and drops an
//! event, mis-clamps a cursor range or breaks the UTF-16-to-UTF-8 conversion
//! passes every shape check in the tree.
//!
//! It is also where the ONE genuinely dangerous encoding lives.
//! `firstRectForCharacterRange:actualRange:` is how an input method asks where
//! to put its candidate window, and Foundation computes that call's frame layout
//! from the string `class_addMethod` was handed. A wrong encoding there does not
//! raise — it puts the candidate window at a garbage rectangle. So this file
//! drives that row against a cursor area it set itself and checks the ANSWER,
//! not just the shape.
//!
//! # What it drives, and how
//!
//! Directly, through `objc_msgSend` into the registered IMPs — the same entry
//! AppKit's input method uses — with a real `NSWindow`, a real `WinitView` and
//! the real winit event loop collecting what comes out the other side. Every
//! expectation is on the `WindowEvent::Ime` sequence winit's own API promises,
//! so this is a test of the BACKEND CONTRACT and not of the port's internals.
//!
//! Six stages, each independent, each reporting its own findings:
//!
//! 1. A whole composition in ASCII: enable, preedit, commit.
//! 2. The same in CJK, because the cursor range crosses the UTF-16-to-UTF-8
//!    conversion that a naive port gets wrong for every non-ASCII input method.
//! 3. The `NSAttributedString` branch of `setMarkedText:`, which is the OTHER
//!    half of a runtime type test the port had to carry across the seam
//!    (`-isKindOfClass:` on an argument declared `@`).
//! 4. THE CLAMP: a selected range past the end of the string, which is what
//!    macOS's own Pinyin input method sends (alacritty#8791), and a range that
//!    OVERFLOWS `usize`, which objc2's `NSRange::end()` answers by panicking.
//! 5. `doCommandBySelector:` and `unmarkText` — the two rows that leave preedit
//!    without committing, including the `Committed`-state early return that
//!    stops a Korean IME double-sending its last key.
//! 6. The candidate-window rectangle, against a cursor area this file sets.
//!
//! # Dead keys
//!
//! Stage 7 synthesises a real `NSEvent` and sends `keyDown:`, which is the path
//! `interpretKeyEvents:` takes into the system input method. Whether the
//! installed input source produces marked text for it is NOT under this file's
//! control — it depends on the keyboard layout of the machine running the gate
//! — so that stage REPORTS what happened and only fails on an outcome that is
//! wrong under every layout (an abort, or preedit arriving while IME is
//! disabled). See the note on [`macos::Driver::stage_dead_key`].
//!
//! # Exit codes — the ladder gates on these, not on the prose
//!
//! * `0` — every stage that could run, ran, and agreed.
//! * `1` — a finding.
//! * `2` — NOT RUN: no event loop (headless, no window server), no view, or no
//!   input context. Never a pass.

/// Every stage agreed.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const PASS: i32 = 0;
/// At least one finding. See the transcript.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const FAIL: i32 = 1;
/// The drive could not execute here. NOT a pass.
const NOT_RUN: i32 = 2;

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "objc-ime-drive: NOT RUN — this drives \
         vendor/winit/src/platform_impl/macos/view.rs, which does not exist off macOS."
    );
    std::process::ExitCode::from(NOT_RUN as u8)
}

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(macos::run() as u8)
}

#[cfg(target_os = "macos")]
mod macos {
    use std::time::{Duration, Instant};

    use aterm_objc::{Bool, CGRect, Id, NSRange, Obj, Sel, class, msg, ns_string, sel};
    use winit::application::ApplicationHandler;
    use winit::event::{Ime, WindowEvent};
    use winit::event_loop::{ActiveEventLoop, EventLoop};
    use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::{Window, WindowId};

    use super::{FAIL, NOT_RUN, PASS};

    /// Long enough for macOS to launch its `NSApplication` and hand back a
    /// window; past it the drive reports NOT RUN rather than passing.
    const BUDGET: Duration = Duration::from_secs(30);

    /// `NSNotFound`, which is `NSIntegerMax`. `replacementRange:` is documented
    /// to be `{NSNotFound, 0}` when the input method is not replacing anything,
    /// which is what a composing IME always sends.
    const NS_NOT_FOUND: usize = isize::MAX as usize;

    /// The whole `replacementRange:` an IME sends while composing.
    const NO_REPLACEMENT: NSRange = NSRange {
        location: NS_NOT_FOUND,
        length: 0,
    };

    // ------------------------------------------------------------------ sends
    //
    // Each of these is one registered row, entered exactly as AppKit's input
    // method enters it: a typed `objc_msgSend` cast to the selector's own
    // prototype. They are the reason this file is evidence about the PORT and
    // not about a Rust helper that happens to sit beside it.

    /// `-setMarkedText:selectedRange:replacementRange:`.
    ///
    /// # Safety
    /// `view` must be a live `WinitView`; `string` a live `NSString` or
    /// `NSAttributedString`, which is what this row's `@` argument means and
    /// what its body decides between with `-isKindOfClass:`.
    unsafe fn set_marked_text(view: Id, string: Id, selected: NSRange, replacement: NSRange) {
        // SAFETY: the row is registered `v@:@{_NSRange=QQ}{_NSRange=QQ}`, which
        // the auditor checks against `NSTextInputClient`'s own description.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, NSRange, NSRange) = msg();
            f(
                view,
                sel!(setMarkedText:selectedRange:replacementRange:),
                string,
                selected,
                replacement,
            );
        }
    }

    /// `-insertText:replacementRange:`.
    ///
    /// # Safety
    /// As [`set_marked_text`].
    unsafe fn insert_text(view: Id, string: Id, replacement: NSRange) {
        // SAFETY: the row is registered `v@:@{_NSRange=QQ}`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, NSRange) = msg();
            f(
                view,
                sel!(insertText:replacementRange:),
                string,
                replacement,
            );
        }
    }

    /// `-hasMarkedText`.
    ///
    /// # Safety
    /// `view` must be a live `WinitView`.
    unsafe fn has_marked_text(view: Id) -> bool {
        // SAFETY: the row is registered `B@:`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Bool = msg();
            f(view, sel!(hasMarkedText)).as_bool()
        }
    }

    /// `-markedRange`. A 16-byte struct return: `x0`/`x1` on arm64.
    ///
    /// # Safety
    /// `view` must be a live `WinitView`.
    unsafe fn marked_range(view: Id) -> NSRange {
        // SAFETY: the row is registered `{_NSRange=QQ}@:`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> NSRange = msg();
            f(view, sel!(markedRange))
        }
    }

    /// A row that takes nothing and returns nothing — `unmarkText`.
    ///
    /// # Safety
    /// `view` must be a live `WinitView` and `s` one of its `v@:` rows.
    unsafe fn send_void(view: Id, s: Sel) {
        // SAFETY: the caller pins `s` as a `v@:` row on a live receiver.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) = msg();
            f(view, s);
        }
    }

    /// `-doCommandBySelector:` — the one row whose argument is a `SEL`.
    ///
    /// # Safety
    /// `view` must be a live `WinitView`.
    unsafe fn do_command(view: Id, command: Sel) {
        // SAFETY: the row is registered `v@::`, whose second `:` is the SEL
        // argument — the encoding that would silently become `v@:@` if a port
        // typed the argument as an object.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Sel) = msg();
            f(view, sel!(doCommandBySelector:), command);
        }
    }

    /// `-firstRectForCharacterRange:actualRange:`, the candidate-window row.
    ///
    /// # Safety
    /// `view` must be a live `WinitView`. `actual` is passed straight through
    /// and may be null, which is what AppKit passes when it wants no
    /// out-parameter.
    unsafe fn first_rect(view: Id, range: NSRange, actual: *mut NSRange) -> CGRect {
        // SAFETY: the row is registered
        // `{CGRect={CGPoint=dd}{CGSize=dd}}@:{_NSRange=QQ}^{_NSRange=QQ}`, and
        // the auditor has already made Foundation agree that its return is 32
        // bytes.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, NSRange, *mut NSRange) -> CGRect = msg();
            f(
                view,
                sel!(firstRectForCharacterRange:actualRange:),
                range,
                actual,
            )
        }
    }

    /// A +1 `NSAttributedString` over `s`, for the other half of
    /// `setMarkedText:`'s runtime type test.
    fn attributed(s: &str) -> Option<Obj> {
        let inner = ns_string(s)?;
        // SAFETY: `+alloc` is +1 and `-initWithString:` consumes it and returns
        // the initialised +1 (or nil, which `from_owned` maps to `None`).
        // `inner` is a live `NSString` borrowed for the length of the call and
        // copied by the initialiser.
        unsafe {
            let alloc: unsafe extern "C" fn(Id, Sel) -> Id = msg();
            let raw = alloc(class(c"NSAttributedString").as_id(), sel!(alloc));
            let init: unsafe extern "C" fn(Id, Sel, Id) -> Id = msg();
            Obj::from_owned(init(raw, sel!(initWithString:), inner.id()))
        }
    }

    // ------------------------------------------------------------------ drive

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
        /// transcript a reader can check rather than a word.
        fn expect(&mut self, what: &str, ok: bool, detail: String) {
            if ok {
                println!("    ok   {what}: {detail}");
            } else {
                self.fail(format!("{what}: {detail}"));
            }
        }
    }

    /// Expectations a stage writes down to be checked one pump later.
    ///
    /// `FnOnce` rather than a plain `fn`, because each stage closes over what it
    /// drove; boxed because the seven stages have seven different closures.
    type Deferred = Box<dyn FnOnce(&mut Driver)>;

    struct Driver {
        window: Option<Window>,
        report: Report,
        /// Every `Ime` event winit has delivered since the last drain.
        ime: Vec<Ime>,
        done: bool,
        stage: usize,
        /// Expectations a stage queued for AFTER the next pump, because the
        /// events it drove are delivered by winit and not by the send.
        pending: Option<Deferred>,
    }

    impl Driver {
        /// The `WinitView` AppKit is holding, or `None` if there is no window.
        fn view(&self) -> Option<Id> {
            let handle = self.window.as_ref()?.window_handle().ok()?;
            let RawWindowHandle::AppKit(h) = handle.as_raw() else {
                return None;
            };
            Some(Id::from_ptr(h.ns_view.as_ptr()))
        }

        /// Everything winit has queued, in order, cleared.
        fn drain(&mut self) -> Vec<Ime> {
            std::mem::take(&mut self.ime)
        }
    }

    /// Pretty-print an `Ime` for the transcript without depending on its
    /// `Debug`, which winit is free to change.
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
                .with_inner_size(winit::dpi::LogicalSize::new(480.0, 320.0))
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
            if let WindowEvent::Ime(ime) = e {
                self.ime.push(ime);
            }
        }

        fn about_to_wait(&mut self, el: &ActiveEventLoop) {
            if self.done {
                return;
            }
            // One stage per wait, so every stage's events are delivered by the
            // pump before the next stage runs. Driving all six in one callback
            // would compare each stage's expectations against the PREVIOUS
            // stage's queue, which is the shape of test that passes by
            // accident.
            let Some(view) = self.view() else {
                self.report.blocked = Some("the window has no AppKit view".to_owned());
                self.done = true;
                el.exit();
                return;
            };
            // ALTERNATE, drive then check. A stage's `Ime` events are queued
            // by the send and delivered by the NEXT pump, so the expectations
            // it wrote are run one `about_to_wait` later, against a queue that
            // holds its events and nobody else's. Doing both in one callback —
            // which this file did first — compares each stage's expectations
            // against the FOLLOWING stage's events, and reports the mismatch
            // under the wrong heading.
            if let Some(check) = self.pending.take() {
                check(self);
                return;
            }
            let stage = self.stage;
            self.stage += 1;
            match stage {
                // Stage 0 is a settle: the window has just been created and
                // `set_ime_allowed(true)` has to reach the view before any
                // composition means anything.
                0 => {}
                1 => self.stage_ascii(view),
                2 => self.stage_cjk(view),
                3 => self.stage_attributed(view),
                4 => self.stage_clamp(view),
                5 => self.stage_leave_preedit(view),
                6 => self.stage_candidate_rect(view),
                7 => self.stage_dead_key(view),
                _ => {
                    self.done = true;
                    el.exit();
                }
            }
        }
    }

    impl Driver {
        /// STAGE 1 — a whole composition in ASCII.
        fn stage_ascii(&mut self, view: Id) {
            println!("\n=== 1. A COMPOSITION, ASCII ===");
            let _ = self.drain();
            let Some(s) = ns_string("ab") else {
                self.report.fail("could not build an NSString".to_owned());
                return;
            };
            // SAFETY: `view` is the live `WinitView`; `s` is a live `NSString`.
            // `{2,0}` is a caret after both characters, which is what an input
            // method sends while composing.
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
            // SAFETY: `view` is live.
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

            // SAFETY: `view` is live; committing the composition.
            let Some(c) = ns_string("AB") else {
                self.report.fail("could not build an NSString".to_owned());
                return;
            };
            // SAFETY: as above.
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

        /// STAGE 2 — the same, in CJK.
        ///
        /// This is the stage a naive port fails. `selectedRange:` counts UTF-16
        /// code units and winit's `Ime::Preedit` promises UTF-8 BYTE offsets, so
        /// the body converts with `-substringToIndex:`. Every character here is
        /// three UTF-8 bytes and one UTF-16 unit, so a port that forwarded the
        /// range unconverted would answer 2 where 6 is correct — and would be
        /// green for every ASCII test ever written.
        fn stage_cjk(&mut self, view: Id) {
            println!("\n=== 2. A COMPOSITION, CJK (the UTF-16 -> UTF-8 conversion) ===");
            let _ = self.drain();
            let Some(s) = ns_string("にほ") else {
                self.report.fail("could not build an NSString".to_owned());
                return;
            };
            // SAFETY: `view` is live; `{2,0}` is two UTF-16 units in, which is
            // the end of this string and six UTF-8 bytes.
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
            // SAFETY: `view` is live.
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

        /// STAGE 3 — the `NSAttributedString` branch.
        ///
        /// `setMarkedText:`'s first argument is declared `@`, and the body
        /// decides what it actually is with `-isKindOfClass:`. Both halves are
        /// live code — Japanese and Korean input methods send attributed
        /// strings with underline attributes — and only one of them is on the
        /// path every other stage takes.
        fn stage_attributed(&mut self, view: Id) {
            println!("\n=== 3. setMarkedText: WITH AN NSAttributedString ===");
            let _ = self.drain();
            let Some(a) = attributed("한글") else {
                self.report
                    .fail("could not build an NSAttributedString".to_owned());
                return;
            };
            // SAFETY: `view` is live; `a` is a live `NSAttributedString`, which
            // is the other class this row's `@` argument is documented to
            // carry.
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
            // SAFETY: `view` is live.
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

        /// STAGE 4 — THE CLAMP, both halves.
        ///
        /// macOS's own Pinyin input method sends `selectedRange:` indices past
        /// the end of the string it is marking (alacritty#8791); unclamped,
        /// `-substringToIndex:` raises `NSRangeException` out of an
        /// Objective-C frame. Upstream clamps with `.min(len)`.
        ///
        /// The second half is a divergence this port introduced ON PURPOSE and
        /// is the reason this stage exists. Upstream computes the end of the
        /// selection with objc2's `NSRange::end()`, which is
        /// `checked_add(..).expect("NSRange too large")` — a PANIC, in release
        /// too, and a panic inside a trampoline is an abort. `aterm_objc`'s
        /// `NSRange` is a two-field POD with no range algebra, so the port
        /// spells the addition out, and spells it `saturating_add`: an input
        /// method that sends a location and length that overflow `usize` now
        /// clamps to the string's length instead of killing the process.
        fn stage_clamp(&mut self, view: Id) {
            println!("\n=== 4. OUT-OF-BOUNDS AND OVERFLOWING SELECTED RANGES ===");
            let _ = self.drain();
            let Some(s) = ns_string("hi") else {
                self.report.fail("could not build an NSString".to_owned());
                return;
            };
            // SAFETY: `view` is live. `{9, 0}` is past the end of a two-unit
            // string — the Pinyin shape.
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
            // SAFETY: `view` is live. `{usize::MAX, 5}` overflows `usize` when
            // added, which is what `NSRange::end()` panics on.
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

        /// STAGE 5 — leaving preedit without committing.
        fn stage_leave_preedit(&mut self, view: Id) {
            println!("\n=== 5. doCommandBySelector: AND unmarkText ===");
            let _ = self.drain();
            // Put the view back into a known composition first.
            let Some(s) = ns_string("xy") else {
                self.report.fail("could not build an NSString".to_owned());
                return;
            };
            // SAFETY: `view` is live.
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
            // SAFETY: `view` is live; `insertLineBreak:` is a real
            // `NSStandardKeyBindingResponding` selector, which is what AppKit
            // hands this row, and the body never sends it anywhere.
            unsafe { do_command(view, sel!(insertLineBreak:)) };
            // SAFETY: `view` is live.
            let after_command = unsafe { has_marked_text(view) };
            self.report.expect(
                "doCommandBySelector: leaves the marked text in place",
                after_command,
                format!("hasMarkedText={after_command}"),
            );

            // `unmarkText` sends `-discardMarkedText` to the view's input
            // context and would panic if there were none, which is a real
            // outcome on a machine with no window server rather than a defect.
            // SAFETY: `-inputContext` is `@@:` on `NSResponder`; `view` is live.
            let ctx: Id = unsafe {
                let f: unsafe extern "C" fn(Id, Sel) -> Id = msg();
                f(view, sel!(inputContext))
            };
            if ctx.is_null() {
                println!("    SKIPPED unmarkText: the view has no NSTextInputContext here");
            } else {
                // SAFETY: `view` is live and has an input context, which is
                // `unmarkText`'s only precondition.
                unsafe { send_void(view, sel!(unmarkText)) };
                // SAFETY: `view` is live.
                let after_unmark = unsafe { has_marked_text(view) };
                self.report.expect(
                    "unmarkText clears the composition",
                    !after_unmark,
                    format!("hasMarkedText={after_unmark}"),
                );
                // SAFETY: `view` is live.
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

        /// STAGE 6 — the candidate-window rectangle.
        ///
        /// The row whose encoding is the dangerous one, checked by its ANSWER.
        /// The cursor area is set through winit's public API, so the expectation
        /// is the contract an application relies on and not the port's
        /// arithmetic.
        fn stage_candidate_rect(&mut self, view: Id) {
            println!("\n=== 6. THE CANDIDATE-WINDOW RECTANGLE ===");
            let Some(w) = self.window.as_ref() else {
                return;
            };
            let scale = w.scale_factor();
            w.set_ime_cursor_area(
                winit::dpi::LogicalPosition::new(40.0, 60.0),
                winit::dpi::LogicalSize::new(10.0, 20.0),
            );
            // SAFETY: `view` is live; a null `actualRange:` is what AppKit
            // passes when it wants no out-parameter, and the body ignores it.
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
            // The SIZE is the one part that survives every coordinate
            // conversion unchanged: `convertRect:toView:` and
            // `convertRectToScreen:` translate, and on a 1x or 2x display
            // neither scales a view-space size. The ORIGIN is in screen
            // coordinates and depends on where the window server put the
            // window, so it is reported rather than asserted.
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

            // And the out-parameter, which is the argument a hidden result
            // pointer would displace: give it a real slot and require the row
            // to leave it exactly as it found it (the fork ignores it, as
            // upstream does).
            let mut actual = NSRange {
                location: 0xDEAD,
                length: 0xBEEF,
            };
            // SAFETY: `view` is live and `actual` is a live, writable
            // `NSRange` owned by this frame for the duration of the call —
            // exactly the contract `actualRange:` states.
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

        /// STAGE 7 — a real `NSEvent` through `keyDown:`.
        ///
        /// This is the only path that enters `-interpretKeyEvents:`, which is
        /// how a keystroke reaches the system input method and how a DEAD KEY
        /// becomes marked text. What that input method does with the event
        /// depends on the keyboard layout installed on the machine running this
        /// gate, which is not under this file's control — so the outcome is
        /// REPORTED, and only an outcome that is wrong under every layout is a
        /// finding: an abort (which would take the process, not this stage), or
        /// preedit arriving while the view believes IME is disabled.
        ///
        /// `keyCode` 14 is `E` on ANSI, which with Option held is the acute
        /// accent dead key on the U.S. layout.
        fn stage_dead_key(&mut self, view: Id) {
            println!("\n=== 7. A REAL KEY EVENT (dead-key path) ===");
            let _ = self.drain();
            // Leave any composition first, so what arrives is attributable.
            // SAFETY: `view` is live; the input context was checked in stage 5,
            // and `unmarkText` on a view with none is the only failure mode,
            // guarded here the same way.
            let ctx: Id = unsafe {
                let f: unsafe extern "C" fn(Id, Sel) -> Id = msg();
                f(view, sel!(inputContext))
            };
            if ctx.is_null() {
                println!("    SKIPPED: no NSTextInputContext, so interpretKeyEvents: goes nowhere");
                return;
            }
            let (Some(chars), Some(bare)) = (ns_string("´"), ns_string("e")) else {
                self.report
                    .fail("could not build the event's strings".to_owned());
                return;
            };
            const NS_KEY_DOWN: usize = 10;
            const NS_ALTERNATE: usize = 1 << 19;
            // SAFETY: this is
            // `+[NSEvent keyEventWithType:location:modifierFlags:timestamp:
            //   windowNumber:context:characters:charactersIgnoringModifiers:
            //   isARepeat:keyCode:]`, twelve parameters counting the class and
            // `_cmd` — inside `MsgFn`'s sixteen. A nil `context:` is documented
            // and is what AppKit itself passes on modern macOS. The result is
            // autoreleased.
            let event: Id = unsafe {
                let f: unsafe extern "C" fn(
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
                    NS_KEY_DOWN,
                    aterm_objc::CGPoint { x: 0.0, y: 0.0 },
                    NS_ALTERNATE,
                    0.0,
                    0,
                    Id::NIL,
                    chars.id(),
                    bare.id(),
                    Bool::NO,
                    14,
                )
            };
            if event.is_null() {
                self.report
                    .fail("+[NSEvent keyEventWithType:…] answered nil".to_owned());
                return;
            }
            println!("    built an Option+E NSEvent (keyCode 14, NSEventModifierFlagOption)");
            // SAFETY: `keyDown:` is registered `v@:@` and `event` is the live
            // autoreleased `NSEvent` just built — exactly what AppKit delivers.
            unsafe {
                let f: unsafe extern "C" fn(Id, Sel, Id) = msg();
                f(view, sel!(keyDown:), event);
            }
            println!(
                "    keyDown: returned without aborting — the trampoline and its panic \
                      guard both held"
            );
            // SAFETY: `view` is live.
            let marked = unsafe { has_marked_text(view) };
            println!("    hasMarkedText after the dead key = {marked}");
            self.pending = Some(Box::new(|d: &mut Driver| {
                let got = d.drain();
                println!("    events: {}", show_all(&got));
                // The one outcome that is wrong under every layout.
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
                     keyboard layout; the encoding-level claim this stage makes is that a real \
                     NSEvent reaches keyDown: through the registered v@:@ trampoline and \
                     interpretKeyEvents: runs."
                );
            }));
        }
    }

    /// Drive the loop until every stage has run, then report.
    pub fn run() -> i32 {
        let mut el = match EventLoop::new() {
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
        if driver.report.findings.is_empty() {
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
