// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/windows/window.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by Andrew Yates for the aterm project in 2026; see the repository NOTICE.
// Adapt pointer handles and preserve thread-executor dispatch with SendHandle; drop the
// rwh_04, rwh_05 and rwh_06 handle methods; build the FlashWindowEx request apart
// (`attention_flash`).

#![cfg(windows_platform)]

use std::cell::Cell;
use std::ffi::c_void;
use std::mem::{self};
use std::sync::{Arc, Mutex, MutexGuard};
use std::{io, panic, ptr};

use windows_sys::Win32::Foundation::{
    HWND, LPARAM, OLE_E_WRONGCOMPOBJ, POINT, POINTS, RECT, RPC_E_CHANGED_MODE, S_OK, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    ClientToScreen, InvalidateRgn, RedrawWindow, RDW_INTERNALPAINT,
};
use windows_sys::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
};
use windows_sys::Win32::System::Ole::{OleInitialize, RegisterDragDrop};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetActiveWindow, MapVirtualKeyW, ReleaseCapture, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD,
    KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC, VK_LMENU, VK_MENU,
};
use windows_sys::Win32::UI::Input::Touch::{RegisterTouchWindow, TWF_WANTPALM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, FlashWindowEx, GetClientRect, GetCursorPos, GetForegroundWindow,
    GetSystemMetrics, GetWindowPlacement, LoadCursorW, PeekMessageW, PostMessageW,
    RegisterClassExW, SetCursor, SetForegroundWindow, SetWindowPlacement, SetWindowPos,
    SetWindowTextW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, FLASHWINFO, FLASHW_ALL, FLASHW_STOP,
    FLASHW_TIMERNOFG, FLASHW_TRAY, HTCAPTION, NID_READY, PM_NOREMOVE, SM_DIGITIZER,
    SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, WM_NCLBUTTONDOWN, WNDCLASSEXW,
};

use crate::cursor::Cursor;
use crate::dpi::{PhysicalPosition, PhysicalSize, Position, Size};
use crate::error::{ExternalError, NotSupportedError, OsError as RootOsError};
use crate::icon::Icon;
use crate::platform_impl::platform::dark_mode::try_theme;
use crate::platform_impl::platform::definitions::{
    CLSID_TaskbarList, IID_ITaskbarList, IID_ITaskbarList2, ITaskbarList, ITaskbarList2,
};
use crate::platform_impl::platform::dpi::{
    dpi_to_scale_factor, enable_non_client_dpi_scaling, hwnd_dpi,
};
use crate::platform_impl::platform::drop_handler::FileDropHandler;
use crate::platform_impl::platform::event_loop::{self, ActiveEventLoop, DESTROY_MSG_ID};
use crate::platform_impl::platform::icon::{self, IconType};
use crate::platform_impl::platform::ime::ImeContext;
use crate::platform_impl::platform::keyboard::KeyEventBuilder;
use crate::platform_impl::platform::monitor::{self, MonitorHandle};
use crate::platform_impl::platform::window_state::{SavedWindow, WindowFlags, WindowState};
use crate::platform_impl::platform::{util, Fullscreen, WindowId};
use crate::window::{ImePurpose, Theme, UserAttentionType, WindowAttributes};

// SAFETY: an `HWND` names a process-global window, and calls that need its owner thread go
// through `thread_executor`.
unsafe impl Send for Window {}
unsafe impl Sync for Window {}

/// The Win32 implementation of the main `Window` object.
pub(crate) struct Window {
    /// Main handle for the window.
    window: HWND,

    /// The current window state.
    window_state: Arc<Mutex<WindowState>>,

    thread_executor: event_loop::EventLoopThreadExecutor,
}

impl Window {
    pub(crate) fn new(
        event_loop: &ActiveEventLoop,
        w_attr: WindowAttributes,
    ) -> Result<Window, RootOsError> {
        unsafe { init(w_attr, event_loop) }
    }

    pub(crate) fn maybe_queue_on_main(&self, f: impl FnOnce(&Self) + Send + 'static) {
        f(self)
    }

