// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The toolbar driver: `toolbar.rs`'s four declared classes and its AppKit
//! binding sites, entered through AppKit's own dispatch on a real `NSWindow`
//! and read back as pixels, wakes and registered encodings.
//!
//! `toolbar.rs`'s `objc_tests` check registered encodings against a table in
//! the same file, so a wrong encoding with a matching table edit passes there.
//! This driver reads the classes off the live objects AppKit holds instead.
//!
//! # What it refuses to do
//!
//! * It never calls `toolbar::native_strip_container`. It finds the strip the
//!   way AppKit does (`-[NSWindow toolbar]` -> `-items` -> item 0 -> `-view`),
//!   so the thing under test is what the toolbar delegate handed AppKit.
//! * It never sends through `crate::appkit`. Every message goes through this
//!   module's own typed casts ([`macos`]'s `s_*` family), so a wrong prototype
//!   in the helper layer cannot agree with itself.
//! * It never asserts a pixel hash against a stored golden: hashes depend on
//!   backing scale, system font and accent tint. It asserts relations (every
//!   state draws; states that must differ do) and prints the whole state
//!   matrix, so running it at two commits on one machine and diffing the
//!   transcripts is the A/B oracle. The strip's ink columns catch layout
//!   defects no encoding check sees (e.g. `NSTextAlignmentCenter` is 1 on
//!   arm64, not 2).
//!
//! # Exit contract (read by `aterm_verify::stages::OBJC_DRIVES`)
//!
//! | code | meaning |
//! |---|---|
//! | 0 | every check passed |
//! | 1 | at least one finding; the transcript names each |
//! | 2 | not run: no event loop, no window, or no toolbar was installed; or the only failures were clicks that need this drive to be the active app with its window key, made while another app held activation. Never a pass. |
//! | 3 | the drive hung (a modal tracking loop never returned) and the watchdog killed it |
//!
//! `3` has its own code because this driver enters `-mouseDown:` IMPs directly,
//! and a context menu that really popped would run
//! `-[NSMenu popUpContextMenu:…]`'s modal tracking loop and never return.
//!
//! Stages 3 to 5 click through AppKit's own routing, which reaches the strip
//! only while the drive is the active app and its window is key; an inactive
//! app spends the click on activation. macOS can refuse the activation the
//! drive asks for, or hand it to another app mid-stage: at the desk on
//! 2026-10-19 a person's trackpad click on another window did, and three clicks
//! failed with `app active=false` (`docs/measured/desk-appkit-2026-10-19.md`).
//! Such a click measured nothing about the strip, so it is `2`, never a
//! finding; a click that fails while the drive held activation is still `1`.

/// Every check passed.
#[cfg(target_os = "macos")]
pub(crate) const PASS: i32 = 0;
/// At least one finding. See the transcript.
#[cfg(target_os = "macos")]
pub(crate) const FINDING: i32 = 1;
/// The drive could not execute here. NOT a pass.
#[cfg(target_os = "macos")]
pub(crate) const NOT_RUN: i32 = 2;
/// A modal tracking loop never returned; the watchdog killed the process.
#[cfg(target_os = "macos")]
pub(crate) const HUNG: i32 = 3;

