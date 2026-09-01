// A display-free event loop for `--headless` runs.
//
// WHY THIS EXISTS. The other two Unix backends are named after the display server they
// speak to, and `EventLoop::new` picks one by reading `WAYLAND_DISPLAY` / `DISPLAY`. With
// neither set there was no third answer, so building an event loop FAILED — even for a
// caller that had already said it wants no window at all. That made `--headless` require
// exactly the thing it exists to do without: CI, containers, a plain SSH session and any
// `env -i` harness could not run the app, and the failure arrived as "cannot open a
// display" advising the flag the caller had already passed.
//
// WHAT IT IS. Everything a windowless run actually consumes from an event loop, and
// nothing else: a proxy that wakes it from other threads, `ControlFlow` honoured
// (including `WaitUntil` deadlines), and the `NewEvents -> Resumed -> UserEvent ->
// AboutToWait -> LoopExiting` order the other backends emit, so an `ApplicationHandler`
// cannot tell which backend drove it. It owns no connection and no display.
//
// WHAT IT REFUSES. Windows and monitors. A headless loop has no display to put a surface
// on, so `create_window` is an error rather than a stub that returns something unusable,
// and monitor enumeration is empty. That is not a limitation being papered over: a caller
// selects this backend only by explicitly asking for it (`with_headless`), so reaching a
// window method here is a bug in the caller, and it should say so where it happens.

use std::cell::Cell;
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::os::unix::io::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::EventLoopError;
use crate::event::{Event, StartCause};
use crate::event_loop::{
    ActiveEventLoop as RootELW, ControlFlow, DeviceEvents, EventLoopClosed,
};
use crate::platform::pump_events::PumpStatus;
use crate::window::{CustomCursor, CustomCursorSource, Theme};

use super::{MonitorHandle, OwnedDisplayHandle};

/// The wake pipe. A proxy writes one byte; the loop polls the read end and drains it.
///
/// A PIPE rather than a condvar because `EventLoop` must answer `AsFd`/`AsRawFd` — an
/// embedder may select on the loop alongside its own sources, and a backend that could
/// only panic there would be a worse answer than no backend at all. The same object
/// therefore serves both obligations.
///
/// The write end is `O_NONBLOCK`: once a byte is pending the loop is already going to
/// wake, so a full pipe (`EAGAIN`) means the wake is redundant, not lost. That is what
/// makes `send_event` safe to call from any thread without ever blocking the sender.
struct WakePipe {
    read: OwnedFd,
    write: Arc<OwnedFd>,
}

