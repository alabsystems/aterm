// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/mod.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::CStr;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::ops::Deref;
use std::os::raw::*;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};
use std::{fmt, ptr, slice, str};

use aterm_x11::proto::{xinput, xproto};
use libc::{setlocale, LC_CTYPE};
use tracing::warn;

use crate::error::{EventLoopError, OsError as RootOsError};
use crate::event::{Event, StartCause, WindowEvent};
use crate::event_loop::{ActiveEventLoop as RootAEL, ControlFlow, EventLoopClosed};
use crate::platform::pump_events::PumpStatus;
use crate::platform_impl::common::poll::{self, Waker};
use crate::platform_impl::common::xkb::Context;
use crate::platform_impl::platform::{min_timeout, WindowId};
use crate::platform_impl::{ActiveEventLoop as PlatformActiveEventLoop, OsError};
use crate::window::WindowAttributes;

#[cfg(test)]
mod activation_tests;
mod atoms;
#[cfg(test)]
mod attention_tests;
#[cfg(all(test, target_os = "linux"))]
mod background_tests;
#[cfg(all(test, target_os = "linux"))]
mod bell_tests;

/// Held by each test that makes windows on the process's one X11 connection (`X11_BACKEND`).
#[cfg(test)]
pub(super) fn shared_display_turn() -> std::sync::MutexGuard<'static, ()> {
    static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    TURN.lock().unwrap_or_else(|e| e.into_inner())
}
#[cfg(test)]
mod compose_tests;
#[cfg(all(test, target_os = "linux"))]
mod cursor_theme_tests;
mod dnd;
#[cfg(all(test, target_os = "linux"))]
mod dnd_tests;
mod event_processor;
#[cfg(all(test, target_os = "linux"))]
mod exposure_tests;
pub mod ffi;
#[cfg(test)]
mod focus_tests;
#[cfg(all(test, target_os = "linux"))]
mod glx_error_tests;
#[cfg(all(test, target_os = "linux"))]
mod gone_tests;
#[cfg(all(test, target_os = "linux"))]
mod held_frame_tests;
mod ime;
#[cfg(test)]
mod ime_spot_tests;
#[cfg(test)]
mod level_tests;
mod monitor;
#[cfg(all(test, target_os = "linux"))]
mod occlusion_tests;
#[cfg(all(test, target_os = "linux"))]
mod place_tests;
#[cfg(all(test, target_os = "linux"))]
mod press_time_tests;
mod startup_notify;
mod sync_request;
#[cfg(all(test, target_os = "linux"))]
mod sync_request_tests;
#[cfg(all(test, target_os = "linux"))]
mod theme_tests;
mod util;
mod window;
mod xdisplay;
mod xsettings;

use atoms::*;
use dnd::{Dnd, DndState};
use event_processor::{EventProcessor, MAX_MOD_REPLAY_LEN};
use ime::{Ime, ImeCreationError, ImeReceiver, ImeRequest, ImeSender};
pub(crate) use monitor::MonitorHandle;
use window::UnownedWindow;
pub(crate) use xdisplay::{XConnection, XError, XNotSupported};

const ALL_DEVICES: u16 = xinput::ALL_DEVICES;
const ALL_MASTER_DEVICES: u16 = xinput::ALL_MASTER_DEVICES;
const ICONIC_STATE: u32 = 3;

/// XKB's `EventType` bits that `XkbSelectEvents` takes (`X11/XKB.h`).
const XKB_NEW_KEYBOARD_NOTIFY_MASK: c_ulong = 1 << 0;
const XKB_MAP_NOTIFY_MASK: c_ulong = 1 << 1;
const XKB_STATE_NOTIFY_MASK: c_ulong = 1 << 2;

struct WakeSender<T> {
    sender: Sender<T>,
    waker: Waker,
}

impl<T> Clone for WakeSender<T> {
    fn clone(&self) -> Self {
        Self { sender: self.sender.clone(), waker: self.waker.clone() }
    }
}

