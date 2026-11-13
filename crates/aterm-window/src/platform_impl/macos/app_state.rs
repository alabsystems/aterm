// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/macos/app_state.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fmt;
use std::mem;
use std::panic::AssertUnwindSafe;
use std::rc::Weak;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

// LOCAL PATCH (aterm): Objective-C goes through `aterm_objc` (typed `send` prototypes,
// `Obj`/`Retained` handles, `MainThread` witnesses, the seam's `consts`) where upstream used objc2
// and its framework bindings.
use aterm_objc::send::{
    send_bool, send_bool_isize, send_id, send_id_usize, send_usize, send_v_bool, send_v_id,
};
use aterm_objc::{autoreleasepool, class, sel, Bool, Id, MainThread};

static QUIT_CONFIRM_HOOK: OnceLock<fn(ReplyWait) -> TerminateAnswer> = OnceLock::new();

/// Registers the handler that answers AppKit's `applicationShouldTerminate:` (`terminate:`).
pub(crate) fn set_quit_confirm_hook(hook: fn(ReplyWait) -> TerminateAnswer) {
    let _ = QUIT_CONFIRM_HOOK.set(hook);
}

/// The handler's answer to an ask `wait` describes; with none registered, AppKit terminates now.
fn quit_confirm_answer(wait: ReplyWait) -> TerminateAnswer {
    QUIT_CONFIRM_HOOK.get().map_or(TerminateAnswer::Now, |hook| hook(wait))
}

/// How far AppKit's launch of this process has come.
///
/// LOCAL PATCH (aterm): readable from any thread ([`appkit_launch_stage`]), so the application
/// can name where a launch stopped while its main thread is still inside AppKit: aterm's launch
/// watch reads it from its sampler thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum AppKitLaunchStage {
    /// `-[NSApplication run]` has not been entered for the launch yet. The event loop's
    /// construction is here: its `+[NSApplication sharedApplication]` checks the process in with
    /// LaunchServices.
    NotRun = 0,
    /// Inside `-[NSApplication run]`, before `applicationDidFinishLaunching:`: AppKit's own
    /// launch work, window restoration and any alert it raises there.
    Running = 1,
    /// Inside `applicationDidFinishLaunching:`: this crate's setup, then the `Init` and `Resumed`
    /// it hands the event handler installed then. That is the application's own where the loop's
    /// run launches AppKit; where `prepare_launch` launches it, a no-op one, and the loop's later
    /// run hands the application's handler `Init` and `Resumed` at [`Self::Finished`].
    DidFinish = 2,
    /// `applicationDidFinishLaunching:` returned.
    Finished = 3,
}

impl AppKitLaunchStage {
    const fn from_u8(stage: u8) -> Self {
        match stage {
            0 => Self::NotRun,
            1 => Self::Running,
            2 => Self::DidFinish,
            _ => Self::Finished,
        }
    }
}

/// The process's one launch, as [`AppKitLaunchStage`].
static LAUNCH_STAGE: AtomicU8 = AtomicU8::new(AppKitLaunchStage::NotRun as u8);

/// How far AppKit's launch has come, read from any thread.
pub(crate) fn appkit_launch_stage() -> AppKitLaunchStage {
    AppKitLaunchStage::from_u8(LAUNCH_STAGE.load(Ordering::Acquire))
}

/// Move `cell` on to `stage`; a launch never goes back, so an earlier stage is ignored.
fn advance_launch_stage(cell: &AtomicU8, stage: AppKitLaunchStage) {
    cell.fetch_max(stage as u8, Ordering::AcqRel);
}

/// Marks the launch [`AppKitLaunchStage::Finished`] as `applicationDidFinishLaunching:` returns,
/// whichever way it returns.
struct LaunchFinishedOnReturn;

impl Drop for LaunchFinishedOnReturn {
    fn drop(&mut self) {
        advance_launch_stage(&LAUNCH_STAGE, AppKitLaunchStage::Finished);
    }
}

type HandlerCallback = Box<dyn FnOnce(&ApplicationDelegate)>;

enum HandlerWork {
    Callback(HandlerCallback),
    ExitBarrier,
}

/// Work that may enter the application event handler, in native-callback order.
///
/// A separate armed bit lets a nested run-loop block discover that the handler is still borrowed,
/// disarm without removing the front item, and leave the outer handler's normal return path to arm
/// one later block. Requeuing from the nested block itself could spin inside the modal loop. The
/// in-progress bit keeps native callbacks behind the whole batch, including a multi-event unit
/// such as a scale-factor change.
struct PendingHandlerWork<T> {
    queue: VecDeque<T>,
    drain_armed: bool,
    batch_in_progress: bool,
}

impl<T> PendingHandlerWork<T> {
    const fn new() -> Self {
        Self { queue: VecDeque::new(), drain_armed: false, batch_in_progress: false }
    }

    fn can_run_inline(&self) -> bool {
        self.queue.is_empty() && !self.drain_armed && !self.batch_in_progress
    }

    fn push(&mut self, work: T) {
        self.queue.push_back(work);
    }

    /// Marks one outer-loop delivery as pending. Whether the caller must schedule its block.
    fn arm(&mut self) -> bool {
        if self.drain_armed || self.batch_in_progress || self.queue.is_empty() {
            false
        } else {
            self.drain_armed = true;
            true
        }
    }

    /// Begins an armed batch, retaining every item when another handler is active.
    fn begin_batch(&mut self, handler_in_use: bool) -> usize {
        if !self.drain_armed {
            return 0;
        }
        self.drain_armed = false;
        if handler_in_use {
            return 0;
        }
        let len = self.queue.len();
        self.batch_in_progress = len != 0;
        len
    }

