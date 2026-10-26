// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/windows/event_loop.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE.
// Modified by Andrew Yates for the aterm project in 2026: the IME messages' decisions are
// `ime::ime_events`.

#![allow(non_snake_case)]

mod runner;

use std::cell::Cell;
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle, RawHandle};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use std::{mem, panic, ptr};

use crate::utils::Lazy;

use windows_sys::Win32::Foundation::{
    GetLastError, FALSE, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WAIT_FAILED, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromRect, MonitorFromWindow, RedrawWindow, ScreenToClient,
    ValidateRect, MONITORINFO, MONITOR_DEFAULTTONULL, RDW_INTERNALPAINT, SC_SCREENSAVE,
};
use windows_sys::Win32::System::Ole::RevokeDragDrop;
use windows_sys::Win32::System::Threading::{
    CreateWaitableTimerExW, GetCurrentThreadId, SetWaitableTimer,
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, INFINITE, TIMER_ALL_ACCESS,
};
use windows_sys::Win32::UI::Controls::{HOVER_DEFAULT, WM_MOUSELEAVE};
use windows_sys::Win32::UI::Input::Ime::ISC_SHOWUICOMPOSITIONWINDOW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows_sys::Win32::UI::Input::Pointer::{
    POINTER_FLAG_DOWN, POINTER_FLAG_UP, POINTER_FLAG_UPDATE,
};
use windows_sys::Win32::UI::Input::Touch::{
    CloseTouchInputHandle, GetTouchInputInfo, TOUCHEVENTF_DOWN, TOUCHEVENTF_MOVE, TOUCHEVENTF_UP,
    TOUCHINPUT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetCursorPos,
    GetMenu, KillTimer, LoadCursorW, MsgWaitForMultipleObjectsEx, PeekMessageW, PostMessageW,
    RegisterClassExW, RegisterWindowMessageA, SetCursor, SetTimer, SetWindowPos, TranslateMessage,
    CREATESTRUCTW, GWL_STYLE, GWL_USERDATA, HTCAPTION, HTCLIENT, MNC_CLOSE, MSG,
    MWMO_INPUTAVAILABLE, PM_REMOVE, PT_PEN, PT_TOUCH, QS_ALLINPUT, SC_MINIMIZE, SC_RESTORE,
    SIZE_MAXIMIZED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, USER_TIMER_MINIMUM,
    WHEEL_DELTA, WINDOWPOS, WMSZ_BOTTOM, WMSZ_BOTTOMLEFT, WMSZ_BOTTOMRIGHT, WMSZ_LEFT, WMSZ_RIGHT,
    WMSZ_TOP, WMSZ_TOPLEFT, WMSZ_TOPRIGHT, WM_CAPTURECHANGED, WM_CLOSE, WM_CREATE, WM_DESTROY,
    WM_DPICHANGED, WM_ENTERSIZEMOVE, WM_EXITSIZEMOVE, WM_GETMINMAXINFO, WM_IME_COMPOSITION,
    WM_IME_ENDCOMPOSITION, WM_IME_SETCONTEXT, WM_IME_STARTCOMPOSITION, WM_KEYDOWN, WM_KEYUP,
    WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MENUCHAR,
    WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCACTIVATE, WM_NCCREATE, WM_NCDESTROY,
    WM_NCLBUTTONDOWN, WM_PAINT, WM_POINTERDOWN, WM_POINTERUP, WM_POINTERUPDATE, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WM_SETCURSOR, WM_SETFOCUS, WM_SETTINGCHANGE, WM_SIZE, WM_SIZING, WM_SYSCOMMAND,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER, WM_TOUCH, WM_WINDOWPOSCHANGED, WM_WINDOWPOSCHANGING,
    WM_XBUTTONDOWN, WM_XBUTTONUP, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TRANSPARENT, WS_OVERLAPPED, WS_POPUP, WS_VISIBLE,
};

use crate::dpi::{PhysicalPosition, PhysicalSize};
use crate::error::EventLoopError;
use crate::event::{Event, Force, Ime, InnerSizeWriter, Touch, TouchPhase, WindowEvent};
use crate::event_loop::{ActiveEventLoop as RootAEL, ControlFlow, EventLoopClosed};
use crate::keyboard::ModifiersState;
use crate::platform::pump_events::PumpStatus;
use crate::platform_impl::platform::dark_mode::try_theme;
use crate::platform_impl::platform::dpi::{become_dpi_aware, dpi_to_scale_factor};
use crate::platform_impl::platform::drop_handler::FileDropHandler;
use crate::platform_impl::platform::ime::{ime_events, LazyImeContext};
use crate::platform_impl::platform::keyboard::KeyEventBuilder;
use crate::platform_impl::platform::keyboard_layout::LAYOUT_CACHE;
use crate::platform_impl::platform::monitor::{self, MonitorHandle};
use crate::platform_impl::platform::window::InitData;
use crate::platform_impl::platform::window_state::{CursorFlags, WindowFlags, WindowState};
use crate::platform_impl::platform::{util, Fullscreen, WindowId, DEVICE_ID};
use crate::window::WindowId as RootWindowId;
use runner::{EventLoopRunner, EventLoopRunnerShared};

use super::window::add_taskbar_tab;

/// some backends like macos uses an uninhabited `Never` type,
/// on windows, `UserEvent`s are also dispatched through the
/// WNDPROC callback, and due to the re-entrant nature of the
/// callback, recursively delivered events must be queued in a
/// buffer, the current implementation put this queue in
/// `EventLoopRunner`, which is shared between the event pumping
/// loop and the callback.
pub(crate) struct UserEventPlaceholder;

pub(crate) struct WindowData {
    pub window_state: Arc<Mutex<WindowState>>,
    pub event_loop_runner: EventLoopRunnerShared<UserEventPlaceholder>,
    pub key_event_builder: KeyEventBuilder,
    pub _file_drop_handler: Option<FileDropHandler>,
    pub userdata_removed: Cell<bool>,
    pub recurse_depth: Cell<u32>,
    /// Whether the last `WM_SIZE` minimized the window.
    pub minimized: Cell<bool>,
}

const SIZE_MINIMIZED: u32 = 1;

fn minimize_edge(was: &Cell<bool>, wparam: WPARAM) -> Option<bool> {
    let minimized = wparam == SIZE_MINIMIZED as usize;
    (was.replace(minimized) != minimized).then_some(minimized)
}

#[cfg(test)]
mod minimize_edge_tests {
    use std::cell::Cell;

    use super::SIZE_MINIMIZED;
    use windows_sys::Win32::UI::WindowsAndMessaging::SIZE_MAXIMIZED;

    const SIZE_RESTORED: u32 = 0;

    #[test]
    fn only_the_edges_of_a_minimize_are_occlusion() {
        let was = Cell::new(false);
        let edge = |wparam: u32| super::minimize_edge(&was, wparam as usize);
        assert_eq!(edge(SIZE_RESTORED), None, "a drag step");
        assert_eq!(edge(SIZE_MAXIMIZED), None, "a maximize of a shown window");
        assert_eq!(edge(SIZE_MINIMIZED), Some(true));
        assert_eq!(edge(SIZE_MINIMIZED), None, "already minimized");
        assert_eq!(edge(SIZE_MAXIMIZED), Some(false), "back, maximized");
        assert_eq!(edge(SIZE_MINIMIZED), Some(true));
        assert_eq!(edge(SIZE_RESTORED), Some(false), "back, restored");
        assert_eq!(edge(SIZE_RESTORED), None);
    }
}

impl WindowData {
    fn send_event(&self, event: Event<UserEventPlaceholder>) {
        self.event_loop_runner.send_event(event);
    }

    fn window_state_lock(&self) -> MutexGuard<'_, WindowState> {
        self.window_state.lock().unwrap()
    }
}

struct ThreadMsgTargetData {
    event_loop_runner: EventLoopRunnerShared<UserEventPlaceholder>,
}

impl ThreadMsgTargetData {
    fn send_event(&self, event: Event<UserEventPlaceholder>) {
        self.event_loop_runner.send_event(event);
    }
}

/// The result of a subclass procedure (the message handling callback)
#[derive(Clone, Copy)]
pub(crate) enum ProcResult {
    DefWindowProc(WPARAM),
    Value(isize),
}

pub struct EventLoop<T: 'static> {
    user_event_sender: Sender<T>,
    user_event_receiver: Receiver<T>,
    window_target: RootAEL,
    high_resolution_timer: Option<OwnedHandle>,
}

/// LOCAL PATCH (aterm): empty. Upstream's `EventLoopBuilderExtWindows` (`with_any_thread`,
/// `with_dpi_aware`, `with_msg_hook`) had no aterm caller, so the loop is made on the main thread,
/// the process is DPI aware, and every message is translated and dispatched.
#[derive(Default)]
pub(crate) struct PlatformSpecificEventLoopAttributes;

pub struct ActiveEventLoop {
    thread_id: u32,
    thread_msg_target: HWND,
    pub(crate) runner_shared: EventLoopRunnerShared<UserEventPlaceholder>,
}