impl<T> WakeSender<T> {
    pub fn send(&self, t: T) -> Result<(), EventLoopClosed<T>> {
        let res = self.sender.send(t).map_err(|e| EventLoopClosed(e.0));
        if res.is_ok() {
            self.waker.wake();
        }
        res
    }
}

struct PeekableReceiver<T> {
    recv: Receiver<T>,
    first: Option<T>,
}

impl<T> PeekableReceiver<T> {
    pub fn from_recv(recv: Receiver<T>) -> Self {
        Self { recv, first: None }
    }

    pub fn has_incoming(&mut self) -> bool {
        if self.first.is_some() {
            return true;
        }

        match self.recv.try_recv() {
            Ok(v) => {
                self.first = Some(v);
                true
            },
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                warn!("Channel was disconnected when checking incoming");
                false
            },
        }
    }

    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        if let Some(first) = self.first.take() {
            return Ok(first);
        }
        self.recv.try_recv()
    }
}

pub struct ActiveEventLoop {
    xconn: Arc<XConnection>,
    wm_delete_window: xproto::Atom,
    net_wm_ping: xproto::Atom,
    ime_sender: ImeSender,
    control_flow: Cell<ControlFlow>,
    exit: Cell<Option<i32>>,
    root: xproto::Window,
    ime: Option<RefCell<Ime>>,
    windows: RefCell<HashMap<WindowId, Weak<UnownedWindow>>>,
    redraw_sender: WakeSender<WindowId>,
}

pub struct EventLoop<T: 'static> {
    loop_running: bool,
    /// Run exit status once the connection ended under it (`end_if_connection_lost`).
    failure: Option<EventLoopError>,
    waker: Waker,
    event_processor: EventProcessor,
    redraw_receiver: PeekableReceiver<WindowId>,
    user_receiver: PeekableReceiver<T>,
    user_sender: Sender<T>,
}

pub struct EventLoopProxy<T: 'static> {
    user_sender: WakeSender<T>,
}

impl<T: 'static> Clone for EventLoopProxy<T> {
    fn clone(&self) -> Self {
        EventLoopProxy { user_sender: self.user_sender.clone() }
    }
}