    fn pop_batch_item(&mut self) -> T {
        debug_assert!(self.batch_in_progress, "only an active batch has items");
        self.queue.pop_front().expect("the batch length fixes its original items")
    }

    fn finish_batch(&mut self) {
        debug_assert!(self.batch_in_progress, "only a started handler batch can finish");
        self.batch_in_progress = false;
    }

    /// Begins a batch over the queue, armed or not, where an exit commits without the exit
    /// barrier's block (`ApplicationDelegate::run_owed_work`): false inside a batch in progress,
    /// which owns its items. The batch stays in progress, so nothing arms and what its work
    /// queues joins it ([`Self::pop_owed`]), until `finish_batch` or `finish_batch_and_take_tail`
    /// ends it, an empty one included.
    fn begin_owed(&mut self) -> bool {
        if self.batch_in_progress {
            return false;
        }
        self.drain_armed = false;
        self.batch_in_progress = true;
        true
    }

    /// The owed batch's next item, work queued while it runs included.
    fn pop_owed(&mut self) -> Option<T> {
        debug_assert!(self.batch_in_progress, "only an owed batch pops past its snapshot");
        self.queue.pop_front()
    }

    /// Finishes the active batch and extracts everything behind its ordered exit barrier.
    ///
    /// The caller drops the returned work after releasing the FIFO's `RefCell` borrow, so a
    /// future callback capture with re-entrant `Drop` cannot collide with that borrow.
    fn finish_batch_and_take_tail(&mut self) -> VecDeque<T> {
        debug_assert!(self.batch_in_progress, "only a started handler batch can finish");
        self.drain_armed = false;
        self.batch_in_progress = false;
        std::mem::take(&mut self.queue)
    }
}

impl<T> fmt::Debug for PendingHandlerWork<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingHandlerWork")
            .field("queued", &self.queue.len())
            .field("drain_armed", &self.drain_armed)
            .field("batch_in_progress", &self.batch_in_progress)
            .finish()
    }
}

/// Rearms work queued by a nested callback once the outer handler releases its borrow.
///
/// Objective-C exceptions unwind Rust drops before `aterm_objc::contain` catches them, without
/// making `std::thread::panicking()` true. Rust panics do set it and stop the app, so retain the
/// previous behavior of leaving work unarmed on that fatal path.
struct HandlerWorkRearm<'a>(&'a ApplicationDelegate);

/// Ends the batch `ApplicationDelegate::run_owed_work` began, on its return or its unwind: a
/// completed run gives up the rest, as the exit barrier gives up its tail; a run a panic stopped
/// keeps it for the fatal turn, as the batch block does. The given-up work is dropped after the
/// FIFO's `RefCell` borrow is released.
struct OwedBatch<'a> {
    delegate: &'a ApplicationDelegate,
    completed: Cell<bool>,
}

impl Drop for OwedBatch<'_> {
    fn drop(&mut self) {
        let given_up = {
            let mut pending = self.delegate.ivars().pending_handler_work.borrow_mut();
            if self.completed.get() {
                Some(pending.finish_batch_and_take_tail())
            } else {
                pending.finish_batch();
                None
            }
        };
        drop(given_up);
    }
}

impl Drop for HandlerWorkRearm<'_> {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.0.arm_handler_work();
        }
    }
}

use super::aterm_objc_seam::consts::NS_APPLICATION_ACTIVATION_POLICY_REGULAR;
use super::event_handler::EventHandler;
use super::event_loop::{
    notify_windows_of_exit, stop_app_immediately, stop_app_on_panic, ActiveEventLoop, PanicInfo,
};
use super::observer::{EventLoopWaker, RunLoop};
use super::terminate::{self, ReplyWait, TerminateAnswer};
use super::{menu, wake_queue, WindowId};
use crate::event::{Event, StartCause, WindowEvent};
use crate::event_loop::{ActiveEventLoop as RootActiveEventLoop, ControlFlow};
use crate::window::WindowId as RootWindowId;

#[derive(Debug)]
pub(super) struct AppState {
    /// LOCAL PATCH (aterm): the raw `NSApplicationActivationPolicy`, which is an `NSInteger`.
    activation_policy: Option<isize>,
    activate_ignoring_other_apps: bool,
    run_loop: RunLoop,
    event_handler: EventHandler,
    /// The loop-owned panic slot that lets queued handler work return an unwind through the pump.
    panic_info: Weak<PanicInfo>,
    /// Native callbacks deferred until the current event handler and any nested modal loop return.
    pending_handler_work: RefCell<PendingHandlerWork<HandlerWork>>,
    /// One accepted native termination is waiting to be retried after the active handler.
    termination_retry_pending: Cell<bool>,
    stop_on_launch: Cell<bool>,
    stop_before_wait: Cell<bool>,
    stop_after_wait: Cell<bool>,
    stop_on_redraw: Cell<bool>,
    /// Whether `applicationDidFinishLaunching:` has been run or not.
    is_launched: Cell<bool>,
    /// Whether an `EventLoop` is currently running.
    is_running: Cell<bool>,
    /// Whether the user has requested the event loop to exit.
    exit: Cell<bool>,
    /// Whether the ordered exit barrier has drained everything owed before the request.
    exit_committed: Cell<bool>,
    control_flow: Cell<ControlFlow>,
    waker: RefCell<EventLoopWaker>,
    start_time: Cell<Option<Instant>>,
    wait_timeout: Cell<Option<Instant>>,
    pending_redraw: RefCell<Vec<WindowId>>,
    /// LOCAL PATCH (aterm): set when a user event's send armed the wake since the last take;
    /// `cleared` then wakes the run loop before it sleeps, so the event is not left waiting.
    wake_armed: wake_queue::Armed,
    /// LOCAL PATCH (aterm): what the user-event queue's latest take found, which
    /// `ActiveEventLoop::user_events_drained` reports.
    drained: wake_queue::Drained,
}

