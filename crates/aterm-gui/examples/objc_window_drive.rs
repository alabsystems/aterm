// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WINDOW DRIVER: `vendor/winit`'s `window_delegate.rs` answering for real,
//! through the 177 AppKit BINDING calls W8 moved off `objc2-app-kit`.
//!
//! # Why this exists beside the other three
//!
//! `objc_live_class_audit.rs` asks whether the fork's declared classes are
//! SHAPED right; `objc_ime_drive.rs` drives `view.rs`'s composition; W7's
//! `objc_toolbar_drive.rs` drives `aterm-gui`'s tab strip. NONE of them touches
//! the window surface, and W8 rewrote every send in it: resize, move, focus,
//! fullscreen, occlusion, close, drag-and-drop, the style mask, the appearance
//! observer and the tab group.
//!
//! `crates/aterm-objc/tests/winit_sent_prototypes.rs` is the other half of the
//! evidence and it is the CHEAPER half — it reads `method_getTypeEncoding` for
//! all 96 sent selectors and catches a wrong prototype, which is how the
//! `-requestUserAttention:` signedness defect was found. It cannot catch a
//! CORRECT send of the WRONG selector, an inverted boolean, a constant that
//! encodes fine and means something else (W7 shipped exactly that), or an
//! ownership mistake. That is this file.
//!
//! # What it drives, and against what
//!
//! A real `NSWindow` with a real `WindowDelegate` installed, through winit's
//! own public API — so every expectation is on the BACKEND CONTRACT rather than
//! on the port's internals, and the same file would pass against `objc2` and
//! against the port. That is the point: it is an A/B oracle, not a mirror of
//! what the port happens to do.
//!
//! Two rows cannot be reached through the public API and are entered at their
//! IMPs, exactly as AppKit enters them:
//!
//!  * `draggingEntered:` — the row whose ABI W6 CORRECTED, from a one-byte
//!    `B@:@` to the eight-byte `Q@:@` `NSDraggingDestination` declares. Its
//!    return value is checked against `NSDragOperationCopy`, which is the one
//!    thing a `bool` return could never have got right on the x86_64 slice.
//!  * `windowShouldClose:` — which must answer NO and queue `CloseRequested`,
//!    the inversion that would make a window uncloseable or unstoppable.
//!
//! # The weak-reference stage, added by W9
//!
//! Stage 12 does not drive `window_delegate.rs` at all; it drives
//! `aterm_objc::weak`, the capability `vendor/winit`'s `view.rs:169` is blocked
//! on, against the objects it exists for. It is here rather than in a fifth
//! driver because this file already stands up the exact graph the capability is
//! about — a live `NSWindow`, its `NSView` and the delegate AppKit is holding,
//! with the window retaining the view and the view wanting to name the window
//! back — and because libtest cannot host AppKit, so `aterm-objc`'s own
//! `tests/weak.rs` can only reach Foundation. Every property it checks is
//! proved there on `NSObject` and `NSMutableString`; what is new here is that
//! the classes are AppKit's, with AppKit's own `dealloc` and AppKit's own
//! references.
//!
//! # What is REPORTED rather than asserted, and why
//!
//! Absolute geometry is not asserted. A window's frame depends on the display,
//! the menu bar height, the dock, and whether the compositor honoured the
//! request at all — so the stages assert ROUND TRIPS and INVARIANTS ("what I
//! set is what I read", "this event arrived", "these two disagree") and PRINT
//! the numbers. A golden here would be a machine-specific constant pretending
//! to be a contract, which is the failure W7's toolbar matrix names.
//!
//! Fullscreen is driven but its completion is NOT required: the transition is
//! animated, takes about a second, and macOS refuses it outright while another
//! space transition is in flight. The stage asserts that the request is
//! ACCEPTED and that the state machine ends where it started, and reports the
//! rest.
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
/// The drive could not execute here. NOT a pass.
const NOT_RUN: i32 = 2;

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "objc-window-drive: NOT RUN — this drives \
         vendor/winit/src/platform_impl/macos/window_delegate.rs, which does not exist off macOS."
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

    use aterm_objc::{
        Bool, Id, Obj, Sel, WeakObj, WeakSlot, autoreleasepool, class, msg, ns_string, sel,
    };
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, EventLoop};
    use winit::platform::macos::WindowExtMacOS;
    use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::{Theme, Window, WindowButtons, WindowId};

    use super::{FAIL, NOT_RUN, PASS};

    /// Long enough for macOS to launch its `NSApplication`, hand back a window
    /// and run every stage; past it the drive reports NOT RUN.
    const BUDGET: Duration = Duration::from_secs(60);

    /// `NSDragOperationCopy`, the value `draggingEntered:` must answer for a
    /// pasteboard carrying filenames.
    const NS_DRAG_OPERATION_COPY: usize = 1;
    /// `NSDragOperationNone`, for a pasteboard carrying none.
    const NS_DRAG_OPERATION_NONE: usize = 0;

    // ------------------------------------------------------------------ sends
    //
    // Entered exactly as AppKit enters them: a typed `objc_msgSend` cast to the
    // row's own prototype. Deliberately NOT through `aterm_objc::send::*` — a
    // driver that reached for the same helpers the port reaches for would agree
    // with the port about a shape they both got wrong.

    /// `-draggingEntered:`, whose return is `NSDragOperation` — an
    /// `NSUInteger`, not a `BOOL`. The whole point of the row.
    ///
    /// # Safety
    /// `delegate` must be a live `WinitWindowDelegate`; `info` a live object
    /// conforming to `NSDraggingInfo`.
    unsafe fn dragging_entered(delegate: Id, info: Id) -> usize {
        // SAFETY: the row is registered `Q@:@`, which the live auditor checks
        // against `NSDraggingDestination`'s own description.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) -> usize = msg();
            f(delegate, sel!(draggingEntered:), info)
        }
    }

    /// `-windowShouldClose:`, a real `BOOL` row (`B@:@`).
    ///
    /// # Safety
    /// `delegate` must be a live `WinitWindowDelegate`.
    unsafe fn window_should_close(delegate: Id, sender: Id) -> bool {
        // SAFETY: registered `B@:@`, per `NSWindowDelegate`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) -> Bool = msg();
            f(delegate, sel!(windowShouldClose:), sender).as_bool()
        }
    }

    /// `-[NSWindow delegate]` — the delegate AppKit is actually holding, which
    /// is what `setDelegate:` was supposed to install.
    ///
    /// # Safety
    /// `window` must be a live `NSWindow`.
    unsafe fn window_delegate(window: Id) -> Id {
        // SAFETY: `-delegate` is `@16@0:8` on `NSWindow`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = msg();
            f(window, sel!(delegate))
        }
    }

    /// `-[NSWindow styleMask]`, read independently of the port.
    ///
    /// # Safety
    /// `window` must be a live `NSWindow`.
    unsafe fn style_mask(window: Id) -> usize {
        // SAFETY: `-styleMask` is `Q16@0:8` on `NSWindow`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> usize = msg();
            f(window, sel!(styleMask))
        }
    }

    /// `-[NSWindow title]` as a Rust `String`, read independently of the port.
    ///
    /// # Safety
    /// `window` must be a live `NSWindow`.
    unsafe fn window_title(window: Id) -> String {
        // SAFETY: `-title` is `@16@0:8` on `NSWindow` and never answers nil.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = msg();
            aterm_objc::ns_string_to_rust(f(window, sel!(title)))
        }
    }

    /// A pasteboard carrying `NSFilenamesPboardType`, and a dragging-info stub
    /// that answers it — the two objects `draggingEntered:` reads.
    ///
    /// Returns `(info, pasteboard)`; both are +1 and owned by the caller.
    fn dragging_info(paths: &[&str]) -> Option<(Obj, Obj)> {
        // SAFETY: every send below is a documented Foundation/AppKit selector
        // on the class named, with the prototype cast to match.
        unsafe {
            let pb: unsafe extern "C" fn(Id, Sel, Id) -> Id = msg();
            let name = ns_string("aterm-objc-window-drive")?;
            let board = pb(
                class(c"NSPasteboard").as_id(),
                sel!(pasteboardWithName:),
                name.id(),
            );
            let board = Obj::retain(board)?;

            // `+arrayWithObjects:count:` for the declared types, then the
            // property list for `NSFilenamesPboardType`.
            let arr: unsafe extern "C" fn(Id, Sel, *const Id, usize) -> Id = msg();
            let ty = filenames_pboard_type();
            let types = [ty];
            let type_list = arr(
                class(c"NSArray").as_id(),
                sel!(arrayWithObjects:count:),
                types.as_ptr(),
                types.len(),
            );

            let declare: unsafe extern "C" fn(Id, Sel, Id, Id) -> isize = msg();
            declare(board.id(), sel!(declareTypes:owner:), type_list, Id::NIL);

            if !paths.is_empty() {
                let strings: Vec<Obj> = paths.iter().filter_map(|p| ns_string(p)).collect();
                if strings.len() != paths.len() {
                    return None;
                }
                let ids: Vec<Id> = strings.iter().map(Obj::id).collect();
                let list = arr(
                    class(c"NSArray").as_id(),
                    sel!(arrayWithObjects:count:),
                    ids.as_ptr(),
                    ids.len(),
                );
                let set: unsafe extern "C" fn(Id, Sel, Id, Id) -> Bool = msg();
                set(board.id(), sel!(setPropertyList:forType:), list, ty);
            }

            let info = ATermDragInfo::alloc_init(aterm_objc::MainThread::new()?, board.id())?;
            let info = Obj::retain(info.as_id())?;
            Some((info, board))
        }
    }

    /// `NSFilenamesPboardType`, AppKit's own global.
    fn filenames_pboard_type() -> Id {
        #[link(name = "AppKit", kind = "framework")]
        unsafe extern "C" {
            #[link_name = "NSFilenamesPboardType"]
            static TY: Id;
        }
        // SAFETY: an AppKit global that exists for the life of the process.
        unsafe { TY }
    }

    aterm_objc::declare_class! {
        /// The smallest object that answers `-draggingPasteboard`, which is the
        /// only thing `draggingEntered:` and `performDragOperation:` ask of the
        /// sender AppKit gives them.
        ///
        /// It deliberately does NOT claim `NSDraggingInfo`: the fork's rows
        /// take a bare `id` and send one selector to it, so claiming the
        /// protocol would test AppKit's conformance machinery rather than the
        /// port.
        pub(super) struct ATermDragInfo: NSObject {
            const NAME: &str = "ATermObjcWindowDriveDragInfo";
            type Ivars = Id;

            @sel(draggingPasteboard)
            fn dragging_pasteboard(&self) -> Id {
                *self.ivars()
            }
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

    struct Driver {
        window: Option<Window>,
        report: Report,
        resized: usize,
        moved: usize,
        focused: Vec<bool>,
        close_requested: usize,
        hovered: Vec<String>,
        done: bool,
        stage: usize,
    }

    impl Driver {
        /// The `WinitWindowDelegate` AppKit is holding, and the `NSWindow` it
        /// belongs to, reached WITHOUT the port: from the raw window handle's
        /// view, up to its window, then that window's own `-delegate`.
        fn pair(&self) -> Option<(Id, Id)> {
            let handle = self.window.as_ref()?.window_handle().ok()?;
            let RawWindowHandle::AppKit(h) = handle.as_raw() else {
                return None;
            };
            let view = Id::from_ptr(h.ns_view.as_ptr());
            // SAFETY: `-window` is `@16@0:8` on `NSView`.
            let window = unsafe {
                let f: unsafe extern "C" fn(Id, Sel) -> Id = msg();
                f(view, sel!(window))
            };
            if window.is_null() {
                return None;
            }
            // SAFETY: `window` is the live `NSWindow` the view is installed in.
            let delegate = unsafe { window_delegate(window) };
            if delegate.is_null() {
                return None;
            }
            Some((delegate, window))
        }

        /// The window's own `NSView`, from the raw handle. Nil if there is none.
        fn ns_view(&self) -> Id {
            let Some(handle) = self.window.as_ref().and_then(|w| w.window_handle().ok()) else {
                return Id::NIL;
            };
            let RawWindowHandle::AppKit(h) = handle.as_raw() else {
                return Id::NIL;
            };
            Id::from_ptr(h.ns_view.as_ptr())
        }
    }

    impl ApplicationHandler for Driver {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let attrs = Window::default_attributes()
                .with_title("objc-window-drive")
                .with_inner_size(winit::dpi::LogicalSize::new(480.0, 320.0))
                .with_visible(false);
            match el.create_window(attrs) {
                Ok(w) => self.window = Some(w),
                Err(e) => {
                    self.report.blocked = Some(format!("no window could be created: {e}"));
                    self.done = true;
                    el.exit();
                }
            }
        }

        fn window_event(&mut self, _el: &ActiveEventLoop, _id: WindowId, e: WindowEvent) {
            match e {
                WindowEvent::Resized(_) => self.resized += 1,
                WindowEvent::Moved(_) => self.moved += 1,
                WindowEvent::Focused(f) => self.focused.push(f),
                WindowEvent::CloseRequested => self.close_requested += 1,
                WindowEvent::HoveredFile(p) => {
                    self.hovered.push(p.to_string_lossy().into_owned());
                }
                _ => {}
            }
        }

        fn about_to_wait(&mut self, el: &ActiveEventLoop) {
            if self.done {
                return;
            }
            let Some((delegate, ns)) = self.pair() else {
                self.report.blocked =
                    Some("the window has no AppKit view, window or delegate".to_owned());
                self.done = true;
                el.exit();
                return;
            };
            // One stage per wait, so a stage's winit events are delivered by the
            // pump before the next stage reads them — the alternation
            // `objc_ime_drive.rs` had to learn.
            self.stage += 1;
            self.report.ran += 1;
            match self.stage {
                1 => self.stage_title(ns),
                2 => self.stage_style_mask(ns),
                3 => self.stage_geometry(),
                4 => self.stage_size_limits(),
                5 => self.stage_visibility_and_focus(),
                6 => self.stage_theme(),
                7 => self.stage_window_ext(),
                8 => self.stage_drag_and_drop(delegate),
                9 => self.stage_close(delegate, ns),
                10 => self.stage_fullscreen_enter(),
                11 => self.stage_fullscreen_exit(),
                12 => self.stage_weak_references(delegate, ns),
                _ => {
                    self.done = true;
                    el.exit();
                }
            }
        }
    }

    impl Driver {
        fn w(&self) -> &Window {
            self.window.as_ref().expect("a window")
        }

        /// `-setTitle:`/`-title` — the `NSString` round trip, and the one path
        /// where a +1 string is handed to a copying setter.
        fn stage_title(&mut self, ns: Id) {
            println!("\n[1] title — setTitle: / title, through NSString");
            for t in ["objc-window-drive", "ünïcødé ⌘ 日本語", ""] {
                self.w().set_title(t);
                let via_port = self.w().title();
                // SAFETY: `ns` is the live `NSWindow`.
                let via_appkit = unsafe { window_title(ns) };
                self.report.check(
                    via_port == t && via_appkit == t,
                    &format!("title {t:?} round-trips (port {via_port:?}, AppKit {via_appkit:?})"),
                );
            }
        }

        /// The style-mask surface: every setter that reads-modifies-writes the
        /// `NSUInteger` that `NSWindowStyleMask` used to be, checked against
        /// AppKit's own `-styleMask` rather than against the port's.
        fn stage_style_mask(&mut self, ns: Id) {
            println!("\n[2] style mask — resizable, buttons, decorations");
            const TITLED: usize = 1 << 0;
            const CLOSABLE: usize = 1 << 1;
            const MINIATURIZABLE: usize = 1 << 2;
            const RESIZABLE: usize = 1 << 3;

            // SAFETY: `ns` is the live `NSWindow`, here and below.
            let mask = |()| unsafe { style_mask(ns) };

            self.w().set_resizable(false);
            let m = mask(());
            self.report.check(
                m & RESIZABLE == 0 && !self.w().is_resizable(),
                &format!("set_resizable(false) clears NSWindowStyleMaskResizable (mask {m:#x})"),
            );
            self.w().set_resizable(true);
            let m = mask(());
            self.report.check(
                m & RESIZABLE != 0 && self.w().is_resizable(),
                &format!("set_resizable(true) sets it back (mask {m:#x})"),
            );

            self.w().set_enabled_buttons(WindowButtons::empty());
            let m = mask(());
            self.report.check(
                m & CLOSABLE == 0 && m & MINIATURIZABLE == 0,
                &format!("empty buttons clear Closable|Miniaturizable (mask {m:#x})"),
            );
            let b = self.w().enabled_buttons();
            self.report.check(
                !b.contains(WindowButtons::CLOSE) && !b.contains(WindowButtons::MINIMIZE),
                &format!("enabled_buttons() agrees: {b:?}"),
            );
            self.w().set_enabled_buttons(WindowButtons::all());
            let b = self.w().enabled_buttons();
            self.report.check(
                b.contains(WindowButtons::CLOSE)
                    && b.contains(WindowButtons::MINIMIZE)
                    && b.contains(WindowButtons::MAXIMIZE),
                &format!("all buttons restored: {b:?} (mask {:#x})", mask(())),
            );

            self.w().set_decorations(false);
            let m = mask(());
            self.report.check(
                m & TITLED == 0 && !self.w().is_decorated(),
                &format!("set_decorations(false) clears Titled (mask {m:#x})"),
            );
            self.w().set_decorations(true);
            let m = mask(());
            self.report.check(
                m & TITLED != 0 && self.w().is_decorated(),
                &format!("set_decorations(true) restores it (mask {m:#x})"),
            );
        }

        /// `-setFrameOrigin:`, `-frame`, `-contentRectForFrameRect:` and
        /// `-setContentSize:` — the four struct-passing sends, which are the
        /// ones an ABI mistake corrupts silently.
        fn stage_geometry(&mut self) {
            use winit::dpi::{PhysicalPosition, PhysicalSize};
            println!("\n[3] geometry — position and size round trips");

            let before = self.w().outer_position().ok();
            self.w().set_outer_position(PhysicalPosition::new(120, 140));
            let after = self.w().outer_position().ok();
            self.report.check(
                after.is_some_and(|p| p.x == 120 && p.y == 140),
                &format!(
                    "set_outer_position(120,140) -> outer_position {after:?} (was {before:?})"
                ),
            );

            let _ = self.w().request_inner_size(PhysicalSize::new(640_u32, 400));
            let got = self.w().inner_size();
            let scale = self.w().scale_factor();
            self.report.check(
                got.width == 640 && got.height == 400,
                &format!("request_inner_size(640x400) -> inner_size {got:?} at scale {scale}"),
            );

            let outer = self.w().outer_size();
            self.report.check(
                outer.width >= got.width && outer.height >= got.height,
                &format!("outer_size {outer:?} contains inner_size {got:?}"),
            );
            let inner_pos = self.w().inner_position().ok();
            self.report.check(
                inner_pos.is_some(),
                &format!("inner_position answers {inner_pos:?}"),
            );
            self.report.note(&format!(
                "winit delivered {} Resized and {} Moved so far",
                self.resized, self.moved
            ));
        }

        /// `-setContentMinSize:`/`-setContentMaxSize:` and the clamp that
        /// follows them — the two sends whose effect is a RESIZE the caller did
        /// not ask for.
        fn stage_size_limits(&mut self) {
            use winit::dpi::PhysicalSize;
            println!("\n[4] size limits — min/max clamp the current size");

            self.w()
                .set_min_inner_size(Some(PhysicalSize::new(700_u32, 500)));
            let got = self.w().inner_size();
            self.report.check(
                got.width >= 700 && got.height >= 500,
                &format!("min 700x500 grew the window to {got:?}"),
            );

            self.w()
                .set_max_inner_size(Some(PhysicalSize::new(500_u32, 380)));
            let got = self.w().inner_size();
            self.report.check(
                got.width <= 700 && got.height <= 500,
                &format!("max 500x380 did not grow the window: {got:?}"),
            );

            self.w().set_min_inner_size(None::<PhysicalSize<u32>>);
            self.w().set_max_inner_size(None::<PhysicalSize<u32>>);
            let _ = self.w().request_inner_size(PhysicalSize::new(480_u32, 320));
            self.report.check(
                self.w().inner_size() == PhysicalSize::new(480, 320),
                &format!(
                    "limits cleared, 480x320 sticks: {:?}",
                    self.w().inner_size()
                ),
            );

            self.w()
                .set_resize_increments(Some(PhysicalSize::new(8_u32, 8)));
            let inc = self.w().resize_increments();
            self.report.check(
                inc.is_some(),
                &format!("resize_increments round-trips: {inc:?}"),
            );
            self.w().set_resize_increments(None::<PhysicalSize<u32>>);
        }

        /// `-makeKeyAndOrderFront:`, `-orderOut:`, `-isVisible`,
        /// `-isMiniaturized` and the `Focused` events the delegate queues.
        fn stage_visibility_and_focus(&mut self) {
            println!("\n[5] visibility and focus");
            self.w().set_visible(true);
            self.report.check(
                self.w().is_visible() == Some(true),
                &format!(
                    "set_visible(true) -> is_visible {:?}",
                    self.w().is_visible()
                ),
            );
            self.w().focus_window();
            self.report.note(&format!(
                "has_focus {} — an unfocused test process may not be granted key",
                self.w().has_focus()
            ));
            self.report.check(
                self.w().is_minimized() == Some(false),
                &format!(
                    "is_minimized {:?} before minimizing",
                    self.w().is_minimized()
                ),
            );
            self.w().set_visible(false);
            self.report.check(
                self.w().is_visible() == Some(false),
                &format!(
                    "set_visible(false) -> is_visible {:?}",
                    self.w().is_visible()
                ),
            );
            self.w().set_visible(true);
            self.report
                .note(&format!("Focused events so far: {:?}", self.focused));
        }

        /// `+appearanceNamed:`, `-setAppearance:`, `-appearance` and
        /// `-bestMatchFromAppearancesWithNames:` — the four-send round trip
        /// that decides light vs dark, plus the KVO observer's key path.
        fn stage_theme(&mut self) {
            println!("\n[6] theme — appearanceNamed:/setAppearance:/bestMatch…");
            self.w().set_theme(Some(Theme::Dark));
            let t = self.w().theme();
            self.report.check(
                t == Some(Theme::Dark),
                &format!("set_theme(Dark) -> theme() {t:?}"),
            );
            self.w().set_theme(Some(Theme::Light));
            let t = self.w().theme();
            self.report.check(
                t == Some(Theme::Light),
                &format!("set_theme(Light) -> theme() {t:?}"),
            );
            self.w().set_theme(None);
            let t = self.w().theme();
            self.report.check(
                t.is_some(),
                &format!("set_theme(None) falls back to the system appearance: {t:?}"),
            );
        }

        /// `WindowExtMacOS` — shadow, tabbing identifier, tab count and the
        /// document-edited dot. Six more sends, three of them nil-answering.
        fn stage_window_ext(&mut self) {
            println!("\n[7] WindowExtMacOS — shadow, tabs, document-edited");
            let had = self.w().has_shadow();
            self.w().set_has_shadow(!had);
            self.report.check(
                self.w().has_shadow() == !had,
                &format!("set_has_shadow({}) round-trips", !had),
            );
            self.w().set_has_shadow(had);

            self.w().set_tabbing_identifier("aterm-objc-window-drive");
            let id = self.w().tabbing_identifier();
            self.report.check(
                id == "aterm-objc-window-drive",
                &format!("tabbing_identifier round-trips: {id:?}"),
            );

            let n = self.w().num_tabs();
            self.report.check(
                n >= 1,
                &format!("num_tabs {n} — nil tabbedWindows must read as the one tab it is"),
            );
            // Exercises `-tabGroup`, which answers nil for an untabbed window;
            // the port must return rather than message nil.
            self.w().select_tab_at_index(0);
            self.w().select_next_tab();
            self.w().select_previous_tab();
            self.report
                .check(true, "tab navigation on an untabbed window does not raise");

            self.w().set_document_edited(true);
            self.report.check(
                self.w().is_document_edited(),
                "set_document_edited(true) -> is_document_edited",
            );
            self.w().set_document_edited(false);
            self.report.check(
                !self.w().is_document_edited(),
                "set_document_edited(false) -> !is_document_edited",
            );

            self.w().set_content_protected(true);
            self.w().set_content_protected(false);
            self.w().set_blur(true);
            self.w().set_blur(false);
            let _ = self.w().set_cursor_hittest(false);
            let _ = self.w().set_cursor_hittest(true);
            self.report.check(
                true,
                "content protection, blur and cursor hittest all complete",
            );
        }

        /// THE ROW WHOSE ABI W6 CORRECTED. `draggingEntered:` returns an
        /// eight-byte `NSDragOperation`, not a one-byte `BOOL`, and the value
        /// it must answer is `NSDragOperationCopy` (1) for a pasteboard with
        /// filenames and `NSDragOperationNone` (0) for one without.
        fn stage_drag_and_drop(&mut self, delegate: Id) {
            println!("\n[8] drag and drop — draggingEntered: returns NSDragOperation");
            let Some((info, _pb)) = dragging_info(&["/tmp/aterm-drive-a", "/tmp/aterm-drive-b"])
            else {
                self.report
                    .note("could not build a dragging-info stub; stage skipped");
                return;
            };
            // SAFETY: `delegate` is the live `WinitWindowDelegate`; `info` is a
            // live object answering `-draggingPasteboard`.
            let op = unsafe { dragging_entered(delegate, info.id()) };
            self.report.check(
                op == NS_DRAG_OPERATION_COPY,
                &format!("two filenames -> {op} (NSDragOperationCopy is {NS_DRAG_OPERATION_COPY})"),
            );
            self.report.check(
                op >> 8 == 0,
                &format!(
                    "the answer occupies one byte's worth of value ({op:#x}) — a BOOL-shaped \
                     return would have left the upper bits of the return register unspecified"
                ),
            );

            let Some((empty, _pb2)) = dragging_info(&[]) else {
                self.report
                    .note("could not build an empty dragging-info stub");
                return;
            };
            // SAFETY: as above, for a pasteboard carrying no filenames list.
            let op = unsafe { dragging_entered(delegate, empty.id()) };
            self.report.check(
                op == NS_DRAG_OPERATION_NONE,
                &format!("no filenames -> {op} (NSDragOperationNone is {NS_DRAG_OPERATION_NONE})"),
            );
        }

        /// `windowShouldClose:` must answer NO and queue `CloseRequested` — the
        /// inversion that would make the window uncloseable or unstoppable.
        fn stage_close(&mut self, delegate: Id, ns: Id) {
            println!("\n[9] close — windowShouldClose: answers NO and queues CloseRequested");
            let before = self.close_requested;
            // SAFETY: `delegate` is the live delegate; `ns` is a valid sender.
            let answer = unsafe { window_should_close(delegate, ns) };
            self.report.check(
                !answer,
                "windowShouldClose: answers NO, so winit decides when to close",
            );
            self.report.note(&format!(
                "CloseRequested count {} -> {} (delivered on the next pump)",
                before, self.close_requested
            ));
        }

        /// The fullscreen state machine, ENTERING. Split from the exit by a
        /// pump on purpose — see the note on [`Self::stage_fullscreen_exit`].
        fn stage_fullscreen_enter(&mut self) {
            use winit::window::Fullscreen;
            println!("\n[10] fullscreen — entering");
            self.report
                .check(self.w().fullscreen().is_none(), "starts windowed");
            self.w().set_fullscreen(Some(Fullscreen::Borderless(None)));
            self.report.check(
                self.w().fullscreen().is_some(),
                "set_fullscreen(Borderless) is accepted by the state machine",
            );
        }

        /// The fullscreen state machine, LEAVING — and the assertion here is
        /// deliberately weaker than the obvious one.
        ///
        /// THE OBVIOUS ONE IS WRONG, and this file asserted it for one run.
        /// `set_fullscreen(None)` immediately after `set_fullscreen(Some(_))`
        /// does NOT return `fullscreen()` to `None`: AppKit delivers
        /// `windowWillEnterFullScreen:` synchronously from inside
        /// `-toggleFullScreen:`, which sets `in_fullscreen_transition`, and
        /// winit's documented answer to a fullscreen change during a transition
        /// is to DEFER it into `target_fullscreen` and apply it when
        /// `windowDidEnterFullScreen:` arrives. Both states are legal; a driver
        /// that demands one of them is testing the timing of the machine it is
        /// standing on, not the port.
        ///
        /// So: drive the exit a pump later than the entry (which is the path a
        /// real application takes), then require only that the machine is in a
        /// LEGAL state and report which. What would be a real finding — a raise
        /// out of an AppKit frame, or a panic — fails the process, not this
        /// assertion.
        fn stage_fullscreen_exit(&mut self) {
            println!("\n[11] fullscreen — leaving, and simple fullscreen");
            let before = self.w().fullscreen().is_some();
            self.w().set_fullscreen(None);
            let after = self.w().fullscreen();
            self.report.check(
                after.is_none() || before,
                "set_fullscreen(None) either lands or is deferred by a live transition",
            );
            self.report.note(&format!(
                "fullscreen() was {before:?} before the exit request and is {} after",
                after.is_some()
            ));

            let was = self.w().simple_fullscreen();
            let took = self.w().set_simple_fullscreen(true);
            self.report.note(&format!(
                "set_simple_fullscreen(true) -> {took} (was {was}, now {}) — refused while \
                 native fullscreen is active, which is upstream's rule",
                self.w().simple_fullscreen()
            ));
            self.w().set_simple_fullscreen(false);
            self.report.check(
                !self.w().simple_fullscreen(),
                "simple fullscreen is off again",
            );
            let m = self.w().is_maximized();
            self.report.check(
                m == self.w().is_maximized(),
                &format!("is_maximized answers without raising and is stable: {m}"),
            );
        }

        /// W9's capability, against the objects it exists for.
        ///
        /// `aterm_objc`'s `tests/weak.rs` proves every property here on
        /// `NSObject` and `NSMutableString`, because libtest cannot host
        /// AppKit. This stage runs the same experiments on the real thing: the
        /// live `NSWindow` this drive created, its `NSView`, and the
        /// `WinitWindowDelegate` AppKit is holding — which is exactly the edge
        /// `vendor/winit`'s `view.rs:169` needs, an `NSView` naming its
        /// `NSWindow` weakly because the window retains the view.
        ///
        /// Foundation and AppKit are not the same runtime surface for this:
        /// `NSWindow` and `NSView` are AppKit classes with their own
        /// `dealloc`, their own `-retain` overrides in some subclasses, and (in
        /// the window's case) an `NSApplication` that keeps its own references.
        /// A weak reference that worked on `NSObject` and not on `NSWindow`
        /// would be found here and nowhere else.
        fn stage_weak_references(&mut self, delegate: Id, ns: Id) {
            println!(
                "\n[12] weak references — objc_initWeak/loadWeak/storeWeak/copyWeak/destroyWeak"
            );

            let view = self.ns_view();
            if view.is_null() {
                self.report
                    .check(false, "the window has an NSView to weakly reference");
                return;
            }

            // The retain count is read BEFORE any weak reference exists.
            //
            // It was first read after the three below were made and compared
            // only across `clone_weak`, and a plant that made `WeakObj::new`
            // retain sailed past it — the baseline had already moved. A
            // difference is only evidence if the measurement starts before the
            // thing it is measuring.
            //
            // SAFETY: `-retainCount` is `q16@0:8`; used as a DIFFERENCE only,
            // which is the only thing this number can honestly support.
            let count = |id: Id| unsafe {
                let f: unsafe extern "C" fn(Id, Sel) -> isize = msg();
                f(id, sel!(retainCount))
            };
            let before = count(view);

            // ---- the live AppKit objects, weakly held -------------------
            // SAFETY: `ns`, `view` and `delegate` are live instances this
            // window owns for the whole stage — `self.window` is still held.
            let (w_win, w_view, w_del) =
                unsafe { (WeakObj::new(ns), WeakObj::new(view), WeakObj::new(delegate)) };
            for (name, weak, want) in [
                ("NSWindow", &w_win, ns),
                ("NSView", &w_view, view),
                ("WinitWindowDelegate", &w_del, delegate),
            ] {
                let got = weak.load();
                self.report.check(
                    got.as_ref().map(Obj::id).map(Id::addr) == Some(want.addr()),
                    &format!("a weak reference to the live {name} loads back as the same object"),
                );
            }

            // The +0 load, against a live AppKit object — and it is now the
            // RAW ABI, with no safe wrapper above it.
            //
            // `WeakObj::load_borrowed` used to be checked here. It is WITHDRAWN
            // (see `aterm-objc`'s `weak.rs` module docs): the pool argument
            // named a lifetime the runtime does not honour, because
            // `objc_loadWeak` autoreleases into the innermost pool ON THE
            // THREAD and a caller inside a nested pool can still name the outer
            // one. Deleting it costs this driver nothing measurable — the same
            // objc4 source that makes it unsound makes it slower than `load`.
            let slot = WeakSlot::uninit();
            let borrowed = autoreleasepool(|_pool| {
                // SAFETY: `slot` is freshly `uninit()`, is a local of this
                // block that does not move before `destroy` below, and `ns` is
                // a live NSWindow this stage holds. The +0 answer is read
                // inside the pool that is open right now, which — this call
                // being the innermost frame — is the one it lands in.
                unsafe {
                    slot.init(ns);
                    slot.load_autoreleased().addr()
                }
            });
            // SAFETY: `slot` was initialised immediately above and has not
            // moved; the runtime must not be left holding this stack address.
            unsafe { slot.destroy() };
            self.report.check(
                borrowed == ns.addr(),
                "the raw +0 weak load (objc_loadWeak) inside a pool answers the same NSWindow",
            );

            // A weak reference must not keep an AppKit object alive, so it must
            // not retain one either — neither `objc_initWeak` nor
            // `objc_copyWeak`.
            let extra = w_view.clone_weak();
            let after = count(view);
            self.report.check(
                after == before,
                &format!(
                    "two weak references leave the NSView's retain count at {before} \
                     (read {after}); a weak reference that retains is a strong one with a \
                     misleading name"
                ),
            );
            self.report.check(
                extra.load().map(|o| o.id().addr()) == Some(view.addr()),
                "objc_copyWeak's second registration names the same NSView",
            );

            // ---- moving the handle, against a real AppKit object ---------
            //
            // Six moves of the Rust value; the registered ADDRESS must not
            // move, because the runtime holds it.
            let registered = w_view.slot_addr();
            let moved = {
                let boxed = Box::new(w_view);
                let mut v = Vec::with_capacity(1);
                v.push(*boxed);
                for _ in 0..8 {
                    v.push(WeakObj::empty());
                }
                v.swap_remove(0)
            };
            self.report.check(
                moved.slot_addr() == registered,
                "a WeakObj naming a live NSView keeps its registered address across six moves",
            );
            self.report.check(
                moved.load().map(|o| o.id().addr()) == Some(view.addr()),
                "and still resolves to that NSView afterwards",
            );

            // ---- the hazard, on a REAL AppKit class ----------------------
            self.stage_weak_dealloc();
        }

        /// The nil-on-dealloc half, and the memcpy hazard, against a standalone
        /// `NSView` this stage owns outright.
        ///
        /// The window's own view cannot be used for it: AppKit holds
        /// references to an installed view and this drive cannot make it
        /// deallocate. A free-standing `[[NSView alloc] initWithFrame:]` that
        /// is never added to a hierarchy is owned by exactly one reference —
        /// this one — so releasing it really does run `-dealloc`.
        fn stage_weak_dealloc(&mut self) {
            // Built and destroyed inside a pool, so an autorelease anywhere in
            // `-dealloc` cannot leave the object alive past the check.
            let observed = autoreleasepool(|_| {
                // SAFETY: `+alloc` is `@16#0:8` and `-initWithFrame:` is
                // `@48@0:8{CGRect=dddd}16` on `NSView`; the pair yields a +1.
                let view = unsafe {
                    let alloc: unsafe extern "C" fn(Id, Sel) -> Id = msg();
                    let init: unsafe extern "C" fn(Id, Sel, aterm_objc::CGRect) -> Id = msg();
                    let raw = alloc(class(c"NSView").as_id(), sel!(alloc));
                    Obj::from_owned(init(
                        raw,
                        sel!(initWithFrame:),
                        aterm_objc::CGRect {
                            origin: aterm_objc::CGPoint { x: 0.0, y: 0.0 },
                            size: aterm_objc::CGSize {
                                width: 10.0,
                                height: 10.0,
                            },
                        },
                    ))
                }?;
                let target = view.id().addr();

                let weak = WeakObj::from_obj(&view);
                // The naive inline move, performed by hand on two raw slots —
                // what a `struct Weak { slot: Id }` would emit, and what Rust
                // has no move constructor to do anything else about.
                let registered = WeakSlot::uninit();
                let memcpyd = WeakSlot::uninit();
                // SAFETY: `registered` is freshly `uninit()`, does not move for
                // the rest of this closure, and `view` is a live +1.
                unsafe { registered.init(view.id()) };
                // SAFETY: both slots are `#[repr(transparent)]` over one
                // pointer word, both are live locals here, and they do not
                // overlap. This deliberately creates the unsound state the
                // check below measures.
                unsafe { std::ptr::copy_nonoverlapping(registered.addr(), memcpyd.addr(), 1) };

                // The handle is MOVED (into the return position of this
                // closure's inner block) before the object dies.
                let moved = weak;
                drop(view);

                // SAFETY: three plain pointer-word reads. None is
                // dereferenced, messaged or retained — `memcpyd`'s is a
                // dangling pointer and reading its value is all that is done
                // with it.
                let (a, b, c) = unsafe { (registered.peek(), memcpyd.peek(), moved.peek()) };
                let live = moved.is_live();
                // SAFETY: `registered` is initialised and destroyed exactly
                // once, here, before this frame dies. `memcpyd` was never
                // registered and must NOT be destroyed.
                unsafe { registered.destroy() };
                Some((target, a, b, c, live))
            });

            let Some((target, registered, memcpyd, boxed, live)) = observed else {
                self.report
                    .check(false, "a standalone NSView could be created");
                return;
            };

            self.report.check(
                registered.is_null(),
                &format!(
                    "the runtime nils the slot it registered when a real NSView deallocates \
                     (read {registered:?})"
                ),
            );
            self.report.check(
                memcpyd.addr() == target,
                &format!(
                    "THE HAZARD: the memcpy'd slot still holds the dead NSView's address \
                     {target:#x} (read {memcpyd:?}) — an inline slot would hand this to the \
                     next `load`"
                ),
            );
            self.report.check(
                boxed.is_null() && !live,
                &format!(
                    "the MOVED WeakObj reads nil and reports gone (read {boxed:?}, live={live})"
                ),
            );
        }
    }

    /// Drive the loop until every stage has run, then report.
    pub fn run() -> i32 {
        let mut el = match EventLoop::new() {
            Ok(el) => el,
            Err(e) => {
                eprintln!("objc-window-drive: NOT RUN — no event loop: {e}");
                return NOT_RUN;
            }
        };
        let mut driver = Driver {
            window: None,
            report: Report::default(),
            resized: 0,
            moved: 0,
            focused: Vec::new(),
            close_requested: 0,
            hovered: Vec::new(),
            done: false,
            stage: 0,
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
                    "objc-window-drive: NOT RUN — the stages did not finish within {BUDGET:?}"
                );
                return NOT_RUN;
            }
        }

        // The drag stage's HoveredFile events are delivered by the pump, so
        // they are read after the loop rather than inside the stage.
        let hovered = std::mem::take(&mut driver.hovered);
        println!("\n[8b] the paths draggingEntered: queued");
        let want = ["/tmp/aterm-drive-a", "/tmp/aterm-drive-b"];
        driver.report.check(
            hovered == want,
            &format!("HoveredFile events {hovered:?} (want {want:?})"),
        );
        driver.report.check(
            driver.close_requested >= 1,
            &format!("CloseRequested arrived {} time(s)", driver.close_requested),
        );

        drop(driver.window.take());

        if let Some(why) = driver.report.blocked {
            eprintln!("objc-window-drive: NOT RUN — {why}");
            return NOT_RUN;
        }
        println!("\n=== VERDICT ===");
        println!("  {} stage(s) ran", driver.report.ran);
        if driver.report.findings.is_empty() {
            println!("objc-window-drive: OK — every stage that ran agreed.");
            PASS
        } else {
            for f in &driver.report.findings {
                println!("  FAIL: {f}");
            }
            println!(
                "objc-window-drive: {} FINDING(S)",
                driver.report.findings.len()
            );
            FAIL
        }
    }
}