impl<T: 'static> EventLoop<T> {
    /// The loop on `xconn` ([`startup_failure`], [`EventLoopError::DisplayLost`]).
    pub(crate) fn new(xconn: Arc<XConnection>) -> Result<EventLoop<T>, EventLoopError> {
        let root = xconn.default_root().root;
        let atoms = xconn.atoms();

        let wm_delete_window = atoms[WM_DELETE_WINDOW];
        let net_wm_ping = atoms[_NET_WM_PING];

        let dnd = Dnd::new(Arc::clone(&xconn)).map_err(|e| startup_failure(&xconn, e))?;

        let (ime_sender, ime_receiver) = mpsc::channel();
        let (ime_event_sender, ime_event_receiver) = mpsc::channel();
        unsafe {
            let default_locale = setlocale(LC_CTYPE, ptr::null());
            setlocale(LC_CTYPE, c"".as_ptr());

            let locale_supported = (xconn.xlib.XSupportsLocale)() == 1;
            if !locale_supported {
                let _unsupported_locale = setlocale(LC_CTYPE, ptr::null());
                warn!(
                    "Unsupported locale \"{}\". Restoring default locale \"{}\".",
                    CStr::from_ptr(_unsupported_locale).to_string_lossy(),
                    CStr::from_ptr(default_locale).to_string_lossy()
                );
                setlocale(LC_CTYPE, default_locale);
            }
        }

        let ime = Ime::new(Arc::clone(&xconn), ime_event_sender);
        if let Err(ImeCreationError::OpenFailure(_state)) = ime.as_ref() {
            warn!("Failed to open input method: {_state:#?}");
        } else if let Err(_err) = ime.as_ref() {
            warn!("Failed to set input method destruction callback: {_err:?}");
        }

        let ime = ime.ok().map(RefCell::new);

        let failed = |e: X11Error| startup_failure(&xconn, e);
        let randr_event_offset = xconn.select_xrandr_input(root).map_err(failed)?;

        let extension = |name: &'static [u8], missing: &'static str| {
            xconn
                .xcb_connection()
                .extension(name)
                .map_err(X11Error::from)
                .and_then(|info| info.ok_or(X11Error::MissingExtension(missing)))
                .map_err(failed)
        };
        let xi2ext = extension(xinput::NAME, "XInputExtension")?;
        let xkbext = extension(xdisplay::XKB_EXTENSION_NAME, "XKEYBOARD")?;

        xconn.request(xinput::xi_query_version(xi2ext.major_opcode, 2, 3)).map_err(failed)?;

        xconn.update_cached_wm_info(root);

        let waker = Waker::new().map_err(|e| failed(X11Error::Io(e)))?;

        let (redraw_sender, redraw_channel) = mpsc::channel();

        let (user_sender, user_channel) = mpsc::channel();

        let xkb_context =
            Context::from_x11_xkb(xconn.xcb_connection().as_raw().as_ptr()).map_err(|_| {
                failed(X11Error::MissingExtension("XKEYBOARD (xkb_x11_setup_xkb_extension)"))
            })?;

        let mut xmodmap = util::ModifierKeymap::new();
        xmodmap.reload_from_x_connection(&xconn);

        let window_target = ActiveEventLoop {
            ime,
            root,
            control_flow: Cell::new(ControlFlow::default()),
            exit: Cell::new(None),
            windows: Default::default(),
            ime_sender,
            xconn,
            wm_delete_window,
            net_wm_ping,
            redraw_sender: WakeSender {
                sender: redraw_sender, // not used again so no clone
                waker: waker.clone(),
            },
        };

        let root_window_target =
            RootAEL { p: PlatformActiveEventLoop::X(window_target), _marker: PhantomData };

        let event_processor = EventProcessor {
            target: root_window_target,
            dnd,
            devices: Default::default(),
            randr_event_offset,
            ime_receiver,
            ime_event_receiver,
            xi2ext,
            xfiltered_modifiers: VecDeque::with_capacity(MAX_MOD_REPLAY_LEN),
            xmodmap,
            xkbext,
            xkb_context,
            num_touch: 0,
            held_key_press: None,
            first_touch: None,
            active_window: None,
            modifiers: Default::default(),
            composing: None,
        };

        let xconn = &EventProcessor::window_target(&event_processor.target).xconn;

        xconn
            .select_xinput_events(root, ALL_DEVICES, xinput::event_mask::HIERARCHY)
            .map_err(|e| startup_failure(xconn, e))?
            .ignore_error();

        xconn
            .select_xkb_events(
                ffi::XKB_USE_CORE_KBD, // Use the "core keyboard device"
                XKB_NEW_KEYBOARD_NOTIFY_MASK | XKB_MAP_NOTIFY_MASK | XKB_STATE_NOTIFY_MASK,
            )
            .map_err(|e| startup_failure(xconn, e))?;

        event_processor.init_device(ALL_DEVICES);

        Ok(EventLoop {
            loop_running: false,
            failure: None,
            waker,
            event_processor,
            redraw_receiver: PeekableReceiver::from_recv(redraw_channel),
            user_receiver: PeekableReceiver::from_recv(user_channel),
            user_sender,
        })
    }

    pub fn create_proxy(&self) -> EventLoopProxy<T> {
        EventLoopProxy {
            user_sender: WakeSender { sender: self.user_sender.clone(), waker: self.waker.clone() },
        }
    }

    pub(crate) fn window_target(&self) -> &RootAEL {
        &self.event_processor.target
    }

    pub fn run_on_demand<F>(&mut self, mut event_handler: F) -> Result<(), EventLoopError>
    where
        F: FnMut(Event<T>, &RootAEL),
    {
        let exit = loop {
            match self.pump_events(None, &mut event_handler) {
                PumpStatus::Exit(0) => {
                    break Ok(());
                },
                PumpStatus::Exit(code) => {
                    break Err(self.failure.take().unwrap_or(EventLoopError::ExitFailure(code)));
                },
                _ => {
                    continue;
                },
            }
        };

        let wt = EventProcessor::window_target(&self.event_processor.target);
        let lost = matches!(exit, Err(EventLoopError::DisplayLost(_)));
        if !lost && !wt.x_connection().is_broken() {
            if let Err(x_err) = wt.x_connection().sync_with_server() {
                eprintln!(
                    "aterm-window: an X error was pending when the event loop ended \
                     (an earlier request's, not the run's end): {x_err}"
                );
            }
        }

        exit
    }

    pub fn pump_events<F>(&mut self, timeout: Option<Duration>, mut callback: F) -> PumpStatus
    where
        F: FnMut(Event<T>, &RootAEL),
    {
        if !self.loop_running {
            self.loop_running = true;

            self.single_iteration(&mut callback, StartCause::Init);
        }

        if !self.exiting() && !self.end_if_connection_lost(false) {
            self.poll_events_with_timeout(timeout, &mut callback);
        }
        if let Some(code) = self.exit_code() {
            self.loop_running = false;

            callback(Event::LoopExiting, self.window_target());

            PumpStatus::Exit(code)
        } else {
            PumpStatus::Continue
        }
    }

    /// End the loop on a broken X connection ([`EventLoopError::DisplayLost`]).
    fn end_if_connection_lost(&mut self, hung_up: bool) -> bool {
        let wt = EventProcessor::window_target(&self.event_processor.target);
        let Some(end) = wt.x_connection().connection_end(hung_up) else {
            return false;
        };
        if self.exit_code().is_none() {
            self.failure = Some(connection_ended(end));
            self.set_exit_code(1);
        }
        true
    }

    fn has_pending(&mut self) -> bool {
        self.event_processor.poll()
            || self.user_receiver.has_incoming()
            || self.redraw_receiver.has_incoming()
    }

    pub fn poll_events_with_timeout<F>(&mut self, mut timeout: Option<Duration>, mut callback: F)
    where
        F: FnMut(Event<T>, &RootAEL),
    {
        let start = Instant::now();

        let has_pending = self.has_pending();
        if self.end_if_connection_lost(false) {
            return;
        }

        timeout = if has_pending {
            Some(Duration::ZERO)
        } else {
            let control_flow_timeout = match self.control_flow() {
                ControlFlow::Wait => None,
                ControlFlow::Poll => Some(Duration::ZERO),
                ControlFlow::WaitUntil(wait_deadline) => {
                    Some(wait_deadline.saturating_duration_since(start))
                },
            };

            min_timeout(control_flow_timeout, timeout)
        };
        let wait = min_timeout(
            timeout,
            self.sync_deadline().map(|deadline| deadline.saturating_duration_since(start)),
        );

        let x11_fd = {
            let wt = EventProcessor::window_target(&self.event_processor.target);
            wt.xconn.xcb_connection().file_descriptor()
        };
        let mut fds = [poll::readable(x11_fd), poll::readable(self.waker.as_raw_fd())];
        if let Err(error) = poll::wait(&mut fds, wait) {
            tracing::error!("Failed to poll for events: {error:?}");
            let exit_code = error.raw_os_error().unwrap_or(1);
            if self.exit_code().is_none() {
                let failure = Arc::new(X11Error::Io(error));
                self.failure = Some(EventLoopError::Os(os_error!(OsError::XError(failure))));
                self.set_exit_code(exit_code);
            }
            return;
        }
        if poll::ready(&fds[1]) {
            self.waker.drain();
        }
        let hung_up = fds[0].revents & (libc::POLLHUP | libc::POLLERR) != 0;
        if self.end_if_connection_lost(hung_up) {
            return;
        }

        let cause = match self.control_flow() {
            ControlFlow::Poll => StartCause::Poll,
            ControlFlow::Wait => StartCause::WaitCancelled { start, requested_resume: None },
            ControlFlow::WaitUntil(deadline) => {
                if Instant::now() < deadline {
                    StartCause::WaitCancelled { start, requested_resume: Some(deadline) }
                } else {
                    StartCause::ResumeTimeReached { start, requested_resume: deadline }
                }
            },
        };

        if !self.has_pending()
            && !matches!(&cause, StartCause::ResumeTimeReached { .. } | StartCause::Poll)
            && timeout.is_none()
        {
            self.end_turn_of_sync_requests();
            return;
        }

        self.single_iteration(&mut callback, cause);
    }

    fn single_iteration<F>(&mut self, callback: &mut F, cause: StartCause)
    where
        F: FnMut(Event<T>, &RootAEL),
    {
        callback(Event::NewEvents(cause), &self.event_processor.target);

        if cause == StartCause::Init {
            callback(Event::Resumed, &self.event_processor.target);
        }

        self.drain_events(callback);
        if self.end_if_connection_lost(false) {
            return;
        }

        {
            while let Ok(event) = self.user_receiver.try_recv() {
                callback(Event::UserEvent(event), &self.event_processor.target);
            }
        }

        {
            let mut windows = HashSet::new();

            while let Ok(window_id) = self.redraw_receiver.try_recv() {
                windows.insert(window_id);
            }

            for window_id in windows {
                let exposed = self
                    .event_processor
                    .with_window(u64::from(window_id) as xproto::Window, |w| w.take_exposed())
                    .unwrap_or(false);
                let window_id = crate::window::WindowId(window_id);
                let event = Event::WindowEvent { window_id, event: WindowEvent::RedrawRequested };
                let target = &self.event_processor.target;
                if exposed {
                    crate::platform::x11::dispatch_exposure(|| callback(event, target));
                } else {
                    callback(event, target);
                }
            }
        }

        {
            callback(Event::AboutToWait, &self.event_processor.target);
        }

        self.end_turn_of_sync_requests();
    }

    /// A turn ended, or the loop woke for a deadline.
    fn end_turn_of_sync_requests(&mut self) {
        if self.connection_broken() {
            return;
        }
        let now = Instant::now();
        let mut redraw_queued = None;
        let EventLoop { event_processor, redraw_receiver, .. } = self;
        let wt = EventProcessor::window_target(&event_processor.target);
        for window in wt.windows.borrow().values().filter_map(Weak::upgrade) {
            window.sync_turn_ended(now, || {
                *redraw_queued.get_or_insert_with(|| redraw_receiver.has_incoming())
            });
        }
    }

    /// Earliest deadline of pending resize sync requests (`sync_request.rs`).
    fn sync_deadline(&self) -> Option<Instant> {
        let wt = EventProcessor::window_target(&self.event_processor.target);
        let windows = wt.windows.borrow();
        windows.values().filter_map(Weak::upgrade).filter_map(|window| window.sync_deadline()).min()
    }

    fn drain_events<F>(&mut self, callback: &mut F)
    where
        F: FnMut(Event<T>, &RootAEL),
    {
        let mut xev = MaybeUninit::uninit();

        while !self.connection_broken()
            && unsafe { self.event_processor.poll_one_event(xev.as_mut_ptr()) }
        {
            let mut xev = unsafe { xev.assume_init() };
            self.event_processor.process_event(&mut xev, |window_target, event| {
                if let Event::WindowEvent {
                    window_id: crate::window::WindowId(wid),
                    event: WindowEvent::RedrawRequested,
                } = event
                {
                    let window_target = EventProcessor::window_target(window_target);
                    window_target.redraw_sender.send(wid).unwrap();
                } else {
                    callback(event, window_target);
                }
            });
        }
    }

    /// Whether the connection to the X server is broken ([`XConnection::is_broken`]).
    fn connection_broken(&self) -> bool {
        EventProcessor::window_target(&self.event_processor.target).x_connection().is_broken()
    }

    fn control_flow(&self) -> ControlFlow {
        let window_target = EventProcessor::window_target(&self.event_processor.target);
        window_target.control_flow()
    }

    fn exiting(&self) -> bool {
        let window_target = EventProcessor::window_target(&self.event_processor.target);
        window_target.exiting()
    }

    fn set_exit_code(&self, code: i32) {
        let window_target = EventProcessor::window_target(&self.event_processor.target);
        window_target.set_exit_code(code);
    }

    fn exit_code(&self) -> Option<i32> {
        let window_target = EventProcessor::window_target(&self.event_processor.target);
        window_target.exit_code()
    }
}