// LOCAL PATCH (aterm): declared by `aterm_objc::declare_class!`, where upstream used objc2's. All
// three rows are `@optional`, which AppKit reaches by `-respondsToSelector:`, so a row that failed
// to register would fail silently; the live-class audit reads them back.
aterm_objc::declare_class! {
    /// The `NSApplicationDelegate` installed on `NSApp` and owner of the event-loop state.
    #[derive(Debug)]
    pub(super) struct ApplicationDelegate: NSObject {
        const NAME: &str = "WinitApplicationDelegate";
        type Ivars = AppState;
        protocols: [NSObject, NSApplicationDelegate];

        @sel(applicationDidFinishLaunching:) @abort_on_exception
        fn app_did_finish_launching(&self, _notification: Id) {
            self.did_finish_launching()
        }

        @sel(applicationWillTerminate:) @abort_on_exception
        fn app_will_terminate(&self, _notification: Id) {
            self.will_terminate()
        }

        // LOCAL PATCH (aterm): the application's handler decides, told whether it could reply
        // from AppKit's wait; a quit it takes up answers `NSTerminateLater`, and AppKit then
        // waits for the one reply the loop's exit or a refusal sends (`terminate.rs`).
        @sel(applicationShouldTerminate:)
        fn app_should_terminate(&self, _sender: Id) -> usize {
            if self.exiting() {
                // LOCAL PATCH (aterm): the loop already exits, so this quit asks no question. A
                // Cancel would make macOS abandon a logout, restart or shutdown; where a reply
                // can come the quit is taken up (`Later`), the exit commits inside AppKit's wait
                // ([`Self::commit_owed_exit`]) and AppKit hears YES after it. Where none can, it
                // is refused, as `should_terminate` refuses any such `Later`.
                let wait = self.reply_wait();
                return terminate::should_terminate(TerminateAnswer::Later, wait);
            }
            if self.ivars().termination_retry_pending.get() {
                return terminate::should_terminate(TerminateAnswer::Cancel, ReplyWait::Unanswerable);
            }
            if self.is_running() && self.ivars().event_handler.in_use() {
                // `terminate:` arrived from a nested modal run loop while an outer callback still
                // owns EventHandler's RefCell. Nothing can answer a `Later` from here, and a `Now`
                // would enter `applicationWillTerminate:` and deliver LoopExiting through that same
                // handler. Cancel only this synchronous attempt, before the application's handler
                // is asked (so a person's quit question is put once, on the retry), and retry it
                // from the ordered handler-work queue once every older callback has completed,
                // where the retry is answerable and takes the ordinary path below.
                self.ivars().termination_retry_pending.set(true);
                self.defer_termination_retry();
                return terminate::should_terminate(TerminateAnswer::Cancel, ReplyWait::Unanswerable);
            }
            let wait = self.reply_wait();
            terminate::should_terminate(quit_confirm_answer(wait), wait)
        }
    }
}

impl ApplicationDelegate {
    pub(super) fn new(
        mtm: MainThread,
        activation_policy: Option<isize>,
        activate_ignoring_other_apps: bool,
        panic_info: Weak<PanicInfo>,
        wake_armed: wake_queue::Armed,
        drained: wake_queue::Drained,
    ) -> aterm_objc::Retained<Self> {
        ApplicationDelegate::alloc_init(mtm, AppState {
            activation_policy,
            activate_ignoring_other_apps,
            run_loop: RunLoop::main(mtm),
            event_handler: EventHandler::new(),
            panic_info,
            pending_handler_work: RefCell::new(PendingHandlerWork::new()),
            termination_retry_pending: Cell::new(false),
            stop_on_launch: Cell::new(false),
            stop_before_wait: Cell::new(false),
            stop_after_wait: Cell::new(false),
            stop_on_redraw: Cell::new(false),
            is_launched: Cell::new(false),
            is_running: Cell::new(false),
            exit: Cell::new(false),
            exit_committed: Cell::new(false),
            control_flow: Cell::new(ControlFlow::default()),
            waker: RefCell::new(EventLoopWaker::new(mtm)),
            start_time: Cell::new(None),
            wait_timeout: Cell::new(None),
            pending_redraw: RefCell::new(vec![]),
            wake_armed,
            drained,
        })
        .expect("couldn't create `WinitApplicationDelegate`")
    }

    fn did_finish_launching(&self) {
        trace_scope!("applicationDidFinishLaunching:");
        self.ivars().is_launched.set(true);
        advance_launch_stage(&LAUNCH_STAGE, AppKitLaunchStage::DidFinish);
        let _finished = LaunchFinishedOnReturn;

        let mtm = self.mtm();
        let ns_app = app();
        // SAFETY (for this whole block): `ns_app` is the live shared `NSApplication`, as
        // `window_activation_hack` requires; `-setActivationPolicy:` is `B24@0:8q16` (its answer is
        // ignored, as upstream ignored it); `+currentApplication` is `@16#0:8` and
        // `-bundleIdentifier` `@16@0:8`, which may answer nil; `-activateIgnoringOtherApps:` is
        // `v20@0:8B16`.
        unsafe {
            if let Some(activation_policy) = self.ivars().activation_policy {
                send_bool_isize(ns_app, sel!(setActivationPolicy:), activation_policy);
            } else {
                let running =
                    send_id(class(c"NSRunningApplication").as_id(), sel!(currentApplication));
                let is_bundled = !send_id(running, sel!(bundleIdentifier)).is_null();
                if !is_bundled {
                    send_bool_isize(
                        ns_app,
                        sel!(setActivationPolicy:),
                        NS_APPLICATION_ACTIVATION_POLICY_REGULAR,
                    );
                }
            }

            window_activation_hack(ns_app);
            send_v_bool(
                ns_app,
                sel!(activateIgnoringOtherApps:),
                self.ivars().activate_ignoring_other_apps,
            );
        }

        // LOCAL PATCH (aterm): always; `with_default_menu` is gone. aterm replaces the main menu,
        // but this one registers the Services menu.
        // SAFETY: `ns_app` is the live shared `NSApplication`.
        unsafe { menu::initialize(ns_app, mtm) };

        self.ivars().waker.borrow_mut().start();

        self.set_is_running(true);
        self.dispatch_init_events();

        if self.ivars().stop_on_launch.get() && !self.exiting() {
            // SAFETY: `app()` is the live shared `NSApplication`.
            unsafe { stop_app_immediately(app()) };
        }
    }

