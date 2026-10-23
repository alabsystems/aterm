// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live AppKit driver exercising `aterm-window` delegate, view, and menu seams.

/// Every stage agreed.
#[cfg(target_os = "macos")]
const PASS: i32 = 0;
/// At least one finding. See the transcript.
#[cfg(target_os = "macos")]
const FAIL: i32 = 1;
/// The drive could not execute here. Not a pass.
const NOT_RUN: i32 = 2;

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "objc-window-drive: NOT RUN — this drives \
         crates/aterm-window/src/platform_impl/macos/window_delegate.rs, which does not exist off macOS."
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
        Bool, Id, Obj, Sel, WeakObj, WeakSlot, autoreleasepool, checked_msg, class, ns_string, sel,
    };
    use aterm_window::application::ApplicationHandler;
    use aterm_window::event::WindowEvent;
    use aterm_window::event_loop::ActiveEventLoop;
    use aterm_window::platform::macos::MonitorHandleExtMacOS;
    use aterm_window::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
    use aterm_window::window::{Theme, Window, WindowId};

    use super::{FAIL, NOT_RUN, PASS};

    /// Long enough for macOS to launch its `NSApplication`, hand back a window
    /// and run every stage; past it the drive reports NOT RUN.
    const BUDGET: Duration = Duration::from_secs(60);

    /// `NSDragOperationCopy`, the value `draggingEntered:` must answer for a
    /// pasteboard carrying filenames.
    const NS_DRAG_OPERATION_COPY: usize = 1;
    /// `NSDragOperationNone`, for a pasteboard carrying none.
    const NS_DRAG_OPERATION_NONE: usize = 0;

    /// `NSEventTypeKeyUp` and `NSEventModifierFlagCommand`, spelled here so the
    /// driver's expectation is not the port's constant.
    const NS_EVENT_TYPE_KEY_UP: usize = 11;
    const NS_EVENT_MODIFIER_FLAG_COMMAND: usize = 1 << 20;

    /// # Safety
    /// `delegate` and `info` must be live Objective-C objects matching `-draggingEntered:`.
    unsafe fn dragging_entered(delegate: Id, info: Id) -> usize {
        // SAFETY: registered `Q@:@` matching `NSDraggingDestination`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, Id) -> usize = checked_msg();
            f(delegate, sel!(draggingEntered:), info)
        }
    }

    /// # Safety
    /// `delegate` and `sender` must be live Objective-C objects.
    unsafe fn window_should_close(delegate: Id, sender: Id) -> bool {
        // SAFETY: registered `B@:@` matching `NSWindowDelegate`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Bool = checked_msg();
            f(delegate, sel!(windowShouldClose:), sender).as_bool()
        }
    }

    /// # Safety
    /// `window` must be a live `NSWindow`.
    unsafe fn window_delegate(window: Id) -> Id {
        // SAFETY: `-delegate` is `@16@0:8` on `NSWindow`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            f(window, sel!(delegate))
        }
    }

    /// # Safety
    /// `window` must be a live `NSWindow`.
    unsafe fn style_mask(window: Id) -> usize {
        // SAFETY: `-styleMask` is `Q16@0:8` on `NSWindow`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
            f(window, sel!(styleMask))
        }
    }

    /// # Safety
    /// `window` must be a live `NSWindow`.
    unsafe fn window_title(window: Id) -> String {
        // SAFETY: `-title` is `@16@0:8` on `NSWindow`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            aterm_objc::ns_string_to_rust(f(window, sel!(title)))
        }
    }

    /// # Safety
    /// Must be called on the main thread after `+sharedApplication`.
    unsafe fn app_menu(sel: Sel) -> Id {
        // SAFETY: `+sharedApplication` and menu getters are `@16@0:8`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let app = f(class(c"NSApplication").as_id(), sel!(sharedApplication));
            f(app, sel)
        }
    }

    /// # Safety
    /// `menu` must be nil or a live `NSMenu`.
    unsafe fn menu_count(menu: Id) -> isize {
        // SAFETY: `-numberOfItems` is `q16@0:8`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> isize = checked_msg();
            f(menu, sel!(numberOfItems))
        }
    }

    /// # Safety
    /// `menu` must be a live `NSMenu` and `0 <= i < menu_count(menu)`.
    unsafe fn menu_item_at(menu: Id, i: isize) -> Id {
        // SAFETY: `-itemAtIndex:` is `@24@0:8q16`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, isize) -> Id = checked_msg();
            f(menu, sel!(itemAtIndex:), i)
        }
    }

    /// One menu row, as the properties `menu.rs` sets on it.
    struct Row {
        title: String,
        separator: bool,
        action: Sel,
        key: String,
        mask: usize,
        submenu: Id,
    }

    /// # Safety
    /// `item` must be a live `NSMenuItem`.
    unsafe fn read_row(item: Id) -> Row {
        // SAFETY: `NSMenuItem` property getters match their documented Objective-C encodings.
        unsafe {
            let id_of: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let bool_of: unsafe extern "C-unwind" fn(Id, Sel) -> Bool = checked_msg();
            let sel_of: unsafe extern "C-unwind" fn(Id, Sel) -> Sel = checked_msg();
            let usize_of: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
            Row {
                title: aterm_objc::ns_string_to_rust(id_of(item, sel!(title))),
                separator: bool_of(item, sel!(isSeparatorItem)).as_bool(),
                action: sel_of(item, sel!(action)),
                key: aterm_objc::ns_string_to_rust(id_of(item, sel!(keyEquivalent))),
                mask: usize_of(item, sel!(keyEquivalentModifierMask)),
                submenu: id_of(item, sel!(submenu)),
            }
        }
    }

    /// `-[NSScreen backingScaleFactor]` for `+[NSScreen mainScreen]`, read without
    /// the port.
    fn main_screen_backing_scale() -> Option<f64> {
        // SAFETY: `+mainScreen` is `@16#0:8`.
        unsafe {
            let id_of: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let f64_of: unsafe extern "C-unwind" fn(Id, Sel) -> f64 = checked_msg();
            let screen = id_of(class(c"NSScreen").as_id(), sel!(mainScreen));
            if screen.is_null() {
                return None;
            }
            Some(f64_of(screen, sel!(backingScaleFactor)))
        }
    }

    /// `CGMainDisplayID()`, the display the window server calls main.
    fn cg_main_display_id() -> u32 {
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGMainDisplayID() -> u32;
        }
        // SAFETY: nullary CoreGraphics query with no preconditions.
        unsafe { CGMainDisplayID() }
    }

    #[repr(C)]
    struct DlInfo {
        fname: *const std::ffi::c_char,
        fbase: *mut std::ffi::c_void,
        sname: *const std::ffi::c_char,
        saddr: *mut std::ffi::c_void,
    }

    unsafe extern "C" {
        fn class_getInstanceMethod(cls: *mut std::ffi::c_void, sel: Sel) -> *mut std::ffi::c_void;
        fn method_getImplementation(m: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
        fn method_getTypeEncoding(m: *mut std::ffi::c_void) -> *const std::ffi::c_char;
        fn object_getClass(obj: Id) -> *mut std::ffi::c_void;
        fn dladdr(addr: *const std::ffi::c_void, info: *mut DlInfo) -> std::ffi::c_int;
    }

    /// `(the Method handle, its IMP, its registered type encoding)` for an
    /// instance method, read straight from libobjc.
    fn live_method(cls: *mut std::ffi::c_void, sel: Sel) -> Option<(usize, usize, String)> {
        // SAFETY: `cls` is a live class and `sel` is an interned selector.
        unsafe {
            let m = class_getInstanceMethod(cls, sel);
            if m.is_null() {
                return None;
            }
            let imp = method_getImplementation(m);
            let types = method_getTypeEncoding(m);
            let types = if types.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr(types)
                    .to_string_lossy()
                    .into_owned()
            };
            Some((m as usize, imp as usize, types))
        }
    }

    /// The mach-o image an address lives in, by `dladdr`.
    fn image_of(addr: usize) -> String {
        let mut info = DlInfo {
            fname: std::ptr::null(),
            fbase: std::ptr::null_mut(),
            sname: std::ptr::null(),
            saddr: std::ptr::null_mut(),
        };
        // SAFETY: `dladdr` queries dyld image metadata for `addr`.
        let ok = unsafe { dladdr(addr as *const std::ffi::c_void, &mut info) };
        if ok == 0 || info.fname.is_null() {
            return "<unknown image>".to_owned();
        }
        // SAFETY: `info.fname` is a non-null NUL-terminated string owned by dyld.
        unsafe { std::ffi::CStr::from_ptr(info.fname) }
            .to_string_lossy()
            .into_owned()
    }

    /// # Safety
    /// `chars` must be a live `NSString`.
    unsafe fn key_up_event(window_number: isize, modifiers: usize, chars: Id) -> Id {
        // SAFETY: `NSEvent` key-event factory matches its 10-argument Objective-C signature.
        unsafe {
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
            ) -> Id = checked_msg();
            f(
                class(c"NSEvent").as_id(),
                sel!(
                    keyEventWithType:location:modifierFlags:timestamp:windowNumber:context:characters:charactersIgnoringModifiers:isARepeat:keyCode:
                ),
                NS_EVENT_TYPE_KEY_UP,
                aterm_objc::CGPoint { x: 0.0, y: 0.0 },
                modifiers,
                0.0,
                window_number,
                Id::NIL,
                chars,
                chars,
                Bool::NO,
                4,
            )
        }
    }

    /// `+[NSProcessInfo processInfo].processName`, which the About, Hide and Quit
    /// titles are built from.
    fn process_name() -> String {
        // SAFETY: `+processInfo` and `-processName` return a live `NSString`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let pi = f(class(c"NSProcessInfo").as_id(), sel!(processInfo));
            aterm_objc::ns_string_to_rust(f(pi, sel!(processName)))
        }
    }

    /// A pasteboard carrying `NSFilenamesPboardType` and a dragging-info stub that
    /// answers it, as `(info, pasteboard)`, both +1 and owned by the caller.
    fn dragging_info(paths: &[&str]) -> Option<(Obj, Obj)> {
        // SAFETY: documented Foundation and AppKit selectors called on live objects.
        unsafe {
            let pb: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
            let name = ns_string("aterm-objc-window-drive")?;
            let board = pb(
                class(c"NSPasteboard").as_id(),
                sel!(pasteboardWithName:),
                name.id(),
            );
            let board = Obj::retain(board)?;

            let arr: unsafe extern "C-unwind" fn(Id, Sel, *const Id, usize) -> Id = checked_msg();
            let ty = filenames_pboard_type();
            let types = [ty];
            let type_list = arr(
                class(c"NSArray").as_id(),
                sel!(arrayWithObjects:count:),
                types.as_ptr(),
                types.len(),
            );

            let declare: unsafe extern "C-unwind" fn(Id, Sel, Id, Id) -> isize = checked_msg();
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
                let set: unsafe extern "C-unwind" fn(Id, Sel, Id, Id) -> Bool = checked_msg();
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
        // SAFETY: `NSFilenamesPboardType` is a process-lifetime AppKit global.
        unsafe { TY }
    }

    aterm_objc::declare_class! {
        /// The smallest object that answers `-draggingPasteboard`, the only thing
        /// `draggingEntered:` and `performDragOperation:` ask of their sender.
        pub(super) struct ATermDragInfo: NSObject {
            const NAME: &str = "ATermObjcWindowDriveDragInfo";
            type Ivars = Id;

            @sel(draggingPasteboard)
            fn dragging_pasteboard(&self) -> Id {
                *self.ivars()
            }
        }
    }

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
        /// Key-up events that came back through the swizzled `-sendEvent:`.
        key_ups: usize,
        posted_cmd_key_up: bool,
        done: bool,
        stage: usize,
    }

    impl Driver {
        /// The `WinitWindowDelegate` AppKit is holding and its `NSWindow`, reached
        /// without the port: the window's content view, its window, that window's
        /// `-delegate`.
        fn pair(&self) -> Option<(Id, Id)> {
            let view = self.ns_view();
            if view.is_null() {
                return None;
            }
            // SAFETY: `-window` is `@16@0:8` on `NSView`.
            let window = unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
                f(view, sel!(window))
            };
            if window.is_null() {
                return None;
            }
            // SAFETY: `window` is the live `NSWindow` hosting the view.
            let delegate = unsafe { window_delegate(window) };
            if delegate.is_null() {
                return None;
            }
            Some((delegate, window))
        }

        /// The window's own content `NSView`. Nil if there is none.
        fn ns_view(&self) -> Id {
            self.window
                .as_ref()
                .map_or(Id::NIL, aterm_objc::HasNsView::ns_view)
        }
    }

    impl ApplicationHandler for Driver {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let attrs = Window::default_attributes()
                .with_title("objc-window-drive")
                .with_inner_size(aterm_window::dpi::LogicalSize::new(480.0, 320.0))
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
                WindowEvent::KeyboardInput { event, .. }
                    if event.state == aterm_window::event::ElementState::Released =>
                {
                    self.key_ups += 1;
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
            self.stage += 1;
            self.report.ran += 1;
            match self.stage {
                1 => self.stage_title(ns),
                2 => self.stage_style_mask(ns),
                3 => self.stage_geometry(),
                4 => self.stage_size_limits(),
                5 => self.stage_visibility_and_focus(),
                6 => self.stage_theme(),
                7 => self.stage_drag_and_drop(delegate),
                8 => self.stage_close(delegate, ns),
                9 => self.stage_fullscreen_enter(),
                10 => self.stage_fullscreen_exit(),
                11 => self.stage_weak_references(delegate, ns),
                12 => self.stage_default_menu(),
                13 => self.stage_monitors(),
                14 => self.stage_send_event_swizzle(),
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

        /// `-setTitle:`/`-title`: the `NSString` round trip, and the one path where a
        /// +1 string is handed to a copying setter.
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

        /// The style mask the window was built with, read off AppKit's own `-styleMask`
        /// rather than the port's state.
        fn stage_style_mask(&mut self, ns: Id) {
            println!("\n[2] style mask — the mask the window was built with");
            const TITLED: usize = 1 << 0;
            const CLOSABLE: usize = 1 << 1;
            const MINIATURIZABLE: usize = 1 << 2;
            const RESIZABLE: usize = 1 << 3;
            const BUILT: usize = TITLED | CLOSABLE | MINIATURIZABLE | RESIZABLE;

            // SAFETY: `ns` is the live `NSWindow`.
            let m = unsafe { style_mask(ns) };
            self.report.check(
                m & BUILT == BUILT && self.w().is_decorated(),
                &format!("titled, closable, miniaturizable and resizable (mask {m:#x})"),
            );
        }

        /// `-setFrameOrigin:`, `-frame`, `-contentRectForFrameRect:` and
        /// `-setContentSize:`: the struct-passing sends an ABI mistake corrupts silently.
        fn stage_geometry(&mut self) {
            use aterm_window::dpi::{PhysicalPosition, PhysicalSize};
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

        /// `-setContentMinSize:` and the clamp that follows: the send whose effect is a
        /// resize the caller did not ask for.
        fn stage_size_limits(&mut self) {
            use aterm_window::dpi::PhysicalSize;
            println!("\n[4] size limits — min clamps the current size");

            self.w()
                .set_min_inner_size(Some(PhysicalSize::new(700_u32, 500)));
            let got = self.w().inner_size();
            self.report.check(
                got.width >= 700 && got.height >= 500,
                &format!("min 700x500 grew the window to {got:?}"),
            );

            self.w().set_min_inner_size(None::<PhysicalSize<u32>>);
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
        /// `-bestMatchFromAppearancesWithNames:`: the round trip that decides light
        /// vs dark.
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

        /// `draggingEntered:` returns an eight-byte `NSDragOperation`, not a one-byte
        /// `BOOL`: `NSDragOperationCopy` (1) for a pasteboard with filenames and
        /// `NSDragOperationNone` (0) for one without.
        fn stage_drag_and_drop(&mut self, delegate: Id) {
            println!("\n[7] drag and drop — draggingEntered: returns NSDragOperation");
            let Some((info, _pb)) = dragging_info(&["/tmp/aterm-drive-a", "/tmp/aterm-drive-b"])
            else {
                self.report
                    .note("could not build a dragging-info stub; stage skipped");
                return;
            };
            // SAFETY: `delegate` is the live `WinitWindowDelegate` and `info` is a live `FakeDragInfo`.
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
            // SAFETY: `delegate` and `empty` are live objects.
            let op = unsafe { dragging_entered(delegate, empty.id()) };
            self.report.check(
                op == NS_DRAG_OPERATION_NONE,
                &format!("no filenames -> {op} (NSDragOperationNone is {NS_DRAG_OPERATION_NONE})"),
            );
        }

        /// `windowShouldClose:` must answer NO and queue `CloseRequested` — the
        /// inversion that would make the window uncloseable or unstoppable.
        fn stage_close(&mut self, delegate: Id, ns: Id) {
            println!("\n[8] close — windowShouldClose: answers NO and queues CloseRequested");
            let before = self.close_requested;
            // SAFETY: `delegate` and `ns` are live objects.
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

        /// The fullscreen state machine, entering; the exit is a pump later (see
        /// [`Self::stage_fullscreen_exit`]).
        fn stage_fullscreen_enter(&mut self) {
            use aterm_window::window::Fullscreen;
            println!("\n[9] fullscreen — entering");
            self.report
                .check(self.w().fullscreen().is_none(), "starts windowed");
            self.w().set_fullscreen(Some(Fullscreen::Borderless(None)));
            self.report.check(
                self.w().fullscreen().is_some(),
                "set_fullscreen(Borderless) is accepted by the state machine",
            );
        }

        /// The fullscreen state machine, leaving. `set_fullscreen(None)` right after
        /// `set_fullscreen(Some(_))` need not return `fullscreen()` to `None`: AppKit
        /// delivers `windowWillEnterFullScreen:` synchronously inside
        /// `-toggleFullScreen:`, setting `in_fullscreen_transition`, and winit defers a
        /// change during a transition into `target_fullscreen`.
        fn stage_fullscreen_exit(&mut self) {
            println!("\n[10] fullscreen — leaving");
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

            let m = self.w().is_maximized();
            self.report.check(
                m == self.w().is_maximized(),
                &format!("is_maximized answers without raising and is stable: {m}"),
            );
        }

        /// `aterm_objc::weak` against the objects it exists for. `aterm-objc`'s
        /// `tests/weak.rs` proves each property on `NSObject` and `NSMutableString`
        /// (libtest cannot host AppKit); this runs the same experiments on the live
        /// `NSWindow`, its `NSView` and the `WinitWindowDelegate`, AppKit classes with
        /// their own `dealloc` and references.
        fn stage_weak_references(&mut self, delegate: Id, ns: Id) {
            println!(
                "\n[11] weak references — objc_initWeak/loadWeak/storeWeak/copyWeak/destroyWeak"
            );

            let view = self.ns_view();
            if view.is_null() {
                self.report
                    .check(false, "the window has an NSView to weakly reference");
                return;
            }

            // SAFETY: `-retainCount` is `Q16@0:8` on `NSObject`.
            let count = |id: Id| unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
                f(id, sel!(retainCount))
            };
            let before = count(view);

            // SAFETY: `ns`, `view`, and `delegate` are live objects.
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

            let slot = WeakSlot::uninit();
            let borrowed = autoreleasepool(|_pool| {
                // SAFETY: `slot` is freshly `uninit()` and pinned in place.
                unsafe {
                    slot.init(ns);
                    slot.load_autoreleased().addr()
                }
            });
            // SAFETY: `slot` was initialized above and is destroyed once.
            unsafe { slot.destroy() };
            self.report.check(
                borrowed == ns.addr(),
                "the raw +0 weak load (objc_loadWeak) inside a pool answers the same NSWindow",
            );

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

            self.stage_weak_dealloc();
        }

        /// The nil-on-dealloc half, and the memcpy hazard, against a standalone
        /// `NSView`: AppKit holds references to the window's installed view, but a
        /// free-standing `[[NSView alloc] initWithFrame:]` has exactly one owner, so
        /// releasing it really runs `-dealloc`.
        fn stage_weak_dealloc(&mut self) {
            let observed = autoreleasepool(|_| {
                // SAFETY: `+alloc` and `-initWithFrame:` return a +1 `NSView`.
                let view = unsafe {
                    let alloc: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
                    let init: unsafe extern "C-unwind" fn(Id, Sel, aterm_objc::CGRect) -> Id =
                        checked_msg();
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
                let registered = WeakSlot::uninit();
                let memcpyd = WeakSlot::uninit();
                // SAFETY: `registered` is freshly `uninit()` and pinned in place.
                unsafe { registered.init(view.id()) };
                // SAFETY: `WeakSlot` is `#[repr(transparent)]` over one pointer word.
                unsafe { std::ptr::copy_nonoverlapping(registered.addr(), memcpyd.addr(), 1) };

                let moved = weak;
                drop(view);

                // SAFETY: plain pointer-word reads without dereferencing.
                let (a, b, c) = unsafe { (registered.peek(), memcpyd.peek(), moved.peek()) };
                let live = moved.is_live();
                // SAFETY: `registered` was initialized once and is destroyed at its original address.
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

        /// The default application menu (`crates/aterm-window`'s `menu.rs`), read back
        /// off `[NSApp mainMenu]`.
        fn stage_default_menu(&mut self) {
            println!("\n[12] default menu — winit's menu.rs, off [NSApp mainMenu]");
            let name = process_name();
            // SAFETY: shared application is live on the main thread.
            let (menubar, services) =
                unsafe { (app_menu(sel!(mainMenu)), app_menu(sel!(servicesMenu))) };
            if menubar.is_null() {
                self.report
                    .check(false, "[NSApp mainMenu] is nil — no menu was installed");
                return;
            }
            // SAFETY: `menubar` is the live `NSMenu`.
            let top = unsafe { menu_count(menubar) };
            self.report.check(
                top == 1,
                &format!("the menu bar holds one item (got {top})"),
            );
            if top < 1 {
                return;
            }
            // SAFETY: index 0 is in bounds.
            let app_item = unsafe { menu_item_at(menubar, 0) };
            // SAFETY: `app_item` is a live `NSMenuItem`.
            let app_row = unsafe { read_row(app_item) };
            self.report.check(
                !app_row.submenu.is_null(),
                "the application item carries a submenu",
            );
            if app_row.submenu.is_null() {
                return;
            }
            let app_menu = app_row.submenu;
            // SAFETY: `app_menu` is the live submenu.
            let n = unsafe { menu_count(app_menu) };
            self.report.check(
                n == 8,
                &format!("the application menu holds eight rows (got {n})"),
            );
            if n != 8 {
                return;
            }

            const COMMAND: usize = 1 << 20;
            const OPTION_COMMAND: usize = (1 << 19) | (1 << 20);
            let expected: [(String, bool, Sel, &str, usize); 8] = [
                (
                    format!("About {name}"),
                    false,
                    sel!(orderFrontStandardAboutPanel:),
                    "",
                    COMMAND,
                ),
                (String::new(), true, Sel::NULL, "", 0),
                (
                    "Services".to_owned(),
                    false,
                    sel!(submenuAction:),
                    "",
                    COMMAND,
                ),
                (format!("Hide {name}"), false, sel!(hide:), "h", COMMAND),
                (
                    "Hide Others".to_owned(),
                    false,
                    sel!(hideOtherApplications:),
                    "h",
                    OPTION_COMMAND,
                ),
                (
                    "Show All".to_owned(),
                    false,
                    sel!(unhideAllApplications:),
                    "",
                    COMMAND,
                ),
                (String::new(), true, Sel::NULL, "", 0),
                (
                    format!("Quit {name}"),
                    false,
                    sel!(terminate:),
                    "q",
                    COMMAND,
                ),
            ];

            for (i, (title, separator, action, key, mask)) in expected.iter().enumerate() {
                // SAFETY: `i < 8 == numberOfItems`.
                let row = unsafe { read_row(menu_item_at(app_menu, i as isize)) };
                self.report.check(
                    row.separator == *separator,
                    &format!("row {i} separator={} (want {separator})", row.separator),
                );
                if *separator {
                    // A separator's title, action and key equivalent are
                    // AppKit's, not the fork's; asserting them would be
                    // asserting `+separatorItem`.
                    continue;
                }
                self.report.check(
                    row.title == *title,
                    &format!("row {i} title {:?} (want {title:?})", row.title),
                );
                self.report.check(
                    row.action == *action,
                    &format!(
                        "row {i} action {:?} (want {:?})",
                        row.action.name(),
                        action.name()
                    ),
                );
                self.report.check(
                    row.key == *key,
                    &format!("row {i} key equivalent {:?} (want {key:?})", row.key),
                );
                self.report.check(
                    row.mask == *mask,
                    &format!("row {i} modifier mask {:#x} (want {mask:#x})", row.mask),
                );
            }

            // SAFETY: index 2 is in bounds.
            let services_row = unsafe { read_row(menu_item_at(app_menu, 2)) };
            self.report.check(
                !services_row.submenu.is_null(),
                "the Services row carries a submenu",
            );
            self.report.check(
                !services.is_null() && services.addr() == services_row.submenu.addr(),
                &format!(
                    "[NSApp servicesMenu] IS the Services row's submenu ({:?} vs {:?})",
                    services, services_row.submenu
                ),
            );
        }

        /// The monitor surface (`crates/aterm-window`'s `monitor.rs`).
        fn stage_monitors(&mut self) {
            println!("\n[13] monitors — winit's monitor.rs, against CoreGraphics and AppKit");
            let cg_main = cg_main_display_id();

            let Some(primary) = self.w().primary_monitor() else {
                self.report.check(false, "there is a primary monitor");
                return;
            };
            self.report.check(
                primary.native_id() == cg_main,
                &format!(
                    "primary_monitor's native id {} is CGMainDisplayID() {cg_main}",
                    primary.native_id()
                ),
            );

            let all: Vec<u32> = self
                .w()
                .available_monitors()
                .map(|m| m.native_id())
                .collect();
            self.report.check(
                all.contains(&cg_main),
                &format!("available_monitors {all:?} contains the main display {cg_main}"),
            );

            match self.w().current_monitor() {
                Some(m) => self.report.check(
                    m.native_id() == cg_main,
                    &format!(
                        "the window's current_monitor {} is the main display {cg_main} \
                         (get_display_id round-trips through the screen's device \
                         description)",
                        m.native_id()
                    ),
                ),
                None => self
                    .report
                    .check(false, "the window reports a current monitor"),
            }

            match main_screen_backing_scale() {
                Some(appkit) => self.report.check(
                    (primary.scale_factor() - appkit).abs() < f64::EPSILON,
                    &format!(
                        "primary scale factor {} is AppKit's -backingScaleFactor {appkit} \
                         (1.0 here would mean ns_screen found no screen)",
                        primary.scale_factor()
                    ),
                ),
                None => self
                    .report
                    .note("no main screen; scale factor not compared"),
            }
            let size = primary.size();
            let pos = primary.position();
            self.report.check(
                size.width > 0 && size.height > 0,
                &format!("the primary monitor has a size ({size:?}, at {pos:?})"),
            );
            match primary.refresh_rate_millihertz() {
                Some(hz) => self.report.note(&format!("refresh rate {hz} mHz")),
                None => self.report.note("no refresh rate reported"),
            }
        }

        /// The `-sendEvent:` swizzle (`crates/aterm-window`'s `app.rs`, through
        /// `aterm_objc::swizzle`).
        fn stage_send_event_swizzle(&mut self) {
            println!("\n[14] -sendEvent: swizzle — winit's app.rs, off the runtime's own tables");
            // SAFETY: `+sharedApplication` is `@16#0:8`.
            let app = unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
                f(class(c"NSApplication").as_id(), sel!(sharedApplication))
            };
            // SAFETY: `app` is the live shared application.
            let app_cls = unsafe { object_getClass(app) };
            let ns_app_cls = class(c"NSApplication").as_ptr();

            let Some((m_via_instance, imp, types)) = live_method(app_cls, sel!(sendEvent:)) else {
                self.report
                    .check(false, "-[NSApplication sendEvent:] resolves");
                return;
            };
            let Some((m_via_class, _, _)) = live_method(ns_app_cls, sel!(sendEvent:)) else {
                self.report
                    .check(false, "NSApplication implements sendEvent:");
                return;
            };

            self.report.check(
                m_via_instance == m_via_class,
                &format!(
                    "the row reached through [NSApp class] IS NSApplication's own \
                     ({m_via_instance:#x} vs {m_via_class:#x})"
                ),
            );

            let image = image_of(imp);
            let appkit = image_of(
                live_method(class(c"NSWindow").as_ptr(), sel!(sendEvent:)).map_or(0, |(_, i, _)| i),
            );
            self.report.note(&format!("IMP {imp:#x} in {image}"));
            self.report.note(&format!("AppKit's own image is {appkit}"));
            self.report.note(&format!(
                "registered encoding {types:?} — UNCHANGED BY A SWIZZLE, and \
                 therefore not evidence of one"
            ));
            self.report.check(
                !image.contains("/AppKit.framework/"),
                &format!(
                    "-[NSApplication sendEvent:] no longer lands in AppKit (it is in {image})"
                ),
            );
            let exe = std::env::current_exe().unwrap_or_default();
            let real = |p: &std::path::Path| {
                std::fs::canonicalize(p)
                    .unwrap_or_else(|_| p.to_path_buf())
                    .to_string_lossy()
                    .into_owned()
            };
            let exe_real = real(&exe);
            let image_real = real(std::path::Path::new(&image));
            self.report.check(
                !exe_real.is_empty() && image_real == exe_real,
                &format!("…it lands in this executable ({image_real} vs {exe_real})"),
            );

            // SAFETY: `-keyWindow` is `@16@0:8` on `NSApplication`.
            let key_window = unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
                f(app, sel!(keyWindow))
            };
            if key_window.is_null() {
                self.report
                    .note("no key window in this session; the Cmd+keyUp round trip is skipped");
                return;
            }
            // SAFETY: `-windowNumber` is `q16@0:8` on `NSWindow`.
            let number = unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel) -> isize = checked_msg();
                f(key_window, sel!(windowNumber))
            };
            let Some(chars) = ns_string("h") else {
                self.report
                    .check(false, "Foundation accepts a one-character string");
                return;
            };
            // SAFETY: `chars` is a live `NSString` and `app` is the live `NSApplication`.
            autoreleasepool(|_| unsafe {
                let ev = key_up_event(number, NS_EVENT_MODIFIER_FLAG_COMMAND, chars.id());
                if ev.is_null() {
                    self.report
                        .check(false, "NSEvent builds a synthetic key-up");
                    return;
                }
                let send: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
                send(app, sel!(sendEvent:), ev);
                self.posted_cmd_key_up = true;
            });
            self.report
                .note("posted a Cmd-modified keyUp to [NSApp sendEvent:]; checked after the pump");
        }
    }

    /// Drive the window loop through every verification stage and return an exit code.
    pub fn run() -> i32 {
        let mut el = match aterm_gui::quiet_driver_event_loop_builder::<()>().build() {
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
            key_ups: 0,
            posted_cmd_key_up: false,
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

        let hovered = std::mem::take(&mut driver.hovered);
        println!("\n[7b] the paths draggingEntered: queued");
        let want = ["/tmp/aterm-drive-a", "/tmp/aterm-drive-b"];
        driver.report.check(
            hovered == want,
            &format!("HoveredFile events {hovered:?} (want {want:?})"),
        );
        driver.report.check(
            driver.close_requested >= 1,
            &format!("CloseRequested arrived {} time(s)", driver.close_requested),
        );

        println!("\n[14b] the Cmd+keyUp the swizzled sendEvent: forwarded");
        if driver.posted_cmd_key_up {
            driver.report.check(
                driver.key_ups >= 1,
                &format!(
                    "a released-key event arrived {} time(s). AppKit does not deliver \
                     keyUp: to a window while Command is held, so this is the \
                     replacement -sendEvent: forwarding to -[NSWindow sendEvent:] and \
                     nothing else",
                    driver.key_ups
                ),
            );
        } else {
            driver
                .report
                .note("no key window; nothing was posted, so nothing is asserted");
        }

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