/// Error returned once the X connection ended ([`EventLoopError::DisplayLost`]).
fn connection_ended(end: xdisplay::ConnectionEnd) -> EventLoopError {
    match end {
        xdisplay::ConnectionEnd::Transport(why) => EventLoopError::DisplayLost(why),
        xdisplay::ConnectionEnd::Closed(error) => {
            EventLoopError::Os(os_error!(OsError::XError(Arc::new(X11Error::X11(error)))))
        },
    }
}

/// Error returned when a startup request fails ([`connection_ended`]).
fn startup_failure(xconn: &XConnection, error: impl Into<X11Error>) -> EventLoopError {
    match xconn.connection_end(false) {
        Some(end) => connection_ended(end),
        None => EventLoopError::Os(os_error!(OsError::XError(Arc::new(error.into())))),
    }
}

impl ActiveEventLoop {
    /// Returns the `XConnection` of this events loop.
    #[inline]
    pub(crate) fn x_connection(&self) -> &Arc<XConnection> {
        &self.xconn
    }

    pub fn available_monitors(&self) -> impl Iterator<Item = MonitorHandle> {
        self.xconn.available_monitors().into_iter().flatten()
    }

    pub fn primary_monitor(&self) -> Option<MonitorHandle> {
        self.xconn.primary_monitor().ok()
    }