    /// LOCAL PATCH (aterm): whether the application's handler could run in AppKit's wait for a
    /// reply to the quit it asks about now: the loop runs and the handler is set and not on the
    /// stack (AppKit asking from inside it, from a modal alert or panel it runs, is not).
    ///
    /// Nor can it with a panic pending: the observers then deliver nothing (`cleared`), so no
    /// exit would commit to reply to a `Later`, and AppKit would wait on it for good.
    fn reply_wait(&self) -> ReplyWait {
        let panicking = self.ivars().panic_info.upgrade().is_some_and(|info| info.is_panicking());
        if self.is_running() && self.ivars().event_handler.ready() && !panicking {
            ReplyWait::Answerable
        } else {
            ReplyWait::Unanswerable
        }
    }

    /// LOCAL PATCH (aterm): AppKit calls `exit(0)` once this returns, so `run_app` never returns:
    /// the loop's exit, the application's shutdown in it, is delivered here, inside AppKit
    /// (`terminate::exit_ends_in_appkit`), unless a deferred quit's YES delivered it already, or
    /// the event handler is on the stack (AppKit terminating from inside a modal alert or panel
    /// it runs), where it could not be handed `LoopExiting` (`terminate::appkit_terminates`).
    /// AppKit terminating is the exit: asked for here where the application did not ask
    /// ([`Self::exit`]), which closes the handler queue as the exit barrier does, so the work
    /// queued before it runs first and ends at the barrier ([`Self::run_owed_work`]), and
    /// nothing that work causes can feed it. The exit is then committed, as the barrier's block
    /// would commit it, so each window's `Destroyed` reaches the handler, the one an `exit()`
    /// already asked for included (AppKit terminating at once, a second `terminate:` during its
    /// wait on a deferred quit's reply). A panic that stops that work still leaves the windows
    /// closed before `LoopExiting`.
    fn will_terminate(&self) {
        trace_scope!("applicationWillTerminate:");
        let _ = self.mtm();
        terminate::appkit_terminates(self.ivars().event_handler.in_use(), || {
            if self.is_running() {
                self.exit();
                let _ = self.run_owed_work();
                self.close_windows_and_exit();
            } else {
                self.internal_exit();
            }
        });
    }

    /// The delegate `NSApp` is holding, as this class.
    pub fn get(w: aterm_objc::MainThread) -> aterm_objc::Retained<Self> {
        let _ = w;
        // SAFETY: `-delegate` is `@16@0:8` on `NSApplication` and answers +0.
        let delegate = unsafe { send_id(app(), sel!(delegate)) };
        assert!(!delegate.is_null(), "a delegate was not configured on the application");
        // SAFETY: `delegate` is the live object `-[NSApplication delegate]` just answered, and
        // `-isKindOfClass:` is `B@:#` on every Apple runtime.
        let is_ours = unsafe {
            let send: unsafe extern "C-unwind" fn(
                Id,
                aterm_objc::Sel,
                aterm_objc::ClassPtr,
            ) -> Bool = aterm_objc::checked_msg();
            send(
                delegate,
                aterm_objc::sel!(isKindOfClass:),
                <Self as aterm_objc::ClassType>::class(),
            )
        };
        assert!(
            is_ours.as_bool(),
            "tried to get a delegate that was not the one Winit has registered"
        );
        // SAFETY: just checked that the delegate is an instance of `ApplicationDelegate`; `retain`
        // takes the borrowed reference to +1.
        unsafe { aterm_objc::Retained::retain(delegate) }.expect("a non-null delegate retains")
    }

    /// Main-thread witness for the receiver's methods, which AppKit calls on the main thread.
    #[track_caller]
    fn mtm(&self) -> MainThread {
        MainThread::new().expect(
            "a WinitApplicationDelegate method ran off the main thread; AppKit delivers on it",
        )
    }

    /// Returns a retained `+1` handle to this delegate.
    pub(super) fn retained(&self) -> aterm_objc::Retained<Self> {
        // SAFETY: `self` borrows a live instance of this class, so `as_id()` is a live non-null
        // receiver for `objc_retain`.
        unsafe { aterm_objc::Retained::retain(self.as_id()) }
            .expect("retaining a live ApplicationDelegate")
    }

    /// This delegate as the `id` `-[NSApplication setDelegate:]` takes.
    pub(super) fn as_delegate_id(&self) -> Id {
        let id = self.as_id();
        debug_assert!(
            {
                // SAFETY: `id` is a live instance of this class; `-conformsToProtocol:` is
                // `B24@0:8@16` on `NSObject`.
                unsafe {
                    let f: unsafe extern "C-unwind" fn(
                        Id,
                        aterm_objc::Sel,
                        aterm_objc::ProtocolPtr,
                    ) -> Bool = aterm_objc::checked_msg();
                    f(id, sel!(conformsToProtocol:), aterm_objc::protocol(c"NSApplicationDelegate"))
                        .as_bool()
                }
            },
            "WinitApplicationDelegate does not conform to NSApplicationDelegate: \
             class_addProtocol did not run at declaration, so aterm's live-class \
             audit and typed consumers see a delegate that does not claim the \
             protocol (AppKit itself reaches the @optional rows by \
             -respondsToSelector: and is unaffected)"
        );
        id
    }