impl<T: 'static> EventLoop<T> {
    pub(crate) fn new(
        _attributes: &mut PlatformSpecificEventLoopAttributes,
    ) -> Result<Self, EventLoopError> {
        let thread_id = unsafe { GetCurrentThreadId() };

        if thread_id != main_thread_id() {
            panic!(
                "Initializing the event loop outside of the main thread is a significant \
                 cross-platform compatibility hazard."
            );
        }

        become_dpi_aware();

        let thread_msg_target = create_event_target_window();

        let runner_shared = Rc::new(EventLoopRunner::new(thread_msg_target));

        let (user_event_sender, user_event_receiver) = mpsc::channel();
        insert_event_target_window_data(thread_msg_target, runner_shared.clone());

        Ok(EventLoop {
            user_event_sender,
            user_event_receiver,
            window_target: RootAEL {
                p: ActiveEventLoop { thread_id, thread_msg_target, runner_shared },
                _marker: PhantomData,
            },
            high_resolution_timer: None,
        })
    }

    pub fn window_target(&self) -> &RootAEL {
        &self.window_target
    }

    pub fn run<F>(mut self, event_handler: F) -> Result<(), EventLoopError>
    where
        F: FnMut(Event<T>, &RootAEL),
    {
        self.run_on_demand(event_handler)
    }

    pub fn run_on_demand<F>(&mut self, mut event_handler: F) -> Result<(), EventLoopError>
    where
        F: FnMut(Event<T>, &RootAEL),
    {
        {
            let runner = &self.window_target.p.runner_shared;

            let event_loop_windows_ref = &self.window_target;
            let user_event_receiver = &self.user_event_receiver;
            // SAFETY: `runner.clear_event_handler()` is called before returning.
            unsafe {
                runner.set_event_handler(move |event| {
                    let event = match event.map_nonuser_event() {
                        Ok(non_user_event) => non_user_event,
                        Err(_user_event_placeholder) => Event::UserEvent(
                            user_event_receiver
                                .try_recv()
                                .expect("user event signaled but not received"),
                        ),
                    };
                    event_handler(event, event_loop_windows_ref)
                });
            }
        }

        let exit_code = loop {
            self.wait_for_messages(None);
            if let Some(code) = self.exit_code() {
                break code;
            }

            self.dispatch_peeked_messages();

            if let Some(code) = self.exit_code() {
                break code;
            }
        };

        let runner = &self.window_target.p.runner_shared;
        runner.loop_destroyed();

        // SAFETY: `reset_runner()` clears the event handler.
        runner.reset_runner();

        if exit_code == 0 {
            Ok(())
        } else {
            Err(EventLoopError::ExitFailure(exit_code))
        }
    }

    pub fn pump_events<F>(&mut self, timeout: Option<Duration>, mut event_handler: F) -> PumpStatus
    where
        F: FnMut(Event<T>, &RootAEL),
    {
        {
            let runner = &self.window_target.p.runner_shared;
            let event_loop_windows_ref = &self.window_target;
            let user_event_receiver = &self.user_event_receiver;

            // SAFETY: `runner.clear_event_handler()` is called before returning.
            unsafe {
                runner.set_event_handler(move |event| {
                    let event = match event.map_nonuser_event() {
                        Ok(non_user_event) => non_user_event,
                        Err(_user_event_placeholder) => Event::UserEvent(
                            user_event_receiver
                                .recv()
                                .expect("user event signaled but not received"),
                        ),
                    };
                    event_handler(event, event_loop_windows_ref)
                });
                runner.wakeup();
            }
        }

        if self.exit_code().is_none() {
            self.wait_for_messages(timeout);
        }
        if self.exit_code().is_none() {
            self.dispatch_peeked_messages();
        }

        let runner = &self.window_target.p.runner_shared;

        let status = if let Some(code) = runner.exit_code() {
            runner.loop_destroyed();

            runner.reset_runner();
            PumpStatus::Exit(code)
        } else {
            runner.prepare_wait();
            PumpStatus::Continue
        };

        // SAFETY: pairs with `runner.set_event_handler()` so the callback does not outlive its borrow.
        runner.clear_event_handler();

        status
    }

    fn wait_for_messages(&mut self, timeout: Option<Duration>) {
        let runner = &self.window_target.p.runner_shared;

        runner.prepare_wait();
        wait_for_messages_impl(&mut self.high_resolution_timer, runner.control_flow(), timeout);
        runner.wakeup();
    }

    fn dispatch_peeked_messages(&mut self) {
        let runner = &self.window_target.p.runner_shared;

        runner.interrupt_msg_dispatch.set(false);

        // SAFETY: `MSG` is a plain C struct safe to zero-initialize.
        let mut msg: MSG = unsafe { mem::zeroed() };

        loop {
            unsafe {
                if PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) == false.into() {
                    break;
                }

                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            if let Err(payload) = runner.take_panic_error() {
                runner.reset_runner();
                panic::resume_unwind(payload);
            }

            if let Some(_code) = runner.exit_code() {
                break;
            }

            if runner.interrupt_msg_dispatch.get() {
                break;
            }
        }
    }

    pub fn create_proxy(&self) -> EventLoopProxy<T> {
        EventLoopProxy {
            target_window: self.window_target.p.thread_msg_target,
            event_send: self.user_event_sender.clone(),
        }
    }

    fn exit_code(&self) -> Option<i32> {
        self.window_target.p.exit_code()
    }
}

impl ActiveEventLoop {
    #[inline(always)]
    pub(crate) fn create_thread_executor(&self) -> EventLoopThreadExecutor {
        EventLoopThreadExecutor { thread_id: self.thread_id, target_window: self.thread_msg_target }
    }

    pub fn available_monitors(&self) -> VecDeque<MonitorHandle> {
        monitor::available_monitors()
    }

    pub fn primary_monitor(&self) -> Option<MonitorHandle> {
        let monitor = monitor::primary_monitor();
        Some(monitor)
    }

    #[cfg(feature = "rwh_06")]
    pub fn raw_display_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
        Ok(rwh_06::RawDisplayHandle::Windows(rwh_06::WindowsDisplayHandle::new()))
    }

    pub(crate) fn set_control_flow(&self, control_flow: ControlFlow) {
        self.runner_shared.set_control_flow(control_flow)
    }

    pub(crate) fn exit(&self) {
        self.runner_shared.set_exit_code(0)
    }

    pub(crate) fn exiting(&self) -> bool {
        self.runner_shared.exit_code().is_some()
    }

    fn exit_code(&self) -> Option<i32> {
        self.runner_shared.exit_code()
    }
}

fn main_thread_id() -> u32 {
    static mut MAIN_THREAD_ID: u32 = 0;

    #[used]
    #[allow(non_upper_case_globals)]
    #[link_section = ".CRT$XCU"]
    static INIT_MAIN_THREAD_ID: unsafe extern "C" fn() = {
        unsafe extern "C" fn initer() {
            unsafe {
                MAIN_THREAD_ID = GetCurrentThreadId();
            }
        }
        initer
    };

    unsafe { MAIN_THREAD_ID }
}

fn min_timeout(a: Option<Duration>, b: Option<Duration>) -> Option<Duration> {
    a.map_or(b, |a_timeout| b.map_or(Some(a_timeout), |b_timeout| Some(a_timeout.min(b_timeout))))
}

fn dur2timeout(dur: Duration) -> u32 {
    dur.as_secs()
        .checked_mul(1000)
        .and_then(|ms| ms.checked_add((dur.subsec_nanos() as u64) / 1_000_000))
        .and_then(|ms| {
            if !dur.subsec_nanos().is_multiple_of(1_000_000) {
                ms.checked_add(1)
            } else {
                Some(ms)
            }
        })
        .map(|ms| if ms > u32::MAX as u64 { INFINITE } else { ms as u32 })
        .unwrap_or(INFINITE)
}

impl<T> Drop for EventLoop<T> {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.window_target.p.thread_msg_target);
        }
    }
}

const FIFTY_DAYS: Duration = Duration::from_secs(50_u64 * 24 * 60 * 60);
const MIN_WAIT: Duration = Duration::from_nanos(100);

fn create_high_resolution_timer() -> Option<OwnedHandle> {
    unsafe {
        let handle: HANDLE = CreateWaitableTimerExW(
            ptr::null(),
            ptr::null(),
            CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
            TIMER_ALL_ACCESS,
        );
        if handle.is_null() {
            None
        } else {
            Some(OwnedHandle::from_raw_handle(handle))
        }
    }
}