    #[cfg(feature = "rwh_06")]
    pub fn raw_display_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
        let display_handle = rwh_06::XlibDisplayHandle::new(
            // SAFETY: Display will never be null.
            Some(
                std::ptr::NonNull::new(self.xconn.display as *mut _)
                    .expect("X11 display should never be null"),
            ),
            self.xconn.default_screen_index() as c_int,
        );
        Ok(display_handle.into())
    }

    pub(crate) fn set_control_flow(&self, control_flow: ControlFlow) {
        self.control_flow.set(control_flow)
    }

    pub(crate) fn control_flow(&self) -> ControlFlow {
        self.control_flow.get()
    }

    pub(crate) fn exit(&self) {
        self.exit.set(Some(0))
    }

    pub(crate) fn exiting(&self) -> bool {
        self.exit.get().is_some()
    }

    pub(crate) fn set_exit_code(&self, code: i32) {
        self.exit.set(Some(code))
    }

    pub(crate) fn exit_code(&self) -> Option<i32> {
        self.exit.get()
    }
}

impl<T: 'static> EventLoopProxy<T> {
    pub fn send_event(&self, event: T) -> Result<(), EventLoopClosed<T>> {
        self.user_sender.send(event).map_err(|e| EventLoopClosed(e.0))
    }
}