/// Drive the real tab strip and return the exit code above.
///
/// Called from `examples/objc_toolbar_drive.rs`, which owns `fn main`; the body
/// lives here because `mod toolbar`, `Wake` and `WindowId` are library-private
/// (as with `run_redraw_conformance`).
#[cfg(target_os = "macos")]
#[must_use]
pub fn run_toolbar_drive() -> i32 {
    // The drive is a windowed process of its own: it builds AppKit windows and
    // reads NSApp, so it mints the proof a windowed launch carries and hands it
    // to the body that sends `+sharedApplication`.
    macos::run(crate::platform::Windowed::launch())
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::CStr;
    use std::time::{Duration, Instant};

    use aterm_objc::{
        Bool, CGPoint, CGRect, CGSize, ClassPtr, Id, Sel, checked_msg, class, class_methods,
        class_name, class_of, class_protocols, method_types, ns_string, protocol,
        protocol_method_types, sel, strip_method_offsets, superclass_of,
    };
    use aterm_window::application::ApplicationHandler;
    use aterm_window::event_loop::{ActiveEventLoop, EventLoop};
    use aterm_window::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
    use aterm_window::window::{Window, WindowId as WinitWindowId};

    use super::{FINDING, HUNG, NOT_RUN, PASS};
    use crate::session_chrome::{TabChromeExt, TabMenuEntry};
    use crate::tab_bar::TabStripMetadata;
    use crate::tab_model::{TabConnRole, TabIconKind, TabId};
    use crate::{Wake, WindowId, toolbar};

    // ---------------------------------------------------------------- sends
    //
    // Not `crate::appkit`: an instrument using the helper layer would only show
    // that layer agreeing with itself. Every prototype below is written out
    // here and checked against clang's `@encode`-derived signature.

    /// `+[NSMenu popUpContextMenu:withEvent:forView:]`, the strip's one pop site
    /// (toolbar.rs), as the class method it is: `(cls, _cmd, menu, event,
    /// view)`, void.
    type PopUpContextMenu = unsafe extern "C-unwind" fn(Id, Sel, Id, Id, Id);

    /// The site that replaces it for the life of the drive process.
    ///
    /// No menu ever tracks in this process. A synthesized ctrl-left or right
    /// mouse may route into the titlebar's view tree on some macOS versions,
    /// where `popUpContextMenu:` opens a real menu whose synchronous tracking
    /// loop never returns. So the pop site is replaced before stage 5 with a
    /// stub that records the pop (count and item count) and returns; the stage
    /// reads the strip's `TabContextMenuOpening` wake and this record.
    static POP_UP_CONTEXT_MENU: aterm_objc::swizzle::SwizzleSite<PopUpContextMenu> =
        aterm_objc::swizzle::SwizzleSite::new();
    /// Pops the stub swallowed, and the item count of the last one.
    static MENUS_SWALLOWED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static LAST_MENU_ITEMS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// The stub, entered by the runtime as the class method it replaces.
    /// Nothing here can unwind (two atomic stores and one `-numberOfItems` send
    /// on a live menu).
    unsafe extern "C-unwind" fn swallow_pop_up(
        _cls: Id,
        _cmd: Sel,
        menu: Id,
        _event: Id,
        _view: Id,
    ) {
        use std::sync::atomic::Ordering::Relaxed;
        // SAFETY: `menu` is the live NSMenu the strip just built (+0 for this
        // call); `-numberOfItems` is `-(NSInteger)`, sent as that. A nil menu
        // (or a negative answer) is recorded as zero items.
        let items = if menu.is_null() {
            0
        } else {
            usize::try_from(unsafe { s_isize(menu, sel!(numberOfItems)) }).unwrap_or(0)
        };
        LAST_MENU_ITEMS.store(items, Relaxed);
        MENUS_SWALLOWED.fetch_add(1, Relaxed);
    }

    /// How many menus the stub has swallowed so far.
    fn pops() -> usize {
        MENUS_SWALLOWED.load(std::sync::atomic::Ordering::Relaxed)
    }

    unsafe fn s_v_id(r: Id, s: Sel, a: Id) {
        // SAFETY: as above, for `void (id, SEL, id)`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
            f(r, s, a);
        }
    }
    unsafe fn s_v_bool(r: Id, s: Sel, a: bool) {
        // SAFETY: as above, for `void (id, SEL, BOOL)`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, Bool) = checked_msg();
            f(r, s, Bool::new(a));
        }
    }
    unsafe fn s_id(r: Id, s: Sel) -> Id {
        // SAFETY: as above, for `id (id, SEL)`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            f(r, s)
        }
    }
    unsafe fn s_id_id(r: Id, s: Sel, a: Id) -> Id {
        // SAFETY: as above, for `id (id, SEL, id)`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
            f(r, s, a)
        }
    }
    unsafe fn s_id_usize(r: Id, s: Sel, a: usize) -> Id {
        // SAFETY: as above, for `id (id, SEL, NSUInteger)`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Id = checked_msg();
            f(r, s, a)
        }
    }
    unsafe fn s_usize(r: Id, s: Sel) -> usize {
        // SAFETY: as above, for `NSUInteger (id, SEL)`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
            f(r, s)
        }
    }
    unsafe fn s_isize(r: Id, s: Sel) -> isize {
        // SAFETY: as above, for `NSInteger (id, SEL)`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> isize = checked_msg();
            f(r, s)
        }
    }
    unsafe fn s_bool(r: Id, s: Sel) -> bool {
        // SAFETY: as above, for `BOOL (id, SEL)`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> Bool = checked_msg();
            f(r, s).as_bool()
        }
    }
    unsafe fn s_bool_id(r: Id, s: Sel, a: Id) -> bool {
        // SAFETY: as above, for `BOOL (id, SEL, id)`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Bool = checked_msg();
            f(r, s, a).as_bool()
        }
    }
    unsafe fn s_rect(r: Id, s: Sel) -> CGRect {
        // SAFETY: as above, for `NSRect (id, SEL)`. 32 bytes, returned in
        // registers on both Apple ABIs for this shape.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> CGRect = checked_msg();
            f(r, s)
        }
    }
    unsafe fn s_id_point(r: Id, s: Sel, p: CGPoint) -> Id {
        // SAFETY: as above, for `id (id, SEL, NSPoint)`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, CGPoint) -> Id = checked_msg();
            f(r, s, p)
        }
    }
    unsafe fn s_point_point_id(r: Id, s: Sel, p: CGPoint, v: Id) -> CGPoint {
        // SAFETY: as above, for `NSPoint (id, SEL, NSPoint, id)`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, CGPoint, Id) -> CGPoint = checked_msg();
            f(r, s, p, v)
        }
    }
    unsafe fn s_v_rect_id(r: Id, s: Sel, a: CGRect, b: Id) {
        // SAFETY: as above, for `void (id, SEL, NSRect, id)`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, CGRect, Id) = checked_msg();
            f(r, s, a, b);
        }
    }
    unsafe fn s_id_rect(r: Id, s: Sel, a: CGRect) -> Id {
        // SAFETY: as above, for `id (id, SEL, NSRect)`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, CGRect) -> Id = checked_msg();
            f(r, s, a)
        }
    }
    unsafe fn s_bytes(r: Id, s: Sel) -> *const u8 {
        // SAFETY: as above, for `unsigned char * (id, SEL)`
        // (`-[NSBitmapImageRep bitmapData]`). Taken as `Id` and re-cast
        // because both are one pointer-sized return in the same register.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            f(r, s).as_ptr().cast_const().cast::<u8>()
        }
    }
    unsafe fn s_v_rect_bool(r: Id, s: Sel, a: CGRect, b: bool) {
        // SAFETY: as above, for `void (id, SEL, NSRect, BOOL)`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, CGRect, Bool) = checked_msg();
            f(r, s, a, Bool::new(b));
        }
    }

    /// `+[NSEvent mouseEventWithType:location:modifierFlags:timestamp:windowNumber:context:eventNumber:clickCount:pressure:]`,
    /// encoded `@92@0:8Q16{CGPoint=dd}24Q40d48q56@64q72q80f88`: `pressure` is
    /// `f32` and the two counters are `NSInteger`.
    unsafe fn mouse_event(
        kind: usize,
        loc: CGPoint,
        flags: usize,
        win: isize,
        clicks: isize,
    ) -> Id {
        // SAFETY: a class method on `NSEvent` (linked), cast to the exact
        // prototype above; every argument is a plain scalar or nil.
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
                sel!(mouseEventWithType:location:modifierFlags:timestamp:windowNumber:context:eventNumber:clickCount:pressure:),
                kind,
                loc,
                flags,
                0.0,
                win,
                Id::NIL,
                0,
                clicks,
                if clicks > 0 { 1.0 } else { 0.0 },
            )
        }
    }

    /// `+[NSEvent keyEventWithType:…]`, encoded
    /// `@96@0:8Q16{CGPoint=dd}24Q40d48q56@64@72@80B88S92`.
    unsafe fn key_event(kind: usize, win: isize, chars: Id, code: u16) -> Id {
        // SAFETY: as [`mouse_event`], against the signature above.
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
                sel!(keyEventWithType:location:modifierFlags:timestamp:windowNumber:context:characters:charactersIgnoringModifiers:isARepeat:keyCode:),
                kind,
                CGPoint { x: 0.0, y: 0.0 },
                0,
                0.0,
                win,
                Id::NIL,
                chars,
                chars,
                Bool::new(false),
                code,
            )
        }
    }

    const LEFT_DOWN: usize = 1;
    const LEFT_UP: usize = 2;
    const RIGHT_DOWN: usize = 3;
    const RIGHT_UP: usize = 4;
    const LEFT_DRAG: usize = 6;
    const MOUSE_MOVED: usize = 5;
    const KEY_DOWN: usize = 10;
    const KEY_UP: usize = 11;
    /// `NSEventModifierFlagControl`.
    const CTRL: usize = 1 << 18;

    /// How long a stage waits for macOS to make the drive the active app with
    /// its window key before it clicks anyway. Past it the clicks that need
    /// activation and fail read as not measured, never as findings.
    const FOCUS_WAIT: Duration = Duration::from_secs(10);

    /// An autoreleased `NSString`, for an event's `-characters`.
    fn nsstr(s: &str) -> Id {
        ns_string(s).map_or(Id::NIL, aterm_objc::Obj::autorelease)
    }

    fn name_of(id: Id) -> String {
        if id.is_null() {
            return "nil".to_owned();
        }
        // SAFETY: `id` is non-null here; `class_of` and `class_name` are
        // read-only runtime queries and the name is immortal.
        unsafe { class_name(class_of(id)) }
            .to_string_lossy()
            .into_owned()
    }

    /// Intern a class or protocol name known only as a `&str`.
    ///
    /// [`aterm_objc::protocol`] takes `&'static CStr`; the names in stage 10
    /// come from `class_protocols`. One small leak per name, bounded by four
    /// superclass chains in a process that drives one window and exits (as
    /// `objc_live_class_audit.rs` does).
    fn cstr_named(name: &str) -> &'static CStr {
        Box::leak(
            std::ffi::CString::new(name)
                .expect("a runtime-supplied name has no interior NUL")
                .into_boxed_c_str(),
        )
    }

    /// Round to a sixteenth of a point, so a layout figure is printable without
    /// a float's last bit deciding whether two transcripts differ.
    fn r4(v: f64) -> f64 {
        (v * 16.0).round() / 16.0
    }

    fn show_rect(r: CGRect) -> String {
        format!(
            "({:.2},{:.2} {:.2}x{:.2})",
            r4(r.origin.x),
            r4(r.origin.y),
            r4(r.size.width),
            r4(r.size.height)
        )
    }

    // ------------------------------------------------------------------ model

    fn meta(
        icon: Option<TabIconKind>,
        dirty: bool,
        busy: bool,
        conn: Option<TabConnRole>,
    ) -> TabStripMetadata {
        TabStripMetadata {
            icon,
            dirty,
            busy,
            attention: crate::tab_bar::ChipLevel::Off,
            conn,
            closable: true,
            team: crate::tab_bar::TabTeam::NONE,
        }
    }

    struct Model {
        titles: Vec<String>,
        ids: Vec<TabId>,
        metadata: Vec<TabStripMetadata>,
        tooltips: Vec<Option<String>>,
        ext: Vec<TabChromeExt>,
        active: usize,
    }

    fn model(n: usize) -> Model {
        let names = ["zsh", "vim", "htop", "cargo", "ssh"];
        let mut m = Model {
            titles: Vec::new(),
            ids: Vec::new(),
            metadata: Vec::new(),
            tooltips: Vec::new(),
            ext: Vec::new(),
            active: 1.min(n.saturating_sub(1)),
        };
        for i in 0..n {
            m.titles.push(names[i % names.len()].to_owned());
            m.ids.push(TabId::from_stored(i as u64 + 1));
            m.metadata.push(meta(
                if i == 0 {
                    Some(TabIconKind::Settings)
                } else {
                    None
                },
                i == 1,
                i == 2,
                if i == 2 {
                    Some(TabConnRole::Outbound)
                } else {
                    None
                },
            ));
            // The hover is the session chrome's tooltip with the presence lines
            // after it (`App::tab_tooltips`); the extension carries the bare one.
            m.tooltips
                .push(Some(format!("tip {i}\n{}", presence_line(i))));
            m.ext.push(TabChromeExt {
                tooltip: Some(format!("tip {i}")),
                menu: vec![
                    TabMenuEntry::Header(format!("session {i}")),
                    TabMenuEntry::Separator,
                    TabMenuEntry::Action {
                        label: "Close Tab".to_owned(),
                        action: crate::menu::MenuAction::About,
                        enabled: true,
                    },
                ],
            });
        }
        m
    }

    /// The presence line the model's hover for tab `i` ends in, which the
    /// extension's bare tooltip lacks.
    fn presence_line(i: usize) -> String {
        format!("last typed into by worker-{i} at 14:02")
    }

    /// Whether every chip's hover, as `chrome` reads it, is the strip's tooltip
    /// with its presence line, not the extension's bare one.
    fn hovers_keep_presence(handle: &toolbar::ToolbarHandle, n: usize) -> (bool, String) {
        let chrome = toolbar::read_tab_chrome(handle).unwrap_or_default();
        ((0..n).all(|i| chrome.contains(&presence_line(i))), chrome)
    }

    fn apply(handle: &toolbar::ToolbarHandle, m: &Model) {
        toolbar::set_window_tabs(
            handle,
            &m.titles,
            &m.ids,
            &m.metadata,
            &m.tooltips,
            &m.ext,
            m.active,
            None,
        );
    }

    // ----------------------------------------------------------------- driver

    struct Driver {
        window: Option<Window>,
        wakes: Vec<String>,
        blocked: Option<String>,
    }

    fn show_wake(w: &Wake) -> Option<String> {
        Some(match w {
            Wake::SelectTab { index, .. } => format!("SelectTab({index})"),
            Wake::CloseTab { index, .. } => format!("CloseTab({index})"),
            Wake::TabCmd { action, .. } => format!("TabCmd({action:?})"),
            Wake::TabContextMenuOpening { .. } => "TabContextMenuOpening".to_owned(),
            Wake::TabMenuAction { action, .. } => format!("TabMenuAction({action:?})"),
            Wake::MenuAction { action, .. } => format!("MenuAction({action:?})"),
            Wake::BeginSessionRename { tab, .. } => format!("BeginSessionRename({tab})"),
            Wake::CommitSessionRename { session, text, .. } => {
                format!("CommitSessionRename({session},{text:?})")
            }
            Wake::CancelSessionRename { session, .. } => format!("CancelSessionRename({session})"),
            _ => return None,
        })
    }

    impl ApplicationHandler<Wake> for Driver {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let attrs = Window::default_attributes()
                .with_title("objc-toolbar-drive")
                .with_inner_size(aterm_window::dpi::LogicalSize::new(1200.0, 400.0))
                .with_position(aterm_window::dpi::LogicalPosition::new(80.0, 80.0))
                .with_visible(true);
            match el.create_window(attrs) {
                Ok(w) => self.window = Some(w),
                Err(e) => {
                    self.blocked = Some(format!("no window: {e}"));
                    el.exit();
                }
            }
        }
        fn window_event(
            &mut self,
            _el: &ActiveEventLoop,
            _id: WinitWindowId,
            _e: aterm_window::event::WindowEvent,
        ) {
        }
        fn user_event(&mut self, _el: &ActiveEventLoop, w: Wake) {
            if let Some(s) = show_wake(&w) {
                self.wakes.push(s);
            }
        }
    }

    /// The findings ledger. `check` prints every question it asked, passing or
    /// failing: the transcript is what an A/B between two commits diffs.
    struct Ctx {
        findings: Vec<String>,
        /// Clicks that failed while another app held activation: they measured
        /// nothing about the strip (exit 2 when nothing else failed).
        unmeasured: Vec<String>,
    }
    impl Ctx {
        fn check(&mut self, ok: bool, msg: String) {
            println!("  {} {msg}", if ok { "ok  " } else { "FAIL" });
            if !ok {
                self.findings.push(msg);
            }
        }

        /// A click that reaches the strip only while the drive is the active app
        /// with its window key. `focused` says whether that held both before its
        /// events went in and after they were pumped; a failure without it is
        /// not a finding but a click spent on activation.
        fn check_focused(&mut self, focused: bool, ok: bool, msg: String) {
            if ok || focused {
                self.check(ok, msg);
            } else {
                println!("  NOT MEASURED {msg}: another app held activation");
                self.unmeasured.push(msg);
            }
        }
    }

    // ------------------------------------------------------------- tree walk

    /// The strip container, reached the way AppKit reaches it: window ->
    /// toolbar -> items -> item 0 -> view.
    ///
    /// Never `toolbar::native_strip_container`: if
    /// `toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:` were
    /// mis-registered, or the delegate stopped claiming `NSToolbarDelegate`,
    /// this returns nil and every later stage fails.
    unsafe fn strip_of(ns_window: Id) -> Id {
        // SAFETY: `ns_window` is a live `NSWindow`; every send is a read-only
        // accessor at the prototype its helper declares.
        unsafe {
            let tb = s_id(ns_window, sel!(toolbar));
            if tb.is_null() {
                return Id::NIL;
            }
            let items = s_id(tb, sel!(items));
            if items.is_null() || s_usize(items, sel!(count)) == 0 {
                return Id::NIL;
            }
            let item = s_id_usize(items, sel!(objectAtIndex:), 0);
            s_id(item, sel!(view))
        }
    }

    unsafe fn subviews(v: Id) -> Vec<Id> {
        // SAFETY: `v` is a live `NSView`; `-subviews` answers a live array.
        unsafe {
            let arr = s_id(v, sel!(subviews));
            if arr.is_null() {
                return Vec::new();
            }
            let n = s_usize(arr, sel!(count));
            (0..n)
                .map(|i| s_id_usize(arr, sel!(objectAtIndex:), i))
                .collect()
        }
    }

    /// Every tab chip, in x order. The `+` button is excluded by class name.
    unsafe fn chips(strip: Id) -> Vec<Id> {
        // SAFETY: `strip` is a live `NSView`; `-frame` is read-only.
        unsafe {
            let mut v: Vec<(f64, Id)> = subviews(strip)
                .into_iter()
                .filter(|s| name_of(*s).contains("TabView"))
                .map(|s| (s_rect(s, sel!(frame)).origin.x, s))
                .collect();
            v.sort_by(|a, b| a.0.total_cmp(&b.0));
            v.into_iter().map(|(_, s)| s).collect()
        }
    }

    unsafe fn plus_button(strip: Id) -> Option<Id> {
        // SAFETY: `strip` is a live `NSView`.
        unsafe { subviews(strip) }
            .into_iter()
            .find(|v| name_of(*v).contains("ChromeButton"))
    }

    /// Capture `view` through AppKit's `-cacheDisplayInRect:` pair and return
    /// `(w, h, bytes)`: what `drawRect:` actually put down, so a port that
    /// registered every encoding correctly and drew the wrong thing is caught.
    unsafe fn capture(view: Id) -> Option<(usize, usize, Vec<u8>)> {
        // SAFETY: `view` is a live `NSView` on the main thread; the two sends
        // are the documented caching pair and the rep's accessors are
        // read-only. `bitmapData` is valid while `rep` is, which is for the
        // rest of this autorelease scope.
        unsafe {
            let b = s_rect(view, sel!(bounds));
            if !(b.size.width > 1.0 && b.size.height > 1.0) {
                return None;
            }
            let rep = s_id_rect(view, sel!(bitmapImageRepForCachingDisplayInRect:), b);
            if rep.is_null() {
                return None;
            }
            s_v_rect_id(view, sel!(cacheDisplayInRect:toBitmapImageRep:), b, rep);
            let w = usize::try_from(s_isize(rep, sel!(pixelsWide))).unwrap_or(0);
            let h = usize::try_from(s_isize(rep, sel!(pixelsHigh))).unwrap_or(0);
            let stride = usize::try_from(s_isize(rep, sel!(bytesPerRow))).unwrap_or(0);
            let spp = usize::try_from(s_isize(rep, sel!(samplesPerPixel))).unwrap_or(0);
            let data = s_bytes(rep, sel!(bitmapData));
            if data.is_null() || w == 0 || h == 0 || stride == 0 {
                return None;
            }
            let row_len = stride.min(w * spp.max(1));
            let mut out = Vec::with_capacity(w * h * 4);
            for y in 0..h {
                out.extend_from_slice(std::slice::from_raw_parts(data.add(y * stride), row_len));
            }
            Some((w, h, out))
        }
    }

    /// Per-column ink profile: how many non-transparent samples each x column
    /// holds. It localises a pixel difference to a column band (e.g. "the label
    /// moved 60 points right").
    fn ink_columns(w: usize, h: usize, px: &[u8]) -> Vec<u32> {
        let spp = px.len().checked_div(w * h).unwrap_or(4).max(1);
        let mut cols = vec![0u32; w];
        for y in 0..h {
            for (x, col) in cols.iter_mut().enumerate() {
                let i = (y * w + x) * spp;
                if i + spp > px.len() {
                    continue;
                }
                let a = if spp >= 4 { px[i + 3] } else { 255 };
                if a > 8 {
                    *col += 1;
                }
            }
        }
        cols
    }

    fn spans(cols: &[u32]) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut start: Option<usize> = None;
        for (i, c) in cols.iter().enumerate() {
            match (start, *c > 0) {
                (None, true) => start = Some(i),
                (Some(s), false) => {
                    out.push((s, i - 1));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            out.push((s, cols.len() - 1));
        }
        out
    }

    fn show_spans(cols: &[u32]) -> String {
        spans(cols)
            .iter()
            .map(|(a, b)| format!("{a}..{b}"))
            .collect::<Vec<_>>()
            .join(",")
    }

    // --------------------------------------------------- the registered rows

    /// Every protocol reachable from `seed`, following protocol inheritance
    /// (`NSTextFieldDelegate` inherits `NSControlTextEditingDelegate`, where two
    /// of the three rename rows are declared).
    fn expand_protocols(into: &mut Vec<String>, seed: impl IntoIterator<Item = String>) {
        for name in seed {
            let mut stack = vec![name];
            while let Some(next) = stack.pop() {
                if into.contains(&next) {
                    continue;
                }
                stack.extend(protocol_parents(protocol(cstr_named(&next))));
                into.push(next);
            }
        }
    }

    /// The protocols a protocol itself inherits. `protocol_copyProtocolList` has
    /// no `aterm-objc` wrapper (no shipping call site needs one), so the
    /// prototype is declared here.
    fn protocol_parents(proto: aterm_objc::ProtocolPtr) -> Vec<String> {
        unsafe extern "C" {
            fn protocol_copyProtocolList(
                proto: aterm_objc::ProtocolPtr,
                count: *mut std::ffi::c_uint,
            ) -> *mut aterm_objc::ProtocolPtr;
            fn protocol_getName(proto: aterm_objc::ProtocolPtr) -> *const std::ffi::c_char;
            fn free(ptr: *mut std::ffi::c_void);
        }
        if proto.is_null() {
            return Vec::new();
        }
        let mut count: std::ffi::c_uint = 0;
        // SAFETY: `proto` is live; the runtime writes the count through the
        // pointer and hands back a malloc'd array this function owns.
        let list = unsafe { protocol_copyProtocolList(proto, &raw mut count) };
        if list.is_null() {
            return Vec::new();
        }
        let mut names = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            // SAFETY: `i < count`; protocol objects and their names are
            // immortal.
            let name = unsafe { CStr::from_ptr(protocol_getName(*list.add(i))) };
            names.push(name.to_string_lossy().into_owned());
        }
        // SAFETY: the array is this function's to free; the immortal protocol
        // objects it held are not touched.
        unsafe { free(list.cast()) };
        names
    }

    /// Where a row's shape is written down outside this tree, and what that
    /// authority says.
    ///
    /// Asked in order: every protocol the class claims (expanded through
    /// inheritance), then the superclass chain, where an override's original
    /// lives. `None` means nothing outside this tree declares the selector (a
    /// first-party action such as `closeTab:`); such rows are printed.
    unsafe fn authority_for(cls: ClassPtr, s: Sel) -> Option<(String, String)> {
        // SAFETY: `cls` is a live class object.
        let mut protos: Vec<String> = Vec::new();
        expand_protocols(&mut protos, unsafe { class_protocols(cls) });
        for p in &protos {
            // SAFETY: `protocol` answers a live protocol or nil, which
            // `protocol_method_types` tolerates.
            if let Some(t) = unsafe { protocol_method_types(protocol(cstr_named(p)), s, true) } {
                return Some((format!("protocol {p}"), strip_method_offsets(&t)));
            }
        }
        // SAFETY: `superclass_of` tolerates and terminates at nil.
        let mut walk = unsafe { superclass_of(cls) };
        while !walk.is_null() {
            // SAFETY: `walk` is a live class while non-null.
            if let Some(t) = unsafe { method_types(walk, s) } {
                // SAFETY: as above; the name is immortal.
                let n = unsafe { class_name(walk) }.to_string_lossy().into_owned();
                return Some((format!("class {n}"), strip_method_offsets(&t)));
            }
            // SAFETY: as above.
            walk = unsafe { superclass_of(walk) };
        }
        None
    }

    /// One declared class, audited off a LIVE instance.
    struct Declared {
        /// The instance this class was read from, and how it was found.
        found_by: &'static str,
        /// The name `declare_class!` registered, used only to ask
        /// `objc_getClass` whether the live object's class is that class.
        name: &'static CStr,
        cls: ClassPtr,
        /// A protocol the class must conform to, if its rows come from one; an
        /// encoding check cannot see a class that stopped claiming it.
        conforms: Option<&'static CStr>,
    }

    /// Read every registered row of `d`'s class and check it against whatever
    /// the runtime says it should be.
    fn audit_declared(cx: &mut Ctx, d: &Declared) {
        println!("  --  {} found by {}", d.name.to_string_lossy(), d.found_by);
        if d.cls.is_null() {
            cx.check(false, format!("{:?} was not live", d.name));
            return;
        }
        // SAFETY: `d.cls` came from `class_of` on a live instance.
        let live_name = unsafe { class_name(d.cls) };
        cx.check(
            live_name == d.name,
            format!(
                "the live object's class is {:?} (runtime says {live_name:?})",
                d.name
            ),
        );
        cx.check(
            d.cls == class(d.name),
            format!(
                "{:?} on the live object IS the class objc_getClass answers",
                d.name
            ),
        );
        // SAFETY: `d.cls` is live.
        let sup = unsafe { class_name(superclass_of(d.cls)) };
        println!("      superclass {}", sup.to_string_lossy());
        if let Some(p) = d.conforms {
            let proto = protocol(p);
            // Registered by the host's AppKit, not supplied by aterm, so the
            // conformance below is about something AppKit knows.
            let hosts =
                !proto.is_null() && !aterm_objc::protocols_registered_by_aterm().contains(&p);
            cx.check(hosts, format!("{p:?} is registered by this host's AppKit"));
            // SAFETY: `+conformsToProtocol:` is a read-only `NSObject` query
            // on a live class object.
            let ok = unsafe {
                s_bool_id(
                    d.cls.as_id(),
                    sel!(conformsToProtocol:),
                    Id::from_ptr(proto.as_ptr()),
                )
            };
            cx.check(
                ok,
                format!("{:?} claims {p:?} — the class SAYS what it is", d.name),
            );
        }
        // SAFETY: `d.cls` is live.
        let mut rows = unsafe { class_methods(d.cls) };
        rows.sort_by_key(|(s, _)| format!("{s:?}"));
        let mut unauthoritative = Vec::new();
        for (s, enc) in &rows {
            let Some(enc) = enc else {
                cx.check(false, format!("{s:?} registered with NO type string"));
                continue;
            };
            let got = strip_method_offsets(enc);
            // SAFETY: `d.cls` is live and `s` is interned.
            match unsafe { authority_for(d.cls, *s) } {
                Some((src, want)) => {
                    if got == want {
                        println!("      {s:<52?} {got:<20} = {src}");
                    } else {
                        cx.check(
                            false,
                            format!("{s:?} registers {got} but {src} says {want}"),
                        );
                    }
                }
                None => {
                    println!("      {s:<52?} {got:<20} (first-party action: no authority)");
                    unauthoritative.push(format!("{s:?}"));
                }
            }
        }
        println!(
            "      {} row(s); {} with no authority outside this tree: {}",
            rows.len(),
            unauthoritative.len(),
            unauthoritative.join(" ")
        );
    }

    // -------------------------------------------------------------------- run

    #[expect(
        clippy::too_many_lines,
        reason = "one linear drive of one window: every stage depends on the \
                  live objects the stage before it found, and splitting it \
                  would thread a dozen raw `Id`s through signatures that say \
                  nothing"
    )]
    pub(crate) fn run(_windowed: crate::platform::Windowed) -> i32 {
        // The watchdog: this driver enters `-mouseDown:`/`-rightMouseDown:`
        // IMPs directly, and a context menu that popped would run a modal
        // tracking loop that never returns. A hang gets its own exit code.
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(180));
            crate::logging::stderr_line!(
                "WATCHDOG: the drive hung (a modal tracking loop never returned)"
            );
            std::process::exit(HUNG);
        });

        // The quiet builder starts inactive. Stage 3 asks to be the active app
        // with its window key, which its clicks and stage 4's drag need, and
        // stage 5 asks again for AppKit's own routing: an inactive app's click
        // is consumed as an activation click (`Ctx::check_focused`).
        let mut el = match crate::quiet_driver_event_loop_builder::<Wake>().build() {
            Ok(el) => el,
            Err(e) => {
                crate::logging::stderr_line!("objc-toolbar-drive: NOT RUN — no event loop: {e}");
                return NOT_RUN;
            }
        };
        let proxy = el.create_proxy();
        let mut d = Driver {
            window: None,
            wakes: Vec::new(),
            blocked: None,
        };
        let start = Instant::now();
        while d.window.is_none() && start.elapsed() < Duration::from_secs(20) {
            if let PumpStatus::Exit(_) = el.pump_app_events(Some(Duration::from_millis(8)), &mut d)
            {
                break;
            }
        }
        let Some(win) = d.window.as_ref() else {
            crate::logging::stderr_line!(
                "objc-toolbar-drive: NOT RUN — {}",
                d.blocked.unwrap_or_else(|| "no window".to_owned())
            );
            return NOT_RUN;
        };
        let Some(handle) = toolbar::install_window_toolbar(win, &proxy, WindowId(1)) else {
            crate::logging::stderr_line!(
                "objc-toolbar-drive: NOT RUN — install_window_toolbar returned None"
            );
            return NOT_RUN;
        };
        let ns_view = crate::platform::ns_view_of(win);
        if ns_view.is_null() {
            crate::logging::stderr_line!("objc-toolbar-drive: NOT RUN — no content view");
            return NOT_RUN;
        }
        // SAFETY: `ns_view` is winit's live content view; both accessors are
        // read-only.
        let ns_window = unsafe { s_id(ns_view, sel!(window)) };
        // SAFETY: as above.
        let win_no = unsafe { s_isize(ns_window, sel!(windowNumber)) };
        let pump = |d: &mut Driver, el: &mut EventLoop<Wake>, ms: u64| {
            let t = Instant::now();
            while t.elapsed() < Duration::from_millis(ms) {
                let _ = el.pump_app_events(Some(Duration::from_millis(4)), d);
            }
        };

        let m3 = model(3);
        apply(&handle, &m3);
        pump(&mut d, &mut el, 250);

        let mut cx = Ctx {
            findings: Vec::new(),
            unmeasured: Vec::new(),
        };
        println!("=== objc-toolbar-drive ===");

        // ---------------------------------------------------------- stage 1
        println!("\n-- stage 1: the strip AppKit is holding");
        // SAFETY: `ns_window` is live.
        let strip = unsafe { strip_of(ns_window) };
        cx.check(
            !strip.is_null(),
            format!("toolbar item 0 has a view: {}", name_of(strip)),
        );
        if strip.is_null() {
            // Nothing below means anything, and an empty toolbar is what a
            // delegate that stopped claiming `NSToolbarDelegate` leaves: a
            // finding, not a could-not-run.
            println!("objc-toolbar-drive: 1 FINDING(S)");
            return FINDING;
        }
        // SAFETY: `strip` is live.
        let strip_frame = unsafe { s_rect(strip, sel!(frame)) };
        println!(
            "  --  strip class={} frame={}",
            name_of(strip),
            show_rect(strip_frame)
        );
        // SAFETY: as above.
        let subs = unsafe { subviews(strip) };
        println!(
            "  --  {} subviews: {}",
            subs.len(),
            subs.iter()
                .map(|s| name_of(*s))
                .collect::<Vec<_>>()
                .join(" ")
        );
        // SAFETY: as above.
        let cs = unsafe { chips(strip) };
        cx.check(
            cs.len() == 3,
            format!("3 tab chips are live (got {})", cs.len()),
        );
        if cs.len() != 3 {
            for f in &cx.findings {
                println!("  FINDING: {f}");
            }
            println!("objc-toolbar-drive: {} FINDING(S)", cx.findings.len());
            return FINDING;
        }
        for (i, c) in cs.iter().enumerate() {
            // SAFETY: each chip is a live `NSView`.
            unsafe {
                println!(
                    "  --  chip{i} {} frame={} subviews={}",
                    name_of(*c),
                    show_rect(s_rect(*c, sel!(frame))),
                    subviews(*c).len()
                );
            }
        }
        println!("  --  chrome: {:?}", toolbar::read_tab_chrome(&handle));
        let (kept, chrome) = hovers_keep_presence(&handle, 3);
        cx.check(
            kept,
            format!("each built chip's hover keeps its presence line: {chrome}"),
        );

        // ---------------------------------------------------------- stage 2
        println!("\n-- stage 2: AppKit draws the strip (cacheDisplayInRect:)");
        // SAFETY: `strip` is a live view on the main thread.
        if let Some((w, h, px)) = unsafe { capture(strip) } {
            let cols = ink_columns(w, h, &px);
            let inked = cols.iter().filter(|c| **c > 0).count();
            println!(
                "  --  {w}x{h} hash={:016x} ink_cols={inked}",
                aterm_hash::fnv1a_64(&px)
            );
            println!("  --  ink spans: {}", show_spans(&cols));
            cx.check(inked > 40, format!("the strip drew ink in {inked} columns"));
        } else {
            cx.check(false, "cacheDisplayInRect: produced no bitmap".to_owned());
        }

        // ---------------------------------------------------------- stage 3
        println!("\n-- stage 3: a left click on each hit-tested chip view");
        // SAFETY: `+sharedApplication` on a linked class; later keyboard
        // events still enter through NSApp's event dispatch.
        let app = unsafe { s_id(class(c"NSApplication").as_id(), sel!(sharedApplication)) };
        // Whether AppKit routes a click to this drive's window: the app active
        // and the window key. Another app's activation takes both.
        // SAFETY: the live NSApp and the drive's own window; `-isActive` and
        // `-isKeyWindow` are read-only `B16@0:8` getters.
        let focused =
            || unsafe { s_bool(app, sel!(isActive)) && s_bool(ns_window, sel!(isKeyWindow)) };
        // Asks to be the active app with the window key and pumps until macOS
        // has granted it or `FOCUS_WAIT` has passed; macOS may refuse while a
        // person is using another app.
        let take_focus = |d: &mut Driver, el: &mut EventLoop<Wake>| {
            // SAFETY: the live NSApp and the live window this drive built.
            unsafe {
                s_v_bool(app, sel!(activateIgnoringOtherApps:), true);
                s_v_id(ns_window, sel!(makeKeyAndOrderFront:), Id::NIL);
            }
            let t = Instant::now();
            while !focused() && t.elapsed() < FOCUS_WAIT {
                let _ = el.pump_app_events(Some(Duration::from_millis(4)), d);
            }
            println!(
                "  --  asked for activation: granted={} after {:.2}s",
                focused(),
                t.elapsed().as_secs_f64()
            );
        };
        // Active and key first, as stage 5 does. The drive launches under the
        // quiet posture (Accessory, no activation); this stage audits tab
        // selection by click, which needs the key window: otherwise the click
        // hit-tests to the chip's label (`NSTextField`, no first mouse) and
        // AppKit spends it on key-window selection.
        take_focus(&mut d, &mut el);
        pump(&mut d, &mut el, 250);
        for (i, c) in cs.iter().enumerate() {
            d.wakes.clear();
            let before = focused();
            // SAFETY: `c` and `strip` are live views, `ns_window` is live;
            // the synthesized events are +0 autoreleased `NSEvent`s.
            unsafe {
                let b = s_rect(*c, sel!(bounds));
                let mid = CGPoint {
                    x: b.size.width * 0.35,
                    y: b.size.height * 0.5,
                };
                let wp = s_point_point_id(*c, sel!(convertPoint:toView:), mid, Id::NIL);
                let lp = s_point_point_id(*c, sel!(convertPoint:toView:), mid, strip);
                let hit = s_id_point(strip, sel!(hitTest:), lp);
                s_v_id(
                    hit,
                    sel!(mouseDown:),
                    mouse_event(LEFT_DOWN, wp, 0, win_no, 1),
                );
                s_v_id(hit, sel!(mouseUp:), mouse_event(LEFT_UP, wp, 0, win_no, 1));
                println!("  --  chip{i} hitTest -> {}", name_of(hit));
            }
            pump(&mut d, &mut el, 120);
            cx.check_focused(
                before && focused(),
                d.wakes.iter().any(|w| w == &format!("SelectTab({i})")),
                format!("chip{i} click posted SelectTab({i}); saw {:?}", d.wakes),
            );
        }

        // ---------------------------------------------------------- stage 4
        println!("\n-- stage 4: drag chip0 across chip2 (down, 6 drags, up)");
        d.wakes.clear();
        let before = focused();
        // SAFETY: live views and NSWindow, as stage 3.
        unsafe {
            let b0 = s_rect(cs[0], sel!(bounds));
            let p0 = s_point_point_id(
                cs[0],
                sel!(convertPoint:toView:),
                CGPoint {
                    x: b0.size.width * 0.5,
                    y: b0.size.height * 0.5,
                },
                Id::NIL,
            );
            let b2 = s_rect(cs[2], sel!(bounds));
            let p2 = s_point_point_id(
                cs[2],
                sel!(convertPoint:toView:),
                CGPoint {
                    x: b2.size.width * 0.6,
                    y: b2.size.height * 0.5,
                },
                Id::NIL,
            );
            s_v_id(
                ns_window,
                sel!(sendEvent:),
                mouse_event(LEFT_DOWN, p0, 0, win_no, 1),
            );
            for k in 1..=6 {
                let t = f64::from(k) / 6.0;
                let p = CGPoint {
                    x: p0.x + (p2.x - p0.x) * t,
                    y: p0.y,
                };
                s_v_id(
                    ns_window,
                    sel!(sendEvent:),
                    mouse_event(LEFT_DRAG, p, 0, win_no, 0),
                );
            }
            s_v_id(
                ns_window,
                sel!(sendEvent:),
                mouse_event(LEFT_UP, p2, 0, win_no, 1),
            );
        }
        pump(&mut d, &mut el, 200);
        cx.check_focused(
            before && focused(),
            d.wakes.iter().any(|w| w.starts_with("TabCmd(Move")),
            format!("the drag posted a reorder; saw {:?}", d.wakes),
        );

        // ---------------------------------------------------------- stage 5
        println!("\n-- stage 5: the context menu, two ways in");
        // The pop site goes first (see `POP_UP_CONTEXT_MENU`). A class method is
        // an instance method of the metaclass, so the row is claimed there;
        // `install` refuses unless the registered encoding is the stub's.
        // SAFETY: `NSMenu` is a live, immortal class object; `class_of` on it
        // answers its metaclass.
        let meta = unsafe { aterm_objc::class_of(class(c"NSMenu").as_id()) };
        // SAFETY: the stub has exactly the row's prototype and cannot unwind;
        // NSMenu's metaclass is live and the selector is interned.
        let pop_site_replaced = match unsafe {
            POP_UP_CONTEXT_MENU.install(
                meta,
                sel!(popUpContextMenu:withEvent:forView:),
                swallow_pop_up,
            )
        } {
            Ok(_) => {
                println!(
                    "  --  +[NSMenu popUpContextMenu:withEvent:forView:] replaced for this \
                     process: pops are recorded, never tracked"
                );
                true
            }
            Err(e) => {
                cx.check(
                    false,
                    format!(
                        "the pop site could not be replaced ({e:?}); no menu path is safe to drive"
                    ),
                );
                false
            }
        };
        let menus = toolbar::read_tab_menus(&handle);
        println!("  --  read_tab_menus: {} line(s)", menus.len());
        for l in &menus {
            println!("      {l}");
        }
        // NSApp's real event routing needs the application active and this
        // window key; otherwise the first click is consumed as an activation
        // click. Asked again here: another app may have taken it since stage 3.
        take_focus(&mut d, &mut el);
        pump(&mut d, &mut el, 250);
        // Exercise real NSApp dispatch for every chip as well as stage 3's
        // direct-responder checks. These are plain left clicks; context-menu
        // dispatch below remains guarded by a successful pop-site replacement.
        for (i, c) in cs.iter().enumerate() {
            d.wakes.clear();
            let before = focused();
            // SAFETY: `c` is a live view, `app` is the live NSApp, and the
            // synthesized events are +0 autoreleased `NSEvent`s.
            unsafe {
                let b = s_rect(*c, sel!(bounds));
                let mid = CGPoint {
                    x: b.size.width * 0.35,
                    y: b.size.height * 0.5,
                };
                let wp = s_point_point_id(*c, sel!(convertPoint:toView:), mid, Id::NIL);
                s_v_id(
                    app,
                    sel!(sendEvent:),
                    mouse_event(LEFT_DOWN, wp, 0, win_no, 1),
                );
                s_v_id(
                    app,
                    sel!(sendEvent:),
                    mouse_event(LEFT_UP, wp, 0, win_no, 1),
                );
            }
            pump(&mut d, &mut el, 120);
            cx.check_focused(
                before && focused(),
                d.wakes.iter().any(|w| w == &format!("SelectTab({i})")),
                format!(
                    "chip{i} NSApp click posted SelectTab({i}); saw {:?}",
                    d.wakes
                ),
            );
        }
        // The block below enters the strip's menu, through AppKit's routing
        // and straight at the IMPs, so it runs only over the stub; over the
        // real pop site it could hang in the tracking loop (exit 3) instead of
        // failing with the recorded reason. Its other observations (the control
        // click, the hit map) are skipped with it.
        if pop_site_replaced {
            // SAFETY: live views, NSApp and NSWindow throughout this block; every
            // synthesized event is +0 autoreleased.
            unsafe {
                let b = s_rect(cs[1], sel!(bounds));
                let mid = CGPoint {
                    x: b.size.width * 0.35,
                    y: b.size.height * 0.5,
                };
                let wp = s_point_point_id(cs[1], sel!(convertPoint:toView:), mid, Id::NIL);
                let lp = s_point_point_id(cs[1], sel!(convertPoint:toView:), mid, strip);
                let ctrl_ev = mouse_event(LEFT_DOWN, wp, CTRL, win_no, 1);
                let right_ev = mouse_event(RIGHT_DOWN, wp, 0, win_no, 1);
                println!(
                    "  --  hitTest at the press point -> {}",
                    name_of(s_id_point(strip, sel!(hitTest:), lp))
                );

                // App and window are active/key for the real NSApp routing
                // control below; the view's responder was checked in stage 3.
                println!(
                    "  --  app active={} window key={} main={}",
                    s_bool(app, sel!(isActive)),
                    s_bool(ns_window, sel!(isKeyWindow)),
                    s_bool(ns_window, sel!(isMainWindow))
                );

                // The control: a plain click through the same NSApp routing as
                // the context clicks must select before those observations count.
                // The pop site is stubbed, so no route enters a tracking loop.
                d.wakes.clear();
                let before = focused();
                s_v_id(
                    app,
                    sel!(sendEvent:),
                    mouse_event(LEFT_DOWN, wp, 0, win_no, 1),
                );
                s_v_id(
                    app,
                    sel!(sendEvent:),
                    mouse_event(LEFT_UP, wp, 0, win_no, 1),
                );
                pump(&mut d, &mut el, 200);
                let control_arrived = d.wakes.iter().any(|w| w == "SelectTab(1)");
                println!("  --  [NSApp sendEvent:] plain left -> {:?}", d.wakes);
                cx.check_focused(
                    before && focused(),
                    control_arrived,
                    "the CONTROL arrived: NSApp routes a plain left click".to_owned(),
                );

                d.wakes.clear();
                let before = pops();
                s_v_id(app, sel!(sendEvent:), ctrl_ev);
                s_v_id(
                    app,
                    sel!(sendEvent:),
                    mouse_event(LEFT_UP, wp, CTRL, win_no, 1),
                );
                pump(&mut d, &mut el, 250);
                println!(
                    "  --  [NSApp sendEvent:] ctrl-left    -> {:?}  (pops routed: {})",
                    d.wakes,
                    pops() - before
                );

                d.wakes.clear();
                let before = pops();
                s_v_id(app, sel!(sendEvent:), right_ev);
                s_v_id(
                    app,
                    sel!(sendEvent:),
                    mouse_event(RIGHT_UP, wp, 0, win_no, 1),
                );
                pump(&mut d, &mut el, 250);
                println!(
                    "  --  [NSApp sendEvent:] rightMouse   -> {:?}  (pops routed: {})",
                    d.wakes,
                    pops() - before
                );

                let frame_view = s_id(s_id(ns_window, sel!(contentView)), sel!(superview));
                println!(
                    "  --  theme frame={} hitTest(window pt) -> {}",
                    name_of(frame_view),
                    name_of(s_id_point(frame_view, sel!(hitTest:), wp))
                );

                // Straight into the registered IMP, which AppKit calls once its
                // routing has picked the view. Nothing is queued to dismiss the
                // menu: the pop lands in the stub, and a posted ESC would reach
                // the key window as a stray Escape keyDown.
                for (label, recv, s, ev) in [
                    (
                        "chip rightMouseDown:",
                        cs[1],
                        sel!(rightMouseDown:),
                        right_ev,
                    ),
                    ("chip mouseDown: ctrl", cs[1], sel!(mouseDown:), ctrl_ev),
                ] {
                    d.wakes.clear();
                    let before = pops();
                    s_v_id(recv, s, ev);
                    pump(&mut d, &mut el, 300);
                    println!(
                        "  --  [{label}] -> {:?}  (pops: {}, items in the last: {})",
                        d.wakes,
                        pops() - before,
                        LAST_MENU_ITEMS.load(std::sync::atomic::Ordering::Relaxed)
                    );
                    cx.check(
                        d.wakes.iter().any(|w| w == "TabContextMenuOpening"),
                        format!("[{label}] pops the tab context menu"),
                    );
                    cx.check(
                        pops() > before,
                        format!("[{label}] reached +[NSMenu popUpContextMenu:withEvent:forView:]"),
                    );
                }

                // Where a click lands across the chip (no pumping needed): the
                // label owns the middle, the chip owns the edges.
                let bb = s_rect(cs[1], sel!(bounds));
                let map: Vec<String> = (0..20)
                    .map(|k| {
                        let fx = (f64::from(k) + 0.5) / 20.0;
                        let p = s_point_point_id(
                            cs[1],
                            sel!(convertPoint:toView:),
                            CGPoint {
                                x: bb.size.width * fx,
                                y: bb.size.height * 0.5,
                            },
                            strip,
                        );
                        name_of(s_id_point(strip, sel!(hitTest:), p))
                    })
                    .collect();
                println!("  --  hit map across chip1: {}", map.join(" "));

                // And the label, which the user's pointer is actually over:
                // `NSTextField` is not a chip, so both menu routes must survive
                // the responder walk from the label up to the chip.
                let label = subviews(cs[1])
                    .into_iter()
                    .find(|v| name_of(*v) == "NSTextField");
                if let Some(lab) = label {
                    println!(
                        "  --  label frame={} menuForEvent -> {}",
                        show_rect(s_rect(lab, sel!(frame))),
                        name_of(s_id_id(lab, sel!(menuForEvent:), right_ev))
                    );
                    for (label_txt, s, ev) in [
                        ("label rightMouseDown:", sel!(rightMouseDown:), right_ev),
                        ("label mouseDown: ctrl", sel!(mouseDown:), ctrl_ev),
                    ] {
                        d.wakes.clear();
                        s_v_id(lab, s, ev);
                        pump(&mut d, &mut el, 300);
                        println!("  --  [{label_txt}] -> {:?}", d.wakes);
                        cx.check(
                            d.wakes.iter().any(|w| w == "TabContextMenuOpening"),
                            format!("[{label_txt}] reaches the tab's menu"),
                        );
                    }
                } else {
                    cx.check(false, "chip1 has an NSTextField label".to_owned());
                }
            }
        } else {
            println!("  --  stage 5's menu entries NOT driven: the pop site is still AppKit's");
        }

        // ---------------------------------------------------------- stage 6
        println!("\n-- stage 6: a real resize, then the strip's own re-layout");
        // SAFETY: `strip` is live; `plus_button` walks its subviews.
        let plus_before = unsafe { plus_button(strip).map(|p| s_rect(p, sel!(frame))) };
        // SAFETY: as above.
        let strip_w0 = unsafe { s_rect(strip, sel!(bounds)) }.size.width;
        // SAFETY: `ns_window` is live; `-setFrame:display:` is the documented
        // resize.
        unsafe {
            let f = s_rect(ns_window, sel!(frame));
            s_v_rect_bool(
                ns_window,
                sel!(setFrame:display:),
                CGRect {
                    origin: f.origin,
                    size: CGSize {
                        width: 820.0,
                        height: f.size.height,
                    },
                },
                true,
            );
        }
        pump(&mut d, &mut el, 250);
        // SAFETY: `strip` is live.
        let strip_w1 = unsafe { s_rect(strip, sel!(bounds)) }.size.width;
        cx.check(
            strip_w1 < strip_w0 - 100.0,
            format!("the strip followed the window: {strip_w0:.1} -> {strip_w1:.1}"),
        );
        // SAFETY: as above.
        let plus_mid = unsafe { plus_button(strip).map(|p| s_rect(p, sel!(frame))) };
        if let (Some(a), Some(b)) = (plus_before, plus_mid) {
            let gap_a = strip_w0 - (a.origin.x + a.size.width);
            let gap_b = strip_w1 - (b.origin.x + b.size.width);
            cx.check(
                (gap_a - gap_b).abs() < 1.0,
                format!("the \"+\" stayed right-anchored across the resize: gap {gap_a:.2} -> {gap_b:.2}"),
            );
        } else {
            cx.check(
                false,
                "the \"+\" button is live before and after".to_owned(),
            );
        }
        // The chips follow the edge too: `reflow_window_tabs` runs in the
        // resize's own turn, so a narrowed window does not keep the wide layout
        // with the last chip under the "+".
        toolbar::reflow_window_tabs(&handle);
        pump(&mut d, &mut el, 100);
        // SAFETY: `strip` is live; `chips` walks its subviews.
        let flowed: Vec<CGRect> = unsafe { chips(strip) }
            .into_iter()
            // SAFETY: live views.
            .map(|c| unsafe { s_rect(c, sel!(frame)) })
            .collect();
        // SAFETY: `strip` is live.
        let plus_x = unsafe { plus_button(strip).map(|p| s_rect(p, sel!(frame)).origin.x) };
        let last_right = flowed.last().map(|f| f.origin.x + f.size.width);
        cx.check(
            flowed.len() == 3
                && matches!((last_right, plus_x), (Some(r), Some(p)) if r <= p + 0.5),
            format!(
                "the resize's re-flow keeps every chip left of the \"+\": {} chips, last ends at {last_right:?}, \"+\" at {plus_x:?}",
                flowed.len()
            ),
        );
        apply(&handle, &m3);
        pump(&mut d, &mut el, 200);
        let (kept, chrome) = hovers_keep_presence(&handle, 3);
        cx.check(
            kept,
            format!("each chip's hover keeps its presence line after a re-apply: {chrome}"),
        );
        // SAFETY: `strip` is live.
        let cs2 = unsafe { chips(strip) };
        cx.check(
            cs2.len() == 3,
            format!("3 chips after the re-layout (got {})", cs2.len()),
        );
        // SAFETY: live views.
        let relaid: Vec<CGRect> = cs2
            .iter()
            .map(|c| unsafe { s_rect(*c, sel!(frame)) })
            .collect();
        cx.check(
            relaid.len() == flowed.len()
                && relaid.iter().zip(&flowed).all(|(a, b)| {
                    a.origin.x == b.origin.x
                        && a.size.width == b.size.width
                        && a.size.height == b.size.height
                }),
            format!(
                "the re-flow landed on the refresh's own cells: {} vs {}",
                flowed
                    .iter()
                    .map(|f| show_rect(*f))
                    .collect::<Vec<_>>()
                    .join(" "),
                relaid
                    .iter()
                    .map(|f| show_rect(*f))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        );
        for (i, c) in cs2.iter().enumerate() {
            // SAFETY: live views.
            unsafe { println!("  --  chip{i} frame={}", show_rect(s_rect(*c, sel!(frame)))) }
        }
        // SAFETY: `strip` is live.
        if let Some((w, h, px)) = unsafe { capture(strip) } {
            let cols = ink_columns(w, h, &px);
            println!(
                "  --  narrow {w}x{h} hash={:016x}",
                aterm_hash::fnv1a_64(&px)
            );
            println!("  --  ink spans: {}", show_spans(&cols));
        }

        // ---------------------------------------------------------- stage 7
        //
        // The rename editor. `control:textView:doCommandBySelector:` takes a
        // `SEL` and returns a `BOOL`, whose one-byte-vs-eight-byte failure mode
        // is silent on arm64. Only a real Escape typed into a real field editor
        // reaches that row, and only a real Return reaches its commit twin, so
        // this stage types both. Opening the editor must not end the editing
        // session (see the relay attach in `toolbar.rs`).
        println!("\n-- stage 7: the inline rename editor, typed through AppKit's field editor");
        cx.check(
            toolbar::can_present_tab_rename(&handle),
            "can_present_tab_rename".to_owned(),
        );
        let mut rename_target_cls = ClassPtr::NULL;
        d.wakes.clear();
        cx.check(
            toolbar::begin_tab_rename(&handle, m3.ids[1], 77, "vim", "name"),
            "begin_tab_rename".to_owned(),
        );
        pump(&mut d, &mut el, 200);
        // Opening an editor decides nothing, so it must post nothing: a commit
        // here is an unrequested rename and a latch that kills every later exit.
        cx.check(
            d.wakes.is_empty(),
            format!("opening the editor posted no outcome; saw {:?}", d.wakes),
        );
        // SAFETY: `ns_window` is live; `-firstResponder` is read-only.
        let fr = unsafe { s_id(ns_window, sel!(firstResponder)) };
        println!("  --  firstResponder after begin: {}", name_of(fr));
        cx.check(
            name_of(fr).contains("Text") || name_of(fr).contains("Field"),
            format!("the field editor took key focus (got {})", name_of(fr)),
        );
        cx.check(
            toolbar::rename_editor_text(&handle).as_deref() == Some("vim"),
            format!(
                "the seed is in the field: {:?}",
                toolbar::rename_editor_text(&handle)
            ),
        );
        // The field editor's delegate is the NSTextField control, whose own
        // delegate is the declared `ATermTabRenameTarget`; this is the only
        // moment that object is live, so stage 10 reads it now.
        // SAFETY: `fr` is a live responder while the editor is up; both
        // `-delegate` sends are read-only accessors.
        unsafe {
            let ctl = s_id(fr, sel!(delegate));
            let tgt = if ctl.is_null() {
                Id::NIL
            } else {
                s_id(ctl, sel!(delegate))
            };
            println!(
                "  --  field editor delegate={} -> its delegate={}",
                name_of(ctl),
                name_of(tgt)
            );
            if !tgt.is_null() {
                rename_target_cls = class_of(tgt);
            }
        }
        // SAFETY: `app` is the live NSApp; the events are +0 autoreleased.
        unsafe {
            for (ch, code) in [("Z", 6u16), ("Q", 12u16)] {
                s_v_id(
                    app,
                    sel!(sendEvent:),
                    key_event(KEY_DOWN, win_no, nsstr(ch), code),
                );
                s_v_id(
                    app,
                    sel!(sendEvent:),
                    key_event(KEY_UP, win_no, nsstr(ch), code),
                );
            }
        }
        pump(&mut d, &mut el, 200);
        let typed = toolbar::rename_editor_text(&handle);
        cx.check(
            typed
                .as_deref()
                .is_some_and(|t| t.contains('Z') || t.contains('Q')),
            format!("real key events reached the field editor: {typed:?}"),
        );
        cx.check(
            toolbar::rename_editor_edit(&handle, crate::platform::RenameEditorEdit::SelectAll),
            "rename_editor_edit(SelectAll) found a field editor".to_owned(),
        );
        // Escape: `control:textView:doCommandBySelector:` with
        // `cancelOperation:`, entered by a real key event through AppKit's
        // key-binding machinery.
        d.wakes.clear();
        // SAFETY: live NSApp, +0 autoreleased events.
        unsafe {
            s_v_id(
                app,
                sel!(sendEvent:),
                key_event(KEY_DOWN, win_no, nsstr("\u{1b}"), 53),
            );
            s_v_id(
                app,
                sel!(sendEvent:),
                key_event(KEY_UP, win_no, nsstr("\u{1b}"), 53),
            );
        }
        pump(&mut d, &mut el, 300);
        println!("  --  Escape in the field editor -> {:?}", d.wakes);
        cx.check(
            d.wakes.iter().any(|w| w.starts_with("CancelSessionRename")),
            format!(
                "Escape through control:textView:doCommandBySelector: cancels; saw {:?}",
                d.wakes
            ),
        );
        toolbar::end_tab_rename(&handle);
        pump(&mut d, &mut el, 150);
        // Return: the commit exit, `controlTextDidEndEditing:`, not the selector
        // row (`abortEditing` suppresses that notification), so both need
        // driving.
        d.wakes.clear();
        cx.check(
            toolbar::begin_tab_rename(&handle, m3.ids[1], 77, "vim", "name"),
            "begin_tab_rename again, for the commit branch".to_owned(),
        );
        pump(&mut d, &mut el, 200);
        d.wakes.clear();
        // SAFETY: live NSApp, +0 autoreleased events.
        unsafe {
            s_v_id(
                app,
                sel!(sendEvent:),
                key_event(KEY_DOWN, win_no, nsstr("\r"), 36),
            );
            s_v_id(
                app,
                sel!(sendEvent:),
                key_event(KEY_UP, win_no, nsstr("\r"), 36),
            );
        }
        pump(&mut d, &mut el, 300);
        println!("  --  Return in the field editor -> {:?}", d.wakes);
        cx.check(
            d.wakes
                .iter()
                .any(|w| w.starts_with("CommitSessionRename") && w.contains("vim")),
            format!(
                "Return through controlTextDidEndEditing: commits what was typed; saw {:?}",
                d.wakes
            ),
        );
        // Tearing the field down is `App`'s job on that wake; `end_tab_rename`
        // removes it, and that is what this checks.
        toolbar::end_tab_rename(&handle);
        pump(&mut d, &mut el, 150);
        // SAFETY: `ns_window` is live.
        println!(
            "  --  firstResponder after end: {}",
            name_of(unsafe { s_id(ns_window, sel!(firstResponder)) })
        );
        cx.check(
            toolbar::rename_editor_text(&handle).is_none(),
            "the editor is gone after end_tab_rename".to_owned(),
        );

        // ---------------------------------------------------------- stage 8
        //
        // The ownership ledger. Every rebuild runs inside an `autoreleasepool`,
        // as the shipped app does (AppKit drains one per event-loop iteration);
        // without one the driver would measure its own missing pool.
        println!("\n-- stage 8: the ownership ledger over 200 pooled strip rebuilds");
        let m4 = model(4);
        apply(&handle, &m3);
        pump(&mut d, &mut el, 100);
        // SAFETY: `strip` is live; every send is a read-only accessor.
        let (rc0, sv0, ta0) = unsafe {
            (
                s_usize(strip, sel!(retainCount)),
                subviews(strip).len(),
                s_usize(s_id(strip, sel!(trackingAreas)), sel!(count)),
            )
        };
        // SAFETY: the chips are live views.
        let chip_ta0: Vec<usize> = unsafe {
            chips(strip)
                .iter()
                .map(|c| s_usize(s_id(*c, sel!(trackingAreas)), sel!(count)))
                .collect()
        };
        for i in 0..200 {
            aterm_objc::autoreleasepool(|_| apply(&handle, if i % 2 == 0 { &m4 } else { &m3 }));
        }
        apply(&handle, &m3);
        pump(&mut d, &mut el, 200);
        // SAFETY: as above.
        let (rc1, sv1, ta1) = unsafe {
            (
                s_usize(strip, sel!(retainCount)),
                subviews(strip).len(),
                s_usize(s_id(strip, sel!(trackingAreas)), sel!(count)),
            )
        };
        // SAFETY: as above.
        let chip_ta1: Vec<usize> = unsafe {
            chips(strip)
                .iter()
                .map(|c| s_usize(s_id(*c, sel!(trackingAreas)), sel!(count)))
                .collect()
        };
        println!(
            "  --  strip retainCount {rc0} -> {rc1}; subviews {sv0} -> {sv1}; trackingAreas {ta0} -> {ta1}"
        );
        println!("  --  chip trackingAreas {chip_ta0:?} -> {chip_ta1:?}");
        cx.check(
            sv1 == sv0,
            format!("no subview accumulated: {sv0} -> {sv1}"),
        );
        cx.check(
            ta1 <= ta0 + 1,
            format!("no tracking area accumulated on the strip: {ta0} -> {ta1}"),
        );
        cx.check(
            chip_ta1.iter().all(|c| *c <= 2),
            format!("no tracking area accumulated on a chip: {chip_ta1:?}"),
        );
        cx.check(
            rc1 <= rc0 + 2,
            format!("the strip's retain count is stable: {rc0} -> {rc1}"),
        );

        // ---------------------------------------------------------- stage 9
        //
        // The state matrix: icon kinds, status marks, connection roles, dark
        // mode, the accent tint, hover and press. Each row applies a state, lets
        // AppKit draw it, and prints the hash. Hashes are printed, not asserted
        // (they are machine-specific); what is asserted is relational: every
        // row draws, and rows that must differ do differ.
        println!("\n-- stage 9: the drawing state matrix");
        // SAFETY: `ns_window` is live.
        unsafe {
            let f = s_rect(ns_window, sel!(frame));
            s_v_rect_bool(
                ns_window,
                sel!(setFrame:display:),
                CGRect {
                    origin: f.origin,
                    size: CGSize {
                        width: 1200.0,
                        height: f.size.height,
                    },
                },
                true,
            );
        }
        pump(&mut d, &mut el, 150);
        let mut matrix: Vec<(String, Option<u64>)> = Vec::new();
        let row = |name: &str,
                   d: &mut Driver,
                   el: &mut EventLoop<Wake>,
                   matrix: &mut Vec<(String, Option<u64>)>| {
            let t = Instant::now();
            while t.elapsed() < Duration::from_millis(90) {
                let _ = el.pump_app_events(Some(Duration::from_millis(4)), d);
            }
            // SAFETY: `ns_window` is live and the strip is re-found through
            // AppKit every row, so a rebuild that replaced it is followed.
            let s = unsafe { strip_of(ns_window) };
            // SAFETY: `s` is a live view or nil, which `capture` rejects.
            match unsafe { capture(s) } {
                Some((w, hgt, px)) => {
                    let h = aterm_hash::fnv1a_64(&px);
                    println!("  {name:<34} {w}x{hgt} {h:016x}");
                    matrix.push((name.to_owned(), Some(h)));
                }
                None => {
                    println!("  {name:<34} NO CAPTURE");
                    matrix.push((name.to_owned(), None));
                }
            }
        };
        for n in [1usize, 2, 3, 5, 8] {
            apply(&handle, &model(n));
            row(&format!("tabs={n}"), &mut d, &mut el, &mut matrix);
        }
        let mut m = model(4);
        for kind in [
            TabIconKind::Settings,
            TabIconKind::Markdown,
            TabIconKind::Editor,
            TabIconKind::Recovery,
        ] {
            m.metadata[0].icon = Some(kind);
            apply(&handle, &m);
            row(&format!("icon={kind:?}"), &mut d, &mut el, &mut matrix);
        }
        for (name, f) in [
            ("dirty", 0usize),
            ("busy", 1),
            ("attention", 2),
            ("not-closable", 3),
        ] {
            let mut mm = model(4);
            for t in &mut mm.metadata {
                t.dirty = f == 0;
                t.busy = f == 1;
                t.attention = crate::app_presence::chip_of_attention(f == 2);
                t.closable = f != 3;
                t.conn = None;
            }
            apply(&handle, &mm);
            row(name, &mut d, &mut el, &mut matrix);
        }
        for role in [
            TabConnRole::Outbound,
            TabConnRole::Inbound,
            TabConnRole::Both,
        ] {
            let mut mm = model(4);
            for t in &mut mm.metadata {
                t.conn = Some(role);
            }
            apply(&handle, &mm);
            row(&format!("conn={role:?}"), &mut d, &mut el, &mut matrix);
        }
        apply(&handle, &model(3));
        for (name, dark) in [("dark=true", true), ("dark=false", false)] {
            toolbar::set_strip_dark(&handle, dark);
            row(name, &mut d, &mut el, &mut matrix);
        }
        for (name, c) in [
            ("accent=None", None),
            ("accent=(220,40,60)", Some([220u8, 40, 60])),
            ("accent=(20,180,90)", Some([20u8, 180, 90])),
        ] {
            toolbar::set_active_tab_color(&handle, c);
            row(name, &mut d, &mut el, &mut matrix);
        }
        toolbar::set_active_tab_color(&handle, None);
        // Hover and press, driven into the registered IMPs a tracking area and
        // a mouse would enter.
        // SAFETY: `ns_window` is live.
        let strip_now = unsafe { strip_of(ns_window) };
        // SAFETY: `strip_now` is a live view.
        let cs3 = unsafe { chips(strip_now) };
        // SAFETY: as above.
        let plus = unsafe { plus_button(strip_now) };
        if let Some(chip) = cs3.first().copied() {
            for (name, s) in [
                ("chip hover", sel!(mouseEntered:)),
                ("chip hover off", sel!(mouseExited:)),
            ] {
                // SAFETY: `chip` is a live view; the event is +0 autoreleased.
                unsafe {
                    let b = s_rect(chip, sel!(bounds));
                    let wp = s_point_point_id(
                        chip,
                        sel!(convertPoint:toView:),
                        CGPoint {
                            x: b.size.width * 0.5,
                            y: b.size.height * 0.5,
                        },
                        Id::NIL,
                    );
                    s_v_id(chip, s, mouse_event(MOUSE_MOVED, wp, 0, win_no, 0));
                }
                row(name, &mut d, &mut el, &mut matrix);
            }
        } else {
            cx.check(false, "a chip is live for the hover rows".to_owned());
        }
        if let Some(p) = plus {
            // SAFETY: `p` is the live `+` button; the events are +0.
            let wp = unsafe {
                let b = s_rect(p, sel!(bounds));
                s_point_point_id(
                    p,
                    sel!(convertPoint:toView:),
                    CGPoint {
                        x: b.size.width * 0.5,
                        y: b.size.height * 0.5,
                    },
                    Id::NIL,
                )
            };
            // SAFETY: as above.
            unsafe {
                s_v_id(
                    p,
                    sel!(mouseEntered:),
                    mouse_event(MOUSE_MOVED, wp, 0, win_no, 0),
                )
            };
            row("plus hover", &mut d, &mut el, &mut matrix);
            // SAFETY: as above.
            unsafe {
                s_v_id(
                    p,
                    sel!(mouseDown:),
                    mouse_event(LEFT_DOWN, wp, 0, win_no, 1),
                )
            };
            row("plus pressed", &mut d, &mut el, &mut matrix);
            d.wakes.clear();
            // SAFETY: as above.
            unsafe { s_v_id(p, sel!(mouseUp:), mouse_event(LEFT_UP, wp, 0, win_no, 1)) };
            pump(&mut d, &mut el, 150);
            println!("  --  plus click -> {:?}", d.wakes);
            cx.check(
                d.wakes.iter().any(|w| w.contains("NewTab")),
                format!("the \"+\" posted MenuAction(NewTab); saw {:?}", d.wakes),
            );
            // SAFETY: as above.
            unsafe {
                s_v_id(
                    p,
                    sel!(mouseExited:),
                    mouse_event(MOUSE_MOVED, wp, 0, win_no, 0),
                )
            };
            row("plus idle", &mut d, &mut el, &mut matrix);
        } else {
            cx.check(
                false,
                "the \"+\" button is live for the press rows".to_owned(),
            );
        }

        // Relational checks: absolute hashes are machine-specific, these are
        // not, and a branch that stopped drawing a state fails them. Each group
        // holds states that must draw differently, and together the groups name
        // every state the matrix draws, so a row the loops above stopped
        // capturing, or one no group judges, fails the first check.
        let groups: [&[&str]; 8] = [
            &["tabs=1", "tabs=2", "tabs=3", "tabs=5", "tabs=8"],
            &[
                "icon=Settings",
                "icon=Markdown",
                "icon=Editor",
                "icon=Recovery",
            ],
            &["dirty", "busy", "attention", "not-closable"],
            &["conn=Outbound", "conn=Inbound", "conn=Both"],
            &["accent=None", "accent=(220,40,60)", "accent=(20,180,90)"],
            &["dark=true", "dark=false"],
            &["chip hover", "chip hover off"],
            &["plus hover", "plus pressed", "plus idle"],
        ];
        let mut named: Vec<&str> = groups.iter().flat_map(|g| g.iter().copied()).collect();
        named.sort_unstable();
        let mut captured: Vec<&str> = matrix.iter().map(|(n, _)| n.as_str()).collect();
        captured.sort_unstable();
        let unjudged: Vec<&str> = captured
            .iter()
            .filter(|n| !named.contains(n))
            .copied()
            .collect();
        let uncaptured: Vec<&str> = named
            .iter()
            .filter(|n| !captured.contains(n))
            .copied()
            .collect();
        cx.check(
            captured == named,
            format!(
                "the {} states the groups name were each captured once (got {}; \
                 never captured: {uncaptured:?}; in no group: {unjudged:?})",
                named.len(),
                captured.len()
            ),
        );
        let missing: Vec<&str> = matrix
            .iter()
            .filter(|(_, h)| h.is_none())
            .map(|(n, _)| n.as_str())
            .collect();
        cx.check(
            missing.is_empty(),
            format!("every state produced a bitmap; missing: {missing:?}"),
        );
        let hash_of = |name: &str| -> Option<u64> {
            matrix.iter().find(|(n, _)| n == name).and_then(|(_, h)| *h)
        };
        for group in groups {
            let hs: Vec<Option<u64>> = group.iter().map(|n| hash_of(n)).collect();
            let mut uniq = hs.clone();
            uniq.sort_unstable();
            uniq.dedup();
            cx.check(
                uniq.len() == group.len(),
                format!("these states all draw differently: {group:?} -> {hs:02x?}"),
            );
        }

        // --------------------------------------------------------- stage 10
        //
        // The four declared classes, read off live objects: every class is
        // reached through the object AppKit holds, every row comes from
        // `class_copyMethodList`, and every encoding is compared against a
        // protocol the class claims or an ancestor class, never a string this
        // tree wrote.
        println!("\n-- stage 10: the four declared classes, off the live objects");
        // SAFETY: `ns_window` is live; the toolbar's delegate is the declared
        // `ATermToolbarDelegate` while the toolbar exists.
        let tb_delegate = unsafe {
            let tb = s_id(ns_window, sel!(toolbar));
            if tb.is_null() {
                Id::NIL
            } else {
                s_id(tb, sel!(delegate))
            }
        };
        let declared = [
            Declared {
                found_by: "the toolbar item's own view -> subviews -> ATermTabView",
                name: c"ATermTabView",
                // SAFETY: `cs3` holds live chips.
                cls: cs3
                    .first()
                    .map_or(ClassPtr::NULL, |c| unsafe { class_of(*c) }),
                conforms: None,
            },
            Declared {
                found_by: "the strip's ChromeButton subview",
                name: c"ATermChromeButton",
                // SAFETY: `plus` is a live view when present.
                cls: plus.map_or(ClassPtr::NULL, |p| unsafe { class_of(p) }),
                conforms: None,
            },
            Declared {
                found_by: "-[[NSWindow toolbar] delegate]",
                cls: if tb_delegate.is_null() {
                    ClassPtr::NULL
                } else {
                    // SAFETY: non-null live object.
                    unsafe { class_of(tb_delegate) }
                },
                name: c"ATermToolbarDelegate",
                conforms: Some(c"NSToolbarDelegate"),
            },
            Declared {
                found_by: "-[[field editor delegate] delegate], during stage 7's rename",
                name: c"ATermTabRenameTarget",
                cls: rename_target_cls,
                conforms: Some(c"NSTextFieldDelegate"),
            },
        ];
        for dcl in &declared {
            audit_declared(&mut cx, dcl);
        }

        // ---------------------------------------------------------- verdict
        drop(handle);
        drop(d.window.take());
        println!("\n=== VERDICT ===");
        for f in &cx.findings {
            println!("  FINDING: {f}");
        }
        for u in &cx.unmeasured {
            println!("  NOT MEASURED: {u}");
        }
        if !cx.findings.is_empty() {
            println!("objc-toolbar-drive: {} FINDING(S)", cx.findings.len());
            FINDING
        } else if !cx.unmeasured.is_empty() {
            println!(
                "objc-toolbar-drive: NOT RUN — nothing failed, but {} click(s) that need this \
                 drive to be the active app ran while another app held activation (macOS \
                 refused it, or a person used another window), so they measured nothing; \
                 run it on an idle desktop",
                cx.unmeasured.len()
            );
            NOT_RUN
        } else {
            println!("objc-toolbar-drive: OK");
            PASS
        }
    }
}