    /// Place the event handler in the application delegate for the duration of the given closure.
    pub fn set_event_handler<R>(
        &self,
        handler: impl FnMut(Event<HandlePendingUserEvents>, &RootActiveEventLoop),
        closure: impl FnOnce() -> R,
    ) -> R {
        self.ivars().event_handler.set(handler, closure)
    }

    /// Stops the initial `-[NSApplication run]` pass once launching finishes.
    pub fn set_stop_on_launch(&self) {
        self.ivars().stop_on_launch.set(true);
    }

    pub fn set_stop_before_wait(&self, value: bool) {
        self.ivars().stop_before_wait.set(value)
    }

    pub fn set_stop_after_wait(&self, value: bool) {
        self.ivars().stop_after_wait.set(value)
    }

    pub fn set_stop_on_redraw(&self, value: bool) {
        self.ivars().stop_on_redraw.set(value)
    }

    pub fn set_wait_timeout(&self, value: Option<Instant>) {
        self.ivars().wait_timeout.set(value)
    }

    /// Clears the `running` state and resets the `control_flow` state when an `EventLoop` exits.
    ///
    /// LOCAL PATCH (aterm): `LoopExiting` goes out once per run: a run whose exit was delivered
    /// inside AppKit (a deferred quit's YES, then `applicationWillTerminate:`) is not told again.
    pub fn internal_exit(&self) {
        if self.is_running() {
            self.handle_event(Event::LoopExiting);
        }

        self.set_is_running(false);
        self.set_stop_on_redraw(false);
        self.set_stop_before_wait(false);
        self.set_stop_after_wait(false);
        self.set_wait_timeout(None);
    }

    pub fn is_launched(&self) -> bool {
        self.ivars().is_launched.get()
    }

    /// LOCAL PATCH (aterm): `-[NSApplication run]` is about to be entered; before
    /// `applicationDidFinishLaunching:` that run is the launch ([`AppKitLaunchStage::Running`]).
    pub fn entering_run(&self) {
        if !self.is_launched() {
            advance_launch_stage(&LAUNCH_STAGE, AppKitLaunchStage::Running);
        }
    }

    pub fn set_is_running(&self, value: bool) {
        self.ivars().is_running.set(value)
    }

    pub fn is_running(&self) -> bool {
        self.ivars().is_running.get()
    }

    pub fn exit(&self) {
        if !self.ivars().exit.replace(true) {
            // A programmatic exit supersedes an AppKit `terminate:` retry accepted while the
            // same handler was active. Its queued closures re-check this bit before retrying.
            self.ivars().termination_retry_pending.set(false);
            self.ivars().pending_handler_work.borrow_mut().push(HandlerWork::ExitBarrier);
            self.arm_handler_work();
        }
    }

    pub fn exiting(&self) -> bool {
        self.ivars().exit.get()
    }

    pub fn exit_committed(&self) -> bool {
        self.ivars().exit_committed.get()
    }

    pub fn user_events_drained(&self) -> bool {
        self.ivars().drained.emptied()
    }

    pub fn set_control_flow(&self, value: ControlFlow) {
        self.ivars().control_flow.set(value)
    }

    pub fn control_flow(&self) -> ControlFlow {
        self.ivars().control_flow.get()
    }

    pub fn maybe_queue_window_event(&self, window_id: WindowId, event: WindowEvent) {
        self.maybe_queue_event(Event::WindowEvent { window_id: RootWindowId(window_id), event });
    }

    pub fn handle_window_event(&self, window_id: WindowId, event: WindowEvent) {
        self.handle_event(Event::WindowEvent { window_id: RootWindowId(window_id), event });
    }

    pub fn handle_redraw(&self, window_id: WindowId) {
        let _ = self.mtm();
        if self.exiting() {
            return;
        }
        let origin = super::key_origin::capture();
        let deliver = move |this: &ApplicationDelegate| {
            this.handle_event_with_origin(
                Event::WindowEvent {
                    window_id: RootWindowId(window_id),
                    event: WindowEvent::RedrawRequested,
                },
                origin,
            );

            if this.ivars().stop_on_redraw.get() && !this.exiting() {
                // SAFETY: `app()` is the live shared `NSApplication`.
                unsafe { stop_app_immediately(app()) };
            }
        };
        if !self.ivars().event_handler.in_use()
            && self.ivars().pending_handler_work.borrow().can_run_inline()
        {
            deliver(self);
        } else {
            // This is one event per AppKit `drawRect:`, unlike coalesced application redraw
            // requests. Keep it behind every older native callback without merging it.
            self.queue_handler_work(deliver);
        }
    }

    pub fn queue_redraw(&self, window_id: WindowId) {
        if self.exiting() {
            return;
        }
        let mut pending_redraw = self.ivars().pending_redraw.borrow_mut();
        if !pending_redraw.contains(&window_id) {
            pending_redraw.push(window_id);
        }
        self.ivars().run_loop.wakeup();
    }

    #[track_caller]
    fn maybe_queue_event(&self, event: Event<HandlePendingUserEvents>) {
        let exit_destroyed = self.exit_committed()
            && matches!(&event, Event::WindowEvent { event: WindowEvent::Destroyed, .. });
        if self.exiting() && !exit_destroyed {
            return;
        }
        let origin = super::key_origin::capture();
        if !self.ivars().event_handler.in_use()
            && self.ivars().pending_handler_work.borrow().can_run_inline()
        {
            self.handle_event_with_origin(event, origin);
        } else {
            tracing::debug!(?event, "deferred event behind active or pending handler work");
            self.queue_handler_work(move |this| this.handle_event_with_origin(event, origin));
        }
    }