/// # Safety
/// `timer` must be a valid handle from [`create_high_resolution_timer`] and `timeout` must fit in `(0, i64::MAX * 100ns)`.
unsafe fn set_high_resolution_timer(timer: RawHandle, timeout: Duration) -> Result<(), u32> {
    const INTERVAL_NS: u32 = MIN_WAIT.subsec_nanos();
    const INTERVALS_IN_SEC: u64 = (Duration::from_secs(1).as_nanos() / INTERVAL_NS as u128) as u64;
    let intervals_to_wait: u64 =
        timeout.as_secs() * INTERVALS_IN_SEC + u64::from(timeout.subsec_nanos() / INTERVAL_NS);
    debug_assert!(intervals_to_wait < i64::MAX as u64, "Must be called with smaller duration",);
    let due_time: i64 = -(intervals_to_wait as i64);
    unsafe {
        let set_result = SetWaitableTimer(timer, &due_time, 0, None, ptr::null(), FALSE);
        if set_result != FALSE {
            Ok(())
        } else {
            Err(GetLastError())
        }
    }
}

fn wait_for_messages_impl(
    high_resolution_timer: &mut Option<OwnedHandle>,
    control_flow: ControlFlow,
    timeout: Option<Duration>,
) {
    let timeout = {
        let control_flow_timeout = match control_flow {
            ControlFlow::Wait => None,
            ControlFlow::Poll => Some(Duration::ZERO),
            ControlFlow::WaitUntil(wait_deadline) => {
                let start = Instant::now();
                Some(wait_deadline.saturating_duration_since(start))
            },
        };
        let timeout = min_timeout(timeout, control_flow_timeout);
        if timeout == Some(Duration::ZERO) {
            return;
        }
        timeout
            .map(|t| t.min(FIFTY_DAYS))
            // If timeout is less than minimally supported by Windows,
            // increase it to that minimum. Who want less than microsecond delays anyway?
            .map(|t| t.max(MIN_WAIT))
    };

    if timeout.is_some() && high_resolution_timer.is_none() {
        *high_resolution_timer = create_high_resolution_timer();
    }

    let high_resolution_timer: Option<RawHandle> =
        high_resolution_timer.as_ref().map(OwnedHandle::as_raw_handle);

    let use_timer: bool;
    if let (Some(handle), Some(timeout)) = (high_resolution_timer, timeout) {
        let res = unsafe {
            // SAFETY: `handle` was created by `create_high_resolution_timer` and `timeout` is clamped.
            set_high_resolution_timer(handle, timeout)
        };
        if let Err(_error_code) = res {
            tracing::trace!("Failed to set high resolution timer: last error {}", _error_code);
            use_timer = false;
        } else {
            use_timer = true;
        }
    } else {
        use_timer = false;
    }

    unsafe {
        let wait_duration_ms = timeout.map(dur2timeout).unwrap_or(INFINITE);

        let (num_handles, raw_handles) =
            if use_timer { (1, [high_resolution_timer.unwrap()]) } else { (0, [ptr::null_mut()]) };

        let result = MsgWaitForMultipleObjectsEx(
            num_handles,
            raw_handles.as_ptr() as *const _,
            wait_duration_ms,
            QS_ALLINPUT,
            MWMO_INPUTAVAILABLE,
        );
        if result == WAIT_FAILED {
            tracing::warn!("Failed to MsgWaitForMultipleObjectsEx: error code {}", GetLastError(),);
        }
    }
}

pub(crate) struct EventLoopThreadExecutor {
    thread_id: u32,
    target_window: HWND,
}

unsafe impl Send for EventLoopThreadExecutor {}
unsafe impl Sync for EventLoopThreadExecutor {}

impl EventLoopThreadExecutor {
    /// Check to see if we're in the parent event loop's thread.
    pub(super) fn in_event_loop_thread(&self) -> bool {
        let cur_thread_id = unsafe { GetCurrentThreadId() };
        self.thread_id == cur_thread_id
    }

    /// Executes a function in the event loop thread. If we're already in the event loop thread,
    /// we just call the function directly.
    pub(super) fn execute_in_thread<F>(&self, mut function: F)
    where
        F: FnMut() + Send + 'static,
    {
        unsafe {
            if self.in_event_loop_thread() {
                function();
            } else {
                let boxed2: ThreadExecFn = Box::new(Box::new(function));

                let raw = Box::into_raw(boxed2);

                let res = PostMessageW(self.target_window, EXEC_MSG_ID.get(), raw as usize, 0);
                assert!(res != false.into(), "PostMessage failed; is the messages queue full?");
            }
        }
    }
}

type ThreadExecFn = Box<Box<dyn FnMut()>>;

pub struct EventLoopProxy<T: 'static> {
    target_window: HWND,
    event_send: Sender<T>,
}
unsafe impl<T: Send + 'static> Send for EventLoopProxy<T> {}
unsafe impl<T: Send + 'static> Sync for EventLoopProxy<T> {}

impl<T: 'static> Clone for EventLoopProxy<T> {
    fn clone(&self) -> Self {
        Self { target_window: self.target_window, event_send: self.event_send.clone() }
    }
}

impl<T: 'static> EventLoopProxy<T> {
    pub fn send_event(&self, event: T) -> Result<(), EventLoopClosed<T>> {
        self.event_send
            .send(event)
            .inspect(|()| {
                unsafe { PostMessageW(self.target_window, USER_EVENT_MSG_ID.get(), 0, 0) };
            })
            .map_err(|e| EventLoopClosed(e.0))
    }
}

/// A lazily-initialized window message ID.
pub struct LazyMessageId {
    /// The ID.
    id: AtomicU32,

    /// The name of the message.
    name: &'static str,
}

const INVALID_ID: u32 = 0x0;

impl LazyMessageId {
    const fn new(name: &'static str) -> Self {
        Self { id: AtomicU32::new(INVALID_ID), name }
    }

    /// Get the message ID.
    pub fn get(&self) -> u32 {
        let id = self.id.load(Ordering::Relaxed);

        if id != INVALID_ID {
            return id;
        }

        // SAFETY: `self.name` is a NUL-terminated string.
        assert!(self.name.ends_with('\0'));
        let new_id = unsafe { RegisterWindowMessageA(self.name.as_ptr()) };

        assert_ne!(
            new_id,
            0,
            "RegisterWindowMessageA returned zero for '{}': {}",
            self.name,
            std::io::Error::last_os_error()
        );

        self.id.store(new_id, Ordering::Relaxed);

        new_id
    }
}

static USER_EVENT_MSG_ID: LazyMessageId = LazyMessageId::new("Winit::WakeupMsg\0");
static EXEC_MSG_ID: LazyMessageId = LazyMessageId::new("Winit::ExecMsg\0");
pub(crate) static DESTROY_MSG_ID: LazyMessageId = LazyMessageId::new("Winit::DestroyMsg\0");
pub(crate) static SET_RETAIN_STATE_ON_SIZE_MSG_ID: LazyMessageId =
    LazyMessageId::new("Winit::SetRetainMaximized\0");
const SIZE_MOVE_PUMP_TIMER_ID: usize = 0x4154_524d;
static THREAD_EVENT_TARGET_WINDOW_CLASS: Lazy<Vec<u16>> =
    Lazy::new(|| util::encode_wide("Winit Thread Event Target"));
/// When the taskbar is created, it registers a message with the "TaskbarCreated" string and then
/// broadcasts this message to all top-level windows <https://docs.microsoft.com/en-us/windows/win32/shell/taskbar#taskbar-creation-notification>
pub(crate) static TASKBAR_CREATED: LazyMessageId = LazyMessageId::new("TaskbarCreated\0");

fn create_event_target_window() -> HWND {
    use windows_sys::Win32::UI::WindowsAndMessaging::{CS_HREDRAW, CS_VREDRAW};
    unsafe {
        let class = WNDCLASSEXW {
            cbSize: mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(thread_event_target_callback),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: util::get_instance_handle(),
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: ptr::null(),
            lpszClassName: THREAD_EVENT_TARGET_WINDOW_CLASS.as_ptr(),
            hIconSm: std::ptr::null_mut(),
        };

        RegisterClassExW(&class);
    }

    unsafe {
        let window = CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED | WS_EX_TOOLWINDOW,
            THREAD_EVENT_TARGET_WINDOW_CLASS.as_ptr(),
            ptr::null(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            util::get_instance_handle(),
            ptr::null(),
        );

        super::set_window_long(
            window,
            GWL_STYLE,
            // Visible so the layered target receives WM_PAINT during resize.
            (WS_VISIBLE | WS_POPUP) as isize,
        );
        window
    }
}

fn insert_event_target_window_data(
    thread_msg_target: HWND,
    event_loop_runner: EventLoopRunnerShared<UserEventPlaceholder>,
) {
    let userdata = ThreadMsgTargetData { event_loop_runner };
    let input_ptr = Box::into_raw(Box::new(userdata));

    unsafe { super::set_window_long(thread_msg_target, GWL_USERDATA, input_ptr as isize) };
}

unsafe fn capture_mouse(window: HWND, window_state: &mut WindowState) {
    window_state.mouse.capture_count += 1;
    unsafe { SetCapture(window) };
}

unsafe fn release_mouse(mut window_state: MutexGuard<'_, WindowState>) {
    window_state.mouse.capture_count = window_state.mouse.capture_count.saturating_sub(1);
    if window_state.mouse.capture_count == 0 {
        drop(window_state);
        unsafe { ReleaseCapture() };
    }
}

