// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/wayland/event_loop/mod.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Modified by the aterm project in 2026; see the repository NOTICE.
//!
//! The event-loop routines.
//!
//! LOCAL PATCH (aterm), 2026-10-17 (docs/AUDIT-third-party-2026-10-15.md, LW.7 with winit-core
//! WC.8): the loop waits in `common::poll` (one `poll(2)` over the display's socket, the loop's
//! eventfd waker, the clipboard's and any drop being read) and reads the display through
//! libwayland's `prepare_read` / `read_events` / `dispatch_pending` on its own queue, where
//! upstream ran a calloop loop with calloop-wayland-source. User events and clipboard requests
//! come over channels that wake the poll, each with a waker of its own: a user event's wake is
//! news for the application (`dispatched_events`), and a clipboard request is answered and
//! flushed within the turn that woke for it, as calloop's channel source answered it, with no
//! `NewEvents`/`AboutToWait` turn of its own. Key repeat is a deadline the poll waits for. A
//! display call that fails ends the loop with `EventLoopError::DisplayLost` when the
//! compositor's connection died (`wire_failure`), else with the call's own error. CUT:
//! the `pump_events` wake-up thread, which existed to wake calloop for an embedder polling the
//! loop's descriptor (no aterm crate embeds the loop; `AsFd` left with calloop), and the
//! `AsFd`/`AsRawFd` impls.

use std::cell::{Cell, RefCell};
use std::marker::PhantomData;
use std::mem;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::cursor::OnlyCursorImage;
use crate::dpi::LogicalSize;
use crate::error::{EventLoopError, OsError as RootOsError};
use crate::event::{Event, InnerSizeWriter, StartCause, WindowEvent};
use crate::event_loop::{ActiveEventLoop as RootActiveEventLoop, ControlFlow, DeviceEvents};
use crate::platform::pump_events::PumpStatus;
use crate::platform_impl::common::poll::{self, Waker};
use crate::platform_impl::platform::min_timeout;
use crate::platform_impl::{
    ActiveEventLoop as PlatformActiveEventLoop, OsError, PlatformCustomCursor,
};
use crate::window::{CustomCursor as RootCustomCursor, CustomCursorSource};

mod proxy;
pub mod sink;

pub use proxy::EventLoopProxy;
use sink::EventSink;

use super::clipboard::{ClipboardRequest, WaylandClipboard};
use super::state::{Connection, WindowCompositorUpdate, WinitState};
use super::window::state::FrameCallbackState;
use super::{logical_to_physical_rounded, WaylandError, WindowId};

/// The Wayland event loop.
pub struct EventLoop<T: 'static> {
    /// Has `run` or `run_on_demand` been called or a call to `pump_events` that starts the loop
    loop_running: bool,

    buffer_sink: EventSink,
    compositor_updates: Vec<WindowCompositorUpdate>,
    window_ids: Vec<WindowId>,

    /// Sender of user events.
    user_events_sender: Sender<T>,

    /// The user events, as they arrive.
    user_events_receiver: Receiver<T>,

    /// Pending events from the user, taken from the channel by the last wait.
    pending_user_events: Vec<T>,

    /// The clipboard handle (see `clipboard.rs`); `clipboard()` clones it out.
    clipboard: WaylandClipboard,

    /// The clipboard's requests: any thread posts, the loop thread (which owns the seats,
    /// their serials and the data devices) answers.
    clipboard_requests: Receiver<ClipboardRequest>,

    /// Wakes the loop for a clipboard request: not news for the application, so its wake
    /// leaves `dispatched_events` alone.
    clipboard_waker: Waker,

    /// Whether the last flush left requests queued (the socket was full): the next wait
    /// also waits for the socket to take more.
    flush_pending: bool,

    /// Whether the last wait saw the display's socket hang up or fail: a display call that
    /// fails after it is the compositor's death ([`wire_failure`]).
    hung_up: bool,

    /// What the run ends with in place of `ExitFailure` once a display call failed.
    failure: Option<EventLoopError>,

    /// The connection to the Wayland server.
    connection: Arc<Connection>,

    /// Event loop window target.
    window_target: RootActiveEventLoop,
}