    /// Runs the FIFO snapshot pending at the start of an outer-loop block, never from inside an
    /// active handler. Work appended by that batch waits for the next block.
    ///
    /// `window_delegate` also uses this for scale-factor work whose synchronous size writer means
    /// the whole operation, rather than only its two events, has to wait for handler availability.
    pub(super) fn queue_handler_work(&self, work: impl FnOnce(&ApplicationDelegate) + 'static) {
        if self.exiting() {
            return;
        }
        self.ivars().pending_handler_work.borrow_mut().push(HandlerWork::Callback(Box::new(work)));
        self.arm_handler_work();
    }

    /// Retries `terminate:` only after every handler callback older than the request.
    ///
    /// The FIFO sentinel schedules a separate run-loop block because `willTerminate:` closes our
    /// windows synchronously. Their Destroyed events must run inline before LoopExiting, not queue
    /// behind the batch containing the sentinel. If an older callback appended more FIFO work,
    /// the block puts another sentinel at the tail and tries again after that work has drained.
    fn defer_termination_retry(&self) {
        self.queue_handler_work(|this| {
            if !this.ivars().termination_retry_pending.get() || this.exiting() {
                return;
            }
            let retained = this.retained();
            this.ivars().run_loop.queue_closure(move || {
                if !retained.ivars().termination_retry_pending.get() || retained.exiting() {
                    return;
                }
                if retained.ivars().event_handler.in_use()
                    || !retained.ivars().pending_handler_work.borrow().can_run_inline()
                {
                    retained.defer_termination_retry();
                    return;
                }
                // Clear first: `terminate:` synchronously asks this delegate again, now with no
                // active handler, and that second decision must be NSTerminateNow.
                retained.ivars().termination_retry_pending.set(false);
                // SAFETY: `app()` is the live shared `NSApplication`; `terminate:` is
                // `v24@0:8@16`, and nil is the conventional programmatic sender.
                unsafe {
                    send_v_id(app(), sel!(terminate:), Id::NIL);
                }
            });
            this.ivars().run_loop.wakeup();
        });
    }

    /// Closes this backend's windows only after every callback owed before `exit()`.
    fn commit_exit(&self) {
        debug_assert!(self.exiting(), "only a requested exit can be committed");
        debug_assert!(
            self.ivars().pending_handler_work.borrow().can_run_inline(),
            "the exit barrier must release the handler FIFO before closing windows"
        );
        self.deliver_exit();
    }

    /// LOCAL PATCH (aterm): the exit barrier's block runs in the default mode, but while AppKit
    /// waits on a deferred quit's reply it turns the run loop in `NSModalPanelRunLoopMode`
    /// (`terminate.rs`, measured), where that block never runs and only the common-mode observers
    /// do, so a requested exit would never commit and AppKit would wait on its reply for good.
    /// With a reply owed this observer commits it instead, the handler free, after the work the
    /// barrier would have run first ([`Self::run_owed_work`]). That work runs inside the exit,
    /// which ends the process whatever it answers, so a cancel in it is no refusal
    /// (`terminate::refuse`): a NO there would tell macOS the quit was refused while the exit
    /// went on all the same, the windows closed and the run ended once `terminate:` returned
    /// (measured by `objc_window_drive` stage [15j]'s negative control). Only the panic guard
    /// refuses the quit, for a panic in that work, and nothing is delivered after it.
    /// Without a reply owed the barrier owns the exit.
    fn commit_owed_exit(&self) {
        if !terminate::reply_owed() {
            return;
        }
        let _ = terminate::exit_then_proceed(|| {
            if self.run_owed_work() {
                self.close_windows_and_exit();
            }
        });
    }

    /// LOCAL PATCH (aterm): runs the handler work queued before an exit that commits without the
    /// exit barrier's block (AppKit's wait on a deferred quit's reply, `applicationWillTerminate:`),
    /// in order, up to the barrier, each callback contained as the batch block contains it: with
    /// the exit asked for in both callers every enqueue refuses, so nothing joins the batch and
    /// it ends; what is behind the barrier is given up, since the exit that follows ends the
    /// process ([`OwedBatch`]). Answers whether the exit may go on: false once a panic is
    /// pending, which a deeper guard caught (it refused the quit). Inside a batch in progress
    /// nothing runs: that batch owns its items, and it is on the stack beneath AppKit's
    /// `terminate:`, which ends the process before the batch resumes.
    fn run_owed_work(&self) -> bool {
        debug_assert!(self.exiting(), "owed work runs only once the exit is asked for");
        let panicking =
            || self.ivars().panic_info.upgrade().is_some_and(|info| info.is_panicking());
        if !self.ivars().pending_handler_work.borrow_mut().begin_owed() {
            return !panicking();
        }
        let batch = OwedBatch { delegate: self, completed: Cell::new(false) };
        loop {
            if panicking() {
                return false;
            }
            let work = self.ivars().pending_handler_work.borrow_mut().pop_owed();
            match work {
                Some(HandlerWork::Callback(work)) => {
                    let _ = aterm_objc::contain("owed event-handler work", || work(self));
                },
                Some(HandlerWork::ExitBarrier) | None => break,
            }
        }
        let completed = !panicking();
        batch.completed.set(completed);
        completed
    }

    /// The exit committed: each window's `Destroyed` passes from here on, and no `terminate:`
    /// retry is owed.
    fn mark_exit_committed(&self) {
        self.ivars().exit_committed.set(true);
        self.ivars().termination_retry_pending.set(false);
    }

    /// The exit delivered where it ends inside AppKit: committed, the windows closed, then
    /// `LoopExiting`.
    fn close_windows_and_exit(&self) {
        self.mark_exit_committed();
        // SAFETY: `app()` is the live shared `NSApplication`.
        unsafe { notify_windows_of_exit(app()) };
        self.internal_exit();
    }