fn normalize_pointer_pressure(pressure: u32) -> Option<Force> {
    match pressure {
        1..=1024 => Some(Force::Normalized(pressure as f64 / 1024.0)),
        _ => None,
    }
}

fn update_modifiers(window: HWND, userdata: &WindowData) {
    use crate::event::WindowEvent::ModifiersChanged;

    let modifiers = {
        let mut layouts = LAYOUT_CACHE.lock().unwrap();
        layouts.get_agnostic_mods()
    };

    let mut window_state = userdata.window_state.lock().unwrap();
    if window_state.modifiers_state != modifiers {
        window_state.modifiers_state = modifiers;

        drop(window_state);

        userdata.send_event(Event::WindowEvent {
            window_id: RootWindowId(WindowId(window)),
            event: ModifiersChanged(modifiers.into()),
        });
    }
}

unsafe fn gain_active_focus(window: HWND, userdata: &WindowData) {
    use crate::event::WindowEvent::Focused;

    update_modifiers(window, userdata);

    userdata.send_event(Event::WindowEvent {
        window_id: RootWindowId(WindowId(window)),
        event: Focused(true),
    });
}

unsafe fn lose_active_focus(window: HWND, userdata: &WindowData) {
    use crate::event::WindowEvent::{Focused, ModifiersChanged};

    userdata.window_state_lock().modifiers_state = ModifiersState::empty();
    userdata.send_event(Event::WindowEvent {
        window_id: RootWindowId(WindowId(window)),
        event: ModifiersChanged(ModifiersState::empty().into()),
    });

    userdata.send_event(Event::WindowEvent {
        window_id: RootWindowId(WindowId(window)),
        event: Focused(false),
    });
}