struct DeviceInfo<'a> {
    xconn: &'a XConnection,
    info: *const ffi::XIDeviceInfo,
    count: usize,
}

impl<'a> DeviceInfo<'a> {
    fn get(xconn: &'a XConnection, device: c_int) -> Option<Self> {
        unsafe {
            let mut count = 0;
            let info = (xconn.xinput2.XIQueryDevice)(xconn.display, device, &mut count);
            xconn.check_errors().ok()?;

            if info.is_null() || count == 0 {
                None
            } else {
                Some(DeviceInfo { xconn, info, count: count as usize })
            }
        }
    }
}

impl Drop for DeviceInfo<'_> {
    fn drop(&mut self) {
        assert!(!self.info.is_null());
        unsafe { (self.xconn.xinput2.XIFreeDeviceInfo)(self.info as *mut _) };
    }
}

impl Deref for DeviceInfo<'_> {
    type Target = [ffi::XIDeviceInfo];

    fn deref(&self) -> &Self::Target {
        unsafe { slice::from_raw_parts(self.info, self.count) }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId(xinput::DeviceId);

impl DeviceId {
    #[allow(unused)]
    pub const fn dummy() -> Self {
        DeviceId(0)
    }
}

pub(crate) struct Window(Arc<UnownedWindow>);

impl Deref for Window {
    type Target = UnownedWindow;

    #[inline]
    fn deref(&self) -> &UnownedWindow {
        &self.0
    }
}

impl Window {
    pub(crate) fn new(
        event_loop: &ActiveEventLoop,
        attribs: WindowAttributes,
    ) -> Result<Self, RootOsError> {
        let window = Arc::new(UnownedWindow::new(event_loop, attribs)?);
        event_loop.windows.borrow_mut().insert(window.id(), Arc::downgrade(&window));
        Ok(Window(window))
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let window = self.deref();
        let xconn = &window.xconn;

        if let Ok(c) = xconn.send_void(xproto::destroy_window(window.id().0 as xproto::Window)) {
            c.ignore_error();
        }
    }
}

/// Generic sum error type for X11 errors.
#[derive(Debug)]
pub enum X11Error {
    /// An error from the Xlib library.
    Xlib(XError),

    /// A request, its reply or the connection under Xlib failed.
    X11(aterm_x11::Error),

    /// Got `null` from an Xlib function without a reason.
    UnexpectedNull(&'static str),

    /// An extension that we rely on is not available.
    MissingExtension(&'static str),

    /// Unable to parse xsettings.
    XsettingsParse(xsettings::ParserError),

    /// Failed to get property.
    GetProperty(util::GetPropertyError),

    /// A system call the event loop made failed (its poll).
    Io(std::io::Error),
}

impl fmt::Display for X11Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            X11Error::Xlib(e) => write!(f, "Xlib error: {e}"),
            X11Error::GetProperty(e) => write!(f, "Failed to get X property {e}"),
            X11Error::X11(e) => write!(f, "X11 error: {e}"),
            X11Error::UnexpectedNull(s) => write!(f, "Xlib function returned null: {s}"),
            X11Error::MissingExtension(s) => write!(f, "Missing X11 extension: {s}"),
            X11Error::XsettingsParse(err) => {
                write!(f, "Failed to parse xsettings: {err:?}")
            },
            X11Error::Io(e) => write!(f, "IO error: {e}"),
        }
    }
}