    /// The committed exit: the windows closed, then `LoopExiting`.
    fn deliver_exit(&self) {
        self.mark_exit_committed();
        let app = app();
        // LOCAL PATCH (aterm): while AppKit waits on a deferred quit's reply its `terminate:` is
        // on the stack, `-[NSApp run]` cannot return and a `-stop:` ends nothing until
        // `terminate:` returns, so the exit is delivered here (the windows closed, then
        // `LoopExiting`, the application's shutdown in it), and AppKit hears YES after it and
        // ends the process (`terminate.rs`). After the panic guard's NO during that exit the
        // run ends once `terminate:` returns, on the stop the guard queued.
        let ended_in_appkit = terminate::exit_then_proceed(|| self.close_windows_and_exit());
        if !ended_in_appkit {
            // Post the stop first so an Objective-C exception during a synchronous close cannot
            // strand a committed exit. `NSApp run` cannot return until this block returns, so
            // each `Destroyed` still runs inline before `pump_events` emits `LoopExiting`.
            // SAFETY: `app` is the live shared `NSApplication`.
            unsafe {
                stop_app_immediately(app);
                notify_windows_of_exit(app);
            }
        }
    }

    fn arm_handler_work(&self) {
        if self.ivars().event_handler.in_use()
            || !self.ivars().pending_handler_work.borrow_mut().arm()
        {
            return;
        }

        let this = self.retained();
        self.ivars().run_loop.queue_closure(move || {
            let batch_len = this
                .ivars()
                .pending_handler_work
                .borrow_mut()
                .begin_batch(this.ivars().event_handler.in_use());
            if batch_len == 0 {
                // A nested modal loop reached this block while another event was being handled.
                // Its HandlerWorkRearm owns rearming once the borrow is released; doing it here
                // could spin inside the nested loop.
                return;
            }
            let panic_info = Weak::clone(&this.ivars().panic_info);
            let reached_exit_barrier = Cell::new(false);
            let completed = stop_app_on_panic(
                this.mtm(),
                Weak::clone(&panic_info),
                AssertUnwindSafe(|| {
                    for _ in 0..batch_len {
                        // A deeper AppKit guard can catch a panic into the same slot and return
                        // normally through this work item. Once that happens, preserve the rest
                        // of the snapshot for the fatal turn instead of running more user code.
                        if panic_info.upgrade().is_some_and(|info| info.is_panicking()) {
                            break;
                        }
                        let work = this.ivars().pending_handler_work.borrow_mut().pop_batch_item();
                        match work {
                            HandlerWork::Callback(work) => {
                                // Contain here, inside the panic guard, so an Objective-C
                                // exception cannot skip the remaining snapshot or strand every
                                // later callback behind it.
                                let _ = aterm_objc::contain("queued event-handler work", || {
                                    work(&this);
                                });
                            },
                            HandlerWork::ExitBarrier => {
                                reached_exit_barrier.set(true);
                                break;
                            },
                        }
                    }
                }),
            );
            let discarded_tail = {
                let mut pending = this.ivars().pending_handler_work.borrow_mut();
                if reached_exit_barrier.get() {
                    Some(pending.finish_batch_and_take_tail())
                } else {
                    pending.finish_batch();
                    None
                }
            };
            drop(discarded_tail);
            if completed.is_some() && !panic_info.upgrade().is_some_and(|info| info.is_panicking())
            {
                if reached_exit_barrier.get() {
                    let _ = stop_app_on_panic(
                        this.mtm(),
                        Weak::clone(&panic_info),
                        AssertUnwindSafe(|| this.commit_exit()),
                    );
                } else {
                    this.arm_handler_work();
                }
            }
        });
        // `CFRunLoopPerformBlock` does not itself wake a run loop that is about to sleep.
        self.ivars().run_loop.wakeup();
    }

    #[track_caller]
    fn handle_event(&self, event: Event<HandlePendingUserEvents>) {
        self.handle_event_with_origin(event, super::key_origin::capture());
    }

    fn handle_event_with_origin(
        &self,
        event: Event<HandlePendingUserEvents>,
        origin: super::key_origin::Captured,
    ) {
        let _rearm = HandlerWorkRearm(self);
        let window_event = matches!(&event, Event::WindowEvent { .. });
        let deliver = || {
            self.ivars()
                .event_handler
                .handle_event(event, &ActiveEventLoop::new_root(self.retained()));
        };
        if window_event {
            origin.dispatch(deliver);
        } else {
            deliver();
        }
    }

    /// Dispatches `NewEvents(Init)` and `Resumed`.
    pub fn dispatch_init_events(&self) {
        self.handle_event(Event::NewEvents(StartCause::Init));
        self.handle_event(Event::Resumed);
    }

    /// LOCAL PATCH (aterm): a nested run loop inside the handler (an app-modal `NSAlert runModal`)
    /// turns the observers with the handler borrowed, so nothing re-arms the waker: stop it.
    fn stop_waker_in_nested_loop(&self) {
        if self.ivars().event_handler.in_use() {
            self.ivars().waker.borrow_mut().stop();
        }
    }