impl<T: 'static> EventLoop<T> {
    pub fn new() -> Result<EventLoop<T>, EventLoopError> {
        let wire_error = |error: aterm_wayland::proxy::Error| os_error!(OsError::from(error));

        let connection = Arc::new(Connection::connect().map_err(wire_error)?);

        let mut winit_state =
            WinitState::new(Arc::clone(&connection)).map_err(|error| os_error!(error))?;

        // NOTE: do a roundtrip after binding the globals to prevent potential
        // races with the server.
        let result = connection.queue.roundtrip();
        winit_state.drain();
        result.map_err(wire_error)?;

        // An event's loop awakener to wake up for window events from winit's windows.
        let waker =
            Waker::new().map_err(|error| os_error!(OsError::from(WaylandError::Io(error))))?;

        // Setup the user proxy.
        let (user_events_sender, user_events_receiver) = mpsc::channel();

        // The clipboard's request channel: any thread posts, the loop thread (which owns the
        // seats, their serials and the data devices) answers.
        let (clipboard_sender, clipboard_requests) = mpsc::channel();
        let clipboard_waker =
            Waker::new().map_err(|error| os_error!(OsError::from(WaylandError::Io(error))))?;
        let clipboard = WaylandClipboard::new(
            clipboard_sender,
            clipboard_waker.clone(),
            winit_state.clipboard.shared(),
            std::thread::current().id(),
        );

        let window_target = ActiveEventLoop {
            waker,
            control_flow: Cell::new(ControlFlow::default()),
            exit: Cell::new(None),
            state: RefCell::new(winit_state),
            connection: Arc::clone(&connection),
        };

        let event_loop = Self {
            loop_running: false,
            compositor_updates: Vec::new(),
            buffer_sink: EventSink::default(),
            window_ids: Vec::new(),
            user_events_sender,
            user_events_receiver,
            pending_user_events: Vec::new(),
            clipboard,
            clipboard_requests,
            clipboard_waker,
            flush_pending: false,
            hung_up: false,
            failure: None,
            connection,
            window_target: RootActiveEventLoop {
                p: PlatformActiveEventLoop::Wayland(window_target),
                _marker: PhantomData,
            },
        };

        Ok(event_loop)
    }

    /// A clone of the clipboard handle (see `clipboard.rs`).
    pub fn clipboard(&self) -> WaylandClipboard {
        self.clipboard.clone()
    }

    pub fn run_on_demand<F>(&mut self, mut event_handler: F) -> Result<(), EventLoopError>
    where
        F: FnMut(Event<T>, &RootActiveEventLoop),
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

        // Applications aren't allowed to carry windows between separate
        // `run_on_demand` calls but if they have only just dropped their
        // windows we need to make sure those last requests are sent to the
        // compositor.
        let _ = self.roundtrip().map_err(EventLoopError::Os);

        exit
    }

    pub fn pump_events<F>(&mut self, timeout: Option<Duration>, mut callback: F) -> PumpStatus
    where
        F: FnMut(Event<T>, &RootActiveEventLoop),
    {
        if !self.loop_running {
            self.loop_running = true;

            // Run the initial loop iteration.
            self.single_iteration(&mut callback, StartCause::Init);
        }

        // Consider the possibility that the `StartCause::Init` iteration could
        // request to Exit.
        if !self.exiting() {
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

    pub fn poll_events_with_timeout<F>(&mut self, mut timeout: Option<Duration>, mut callback: F)
    where
        F: FnMut(Event<T>, &RootActiveEventLoop),
    {
        let cause = loop {
            let start = Instant::now();

            timeout = {
                let control_flow_timeout = match self.control_flow() {
                    ControlFlow::Wait => None,
                    ControlFlow::Poll => Some(Duration::ZERO),
                    ControlFlow::WaitUntil(wait_deadline) => {
                        Some(wait_deadline.saturating_duration_since(start))
                    },
                };
                min_timeout(control_flow_timeout, timeout)
            };

            // Checking for flush error is essential to perform an exit with error, since
            // once we have a protocol error, we could get stuck retrying...
            if let Err((failure, code)) = self.flush() {
                self.fail(failure, code);
                return;
            }

            if let Err((failure, code)) = self.loop_dispatch(timeout) {
                // NOTE We exit on errors from dispatches, since if we've got protocol error
                // libwayland-client will inform us anyway, but crashing downstream is
                // not really an option. Instead we end the loop with the failure
                // (`wire_failure`): the display's loss when the compositor died, else
                // the error itself; the exit code is the error's errno, or 1.
                self.fail(failure, code);
                return;
            }

            // NB: `StartCause::Init` is handled as a special case and doesn't need
            // to be considered here
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

            // Reduce spurious wake-ups.
            let dispatched_events = self.with_state(|state| state.dispatched_events);
            if matches!(cause, StartCause::WaitCancelled { .. })
                && !dispatched_events
                && timeout.is_none()
            {
                continue;
            }

            break cause;
        };

        self.single_iteration(&mut callback, cause);
    }

    fn single_iteration<F>(&mut self, callback: &mut F, cause: StartCause)
    where
        F: FnMut(Event<T>, &RootActiveEventLoop),
    {
        // NOTE currently just indented to simplify the diff

        // We retain these grow-only scratch buffers as part of the EventLoop
        // for the sake of avoiding lots of reallocs. We take them here to avoid
        // trying to mutably borrow `self` more than once and we swap them back
        // when finished.
        let mut compositor_updates = std::mem::take(&mut self.compositor_updates);
        let mut buffer_sink = std::mem::take(&mut self.buffer_sink);
        let mut window_ids = std::mem::take(&mut self.window_ids);

        callback(Event::NewEvents(cause), &self.window_target);

        // NB: For consistency all platforms must emit a 'resumed' event even though Wayland
        // applications don't themselves have a formal suspend/resume lifecycle.
        if cause == StartCause::Init {
            callback(Event::Resumed, &self.window_target);
        }

        // Handle pending user events. We don't need back buffer, since we can't dispatch
        // user events indirectly via callback to the user.
        for user_event in std::mem::take(&mut self.pending_user_events) {
            callback(Event::UserEvent(user_event), &self.window_target);
        }

        // Drain the pending compositor updates.
        self.with_state(|state| compositor_updates.append(&mut state.window_compositor_updates));

        for mut compositor_update in compositor_updates.drain(..) {
            let window_id = compositor_update.window_id;
            if compositor_update.scale_changed {
                let Some((physical_size, scale_factor)) = self.with_state(|state| {
                    let windows = state.windows.get_mut();
                    let window = windows.get(&window_id)?.lock().unwrap();
                    let scale_factor = window.scale_factor();
                    let size = logical_to_physical_rounded(window.inner_size(), scale_factor);
                    Some((size, scale_factor))
                }) else {
                    continue;
                };

                // Stash the old window size.
                let old_physical_size = physical_size;

                let new_inner_size = Arc::new(Mutex::new(physical_size));
                callback(
                    Event::WindowEvent {
                        window_id: crate::window::WindowId(window_id),
                        event: WindowEvent::ScaleFactorChanged {
                            scale_factor,
                            inner_size_writer: InnerSizeWriter::new(Arc::downgrade(
                                &new_inner_size,
                            )),
                        },
                    },
                    &self.window_target,
                );

                let physical_size = *new_inner_size.lock().unwrap();
                drop(new_inner_size);

                // Resize the window when user altered the size.
                if old_physical_size != physical_size {
                    self.with_state(|state| {
                        let windows = state.windows.get_mut();
                        if let Some(window) = windows.get(&window_id) {
                            let mut window = window.lock().unwrap();

                            let new_logical_size: LogicalSize<f64> =
                                physical_size.to_logical(scale_factor);
                            window.request_inner_size(new_logical_size.into());
                        }
                    });

                    // Make it queue resize.
                    compositor_update.resized = true;
                }
            }

            // NOTE: Rescale changed the physical size which winit operates in, thus we should
            // resize.
            if compositor_update.resized || compositor_update.scale_changed {
                let Some(physical_size) = self.with_state(|state| {
                    let windows = state.windows.get_mut();
                    let window = windows.get(&window_id)?.lock().unwrap();

                    // LOCAL PATCH (aterm), LW.5: the client-side frame this drew first (its
                    // subsurfaces had to be ready before the app's commit inside `Resized`,
                    // or mutter clipped the stored geometry to the content) is gone; the
                    // application's header is part of the surface it presents.
                    let scale_factor = window.scale_factor();
                    let size = logical_to_physical_rounded(window.inner_size(), scale_factor);

                    // Mark the window as needed a redraw.
                    if let Some(requests) = state.window_requests.get_mut().get_mut(&window_id) {
                        requests.redraw_requested.store(true, Ordering::Relaxed);
                    }

                    Some(size)
                }) else {
                    continue;
                };

                callback(
                    Event::WindowEvent {
                        window_id: crate::window::WindowId(window_id),
                        event: WindowEvent::Resized(physical_size),
                    },
                    &self.window_target,
                );
            }

            if compositor_update.close_window {
                callback(
                    Event::WindowEvent {
                        window_id: crate::window::WindowId(window_id),
                        event: WindowEvent::CloseRequested,
                    },
                    &self.window_target,
                );
            }
        }

        // Push the events directly from the window.
        self.with_state(|state| {
            buffer_sink.append(&mut state.window_events_sink.lock().unwrap());
        });
        for event in buffer_sink.drain() {
            let event = event.map_nonuser_event().unwrap();
            callback(event, &self.window_target);
        }

        // Handle non-synthetic events.
        self.with_state(|state| {
            buffer_sink.append(&mut state.events_sink);
        });
        for event in buffer_sink.drain() {
            let event = event.map_nonuser_event().unwrap();
            callback(event, &self.window_target);
        }

        // Collect the window ids
        self.with_state(|state| {
            window_ids.extend(state.window_requests.get_mut().keys());
        });

        for window_id in window_ids.iter() {
            let event = self.with_state(|state| {
                let window_requests = state.window_requests.get_mut();
                if window_requests.get(window_id).is_some_and(|requests| requests.take_closed()) {
                    mem::drop(window_requests.remove(window_id));
                    mem::drop(state.windows.get_mut().remove(window_id));
                    mem::drop(state.surface_scales.remove(window_id));
                    return Some(WindowEvent::Destroyed);
                }

                let mut window = state.windows.get_mut().get_mut(window_id)?.lock().unwrap();

                if window.frame_callback_state() == FrameCallbackState::Requested {
                    return None;
                }

                // Reset the frame callbacks state.
                window.frame_callback_reset();
                let redraw_requested = window_requests
                    .get(window_id)
                    .is_some_and(|requests| requests.take_redraw_requested());

                redraw_requested.then_some(WindowEvent::RedrawRequested)
            });

            if let Some(event) = event {
                callback(
                    Event::WindowEvent { window_id: crate::window::WindowId(*window_id), event },
                    &self.window_target,
                );
            }
        }

        // Reset the hint that we've dispatched events.
        self.with_state(|state| {
            state.dispatched_events = false;
        });

        // This is always the last event we dispatch before poll again
        callback(Event::AboutToWait, &self.window_target);

        // LOCAL PATCH (aterm), LW.5: upstream redrew each window's client-side frame here and
        // woke the loop when one needed a redraw; with no frame there is nothing to refresh.
        window_ids.clear();

        std::mem::swap(&mut self.compositor_updates, &mut compositor_updates);
        std::mem::swap(&mut self.buffer_sink, &mut buffer_sink);
        std::mem::swap(&mut self.window_ids, &mut window_ids);
    }

    #[inline]
    pub fn create_proxy(&self) -> EventLoopProxy<T> {
        EventLoopProxy::new(self.user_events_sender.clone(), self.waker().clone())
    }

    #[inline]
    pub fn window_target(&self) -> &RootActiveEventLoop {
        &self.window_target
    }

    fn active(&self) -> &ActiveEventLoop {
        match &self.window_target.p {
            PlatformActiveEventLoop::Wayland(window_target) => window_target,
            _ => unreachable!(),
        }
    }

    fn waker(&self) -> &Waker {
        &self.active().waker
    }

    fn with_state<'a, U: 'a, F: FnOnce(&'a mut WinitState) -> U>(&'a mut self, callback: F) -> U {
        let state = match &mut self.window_target.p {
            PlatformActiveEventLoop::Wayland(window_target) => window_target.state.get_mut(),
            _ => unreachable!(),
        };

        callback(state)
    }

    /// Wait for the display, a wake, a drop being read or a key's repeat, at most `timeout`
    /// (`None`: for ever); read and dispatch what came, and apply it to the state. `Err` is
    /// what the loop ends with, and its exit code ([`wire_failure`]).
    fn loop_dispatch(&mut self, timeout: Option<Duration>) -> Result<(), (EventLoopError, i32)> {
        let connection = Arc::clone(&self.connection);
        let queue = &connection.queue;
        let wake_fd = self.waker().as_raw_fd();
        let failed =
            |hung_up: bool| move |error: aterm_wayland::proxy::Error| wire_failure(error, hung_up);
        let hung_up = self.hung_up;

        // Dispatch what another reader of the display queued for this loop, then announce
        // this read; a queue that filled meanwhile is dispatched first.
        let intent = loop {
            queue.dispatch_pending().map_err(failed(hung_up))?;
            self.with_state(WinitState::drain);
            // What the state answered goes out before the wait (calloop-wayland-source's
            // `before_sleep` flushed here).
            self.flush()?;
            match queue.prepare_read().map_err(failed(hung_up))? {
                Some(intent) => break intent,
                None => continue,
            }
        };

        // Something is already waiting to be handled: look, but do not block.
        let (ready_now, repeat_at, dnd_fds) = self.with_state(|state| {
            (
                state.dispatched_events,
                state.next_key_repeat(),
                state.dnd_pipes.iter().map(|pipe| pipe.fd()).collect::<Vec<_>>(),
            )
        });
        let busy = ready_now || !self.pending_user_events.is_empty();
        let wait = if busy {
            Some(Duration::ZERO)
        } else {
            let until_repeat = repeat_at.map(|at| at.saturating_duration_since(Instant::now()));
            min_timeout(timeout, until_repeat)
        };

        let display_fd = connection.display().fd().map_err(failed(hung_up))?;
        let mut fds = Vec::with_capacity(3 + dnd_fds.len());
        let mut display_events = libc::POLLIN;
        if self.flush_pending {
            display_events |= libc::POLLOUT;
        }
        fds.push(libc::pollfd { fd: display_fd, events: display_events, revents: 0 });
        fds.push(poll::readable(wake_fd));
        fds.push(poll::readable(self.clipboard_waker.as_raw_fd()));
        fds.extend(dnd_fds.iter().map(|fd| poll::readable(*fd)));

        if let Err(error) = poll::wait(&mut fds, wait) {
            tracing::error!("Error dispatching event loop: {}", error);
            drop(intent);
            let code = error.raw_os_error().unwrap_or(1);
            return Err((
                EventLoopError::Os(os_error!(OsError::from(WaylandError::Io(error)))),
                code,
            ));
        }

        // Read what the socket holds (a hang-up or an error is read too, and says the
        // display is gone); a wake that was not the socket's cancels the read.
        self.hung_up = fds[0].revents & (libc::POLLHUP | libc::POLLERR) != 0;
        let hung_up = self.hung_up;
        if fds[0].revents & libc::POLLIN != 0 || hung_up {
            intent.read().map_err(failed(hung_up))?;
        } else {
            drop(intent);
        }

        if poll::ready(&fds[1]) {
            // Mark that we have something to dispatch.
            self.waker().drain();
            self.with_state(|state| state.dispatched_events = true);
        }
        if poll::ready(&fds[2]) {
            // A clipboard request, answered below.
            self.clipboard_waker.drain();
        }

        queue.dispatch_pending().map_err(failed(hung_up))?;
        let ready_pipes: Vec<_> =
            fds[3..].iter().filter(|p| poll::ready(p)).map(|p| p.fd).collect();
        self.with_state(|state| {
            state.drain();
            if !ready_pipes.is_empty() {
                state.read_dnd_pipes(&ready_pipes);
            }
            if state.fire_key_repeats(Instant::now()) {
                // Required to handle the wakeups from the repeat sources.
                state.dispatched_events = true;
            }
        });

        // The clipboard's requests, which the loop answers now, so their requests to the
        // compositor go out with this turn's flush.
        let requests: Vec<ClipboardRequest> = self.clipboard_requests.try_iter().collect();
        if !requests.is_empty() {
            self.with_state(|state| {
                for request in requests {
                    state.handle_clipboard_request(request);
                }
            });
        }

        // Once dispatching is finished, flush the responses to the compositor.
        self.flush()?;

        // The user's events.
        let mut user_events: Vec<T> = self.user_events_receiver.try_iter().collect();
        if !user_events.is_empty() {
            self.pending_user_events.append(&mut user_events);
            self.with_state(|state| state.dispatched_events = true);
        }

        Ok(())
    }

    /// Write the queued requests. A full socket (EAGAIN) is not an error: the rest stays
    /// queued, and the next wait also waits for the socket to take it. `Err` is what the loop
    /// ends with, as for a failed dispatch ([`wire_failure`]).
    fn flush(&mut self) -> Result<(), (EventLoopError, i32)> {
        match self.connection.display().flush() {
            Ok(()) => self.flush_pending = false,
            Err(aterm_wayland::proxy::Error::Display(libc::EAGAIN)) => self.flush_pending = true,
            Err(error) => return Err(wire_failure(error, self.hung_up)),
        }
        Ok(())
    }

    /// End the loop with `failure`, which `run_on_demand` returns, and `code` for
    /// `pump_events`.
    fn fail(&mut self, failure: EventLoopError, code: i32) {
        self.failure = Some(failure);
        self.set_exit_code(code);
    }

    fn roundtrip(&mut self) -> Result<usize, RootOsError> {
        let connection = Arc::clone(&self.connection);
        let result = connection.queue.roundtrip();
        self.with_state(|state| state.drain());
        result.map(|()| 0).map_err(|error| os_error!(OsError::from(error)))
    }

    fn control_flow(&self) -> ControlFlow {
        self.window_target.p.control_flow()
    }

    fn exiting(&self) -> bool {
        self.window_target.p.exiting()
    }

    fn set_exit_code(&self, code: i32) {
        self.window_target.p.set_exit_code(code)
    }

    fn exit_code(&self) -> Option<i32> {
        self.window_target.p.exit_code()
    }
}

/// What a failed display call ends the loop with, and its exit code (the display's errno, else
/// 1). [`EventLoopError::DisplayLost`] when the connection's transport failed: libwayland's
/// `EPIPE` (which it also reports for end of file) or `ECONNRESET`, the compositor gone (the
/// Linux lane, 2026-10-17: weston killed at rest read as `EPIPE`, once as `ECONNRESET`, and
/// killed mid-resize as `ECONNRESET`), or
/// any error after the wait saw the socket hang up or fail (`hung_up`) but a protocol error
/// (`EPROTO`: the compositor ended the connection over a request it refused, then closed it).
/// Any other failure is the call's own error.
fn wire_failure(error: aterm_wayland::proxy::Error, hung_up: bool) -> (EventLoopError, i32) {
    let errno = match error {
        aterm_wayland::proxy::Error::Display(errno) if errno > 0 => Some(errno),
        _ => None,
    };
    let transport = match errno {
        Some(libc::EPIPE | libc::ECONNRESET) => true,
        Some(libc::EPROTO) => false,
        _ => hung_up,
    };
    let failure = match (transport, errno) {
        (true, Some(errno)) => {
            EventLoopError::DisplayLost(std::io::Error::from_raw_os_error(errno))
        },
        (true, None) => EventLoopError::DisplayLost(std::io::Error::new(
            std::io::ErrorKind::ConnectionAborted,
            format!("the compositor's socket hung up ({error})"),
        )),
        (false, _) => EventLoopError::Os(os_error!(OsError::from(error))),
    };
    (failure, errno.unwrap_or(1))
}

pub struct ActiveEventLoop {
    /// The event loop wakeup source.
    pub(crate) waker: Waker,

    /// The application's latest control_flow state
    pub(crate) control_flow: Cell<ControlFlow>,

    /// The application's exit state.
    pub(crate) exit: Cell<Option<i32>>,

    // TODO remove that RefCell once we can pass `&mut` in `Window::new`.
    /// Winit state.
    pub state: RefCell<WinitState>,

    /// Connection to the wayland server.
    pub(crate) connection: Arc<Connection>,
}

impl ActiveEventLoop {
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

    #[inline]
    pub fn listen_device_events(&self, _allowed: DeviceEvents) {}

    pub(crate) fn create_custom_cursor(&self, cursor: CustomCursorSource) -> RootCustomCursor {
        RootCustomCursor {
            inner: PlatformCustomCursor::Wayland(OnlyCursorImage(Arc::from(cursor.inner.0))),
        }
    }

    #[cfg(feature = "rwh_06")]
    #[inline]
    pub fn raw_display_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
        Ok(rwh_06::WaylandDisplayHandle::new({
            std::ptr::NonNull::new(self.connection.display().as_ptr())
                .expect("wl_display should never be null")
        })
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The display's errno is the exit code, else 1; only a dead transport is
    /// the display's loss, and a protocol error stays one even when the
    /// compositor hung up after sending it.
    #[test]
    fn only_a_dead_transport_is_the_display_s_loss() {
        use aterm_wayland::proxy::Error;
        let lost = |error, hung_up| {
            let (failure, code) = wire_failure(error, hung_up);
            (matches!(failure, EventLoopError::DisplayLost(_)), code)
        };
        for hung_up in [false, true] {
            assert_eq!(lost(Error::Display(libc::EPIPE), hung_up), (true, libc::EPIPE));
            assert_eq!(lost(Error::Display(libc::ECONNRESET), hung_up), (true, libc::ECONNRESET));
            assert_eq!(lost(Error::Display(libc::EPROTO), hung_up), (false, libc::EPROTO));
        }
        assert_eq!(lost(Error::Display(libc::ENOMEM), false), (false, libc::ENOMEM));
        assert_eq!(lost(Error::Display(-1), false), (false, 1));
        assert_eq!(lost(Error::NoObject, false), (false, 1));
        assert_eq!(lost(Error::Display(libc::ENOMEM), true), (true, libc::ENOMEM));
        assert_eq!(lost(Error::Display(-1), true), (true, 1));
        let (EventLoopError::DisplayLost(why), _) =
            wire_failure(Error::Display(libc::EPIPE), false)
        else {
            panic!("EPIPE is the display's loss");
        };
        assert_eq!(why.raw_os_error(), Some(libc::EPIPE));
    }
}
