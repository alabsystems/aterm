// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TOOLBAR DRIVER: `toolbar.rs`'s four declared classes and 268 ported
//! binding sites, entered through AppKit's own dispatch on a real `NSWindow`,
//! and read back as pixels, wakes and registered encodings.
//!
//! # The defect this exists to close
//!
//! `examples/objc_live_class_audit.rs` drives FIVE targets, all of them
//! `vendor/winit`'s. `toolbar.rs` is the largest ported file in the tree —
//! 4,800 lines, four declared classes, and after W7 every AppKit binding call
//! in it — and NOTHING IN THE TREE DROVE IT. Its classes were checked by
//! `#[cfg(test)] mod objc_tests`, whose central case is thirty-two rows of
//! registered encoding against a LITERAL WRITTEN IN THE SAME FILE. That is the
//! circularity D1 named, and W7's own control proved it is not theoretical: a
//! plant that registered `controlTextDidChange:` as `v@:B` **and edited the
//! table to agree** left `the_runtime_reports_every_encoding_the_table_says`
//! GREEN.
//!
//! It is also the exact shape D1 was raised for one file over. W3 ported
//! `window_delegate.rs` and CI read nothing; the answer was a driver. W7 ported
//! `toolbar.rs`'s bindings and CI still read nothing about the strip AppKit is
//! actually holding; this is that driver.
//!
//! # What it refuses to do
//!
//! * It never calls `toolbar::native_strip_container`. It finds the strip the
//!   way AppKit finds it — `-[NSWindow toolbar]` -> `-items` -> item 0 ->
//!   `-view` — so the thing under test is what the toolbar delegate actually
//!   handed AppKit, not what this crate believes it handed over.
//! * It never sends through `crate::appkit`. Every message below goes through
//!   this module's OWN typed casts ([`macos`]'s `s_*` family), so a wrong
//!   prototype in the helper layer cannot agree with itself. The port and its
//!   instrument disagree independently or not at all.
//! * It never asserts a pixel hash against a stored golden. Hashes are
//!   MACHINE-SPECIFIC (backing scale, system font, accent tint), so a stored
//!   one would be a flake generator. What it asserts is RELATIONAL — every
//!   state draws, and states that must differ do differ — and what it PRINTS is
//!   the whole 26-row matrix, which is the A/B oracle: run this binary at two
//!   commits on one machine and diff the transcripts.
//!
//! That last point is how W7 shipped: 26 states, four runs per arm, every hash
//! equal between the branch and `origin/main`. It is also how W7 found the one
//! defect its 4,344 passing tests could not — `NSTextAlignmentCenter` is 1 on
//! arm64, not 2, and the only instrument that could see it was the strip's own
//! ink columns.
//!
//! # The exit contract, which `aterm_verify::stages::toolbar_drive_outcome`
//! reads
//!
//! | code | meaning |
//! |---|---|
//! | 0 | every check passed |
//! | 1 | at least one finding; the transcript names each |
//! | 2 | NOT RUN — no event loop, no window, or no toolbar was installed. NEVER a pass. |
//! | 3 | the drive HUNG (a modal tracking loop never returned) and the watchdog killed it |
//!
//! `3` is its own code and not a panic because the failure it names is
//! specific: this driver enters `-mouseDown:` IMPs directly, and a context menu
//! that actually popped would run `-[NSMenu popUpContextMenu:…]`'s modal
//! tracking loop and never come back. A hang read as a timeout somewhere up the
//! ladder is a stage that decided nothing while looking busy.

/// Every check passed.
#[cfg(target_os = "macos")]
pub const PASS: i32 = 0;
/// At least one finding. See the transcript.
#[cfg(target_os = "macos")]
pub const FINDING: i32 = 1;
/// The drive could not execute here. NOT a pass.
#[cfg(target_os = "macos")]
pub const NOT_RUN: i32 = 2;
/// A modal tracking loop never returned; the watchdog killed the process.
#[cfg(target_os = "macos")]
pub const HUNG: i32 = 3;