impl WakePipe {
    fn new() -> std::io::Result<Self> {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` is a live two-element array, which is what pipe2 writes.
        let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: both fds were just created by pipe2 and are owned exclusively here.
        unsafe { Ok(Self { read: OwnedFd::from_raw_fd(fds[0]), write: Arc::new(OwnedFd::from_raw_fd(fds[1])) }) }
    }

    /// Drain every pending wake byte. The pipe carries no information beyond "something
    /// was posted" — the payload lives in the user-event queue — so the loop only needs
    /// the fd to stop being readable.
    fn drain(&self) {
        let mut buf = [0u8; 64];
        loop {
            // SAFETY: reading into a live local buffer from an owned fd.
            let n = unsafe {
                libc::read(self.read.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len())
            };
            // Anything other than a full buffer means the pipe is empty (or would block),
            // and an error here is either EAGAIN (empty) or EINTR (retry is pointless —
            // the next poll will see it again).
            if n < buf.len() as isize {
                break;
            }
        }
    }
}

/// Post one wake byte. Ignores `EAGAIN`: see [`WakePipe`].
fn poke(write: &OwnedFd) {
    let byte = 1u8;
    // SAFETY: writing one byte from a live local to an owned, non-blocking fd.
    unsafe {
        libc::write(write.as_raw_fd(), std::ptr::addr_of!(byte).cast(), 1);
    }
}

pub struct EventLoopProxy<T: 'static> {
    queue: Arc<Mutex<VecDeque<T>>>,
    write: Arc<OwnedFd>,
}

impl<T: 'static> Clone for EventLoopProxy<T> {
    fn clone(&self) -> Self {
        Self { queue: Arc::clone(&self.queue), write: Arc::clone(&self.write) }
    }
}

impl<T: 'static> EventLoopProxy<T> {
    pub fn send_event(&self, event: T) -> Result<(), EventLoopClosed<T>> {
        // A poisoned queue hands the event back rather than unwinding the sender: the
        // caller's `Err` arm already knows how to cope with a loop it cannot reach, and
        // a panic here would be a second failure on top of the first.
        let Ok(mut queue) = self.queue.lock() else {
            return Err(EventLoopClosed(event));
        };
        queue.push_back(event);
        drop(queue);
        poke(&self.write);
        Ok(())
    }
}

/// The windowless twin of the X11/Wayland `ActiveEventLoop`: the control-flow and exit
/// state a handler is allowed to set, and nothing that implies a display.
pub struct ActiveEventLoop {
    control_flow: Cell<ControlFlow>,
    exit: Cell<Option<i32>>,
}

impl ActiveEventLoop {
    fn new() -> Self {
        Self { control_flow: Cell::new(ControlFlow::default()), exit: Cell::new(None) }
    }

    #[inline]
    pub fn create_custom_cursor(&self, _source: CustomCursorSource) -> CustomCursor {
        unimplemented!(
            "a headless event loop has no display to define a cursor on; this backend is \
             selected only by an explicit `with_headless`, so reaching here is a caller bug"
        )
    }

    #[inline]
    pub fn available_monitors(&self) -> VecDeque<MonitorHandle> {
        VecDeque::new()
    }

    #[inline]
    pub fn primary_monitor(&self) -> Option<MonitorHandle> {
        None
    }

    #[inline]
    pub fn listen_device_events(&self, _allowed: DeviceEvents) {
        // No device source exists without a display server, so device-event filtering has
        // nothing to widen or narrow. Silently accepting the request is right: the caller
        // is expressing a preference about events this backend simply never emits.
    }

    #[inline]
    pub fn system_theme(&self) -> Option<Theme> {
        None
    }

    #[inline]
    pub(crate) fn set_control_flow(&self, control_flow: ControlFlow) {
        self.control_flow.set(control_flow);
    }

    #[inline]
    pub(crate) fn control_flow(&self) -> ControlFlow {
        self.control_flow.get()
    }

    #[inline]
    pub(crate) fn clear_exit(&self) {
        self.exit.set(None);
    }

    #[inline]
    pub(crate) fn exit(&self) {
        self.exit.set(Some(0));
    }

    #[inline]
    pub(crate) fn exiting(&self) -> bool {
        self.exit.get().is_some()
    }

    #[allow(dead_code)]
    #[inline]
    pub(crate) fn set_exit_code(&self, code: i32) {
        self.exit.set(Some(code));
    }

    #[allow(dead_code)]
    #[inline]
    pub(crate) fn exit_code(&self) -> Option<i32> {
        self.exit.get()
    }

    #[inline]
    pub(crate) fn owned_display_handle(&self) -> OwnedDisplayHandle {
        OwnedDisplayHandle::Headless
    }

    /// There is no display, so there is no handle to hand out. `NotSupported` is the
    /// honest answer and the one a graphics backend can act on — it is the same answer
    /// it would get from a platform that genuinely cannot present.
    #[cfg(feature = "rwh_06")]
    #[inline]
    pub fn raw_display_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
        Err(rwh_06::HandleError::NotSupported)
    }

    /// The 0.5 handle API has no error path, and inventing a null display handle would
    /// hand a caller something it would dereference. A headless run reaches this only by
    /// trying to present without a surface, which is a caller bug, so it says so.
    #[cfg(feature = "rwh_05")]
    #[inline]
    pub fn raw_display_handle_rwh_05(&self) -> rwh_05::RawDisplayHandle {
        unimplemented!(
            "a headless event loop has no display handle; nothing can be presented \
             without a surface, and rwh 0.5 has no way to say so"
        )
    }
}

pub struct EventLoop<T: 'static> {
    /// The root wrapper handed to the callback on every dispatch.
    target: RootELW,
    queue: Arc<Mutex<VecDeque<T>>>,
    wake: WakePipe,
    /// Whether the `StartCause::Init` iteration has already run. `pump_events` may be
    /// called many times for one loop, and only the first is `Init`.
    loop_running: bool,
    _marker: PhantomData<T>,
}

impl<T: 'static> EventLoop<T> {
    pub(crate) fn new() -> Result<Self, EventLoopError> {
        let wake = WakePipe::new().map_err(|_| {
            EventLoopError::Os(os_error!(super::OsError::Misc(
                "could not create the headless wake pipe"
            )))
        })?;
        Ok(Self {
            target: RootELW {
                p: super::ActiveEventLoop::Headless(ActiveEventLoop::new()),
                _marker: PhantomData,
            },
            queue: Arc::new(Mutex::new(VecDeque::new())),
            wake,
            loop_running: false,
            _marker: PhantomData,
        })
    }

    fn state(&self) -> &ActiveEventLoop {
        match &self.target.p {
            super::ActiveEventLoop::Headless(state) => state,
            #[allow(unreachable_patterns)]
            _ => unreachable!("a headless EventLoop always holds a headless ActiveEventLoop"),
        }
    }

    pub fn create_proxy(&self) -> EventLoopProxy<T> {
        EventLoopProxy { queue: Arc::clone(&self.queue), write: Arc::clone(&self.wake.write) }
    }

    pub fn window_target(&self) -> &RootELW {
        &self.target
    }

    pub fn run<F>(mut self, callback: F) -> Result<(), EventLoopError>
    where
        F: FnMut(Event<T>, &RootELW),
    {
        self.run_on_demand(callback)
    }

    pub fn run_on_demand<F>(&mut self, mut callback: F) -> Result<(), EventLoopError>
    where
        F: FnMut(Event<T>, &RootELW),
    {
        loop {
            match self.pump_events(None, &mut callback) {
                PumpStatus::Exit(0) => break Ok(()),
                PumpStatus::Exit(code) => break Err(EventLoopError::ExitFailure(code)),
                PumpStatus::Continue => continue,
            }
        }
    }

    pub fn pump_events<F>(&mut self, timeout: Option<Duration>, mut callback: F) -> PumpStatus
    where
        F: FnMut(Event<T>, &RootELW),
    {
        if !self.loop_running {
            self.loop_running = true;
            self.single_iteration(&mut callback, StartCause::Init);
        }

        // The `Init` iteration is allowed to ask to exit, exactly as it is on the other
        // backends; polling after that request would run a turn the app already declined.
        if !self.state().exiting() {
            self.poll_events_with_timeout(timeout, &mut callback);
        }

        if let Some(code) = self.state().exit_code() {
            self.loop_running = false;
            callback(Event::LoopExiting, &self.target);
            PumpStatus::Exit(code)
        } else {
            PumpStatus::Continue
        }
    }

    fn has_pending(&self) -> bool {
        self.queue.lock().map(|q| !q.is_empty()).unwrap_or(false)
    }

    fn poll_events_with_timeout<F>(&mut self, timeout: Option<Duration>, callback: &mut F)
    where
        F: FnMut(Event<T>, &RootELW),
    {
        let start = Instant::now();

        // Work already queued must not be made to wait on a deadline: block for zero.
        let deadline = if self.has_pending() {
            Some(Duration::ZERO)
        } else {
            let by_control_flow = match self.state().control_flow() {
                ControlFlow::Wait => None,
                ControlFlow::Poll => Some(Duration::ZERO),
                ControlFlow::WaitUntil(resume) => Some(resume.saturating_duration_since(start)),
            };
            match (by_control_flow, timeout) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        };

        self.wait_on_wake_pipe(deadline);
        self.wake.drain();

        // The cause is read AFTER the wait, so a `WaitUntil` that actually elapsed is
        // reported as `ResumeTimeReached` and one cut short by a proxy wake is reported
        // as `WaitCancelled` — the distinction a handler uses to tell a due timer from
        // an early wake.
        let cause = match self.state().control_flow() {
            ControlFlow::Poll => StartCause::Poll,
            ControlFlow::Wait => StartCause::WaitCancelled { start, requested_resume: None },
            ControlFlow::WaitUntil(resume) => {
                if Instant::now() < resume {
                    StartCause::WaitCancelled { start, requested_resume: Some(resume) }
                } else {
                    StartCause::ResumeTimeReached { start, requested_resume: resume }
                }
            },
        };

        self.single_iteration(callback, cause);
    }

    /// Block until the wake pipe is readable or `deadline` elapses. `None` waits forever,
    /// which is what `ControlFlow::Wait` means when no proxy has posted anything.
    fn wait_on_wake_pipe(&self, deadline: Option<Duration>) {
        let mut pollfd = libc::pollfd {
            fd: self.wake.read.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // `poll` takes whole milliseconds, and the wait must round UP: truncating a
        // 119.6 ms remainder to 119 wakes BEFORE the deadline, which costs a whole
        // spurious turn reported as `WaitCancelled` when the timer was in fact about to
        // come due. Rounding up means a `WaitUntil` turn is `ResumeTimeReached` — the
        // thing a handler uses to tell a due timer from an early wake. -1 is "no
        // deadline"; zero stays zero so `Poll` does not gain a millisecond of latency.
        let millis: libc::c_int = match deadline {
            None => -1,
            Some(d) if d.is_zero() => 0,
            Some(d) => {
                let ceil_ms = d.as_nanos().div_ceil(1_000_000).max(1);
                libc::c_int::try_from(ceil_ms).unwrap_or(libc::c_int::MAX)
            },
        };
        // SAFETY: one live pollfd, count 1.
        unsafe {
            libc::poll(&mut pollfd, 1, millis);
        }
    }

    fn single_iteration<F>(&mut self, callback: &mut F, cause: StartCause)
    where
        F: FnMut(Event<T>, &RootELW),
    {
        callback(Event::NewEvents(cause), &self.target);

        if cause == StartCause::Init {
            callback(Event::Resumed, &self.target);
        }

        // Drain by repeated pop rather than by swapping the whole queue out: a handler is
        // allowed to post to its own proxy, and those events belong to THIS turn's drain
        // if they arrive before it finishes, exactly as on the other backends.
        loop {
            let next = match self.queue.lock() {
                Ok(mut queue) => queue.pop_front(),
                Err(_) => None,
            };
            let Some(event) = next else { break };
            callback(Event::UserEvent(event), &self.target);
        }

        callback(Event::AboutToWait, &self.target);
    }
}

impl<T> AsFd for EventLoop<T> {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.wake.read.as_fd()
    }
}

impl<T> AsRawFd for EventLoop<T> {
    fn as_raw_fd(&self) -> RawFd {
        self.wake.read.as_raw_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive one loop to completion, recording the event sequence. The handler exits on
    /// the turn `stop_after` reaches zero so no test can hang the suite.
    fn drive<T: 'static>(
        mut loop_: EventLoop<T>,
        mut on_turn: impl FnMut(&RootELW, usize),
    ) -> Vec<String> {
        let mut log = Vec::new();
        let mut turns = 0usize;
        let _ = loop_.run_on_demand(|event, target| {
            match &event {
                Event::NewEvents(cause) => log.push(format!("NewEvents({cause:?})")),
                Event::Resumed => log.push("Resumed".into()),
                Event::UserEvent(_) => log.push("UserEvent".into()),
                Event::AboutToWait => {
                    log.push("AboutToWait".into());
                    turns += 1;
                    on_turn(target, turns);
                },
                Event::LoopExiting => log.push("LoopExiting".into()),
                _ => {},
            }
            let _ = target;
        });
        log
    }

    /// The order an `ApplicationHandler` is entitled to. `Resumed` must arrive exactly
    /// once, on the `Init` turn, and `LoopExiting` must be last — a handler that sets up
    /// on `Resumed` and tears down on `LoopExiting` is otherwise silently broken here
    /// while working on X11 and Wayland.
    #[test]
    fn the_first_turn_is_init_then_resumed_and_the_last_is_loop_exiting() {
        let loop_: EventLoop<()> = EventLoop::new().unwrap();
        let log = drive(loop_, |target, _| target.p.exit());
        assert_eq!(log.first().map(String::as_str), Some("NewEvents(Init)"));
        assert_eq!(log.get(1).map(String::as_str), Some("Resumed"));
        assert_eq!(log.last().map(String::as_str), Some("LoopExiting"));
        assert_eq!(log.iter().filter(|l| *l == "Resumed").count(), 1, "Resumed is once, not per turn");
    }

    /// A proxy is the ONLY way a windowless run learns anything from another thread, so
    /// an event posted before the loop starts must still be delivered — the queue, not
    /// the wake byte, is what carries the payload.
    #[test]
    fn an_event_posted_before_the_loop_runs_is_still_delivered() {
        let loop_: EventLoop<u32> = EventLoop::new().unwrap();
        loop_.create_proxy().send_event(7).unwrap();
        let log = drive(loop_, |target, _| target.p.exit());
        assert!(log.contains(&"UserEvent".to_string()), "a pre-posted event was dropped: {log:?}");
    }

    /// The cross-thread case this backend exists to serve.
    #[test]
    fn an_event_posted_from_another_thread_wakes_a_waiting_loop() {
        let loop_: EventLoop<u32> = EventLoop::new().unwrap();
        let proxy = loop_.create_proxy();
        // ControlFlow::Wait means the loop blocks indefinitely; only the proxy's wake
        // byte can free it. If the pipe did not work this test would hang, which is a
        // louder failure than a wrong assertion.
        loop_.window_target().p.set_control_flow(ControlFlow::Wait);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            let _ = proxy.send_event(11);
        });
        let log = drive(loop_, |target, turn| {
            if turn >= 2 {
                target.p.exit();
            }
        });
        assert!(log.contains(&"UserEvent".to_string()), "a threaded wake never arrived: {log:?}");
    }

    /// `WaitUntil` must actually wait. A backend that returned immediately would turn
    /// every timer into a busy spin — the exact defect class the spin conformance suite
    /// exists to catch, so it must not be reintroduced by the loop underneath it.
    #[test]
    fn wait_until_sleeps_until_its_deadline_and_reports_resume_time_reached() {
        let loop_: EventLoop<()> = EventLoop::new().unwrap();
        let deadline = Instant::now() + Duration::from_millis(120);
        loop_.window_target().p.set_control_flow(ControlFlow::WaitUntil(deadline));
        let started = Instant::now();
        let log = drive(loop_, |target, turn| {
            if turn >= 2 {
                target.p.exit();
            }
        });
        assert!(
            started.elapsed() >= Duration::from_millis(100),
            "the loop did not wait for its deadline ({:?}) — that is a spin",
            started.elapsed()
        );
        assert!(
            log.iter().any(|l| l.contains("ResumeTimeReached")),
            "an elapsed deadline must report ResumeTimeReached, got {log:?}"
        );
    }

    /// Exit requested during the very first turn must be honoured without running a
    /// second one, matching the other backends.
    #[test]
    fn an_exit_on_the_init_turn_runs_no_further_turn() {
        let loop_: EventLoop<()> = EventLoop::new().unwrap();
        let log = drive(loop_, |target, _| target.p.exit());
        assert_eq!(
            log.iter().filter(|l| l.starts_with("NewEvents")).count(),
            1,
            "a loop told to exit on Init ran another turn: {log:?}"
        );
    }
}