    pub(crate) fn maybe_wait_on_main<R: Send>(&self, f: impl FnOnce(&Self) -> R + Send) -> R {
        f(self)
    }

    fn window_state_lock(&self) -> MutexGuard<'_, WindowState> {
        self.window_state.lock().unwrap()
    }

    pub fn set_title(&self, text: &str) {
        let wide_text = util::encode_wide(text);
        unsafe {
            SetWindowTextW(self.hwnd(), wide_text.as_ptr());
        }
    }

    #[inline]
    pub fn set_visible(&self, visible: bool) {
        let window = super::handle::SendHandle::new(self.window);
        let window_state = Arc::clone(&self.window_state);
        self.thread_executor.execute_in_thread(move || {
            let window = window.get();
            WindowState::set_window_flags(window_state.lock().unwrap(), window, |f| {
                f.set(WindowFlags::VISIBLE, visible)
            });
        });
    }

    #[inline]
    pub fn request_redraw(&self) {
        self.window_state.lock().unwrap().redraw_requested = true;
        unsafe {
            RedrawWindow(self.hwnd(), ptr::null(), std::ptr::null_mut(), RDW_INTERNALPAINT);
        }
    }

    #[inline]
    pub fn outer_position(&self) -> Result<PhysicalPosition<i32>, NotSupportedError> {
        util::WindowArea::Outer
            .get_rect(self.hwnd())
            .map(|rect| Ok(PhysicalPosition::new(rect.left, rect.top)))
            .expect(
                "Unexpected GetWindowRect failure; please report this error to \
                 rust-windowing/winit",
            )
    }

    #[inline]
    pub fn inner_position(&self) -> Result<PhysicalPosition<i32>, NotSupportedError> {
        let mut position: POINT = unsafe { mem::zeroed() };
        if unsafe { ClientToScreen(self.hwnd(), &mut position) } == false.into() {
            panic!(
                "Unexpected ClientToScreen failure: please report this error to \
                 rust-windowing/winit"
            )
        }
        Ok(PhysicalPosition::new(position.x, position.y))
    }

    #[inline]
    pub fn set_outer_position(&self, position: Position) {
        let (x, y): (i32, i32) = position.to_physical::<i32>(self.scale_factor()).into();

        let window_state = Arc::clone(&self.window_state);
        let window = super::handle::SendHandle::new(self.window);
        self.thread_executor.execute_in_thread(move || {
            let window = window.get();
            WindowState::set_window_flags(window_state.lock().unwrap(), window, |f| {
                f.set(WindowFlags::MAXIMIZED, false)
            });
        });

        unsafe {
            SetWindowPos(
                self.hwnd(),
                std::ptr::null_mut(),
                x,
                y,
                0,
                0,
                SWP_ASYNCWINDOWPOS | SWP_NOZORDER | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            InvalidateRgn(self.hwnd(), std::ptr::null_mut(), false.into());
        }
    }

    #[inline]
    pub fn inner_size(&self) -> PhysicalSize<u32> {
        let mut rect: RECT = unsafe { mem::zeroed() };
        if unsafe { GetClientRect(self.hwnd(), &mut rect) } == false.into() {
            panic!(
                "Unexpected GetClientRect failure: please report this error to \
                 rust-windowing/winit"
            )
        }
        PhysicalSize::new((rect.right - rect.left) as u32, (rect.bottom - rect.top) as u32)
    }

    #[inline]
    pub fn outer_size(&self) -> PhysicalSize<u32> {
        util::WindowArea::Outer
            .get_rect(self.hwnd())
            .map(|rect| {
                PhysicalSize::new((rect.right - rect.left) as u32, (rect.bottom - rect.top) as u32)
            })
            .unwrap()
    }

    #[inline]
    pub fn request_inner_size(&self, size: Size) -> Option<PhysicalSize<u32>> {
        let scale_factor = self.scale_factor();
        let physical_size = size.to_physical::<u32>(scale_factor);

        let window_flags = self.window_state_lock().window_flags;
        window_flags.set_size(self.hwnd(), physical_size);

        if physical_size != self.inner_size() {
            let window_state = Arc::clone(&self.window_state);
            let window = super::handle::SendHandle::new(self.window);
            self.thread_executor.execute_in_thread(move || {
                let window = window.get();
                WindowState::set_window_flags(window_state.lock().unwrap(), window, |f| {
                    f.set(WindowFlags::MAXIMIZED, false)
                });
            });
        }

        None
    }

    #[inline]
    pub fn set_resize_increments(&self, increments: Option<Size>) {
        self.window_state_lock().resize_increments = increments;
    }

    /// Returns the `hwnd` of this window.
    #[inline]
    pub fn hwnd(&self) -> HWND {
        self.window
    }

    /// LOCAL PATCH (aterm) (docs/AUDIT-third-party-2026-10-15.md, winit-core step 6, the Windows
    /// half): the window's `HWND` and its class's `HINSTANCE`, which aterm's crates read through
    /// first-party `aterm_types::native_window::HasNativeWindow`. `None` off the event loop's
    /// thread, as upstream's raw-window-handle answered.
    pub fn native_window(&self) -> Option<aterm_types::native_window::NativeWindow> {
        if !self.thread_executor.in_event_loop_thread() {
            tracing::error!("tried to access window handle outside of the main thread");
            return None;
        }
        use windows_sys::Win32::UI::WindowsAndMessaging::GWLP_HINSTANCE;
        // SAFETY: `self.window` is this window's live `HWND`, read on its own thread.
        let hinstance = unsafe { super::get_window_long(self.hwnd(), GWLP_HINSTANCE) };
        Some(aterm_types::native_window::NativeWindow::Win32 {
            hwnd: std::ptr::NonNull::new(self.window)?,
            hinstance: std::ptr::NonNull::new(super::handle::from_isize(hinstance)),
        })
    }

    #[inline]
    pub fn set_cursor(&self, cursor: Cursor) {
        let Cursor::Icon(icon) = cursor;
        self.window_state_lock().mouse.selected_cursor = icon;
        self.thread_executor.execute_in_thread(move || unsafe {
            let cursor = LoadCursorW(std::ptr::null_mut(), util::to_windows_cursor(icon));
            SetCursor(cursor);
        });
    }

    #[inline]
    pub fn scale_factor(&self) -> f64 {
        self.window_state_lock().scale_factor
    }

    unsafe fn handle_os_dragging(&self, wparam: WPARAM) {
        let window = super::handle::SendHandle::new(self.window);
        let window_state = self.window_state.clone();

        self.thread_executor.execute_in_thread(move || {
            let window = window.get();
            {
                let mut guard = window_state.lock().unwrap();
                if !guard.dragging {
                    guard.dragging = true;
                } else {
                    return;
                }
            }

            let points = {
                let mut pos = unsafe { mem::zeroed() };
                unsafe { GetCursorPos(&mut pos) };
                pos
            };
            let points = POINTS { x: points.x as i16, y: points.y as i16 };

            unsafe { ReleaseCapture() };

            unsafe {
                PostMessageW(window, WM_NCLBUTTONDOWN, wparam, &points as *const _ as LPARAM)
            };
        });
    }

    #[inline]
    pub fn drag_window(&self) -> Result<(), ExternalError> {
        unsafe {
            self.handle_os_dragging(HTCAPTION as WPARAM);
        }

        Ok(())
    }

    #[inline]
    pub fn id(&self) -> WindowId {
        WindowId(self.hwnd())
    }

    #[inline]
    pub fn set_minimized(&self, minimized: bool) {
        let window = super::handle::SendHandle::new(self.window);
        let window_state = Arc::clone(&self.window_state);

        let is_minimized = util::is_minimized(self.hwnd());

        self.thread_executor.execute_in_thread(move || {
            let window = window.get();
            WindowState::set_window_flags_in_place(&mut window_state.lock().unwrap(), |f| {
                f.set(WindowFlags::MINIMIZED, is_minimized)
            });
            WindowState::set_window_flags(window_state.lock().unwrap(), window, |f| {
                f.set(WindowFlags::MINIMIZED, minimized)
            });
        });
    }

    #[inline]
    pub fn is_minimized(&self) -> Option<bool> {
        Some(util::is_minimized(self.hwnd()))
    }

    #[inline]
    pub fn set_maximized(&self, maximized: bool) {
        let window = super::handle::SendHandle::new(self.window);
        let window_state = Arc::clone(&self.window_state);

        self.thread_executor.execute_in_thread(move || {
            let window = window.get();
            WindowState::set_window_flags(window_state.lock().unwrap(), window, |f| {
                f.set(WindowFlags::MAXIMIZED, maximized)
            });
        });
    }

    #[inline]
    pub fn is_maximized(&self) -> bool {
        let window_state = self.window_state_lock();
        window_state.window_flags.contains(WindowFlags::MAXIMIZED)
    }

    #[inline]
    pub fn fullscreen(&self) -> Option<Fullscreen> {
        let window_state = self.window_state_lock();
        window_state.fullscreen.clone()
    }

    #[inline]
    pub fn set_fullscreen(&self, fullscreen: Option<Fullscreen>) {
        let window = super::handle::SendHandle::new(self.window);
        let window_state = Arc::clone(&self.window_state);

        let mut window_state_lock = window_state.lock().unwrap();
        let old_fullscreen = window_state_lock.fullscreen.clone();

        match (&old_fullscreen, &fullscreen) {
            _ if old_fullscreen == fullscreen => return,
            (Some(Fullscreen::Borderless(Some(monitor))), Some(Fullscreen::Borderless(None)))
                if *monitor == monitor::current_monitor(window.get()) =>
            {
                return
            },
            _ => {},
        }

        window_state_lock.fullscreen.clone_from(&fullscreen);
        drop(window_state_lock);

        // LOCAL PATCH (aterm): borderless only; exclusive fullscreen (a video mode set with
        // `ChangeDisplaySettingsExW`) had no aterm caller.
        self.thread_executor.execute_in_thread(move || {
            let window = window.get();

            unsafe {
                let mut msg = mem::zeroed();
                PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
            }

            WindowState::set_window_flags(window_state.lock().unwrap(), window, |f| {
                f.set(WindowFlags::MARKER_BORDERLESS_FULLSCREEN, fullscreen.is_some());
            });

            unsafe {
                taskbar_mark_fullscreen(window, fullscreen.is_some());
            }

            match &fullscreen {
                Some(fullscreen) => {
                    let placement = unsafe {
                        let mut placement = mem::zeroed();
                        GetWindowPlacement(window, &mut placement);
                        placement
                    };

                    window_state.lock().unwrap().saved_window = Some(SavedWindow { placement });

                    let monitor = match &fullscreen {
                        Fullscreen::Borderless(Some(monitor)) => monitor.clone(),
                        Fullscreen::Borderless(None) => monitor::current_monitor(window),
                    };

                    let position: (i32, i32) = monitor.position().into();
                    let size: (u32, u32) = monitor.size().into();

                    unsafe {
                        SetWindowPos(
                            window,
                            std::ptr::null_mut(),
                            position.0,
                            position.1,
                            size.0 as i32,
                            size.1 as i32,
                            SWP_ASYNCWINDOWPOS | SWP_NOZORDER,
                        );
                        InvalidateRgn(window, std::ptr::null_mut(), false.into());
                    }
                },
                None => {
                    let mut window_state_lock = window_state.lock().unwrap();
                    if let Some(SavedWindow { placement }) = window_state_lock.saved_window.take() {
                        drop(window_state_lock);
                        unsafe {
                            SetWindowPlacement(window, &placement);
                            InvalidateRgn(window, std::ptr::null_mut(), false.into());
                        }
                    }
                },
            }
        });
    }

    /// LOCAL PATCH (aterm): every window is decorated (no aterm crate turns decorations off).
    #[inline]
    pub fn is_decorated(&self) -> bool {
        true
    }

    #[inline]
    pub fn current_monitor(&self) -> Option<MonitorHandle> {
        Some(monitor::current_monitor(self.hwnd()))
    }

    #[inline]
    pub fn set_window_icon(&self, window_icon: Option<Icon>) {
        if let Some(ref window_icon) = window_icon {
            window_icon.inner.set_for_window(self.hwnd(), IconType::Small);
        } else {
            icon::unset_for_window(self.hwnd(), IconType::Small);
        }
        self.window_state_lock().window_icon = window_icon;
    }

    #[inline]
    pub fn set_taskbar_icon(&self, taskbar_icon: Option<Icon>) {
        if let Some(ref taskbar_icon) = taskbar_icon {
            taskbar_icon.inner.set_for_window(self.hwnd(), IconType::Big);
        } else {
            icon::unset_for_window(self.hwnd(), IconType::Big);
        }
        self.window_state_lock().taskbar_icon = taskbar_icon;
    }

    #[inline]
    pub fn set_ime_cursor_area(&self, spot: Position, size: Size) {
        let window = super::handle::SendHandle::new(self.window);
        let state = self.window_state.clone();
        self.thread_executor.execute_in_thread(move || unsafe {
            let window = window.get();
            let scale_factor = state.lock().unwrap().scale_factor;
            ImeContext::current(window).set_ime_cursor_area(spot, size, scale_factor);
        });
    }

    #[inline]
    pub fn set_ime_allowed(&self, allowed: bool) {
        let window = super::handle::SendHandle::new(self.window);
        let state = self.window_state.clone();
        self.thread_executor.execute_in_thread(move || unsafe {
            let window = window.get();
            state.lock().unwrap().ime_allowed = allowed;
            ImeContext::set_ime_allowed(window, allowed);
        })
    }

    #[inline]
    pub fn set_ime_purpose(&self, _purpose: ImePurpose) {}

    #[inline]
    pub fn request_user_attention(&self, request_type: Option<UserAttentionType>) {
        let window = super::handle::SendHandle::new(self.window);
        let active_window_handle = unsafe { GetActiveWindow() };
        if window.get() == active_window_handle {
            return;
        }

        self.thread_executor.execute_in_thread(move || unsafe {
            let flash_info = attention_flash(window.get(), request_type);
            FlashWindowEx(&flash_info);
        });
    }

    #[inline]
    pub fn set_theme(&self, theme: Option<Theme>) {
        try_theme(self.window, theme);
    }

    #[inline]
    pub fn theme(&self) -> Option<Theme> {
        Some(self.window_state_lock().current_theme)
    }

    #[inline]
    pub fn has_focus(&self) -> bool {
        let window_state = self.window_state.lock().unwrap();
        window_state.has_active_focus()
    }

    #[inline]
    pub fn focus_window(&self) {
        let window_flags = self.window_state_lock().window_flags();

        let is_visible = window_flags.contains(WindowFlags::VISIBLE);
        let is_minimized = util::is_minimized(self.hwnd());
        let is_foreground = self.window == unsafe { GetForegroundWindow() };

        if is_visible && !is_minimized && !is_foreground {
            unsafe { force_window_active(self.window) };
        }
    }
}