    pub fn wakeup(&self, panic_info: Weak<PanicInfo>) {
        let _ = self.mtm();
        let panic_info = panic_info
            .upgrade()
            .expect("The panic info must exist here. This failure indicates a developer error.");

        if panic_info.is_panicking()
            || !self.ivars().event_handler.ready()
            || !self.is_running()
            || self.exiting()
        {
            self.stop_waker_in_nested_loop();
            return;
        }

        let start = self.ivars().start_time.get().unwrap();
        let cause = match self.control_flow() {
            ControlFlow::Poll => StartCause::Poll,
            ControlFlow::Wait => StartCause::WaitCancelled { start, requested_resume: None },
            ControlFlow::WaitUntil(requested_resume) => {
                if Instant::now() >= requested_resume {
                    StartCause::ResumeTimeReached { start, requested_resume }
                } else {
                    StartCause::WaitCancelled { start, requested_resume: Some(requested_resume) }
                }
            },
        };

        // Match `cleared`: an Objective-C exception in this callback must not skip the pump's
        // ordinary stop. A Rust panic still escapes to the outer panic guard and wins.
        let _ = aterm_objc::contain("CFRunLoop observer: NewEvents", || {
            self.handle_event(Event::NewEvents(cause));
        });

        if self.ivars().stop_after_wait.get() && !self.exiting() {
            // SAFETY: `app()` is the live shared `NSApplication`.
            unsafe { stop_app_immediately(app()) };
        }
    }

    pub fn cleared(&self, panic_info: Weak<PanicInfo>) {
        let _ = self.mtm();
        let panic_info = panic_info
            .upgrade()
            .expect("The panic info must exist here. This failure indicates a developer error.");

        if panic_info.is_panicking() || !self.ivars().event_handler.ready() || !self.is_running() {
            self.stop_waker_in_nested_loop();
            return;
        }
        if self.exiting() {
            self.commit_owed_exit();
            return;
        }

        // LOCAL PATCH (aterm): each dispatch below is contained on its own, so a raise in the
        // application's handling of one still lets the rest of the turn run (redraws,
        // `AboutToWait`, the exit checks, the waker re-arm).
        let _ = aterm_objc::contain("CFRunLoop observer: UserEvent", || {
            self.handle_event(Event::UserEvent(HandlePendingUserEvents));
        });

        let redraw = mem::take(&mut *self.ivars().pending_redraw.borrow_mut());
        let handled = Cell::new(0_usize);
        let contained = aterm_objc::contain("CFRunLoop observer: RedrawRequested", || {
            for window_id in &redraw {
                self.handle_event(Event::WindowEvent {
                    window_id: RootWindowId(*window_id),
                    event: WindowEvent::RedrawRequested,
                });
                handled.set(handled.get() + 1);
            }
        });
        if contained.is_err() {
            let mut pending = self.ivars().pending_redraw.borrow_mut();
            let undelivered = redraw.iter().skip(handled.get() + 1).copied();
            let mut restored: Vec<WindowId> =
                undelivered.filter(|w| !pending.contains(w)).collect();
            restored.append(&mut pending);
            *pending = restored;
        }

        let _ = aterm_objc::contain("CFRunLoop observer: AboutToWait", || {
            self.handle_event(Event::AboutToWait);
        });

        // An exit this turn's handlers asked for: the barrier armed by `exit()` owns closing and
        // stopping, unless AppKit waits on a deferred quit's reply ([`Self::commit_owed_exit`]).
        if self.exiting() {
            self.commit_owed_exit();
            return;
        }

        if self.ivars().stop_before_wait.get() {
            // SAFETY: `app()` is the live shared `NSApplication`.
            unsafe { stop_app_immediately(app()) };
        }
        self.ivars().start_time.set(Some(Instant::now()));
        let wait_timeout = self.ivars().wait_timeout.get();
        let app_timeout = match self.control_flow() {
            ControlFlow::Wait => None,
            ControlFlow::Poll => Some(Instant::now()),
            ControlFlow::WaitUntil(instant) => Some(instant),
        };
        self.ivars().waker.borrow_mut().start_at(min_timeout(wait_timeout, app_timeout));

        // LOCAL PATCH (aterm): a send armed after this turn's drain signalled only once; if a
        // nested run loop handled that signal, nothing would bring the drain it is owed, so wake
        // once more (`wake_queue.rs`, COALESCED WAKES).
        if self.ivars().wake_armed.is_set() {
            self.ivars().run_loop.wakeup();
        }
    }
}

#[derive(Debug)]
pub(crate) struct HandlePendingUserEvents;

fn min_timeout(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    a.map_or(b, |a_timeout| b.map_or(Some(a_timeout), |b_timeout| Some(a_timeout.min(b_timeout))))
}

/// Activates multiple windows created before `applicationDidFinishLaunching:`.
/// # Safety
/// `app` must be the live shared `NSApplication`.
unsafe fn window_activation_hack(app: Id) {
    // LOCAL PATCH (aterm): the array is walked by index, in an explicit pool (`-windows` is +0
    // autoreleased).
    autoreleasepool(|_| {
        // SAFETY: `-windows` is `@16@0:8` on `NSApplication`; `-count` is `Q16@0:8` and
        // `-objectAtIndex:` is `@24@0:8Q16` on `NSArray`, and no index below the count is out of
        // range; `-isVisible` is `B16@0:8` and `-makeKeyAndOrderFront:` is `v24@0:8@16` on
        // `NSWindow`, whose argument is the `nil` sender upstream passed as `None`.
        unsafe {
            let windows = send_id(app, sel!(windows));
            let n = send_usize(windows, sel!(count));
            for i in 0..n {
                let window = send_id_usize(windows, sel!(objectAtIndex:), i);
                if send_bool(window, sel!(isVisible)) {
                    tracing::trace!("Activating visible window");
                    send_v_id(window, sel!(makeKeyAndOrderFront:), Id::NIL);
                } else {
                    tracing::trace!("Skipping activating invisible window");
                }
            }
        }
    });
}

fn app() -> Id {
    // SAFETY: `+sharedApplication` is `@16#0:8` on `NSApplication` and creates the instance on
    // first call.
    unsafe { send_id(class(c"NSApplication").as_id(), sel!(sharedApplication)) }
}

#[cfg(test)]
#[path = "app_state_tests.rs"]
mod tests;
