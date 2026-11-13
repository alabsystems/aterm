// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live AppKit driver exercising `aterm-window` delegate, view, and menu seams.

/// Every stage agreed.
#[cfg(target_os = "macos")]
const PASS: i32 = 0;
/// At least one finding.
#[cfg(target_os = "macos")]
const FAIL: i32 = 1;
/// The drive could not execute here.
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
    if std::env::var_os(macos::RENAME_REENTRY_PARENT_ENV).is_some() {
        return std::process::ExitCode::from(macos::rename_reentry_parent() as u8);
    }
    if std::env::var_os(macos::RENAME_REENTRY_CHILD_ENV).is_some() {
        return std::process::ExitCode::from(aterm_gui::run_rename_reentry_drive() as u8);
    }
    if std::env::var_os(macos::TERMINATE_REENTRY_CHILD_ENV).is_some() {
        return std::process::ExitCode::from(macos::terminate_reentry_child() as u8);
    }
    if std::env::var_os(macos::MODAL_EXIT_ORDER_CHILD_ENV).is_some() {
        return std::process::ExitCode::from(macos::modal_exit_order_child() as u8);
    }
    if std::env::var_os(macos::MODAL_EXIT_RUN_APP_CHILD_ENV).is_some() {
        return std::process::ExitCode::from(macos::modal_exit_run_app_child() as u8);
    }
    if std::env::var_os(macos::PREPARED_EXIT_CHILD_ENV).is_some() {
        return std::process::ExitCode::from(macos::prepared_exit_child() as u8);
    }
    if std::env::var_os(macos::DEFERRED_PANIC_CHILD_ENV).is_some() {
        return std::process::ExitCode::from(macos::deferred_panic_child() as u8);
    }
    for mode in macos::DeferredQuit::ALL {
        if std::env::var_os(mode.child_env()).is_some() {
            return std::process::ExitCode::from(macos::deferred_quit_child(mode) as u8);
        }
    }
    std::process::ExitCode::from(macos::run() as u8)
}

#[cfg(target_os = "macos")]
mod macos {
    use std::time::{Duration, Instant};

    use aterm_objc::cf::{CFRunLoopRunInMode, kCFRunLoopDefaultMode};
    use aterm_objc::{
        Bool, Id, Obj, Sel, WeakObj, WeakSlot, autoreleasepool, checked_msg, class, ns_string, sel,
    };
    use aterm_window::application::ApplicationHandler;
    use aterm_window::event::WindowEvent;
    use aterm_window::event_loop::{ActiveEventLoop, EventLoopProxy};
    use aterm_window::platform::macos::{EventLoopExtMacOS as _, MonitorHandleExtMacOS};
    use aterm_window::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
    use aterm_window::window::{Theme, Window, WindowId};

    use super::{FAIL, NOT_RUN, PASS};

    /// Long enough for macOS to launch its `NSApplication`, hand back a window
    /// and run every stage; past it the drive reports NOT RUN.
    const BUDGET: Duration = Duration::from_secs(60);
    /// Re-executes this driver as the process that AppKit is expected to terminate.
    pub const TERMINATE_REENTRY_CHILD_ENV: &str = "ATERM_OBJC_WINDOW_TERMINATE_REENTRY_CHILD";
    /// Re-executes this driver around an API exit requested behind modal-deferred callbacks.
    pub const MODAL_EXIT_ORDER_CHILD_ENV: &str = "ATERM_OBJC_WINDOW_MODAL_EXIT_ORDER_CHILD";
    /// Re-executes the same modal exit through the production `EventLoop::run_app` path.
    pub const MODAL_EXIT_RUN_APP_CHILD_ENV: &str = "ATERM_OBJC_WINDOW_MODAL_EXIT_RUN_APP_CHILD";
    /// Re-executes this driver around exit requested by a prepared loop's first `Resumed`.
    pub const PREPARED_EXIT_CHILD_ENV: &str = "ATERM_OBJC_WINDOW_PREPARED_EXIT_CHILD";
    /// Re-executes this driver around the panic which a deferred callback must propagate.
    pub const DEFERRED_PANIC_CHILD_ENV: &str = "ATERM_OBJC_WINDOW_DEFERRED_PANIC_CHILD";
    /// Re-executes this driver as a process that takes AppKit's quit up (`NSTerminateLater`) and
    /// asks the loop to exit while AppKit waits on its reply.
    pub const DEFERRED_QUIT_CHILD_ENV: &str = "ATERM_OBJC_WINDOW_DEFERRED_QUIT_CHILD";
    /// Re-executes this driver as the deferred-quit child whose exit, asked for from a redraw
    /// outside the run loop's observers, meets a second `terminate:` before any observer commits
    /// it: AppKit then terminates at once (`applicationWillTerminate:`).
    pub const DEFERRED_QUIT_FORCED_CHILD_ENV: &str = "ATERM_OBJC_WINDOW_DEFERRED_QUIT_FORCED_CHILD";
    /// Re-executes this driver as the deferred-quit child whose owed redraw cancels the quit, as
    /// a tab's question answered "Cancel" does, while its exit is already asked for.
    pub const DEFERRED_QUIT_CANCEL_CHILD_ENV: &str = "ATERM_OBJC_WINDOW_DEFERRED_QUIT_CANCEL_CHILD";
    /// The cancelling child's line: whether its cancel sent AppKit a refusal.
    const DEFERRED_QUIT_CANCEL_LINE: &str =
        "objc-window-drive deferred-quit child: the owed redraw's cancel sent a refusal:";