impl Drop for Window {
    #[inline]
    fn drop(&mut self) {
        unsafe {
            PostMessageW(self.hwnd(), DESTROY_MSG_ID.get(), 0, 0);
        }
    }
}

pub(super) struct InitData<'a> {
    pub event_loop: &'a ActiveEventLoop,
    pub attributes: WindowAttributes,
    pub window_flags: WindowFlags,
    pub window: Option<Window>,
}

impl InitData<'_> {
    unsafe fn create_window(&self, window: HWND) -> Window {
        {
            let digitizer = unsafe { GetSystemMetrics(SM_DIGITIZER) as u32 };
            if digitizer & NID_READY != 0 {
                unsafe { RegisterTouchWindow(window, TWF_WANTPALM) };
            }
        }

        let dpi = unsafe { hwnd_dpi(window) };
        let scale_factor = dpi_to_scale_factor(dpi);

        let current_theme = try_theme(window, None);

        let window_state = {
            let window_state = WindowState::new(&self.attributes, scale_factor, current_theme);
            let window_state = Arc::new(Mutex::new(window_state));
            WindowState::set_window_flags(window_state.lock().unwrap(), window, |f| {
                *f = self.window_flags
            });
            window_state
        };

        enable_non_client_dpi_scaling(window);

        unsafe { ImeContext::set_ime_allowed(window, false) };

        Window { window, window_state, thread_executor: self.event_loop.create_thread_executor() }
    }

    unsafe fn create_window_data(&self, win: &Window) -> event_loop::WindowData {
        // LOCAL PATCH (aterm): every window is an OLE drop target (upstream's default; its switch,
        // `with_drag_and_drop`, had no aterm caller).
        let file_drop_handler = {
            let ole_init_result = unsafe { OleInitialize(ptr::null_mut()) };
            if ole_init_result == OLE_E_WRONGCOMPOBJ {
                panic!("OleInitialize failed! Result was: `OLE_E_WRONGCOMPOBJ`");
            } else if ole_init_result == RPC_E_CHANGED_MODE {
                panic!(
                    "OleInitialize failed! Result was: `RPC_E_CHANGED_MODE`. Make sure other \
                     crates are not using multithreaded COM library on the same thread or disable \
                     drag and drop support."
                );
            }

            let file_drop_runner = self.event_loop.runner_shared.clone();
            let file_drop_handler = FileDropHandler::new(
                win.window,
                Box::new(move |event| {
                    if let Ok(e) = event.map_nonuser_event() {
                        file_drop_runner.send_event(e)
                    }
                }),
            );

            let handler_interface_ptr =
                unsafe { &mut (*file_drop_handler.data).interface as *mut _ as *mut c_void };

            assert_eq!(unsafe { RegisterDragDrop(win.window, handler_interface_ptr) }, S_OK);
            Some(file_drop_handler)
        };

        event_loop::WindowData {
            window_state: win.window_state.clone(),
            event_loop_runner: self.event_loop.runner_shared.clone(),
            key_event_builder: KeyEventBuilder::default(),
            _file_drop_handler: file_drop_handler,
            userdata_removed: Cell::new(false),
            recurse_depth: Cell::new(0),
            minimized: Cell::new(false),
        }
    }

    pub unsafe fn on_nccreate(&mut self, window: HWND) -> Option<isize> {
        let runner = self.event_loop.runner_shared.clone();
        let result = runner.catch_unwind(|| {
            let window = unsafe { self.create_window(window) };
            let window_data = unsafe { self.create_window_data(&window) };
            (window, window_data)
        });

        result.map(|(win, userdata)| {
            self.window = Some(win);
            let userdata = Box::into_raw(Box::new(userdata));
            userdata as _
        })
    }

    /// LOCAL PATCH (aterm): upstream's creation-time calls for the attributes no aterm crate sets
    /// (transparency, content protection, the enabled buttons, a maximum size, the system backdrop,
    /// the DWM colours and corners) are folded in: the window is opaque and capturable, its three
    /// buttons are in `init`'s flags, its size is clamped by the minimum alone, and DWM's own
    /// backdrop (`DWMSBT_AUTO`, the default upstream set again) is left as it is. The taskbar tab
    /// upstream's `set_skip_taskbar(false)` added is still added.
    pub unsafe fn on_create(&mut self) {
        let win = self.window.as_mut().expect("failed window creation");

        unsafe { add_taskbar_tab(win.hwnd()) };
        win.set_window_icon(self.attributes.window_icon.clone());
        win.set_taskbar_icon(self.attributes.platform_specific.taskbar_icon.clone());

        let attributes = self.attributes.clone();

        win.set_cursor(Cursor::default());

        win.set_visible(attributes.visible);

        // LOCAL PATCH (aterm): no aterm crate sets a minimum size or a position on Windows, so the
        // size is clamped to upstream's defaults (no minimum, no maximum) and the window stays
        // where Windows put it (`CW_USEDEFAULT`).
        let size = attributes.inner_size.unwrap_or_else(|| PhysicalSize::new(800, 600).into());
        let max_size = PhysicalSize::new(f64::MAX, f64::MAX).into();
        let min_size = PhysicalSize::new(0, 0).into();
        let clamped_size = Size::clamp(size, min_size, max_size, win.scale_factor());
        win.request_inner_size(clamped_size);
    }
}
unsafe fn init(
    attributes: WindowAttributes,
    event_loop: &ActiveEventLoop,
) -> Result<Window, RootOsError> {
    let title = util::encode_wide(&attributes.title);

    let class_name = util::encode_wide(&attributes.platform_specific.class_name);
    unsafe { register_window_class(&class_name) };

    // LOCAL PATCH (aterm): the flags of the attributes no aterm crate sets hold upstream's
    // defaults: activated, resizable, minimizable, maximizable and closable (the buttons
    // upstream enabled in `on_create`), clipping its children, on the taskbar with no owner
    // window, no menu, and neither maximized nor full screen at creation. (Decorated, opaque and
    // at the normal level, which no flag says any more: `window_state::WindowFlags`.)
    let mut window_flags = WindowFlags::MARKER_ACTIVATE
        | WindowFlags::RESIZABLE
        | WindowFlags::MINIMIZABLE
        | WindowFlags::MAXIMIZABLE
        | WindowFlags::CLOSABLE
        | WindowFlags::CLIP_CHILDREN
        | WindowFlags::ON_TASKBAR;
    window_flags
        .set(WindowFlags::NO_BACK_BUFFER, attributes.platform_specific.no_redirection_bitmap);

    let mut initdata = InitData { event_loop, attributes, window_flags, window: None };

    let (style, ex_style) = window_flags.to_window_styles();
    let handle = unsafe {
        CreateWindowExW(
            ex_style,
            class_name.as_ptr(),
            title.as_ptr(),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            util::get_instance_handle(),
            &mut initdata as *mut _ as *mut _,
        )
    };

    if let Err(panic_error) = event_loop.runner_shared.take_panic_error() {
        panic::resume_unwind(panic_error)
    }

    if handle.is_null() {
        return Err(os_error!(io::Error::last_os_error()));
    }

    Ok(initdata.window.unwrap())
}