/// Any window whose callback is configured to this function will have its events propagated
/// through the events loop of the thread the window was created in.
pub(super) unsafe extern "system" fn public_window_callback(
    window: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let userdata = unsafe { super::get_window_long(window, GWL_USERDATA) };

    let userdata_ptr = match (userdata, msg) {
        (0, WM_NCCREATE) => {
            let createstruct = unsafe { &mut *(lparam as *mut CREATESTRUCTW) };
            let initdata = unsafe { &mut *(createstruct.lpCreateParams as *mut InitData<'_>) };

            let result = match unsafe { initdata.on_nccreate(window) } {
                Some(userdata) => unsafe {
                    super::set_window_long(window, GWL_USERDATA, userdata as _);
                    DefWindowProcW(window, msg, wparam, lparam)
                },
                None => -1,
            };

            return result;
        },
        (0, WM_CREATE) => return -1,
        (_, WM_CREATE) => unsafe {
            let createstruct = &mut *(lparam as *mut CREATESTRUCTW);
            let initdata = createstruct.lpCreateParams;
            let initdata = &mut *(initdata as *mut InitData<'_>);

            initdata.on_create();
            return DefWindowProcW(window, msg, wparam, lparam);
        },
        (0, _) => return unsafe { DefWindowProcW(window, msg, wparam, lparam) },
        _ => userdata as *mut WindowData,
    };

    let (result, userdata_removed, recurse_depth) = {
        let userdata = unsafe { &*(userdata_ptr) };

        userdata.recurse_depth.set(userdata.recurse_depth.get() + 1);

        let result = unsafe { public_window_callback_inner(window, msg, wparam, lparam, userdata) };

        let userdata_removed = userdata.userdata_removed.get();
        let recurse_depth = userdata.recurse_depth.get() - 1;
        userdata.recurse_depth.set(recurse_depth);

        (result, userdata_removed, recurse_depth)
    };

    if userdata_removed && recurse_depth == 0 {
        drop(unsafe { Box::from_raw(userdata_ptr) });
    }

    result
}

unsafe fn public_window_callback_inner(
    window: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    userdata: &WindowData,
) -> LRESULT {
    let mut result = ProcResult::DefWindowProc(wparam);

    let mods_changed_callback = || match msg {
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
            update_modifiers(window, userdata);
            result = ProcResult::Value(0);
        },
        _ => (),
    };
    userdata
        .event_loop_runner
        .catch_unwind(mods_changed_callback)
        .unwrap_or_else(|| result = ProcResult::Value(-1));

    let keyboard_callback = || {
        use crate::event::WindowEvent::KeyboardInput;
        let events =
            userdata.key_event_builder.process_message(window, msg, wparam, lparam, &mut result);
        for event in events {
            userdata.send_event(Event::WindowEvent {
                window_id: RootWindowId(WindowId(window)),
                event: KeyboardInput {
                    device_id: DEVICE_ID,
                    event: event.event,
                    is_synthetic: event.is_synthetic,
                },
            });
        }
        if let Some(text) = userdata.key_event_builder.take_alt_code_commit() {
            userdata.send_event(Event::WindowEvent {
                window_id: RootWindowId(WindowId(window)),
                event: WindowEvent::Ime(Ime::Commit(text)),
            });
        }
    };
    userdata
        .event_loop_runner
        .catch_unwind(keyboard_callback)
        .unwrap_or_else(|| result = ProcResult::Value(-1));

    let callback = || match msg {
        WM_ENTERSIZEMOVE => {
            userdata
                .window_state_lock()
                .set_window_flags_in_place(|f| f.insert(WindowFlags::MARKER_IN_SIZE_MOVE));
            unsafe {
                SetTimer(window, SIZE_MOVE_PUMP_TIMER_ID, USER_TIMER_MINIMUM, None);
            }
            result = ProcResult::Value(0);
        },

        WM_EXITSIZEMOVE => {
            let mut state = userdata.window_state_lock();
            if state.dragging {
                state.dragging = false;
                unsafe { PostMessageW(window, WM_LBUTTONUP, 0, lparam) };
            }

            state.set_window_flags_in_place(|f| f.remove(WindowFlags::MARKER_IN_SIZE_MOVE));
            drop(state);
            unsafe {
                KillTimer(window, SIZE_MOVE_PUMP_TIMER_ID);
            }
            result = ProcResult::Value(0);
        },

        WM_TIMER if wparam == SIZE_MOVE_PUMP_TIMER_ID => {
            let in_size_move = userdata
                .window_state_lock()
                .window_flags()
                .contains(WindowFlags::MARKER_IN_SIZE_MOVE);
            if in_size_move && !userdata.event_loop_runner.should_buffer() {
                userdata.event_loop_runner.prepare_wait();
                userdata.event_loop_runner.wakeup();
            }
            result = ProcResult::Value(0);
        },

        WM_NCLBUTTONDOWN => {
            if wparam == HTCAPTION as _ {
                let lparam = 0;
                unsafe { PostMessageW(window, WM_MOUSEMOVE, 0, lparam) };
            }
            result = ProcResult::DefWindowProc(wparam);
        },

        WM_CLOSE => {
            use crate::event::WindowEvent::CloseRequested;
            userdata.send_event(Event::WindowEvent {
                window_id: RootWindowId(WindowId(window)),
                event: CloseRequested,
            });
            result = ProcResult::Value(0);
        },

        WM_DESTROY => {
            use crate::event::WindowEvent::Destroyed;
            unsafe { RevokeDragDrop(window) };
            userdata.send_event(Event::WindowEvent {
                window_id: RootWindowId(WindowId(window)),
                event: Destroyed,
            });
            result = ProcResult::Value(0);
        },

        WM_NCDESTROY => {
            unsafe { super::set_window_long(window, GWL_USERDATA, 0) };
            userdata.userdata_removed.set(true);
            super::dark_mode::forget_menu_theme(window);
            result = ProcResult::Value(0);
        },

        WM_PAINT => {
            userdata.window_state_lock().redraw_requested =
                userdata.event_loop_runner.should_buffer();

            if !userdata.event_loop_runner.should_buffer() {
                userdata.send_event(Event::WindowEvent {
                    window_id: RootWindowId(WindowId(window)),
                    event: WindowEvent::RedrawRequested,
                });
            }

            result = ProcResult::Value(unsafe { DefWindowProcW(window, msg, wparam, lparam) });
            if std::mem::take(&mut userdata.window_state_lock().redraw_requested) {
                unsafe {
                    RedrawWindow(window, ptr::null(), std::ptr::null_mut(), RDW_INTERNALPAINT)
                };
            }
        },
        WM_WINDOWPOSCHANGING => {
            let mut window_state = userdata.window_state_lock();
            if let Some(ref mut fullscreen) = window_state.fullscreen {
                let window_pos = unsafe { &mut *(lparam as *mut WINDOWPOS) };
                let new_rect = RECT {
                    left: window_pos.x,
                    top: window_pos.y,
                    right: window_pos.x + window_pos.cx,
                    bottom: window_pos.y + window_pos.cy,
                };

                const NOMOVE_OR_NOSIZE: u32 = SWP_NOMOVE | SWP_NOSIZE;

                let new_rect = if window_pos.flags & NOMOVE_OR_NOSIZE != 0 {
                    let cur_rect = util::WindowArea::Outer.get_rect(window).expect(
                        "Unexpected GetWindowRect failure; please report this error to \
                         rust-windowing/winit",
                    );

                    match window_pos.flags & NOMOVE_OR_NOSIZE {
                        NOMOVE_OR_NOSIZE => None,

                        SWP_NOMOVE => Some(RECT {
                            left: cur_rect.left,
                            top: cur_rect.top,
                            right: cur_rect.left + window_pos.cx,
                            bottom: cur_rect.top + window_pos.cy,
                        }),

                        SWP_NOSIZE => Some(RECT {
                            left: window_pos.x,
                            top: window_pos.y,
                            right: window_pos.x - cur_rect.left + cur_rect.right,
                            bottom: window_pos.y - cur_rect.top + cur_rect.bottom,
                        }),

                        _ => unreachable!(),
                    }
                } else {
                    Some(new_rect)
                };

                if let Some(new_rect) = new_rect {
                    let new_monitor = unsafe { MonitorFromRect(&new_rect, MONITOR_DEFAULTTONULL) };
                    match fullscreen {
                        Fullscreen::Borderless(ref mut fullscreen_monitor) => {
                            if !new_monitor.is_null()
                                && fullscreen_monitor
                                    .as_ref()
                                    .map(|monitor| new_monitor != monitor.hmonitor())
                                    .unwrap_or(true)
                            {
                                if let Ok(new_monitor_info) = monitor::get_monitor_info(new_monitor)
                                {
                                    let new_monitor_rect = new_monitor_info.monitorInfo.rcMonitor;
                                    window_pos.x = new_monitor_rect.left;
                                    window_pos.y = new_monitor_rect.top;
                                    window_pos.cx = new_monitor_rect.right - new_monitor_rect.left;
                                    window_pos.cy = new_monitor_rect.bottom - new_monitor_rect.top;
                                }
                                *fullscreen_monitor = Some(MonitorHandle::new(new_monitor));
                            }
                        },
                    }
                }
            }

            result = ProcResult::Value(0);
        },

        WM_WINDOWPOSCHANGED => {
            use crate::event::WindowEvent::Moved;

            let windowpos = lparam as *const WINDOWPOS;
            if unsafe { (*windowpos).flags & SWP_NOMOVE != SWP_NOMOVE } {
                let physical_position =
                    unsafe { PhysicalPosition::new((*windowpos).x, (*windowpos).y) };
                userdata.send_event(Event::WindowEvent {
                    window_id: RootWindowId(WindowId(window)),
                    event: Moved(physical_position),
                });
            }

            result = ProcResult::DefWindowProc(wparam);
        },

        WM_SIZE => {
            use crate::event::WindowEvent::{Occluded, Resized};
            let w = super::loword(lparam as u32) as u32;
            let h = super::hiword(lparam as u32) as u32;

            let physical_size = PhysicalSize::new(w, h);
            let event = Event::WindowEvent {
                window_id: RootWindowId(WindowId(window)),
                event: Resized(physical_size),
            };

            {
                let mut w = userdata.window_state_lock();
                if !w.window_flags().contains(WindowFlags::MARKER_RETAIN_STATE_ON_SIZE) {
                    let maximized = wparam == SIZE_MAXIMIZED as usize;
                    w.set_window_flags_in_place(|f| f.set(WindowFlags::MAXIMIZED, maximized));
                }
            }
            if let Some(occluded) = minimize_edge(&userdata.minimized, wparam) {
                userdata.send_event(Event::WindowEvent {
                    window_id: RootWindowId(WindowId(window)),
                    event: Occluded(occluded),
                });
            }
            userdata.send_event(event);
            result = ProcResult::Value(0);
        },

        WM_SIZING => {
            fn snap_to_nearest_increment_delta(value: i32, increment: i32) -> i32 {
                let half_one = increment / 2;
                let half_two = increment - half_one;
                half_one - (value - half_two) % increment
            }

            let scale_factor = userdata.window_state_lock().scale_factor;
            let Some(inc) = userdata
                .window_state_lock()
                .resize_increments
                .map(|inc| inc.to_physical(scale_factor))
                .filter(|inc| inc.width > 0 && inc.height > 0)
            else {
                result = ProcResult::Value(0);
                return;
            };

            let side = wparam as u32;
            let rect = unsafe { &mut *(lparam as *mut RECT) };

            let adj_rect = userdata
                .window_state_lock()
                .window_flags
                .adjust_rect(window, *rect)
                .unwrap_or(*rect);
            let deco_width = rect.left - adj_rect.left + adj_rect.right - rect.right;
            let deco_height = rect.top - adj_rect.top + adj_rect.bottom - rect.bottom;

            let width = rect.right - rect.left - deco_width;
            let height = rect.bottom - rect.top - deco_height;

            let width_delta = snap_to_nearest_increment_delta(width, inc.width);
            let height_delta = snap_to_nearest_increment_delta(height, inc.height);

            // LOCAL PATCH (aterm): no minimum or maximum size to keep the snapped size within (no
            // aterm crate sets either on Windows).

            match side {
                WMSZ_LEFT | WMSZ_BOTTOMLEFT | WMSZ_TOPLEFT => {
                    rect.left -= width_delta;
                },
                WMSZ_RIGHT | WMSZ_BOTTOMRIGHT | WMSZ_TOPRIGHT => {
                    rect.right += width_delta;
                },
                _ => {},
            }

            match side {
                WMSZ_TOP | WMSZ_TOPLEFT | WMSZ_TOPRIGHT => {
                    rect.top -= height_delta;
                },
                WMSZ_BOTTOM | WMSZ_BOTTOMLEFT | WMSZ_BOTTOMRIGHT => {
                    rect.bottom += height_delta;
                },
                _ => {},
            }

            result = ProcResult::DefWindowProc(wparam);
        },

        WM_MENUCHAR => {
            result = ProcResult::Value((MNC_CLOSE << 16) as isize);
        },

        WM_IME_STARTCOMPOSITION | WM_IME_COMPOSITION | WM_IME_ENDCOMPOSITION => {
            let (allowed, mut state) = {
                let w = userdata.window_state_lock();
                (w.ime_allowed, w.ime_state)
            };
            let source = LazyImeContext::new(window);
            let events = ime_events(msg, lparam, allowed, &mut state, &source);
            drop(source);
            userdata.window_state_lock().ime_state = state;
            for event in events {
                userdata.send_event(Event::WindowEvent {
                    window_id: RootWindowId(WindowId(window)),
                    event: WindowEvent::Ime(event),
                });
            }
            result = if msg == WM_IME_COMPOSITION {
                ProcResult::Value(0)
            } else {
                ProcResult::DefWindowProc(wparam)
            };
        },

        WM_IME_SETCONTEXT => {
            let lparam = lparam & !(ISC_SHOWUICOMPOSITIONWINDOW as isize);
            result = ProcResult::Value(unsafe { DefWindowProcW(window, msg, wparam, lparam) });
        },

        WM_SYSCOMMAND => {
            if wparam == SC_RESTORE as usize {
                let mut w = userdata.window_state_lock();
                w.set_window_flags_in_place(|f| f.set(WindowFlags::MINIMIZED, false));
            }
            if wparam == SC_MINIMIZE as usize {
                let mut w = userdata.window_state_lock();
                w.set_window_flags_in_place(|f| f.set(WindowFlags::MINIMIZED, true));
            }

            if wparam == SC_SCREENSAVE as usize {
                let window_state = userdata.window_state_lock();
                if window_state.fullscreen.is_some() {
                    result = ProcResult::Value(0);
                    return;
                }
            }

            result = ProcResult::DefWindowProc(wparam);
        },

        WM_MOUSEMOVE => {
            use crate::event::WindowEvent::{CursorEntered, CursorLeft, CursorMoved};

            let x = super::get_x_lparam(lparam as u32) as i32;
            let y = super::get_y_lparam(lparam as u32) as i32;
            let position = PhysicalPosition::new(x as f64, y as f64);

            let cursor_moved;
            {
                let mut w = userdata.window_state_lock();
                let mouse_was_inside_window =
                    w.mouse.cursor_flags().contains(CursorFlags::IN_WINDOW);

                match get_pointer_move_kind(window, mouse_was_inside_window, x, y) {
                    PointerMoveKind::Enter => {
                        w.mouse
                            .set_cursor_flags(window, |f| f.set(CursorFlags::IN_WINDOW, true))
                            .ok();

                        drop(w);
                        userdata.send_event(Event::WindowEvent {
                            window_id: RootWindowId(WindowId(window)),
                            event: CursorEntered { device_id: DEVICE_ID },
                        });

                        unsafe {
                            TrackMouseEvent(&mut TRACKMOUSEEVENT {
                                cbSize: mem::size_of::<TRACKMOUSEEVENT>() as u32,
                                dwFlags: TME_LEAVE,
                                hwndTrack: window,
                                dwHoverTime: HOVER_DEFAULT,
                            })
                        };
                    },
                    PointerMoveKind::Leave => {
                        w.mouse
                            .set_cursor_flags(window, |f| f.set(CursorFlags::IN_WINDOW, false))
                            .ok();

                        drop(w);
                        userdata.send_event(Event::WindowEvent {
                            window_id: RootWindowId(WindowId(window)),
                            event: CursorLeft { device_id: DEVICE_ID },
                        });
                    },
                    PointerMoveKind::None => drop(w),
                }

                let mut w = userdata.window_state_lock();
                cursor_moved = w.mouse.last_position != Some(position);
                w.mouse.last_position = Some(position);
            }

            if cursor_moved {
                update_modifiers(window, userdata);

                userdata.send_event(Event::WindowEvent {
                    window_id: RootWindowId(WindowId(window)),
                    event: CursorMoved { device_id: DEVICE_ID, position },
                });
            }

            result = ProcResult::Value(0);
        },

        WM_MOUSELEAVE => {
            use crate::event::WindowEvent::CursorLeft;
            {
                let mut w = userdata.window_state_lock();
                w.mouse.set_cursor_flags(window, |f| f.set(CursorFlags::IN_WINDOW, false)).ok();
            }

            userdata.send_event(Event::WindowEvent {
                window_id: RootWindowId(WindowId(window)),
                event: CursorLeft { device_id: DEVICE_ID },
            });

            result = ProcResult::Value(0);
        },

        WM_MOUSEWHEEL => {
            use crate::event::MouseScrollDelta::LineDelta;

            let value = (wparam >> 16) as i16;
            let value = value as f32 / WHEEL_DELTA as f32;

            update_modifiers(window, userdata);

            userdata.send_event(Event::WindowEvent {
                window_id: RootWindowId(WindowId(window)),
                event: WindowEvent::MouseWheel {
                    device_id: DEVICE_ID,
                    delta: LineDelta(0.0, value),
                    phase: TouchPhase::Moved,
                },
            });

            result = ProcResult::Value(0);
        },

        WM_MOUSEHWHEEL => {
            use crate::event::MouseScrollDelta::LineDelta;

            let value = (wparam >> 16) as i16;
            let value = -value as f32 / WHEEL_DELTA as f32;

            update_modifiers(window, userdata);

            userdata.send_event(Event::WindowEvent {
                window_id: RootWindowId(WindowId(window)),
                event: WindowEvent::MouseWheel {
                    device_id: DEVICE_ID,
                    delta: LineDelta(value, 0.0),
                    phase: TouchPhase::Moved,
                },
            });

            result = ProcResult::Value(0);
        },

        WM_KEYDOWN | WM_SYSKEYDOWN => {
            if msg == WM_SYSKEYDOWN {
                result = ProcResult::DefWindowProc(wparam);
            }
        },

        WM_KEYUP | WM_SYSKEYUP => {
            if msg == WM_SYSKEYUP && unsafe { !GetMenu(window).is_null() } {
                result = ProcResult::DefWindowProc(wparam);
            }
        },

        WM_LBUTTONDOWN => {
            use crate::event::ElementState::Pressed;
            use crate::event::MouseButton::Left;
            use crate::event::WindowEvent::MouseInput;

            unsafe { capture_mouse(window, &mut userdata.window_state_lock()) };

            update_modifiers(window, userdata);

            userdata.send_event(Event::WindowEvent {
                window_id: RootWindowId(WindowId(window)),
                event: MouseInput { device_id: DEVICE_ID, state: Pressed, button: Left },
            });
            result = ProcResult::Value(0);
        },

        WM_LBUTTONUP => {
            use crate::event::ElementState::Released;
            use crate::event::MouseButton::Left;
            use crate::event::WindowEvent::MouseInput;

            unsafe { release_mouse(userdata.window_state_lock()) };

            update_modifiers(window, userdata);

            userdata.send_event(Event::WindowEvent {
                window_id: RootWindowId(WindowId(window)),
                event: MouseInput { device_id: DEVICE_ID, state: Released, button: Left },
            });
            result = ProcResult::Value(0);
        },

        WM_RBUTTONDOWN => {
            use crate::event::ElementState::Pressed;
            use crate::event::MouseButton::Right;
            use crate::event::WindowEvent::MouseInput;

            unsafe { capture_mouse(window, &mut userdata.window_state_lock()) };

            update_modifiers(window, userdata);

            userdata.send_event(Event::WindowEvent {
                window_id: RootWindowId(WindowId(window)),
                event: MouseInput { device_id: DEVICE_ID, state: Pressed, button: Right },
            });
            result = ProcResult::Value(0);
        },

        WM_RBUTTONUP => {
            use crate::event::ElementState::Released;
            use crate::event::MouseButton::Right;
            use crate::event::WindowEvent::MouseInput;

            unsafe { release_mouse(userdata.window_state_lock()) };

            update_modifiers(window, userdata);

            userdata.send_event(Event::WindowEvent {
                window_id: RootWindowId(WindowId(window)),
                event: MouseInput { device_id: DEVICE_ID, state: Released, button: Right },
            });
            result = ProcResult::Value(0);
        },

        WM_MBUTTONDOWN => {
            use crate::event::ElementState::Pressed;
            use crate::event::MouseButton::Middle;
            use crate::event::WindowEvent::MouseInput;

            unsafe { capture_mouse(window, &mut userdata.window_state_lock()) };

            update_modifiers(window, userdata);

            userdata.send_event(Event::WindowEvent {
                window_id: RootWindowId(WindowId(window)),
                event: MouseInput { device_id: DEVICE_ID, state: Pressed, button: Middle },
            });
            result = ProcResult::Value(0);
        },

        WM_MBUTTONUP => {
            use crate::event::ElementState::Released;
            use crate::event::MouseButton::Middle;
            use crate::event::WindowEvent::MouseInput;

            unsafe { release_mouse(userdata.window_state_lock()) };

            update_modifiers(window, userdata);

            userdata.send_event(Event::WindowEvent {
                window_id: RootWindowId(WindowId(window)),
                event: MouseInput { device_id: DEVICE_ID, state: Released, button: Middle },
            });
            result = ProcResult::Value(0);
        },

        WM_XBUTTONDOWN => {
            use crate::event::ElementState::Pressed;
            use crate::event::MouseButton::{Back, Forward, Other};
            use crate::event::WindowEvent::MouseInput;
            let xbutton = super::get_xbutton_wparam(wparam as u32);

            unsafe { capture_mouse(window, &mut userdata.window_state_lock()) };

            update_modifiers(window, userdata);

            userdata.send_event(Event::WindowEvent {
                window_id: RootWindowId(WindowId(window)),
                event: MouseInput {
                    device_id: DEVICE_ID,
                    state: Pressed,
                    button: match xbutton {
                        1 => Back,
                        2 => Forward,
                        _ => Other(xbutton),
                    },
                },
            });
            result = ProcResult::Value(0);
        },

        WM_XBUTTONUP => {
            use crate::event::ElementState::Released;
            use crate::event::MouseButton::{Back, Forward, Other};
            use crate::event::WindowEvent::MouseInput;
            let xbutton = super::get_xbutton_wparam(wparam as u32);

            unsafe { release_mouse(userdata.window_state_lock()) };

            update_modifiers(window, userdata);

            userdata.send_event(Event::WindowEvent {
                window_id: RootWindowId(WindowId(window)),
                event: MouseInput {
                    device_id: DEVICE_ID,
                    state: Released,
                    button: match xbutton {
                        1 => Back,
                        2 => Forward,
                        _ => Other(xbutton),
                    },
                },
            });
            result = ProcResult::Value(0);
        },

        WM_CAPTURECHANGED => {
            // lparam here is a handle to the window which is gaining mouse capture.
            if lparam != super::handle::to_isize(window) {
                userdata.window_state_lock().mouse.capture_count = 0;
            }
            result = ProcResult::Value(0);
        },

        WM_TOUCH => {
            let pcount = super::loword(wparam as u32) as usize;
            let mut inputs = Vec::with_capacity(pcount);
            let htouch = super::handle::from_isize(lparam);
            if unsafe {
                GetTouchInputInfo(
                    htouch,
                    pcount as u32,
                    inputs.as_mut_ptr(),
                    mem::size_of::<TOUCHINPUT>() as i32,
                ) > 0
            } {
                unsafe { inputs.set_len(pcount) };
                for input in &inputs {
                    let mut location = POINT { x: input.x / 100, y: input.y / 100 };

                    if unsafe { ScreenToClient(window, &mut location) } == false.into() {
                        continue;
                    }

                    let x = location.x as f64 + (input.x % 100) as f64 / 100f64;
                    let y = location.y as f64 + (input.y % 100) as f64 / 100f64;
                    let location = PhysicalPosition::new(x, y);
                    userdata.send_event(Event::WindowEvent {
                        window_id: RootWindowId(WindowId(window)),
                        event: WindowEvent::Touch(Touch {
                            phase: if util::has_flag(input.dwFlags, TOUCHEVENTF_DOWN) {
                                TouchPhase::Started
                            } else if util::has_flag(input.dwFlags, TOUCHEVENTF_UP) {
                                TouchPhase::Ended
                            } else if util::has_flag(input.dwFlags, TOUCHEVENTF_MOVE) {
                                TouchPhase::Moved
                            } else {
                                continue;
                            },
                            location,
                            force: None,
                            id: input.dwID as u64,
                            device_id: DEVICE_ID,
                        }),
                    });
                }
            }
            unsafe { CloseTouchInputHandle(htouch) };
            result = ProcResult::Value(0);
        },

        WM_POINTERDOWN | WM_POINTERUPDATE | WM_POINTERUP => {
            if let (
                Some(GetPointerFrameInfoHistory),
                Some(SkipPointerFrameMessages),
                Some(GetPointerDeviceRects),
            ) = (
                *util::GET_POINTER_FRAME_INFO_HISTORY,
                *util::SKIP_POINTER_FRAME_MESSAGES,
                *util::GET_POINTER_DEVICE_RECTS,
            ) {
                let pointer_id = super::loword(wparam as u32) as u32;
                let mut entries_count = 0u32;
                let mut pointers_count = 0u32;
                if unsafe {
                    GetPointerFrameInfoHistory(
                        pointer_id,
                        &mut entries_count,
                        &mut pointers_count,
                        ptr::null_mut(),
                    )
                } == false.into()
                {
                    result = ProcResult::Value(0);
                    return;
                }

                let pointer_info_count = (entries_count * pointers_count) as usize;
                let mut pointer_infos = Vec::with_capacity(pointer_info_count);
                if unsafe {
                    GetPointerFrameInfoHistory(
                        pointer_id,
                        &mut entries_count,
                        &mut pointers_count,
                        pointer_infos.as_mut_ptr(),
                    )
                } == false.into()
                {
                    result = ProcResult::Value(0);
                    return;
                }
                unsafe { pointer_infos.set_len(pointer_info_count) };

                for pointer_info in pointer_infos.iter().rev() {
                    let mut device_rect = mem::MaybeUninit::uninit();
                    let mut display_rect = mem::MaybeUninit::uninit();

                    if unsafe {
                        GetPointerDeviceRects(
                            pointer_info.sourceDevice,
                            device_rect.as_mut_ptr(),
                            display_rect.as_mut_ptr(),
                        )
                    } == false.into()
                    {
                        continue;
                    }

                    let device_rect = unsafe { device_rect.assume_init() };
                    let display_rect = unsafe { display_rect.assume_init() };

                    let himetric_to_pixel_ratio_x = (display_rect.right - display_rect.left) as f64
                        / (device_rect.right - device_rect.left) as f64;
                    let himetric_to_pixel_ratio_y = (display_rect.bottom - display_rect.top) as f64
                        / (device_rect.bottom - device_rect.top) as f64;

                    let x = display_rect.left as f64
                        + pointer_info.ptHimetricLocation.x as f64 * himetric_to_pixel_ratio_x;
                    let y = display_rect.top as f64
                        + pointer_info.ptHimetricLocation.y as f64 * himetric_to_pixel_ratio_y;

                    let mut location = POINT { x: x.floor() as i32, y: y.floor() as i32 };

                    if unsafe { ScreenToClient(window, &mut location) } == false.into() {
                        continue;
                    }

                    let force = match pointer_info.pointerType {
                        PT_TOUCH => {
                            let mut touch_info = mem::MaybeUninit::uninit();
                            util::GET_POINTER_TOUCH_INFO.and_then(|GetPointerTouchInfo| {
                                match unsafe {
                                    GetPointerTouchInfo(
                                        pointer_info.pointerId,
                                        touch_info.as_mut_ptr(),
                                    )
                                } {
                                    0 => None,
                                    _ => normalize_pointer_pressure(unsafe {
                                        touch_info.assume_init().pressure
                                    }),
                                }
                            })
                        },
                        PT_PEN => {
                            let mut pen_info = mem::MaybeUninit::uninit();
                            util::GET_POINTER_PEN_INFO.and_then(|GetPointerPenInfo| {
                                match unsafe {
                                    GetPointerPenInfo(pointer_info.pointerId, pen_info.as_mut_ptr())
                                } {
                                    0 => None,
                                    _ => normalize_pointer_pressure(unsafe {
                                        pen_info.assume_init().pressure
                                    }),
                                }
                            })
                        },
                        _ => None,
                    };

                    let x = location.x as f64 + x.fract();
                    let y = location.y as f64 + y.fract();
                    let location = PhysicalPosition::new(x, y);
                    userdata.send_event(Event::WindowEvent {
                        window_id: RootWindowId(WindowId(window)),
                        event: WindowEvent::Touch(Touch {
                            phase: if util::has_flag(pointer_info.pointerFlags, POINTER_FLAG_DOWN) {
                                TouchPhase::Started
                            } else if util::has_flag(pointer_info.pointerFlags, POINTER_FLAG_UP) {
                                TouchPhase::Ended
                            } else if util::has_flag(pointer_info.pointerFlags, POINTER_FLAG_UPDATE)
                            {
                                TouchPhase::Moved
                            } else {
                                continue;
                            },
                            location,
                            force,
                            id: pointer_info.pointerId as u64,
                            device_id: DEVICE_ID,
                        }),
                    });
                }

                unsafe { SkipPointerFrameMessages(pointer_id) };
            }
            result = ProcResult::Value(0);
        },

        WM_NCACTIVATE => {
            let is_active = wparam != false.into();
            if is_active {
                super::dark_mode::reassert_menu_theme(window);
            }
            let active_focus_changed = userdata.window_state_lock().set_active(is_active);
            if active_focus_changed {
                if is_active {
                    unsafe { gain_active_focus(window, userdata) };
                } else {
                    unsafe { lose_active_focus(window, userdata) };
                }
            }
            result = ProcResult::DefWindowProc(wparam);
        },

        WM_SETFOCUS => {
            let active_focus_changed = userdata.window_state_lock().set_focused(true);
            if active_focus_changed {
                unsafe { gain_active_focus(window, userdata) };
            }
            result = ProcResult::Value(0);
        },

        WM_KILLFOCUS => {
            let active_focus_changed = userdata.window_state_lock().set_focused(false);
            if active_focus_changed {
                unsafe { lose_active_focus(window, userdata) };
            }
            result = ProcResult::Value(0);
        },

        WM_SETCURSOR => {
            let set_cursor_to = {
                let window_state = userdata.window_state_lock();
                let in_client_area = super::loword(lparam as u32) as u32 == HTCLIENT;
                if in_client_area {
                    Some(window_state.mouse.selected_cursor)
                } else {
                    None
                }
            };

            match set_cursor_to {
                Some(cursor_icon) => {
                    let hcursor = unsafe {
                        LoadCursorW(std::ptr::null_mut(), util::to_windows_cursor(cursor_icon))
                    };
                    unsafe { SetCursor(hcursor) };
                    result = ProcResult::Value(0);
                },
                None => result = ProcResult::DefWindowProc(wparam),
            }
        },

        // LOCAL PATCH (aterm): no aterm crate sets a minimum or maximum size on Windows, so the
        // track sizes Windows proposes stand, and the message is answered handled, as upstream
        // answered it with neither set.
        WM_GETMINMAXINFO => {
            result = ProcResult::Value(0);
        },

        WM_DPICHANGED => {
            use crate::event::WindowEvent::ScaleFactorChanged;

            let new_dpi_x = super::loword(wparam as u32) as u32;
            let new_scale_factor = dpi_to_scale_factor(new_dpi_x);
            let old_scale_factor: f64;

            let (allow_resize, window_flags) = {
                let mut window_state = userdata.window_state_lock();
                old_scale_factor = window_state.scale_factor;
                window_state.scale_factor = new_scale_factor;

                if new_scale_factor == old_scale_factor {
                    result = ProcResult::Value(0);
                    return;
                }

                let allow_resize = window_state.fullscreen.is_none()
                    && !window_state.window_flags().contains(WindowFlags::MAXIMIZED);

                (allow_resize, window_state.window_flags)
            };

            let suggested_rect = unsafe { *(lparam as *const RECT) };

            let margin_left: i32;
            let margin_top: i32;
            {
                let adjusted_rect =
                    window_flags.adjust_rect(window, suggested_rect).unwrap_or(suggested_rect);
                margin_left = suggested_rect.left - adjusted_rect.left;
                margin_top = suggested_rect.top - adjusted_rect.top;
            }

            let old_physical_inner_rect = util::WindowArea::Inner
                .get_rect(window)
                .expect("failed to query (old) inner window area");
            let old_physical_inner_size = PhysicalSize::new(
                (old_physical_inner_rect.right - old_physical_inner_rect.left) as u32,
                (old_physical_inner_rect.bottom - old_physical_inner_rect.top) as u32,
            );

            let new_physical_inner_size = match allow_resize {
                true => old_physical_inner_size
                    .to_logical::<f64>(old_scale_factor)
                    .to_physical::<u32>(new_scale_factor),
                false => old_physical_inner_size,
            };

            let new_inner_size = Arc::new(Mutex::new(new_physical_inner_size));
            userdata.send_event(Event::WindowEvent {
                window_id: RootWindowId(WindowId(window)),
                event: ScaleFactorChanged {
                    scale_factor: new_scale_factor,
                    inner_size_writer: InnerSizeWriter::new(Arc::downgrade(&new_inner_size)),
                },
            });

            let new_physical_inner_size = *new_inner_size.lock().unwrap();
            drop(new_inner_size);

            let dragging_window: bool;

            {
                let window_state = userdata.window_state_lock();
                dragging_window =
                    window_state.window_flags().contains(WindowFlags::MARKER_IN_SIZE_MOVE);
                if new_physical_inner_size != old_physical_inner_size {
                    WindowState::set_window_flags(window_state, window, |f| {
                        f.set(WindowFlags::MAXIMIZED, false)
                    });
                }
            }

            let new_outer_rect: RECT;
            {
                let suggested_ul =
                    (suggested_rect.left + margin_left, suggested_rect.top + margin_top);

                let mut conservative_rect = RECT {
                    left: suggested_ul.0,
                    top: suggested_ul.1,
                    right: suggested_ul.0 + new_physical_inner_size.width as i32,
                    bottom: suggested_ul.1 + new_physical_inner_size.height as i32,
                };

                conservative_rect = window_flags
                    .adjust_rect(window, conservative_rect)
                    .unwrap_or(conservative_rect);

                if dragging_window {
                    let bias = {
                        let cursor_pos = {
                            let mut pos = unsafe { mem::zeroed() };
                            unsafe { GetCursorPos(&mut pos) };
                            pos
                        };
                        let suggested_cursor_horizontal_ratio = (cursor_pos.x - suggested_rect.left)
                            as f64
                            / (suggested_rect.right - suggested_rect.left) as f64;

                        (cursor_pos.x
                            - (suggested_cursor_horizontal_ratio
                                * (conservative_rect.right - conservative_rect.left) as f64)
                                as i32)
                            - conservative_rect.left
                    };
                    conservative_rect.left += bias;
                    conservative_rect.right += bias;
                }

                let new_dpi_monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONULL) };
                let conservative_rect_monitor =
                    unsafe { MonitorFromRect(&conservative_rect, MONITOR_DEFAULTTONULL) };
                new_outer_rect = if conservative_rect_monitor == new_dpi_monitor {
                    conservative_rect
                } else {
                    let get_monitor_rect = |monitor| {
                        let mut monitor_info = MONITORINFO {
                            cbSize: mem::size_of::<MONITORINFO>() as _,
                            ..unsafe { mem::zeroed() }
                        };
                        unsafe { GetMonitorInfoW(monitor, &mut monitor_info) };
                        monitor_info.rcMonitor
                    };
                    let wrong_monitor = conservative_rect_monitor;
                    let wrong_monitor_rect = get_monitor_rect(wrong_monitor);
                    let new_monitor_rect = get_monitor_rect(new_dpi_monitor);

                    #[allow(clippy::bool_to_int_with_if)]
                    let delta_nudge_to_dpi_monitor = (
                        if wrong_monitor_rect.left == new_monitor_rect.right {
                            -1
                        } else if wrong_monitor_rect.right == new_monitor_rect.left {
                            1
                        } else {
                            0
                        },
                        if wrong_monitor_rect.bottom == new_monitor_rect.top {
                            1
                        } else if wrong_monitor_rect.top == new_monitor_rect.bottom {
                            -1
                        } else {
                            0
                        },
                    );

                    let abort_after_iterations = new_monitor_rect.right - new_monitor_rect.left
                        + new_monitor_rect.bottom
                        - new_monitor_rect.top;
                    for _ in 0..abort_after_iterations {
                        conservative_rect.left += delta_nudge_to_dpi_monitor.0;
                        conservative_rect.right += delta_nudge_to_dpi_monitor.0;
                        conservative_rect.top += delta_nudge_to_dpi_monitor.1;
                        conservative_rect.bottom += delta_nudge_to_dpi_monitor.1;

                        if unsafe { MonitorFromRect(&conservative_rect, MONITOR_DEFAULTTONULL) }
                            == new_dpi_monitor
                        {
                            break;
                        }
                    }

                    conservative_rect
                };
            }

            unsafe {
                SetWindowPos(
                    window,
                    std::ptr::null_mut(),
                    new_outer_rect.left,
                    new_outer_rect.top,
                    new_outer_rect.right - new_outer_rect.left,
                    new_outer_rect.bottom - new_outer_rect.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                )
            };

            result = ProcResult::Value(0);
        },

        WM_SETTINGCHANGE => {
            use crate::event::WindowEvent::ThemeChanged;

            // LOCAL PATCH (aterm): no window is made with a preferred theme (`with_theme` had no
            // aterm caller, and `set_theme` never recorded one), so every window follows the
            // system's.
            let new_theme = try_theme(window, None);
            let mut window_state = userdata.window_state_lock();

            if window_state.current_theme != new_theme {
                window_state.current_theme = new_theme;
                drop(window_state);
                userdata.send_event(Event::WindowEvent {
                    window_id: RootWindowId(WindowId(window)),
                    event: ThemeChanged(new_theme),
                });
            }
            result = ProcResult::DefWindowProc(wparam);
        },

        _ => {
            if msg == DESTROY_MSG_ID.get() {
                unsafe { DestroyWindow(window) };
                result = ProcResult::Value(0);
            } else if msg == SET_RETAIN_STATE_ON_SIZE_MSG_ID.get() {
                let mut window_state = userdata.window_state_lock();
                window_state.set_window_flags_in_place(|f| {
                    f.set(WindowFlags::MARKER_RETAIN_STATE_ON_SIZE, wparam != 0)
                });
                result = ProcResult::Value(0);
            } else if msg == TASKBAR_CREATED.get() {
                unsafe { add_taskbar_tab(window) };
                result = ProcResult::DefWindowProc(wparam);
            } else {
                result = ProcResult::DefWindowProc(wparam);
            }
        },
    };

    userdata
        .event_loop_runner
        .catch_unwind(callback)
        .unwrap_or_else(|| result = ProcResult::Value(-1));

    match result {
        ProcResult::DefWindowProc(wparam) => unsafe { DefWindowProcW(window, msg, wparam, lparam) },
        ProcResult::Value(val) => val,
    }
}