impl std::error::Error for X11Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            X11Error::Xlib(e) => Some(e),
            X11Error::X11(e) => Some(e),
            X11Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<XError> for X11Error {
    fn from(e: XError) -> Self {
        X11Error::Xlib(e)
    }
}

impl From<aterm_x11::Error> for X11Error {
    fn from(e: aterm_x11::Error) -> Self {
        X11Error::X11(e)
    }
}

impl From<ime::ImeContextCreationError> for X11Error {
    fn from(value: ime::ImeContextCreationError) -> Self {
        match value {
            ime::ImeContextCreationError::XError(e) => e.into(),
            ime::ImeContextCreationError::Null => Self::UnexpectedNull("XOpenIM"),
        }
    }
}

impl From<xsettings::ParserError> for X11Error {
    fn from(value: xsettings::ParserError) -> Self {
        Self::XsettingsParse(value)
    }
}

impl From<util::GetPropertyError> for X11Error {
    fn from(value: util::GetPropertyError) -> Self {
        Self::GetProperty(value)
    }
}

/// Type alias for a void cookie.
type VoidCookie<'a> = aterm_x11::VoidCookie<'a>;

/// x11rb's name for dropping a void request's cookie.
trait IgnoreError {
    fn ignore_error(self);
}

impl IgnoreError for VoidCookie<'_> {
    fn ignore_error(self) {}
}

impl XConnection {
    /// A request the backend made for the application failed (`what`, the error).
    pub(crate) fn report(&self, what: &str, error: impl fmt::Display) {
        if !self.is_broken() {
            eprintln!("aterm-window: {what}: {error}");
        }
    }
}

/// Extension trait for `Result<VoidCookie, E>`.
trait CookieResultExt {
    /// Ignore the reply ([`IgnoreError::ignore_error`]) or [`XConnection::report`] failure.
    fn ignore_error_or_report(self, xconn: &XConnection, what: &str);
}