unsafe fn register_window_class(class_name: &[u16]) {
    let class = WNDCLASSEXW {
        cbSize: mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(super::event_loop::public_window_callback),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: util::get_instance_handle(),
        hIcon: std::ptr::null_mut(),
        hCursor: std::ptr::null_mut(), // must be null in order for cursor state to work properly
        hbrBackground: std::ptr::null_mut(),
        lpszMenuName: ptr::null(),
        lpszClassName: class_name.as_ptr(),
        hIconSm: std::ptr::null_mut(),
    };

    unsafe { RegisterClassExW(&class) };
}

struct ComInitialized(#[allow(dead_code)] *mut ());
impl Drop for ComInitialized {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

thread_local! {
    static COM_INITIALIZED: ComInitialized = {
        unsafe {
            CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32);
            ComInitialized(ptr::null_mut())
        }
    };

    static TASKBAR_LIST: Cell<*mut ITaskbarList> = const { Cell::new(ptr::null_mut()) };
    static TASKBAR_LIST2: Cell<*mut ITaskbarList2> = const { Cell::new(ptr::null_mut()) };
}

pub fn com_initialized() {
    COM_INITIALIZED.with(|_| {});
}

unsafe fn taskbar_mark_fullscreen(handle: HWND, fullscreen: bool) {
    com_initialized();

    TASKBAR_LIST2.with(|task_bar_list2_ptr| {
        let mut task_bar_list2 = task_bar_list2_ptr.get();

        if task_bar_list2.is_null() {
            let hr = unsafe {
                CoCreateInstance(
                    &CLSID_TaskbarList,
                    ptr::null_mut(),
                    CLSCTX_ALL,
                    &IID_ITaskbarList2,
                    &mut task_bar_list2 as *mut _ as *mut _,
                )
            };
            if hr != S_OK {
                return;
            }

            let hr_init = unsafe { (*(*task_bar_list2).lpVtbl).parent.HrInit };
            if unsafe { hr_init(task_bar_list2.cast()) } != S_OK {
                return;
            }
            task_bar_list2_ptr.set(task_bar_list2)
        }

        task_bar_list2 = task_bar_list2_ptr.get();
        let mark_fullscreen_window = unsafe { (*(*task_bar_list2).lpVtbl).MarkFullscreenWindow };
        unsafe { mark_fullscreen_window(task_bar_list2, handle, fullscreen.into()) };
    })
}

/// Adds the window's taskbar tab (`ITaskbarList::AddTab`), at creation and when the taskbar is
/// made again (`TaskbarCreated`).
///
/// LOCAL PATCH (aterm): upstream's `set_skip_taskbar(hwnd, false)`; nothing asks for the tab to
/// be skipped (`DeleteTab`) any more.
pub(crate) unsafe fn add_taskbar_tab(hwnd: HWND) {
    com_initialized();
    TASKBAR_LIST.with(|task_bar_list_ptr| {
        let mut task_bar_list = task_bar_list_ptr.get();

        if task_bar_list.is_null() {
            let hr = unsafe {
                CoCreateInstance(
                    &CLSID_TaskbarList,
                    ptr::null_mut(),
                    CLSCTX_ALL,
                    &IID_ITaskbarList,
                    &mut task_bar_list as *mut _ as *mut _,
                )
            };
            if hr != S_OK {
                return;
            }

            let hr_init = unsafe { (*(*task_bar_list).lpVtbl).HrInit };
            if unsafe { hr_init(task_bar_list.cast()) } != S_OK {
                return;
            }
            task_bar_list_ptr.set(task_bar_list)
        }

        task_bar_list = task_bar_list_ptr.get();
        let add_tab = unsafe { (*(*task_bar_list).lpVtbl).AddTab };
        unsafe { add_tab(task_bar_list, hwnd) };
    });
}

unsafe fn force_window_active(handle: HWND) {
    let alt_sc = unsafe { MapVirtualKeyW(VK_MENU as u32, MAPVK_VK_TO_VSC) };

    let inputs = [
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_LMENU,
                    wScan: alt_sc as u16,
                    dwFlags: KEYEVENTF_EXTENDEDKEY,
                    dwExtraInfo: 0,
                    time: 0,
                },
            },
        },
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_LMENU,
                    wScan: alt_sc as u16,
                    dwFlags: KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP,
                    dwExtraInfo: 0,
                    time: 0,
                },
            },
        },
    ];

    unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), mem::size_of::<INPUT>() as i32) };

    unsafe { SetForegroundWindow(handle) };
}