unsafe extern "system" fn thread_event_target_callback(
    window: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let userdata_ptr =
        unsafe { super::get_window_long(window, GWL_USERDATA) } as *mut ThreadMsgTargetData;
    if userdata_ptr.is_null() {
        return unsafe { DefWindowProcW(window, msg, wparam, lparam) };
    }
    let userdata = unsafe { Box::from_raw(userdata_ptr) };

    if msg != WM_PAINT {
        unsafe { RedrawWindow(window, ptr::null(), std::ptr::null_mut(), RDW_INTERNALPAINT) };
    }

    let mut userdata_removed = false;

    let callback = || match msg {
        WM_NCDESTROY => {
            unsafe { super::set_window_long(window, GWL_USERDATA, 0) };
            userdata_removed = true;
            0
        },
        WM_PAINT => unsafe {
            ValidateRect(window, ptr::null());
            DefWindowProcW(window, msg, wparam, lparam)
        },

        _ if msg == USER_EVENT_MSG_ID.get() => {
            userdata.send_event(Event::UserEvent(UserEventPlaceholder));
            0
        },
        _ if msg == EXEC_MSG_ID.get() => {
            let mut function: ThreadExecFn = unsafe { Box::from_raw(wparam as *mut _) };
            function();
            0
        },
        _ => unsafe { DefWindowProcW(window, msg, wparam, lparam) },
    };

    let result = userdata.event_loop_runner.catch_unwind(callback).unwrap_or(-1);
    if userdata_removed {
        drop(userdata);
    } else {
        Box::leak(userdata);
    }
    result
}

enum PointerMoveKind {
    /// Pointer entered to the window.
    Enter,
    /// Pointer leaved the window client area.
    Leave,
    /// Pointer is inside the window or `GetClientRect` failed.
    None,
}

fn get_pointer_move_kind(
    window: HWND,
    mouse_was_inside_window: bool,
    x: i32,
    y: i32,
) -> PointerMoveKind {
    let rect: RECT = unsafe {
        let mut rect: RECT = mem::zeroed();
        if GetClientRect(window, &mut rect) == false.into() {
            return PointerMoveKind::None;
        }
        rect
    };

    let x = (rect.left..rect.right).contains(&x);
    let y = (rect.top..rect.bottom).contains(&y);

    if !mouse_was_inside_window && x && y {
        PointerMoveKind::Enter
    } else if mouse_was_inside_window && !(x && y) {
        PointerMoveKind::Leave
    } else {
        PointerMoveKind::None
    }
}