impl<E: fmt::Display> CookieResultExt for Result<VoidCookie<'_>, E> {
    fn ignore_error_or_report(self, xconn: &XConnection, what: &str) {
        match self {
            Ok(cookie) => cookie.ignore_error(),
            Err(error) => xconn.report(what, error),
        }
    }
}

/// Extension trait for a request's `Result`.
trait ReportExt<T> {
    /// The value, or `None` once the failure is [`XConnection::report`]ed as `what`.
    fn or_report(self, xconn: &XConnection, what: &str) -> Option<T>;
}

impl<T, E: fmt::Display> ReportExt<T> for Result<T, E> {
    fn or_report(self, xconn: &XConnection, what: &str) -> Option<T> {
        self.map_err(|error| xconn.report(what, error)).ok()
    }
}

fn mkwid(w: xproto::Window) -> crate::window::WindowId {
    crate::window::WindowId(crate::platform_impl::platform::WindowId(w as _))
}
fn mkdid(w: xinput::DeviceId) -> crate::event::DeviceId {
    crate::event::DeviceId(crate::platform_impl::DeviceId::X(DeviceId(w)))
}

#[derive(Debug)]
pub struct Device {
    _name: String,
    scroll_axes: Vec<(i32, ScrollAxis)>,
    attachment: c_int,
}

#[derive(Debug, Copy, Clone)]
struct ScrollAxis {
    increment: f64,
    orientation: ScrollOrientation,
    position: f64,
}

#[derive(Debug, Copy, Clone)]
enum ScrollOrientation {
    Vertical,
    Horizontal,
}

impl Device {
    fn new(info: &ffi::XIDeviceInfo) -> Self {
        let name = unsafe { CStr::from_ptr(info.name).to_string_lossy() };
        let mut scroll_axes = Vec::new();

        if Device::physical_device(info) {
            for &class_ptr in Device::classes(info) {
                let ty = unsafe { (*class_ptr).type_ };
                if ty == ffi::xi::SCROLL_CLASS {
                    let info = unsafe { &*(class_ptr as *const ffi::XIScrollClassInfo) };
                    scroll_axes.push((info.number, ScrollAxis {
                        increment: info.increment,
                        orientation: match info.scroll_type {
                            ffi::xi::SCROLL_TYPE_HORIZONTAL => ScrollOrientation::Horizontal,
                            ffi::xi::SCROLL_TYPE_VERTICAL => ScrollOrientation::Vertical,
                            _ => unreachable!(),
                        },
                        position: 0.0,
                    }));
                }
            }
        }

        let mut device =
            Device { _name: name.into_owned(), scroll_axes, attachment: info.attachment };
        device.reset_scroll_position(info);
        device
    }

    fn reset_scroll_position(&mut self, info: &ffi::XIDeviceInfo) {
        if Device::physical_device(info) {
            for &class_ptr in Device::classes(info) {
                let ty = unsafe { (*class_ptr).type_ };
                if ty == ffi::xi::VALUATOR_CLASS {
                    let info = unsafe { &*(class_ptr as *const ffi::XIValuatorClassInfo) };
                    if let Some(&mut (_, ref mut axis)) =
                        self.scroll_axes.iter_mut().find(|&&mut (axis, _)| axis == info.number)
                    {
                        axis.position = info.value;
                    }
                }
            }
        }
    }

    #[inline]
    fn physical_device(info: &ffi::XIDeviceInfo) -> bool {
        info.use_ == ffi::xi::SLAVE_KEYBOARD
            || info.use_ == ffi::xi::SLAVE_POINTER
            || info.use_ == ffi::xi::FLOATING_SLAVE
    }

    #[inline]
    fn classes(info: &ffi::XIDeviceInfo) -> &[*const ffi::XIAnyClassInfo] {
        unsafe {
            slice::from_raw_parts(
                info.classes as *const *const ffi::XIAnyClassInfo,
                info.num_classes as usize,
            )
        }
    }
}

/// Convert the raw X11 representation for a 32-bit floating point to a double.
#[inline]
fn xinput_fp1616_to_float(fp: xinput::Fp1616) -> f64 {
    (fp as f64) / ((1 << 16) as f64)
}