/// LOCAL PATCH (aterm).
fn attention_flash(window: HWND, request_type: Option<UserAttentionType>) -> FLASHWINFO {
    let (flags, count) = request_type
        .map(|ty| match ty {
            UserAttentionType::Critical => (FLASHW_ALL | FLASHW_TIMERNOFG, u32::MAX),
            UserAttentionType::Informational => (FLASHW_TRAY | FLASHW_TIMERNOFG, 0),
        })
        .unwrap_or((FLASHW_STOP, 0));
    FLASHWINFO {
        cbSize: mem::size_of::<FLASHWINFO>() as u32,
        hwnd: window,
        dwFlags: flags,
        uCount: count,
        dwTimeout: 0,
    }
}

#[cfg(test)]
mod attention_tests {
    use super::{attention_flash, UserAttentionType};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FLASHW_ALL, FLASHW_STOP, FLASHW_TIMERNOFG, FLASHW_TRAY,
    };

    #[test]
    fn a_bell_flashes_the_taskbar_button_until_the_window_comes_forward() {
        let hwnd = 0x1234 as _;
        let bell = attention_flash(hwnd, Some(UserAttentionType::Informational));
        assert_eq!(
            (bell.dwFlags, bell.uCount, bell.dwTimeout),
            (FLASHW_TRAY | FLASHW_TIMERNOFG, 0, 0)
        );
        assert_eq!(bell.hwnd, hwnd);
        #[cfg(target_pointer_width = "64")]
        assert_eq!(bell.cbSize, 32);
        let critical = attention_flash(hwnd, Some(UserAttentionType::Critical));
        assert_eq!((critical.dwFlags, critical.uCount), (FLASHW_ALL | FLASHW_TIMERNOFG, u32::MAX));
        assert_eq!(attention_flash(hwnd, None).dwFlags, FLASHW_STOP);
    }
}