/// Drive the real tab strip and answer the exit code above.
///
/// Called from `examples/objc_toolbar_drive.rs`, which owns `fn main` — the
/// body lives here because `mod toolbar`, `Wake` and `WindowId` are all
/// library-private, exactly as `run_redraw_conformance` lives here for the
/// same reason.
#[cfg(target_os = "macos")]
#[must_use]
pub fn run_toolbar_drive() -> i32 {
    macos::run()
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::CStr;
    use std::time::{Duration, Instant};

    use aterm_objc::{
        Bool, CGPoint, CGRect, CGSize, ClassPtr, Id, Sel, class, class_methods, class_name,
        class_of, class_protocols, method_types, msg, ns_string, protocol, protocol_method_types,
        sel, strip_method_offsets, superclass_of,
    };
    use winit::application::ApplicationHandler;
    use winit::event_loop::{ActiveEventLoop, EventLoop};
    use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::{Window, WindowId as WinitWindowId};

    use super::{FINDING, HUNG, NOT_RUN, PASS};
    use crate::session_chrome::{TabChromeExt, TabMenuEntry};
    use crate::tab_bar::TabStripMetadata;
    use crate::tab_model::{TabConnRole, TabIconKind, TabId};
    use crate::{Wake, WindowId, toolbar};

    // ---------------------------------------------------------------- sends
    //
    // DELIBERATELY NOT `crate::appkit`. That module is the helper layer W7
    // routed all 268 sites through; an instrument that used it would be
    // evidence about the layer agreeing with itself. Every prototype below is
    // written out here, and each was checked against clang's own
    // `@encode`-derived signature for its selector.

    unsafe fn s_v_id(r: Id, s: Sel, a: Id) {
        // SAFETY: as above, for `void (id, SEL, id)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) = msg();
            f(r, s, a);
        }
    }
    unsafe fn s_v_bool(r: Id, s: Sel, a: bool) {
        // SAFETY: as above, for `void (id, SEL, BOOL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Bool) = msg();
            f(r, s, Bool::new(a));
        }
    }
    unsafe fn s_id(r: Id, s: Sel) -> Id {
        // SAFETY: as above, for `id (id, SEL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = msg();
            f(r, s)
        }
    }
    unsafe fn s_id_id(r: Id, s: Sel, a: Id) -> Id {
        // SAFETY: as above, for `id (id, SEL, id)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) -> Id = msg();
            f(r, s, a)
        }
    }
    unsafe fn s_id_usize(r: Id, s: Sel, a: usize) -> Id {
        // SAFETY: as above, for `id (id, SEL, NSUInteger)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, usize) -> Id = msg();
            f(r, s, a)
        }
    }
    unsafe fn s_usize(r: Id, s: Sel) -> usize {
        // SAFETY: as above, for `NSUInteger (id, SEL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> usize = msg();
            f(r, s)
        }
    }
    unsafe fn s_isize(r: Id, s: Sel) -> isize {
        // SAFETY: as above, for `NSInteger (id, SEL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> isize = msg();
            f(r, s)
        }
    }
    unsafe fn s_bool(r: Id, s: Sel) -> bool {
        // SAFETY: as above, for `BOOL (id, SEL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Bool = msg();
            f(r, s).as_bool()
        }
    }
    unsafe fn s_bool_id(r: Id, s: Sel, a: Id) -> bool {
        // SAFETY: as above, for `BOOL (id, SEL, id)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) -> Bool = msg();
            f(r, s, a).as_bool()
        }
    }
    unsafe fn s_rect(r: Id, s: Sel) -> CGRect {
        // SAFETY: as above, for `NSRect (id, SEL)`. 32 bytes, returned in
        // registers on both Apple ABIs for this shape.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> CGRect = msg();
            f(r, s)
        }
    }
    unsafe fn s_id_point(r: Id, s: Sel, p: CGPoint) -> Id {
        // SAFETY: as above, for `id (id, SEL, NSPoint)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, CGPoint) -> Id = msg();
            f(r, s, p)
        }
    }
    unsafe fn s_point_point_id(r: Id, s: Sel, p: CGPoint, v: Id) -> CGPoint {
        // SAFETY: as above, for `NSPoint (id, SEL, NSPoint, id)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, CGPoint, Id) -> CGPoint = msg();
            f(r, s, p, v)
        }
    }
    unsafe fn s_v_rect_id(r: Id, s: Sel, a: CGRect, b: Id) {
        // SAFETY: as above, for `void (id, SEL, NSRect, id)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, CGRect, Id) = msg();
            f(r, s, a, b);
        }
    }
    unsafe fn s_id_rect(r: Id, s: Sel, a: CGRect) -> Id {
        // SAFETY: as above, for `id (id, SEL, NSRect)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, CGRect) -> Id = msg();
            f(r, s, a)
        }
    }
    unsafe fn s_bytes(r: Id, s: Sel) -> *const u8 {
        // SAFETY: as above, for `unsigned char * (id, SEL)`
        // (`-[NSBitmapImageRep bitmapData]`). Taken as `Id` and re-cast
        // because both are one pointer-sized return in the same register.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = msg();
            f(r, s).as_ptr().cast_const().cast::<u8>()
        }
    }
    unsafe fn s_v_rect_bool(r: Id, s: Sel, a: CGRect, b: bool) {
        // SAFETY: as above, for `void (id, SEL, NSRect, BOOL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, CGRect, Bool) = msg();
            f(r, s, a, Bool::new(b));
        }
    }
    unsafe fn s_v_id_bool(r: Id, s: Sel, a: Id, b: bool) {
        // SAFETY: as above, for `void (id, SEL, id, BOOL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, Bool) = msg();
            f(r, s, a, Bool::new(b));
        }
    }

    /// `+[NSEvent mouseEventWithType:location:modifierFlags:timestamp:windowNumber:context:eventNumber:clickCount:pressure:]`
    /// — measured `@92@0:8Q16{CGPoint=dd}24Q40d48q56@64q72q80f88`, which is why
    /// `pressure` is `f32` and the two counters are `NSInteger`.
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
            let f: unsafe extern "C" fn(
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
            ) -> Id = msg();
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

    /// `+[NSEvent keyEventWithType:…]` — measured
    /// `@96@0:8Q16{CGPoint=dd}24Q40d48q56@64@72@80B88S92`.
    unsafe fn key_event(kind: usize, win: isize, chars: Id, code: u16) -> Id {
        // SAFETY: as [`mouse_event`], against the measured signature above.
        unsafe {
            let f: unsafe extern "C" fn(
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
            ) -> Id = msg();
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

    /// Intern a class or protocol name that is only known as a `&str`.
    ///
    /// [`aterm_objc::protocol`] takes `&'static CStr` because every ordinary
    /// call site writes a `c"…"` literal; the names in stage 10 come out of
    /// `class_protocols`, which is the case that is not ordinary. One small
    /// leak per name, bounded by four superclass chains in a process that
    /// drives one window and exits — the same bridge
    /// `objc_live_class_audit.rs` uses, for the same reason.
    fn cstr_named(name: &str) -> &'static CStr {
        Box::leak(
            std::ffi::CString::new(name)
                .expect("a runtime-supplied name has no interior NUL")
                .into_boxed_c_str(),
        )
    }

    /// FNV-1a over the captured bitmap. Not cryptographic and not meant to be:
    /// what it has to do is change when any pixel changes and be stable across
    /// runs of the same build on the same machine.
    fn fnv(bytes: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
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
            attention: false,
            conn,
            closable: true,
            drop_target: false,
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
            m.tooltips.push(Some(format!("tip {i}")));
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

    fn apply(handle: &toolbar::ToolbarHandle, m: &Model) {
        toolbar::set_window_tabs(
            handle,
            &m.titles,
            &m.ids,
            &m.metadata,
            &m.tooltips,
            &m.ext,
            m.active,
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
            Wake::MenuAction { action } => format!("MenuAction({action:?})"),
            Wake::BeginSessionRename { tab, .. } => format!("BeginSessionRename({tab})"),
            Wake::CommitSessionRename { session, text, .. } => {
                format!("CommitSessionRename({session},{text:?})")
            }
            Wake::CancelSessionRename { session, .. } => format!("CancelSessionRename({session})"),
            Wake::ConnDragBegin { .. } => "ConnDragBegin".to_owned(),
            Wake::ConnDragTo { .. } => "ConnDragTo".to_owned(),
            Wake::ConnDragCancel { .. } => "ConnDragCancel".to_owned(),
            Wake::ConnDragDrop { .. } => "ConnDragDrop".to_owned(),
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
                .with_inner_size(winit::dpi::LogicalSize::new(1200.0, 400.0))
                .with_position(winit::dpi::LogicalPosition::new(80.0, 80.0))
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
            _e: winit::event::WindowEvent,
        ) {
        }
        fn user_event(&mut self, _el: &ActiveEventLoop, w: Wake) {
            if let Some(s) = show_wake(&w) {
                self.wakes.push(s);
            }
        }
    }

    /// The findings ledger. `check` prints every question it asked, passing or
    /// failing, because the transcript is half the deliverable: the A/B between
    /// two commits is a diff of it.
    struct Ctx {
        findings: Vec<String>,
    }
    impl Ctx {
        fn check(&mut self, ok: bool, msg: String) {
            println!("  {} {msg}", if ok { "ok  " } else { "FAIL" });
            if !ok {
                self.findings.push(msg);
            }
        }
    }

    // ------------------------------------------------------------- tree walk

    /// The strip container, reached the way AppKit reaches it: window ->
    /// toolbar -> items -> item 0 -> view.
    ///
    /// NEVER `toolbar::native_strip_container`. Going through AppKit is what
    /// makes this evidence about what the toolbar delegate HANDED OVER: if
    /// `toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:` were
    /// mis-registered, or the delegate stopped claiming `NSToolbarDelegate`,
    /// this returns nil and every stage below it fails — which is precisely
    /// what the accessor could not tell you.
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

    /// Capture `view` through AppKit's own `-cacheDisplayInRect:` pair and
    /// return `(w, h, bytes)`.
    ///
    /// This is the instrument, and it is AppKit's: the bitmap holds what
    /// `drawRect:` actually put down, so a port that registered every encoding
    /// correctly and drew the wrong thing is still caught. It is what caught
    /// `NSTextAlignmentCenter`.
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
    /// holds. This LOCALISES a pixel difference to a column band, which is the
    /// form in which the alignment defect was legible — a hash says "different",
    /// a span list says "the label moved 60 points right".
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

    /// Every protocol reachable from `seed`, following protocol INHERITANCE.
    ///
    /// A protocol list is a DAG: `NSTextFieldDelegate` inherits
    /// `NSControlTextEditingDelegate`, which is where two of the three rename
    /// rows are actually declared. Reading only the claimed names would miss
    /// every declaration one level up.
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

    /// The protocols a protocol itself inherits.
    ///
    /// `protocol_copyProtocolList` has no `aterm-objc` wrapper because no
    /// SHIPPING call site needs one — the crate grows for the port, not for
    /// the instrument — so the prototype is declared where it is read.
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

    /// Where a row's SHAPE is written down by something other than this tree,
    /// and what that authority says.
    ///
    /// Two sources, asked in order: every protocol the class claims (expanded
    /// through inheritance), then the superclass chain, which is where an
    /// OVERRIDE's original lives. `None` means nothing outside this tree
    /// declares the selector — every such row is a first-party action
    /// (`closeTab:`, `tabMenuAction:`) and is PRINTED rather than silently
    /// dropped.
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
        /// The name `declare_class!` registered — the only thing written down,
        /// and only so `objc_getClass` can be asked whether the live object's
        /// class IS that class.
        name: &'static CStr,
        cls: ClassPtr,
        /// A protocol the class must CONFORM to, if its rows come from one.
        /// This is plant two's tooth: an encoding check cannot see a class that
        /// silently stopped saying what it is.
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
            cx.check(!proto.is_null(), format!("{p:?} is loaded"));
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
    pub fn run() -> i32 {
        // THE WATCHDOG, and it is not belt-and-braces. This driver enters
        // `-mouseDown:`/`-rightMouseDown:` IMPs directly; a context menu that
        // actually popped would run `-[NSMenu popUpContextMenu:…]`'s modal
        // tracking loop and never return. A hang that reached the ladder as a
        // generic timeout is a stage that decided nothing while looking busy,
        // so it gets its own code.
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(180));
            eprintln!("WATCHDOG: the drive hung (a modal tracking loop never returned)");
            std::process::exit(HUNG);
        });

        let mut el = match EventLoop::<Wake>::with_user_event().build() {
            Ok(el) => el,
            Err(e) => {
                eprintln!("objc-toolbar-drive: NOT RUN — no event loop: {e}");
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
            eprintln!(
                "objc-toolbar-drive: NOT RUN — {}",
                d.blocked.unwrap_or_else(|| "no window".to_owned())
            );
            return NOT_RUN;
        };
        let Some(handle) = toolbar::install_window_toolbar(win, &proxy, WindowId(1)) else {
            eprintln!("objc-toolbar-drive: NOT RUN — install_window_toolbar returned None");
            return NOT_RUN;
        };
        let ns_view = {
            let Ok(h) = win.window_handle() else {
                eprintln!("objc-toolbar-drive: NOT RUN — no window handle");
                return NOT_RUN;
            };
            let RawWindowHandle::AppKit(a) = h.as_raw() else {
                eprintln!("objc-toolbar-drive: NOT RUN — not an AppKit window");
                return NOT_RUN;
            };
            Id::from_ptr(a.ns_view.as_ptr().cast())
        };
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
            // Nothing below can mean anything, and an empty toolbar is exactly
            // what a delegate that stopped claiming `NSToolbarDelegate` leaves
            // behind — a FINDING, not a could-not-run.
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

        // ---------------------------------------------------------- stage 2
        println!("\n-- stage 2: AppKit draws the strip (cacheDisplayInRect:)");
        // SAFETY: `strip` is a live view on the main thread.
        if let Some((w, h, px)) = unsafe { capture(strip) } {
            let cols = ink_columns(w, h, &px);
            let inked = cols.iter().filter(|c| **c > 0).count();
            println!("  --  {w}x{h} hash={:016x} ink_cols={inked}", fnv(&px));
            println!("  --  ink spans: {}", show_spans(&cols));
            cx.check(inked > 40, format!("the strip drew ink in {inked} columns"));
        } else {
            cx.check(false, "cacheDisplayInRect: produced no bitmap".to_owned());
        }

        // ---------------------------------------------------------- stage 3
        println!("\n-- stage 3: a real left click on each chip, through [NSApp sendEvent:]");
        // SAFETY: `+sharedApplication` on a linked class.
        let app = unsafe { s_id(class(c"NSApplication").as_id(), sel!(sharedApplication)) };
        for (i, c) in cs.iter().enumerate() {
            d.wakes.clear();
            // SAFETY: `c` and `strip` are live views, `app` the live NSApp;
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
                    app,
                    sel!(sendEvent:),
                    mouse_event(LEFT_DOWN, wp, 0, win_no, 1),
                );
                s_v_id(
                    app,
                    sel!(sendEvent:),
                    mouse_event(LEFT_UP, wp, 0, win_no, 1),
                );
                println!("  --  chip{i} hitTest -> {}", name_of(hit));
            }
            pump(&mut d, &mut el, 120);
            cx.check(
                d.wakes.iter().any(|w| w == &format!("SelectTab({i})")),
                format!("chip{i} click posted SelectTab({i}); saw {:?}", d.wakes),
            );
        }

        // ---------------------------------------------------------- stage 4
        println!("\n-- stage 4: drag chip0 across chip2 (down, 6 drags, up)");
        d.wakes.clear();
        // SAFETY: live views and NSApp, as stage 3.
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
                app,
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
                    app,
                    sel!(sendEvent:),
                    mouse_event(LEFT_DRAG, p, 0, win_no, 0),
                );
            }
            s_v_id(
                app,
                sel!(sendEvent:),
                mouse_event(LEFT_UP, p2, 0, win_no, 1),
            );
        }
        pump(&mut d, &mut el, 200);
        cx.check(
            d.wakes.iter().any(|w| w.starts_with("TabCmd(Move")),
            format!("the drag posted a reorder; saw {:?}", d.wakes),
        );

        // ---------------------------------------------------------- stage 5
        println!("\n-- stage 5: the context menu, two ways in");
        let menus = toolbar::read_tab_menus(&handle);
        println!("  --  read_tab_menus: {} line(s)", menus.len());
        for l in &menus {
            println!("      {l}");
        }
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

            // The app is made ACTIVE and the window KEY first: titlebar event
            // routing depends on both, and a probe that skips them measures its
            // own setup rather than the strip.
            s_v_bool(app, sel!(activateIgnoringOtherApps:), true);
            s_v_id(ns_window, sel!(makeKeyAndOrderFront:), Id::NIL);
            pump(&mut d, &mut el, 250);
            println!(
                "  --  app active={} window key={} main={}",
                s_bool(app, sel!(isActive)),
                s_bool(ns_window, sel!(isKeyWindow)),
                s_bool(ns_window, sel!(isMainWindow))
            );

            // THE CONTROL, and it is the reason the two `[]` lines below are
            // an observation and not a failure: a plain left click through the
            // SAME routing does arrive. `[NSApp sendEvent:]` does not deliver a
            // synthesized ctrl-left or right-mouse into the titlebar's view
            // tree — measured, on both this branch and origin/main — so the
            // menu is entered at its IMP instead.
            d.wakes.clear();
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
            println!("  --  [NSApp sendEvent:] plain left   -> {:?}", d.wakes);
            cx.check(
                control_arrived,
                "the CONTROL arrived: NSApp routes a plain left click into the strip".to_owned(),
            );

            d.wakes.clear();
            s_v_id(app, sel!(sendEvent:), ctrl_ev);
            s_v_id(
                app,
                sel!(sendEvent:),
                mouse_event(LEFT_UP, wp, CTRL, win_no, 1),
            );
            pump(&mut d, &mut el, 250);
            println!("  --  [NSApp sendEvent:] ctrl-left    -> {:?}", d.wakes);

            d.wakes.clear();
            s_v_id(app, sel!(sendEvent:), right_ev);
            s_v_id(
                app,
                sel!(sendEvent:),
                mouse_event(RIGHT_UP, wp, 0, win_no, 1),
            );
            pump(&mut d, &mut el, 250);
            println!("  --  [NSApp sendEvent:] rightMouse   -> {:?}", d.wakes);

            let frame_view = s_id(s_id(ns_window, sel!(contentView)), sel!(superview));
            println!(
                "  --  theme frame={} hitTest(window pt) -> {}",
                name_of(frame_view),
                name_of(s_id_point(frame_view, sel!(hitTest:), wp))
            );

            // STRAIGHT INTO THE REGISTERED IMP, which is what AppKit calls once
            // its routing has picked the view. An ESC is queued first so the
            // menu that pops has something to dismiss it — without it the
            // modal tracking loop is what the watchdog exists for.
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
                s_v_id_bool(
                    app,
                    sel!(postEvent:atStart:),
                    key_event(KEY_DOWN, win_no, nsstr("\u{1b}"), 53),
                    true,
                );
                s_v_id(recv, s, ev);
                pump(&mut d, &mut el, 300);
                println!("  --  [{label}] -> {:?}", d.wakes);
                cx.check(
                    d.wakes.iter().any(|w| w == "TabContextMenuOpening"),
                    format!("[{label}] pops the tab context menu"),
                );
            }

            // WHERE a click lands, across the chip. Free (no pumping), and it
            // is the layout evidence a chip-geometry regression shows up in
            // first: the label owns the middle, the chip owns the edges.
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

            // AND THE LABEL, which is what a user's pointer is actually over:
            // `NSTextField` is not a chip, so both menu routes have to survive
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
                    s_v_id_bool(
                        app,
                        sel!(postEvent:atStart:),
                        key_event(KEY_DOWN, win_no, nsstr("\u{1b}"), 53),
                        true,
                    );
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
        apply(&handle, &m3);
        pump(&mut d, &mut el, 200);
        // SAFETY: `strip` is live.
        let cs2 = unsafe { chips(strip) };
        cx.check(
            cs2.len() == 3,
            format!("3 chips after the re-layout (got {})", cs2.len()),
        );
        for (i, c) in cs2.iter().enumerate() {
            // SAFETY: live views.
            unsafe { println!("  --  chip{i} frame={}", show_rect(s_rect(*c, sel!(frame)))) }
        }
        // SAFETY: `strip` is live.
        if let Some((w, h, px)) = unsafe { capture(strip) } {
            let cols = ink_columns(w, h, &px);
            println!("  --  narrow {w}x{h} hash={:016x}", fnv(&px));
            println!("  --  ink spans: {}", show_spans(&cols));
        }

        // ---------------------------------------------------------- stage 7
        //
        // THE RENAME EDITOR, AND IT IS WHERE D1 LIVES. Two of
        // `TabRenameTarget`'s three rows are why the D1 finding exists at all:
        // `control:textView:doCommandBySelector:` takes a `SEL` and returns a
        // `BOOL`, and its one-byte-vs-eight-byte failure mode is SILENT on
        // arm64. Nothing in the tree calls that row except a real Escape typed
        // into a real field editor, and nothing calls its commit twin except a
        // real Return, so this stage types both.
        //
        // It found a defect on its first run, and the defect had nothing to do
        // with encodings: `-selectText:` ended the editing session
        // `-makeFirstResponder:` had just begun, `controlTextDidEndEditing:`
        // fired from inside `begin_tab_rename` on a relay already attached, and
        // the resulting `done` latch left BOTH exits dead for the rest of the
        // editor's life. See the comment at that attach in `toolbar.rs`.
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
        // THE SPURIOUS-OUTCOME CHECK, and it is the one this stage was written
        // blind to. Opening an editor decides nothing, so it must post nothing:
        // a commit here is a rename the user never asked for AND a latch that
        // kills every later exit.
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
        // delegate is the declared `ATermTabRenameTarget`. Walking to it here
        // is the only moment that object is LIVE, which is why stage 10 reads
        // it now.
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
        // ESCAPE — `control:textView:doCommandBySelector:` with
        // `cancelOperation:`. THE D1 ROW, entered by a real key event through
        // AppKit's own key-binding machinery.
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
        // RETURN — the commit exit, which is `controlTextDidEndEditing:` and
        // NOT the selector row: `abortEditing` suppresses that notification, so
        // the two outcomes leave by two different rows and both need driving.
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
        // Tearing the field down is `App`'s job on that wake, not the strip's,
        // so the field is still up here — `end_tab_rename` is what removes it,
        // and THAT is what this checks.
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
        // THE OWNERSHIP LEDGER. Every rebuild runs inside an
        // `autoreleasepool`, because that is what the shipped app does — AppKit
        // drains one per event-loop iteration — and a driver without one
        // measures its own missing pool: the same 200 rebuilds unpooled take
        // the strip's retain count 152 -> 5755 IDENTICALLY on this branch and
        // on origin/main, which is a statement about this file, not about the
        // port.
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
        // THE STATE MATRIX. Stage 2 captured ONE model; every other branch of
        // the drawing code — icon kinds, status marks, connection roles, dark
        // mode, the accent tint, hover, press — is invisible to it. Each row
        // applies a state, lets AppKit draw it, and prints the hash.
        //
        // The hashes are NOT asserted against a golden: they depend on the
        // backing scale, the system font and the user's accent colour, so a
        // stored one would be a flake generator. They are printed, and the
        // A/B is a diff of two transcripts on one machine — which is how this
        // port shipped (26 states, four runs per arm, every hash equal to
        // origin/main). What IS asserted is relational and machine-independent:
        // every row draws, and rows that must differ do differ.
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
                    let h = fnv(&px);
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
            ("drop_target", 3),
            ("not-closable", 4),
        ] {
            let mut mm = model(4);
            for t in &mut mm.metadata {
                t.dirty = f == 0;
                t.busy = f == 1;
                t.attention = f == 2;
                t.drop_target = f == 3;
                t.closable = f != 4;
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
        // HOVER AND PRESS, driven into the registered IMPs a tracking area and
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

        // The relational teeth. Absolute hashes are machine-specific; these are
        // not, and they are what a branch that silently stopped drawing a state
        // fails.
        cx.check(
            matrix.len() == 27,
            format!("27 states were captured (got {})", matrix.len()),
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
        for group in [
            &["tabs=1", "tabs=2", "tabs=3", "tabs=5", "tabs=8"][..],
            &[
                "icon=Settings",
                "icon=Markdown",
                "icon=Editor",
                "icon=Recovery",
            ][..],
            &["dirty", "busy", "attention", "drop_target", "not-closable"][..],
            &["conn=Outbound", "conn=Inbound", "conn=Both"][..],
            &["accent=None", "accent=(220,40,60)", "accent=(20,180,90)"][..],
            &["dark=true", "dark=false"][..],
            &["chip hover", "chip hover off"][..],
            &["plus hover", "plus pressed", "plus idle"][..],
        ] {
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
        // THE FOUR DECLARED CLASSES, READ OFF LIVE OBJECTS. `toolbar.rs`'s
        // `objc_tests` checks thirty-two encodings against a literal IN THE
        // SAME FILE — the circularity D1 named, and W7's own control proved it
        // is not theoretical: a plant that registered `controlTextDidChange:`
        // as `v@:B` AND edited the table to agree left that test GREEN. Here
        // every class is reached through the object AppKit is holding, every
        // row comes out of `class_copyMethodList`, and every encoding is
        // compared against what a protocol the class claims or an ancestor
        // class says — never against a string this tree wrote.
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
        if cx.findings.is_empty() {
            println!("objc-toolbar-drive: OK");
            PASS
        } else {
            for f in &cx.findings {
                println!("  FINDING: {f}");
            }
            println!("objc-toolbar-drive: {} FINDING(S)", cx.findings.len());
            FINDING
        }
    }
}