    /// How the deferred-quit child exits while AppKit waits on its quit's reply.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub enum DeferredQuit {
        /// The exit, behind a redraw owed first, commits from the run loop's observer.
        #[default]
        Observed,
        /// The exit, asked for from a redraw outside the observers, meets a second
        /// `terminate:` before it commits.
        Forced,
        /// The owed redraw cancels the quit after the exit was asked for: the exit still ends
        /// the process, so no refusal goes to AppKit.
        Cancelled,
    }

    impl DeferredQuit {
        pub const ALL: [Self; 3] = [Self::Observed, Self::Forced, Self::Cancelled];

        pub const fn child_env(self) -> &'static str {
            match self {
                Self::Observed => DEFERRED_QUIT_CHILD_ENV,
                Self::Forced => DEFERRED_QUIT_FORCED_CHILD_ENV,
                Self::Cancelled => DEFERRED_QUIT_CANCEL_CHILD_ENV,
            }
        }
    }
    /// The deferred-quit child's line for each `LoopExiting`: its count, whether the redraw owed
    /// before the exit and the window's `Destroyed` reached the handler first, and whether it
    /// came inside AppKit's `terminate:`.
    const DEFERRED_QUIT_EXIT_LINE: &str = "objc-window-drive deferred-quit child: LoopExiting";
    /// Runs only the watched parent for the exact Rename This Mac re-entry fixture.
    pub const RENAME_REENTRY_PARENT_ENV: &str = "ATERM_OBJC_WINDOW_RENAME_REENTRY_PARENT";
    /// Re-executes the real Rename This Mac field editor inside production `run_app`.
    pub const RENAME_REENTRY_CHILD_ENV: &str = "ATERM_OBJC_WINDOW_RENAME_REENTRY_CHILD";
    /// A whole-child hang detector covering startup and inner waits, not a latency budget.
    const CHILD_WATCHDOG: Duration = Duration::from_secs(60);
    /// Lets a deferred-handler child fail with its own diagnostic before the parent watchdog.
    const DEFERRED_HANDLER_BUDGET: Duration = Duration::from_secs(10);
    /// The exact payload which must return from the deferred handler through `pump_app_events`.
    const DEFERRED_HANDLER_PANIC_MARKER: &str = "objc-window-drive deferred handler panic marker";
    /// Evidence emitted by the historical eager focus callback before Objective-C aborts.
    const RENAME_REENTRY_PANIC_MARKER: &str =
        "tried to handle event while another event is currently being handled";
    /// The Objective-C unwind boundary which turns that historical panic into SIGABRT.
    const RENAME_REENTRY_ABORT_MARKER: &str =
        "aterm-objc: panic escaped Objective-C method `block invoke`; aborting";

    /// `NSDragOperationCopy`, the value `draggingEntered:` must answer for a
    /// pasteboard carrying filenames.
    const NS_DRAG_OPERATION_COPY: usize = 1;
    /// `NSDragOperationNone`, for a pasteboard carrying none.
    const NS_DRAG_OPERATION_NONE: usize = 0;

    /// `NSEventTypeKeyUp` and `NSEventModifierFlagCommand`, spelled here so the
    /// driver's expectation is not the port's constant.
    const NS_EVENT_TYPE_KEY_UP: usize = 11;
    const NS_EVENT_MODIFIER_FLAG_COMMAND: usize = 1 << 20;

    /// `NSAlertFirstButtonReturn`, what `-runModal` answers for a click on the first button.
    const NS_ALERT_FIRST_BUTTON_RETURN: isize = 1000;

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
        // SAFETY: `-title` is `@16@0:8` on `NSWindow` and never answers nil.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            aterm_objc::ns_string_to_rust(f(window, sel!(title)))
        }
    }

    /// # Safety
    /// `obj` must be a live object.
    unsafe fn class_name_of(obj: Id) -> String {
        // SAFETY: `-className` is `@16@0:8` on every `NSObject` and answers its class's name (a
        // KVO subclass answers the class it was made from).
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            aterm_objc::ns_string_to_rust(f(obj, sel!(className)))
        }
    }

    /// Every window in `-[NSApp windows]`, in its order: its address and its class name.
    fn app_windows() -> Vec<(usize, String)> {
        // SAFETY: `+sharedApplication` and `-windows` are `@16@0:8` (+0), `-count` is
        // `Q16@0:8` and `-objectAtIndex:` is `@24@0:8Q16` (+0) on the live array.
        unsafe {
            let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let count: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
            let at: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Id = checked_msg();
            let app = get(class(c"NSApplication").as_id(), sel!(sharedApplication));
            let windows = get(app, sel!(windows));
            (0..count(windows, sel!(count)))
                .map(|i| {
                    let window = at(windows, sel!(objectAtIndex:), i);
                    (window.addr(), class_name_of(window))
                })
                .collect()
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
        // SAFETY: `+mainScreen` is `@16#0:8` and may answer nil;
        // `-backingScaleFactor` is `d16@0:8`.
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

    /// Invoke the ported view's real AppKit drawing entry point.
    ///
    /// # Safety
    /// `view` must be a live WinitView.
    unsafe fn send_draw_rect(view: Id) {
        // SAFETY: `-drawRect:` is `v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16`.
        unsafe {
            let draw: unsafe extern "C-unwind" fn(Id, Sel, aterm_objc::CGRect) = checked_msg();
            draw(view, sel!(drawRect:), aterm_objc::CGRect::default());
        }
    }

    /// Whether the forced deferred-quit child's `drawRect:` comes from its target now, so its
    /// handler tells that redraw from any other.
    static FORCED_DRAW_FROM_TARGET: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    aterm_objc::declare_class! {
        /// The forced deferred-quit child's run-loop source: draws the view outside the event
        /// handler, whose redraw asks for the exit, then sends a second `terminate:` in the same
        /// callout, before any observer could commit that exit.
        struct ATermDrawThenTerminateTarget: NSObject {
            const NAME: &str = "ATermObjcWindowDriveDrawThenTerminateTarget";
            type Ivars = Id;

            @sel(drawThenTerminate:)
            fn draw_then_terminate(&self, _sender: Id) {
                use std::sync::atomic::Ordering;
                FORCED_DRAW_FROM_TARGET.store(true, Ordering::SeqCst);
                // SAFETY: the ivar is the live WinitView retained by its window.
                unsafe { send_draw_rect(*self.ivars()) };
                FORCED_DRAW_FROM_TARGET.store(false, Ordering::SeqCst);
                // SAFETY: the shared `NSApplication` is live; `terminate:` is `v24@0:8@16`, and
                // nil is the conventional programmatic sender.
                unsafe {
                    let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
                    let terminate: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
                    let app = get(class(c"NSApplication").as_id(), sel!(sharedApplication));
                    terminate(app, sel!(terminate:), Id::NIL);
                }
            }
        }
    }

    aterm_objc::declare_class! {
        /// Turns an object-only delayed selector into the `CGRect` argument `drawRect:` needs.
        struct ATermDrawRectTarget: NSObject {
            const NAME: &str = "ATermObjcWindowDriveDrawRectTarget";
            type Ivars = Id;

            @sel(drawWinitView:)
            fn draw_winit_view(&self, _sender: Id) {
                // SAFETY: the ivar is the live WinitView retained by its window.
                unsafe { send_draw_rect(*self.ivars()) };
            }
        }
    }

    struct DeferredPanicChild {
        window: Option<Window>,
        attempted: bool,
        panic_on_redraw: bool,
        failed: Option<String>,
    }

    impl ApplicationHandler<()> for DeferredPanicChild {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let attrs = Window::default_attributes()
                .with_title("objc-window-drive deferred-panic child")
                .with_visible(false);
            match el.create_window(attrs) {
                Ok(window) => self.window = Some(window),
                Err(error) => {
                    self.failed = Some(format!("no child window could be created: {error}"));
                    el.exit();
                }
            }
        }

        fn about_to_wait(&mut self, el: &ActiveEventLoop) {
            if self.attempted || self.failed.is_some() || self.window.is_none() {
                return;
            }
            let view = self
                .window
                .as_ref()
                .map_or(Id::NIL, aterm_objc::HasNsView::ns_view);
            if view.is_null() {
                self.failed = Some("the child window has no WinitView".to_owned());
                el.exit();
                return;
            }

            // The direct AppKit callback runs while AboutToWait owns the event handler. It must
            // therefore enqueue RedrawRequested; invoking the handler inline would reborrow it.
            self.attempted = true;
            self.panic_on_redraw = true;
            eprintln!("deferred-panic child: invoking drawRect: from AboutToWait");
            // SAFETY: the child window owns this live WinitView.
            unsafe { send_draw_rect(view) };
        }

        fn window_event(
            &mut self,
            _el: &ActiveEventLoop,
            _window_id: WindowId,
            event: WindowEvent,
        ) {
            if self.panic_on_redraw && matches!(event, WindowEvent::RedrawRequested) {
                self.panic_on_redraw = false;
                std::panic::panic_any(DEFERRED_HANDLER_PANIC_MARKER);
            }
        }
    }

    struct TerminateReentryChild {
        window: Option<Window>,
        attempted: bool,
        destroyed: bool,
        failed: Option<String>,
    }

    impl ApplicationHandler<()> for TerminateReentryChild {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let attrs = Window::default_attributes()
                .with_title("objc-window-drive terminate-reentry child")
                .with_visible(false);
            match el.create_window(attrs) {
                Ok(window) => self.window = Some(window),
                Err(error) => {
                    self.failed = Some(format!("no child window could be created: {error}"));
                    el.exit();
                }
            }
        }

        fn about_to_wait(&mut self, el: &ActiveEventLoop) {
            if self.attempted || self.failed.is_some() || self.window.is_none() {
                return;
            }
            self.attempted = true;
            eprintln!("terminate child: entering modal alert from AboutToWait");
            if let Err(why) = modal_terminate_attempt() {
                self.failed = Some(why);
                el.exit();
            } else {
                eprintln!("terminate child: modal alert returned; AboutToWait can now return");
            }
        }

        fn window_event(
            &mut self,
            _el: &ActiveEventLoop,
            _window_id: WindowId,
            event: WindowEvent,
        ) {
            if matches!(event, WindowEvent::Destroyed) {
                self.destroyed = true;
            }
        }

        fn exiting(&mut self, _el: &ActiveEventLoop) {
            if !self.destroyed {
                eprintln!(
                    "objc-window-drive terminate child: FAIL — LoopExiting arrived before the \
                     child window's Destroyed event"
                );
                std::process::exit(FAIL);
            }
        }
    }

    /// Whether the deferred-quit child's handler took AppKit's quit up (`Later`).
    static DEFERRED_QUIT_TAKEN: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    /// Whether AppKit asked the deferred-quit child where no reply could come.
    static DEFERRED_QUIT_UNANSWERABLE: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    /// The deferred-quit child's `applicationShouldTerminate:` answer: the quit taken up, as
    /// aterm takes up a logout, restart or Quit & Reopen, where a reply can come.
    fn take_the_quit_up(
        wait: aterm_window::platform::macos::ReplyWait,
    ) -> aterm_window::platform::macos::TerminateAnswer {
        use aterm_window::platform::macos::{ReplyWait, TerminateAnswer};
        use std::sync::atomic::Ordering;
        if wait == ReplyWait::Answerable {
            DEFERRED_QUIT_TAKEN.store(true, Ordering::SeqCst);
            TerminateAnswer::Later
        } else {
            DEFERRED_QUIT_UNANSWERABLE.store(true, Ordering::SeqCst);
            TerminateAnswer::Cancel
        }
    }

    /// Sends `-[NSApp terminate:]` from the run loop's default mode, outside the event handler,
    /// where a logout's quit event reaches it.
    fn schedule_terminate() {
        // SAFETY: the shared `NSApplication` is live; `performSelector:withObject:afterDelay:`
        // takes a selector, an object (nil) and a delay in seconds.
        unsafe {
            let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let later: unsafe extern "C-unwind" fn(Id, Sel, Sel, Id, f64) = checked_msg();
            let app = get(class(c"NSApplication").as_id(), sel!(sharedApplication));
            later(
                app,
                sel!(performSelector:withObject:afterDelay:),
                sel!(terminate:),
                Id::NIL,
                0.05,
            );
        }
    }

    /// Schedules the forced child's draw-then-terminate in AppKit's wait, whose run loop turns in
    /// `NSModalPanelRunLoopMode`.
    fn schedule_draw_then_terminate(target: Id) -> Result<(), String> {
        let mode = ns_string("NSModalPanelRunLoopMode")
            .ok_or_else(|| "Foundation rejected the modal-panel run-loop mode".to_owned())?;
        // SAFETY: `target` is a live `ATermDrawThenTerminateTarget`, which the delayed send
        // retains until it runs; `arrayWithObject:` takes one object and
        // `performSelector:withObject:afterDelay:inModes:` a selector, an object (nil), a delay
        // and an array of mode names.
        unsafe {
            let one: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
            let later: unsafe extern "C-unwind" fn(Id, Sel, Sel, Id, f64, Id) = checked_msg();
            let modes = one(class(c"NSArray").as_id(), sel!(arrayWithObject:), mode.id());
            later(
                target,
                sel!(performSelector:withObject:afterDelay:inModes:),
                sel!(drawThenTerminate:),
                Id::NIL,
                0.0,
                modes,
            );
        }
        Ok(())
    }

    #[derive(Default)]
    struct DeferredQuitChild {
        mode: DeferredQuit,
        window: Option<Window>,
        target: Option<aterm_objc::Retained<ATermDrawThenTerminateTarget>>,
        failed: Option<String>,
        scheduled: bool,
        exit_requested: bool,
        owed_redraw_queued: bool,
        owed_redraw_ran: bool,
        destroyed: bool,
        exits: usize,
    }

    impl DeferredQuitChild {
        /// Records a failure, told at once: one inside AppKit's wait never reaches the pump.
        fn fail(&mut self, why: String) {
            eprintln!("objc-window-drive deferred-quit child: {why}");
            self.failed = Some(why);
        }

        fn view(&self) -> Id {
            self.window
                .as_ref()
                .map_or(Id::NIL, aterm_objc::HasNsView::ns_view)
        }

        /// Queues a redraw behind the handler running now, then asks for the exit: the redraw is
        /// owed before the exit commits.
        fn queue_owed_redraw_and_exit(&mut self, el: &ActiveEventLoop) {
            let view = self.view();
            if view.is_null() {
                self.fail("the child window has no WinitView".to_owned());
                return;
            }
            self.owed_redraw_queued = true;
            // SAFETY: the child window owns this live WinitView; the handler runs, so the redraw
            // is queued behind it.
            unsafe { send_draw_rect(view) };
            self.exit_requested = true;
            el.exit();
        }
    }

    impl ApplicationHandler<()> for DeferredQuitChild {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            if self.window.is_some() || self.failed.is_some() {
                return;
            }
            let attrs = Window::default_attributes()
                .with_title("objc-window-drive deferred-quit child")
                .with_visible(false);
            match el.create_window(attrs) {
                Ok(window) => self.window = Some(window),
                Err(error) => self.fail(format!("no child window could be created: {error}")),
            }
        }

        fn about_to_wait(&mut self, el: &ActiveEventLoop) {
            if self.failed.is_some() || self.window.is_none() {
                return;
            }
            if !self.scheduled {
                self.scheduled = true;
                schedule_terminate();
                return;
            }
            // AppKit's wait on the reply turns the loop in its modal mode, whose observers still
            // bring this callback: the application's own shutdown asks for the exit from here,
            // behind a redraw owed first, or (forced) from a redraw outside the observers, which
            // a second `terminate:` follows.
            if !DEFERRED_QUIT_TAKEN.load(std::sync::atomic::Ordering::SeqCst) || self.exit_requested
            {
                return;
            }
            if self.mode != DeferredQuit::Forced {
                eprintln!("objc-window-drive deferred-quit child: AppKit waits on the reply; exit");
                self.queue_owed_redraw_and_exit(el);
                return;
            }
            if self.target.is_some() {
                return;
            }
            let Some(mtm) = aterm_objc::MainThread::new() else {
                self.fail("the forced child left the main thread".to_owned());
                return;
            };
            let Some(target) = ATermDrawThenTerminateTarget::alloc_init(mtm, self.view()) else {
                self.fail("AppKit did not make the draw-then-terminate target".to_owned());
                return;
            };
            if let Err(why) = schedule_draw_then_terminate(target.as_id()) {
                self.fail(why);
                return;
            }
            eprintln!(
                "objc-window-drive deferred-quit child: AppKit waits on the reply; a redraw asks \
                 for the exit, then a second terminate:"
            );
            self.target = Some(target);
        }

        fn window_event(&mut self, el: &ActiveEventLoop, _window_id: WindowId, event: WindowEvent) {
            match event {
                WindowEvent::RedrawRequested if self.owed_redraw_queued => {
                    self.owed_redraw_ran = true;
                    if self.mode == DeferredQuit::Cancelled {
                        let sent = aterm_window::platform::macos::refuse_deferred_termination();
                        eprintln!("{DEFERRED_QUIT_CANCEL_LINE} {sent}");
                    }
                }
                WindowEvent::RedrawRequested
                    if self.mode == DeferredQuit::Forced
                        && FORCED_DRAW_FROM_TARGET.load(std::sync::atomic::Ordering::SeqCst) =>
                {
                    self.queue_owed_redraw_and_exit(el);
                }
                WindowEvent::Destroyed => self.destroyed = true,
                _ => {}
            }
        }

        fn exiting(&mut self, _el: &ActiveEventLoop) {
            self.exits += 1;
            eprintln!(
                "{DEFERRED_QUIT_EXIT_LINE} {} owed redraw ran: {}, window destroyed: {}, inside \
                 AppKit: {}",
                self.exits,
                self.owed_redraw_ran,
                self.destroyed,
                aterm_window::platform::macos::exit_ends_in_appkit()
            );
        }
    }

    #[derive(Default)]
    struct PreparedExitChild {
        resumed: usize,
        exiting: usize,
        request_was_immediate: bool,
    }

    impl ApplicationHandler<()> for PreparedExitChild {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            self.resumed += 1;
            el.exit();
            self.request_was_immediate = el.exiting();
        }

        fn window_event(
            &mut self,
            _el: &ActiveEventLoop,
            _window_id: WindowId,
            _event: WindowEvent,
        ) {
        }

        fn exiting(&mut self, _el: &ActiveEventLoop) {
            self.exiting += 1;
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum ModalExitMarker {
        DeferredFocus,
        DeferredRedraw,
        PrimaryDestroyed,
        ProbeDestroyed,
        LoopExiting,
    }

    #[derive(Default)]
    struct ModalExitOrderChild {
        primary: Option<Window>,
        probe: Option<Window>,
        attempted: bool,
        ordering_armed: bool,
        markers: Vec<ModalExitMarker>,
        passed: bool,
        unavailable: Option<String>,
        ordering_failure: Option<String>,
    }

    impl ModalExitOrderChild {
        fn primary_delegate(&self) -> Option<Id> {
            let view = self
                .primary
                .as_ref()
                .map_or(Id::NIL, aterm_objc::HasNsView::ns_view);
            if view.is_null() {
                return None;
            }
            // SAFETY: `view` is the primary window's live WinitView and `-window` is
            // `@16@0:8` on NSView.
            let window = unsafe {
                let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
                get(view, sel!(window))
            };
            if window.is_null() {
                return None;
            }
            // SAFETY: `window` is the live WinitWindow hosting `view`.
            let delegate = unsafe { window_delegate(window) };
            (!delegate.is_null()).then_some(delegate)
        }

        fn record_destroyed(&mut self, id: WindowId) {
            if self.primary.as_ref().map(Window::id) == Some(id) {
                self.markers.push(ModalExitMarker::PrimaryDestroyed);
            } else if self.probe.as_ref().map(Window::id) == Some(id) {
                self.markers.push(ModalExitMarker::ProbeDestroyed);
            }
        }
    }

    impl ApplicationHandler<()> for ModalExitOrderChild {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            if self.primary.is_some() || self.unavailable.is_some() {
                return;
            }
            let primary = Window::default_attributes()
                .with_title("objc-window-drive modal-exit primary")
                .with_visible(false);
            let probe = Window::default_attributes()
                .with_title("objc-window-drive modal-exit probe")
                .with_visible(false);
            match el.create_window(primary) {
                Ok(window) => self.primary = Some(window),
                Err(error) => {
                    self.unavailable = Some(format!("no primary child window: {error}"));
                    el.exit();
                    return;
                }
            }
            match el.create_window(probe) {
                Ok(window) => self.probe = Some(window),
                Err(error) => {
                    self.unavailable = Some(format!("no probe child window: {error}"));
                    el.exit();
                    return;
                }
            }
            let Some(delegate) = self.primary_delegate() else {
                self.unavailable = Some("the primary child has no AppKit delegate".to_owned());
                el.exit();
                return;
            };
            let probe_view = self
                .probe
                .as_ref()
                .map_or(Id::NIL, aterm_objc::HasNsView::ns_view);
            if probe_view.is_null() {
                self.unavailable = Some("the probe child has no WinitView".to_owned());
                el.exit();
                return;
            }

            self.attempted = true;
            eprintln!("modal-exit child: entering modal alert from prepared-loop Resumed");
            if let Err(why) = modal_exit_attempt(delegate, probe_view) {
                self.unavailable = Some(why);
                el.exit();
                return;
            }

            // The forced delegate callback ran while this method owned the handler. It must be
            // queued, not re-entered. The exit request is the FIFO boundary under test.
            if !self.markers.is_empty() {
                self.ordering_failure = Some(format!(
                    "a modal callback re-entered Resumed: {:?}",
                    self.markers
                ));
            }
            self.ordering_armed = true;
            el.exit();
            if !el.exiting() {
                self.ordering_failure = Some(
                    "ActiveEventLoop::exiting() stayed false after exit() returned".to_owned(),
                );
            }
        }

        fn window_event(&mut self, _el: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
            if !self.ordering_armed {
                return;
            }
            match event {
                WindowEvent::Focused(false)
                    if self.primary.as_ref().map(Window::id) == Some(window_id) =>
                {
                    self.markers.push(ModalExitMarker::DeferredFocus);
                }
                WindowEvent::RedrawRequested
                    if self.probe.as_ref().map(Window::id) == Some(window_id) =>
                {
                    self.markers.push(ModalExitMarker::DeferredRedraw);
                }
                WindowEvent::Destroyed => self.record_destroyed(window_id),
                _ => {}
            }
        }

        fn exiting(&mut self, _el: &ActiveEventLoop) {
            if !self.ordering_armed {
                return;
            }
            self.markers.push(ModalExitMarker::LoopExiting);
            let callback_len = self.markers.len().saturating_sub(3);
            let (callbacks, tail) = self.markers.split_at(callback_len);
            let callbacks_ordered = callbacks.iter().all(|marker| {
                matches!(
                    marker,
                    ModalExitMarker::DeferredFocus | ModalExitMarker::DeferredRedraw
                )
            }) && callbacks.contains(&ModalExitMarker::DeferredFocus)
                && callbacks.contains(&ModalExitMarker::DeferredRedraw);
            let tail_ordered = matches!(
                tail,
                [
                    ModalExitMarker::PrimaryDestroyed,
                    ModalExitMarker::ProbeDestroyed,
                    ModalExitMarker::LoopExiting,
                ] | [
                    ModalExitMarker::ProbeDestroyed,
                    ModalExitMarker::PrimaryDestroyed,
                    ModalExitMarker::LoopExiting,
                ]
            );
            if callbacks_ordered && tail_ordered && self.ordering_failure.is_none() {
                self.passed = true;
            } else if self.ordering_failure.is_none() {
                self.ordering_failure = Some(format!(
                    "event order was {:?}, not one or more deferred focus/redraw callbacks \
                     including both kinds, both Destroyed events, then LoopExiting",
                    self.markers
                ));
            }
        }
    }

    /// Queues Winit focus and redraw callbacks in an alert's nested modal loop. The outer
    /// prepared-loop `Resumed` calls `ActiveEventLoop::exit` as soon as this returns.
    fn modal_exit_attempt(delegate: Id, probe_view: Id) -> Result<(), String> {
        let message = ns_string("Exercise API exit after a modal callback")
            .ok_or_else(|| "Foundation rejected the modal-exit alert's message".to_owned())?;
        let quit = ns_string("Exit")
            .ok_or_else(|| "Foundation rejected the modal-exit alert's button".to_owned())?;
        let mode = ns_string("NSModalPanelRunLoopMode")
            .ok_or_else(|| "Foundation rejected the modal-exit run-loop mode".to_owned())?;

        // SAFETY: `+new` answers a +1 NSAlert, the setters have their documented signatures,
        // and the alert retains the button through `-runModal`.
        let made = unsafe {
            let new: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let set: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
            let add: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
            Obj::from_owned(new(class(c"NSAlert").as_id(), sel!(new))).and_then(|alert| {
                set(alert.id(), sel!(setMessageText:), message.id());
                let button = Obj::retain(add(alert.id(), sel!(addButtonWithTitle:), quit.id()))?;
                Some((alert, button))
            })
        }
        .ok_or_else(|| "AppKit did not make the modal-exit alert".to_owned())?;
        let (alert, button) = made;
        let mtm = aterm_objc::MainThread::new()
            .ok_or_else(|| "the modal-exit child left the main thread".to_owned())?;
        let draw_target = ATermDrawRectTarget::alloc_init(mtm, probe_view)
            .ok_or_else(|| "AppKit did not make the modal-exit draw target".to_owned())?;

        // SAFETY: every receiver remains live through `-runModal`. Delayed sends are restricted
        // to the modal mode. `windowDidResignKey:`, `drawWinitView:`, and `performClick:` each
        // accept one object; `abortModal` is the bounded fallback.
        let answer = unsafe {
            let one: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
            let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let later: unsafe extern "C-unwind" fn(Id, Sel, Sel, Id, f64, Id) = checked_msg();
            let run: unsafe extern "C-unwind" fn(Id, Sel) -> isize = checked_msg();
            let cancel: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
            let modes = one(class(c"NSArray").as_id(), sel!(arrayWithObject:), mode.id());
            let app = get(class(c"NSApplication").as_id(), sel!(sharedApplication));
            later(
                delegate,
                sel!(performSelector:withObject:afterDelay:inModes:),
                sel!(windowDidResignKey:),
                Id::NIL,
                0.1,
                modes,
            );
            later(
                draw_target.as_id(),
                sel!(performSelector:withObject:afterDelay:inModes:),
                sel!(drawWinitView:),
                Id::NIL,
                0.15,
                modes,
            );
            later(
                button.id(),
                sel!(performSelector:withObject:afterDelay:inModes:),
                sel!(performClick:),
                Id::NIL,
                0.2,
                modes,
            );
            later(
                app,
                sel!(performSelector:withObject:afterDelay:inModes:),
                sel!(abortModal),
                Id::NIL,
                5.0,
                modes,
            );
            let answer = run(alert.id(), sel!(runModal));
            cancel(
                class(c"NSObject").as_id(),
                sel!(cancelPreviousPerformRequestsWithTarget:),
                app,
            );
            cancel(
                class(c"NSObject").as_id(),
                sel!(cancelPreviousPerformRequestsWithTarget:),
                draw_target.as_id(),
            );
            answer
        };
        if answer == NS_ALERT_FIRST_BUTTON_RETURN {
            Ok(())
        } else {
            Err(format!(
                "the modal-exit alert answered {answer}, not {NS_ALERT_FIRST_BUTTON_RETURN}"
            ))
        }
    }

    /// Requests native termination from an alert's nested modal loop, while the caller's
    /// `AboutToWait` handler still owns the event handler's mutable borrow.
    fn modal_terminate_attempt() -> Result<(), String> {
        let message = ns_string("Exercise termination inside a modal event callback")
            .ok_or_else(|| "Foundation rejected the child alert's message".to_owned())?;
        let quit = ns_string("Continue termination")
            .ok_or_else(|| "Foundation rejected the child alert's button".to_owned())?;
        let mode = ns_string("NSModalPanelRunLoopMode")
            .ok_or_else(|| "Foundation rejected the modal run-loop mode".to_owned())?;

        // SAFETY: `+new` answers a +1 `NSAlert`; both setters have their declared AppKit
        // signatures, and the alert retains the button across `-runModal`.
        let made = unsafe {
            let new: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let set: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
            let add: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
            Obj::from_owned(new(class(c"NSAlert").as_id(), sel!(new))).and_then(|alert| {
                set(alert.id(), sel!(setMessageText:), message.id());
                let button = Obj::retain(add(alert.id(), sel!(addButtonWithTitle:), quit.id()))?;
                Some((alert, button))
            })
        }
        .ok_or_else(|| "AppKit did not make the child alert".to_owned())?;
        let (alert, button) = made;

        // SAFETY: all receivers remain live through `-runModal`; the delayed sends are confined
        // to the alert's modal mode. `terminate:` and `performClick:` each take one object;
        // `abortModal` is the bounded fallback if the click is not delivered.
        let answer = unsafe {
            let one: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
            let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let later: unsafe extern "C-unwind" fn(Id, Sel, Sel, Id, f64, Id) = checked_msg();
            let run: unsafe extern "C-unwind" fn(Id, Sel) -> isize = checked_msg();
            let cancel: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
            let modes = one(class(c"NSArray").as_id(), sel!(arrayWithObject:), mode.id());
            let app = get(class(c"NSApplication").as_id(), sel!(sharedApplication));
            later(
                app,
                sel!(performSelector:withObject:afterDelay:inModes:),
                sel!(terminate:),
                Id::NIL,
                0.1,
                modes,
            );
            later(
                button.id(),
                sel!(performSelector:withObject:afterDelay:inModes:),
                sel!(performClick:),
                Id::NIL,
                0.2,
                modes,
            );
            later(
                app,
                sel!(performSelector:withObject:afterDelay:inModes:),
                sel!(abortModal),
                Id::NIL,
                5.0,
                modes,
            );
            let answer = run(alert.id(), sel!(runModal));
            cancel(
                class(c"NSObject").as_id(),
                sel!(cancelPreviousPerformRequestsWithTarget:),
                app,
            );
            answer
        };
        if answer == NS_ALERT_FIRST_BUTTON_RETURN {
            Ok(())
        } else {
            Err(format!(
                "the child alert answered {answer}, not {NS_ALERT_FIRST_BUTTON_RETURN}"
            ))
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
        /// A permanently hidden Winit window whose view isolates the modal redraw probe from
        /// AppKit redraws of the primary window.
        redraw_probe: Option<Window>,
        proxy: Option<EventLoopProxy<u8>>,
        proxy_events: Vec<u8>,
        report: Report,
        resized: usize,
        moved: usize,
        focused: Vec<bool>,
        close_requested: usize,
        hovered: Vec<String>,
        /// Key-up events that came back through the swizzled `-sendEvent:`.
        key_ups: usize,
        posted_cmd_key_up: bool,
        /// What a click on the stage-16 alert's button made `-runModal` answer.
        alert_answer: Option<isize>,
        /// Focus events delivered before the modal-reentry stage makes the winit window resign key
        /// while its `about_to_wait` handler is inside an alert's nested modal run loop.
        modal_focus_before: Option<usize>,
        /// Probe-window redraw events delivered before that modal loop invokes its `drawRect:`.
        modal_probe_redraw_before: Option<usize>,
        probe_redraws: usize,
        /// The windows in `-[NSApp windows]` when the loop was told to exit, by class name.
        windows_at_exit: Vec<String>,
        /// The answered alert's own panel was one of [`Self::windows_at_exit`], by identity.
        alert_panel_listed: bool,
        /// Each window that posted `NSWindowWillCloseNotification` once the loop was told to
        /// exit, by class name.
        closed_at_exit: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
        /// The observer recording [`Self::closed_at_exit`], removed after the pump.
        exit_close_observer: Option<Obj>,
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

        /// The window's own content `NSView`.
        fn ns_view(&self) -> Id {
            self.window
                .as_ref()
                .map_or(Id::NIL, aterm_objc::HasNsView::ns_view)
        }

        /// The hidden redraw probe's content `NSView`.
        fn redraw_probe_view(&self) -> Id {
            self.redraw_probe
                .as_ref()
                .map_or(Id::NIL, aterm_objc::HasNsView::ns_view)
        }
    }

    impl ApplicationHandler<u8> for Driver {
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
                    return;
                }
            }

            let probe_attrs = Window::default_attributes()
                .with_title("objc-window-drive redraw probe")
                .with_inner_size(aterm_window::dpi::LogicalSize::new(32.0, 32.0))
                .with_visible(false);
            match el.create_window(probe_attrs) {
                Ok(w) => self.redraw_probe = Some(w),
                Err(e) => {
                    self.report.blocked = Some(format!(
                        "no hidden redraw-probe window could be created: {e}"
                    ));
                    self.done = true;
                    el.exit();
                }
            }
        }

        fn window_event(&mut self, _el: &ActiveEventLoop, id: WindowId, e: WindowEvent) {
            if self.redraw_probe.as_ref().map(|window| window.id()) == Some(id) {
                if matches!(e, WindowEvent::RedrawRequested) {
                    self.probe_redraws += 1;
                }
                return;
            }
            if self.window.as_ref().map(|window| window.id()) != Some(id) {
                return;
            }
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

        fn user_event(&mut self, _el: &ActiveEventLoop, event: u8) {
            self.proxy_events.push(event);
        }

        fn about_to_wait(&mut self, el: &ActiveEventLoop) {
            if self.done {
                return;
            }
            if self.stage == 15 && self.proxy_events.len() < 2 {
                // The survivor's real send must wake and drain the loop. The
                // outer one-minute budget detects a missing delivery.
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
                15 => {
                    if !self.stage_proxy_lifetime() {
                        self.done = true;
                        el.exit();
                    }
                }
                16 => self.stage_modal_event_reentry(delegate),
                _ => {
                    self.report.check(
                        self.proxy_events == [17, 29],
                        &format!(
                            "the surviving proxy delivers each event once in order: {:?}",
                            self.proxy_events
                        ),
                    );
                    let focus_before = self.modal_focus_before.unwrap_or(self.focused.len());
                    self.report.check(
                        self.focused.len() > focus_before,
                        &format!(
                            "the focus callback raised inside the alert was delivered after its \
                             outer handler returned ({} before, {} after)",
                            focus_before,
                            self.focused.len()
                        ),
                    );
                    let redraw_before =
                        self.modal_probe_redraw_before.unwrap_or(self.probe_redraws);
                    self.report.check(
                        self.probe_redraws == redraw_before + 1,
                        &format!(
                            "the hidden probe's drawRect callback raised inside the alert was \
                             delivered exactly once after its outer handler returned ({} before, \
                             {} after)",
                            redraw_before, self.probe_redraws
                        ),
                    );
                    self.stage_alert_answered_in_the_exit_turn();
                    self.done = true;
                    el.exit();
                }
            }
        }
    }

    impl Driver {
        fn stage_proxy_lifetime(&mut self) -> bool {
            println!("\n[15] event proxy — a clone outlives its original and another clone");
            let original = self.proxy.take().expect("the driver owns a proxy");
            let intermediate = original.clone();
            let survivor = intermediate.clone();
            drop(original);
            drop(intermediate);
            let queued = survivor.send_event(17).is_ok() && survivor.send_event(29).is_ok();
            self.report
                .check(queued, "the surviving proxy accepts both real sends");
            self.proxy = Some(survivor);
            queued
        }

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

        /// The fullscreen state machine, leaving.
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

        /// `aterm_objc::weak` against the objects it exists for.
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
                // SAFETY: both slots are `#[repr(transparent)]` over one pointer word, live locals
                // that do not overlap.
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
                // SAFETY: `i` indexes the eight rows the menu holds (`n == 8` above).
                let row = unsafe { read_row(menu_item_at(app_menu, i as isize)) };
                self.report.check(
                    row.separator == *separator,
                    &format!("row {i} separator={} (want {separator})", row.separator),
                );
                if *separator {
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
            if all.is_empty()
                && let Some(why) = aterm_gui::driver_display_unavailable()
            {
                self.report.blocked = Some(why);
                return;
            }
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

        /// Drives the exact nested-loop shape that used to deliver a queued focus event while the
        /// outer `about_to_wait` callback still held the event handler's mutable borrow.
        fn stage_modal_event_reentry(&mut self, delegate: Id) {
            println!("\n[15c] a focus callback inside an NSAlert's nested modal loop");
            let (Some(message), Some(continue_button), Some(mode)) = (
                ns_string("Exercise nested event delivery"),
                ns_string("Continue"),
                ns_string("NSModalPanelRunLoopMode"),
            ) else {
                self.report.check(
                    false,
                    "Foundation accepts the modal-reentry alert's strings",
                );
                return;
            };
            // SAFETY: `+new` is `@16#0:8`; the setters have their declared AppKit signatures;
            // the alert owns its button and both are retained across `-runModal`.
            let made = unsafe {
                let new: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
                let set: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
                let add: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
                Obj::from_owned(new(class(c"NSAlert").as_id(), sel!(new))).and_then(|alert| {
                    set(alert.id(), sel!(setMessageText:), message.id());
                    let button = Obj::retain(add(
                        alert.id(),
                        sel!(addButtonWithTitle:),
                        continue_button.id(),
                    ))?;
                    Some((alert, button))
                })
            };
            let Some((alert, button)) = made else {
                self.report
                    .check(false, "AppKit makes the modal-reentry alert");
                return;
            };
            let Some(mtm) = aterm_objc::MainThread::new() else {
                self.report
                    .check(false, "the modal-reentry stage runs on the main thread");
                return;
            };
            let Some(draw_target) = ATermDrawRectTarget::alloc_init(mtm, self.redraw_probe_view())
            else {
                self.report
                    .check(false, "the modal-reentry draw target can be made");
                return;
            };

            // SAFETY: these are the same selector signatures and live objects documented by the
            // exit-alert stage below. The delayed sends are restricted to the alert's modal mode;
            // `windowDidResignKey:` and the draw target ignore their object arguments, so nil is
            // sufficient. The draw target adapts the delayed selector to `drawRect:`'s CGRect.
            let answer = unsafe {
                let one: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
                let later: unsafe extern "C-unwind" fn(Id, Sel, Sel, Id, f64, Id) = checked_msg();
                let run: unsafe extern "C-unwind" fn(Id, Sel) -> isize = checked_msg();
                let cancel: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
                let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
                let modes = one(class(c"NSArray").as_id(), sel!(arrayWithObject:), mode.id());
                let app = get(class(c"NSApplication").as_id(), sel!(sharedApplication));
                self.modal_focus_before = Some(self.focused.len());
                self.modal_probe_redraw_before = Some(self.probe_redraws);
                later(
                    delegate,
                    sel!(performSelector:withObject:afterDelay:inModes:),
                    sel!(windowDidResignKey:),
                    Id::NIL,
                    0.1,
                    modes,
                );
                later(
                    draw_target.as_id(),
                    sel!(performSelector:withObject:afterDelay:inModes:),
                    sel!(drawWinitView:),
                    Id::NIL,
                    0.15,
                    modes,
                );
                later(
                    button.id(),
                    sel!(performSelector:withObject:afterDelay:inModes:),
                    sel!(performClick:),
                    Id::NIL,
                    0.2,
                    modes,
                );
                later(
                    app,
                    sel!(performSelector:withObject:afterDelay:inModes:),
                    sel!(abortModal),
                    Id::NIL,
                    BUDGET.as_secs_f64(),
                    modes,
                );
                let answer = run(alert.id(), sel!(runModal));
                // A person's gesture can turn the default mode before the outer handler returns.
                // Do so deterministically: the old queue re-entered the handler at this point.
                let _ = CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.01, 1);
                cancel(
                    class(c"NSObject").as_id(),
                    sel!(cancelPreviousPerformRequestsWithTarget:),
                    app,
                );
                cancel(
                    class(c"NSObject").as_id(),
                    sel!(cancelPreviousPerformRequestsWithTarget:),
                    draw_target.as_id(),
                );
                answer
            };
            self.report.check(
                answer == NS_ALERT_FIRST_BUTTON_RETURN,
                &format!("a click answered the modal-reentry alert ({answer}) without re-entry"),
            );
            let redraw_before = self.modal_probe_redraw_before.unwrap_or(self.probe_redraws);
            self.report.check(
                self.probe_redraws == redraw_before,
                &format!(
                    "the hidden probe's drawRect callback did not re-enter its active outer \
                     handler ({} before, {} inside)",
                    redraw_before, self.probe_redraws
                ),
            );
        }

        /// A real `NSAlert` answered by a click in the turn that then exits the loop, as a Quit
        /// confirmation is: the exit (`crates/aterm-window`'s `notify_windows_of_exit`) must close
        /// only winit's own windows. The answered alert's panel (and, on macOS 27, its
        /// `_NSGlassEffectWindow` child) is still in `-[NSApp windows]` until this turn's pool
        /// pops; closing that child freed it under the panel's frame view, and the pool's
        /// release of the frame view then crashed (aterm 0.110.0 on macOS 27.2). No inner pool
        /// here: the alert must go the way the confirmation's does, into the turn's own.
        fn stage_alert_answered_in_the_exit_turn(&mut self) {
            println!(
                "\n[16] an NSAlert answered in the exit turn — winit's event_loop.rs exit close"
            );
            let (Some(message), Some(quit), Some(mode)) = (
                ns_string("Quit objc-window-drive?"),
                ns_string("Quit"),
                ns_string("NSModalPanelRunLoopMode"),
            ) else {
                self.report
                    .check(false, "Foundation accepts the alert's strings");
                return;
            };
            // SAFETY: `+new` is `@16#0:8` and answers a +1 `NSAlert`; `-setMessageText:` is
            // `v24@0:8@16` and copies; `-addButtonWithTitle:` is `@24@0:8@16` and answers the
            // alert's +0 button, retained here.
            let made = unsafe {
                let new: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
                let set: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
                let add: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
                Obj::from_owned(new(class(c"NSAlert").as_id(), sel!(new))).and_then(|alert| {
                    set(alert.id(), sel!(setMessageText:), message.id());
                    let button =
                        Obj::retain(add(alert.id(), sel!(addButtonWithTitle:), quit.id()))?;
                    Some((alert, button))
                })
            };
            let Some((alert, button)) = made else {
                self.report
                    .check(false, "AppKit makes an NSAlert with a Quit button");
                return;
            };
            // Weak, so the check cannot keep the panel alive and listed by itself.
            // SAFETY: `-window` is `@16@0:8` and answers the panel the live alert holds (+0),
            // live and not deallocating while `alert` is.
            let panel = unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
                WeakObj::new(f(alert.id(), sel!(window)))
            };
            // SAFETY: `+arrayWithObject:` is `@24@0:8@16` (+0); `-performSelector:withObject:
            // afterDelay:inModes:` is `v48@0:8:16@24d32@40` on `NSObject`, and the run loop
            // retains its target and modes until it sends; `-runModal` is `q16@0:8`; `+[NSObject
            // cancelPreviousPerformRequestsWithTarget:]` is `v24@0:8@16`.
            let answer = unsafe {
                let one: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
                let later: unsafe extern "C-unwind" fn(Id, Sel, Sel, Id, f64, Id) = checked_msg();
                let run: unsafe extern "C-unwind" fn(Id, Sel) -> isize = checked_msg();
                let cancel: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
                let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
                let modes = one(class(c"NSArray").as_id(), sel!(arrayWithObject:), mode.id());
                let app = get(class(c"NSApplication").as_id(), sel!(sharedApplication));
                // The person's click, once the modal loop runs.
                later(
                    button.id(),
                    sel!(performSelector:withObject:afterDelay:inModes:),
                    sel!(performClick:),
                    Id::NIL,
                    0.2,
                    modes,
                );
                // The hang detector: a click that never answers ends the loop a minute on.
                later(
                    app,
                    sel!(performSelector:withObject:afterDelay:inModes:),
                    sel!(abortModal),
                    Id::NIL,
                    BUDGET.as_secs_f64(),
                    modes,
                );
                let answer = run(alert.id(), sel!(runModal));
                cancel(
                    class(c"NSObject").as_id(),
                    sel!(cancelPreviousPerformRequestsWithTarget:),
                    app,
                );
                answer
            };
            self.alert_answer = Some(answer);
            drop(button);
            drop(alert);
            let listed = app_windows();
            // SAFETY: the word is only compared, never messaged; the runtime nils it once the
            // panel deallocates.
            let panel_at = unsafe { panel.peek() };
            self.alert_panel_listed =
                !panel_at.is_null() && listed.iter().any(|(addr, _)| *addr == panel_at.addr());
            self.windows_at_exit = listed.into_iter().map(|(_, name)| name).collect();

            let closed = std::rc::Rc::clone(&self.closed_at_exit);
            let record = move |note: Id| {
                // SAFETY: the notification centre calls this with the live `NSNotification`,
                // whose `-object` (`@16@0:8`, +0) is the closing window.
                let window = unsafe {
                    let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
                    f(note, sel!(object))
                };
                if !window.is_null() {
                    // SAFETY: `window` is the live window the notification names.
                    closed.borrow_mut().push(unsafe { class_name_of(window) });
                }
            };
            // SAFETY: `void (^)(NSNotification *)` is the block `-addObserverForName:object:
            // queue:usingBlock:` calls.
            let block = unsafe { aterm_objc::RcBlock::new1(record) };
            let Some((block, name)) = block.zip(ns_string("NSWindowWillCloseNotification")) else {
                self.report
                    .check(false, "a will-close observer could be built");
                return;
            };
            // SAFETY: `+defaultCenter` is `@16@0:8` (+0); `-addObserverForName:object:queue:
            // usingBlock:` is `@48@0:8@16@24@32@?40`, copies the block and answers the +0 token,
            // retained here until `-removeObserver:` after the pump.
            self.exit_close_observer = unsafe {
                let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
                let add: unsafe extern "C-unwind" fn(
                    Id,
                    Sel,
                    Id,
                    Id,
                    Id,
                    aterm_objc::BlockPtr,
                ) -> Id = checked_msg();
                let center = get(class(c"NSNotificationCenter").as_id(), sel!(defaultCenter));
                Obj::retain(add(
                    center,
                    sel!(addObserverForName:object:queue:usingBlock:),
                    name.id(),
                    Id::NIL,
                    Id::NIL,
                    block.as_block_ptr(),
                ))
            };
            self.report.note(&format!(
                "the alert answered {answer}; at exit -[NSApp windows] held {:?}",
                self.windows_at_exit
            ));
        }

        /// What the exit closed, against stage 16's alert.
        fn judge_exit_closes(&mut self) {
            println!("\n[16b] the windows the exit closed");
            if let Some(token) = self.exit_close_observer.take() {
                // SAFETY: `+defaultCenter` is `@16@0:8`; `-removeObserver:` is `v24@0:8@16` with
                // the token `-addObserverForName:…` answered.
                unsafe {
                    let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
                    let remove: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
                    let center = get(class(c"NSNotificationCenter").as_id(), sel!(defaultCenter));
                    remove(center, sel!(removeObserver:), token.id());
                }
            } else {
                self.report
                    .check(false, "stage 16 armed its will-close observer");
                return;
            }
            let own = "WinitWindow";
            let closed = self.closed_at_exit.borrow().clone();
            let own_at_exit = self
                .windows_at_exit
                .iter()
                .filter(|class| *class == own)
                .count();
            let own_closed = closed.iter().filter(|class| *class == own).count();
            let foreign: Vec<&String> = self.windows_at_exit.iter().filter(|c| *c != own).collect();
            self.report.check(
                self.alert_answer == Some(NS_ALERT_FIRST_BUTTON_RETURN),
                &format!(
                    "a click on Quit answered the alert (runModal answered {:?})",
                    self.alert_answer
                ),
            );
            self.report.check(
                self.alert_panel_listed,
                &format!(
                    "the answered alert's own panel was still in -[NSApp windows] at exit, the \
                     state the exit must leave alone ({:?})",
                    self.windows_at_exit
                ),
            );
            let glass = "AppKit._NSGlassEffectWindow";
            if self.windows_at_exit.iter().any(|c| c == glass) {
                self.report.note(&format!(
                    "{glass}, the panel's child the old exit close freed, was listed too"
                ));
            } else {
                self.report.note(&format!(
                    "no {glass} was listed: this macOS gives alerts none"
                ));
            }
            self.report.check(
                own_at_exit == 2,
                &format!(
                    "the primary and hidden redraw-probe windows were both live at exit \
                     ({own_at_exit} own windows in {:?})",
                    self.windows_at_exit
                ),
            );
            self.report.check(
                own_closed == own_at_exit,
                &format!(
                    "the exit closed every winit window ({own_closed}/{own_at_exit}; closed \
                     {closed:?})"
                ),
            );
            self.report.check(
                closed.iter().all(|c| c == own),
                &format!(
                    "the exit closed no window AppKit owns (closed {closed:?}; AppKit's were \
                     {foreign:?})"
                ),
            );
        }
    }

    enum ChildOutcome {
        Exited(std::process::ExitStatus, String, String),
        Hung(String, String),
        CouldNotSpawn(String),
    }

    fn drain_text<R: std::io::Read + Send + 'static>(
        stream: Option<R>,
    ) -> std::thread::JoinHandle<String> {
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut stream) = stream {
                let _ = stream.read_to_string(&mut text);
            }
            text
        })
    }

    fn run_child(mode_env: &str) -> ChildOutcome {
        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(error) => {
                return ChildOutcome::CouldNotSpawn(format!("current_exe: {error}"));
            }
        };
        let mut child = match aterm_uds::spawnfd::command(exe)
            .env_remove(RENAME_REENTRY_PARENT_ENV)
            .env(mode_env, "1")
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                return ChildOutcome::CouldNotSpawn(format!("spawn: {error}"));
            }
        };
        let stdout = drain_text(child.stdout.take());
        let stderr = drain_text(child.stderr.take());
        let deadline = Instant::now() + CHILD_WATCHDOG;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    return ChildOutcome::Exited(
                        status,
                        stdout.join().unwrap_or_default(),
                        stderr.join().unwrap_or_default(),
                    );
                }
                Ok(None) => {}
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout.join();
                    let _ = stderr.join();
                    return ChildOutcome::CouldNotSpawn(format!("waiting for child: {error}"));
                }
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return ChildOutcome::Hung(
                    stdout.join().unwrap_or_default(),
                    stderr.join().unwrap_or_default(),
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn child_diagnostic(stdout: &str, stderr: &str) -> String {
        stdout
            .lines()
            .chain(stderr.lines())
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("no child diagnostic")
            .trim()
            .to_owned()
    }

    fn rename_reentry_diagnostic(stdout: &str, stderr: &str) -> String {
        let mut evidence = Vec::new();
        if stderr.contains(RENAME_REENTRY_PANIC_MARKER) {
            evidence.push(RENAME_REENTRY_PANIC_MARKER);
        }
        if stderr.contains(RENAME_REENTRY_ABORT_MARKER) {
            evidence.push(RENAME_REENTRY_ABORT_MARKER);
        }
        if evidence.is_empty() {
            child_diagnostic(stdout, stderr)
        } else {
            format!(
                "historical stderr evidence [{}]; tail: {}",
                evidence.join("; "),
                child_diagnostic(stdout, stderr)
            )
        }
    }

    fn judge_terminate_reentry(report: &mut Report) {
        println!("\n[15d] terminate: inside an NSAlert nested modal loop");
        match run_child(TERMINATE_REENTRY_CHILD_ENV) {
            ChildOutcome::Exited(status, stdout, stderr) if status.success() => {
                report.check(
                    true,
                    "the child deferred native termination until its active handler returned",
                );
                if !stderr.trim().is_empty() {
                    report.note(&format!(
                        "child stderr: {}",
                        child_diagnostic(&stdout, &stderr)
                    ));
                }
            }
            ChildOutcome::Exited(status, stdout, stderr) if status.code() == Some(NOT_RUN) => {
                report.blocked = Some(format!(
                    "the modal-termination child could not run: {}",
                    child_diagnostic(&stdout, &stderr)
                ));
            }
            ChildOutcome::Exited(status, stdout, stderr) => {
                use std::os::unix::process::ExitStatusExt as _;
                let how = status.signal().map_or_else(
                    || format!("exit {:?}", status.code()),
                    |signal| format!("signal {signal}"),
                );
                report.check(
                    false,
                    &format!(
                        "the modal-termination child ended by {how}: {}",
                        child_diagnostic(&stdout, &stderr)
                    ),
                );
            }
            ChildOutcome::Hung(stdout, stderr) => report.check(
                false,
                &format!(
                    "the modal-termination child did not exit before its watchdog: {}",
                    child_diagnostic(&stdout, &stderr)
                ),
            ),
            ChildOutcome::CouldNotSpawn(why) => {
                report.blocked = Some(format!("the modal-termination child could not run: {why}"));
            }
        }
    }

    fn judge_modal_exit_order(report: &mut Report) {
        println!("\n[15e] API exit: behind callbacks queued by an NSAlert modal loop");
        match run_child(MODAL_EXIT_ORDER_CHILD_ENV) {
            ChildOutcome::Exited(status, stdout, stderr) if status.success() => {
                report.check(
                    true,
                    "exit observed deferred focus/redraw callbacks of both kinds and both \
                     Destroyed events before LoopExiting",
                );
                if !stderr.trim().is_empty() {
                    report.note(&format!(
                        "child stderr: {}",
                        child_diagnostic(&stdout, &stderr)
                    ));
                }
            }
            ChildOutcome::Exited(status, stdout, stderr) if status.code() == Some(NOT_RUN) => {
                report.blocked = Some(format!(
                    "the modal-exit child could not run: {}",
                    child_diagnostic(&stdout, &stderr)
                ));
            }
            ChildOutcome::Exited(status, stdout, stderr) => {
                use std::os::unix::process::ExitStatusExt as _;
                let how = status.signal().map_or_else(
                    || format!("exit {:?}", status.code()),
                    |signal| format!("signal {signal}"),
                );
                report.check(
                    false,
                    &format!(
                        "the modal-exit child ended by {how}: {}",
                        child_diagnostic(&stdout, &stderr)
                    ),
                );
            }
            ChildOutcome::Hung(stdout, stderr) => report.check(
                false,
                &format!(
                    "the modal-exit child did not exit before its watchdog: {}",
                    child_diagnostic(&stdout, &stderr)
                ),
            ),
            ChildOutcome::CouldNotSpawn(why) => {
                report.blocked = Some(format!("the modal-exit child could not run: {why}"));
            }
        }
    }

    fn judge_modal_exit_run_app(report: &mut Report) {
        println!("\n[15f] API exit: production run_app behind an NSAlert modal loop");
        match run_child(MODAL_EXIT_RUN_APP_CHILD_ENV) {
            ChildOutcome::Exited(status, stdout, stderr) if status.success() => {
                report.check(
                    true,
                    "the production run_app path returned after deferred focus/redraw callbacks, \
                     both Destroyed events, and LoopExiting",
                );
                if !stderr.trim().is_empty() {
                    report.note(&format!(
                        "child stderr: {}",
                        child_diagnostic(&stdout, &stderr)
                    ));
                }
            }
            ChildOutcome::Exited(status, stdout, stderr) if status.code() == Some(NOT_RUN) => {
                report.blocked = Some(format!(
                    "the modal run_app child could not run: {}",
                    child_diagnostic(&stdout, &stderr)
                ));
            }
            ChildOutcome::Exited(status, stdout, stderr) => {
                use std::os::unix::process::ExitStatusExt as _;
                let how = status.signal().map_or_else(
                    || format!("exit {:?}", status.code()),
                    |signal| format!("signal {signal}"),
                );
                report.check(
                    false,
                    &format!(
                        "the modal run_app child ended by {how}: {}",
                        child_diagnostic(&stdout, &stderr)
                    ),
                );
            }
            ChildOutcome::Hung(stdout, stderr) => report.check(
                false,
                &format!(
                    "the modal run_app child did not exit before its watchdog: {}",
                    child_diagnostic(&stdout, &stderr)
                ),
            ),
            ChildOutcome::CouldNotSpawn(why) => {
                report.blocked = Some(format!("the modal run_app child could not run: {why}"));
            }
        }
    }

    fn judge_rename_reentry(report: &mut Report) {
        println!("\n[15g] real Rename This Mac field editor inside production run_app");
        match run_child(RENAME_REENTRY_CHILD_ENV) {
            ChildOutcome::Exited(status, stdout, stderr) if status.success() => {
                report.check(
                    true,
                    "Cancel emitted no rename request, and the real field-editor focus callback \
                     waited for user_event before Destroyed and LoopExiting",
                );
                if !stderr.trim().is_empty() {
                    report.note(&format!(
                        "child stderr: {}",
                        child_diagnostic(&stdout, &stderr)
                    ));
                }
            }
            ChildOutcome::Exited(status, stdout, stderr) if status.code() == Some(NOT_RUN) => {
                report.blocked = Some(format!(
                    "the Rename This Mac re-entry child could not run: {}",
                    child_diagnostic(&stdout, &stderr)
                ));
            }
            ChildOutcome::Exited(status, stdout, stderr) => {
                use std::os::unix::process::ExitStatusExt as _;
                let how = status.signal().map_or_else(
                    || format!("exit {:?}", status.code()),
                    |signal| {
                        if signal == 6 {
                            "signal 6 (SIGABRT)".to_owned()
                        } else {
                            format!("signal {signal}")
                        }
                    },
                );
                report.check(
                    false,
                    &format!(
                        "the Rename This Mac re-entry child ended by {how}: {}",
                        rename_reentry_diagnostic(&stdout, &stderr)
                    ),
                );
            }
            ChildOutcome::Hung(stdout, stderr) => report.check(
                false,
                &format!(
                    "the Rename This Mac re-entry child did not exit before its watchdog: {}",
                    rename_reentry_diagnostic(&stdout, &stderr)
                ),
            ),
            ChildOutcome::CouldNotSpawn(why) => {
                report.blocked = Some(format!(
                    "the Rename This Mac re-entry child could not run: {why}"
                ));
            }
        }
    }

    fn judge_prepared_exit(report: &mut Report) {
        println!("\n[15h] API exit: from a prepared loop's first Resumed callback");
        match run_child(PREPARED_EXIT_CHILD_ENV) {
            ChildOutcome::Exited(status, stdout, stderr) if status.success() => {
                report.check(
                    true,
                    "a prepared loop committed Resumed's exit in the same pump call",
                );
                if !stderr.trim().is_empty() {
                    report.note(&format!(
                        "child stderr: {}",
                        child_diagnostic(&stdout, &stderr)
                    ));
                }
            }
            ChildOutcome::Exited(status, stdout, stderr) if status.code() == Some(NOT_RUN) => {
                report.blocked = Some(format!(
                    "the prepared-exit child could not run: {}",
                    child_diagnostic(&stdout, &stderr)
                ));
            }
            ChildOutcome::Exited(status, stdout, stderr) => {
                use std::os::unix::process::ExitStatusExt as _;
                let how = status.signal().map_or_else(
                    || format!("exit {:?}", status.code()),
                    |signal| format!("signal {signal}"),
                );
                report.check(
                    false,
                    &format!(
                        "the prepared-exit child ended by {how}: {}",
                        child_diagnostic(&stdout, &stderr)
                    ),
                );
            }
            ChildOutcome::Hung(stdout, stderr) => report.check(
                false,
                &format!(
                    "the prepared-exit child did not exit before its watchdog: {}",
                    child_diagnostic(&stdout, &stderr)
                ),
            ),
            ChildOutcome::CouldNotSpawn(why) => {
                report.blocked = Some(format!("the prepared-exit child could not run: {why}"));
            }
        }
    }

    /// Each mode's exit ends the process inside AppKit with the owed redraw, the window's
    /// `Destroyed` and one `LoopExiting` delivered; a cancel during it sends no refusal.
    fn judge_deferred_quit(report: &mut Report, mode: DeferredQuit) {
        println!(
            "\n{}",
            match mode {
                DeferredQuit::Observed =>
                    "[15h] quit: an exit while AppKit waits on a deferred quit's reply",
                DeferredQuit::Forced =>
                    "[15i] quit: a second terminate: before a requested exit commits in AppKit's \
                     wait",
                DeferredQuit::Cancelled =>
                    "[15j] quit: a cancel in the work owed before an exit in AppKit's wait",
            }
        );
        match run_child(mode.child_env()) {
            ChildOutcome::Exited(status, _, stderr) if status.success() => {
                let exits: Vec<&str> = stderr
                    .lines()
                    .filter(|line| line.starts_with(DEFERRED_QUIT_EXIT_LINE))
                    .collect();
                let refused = stderr
                    .lines()
                    .any(|line| line == format!("{DEFERRED_QUIT_CANCEL_LINE} true"));
                let kept = stderr
                    .lines()
                    .any(|line| line == format!("{DEFERRED_QUIT_CANCEL_LINE} false"));
                let once_inside = exits.len() == 1
                    && exits[0].ends_with(
                        "owed redraw ran: true, window destroyed: true, inside AppKit: true",
                    )
                    && !refused
                    && (mode != DeferredQuit::Cancelled || kept);
                report.check(
                    once_inside,
                    &if once_inside && mode == DeferredQuit::Cancelled {
                        "the cancel in the owed redraw sent no refusal: the exit went on, the \
                         window's Destroyed and one LoopExiting followed inside AppKit, then \
                         AppKit ended the process on its YES"
                            .to_owned()
                    } else if once_inside && mode == DeferredQuit::Forced {
                        "a second terminate: met the requested exit: the redraw owed before \
                         it ran, the window's Destroyed and one LoopExiting followed inside \
                         AppKit"
                            .to_owned()
                    } else if once_inside {
                        "the exit committed inside AppKit's wait: the redraw owed before it ran, \
                         the window's Destroyed and one LoopExiting followed, then AppKit ended \
                         the process on its YES"
                            .to_owned()
                    } else {
                        format!(
                            "the deferred-quit child ended 0 without the owed redraw, the \
                             window's Destroyed and one LoopExiting inside AppKit, or its cancel \
                             sent a refusal: {exits:?}, refused: {refused}"
                        )
                    },
                );
            }
            ChildOutcome::Exited(status, stdout, stderr) if status.code() == Some(NOT_RUN) => {
                report.blocked = Some(format!(
                    "the deferred-quit child could not run: {}",
                    child_diagnostic(&stdout, &stderr)
                ));
            }
            ChildOutcome::Exited(status, stdout, stderr) => {
                use std::os::unix::process::ExitStatusExt as _;
                let how = status.signal().map_or_else(
                    || format!("exit {:?}", status.code()),
                    |signal| format!("signal {signal}"),
                );
                report.check(
                    false,
                    &format!(
                        "the deferred-quit child ended by {how}: {}",
                        child_diagnostic(&stdout, &stderr)
                    ),
                );
            }
            ChildOutcome::Hung(stdout, stderr) => report.check(
                false,
                &format!(
                    "the deferred-quit child did not exit before its watchdog (AppKit still \
                     waited on the reply): {}",
                    child_diagnostic(&stdout, &stderr)
                ),
            ),
            ChildOutcome::CouldNotSpawn(why) => {
                report.blocked = Some(format!("the deferred-quit child could not run: {why}"));
            }
        }
    }

    fn judge_deferred_handler_panic(report: &mut Report) {
        println!("\n[15i] panic: from a redraw deferred behind AboutToWait");
        match run_child(DEFERRED_PANIC_CHILD_ENV) {
            ChildOutcome::Exited(status, stdout, stderr) if status.success() => {
                report.check(
                    true,
                    "the deferred handler panic returned through pump_app_events",
                );
                if !stderr.trim().is_empty() {
                    report.note(&format!(
                        "child stderr: {}",
                        child_diagnostic(&stdout, &stderr)
                    ));
                }
            }
            ChildOutcome::Exited(status, stdout, stderr) if status.code() == Some(NOT_RUN) => {
                report.blocked = Some(format!(
                    "the deferred-panic child could not run: {}",
                    child_diagnostic(&stdout, &stderr)
                ));
            }
            ChildOutcome::Exited(status, stdout, stderr) => {
                use std::os::unix::process::ExitStatusExt as _;
                let how = status.signal().map_or_else(
                    || format!("exit {:?}", status.code()),
                    |signal| {
                        if signal == 6 {
                            "signal 6 (SIGABRT)".to_owned()
                        } else {
                            format!("signal {signal}")
                        }
                    },
                );
                report.check(
                    false,
                    &format!(
                        "the deferred-panic child ended by {how}: {}",
                        child_diagnostic(&stdout, &stderr)
                    ),
                );
            }
            ChildOutcome::Hung(stdout, stderr) => report.check(
                false,
                &format!(
                    "the deferred-panic child did not exit before its watchdog: {}",
                    child_diagnostic(&stdout, &stderr)
                ),
            ),
            ChildOutcome::CouldNotSpawn(why) => {
                report.blocked = Some(format!("the deferred-panic child could not run: {why}"));
            }
        }
    }

    /// A bounded entry for the old-tree negative control: run only the watched exact-crash child,
    /// without the full driver's unrelated AppKit stages.
    pub fn rename_reentry_parent() -> i32 {
        let mut report = Report::default();
        judge_rename_reentry(&mut report);
        if let Some(why) = report.blocked {
            eprintln!("objc-window-drive rename parent: NOT RUN — {why}");
            NOT_RUN
        } else if report.findings.is_empty() {
            eprintln!("objc-window-drive rename parent: OK — exact rename seam agreed");
            PASS
        } else {
            for finding in &report.findings {
                eprintln!("objc-window-drive rename parent: FINDING — {finding}");
            }
            FAIL
        }
    }

    /// The env-gated process AppKit itself terminates after the idle retry.
    pub fn terminate_reentry_child() -> i32 {
        if let Some(why) = aterm_gui::driver_display_unavailable() {
            eprintln!("objc-window-drive terminate child: NOT RUN — {why}");
            return NOT_RUN;
        }
        let mut el = match aterm_gui::quiet_driver_event_loop_builder::<()>().build() {
            Ok(el) => el,
            Err(error) => {
                eprintln!("objc-window-drive terminate child: NOT RUN — no event loop: {error}");
                return NOT_RUN;
            }
        };
        let mut driver = TerminateReentryChild {
            window: None,
            attempted: false,
            destroyed: false,
            failed: None,
        };
        let started = Instant::now();
        loop {
            let status = el.pump_app_events(Some(Duration::from_millis(8)), &mut driver);
            if let Some(why) = driver.failed.take() {
                eprintln!("objc-window-drive terminate child: NOT RUN — {why}");
                return NOT_RUN;
            }
            if let PumpStatus::Exit(_) = status {
                if driver.attempted {
                    return PASS;
                }
                eprintln!("objc-window-drive terminate child: NOT RUN — exited before the alert");
                return NOT_RUN;
            }
            if started.elapsed() > BUDGET {
                eprintln!(
                    "objc-window-drive terminate child: NOT RUN — AppKit did not terminate it"
                );
                return NOT_RUN;
            }
        }
    }

    /// The env-gated process proving API exit stays behind modal-deferred handler work.
    pub fn modal_exit_order_child() -> i32 {
        if let Some(why) = aterm_gui::driver_display_unavailable() {
            eprintln!("objc-window-drive modal-exit child: NOT RUN — {why}");
            return NOT_RUN;
        }
        let mut el = match aterm_gui::quiet_driver_event_loop_builder::<()>().build() {
            Ok(el) => el,
            Err(error) => {
                eprintln!("objc-window-drive modal-exit child: NOT RUN — no event loop: {error}");
                return NOT_RUN;
            }
        };
        el.prepare_launch();
        let mut driver = ModalExitOrderChild::default();
        let started = Instant::now();
        loop {
            let status = el.pump_app_events(Some(Duration::from_millis(8)), &mut driver);
            if let Some(why) = driver.unavailable.take() {
                eprintln!("objc-window-drive modal-exit child: NOT RUN — {why}");
                return NOT_RUN;
            }
            if driver.ordering_armed && matches!(&status, PumpStatus::Continue) {
                eprintln!(
                    "objc-window-drive modal-exit child: FAIL — pump_app_events returned Continue \
                     after exit() requested the ordered boundary"
                );
                return FAIL;
            }
            if let PumpStatus::Exit(0) = status {
                if let Some(why) = driver.ordering_failure.take() {
                    eprintln!("objc-window-drive modal-exit child: FAIL — {why}");
                    return FAIL;
                }
                if driver.passed {
                    eprintln!(
                        "objc-window-drive modal-exit child: OK — deferred focus/redraw callbacks \
                         of both kinds, both Destroyed events, then LoopExiting"
                    );
                    return PASS;
                }
                eprintln!(
                    "objc-window-drive modal-exit child: FAIL — exited without proving the \
                     callback/Destroyed/LoopExiting order"
                );
                return FAIL;
            }
            if started.elapsed() > DEFERRED_HANDLER_BUDGET {
                eprintln!(
                    "objc-window-drive modal-exit child: FAIL — pump_app_events did not finish \
                     the ordered exit"
                );
                return FAIL;
            }
        }
    }

    /// The same ordered-exit probe through aterm's production `EventLoop::run_app` route.
    pub fn modal_exit_run_app_child() -> i32 {
        if let Some(why) = aterm_gui::driver_display_unavailable() {
            eprintln!("objc-window-drive modal run_app child: NOT RUN — {why}");
            return NOT_RUN;
        }
        let mut el = match aterm_gui::quiet_driver_event_loop_builder::<()>().build() {
            Ok(el) => el,
            Err(error) => {
                eprintln!(
                    "objc-window-drive modal run_app child: NOT RUN — no event loop: {error}"
                );
                return NOT_RUN;
            }
        };
        el.prepare_launch();
        let mut driver = ModalExitOrderChild::default();

        // `run_app` deliberately has no in-process deadline. The parent drive runs this mode in
        // its own process and kills it after `CHILD_WATCHDOG`, so a native-loop regression stays
        // bounded without asking a background thread to control AppKit.
        let result = el.run_app(&mut driver);
        if let Some(why) = driver.unavailable.take() {
            eprintln!("objc-window-drive modal run_app child: NOT RUN — {why}");
            return NOT_RUN;
        }
        if let Err(error) = result {
            eprintln!("objc-window-drive modal run_app child: FAIL — run_app: {error}");
            return FAIL;
        }
        if let Some(why) = driver.ordering_failure.take() {
            eprintln!("objc-window-drive modal run_app child: FAIL — {why}");
            return FAIL;
        }
        if driver.attempted && driver.passed {
            eprintln!(
                "objc-window-drive modal run_app child: OK — run_app returned after deferred \
                 focus/redraw callbacks of both kinds, both Destroyed events, then LoopExiting"
            );
            PASS
        } else {
            eprintln!(
                "objc-window-drive modal run_app child: FAIL — run_app returned without proving \
                 the callback/Destroyed/LoopExiting order"
            );
            FAIL
        }
    }

    /// The env-gated process whose prepared loop requests exit from its first real `Resumed`.
    pub fn prepared_exit_child() -> i32 {
        if let Some(why) = aterm_gui::driver_display_unavailable() {
            eprintln!("objc-window-drive prepared-exit child: NOT RUN — {why}");
            return NOT_RUN;
        }
        let mut el = match aterm_gui::quiet_driver_event_loop_builder::<()>().build() {
            Ok(el) => el,
            Err(error) => {
                eprintln!(
                    "objc-window-drive prepared-exit child: NOT RUN — no event loop: {error}"
                );
                return NOT_RUN;
            }
        };
        el.prepare_launch();

        let mut driver = PreparedExitChild::default();
        let status = el.pump_app_events(Some(Duration::from_millis(8)), &mut driver);
        let (same_call_exit, status) = match status {
            PumpStatus::Exit(code) => (code == 0, format!("Exit({code})")),
            PumpStatus::Continue => (false, "Continue".to_owned()),
        };
        if same_call_exit
            && driver.resumed == 1
            && driver.request_was_immediate
            && driver.exiting == 1
        {
            eprintln!(
                "objc-window-drive prepared-exit child: OK — immediate request, one Resumed, \
                 one LoopExiting, same-call Exit"
            );
            PASS
        } else {
            eprintln!(
                "objc-window-drive prepared-exit child: FAIL — status {status}, Resumed {}, \
                 immediate {}, LoopExiting {}",
                driver.resumed, driver.request_was_immediate, driver.exiting
            );
            FAIL
        }
    }

    /// The env-gated process that takes AppKit's quit up and exits during AppKit's wait on the
    /// reply, behind a redraw owed first, as `mode` says. AppKit ends it (`exit(0)`), so a pump
    /// that returns, or a budget spent, is a failure; one that never returns is the parent's
    /// watchdog's.
    pub fn deferred_quit_child(mode: DeferredQuit) -> i32 {
        use std::sync::atomic::Ordering;
        if let Some(why) = aterm_gui::driver_display_unavailable() {
            eprintln!("objc-window-drive deferred-quit child: NOT RUN — {why}");
            return NOT_RUN;
        }
        aterm_window::platform::macos::set_application_should_terminate_handler(take_the_quit_up);
        let mut el = match aterm_gui::quiet_driver_event_loop_builder::<()>().build() {
            Ok(el) => el,
            Err(error) => {
                eprintln!(
                    "objc-window-drive deferred-quit child: NOT RUN — no event loop: {error}"
                );
                return NOT_RUN;
            }
        };
        let mut driver = DeferredQuitChild {
            mode,
            ..DeferredQuitChild::default()
        };
        let started = Instant::now();
        loop {
            let status = el.pump_app_events(Some(Duration::from_millis(8)), &mut driver);
            if let Some(why) = driver.failed.take() {
                eprintln!("objc-window-drive deferred-quit child: FAIL — {why}");
                return FAIL;
            }
            if DEFERRED_QUIT_UNANSWERABLE.load(Ordering::SeqCst) {
                eprintln!(
                    "objc-window-drive deferred-quit child: FAIL — AppKit asked from where no \
                     reply could come"
                );
                return FAIL;
            }
            if let PumpStatus::Exit(code) = status {
                eprintln!(
                    "objc-window-drive deferred-quit child: FAIL — pump_app_events returned \
                     Exit({code}) after a quit taken up; AppKit ends the process on its YES"
                );
                return FAIL;
            }
            if started.elapsed() > DEFERRED_HANDLER_BUDGET {
                eprintln!(
                    "objc-window-drive deferred-quit child: FAIL — {}",
                    if DEFERRED_QUIT_TAKEN.load(Ordering::SeqCst) {
                        "the quit was taken up but the process did not end"
                    } else {
                        "AppKit never asked about the quit"
                    }
                );
                return FAIL;
            }
        }
    }

    /// The env-gated process whose deferred event handler panics with a known payload.
    pub fn deferred_panic_child() -> i32 {
        if let Some(why) = aterm_gui::driver_display_unavailable() {
            eprintln!("objc-window-drive deferred-panic child: NOT RUN — {why}");
            return NOT_RUN;
        }
        let mut el = match aterm_gui::quiet_driver_event_loop_builder::<()>().build() {
            Ok(el) => el,
            Err(error) => {
                eprintln!(
                    "objc-window-drive deferred-panic child: NOT RUN — no event loop: {error}"
                );
                return NOT_RUN;
            }
        };
        let mut driver = DeferredPanicChild {
            window: None,
            attempted: false,
            panic_on_redraw: false,
            failed: None,
        };
        let started = Instant::now();
        loop {
            let pumped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                el.pump_app_events(Some(Duration::from_millis(8)), &mut driver)
            }));
            match pumped {
                Err(payload) => {
                    let marker = payload.downcast_ref::<&'static str>().copied();
                    if driver.attempted && marker == Some(DEFERRED_HANDLER_PANIC_MARKER) {
                        eprintln!(
                            "objc-window-drive deferred-panic child: OK — pump_app_events \
                             returned the exact deferred handler panic marker"
                        );
                        return PASS;
                    }
                    eprintln!(
                        "objc-window-drive deferred-panic child: FAIL — pump_app_events returned \
                         a panic, but not the exact deferred handler panic marker"
                    );
                    return FAIL;
                }
                Ok(PumpStatus::Exit(_)) => {
                    if let Some(why) = driver.failed.take() {
                        eprintln!("objc-window-drive deferred-panic child: NOT RUN — {why}");
                        return NOT_RUN;
                    }
                    eprintln!(
                        "objc-window-drive deferred-panic child: FAIL — the event loop exited \
                         before the deferred handler panic returned"
                    );
                    return FAIL;
                }
                Ok(PumpStatus::Continue) => {}
            }
            if let Some(why) = driver.failed.take() {
                eprintln!("objc-window-drive deferred-panic child: NOT RUN — {why}");
                return NOT_RUN;
            }
            if started.elapsed() > DEFERRED_HANDLER_BUDGET {
                eprintln!(
                    "objc-window-drive deferred-panic child: FAIL — pump_app_events did not \
                     return the deferred handler panic"
                );
                return FAIL;
            }
        }
    }

    /// Drive the window loop through every verification stage and return an exit code.
    pub fn run() -> i32 {
        if let Some(why) = aterm_gui::driver_display_unavailable() {
            eprintln!("objc-window-drive: NOT RUN — {why}");
            return NOT_RUN;
        }
        let mut el = match aterm_gui::quiet_driver_event_loop_builder::<u8>().build() {
            Ok(el) => el,
            Err(e) => {
                eprintln!("objc-window-drive: NOT RUN — no event loop: {e}");
                return NOT_RUN;
            }
        };
        let mut driver = Driver {
            window: None,
            redraw_probe: None,
            proxy: Some(el.create_proxy()),
            proxy_events: Vec::new(),
            report: Report::default(),
            resized: 0,
            moved: 0,
            focused: Vec::new(),
            close_requested: 0,
            hovered: Vec::new(),
            key_ups: 0,
            posted_cmd_key_up: false,
            alert_answer: None,
            modal_focus_before: None,
            modal_probe_redraw_before: None,
            probe_redraws: 0,
            windows_at_exit: Vec::new(),
            alert_panel_listed: false,
            closed_at_exit: std::rc::Rc::default(),
            exit_close_observer: None,
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

        if driver.report.blocked.is_none() {
            driver.judge_exit_closes();
            judge_terminate_reentry(&mut driver.report);
            if driver.report.blocked.is_none() {
                judge_modal_exit_order(&mut driver.report);
            }
            if driver.report.blocked.is_none() {
                judge_modal_exit_run_app(&mut driver.report);
            }
            if driver.report.blocked.is_none() {
                judge_rename_reentry(&mut driver.report);
            }
            if driver.report.blocked.is_none() {
                judge_prepared_exit(&mut driver.report);
            }
            if driver.report.blocked.is_none() {
                judge_deferred_handler_panic(&mut driver.report);
            }
            if driver.report.blocked.is_none() {
                judge_deferred_quit(&mut driver.report, DeferredQuit::Observed);
            }
            if driver.report.blocked.is_none() {
                judge_deferred_quit(&mut driver.report, DeferredQuit::Forced);
            }
            if driver.report.blocked.is_none() {
                judge_deferred_quit(&mut driver.report, DeferredQuit::Cancelled);
            }
        }

        drop(driver.window.take());
        drop(driver.redraw_probe.take());

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
