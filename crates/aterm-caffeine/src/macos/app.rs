// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The menu-bar app: the mug's state and what a click, a menu choice, a tick, a wake from
//! sleep or a write of the state file does to it ([`Mug`], plain Rust), the declared target
//! AppKit sends those to ([`CaffeineTarget`]), and [`run`], which builds both and runs
//! `NSApplication` until Quit or an update restart.
//!
//! Every handler runs on the main thread inside `-[NSApplication run]`, borrows the mug for
//! as long as it computes and never across a call that runs the run loop: the menu's pop-up
//! runs its own tracking loop, during which a choice, a tick, a state-file write or a wake
//! can arrive, so it is shown after the borrow ends.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aterm_objc::send::{
    send_bool_isize, send_id, send_id_idptr_usize,
    send_id_usize_point_usize_f64_isize_id_i16_isize_isize, send_isize, send_usize, send_v,
    send_v_id, send_v_id_bool, send_v_id_sel_id_id, send_v_sel_id_f64,
};
use aterm_objc::{Bool, CGPoint, Id, MainThread, Obj, Sel, autoreleasepool, class, sel};

use super::ServeContext;
use super::awake::Awake;
use super::ui::{self, StatusUi};
use super::watch::Watch;
use crate::launchd;
use crate::menu::{self, Click, MenuAction, Row};
use crate::model::Mode;
use crate::notify::ChangedObserver;
use crate::paths::Paths;
use crate::state;

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

/// `NSApplicationActivationPolicyAccessory`: no Dock icon and no menu bar of its own.
const ACTIVATION_POLICY_ACCESSORY: isize = 1;
/// `NSEventTypeRightMouseUp`.
const EVENT_TYPE_RIGHT_MOUSE_UP: usize = 4;
/// `NSEventTypeApplicationDefined`: the event posted so `-run` sees its stop.
const EVENT_TYPE_APPLICATION_DEFINED: usize = 15;
/// `NSEventModifierFlagControl`.
const MODIFIER_FLAG_CONTROL: usize = 1 << 18;
/// How often the mug looks again (the time left it shows, a deadline, the update watch).
const TICK_SECS: u64 = 60;
/// What `pmset -g assertions` names the activity.
const AWAKE_REASON: &str = "aterm Caffeine is keeping this Mac awake";
/// `NSWorkspaceDidWakeNotification`, spelled as its value (AppKit's symbol holds it).
const DID_WAKE: &str = "NSWorkspaceDidWakeNotification";
/// `NSRunLoopCommonModes`, spelled as its value (Foundation's symbol holds it).
const COMMON_MODES: &str = "kCFRunLoopCommonModes";

/// How the app ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Exit {
    /// Quit: exit 0, which ends the launchd job.
    Quit,
    /// The origin's code changed: [`launchd::EXIT_RESTART`], which launchd answers by
    /// starting the job again, from the new code.
    Restart,
}

impl Exit {
    fn code(self) -> ExitCode {
        match self {
            Self::Quit => ExitCode::SUCCESS,
            Self::Restart => ExitCode::from(launchd::EXIT_RESTART),
        }
    }
}

/// What the target does once the mug has handled something.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Next {
    /// Look again in this many seconds.
    Tick(u64),
    /// End the app.
    Stop(Exit),
}

/// The seconds to the next tick at `now` in `mode`: [`TICK_SECS`], or a second past a
/// deadline closer than that, so the tick that turns the mug off comes just after it.
fn next_tick_secs(mode: Mode, now: u64) -> u64 {
    match mode.remaining(now) {
        Some(left) if left < TICK_SECS => left + 1,
        _ => TICK_SECS,
    }
}

/// What a click with the event `(type, modifier flags)` means; a click with no current
/// event toggles.
fn click_of(event: Option<(usize, usize)>) -> Click {
    let (kind, flags) = event.unwrap_or((0, 0));
    menu::classify_click(
        kind == EVENT_TYPE_RIGHT_MOUSE_UP,
        flags & MODIFIER_FLAG_CONTROL != 0,
    )
}

/// The wall clock in whole seconds.
fn now() -> u64 {
    aterm_time::unix_secs().unwrap_or(0)
}

/// Everything the mug knows and holds.
struct Mug {
    paths: Paths,
    label: String,
    origin: PathBuf,
    version: String,
    /// The mode as last set or read.
    mode: Mode,
    /// The keep-awake activity, held while the mode is on.
    awake: Option<Awake>,
    /// The one activity-acquisition door; tests can refuse without acquiring it.
    awake_begin: fn(&str) -> Option<Awake>,
    /// Whether a replaced origin still has the `caffeine` verb
    /// ([`crate::code::serves_caffeine`]); tests answer without running anything.
    origin_serves: fn(&Path) -> Option<bool>,
    /// The item in the menu bar; `None` before it is installed (and in tests).
    ui: Option<StatusUi>,
    /// The origin's code, watched when the mug restarts on an update.
    watch: Option<Watch>,
    /// How the app ends, once that is decided; nothing else is handled after.
    exit: Option<Exit>,
}

impl Mug {
    /// Run `f` unless how the app ends is already decided (then nothing more is handled),
    /// and record the end `f` decides.
    fn handled(&mut self, f: impl FnOnce(&mut Self) -> Next) -> Option<Next> {
        if self.exit.is_some() {
            return None;
        }
        let next = f(self);
        if let Next::Stop(exit) = next {
            self.exit = Some(exit);
        }
        Some(next)
    }

    /// Publish this mug's mode. A failed write is logged; the caller stops the
    /// mug so status cannot combine an old on file with its still-running pid.
    fn save(&self) -> bool {
        let file = self.paths.state_file();
        match state::save(&file, self.mode) {
            Ok(()) => true,
            Err(err) => {
                aterm_log::warn!(
                    "caffeine: could not save {}: {err}; stopping mug",
                    file.display()
                );
                false
            }
        }
    }

    /// Hold the activity while on and show its actual state. A refused begin
    /// settles to Off and saves it; a later explicit choice may try again.
    /// False means Off could not be published, so the mug must stop.
    fn apply(&mut self, now: u64) -> bool {
        if !self.mode.is_on(now) {
            self.awake = None;
        } else if self.awake.is_none() {
            self.awake = (self.awake_begin)(AWAKE_REASON);
            if self.awake.is_none() {
                aterm_log::warn!("caffeine: the keep-awake activity did not begin; turning off");
                self.mode = Mode::Off;
                if !self.save() {
                    return false;
                }
            }
        }
        let lit = self.awake.is_some();
        let tip = menu::tooltip(self.mode, now);
        if let Some(ui) = self.ui.as_mut() {
            ui.show(lit, &tip);
        }
        true
    }

    /// Apply `mode` and save the resulting state. A refusal has already saved
    /// Off; no writer publishes the refused on mode afterward.
    fn set(&mut self, mode: Mode, now: u64) -> Next {
        let requested = mode.settled(now);
        self.mode = requested;
        if !self.apply(now) || (self.mode == requested && !self.save()) {
            return Next::Stop(Exit::Quit);
        }
        Next::Tick(next_tick_secs(self.mode, now))
    }

    /// A left click: on forever when off, off when on.
    fn toggle(&mut self, now: u64) -> Next {
        self.set(self.mode.toggled(now), now)
    }

    /// A menu choice.
    fn choose(&mut self, action: MenuAction, now: u64) -> Next {
        match action {
            MenuAction::Activate(choice) => self.set(Mode::chosen(choice, now), now),
            MenuAction::ToggleStartup => {
                let on = launchd::startup_enabled(&self.paths, &self.label);
                if let Err(err) = launchd::set_startup(&self.paths, &self.label, &self.origin, !on)
                {
                    let verb = if on { "turn off" } else { "turn on" };
                    aterm_log::warn!("caffeine: could not {verb} Enable at Startup: {err}");
                }
                Next::Tick(next_tick_secs(self.mode, now))
            }
            MenuAction::Quit => {
                let _ = self.set(Mode::Off, now);
                Next::Stop(Exit::Quit)
            }
        }
    }

    /// The tick, and a wake from sleep: a passed deadline turns the mug off (saved), the
    /// tooltip's time left is brought up to date, and a replaced origin ends the app for
    /// launchd to start the new build.
    fn tick(&mut self, now: u64) -> Next {
        let settled = self.mode.settled(now);
        if settled == self.mode {
            if !self.apply(now) {
                return Next::Stop(Exit::Quit);
            }
        } else if let stop @ Next::Stop(_) = self.set(settled, now) {
            return stop;
        }
        if self.watch.as_mut().is_some_and(Watch::replaced) {
            return self.restart_or_retire();
        }
        Next::Tick(next_tick_secs(self.mode, now))
    }

    /// The origin was replaced: end for launchd to start the new build, unless the new build
    /// has no `caffeine` verb (a rollback past it), which would fail every start launchd made
    /// and be started again. Then the job ends for good (exit 0, which its KeepAlive does not
    /// restart) and its Enable at Startup goes, so no login starts it either. Not knowing
    /// restarts, as before.
    fn restart_or_retire(&mut self) -> Next {
        if (self.origin_serves)(&self.origin) != Some(false) {
            return Next::Stop(Exit::Restart);
        }
        aterm_log::warn!(
            "caffeine: {} has no `caffeine` verb now; the mug ends and Enable at Startup is off",
            self.origin.display()
        );
        if launchd::startup_enabled(&self.paths, &self.label)
            && let Err(err) = launchd::set_startup(&self.paths, &self.label, &self.origin, false)
        {
            aterm_log::warn!("caffeine: could not turn off Enable at Startup: {err}");
        }
        Next::Stop(Exit::Quit)
    }

    /// Take the writer's mode. Accepted requests are not saved back; a refused
    /// activation publishes Off through the same path as every other acquisition.
    fn reload(&mut self, now: u64) -> Next {
        self.mode = state::load(&self.paths.state_file(), now);
        if !self.apply(now) {
            return Next::Stop(Exit::Quit);
        }
        Next::Tick(next_tick_secs(self.mode, now))
    }

    /// The menu's rows at `now`.
    fn rows(&self, now: u64) -> Vec<Row> {
        menu::compose(
            self.mode,
            now,
            launchd::startup_enabled(&self.paths, &self.label),
            &self.version,
        )
    }
}

/// The target's Rust state.
struct TargetIvars {
    /// A `RefCell` because AppKit messages the target through `&self`; every handler
    /// `try_borrow_mut`s, so a nested delivery is dropped rather than a panic (which would
    /// abort inside a declared method).
    mug: RefCell<Mug>,
}

aterm_objc::declare_class! {
    /// What the mug's button, its menu items, the ticks, the workspace's wake notification
    /// and the state-file notification send to. Every method but the accessibility action's
    /// is `- (void)x:(id)y`, and each returns at once off the main thread.
    struct CaffeineTarget: NSObject {
        const NAME: &str = "ATermCaffeineTarget";
        type Ivars = TargetIvars;
        protocols: [NSObject];

        /// The button was released: a left click toggles, a right or Control-click opens
        /// the menu.
        @sel(statusClicked:)
        fn status_clicked(&self, _sender: Id) {
            if MainThread::new().is_none() {
                return;
            }
            let now = now();
            match autoreleasepool(|_| current_click()) {
                Click::Toggle => self.handle(|mug| mug.toggle(now)),
                Click::Menu => self.show_menu(now),
            }
        }

        /// The button's [`ui::SHOW_MENU`] accessibility action. It answers YES at once and
        /// the menu opens on the run loop's next turn: the menu's tracking loop holds the
        /// main thread until the menu closes, and the client (VoiceOver) waits on the answer.
        @sel(showMenuAction:)
        fn show_menu_action(&self, _action: Id) -> Bool {
            if MainThread::new().is_none() {
                return Bool::NO;
            }
            // SAFETY: as `schedule_stop`; `openMenu:` is a method the target declares.
            unsafe {
                send_v_sel_id_f64(
                    self.as_id(),
                    sel!(performSelector:withObject:afterDelay:),
                    sel!(openMenu:),
                    Id::NIL,
                    0.0,
                );
            }
            Bool::YES
        }

        /// The delayed perform [`Self::show_menu_action`] scheduled.
        @sel(openMenu:)
        fn open_menu(&self, _arg: Id) {
            if MainThread::new().is_some() {
                self.show_menu(now());
            }
        }

        /// A menu item was chosen; its tag says which. An unknown tag does nothing.
        @sel(menuAction:)
        fn menu_action(&self, sender: Id) {
            if sender.is_null() || MainThread::new().is_none() {
                return;
            }
            // SAFETY: `sender` is the live NSMenuItem AppKit passed; `-tag` is `q16@0:8`.
            let tag = unsafe { send_isize(sender, sel!(tag)) };
            if let Some(action) = MenuAction::from_tag(tag) {
                let now = now();
                self.handle(|mug| mug.choose(action, now));
            }
        }

        /// The delayed perform [`arm_tick`] scheduled.
        @sel(tick:)
        fn tick(&self, _arg: Id) {
            if MainThread::new().is_some() {
                let now = now();
                self.handle(|mug| mug.tick(now));
            }
        }

        /// `NSWorkspaceDidWakeNotification`: the run loop's clock stood still while the
        /// Mac slept, so tick now rather than when the delayed one fires.
        @sel(workspaceDidWake:)
        fn workspace_did_wake(&self, _note: Id) {
            if MainThread::new().is_some() {
                let now = now();
                self.handle(|mug| mug.tick(now));
            }
        }

        /// A writer posted [`crate::notify::CHANGED`].
        @sel(stateChanged:)
        fn state_changed(&self, _arg: Id) {
            if MainThread::new().is_some() {
                let now = now();
                self.handle(|mug| mug.reload(now));
            }
        }

        /// End `-run`: the delayed perform a [`Next::Stop`] scheduled, so it runs from the
        /// main run loop after any menu tracking has ended.
        @sel(stopRun:)
        fn stop_run(&self, _arg: Id) {
            if MainThread::new().is_some() {
                autoreleasepool(|_| stop_app());
            }
        }
    }
}

impl CaffeineTarget {
    /// Run `f` on the mug (unless the app is ending, or the mug is borrowed by a handler
    /// this one is nested in), then arm the next tick or schedule the stop it asks for.
    fn handle(&self, f: impl FnOnce(&mut Mug) -> Next) {
        let next = autoreleasepool(|_| self.ivars().mug.try_borrow_mut().ok()?.handled(f));
        match next {
            Some(Next::Tick(secs)) => arm_tick(self.as_id(), secs),
            Some(Next::Stop(_)) => schedule_stop(self.as_id()),
            None => {}
        }
    }

    /// Bring the menu up to date and pop it up below the button, outside the borrow.
    fn show_menu(&self, now: u64) {
        let popup = autoreleasepool(|_| {
            let mut mug = self.ivars().mug.try_borrow_mut().ok()?;
            if mug.exit.is_some() {
                return None;
            }
            let rows = mug.rows(now);
            let ui = mug.ui.as_mut()?;
            ui.refresh_menu(self.as_id(), sel!(menuAction:), rows);
            Some(ui.popup())
        });
        if let Some(popup) = popup {
            ui::pop_up(&popup);
            autoreleasepool(|_| drop(popup));
        }
    }
}

/// The running application, which [`run`] made before any of this can be sent.
fn shared_app() -> Id {
    // SAFETY: `+sharedApplication` is `@16@0:8`; on the main thread, where `run` already
    // created the app, it answers that instance.
    unsafe { send_id(class(c"NSApplication").as_id(), sel!(sharedApplication)) }
}

/// The click the event being handled is, read off the app's current event.
fn current_click() -> Click {
    let app = shared_app();
    if app.is_null() {
        return click_of(None);
    }
    // SAFETY: `-currentEvent` is `@16@0:8` on the live app and answers the event being
    // dispatched (+0) or nil; on that event `-type` and `-modifierFlags` are both
    // `Q16@0:8`, read only.
    let event = unsafe {
        let event = send_id(app, sel!(currentEvent));
        (!event.is_null()).then(|| {
            (
                send_usize(event, sel!(type)),
                send_usize(event, sel!(modifierFlags)),
            )
        })
    };
    click_of(event)
}

/// `+[NSObject cancelPreviousPerformRequestsWithTarget:selector:object:]`,
/// `v40@0:8@16:24@32`: no helper has this shape.
type CancelFn = unsafe extern "C-unwind" fn(Id, Sel, Id, Sel, Id);

/// `-[NSObject performSelector:withObject:afterDelay:inModes:]`, `v48@0:8:16@24d32@40`: no
/// helper has this shape.
type PerformInModesFn = unsafe extern "C-unwind" fn(Id, Sel, Sel, Id, f64, Id);

/// An `NSArray` holding [`COMMON_MODES`], retained.
fn common_modes() -> Option<Obj> {
    let mode = aterm_objc::ns_string(COMMON_MODES)?;
    let modes = [mode.id()];
    // SAFETY: `+arrayWithObjects:count:` is `@32@0:8r^@16Q24`; it reads `count` live,
    // non-nil objects (the string, alive for the call) and answers the array at +0, which
    // `Obj::retain` keeps past the caller's pool.
    unsafe {
        Obj::retain(send_id_idptr_usize(
            class(c"NSArray").as_id(),
            sel!(arrayWithObjects:count:),
            modes.as_ptr(),
            modes.len(),
        ))
    }
}

/// Tick `target` in `secs` seconds, in place of any tick already scheduled. A delayed
/// perform on the main run loop's common modes, which `NSApplication` makes hold the menu's
/// tracking mode: a tick runs while the menu is open too, so a deadline is kept then (a tick
/// is safe there: no borrow is held across the pop-up, and a stop it decides still waits for
/// the menu to close, [`schedule_stop`]). The perform retains `target` until it fires or is
/// cancelled; should the modes not be made, it falls back to the default mode alone.
fn arm_tick(target: Id, secs: u64) {
    autoreleasepool(|_| {
        let modes = common_modes();
        // SAFETY: main-thread sends on the live target. The cancel is `CancelFn`'s prototype
        // and drops only `tick:` performs in any mode (not a scheduled `stopRun:`); the
        // perform is `PerformInModesFn`'s prototype, its modes array live across the send
        // (the perform keeps its own copy), or `-performSelector:withObject:afterDelay:`,
        // `v40@0:8:16@24d32`; `tick:` is a method the target declares.
        unsafe {
            let cancel: CancelFn = aterm_objc::checked_msg();
            cancel(
                class(c"NSObject").as_id(),
                sel!(cancelPreviousPerformRequestsWithTarget:selector:object:),
                target,
                sel!(tick:),
                Id::NIL,
            );
            if let Some(modes) = &modes {
                let perform: PerformInModesFn = aterm_objc::checked_msg();
                perform(
                    target,
                    sel!(performSelector:withObject:afterDelay:inModes:),
                    sel!(tick:),
                    Id::NIL,
                    secs as f64,
                    modes.id(),
                );
            } else {
                send_v_sel_id_f64(
                    target,
                    sel!(performSelector:withObject:afterDelay:),
                    sel!(tick:),
                    Id::NIL,
                    secs as f64,
                );
            }
        }
    });
}

/// Send `target` `stopRun:` from the main run loop as soon as it next runs its default mode.
fn schedule_stop(target: Id) {
    // SAFETY: as `arm_tick`; `stopRun:` is a method the target declares.
    unsafe {
        send_v_sel_id_f64(
            target,
            sel!(performSelector:withObject:afterDelay:),
            sel!(stopRun:),
            Id::NIL,
            0.0,
        );
    }
}

/// Drop every delayed perform scheduled on `target`, and the retains they hold.
fn cancel_performs(target: Id) {
    // SAFETY: `+cancelPreviousPerformRequestsWithTarget:` is `v24@0:8@16`.
    unsafe {
        send_v_id(
            class(c"NSObject").as_id(),
            sel!(cancelPreviousPerformRequestsWithTarget:),
            target,
        );
    }
}

/// `[NSApp stop:nil]`, and an application-defined event posted at the front of the queue:
/// `-run` looks at its stop only after it has dispatched an event, and a delayed perform
/// is not one.
fn stop_app() {
    let app = shared_app();
    if app.is_null() {
        return;
    }
    // SAFETY: main-thread sends on the live app. `-stop:` is `v24@0:8@16` (the nil sender);
    // `+otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:
    // data1:data2:` is `@92@0:8Q16{CGPoint=dd}24Q40d48q56@64s72q76q84`, the helper's
    // prototype, answering the event at +0, retained across `-postEvent:atStart:`
    // (`v28@0:8@16B24`).
    unsafe {
        send_v_id(app, sel!(stop:), Id::NIL);
        let raw = send_id_usize_point_usize_f64_isize_id_i16_isize_isize(
            class(c"NSEvent").as_id(),
            sel!(otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:),
            EVENT_TYPE_APPLICATION_DEFINED,
            CGPoint { x: 0.0, y: 0.0 },
            0,
            0.0,
            0,
            Id::NIL,
            0,
            0,
            0,
        );
        if let Some(event) = Obj::retain(raw) {
            send_v_id_bool(app, sel!(postEvent:atStart:), event.id(), true);
        }
    }
}

/// The workspace's notification center (+0), where its wake notification is posted.
fn workspace_center() -> Id {
    // SAFETY: `+sharedWorkspace` and `-notificationCenter` are `@16@0:8`, main thread.
    unsafe {
        let workspace = send_id(class(c"NSWorkspace").as_id(), sel!(sharedWorkspace));
        if workspace.is_null() {
            return Id::NIL;
        }
        send_id(workspace, sel!(notificationCenter))
    }
}

/// Send `target` `workspaceDidWake:` when the Mac wakes; whether it was registered. The
/// center holds `target` unretained: [`stop_observing_wake`] removes it before it goes.
fn observe_wake(target: Id) -> bool {
    let center = workspace_center();
    let Some(name) = aterm_objc::ns_string(DID_WAKE) else {
        return false;
    };
    if center.is_null() {
        return false;
    }
    // SAFETY: `-addObserver:selector:name:object:` is `v48@0:8@16:24@32@40`; the selector
    // is one the target declares; a nil object is "from any sender".
    unsafe {
        send_v_id_sel_id_id(
            center,
            sel!(addObserver:selector:name:object:),
            target,
            sel!(workspaceDidWake:),
            name.id(),
            Id::NIL,
        );
    }
    true
}

/// Undo [`observe_wake`].
fn stop_observing_wake(target: Id) {
    let center = workspace_center();
    if !center.is_null() {
        // SAFETY: `-removeObserver:` is `v24@0:8@16`.
        unsafe { send_v_id(center, sel!(removeObserver:), target) };
    }
}

/// The one stderr line of a mug that will not run, and exit 0: launchd's KeepAlive starts the
/// job again on any other status, only for it to refuse the same way.
fn refuse(why: &str) -> ExitCode {
    eprintln!("aterm caffeine: {why}");
    ExitCode::SUCCESS
}

/// Run the menu-bar mug until Quit (exit 0) or an update restart
/// ([`launchd::EXIT_RESTART`]); on the process main thread only.
pub(super) fn run(ctx: ServeContext) -> ExitCode {
    let Some(main) = MainThread::new() else {
        return refuse("the menu-bar mug runs only on the process main thread");
    };
    let ServeContext {
        paths,
        label,
        origin,
        version,
        restart_on_update,
        instance,
    } = ctx;
    let watch = restart_on_update.then(|| Watch::start(origin.clone()));
    let code_dir = paths.code_dir();
    let mug = Mug {
        // Read in `run_app`, once a writer's change can no longer be missed.
        mode: Mode::Off,
        paths,
        label,
        origin,
        version,
        awake: None,
        awake_begin: Awake::begin,
        origin_serves: crate::code::serves_caffeine,
        ui: None,
        watch,
        exit: None,
    };
    let code = autoreleasepool(|_| run_app(main, mug));
    // A mug that ends for good takes its copy of the app with it, while it still holds the
    // lock (an update restart runs from a fresh copy anyway, made by the next holder).
    if code == ExitCode::SUCCESS
        && crate::code::runs_from_copy(&code_dir)
        && let Err(e) = crate::code::discard_copies(&code_dir)
    {
        eprintln!("aterm caffeine: {}: {e}", code_dir.display());
    }
    // The one-mug lock is let go only now, with the item off the bar and the activity
    // ended: a second mug never shares the bar with this one.
    drop(instance);
    code
}

/// [`run`]'s AppKit half.
fn run_app(main: MainThread, mug: Mug) -> ExitCode {
    let app = shared_app();
    if app.is_null() {
        return refuse("AppKit gave no application");
    }
    // SAFETY: `-setActivationPolicy:` is `B24@0:8q16`; set before the app runs or shows
    // anything, so it never takes a Dock icon. Its answer (whether the policy took) changes
    // nothing here.
    let _ =
        unsafe { send_bool_isize(app, sel!(setActivationPolicy:), ACTIVATION_POLICY_ACCESSORY) };
    let Some(target) = CaffeineTarget::alloc_init(
        main,
        TargetIvars {
            mug: RefCell::new(mug),
        },
    ) else {
        return refuse("the runtime refused the mug's target");
    };
    // Registered before the mode is read: a writer's post from here on reaches the observer
    // (queued until `-run`), so a change saved while the mug starts is never missed.
    let changed = {
        let target = target.clone_retained();
        // SAFETY (in the hook): `stateChanged:` is `v@:@` on the live target it retains.
        ChangedObserver::register(move || unsafe {
            send_v_id(target.as_id(), sel!(stateChanged:), Id::NIL);
        })
    };
    if changed.is_none() {
        aterm_log::warn!(
            "caffeine: not watching the state file; `aterm caffeine on|off` waits for a restart"
        );
    }
    let now = now();
    let first_tick = {
        let Ok(mut mug) = target.ivars().mug.try_borrow_mut() else {
            return ExitCode::FAILURE;
        };
        mug.mode = state::load(&mug.paths.state_file(), now);
        let rows = mug.rows(now);
        mug.ui = ui::install(
            target.as_id(),
            sel!(statusClicked:),
            sel!(showMenuAction:),
            sel!(menuAction:),
            rows,
        );
        if mug.ui.is_none() {
            // Exit 0, so launchd does not start it again only to fail the same way.
            eprintln!("aterm caffeine: the menu bar refused the mug");
            return ExitCode::SUCCESS;
        }
        if !mug.apply(now) {
            return Exit::Quit.code();
        }
        next_tick_secs(mug.mode, now)
    };
    let woke = observe_wake(target.as_id());
    arm_tick(target.as_id(), first_tick);
    // SAFETY: `-run` is `v16@0:8` on the live app, on the main thread; it returns after
    // `stopRun:` (`stop_app`).
    unsafe { send_v(app, sel!(run)) };
    // Nothing reaches the target after this: no perform, no notification.
    cancel_performs(target.as_id());
    if woke {
        stop_observing_wake(target.as_id());
    }
    drop(changed);
    let exit = match target.ivars().mug.try_borrow_mut() {
        Ok(mut mug) => {
            // Off the bar, and the activity ended (Quit has turned it off already).
            mug.ui = None;
            mug.awake = None;
            mug.exit
        }
        Err(_) => None,
    };
    exit.unwrap_or(Exit::Quit).code()
}

#[cfg(test)]
mod tests {
    use aterm_objc::{ClassType, Encode, class_name, method_types};

    use super::*;
    use crate::model::{Choice, HOUR};

    const NOW: u64 = 1_790_000_000;

    #[test]
    fn a_tick_comes_every_minute_or_just_after_a_closer_deadline() {
        assert_eq!(next_tick_secs(Mode::Off, NOW), TICK_SECS);
        assert_eq!(next_tick_secs(Mode::Forever, NOW), TICK_SECS);
        let hour = Mode::chosen(Choice::Hours1, NOW);
        assert_eq!(next_tick_secs(hour, NOW), TICK_SECS);
        assert_eq!(next_tick_secs(hour, NOW + HOUR - 60), TICK_SECS);
        assert_eq!(next_tick_secs(hour, NOW + HOUR - 59), 60);
        assert_eq!(next_tick_secs(hour, NOW + HOUR - 30), 31);
        assert_eq!(next_tick_secs(hour, NOW + HOUR - 1), 2);
        // Past the deadline: settled off, the plain tick.
        assert_eq!(next_tick_secs(hour, NOW + HOUR), TICK_SECS);
        // From any second, ticking on lands the first tick at or past the deadline at most
        // a second past it, and that tick reads the mode off.
        let deadline = NOW + HOUR;
        for start in NOW..deadline {
            let mut at = start;
            while at < deadline {
                at += next_tick_secs(hour, at);
            }
            assert!(at <= deadline + 1, "from {start}: {at}");
            assert_eq!(hour.settled(at), Mode::Off);
        }
    }

    #[test]
    fn a_right_release_or_a_control_click_opens_the_menu() {
        const LEFT_UP: usize = 2;
        assert_eq!(click_of(Some((LEFT_UP, 0))), Click::Toggle);
        assert_eq!(click_of(Some((EVENT_TYPE_RIGHT_MOUSE_UP, 0))), Click::Menu);
        assert_eq!(
            click_of(Some((LEFT_UP, MODIFIER_FLAG_CONTROL))),
            Click::Menu
        );
        assert_eq!(
            click_of(Some((EVENT_TYPE_RIGHT_MOUSE_UP, MODIFIER_FLAG_CONTROL))),
            Click::Menu
        );
        // Other modifiers (Shift, Option, Command) leave a left click a toggle.
        let others = (1 << 17) | (1 << 19) | (1 << 20);
        assert_eq!(click_of(Some((LEFT_UP, others))), Click::Toggle);
        assert_eq!(click_of(None), Click::Toggle);
    }

    #[test]
    fn the_exit_codes_are_launchds() {
        assert_eq!(Exit::Quit.code(), ExitCode::SUCCESS);
        assert_eq!(Exit::Restart.code(), ExitCode::from(launchd::EXIT_RESTART));
    }

    fn scratch_mug(dir: &std::path::Path) -> Mug {
        Mug {
            paths: Paths {
                state_dir: dir.join("state"),
                home: dir.join("home"),
                logs_dir: dir.join("logs"),
            },
            label: "com.aterm.aterm.caffeine.test".into(),
            origin: dir.join("aterm"),
            version: "0.0.0".into(),
            mode: Mode::Off,
            awake: None,
            awake_begin: Awake::begin,
            origin_serves: |_| Some(true),
            ui: None,
            watch: None,
            exit: None,
        }
    }

    fn saved(mug: &Mug) -> Option<Mode> {
        state::read(&mug.paths.state_file()).map(|s| s.mode)
    }

    #[test]
    fn a_click_keeps_the_mac_awake_until_the_next_and_saves_each() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let mut mug = scratch_mug(dir.path());
        assert_eq!(mug.toggle(NOW), Next::Tick(TICK_SECS));
        assert_eq!(mug.mode, Mode::Forever);
        assert!(mug.awake.is_some());
        assert_eq!(saved(&mug), Some(Mode::Forever));
        assert_eq!(mug.toggle(NOW + 1), Next::Tick(TICK_SECS));
        assert_eq!(mug.mode, Mode::Off);
        assert!(mug.awake.is_none());
        assert_eq!(saved(&mug), Some(Mode::Off));
    }

    #[test]
    fn a_refused_activity_is_off_in_the_mug_file_tooltip_and_status() {
        for requested in [Mode::Forever, Mode::activate(Some(HOUR), NOW)] {
            let dir = aterm_tempfile::tempdir().expect("tempdir");
            let mut mug = scratch_mug(dir.path());
            mug.awake_begin = |_| None;
            assert_eq!(mug.set(requested, NOW), Next::Tick(TICK_SECS));
            assert_eq!(mug.mode, Mode::Off, "no assertion must never appear on");
            assert!(mug.awake.is_none());
            assert_eq!(saved(&mug), Some(Mode::Off));
            assert!(
                crate::cli::status_text(mug.mode, NOW, Some(42), false, "l.x")
                    .starts_with("caffeine=off\nuntil=-\n")
            );
            mug.awake_begin = |_| panic!("a refused request does not retry without a new choice");
            assert_eq!(mug.tick(NOW + TICK_SECS), Next::Tick(TICK_SECS));
        }
        // The same refusal path covers a saved request at startup/reload.
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let mut mug = scratch_mug(dir.path());
        state::save(&mug.paths.state_file(), Mode::Forever).expect("saved request");
        mug.mode = Mode::Forever;
        mug.awake_begin = |_| None;
        assert!(mug.apply(NOW));
        assert_eq!(saved(&mug), Some(Mode::Off));
        // A failed Off publication must not leave a live mug validating stale state.
        let blocked = dir.path().join("not-a-directory");
        std::fs::write(&blocked, b"blocked").expect("blocked state directory");
        mug.paths.state_dir = blocked;
        assert_eq!(mug.handled(|m| m.toggle(NOW)), Some(Next::Stop(Exit::Quit)));
        assert!(mug.awake.is_none());
        // A successful acquisition followed by an On-save failure must stop too.
        let mut mug = scratch_mug(dir.path());
        mug.paths.state_dir = dir.path().join("not-a-directory");
        const REASON: &str = "aterm caffeine On-save refusal test";
        mug.awake_begin = |_| Awake::begin(REASON);
        assert_eq!(mug.handled(|m| m.toggle(NOW)), Some(Next::Stop(Exit::Quit)));
        assert!(
            mug.awake.is_some(),
            "the successful acquisition reached the On-save branch"
        );
        assert!(!super::super::awake::tests::assertions_named(REASON).is_empty());
        assert_eq!(mug.handled(|_| panic!("exit is already decided")), None);
        drop(mug);
        assert!(
            super::super::awake::tests::assertions_named(REASON).is_empty(),
            "teardown releases the acquired activity"
        );
    }

    #[test]
    fn a_timed_choice_turns_off_at_its_tick_and_quit_turns_off_and_stops() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let mut mug = scratch_mug(dir.path());
        let hour = Mode::chosen(Choice::Hours1, NOW);
        assert_eq!(
            mug.choose(MenuAction::Activate(Choice::Hours1), NOW),
            Next::Tick(TICK_SECS)
        );
        assert_eq!(mug.mode, hour);
        assert_eq!(saved(&mug), Some(hour));
        assert!(mug.awake.is_some());
        // A tick before the deadline changes nothing and lands just after it.
        assert_eq!(mug.tick(NOW + HOUR - 30), Next::Tick(31));
        assert_eq!(mug.mode, hour);
        // The tick past it: off, saved, the activity ended.
        assert_eq!(mug.tick(NOW + HOUR + 1), Next::Tick(TICK_SECS));
        assert_eq!(mug.mode, Mode::Off);
        assert_eq!(saved(&mug), Some(Mode::Off));
        assert!(mug.awake.is_none());
        // Quit from on: off and saved, then the app ends with exit 0.
        let _ = mug.choose(MenuAction::Activate(Choice::Forever), NOW);
        assert!(mug.awake.is_some());
        assert_eq!(mug.choose(MenuAction::Quit, NOW), Next::Stop(Exit::Quit));
        assert_eq!(saved(&mug), Some(Mode::Off));
        assert!(mug.awake.is_none());
    }

    /// Once Quit or a restart is decided, nothing else is handled: a click, a choice, a tick
    /// or a writer's change arriving before `-run` returns (the stop is a delayed perform)
    /// neither changes the mode nor saves it, nor re-decides the exit.
    #[test]
    fn nothing_is_handled_once_the_end_is_decided() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let mut mug = scratch_mug(dir.path());
        assert_eq!(mug.handled(|m| m.toggle(NOW)), Some(Next::Tick(TICK_SECS)));
        assert_eq!(mug.exit, None);
        assert_eq!(
            mug.handled(|m| m.choose(MenuAction::Quit, NOW)),
            Some(Next::Stop(Exit::Quit))
        );
        assert_eq!(mug.exit, Some(Exit::Quit));
        let file = mug.paths.state_file();
        let before = std::fs::read(&file).expect("read");
        assert_eq!(saved(&mug), Some(Mode::Off));
        assert_eq!(mug.handled(|m| m.toggle(NOW + 1)), None);
        assert_eq!(
            mug.handled(|m| m.choose(MenuAction::Activate(Choice::Hours8), NOW + 1)),
            None
        );
        assert_eq!(mug.handled(|m| m.tick(NOW + 2)), None);
        assert_eq!(mug.handled(|m| m.reload(NOW + 3)), None);
        assert_eq!(mug.mode, Mode::Off);
        assert!(mug.awake.is_none());
        assert_eq!(std::fs::read(&file).expect("read"), before);
        assert_eq!(mug.exit, Some(Exit::Quit));
        // A restart decided by a tick is recorded the same way.
        let mut mug = scratch_mug(dir.path());
        std::fs::write(&mug.origin, b"build 1").expect("exe");
        mug.watch = Some(Watch::start(mug.origin.clone()));
        std::fs::write(&mug.origin, b"build two").expect("exe");
        assert_eq!(
            mug.handled(|m| m.tick(NOW)),
            Some(Next::Stop(Exit::Restart))
        );
        assert_eq!(mug.exit, Some(Exit::Restart));
        assert_eq!(mug.handled(|m| m.choose(MenuAction::Quit, NOW)), None);
        assert_eq!(mug.exit, Some(Exit::Restart));
    }

    #[test]
    fn a_writers_change_is_read_and_not_saved_back() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let mut mug = scratch_mug(dir.path());
        let file = mug.paths.state_file();
        state::save(&file, Mode::Forever).expect("save");
        let before = std::fs::read(&file).expect("read");
        assert_eq!(mug.reload(NOW), Next::Tick(TICK_SECS));
        // What the file restores to in this boot (off where the boot time is unknown).
        assert_eq!(mug.mode, state::load(&file, NOW));
        assert_eq!(mug.awake.is_some(), mug.mode.is_on(NOW));
        assert_eq!(std::fs::read(&file).expect("read"), before);
    }

    /// Enable at Startup flips the login plist under the mug's home (scratch here; nothing
    /// is loaded into launchd), leaves the mode alone, and its checkmark follows the file.
    #[test]
    fn enable_at_startup_writes_and_removes_the_login_plist() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let mut mug = scratch_mug(dir.path());
        let plist = mug.paths.startup_plist(&mug.label);
        let checked = |mug: &Mug| {
            mug.rows(NOW).iter().any(|row| {
                matches!(
                    row,
                    Row::Action {
                        action: MenuAction::ToggleStartup,
                        checked: true,
                        ..
                    }
                )
            })
        };
        assert!(!checked(&mug));
        assert_eq!(
            mug.choose(MenuAction::ToggleStartup, NOW),
            Next::Tick(TICK_SECS)
        );
        let text = std::fs::read_to_string(&plist).expect("the login plist");
        assert!(
            text.contains(&format!("<string>{}</string>", mug.origin.display())),
            "{text}"
        );
        assert!(checked(&mug));
        assert_eq!(mug.mode, Mode::Off, "the mode is not touched");
        assert_eq!(saved(&mug), None, "nor saved");
        let _ = mug.choose(MenuAction::ToggleStartup, NOW);
        assert!(!plist.exists());
        assert!(!checked(&mug));
    }

    #[test]
    fn a_replaced_origin_ends_the_app_for_a_restart() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let mut mug = scratch_mug(dir.path());
        std::fs::write(&mug.origin, b"build 1").expect("exe");
        mug.watch = Some(Watch::start(mug.origin.clone()));
        assert_eq!(mug.tick(NOW), Next::Tick(TICK_SECS));
        std::fs::write(&mug.origin, b"build two").expect("exe");
        assert_eq!(mug.tick(NOW + 60), Next::Stop(Exit::Restart));
        // Not knowing whether the new build serves restarts too.
        mug.origin_serves = |_| None;
        assert_eq!(mug.restart_or_retire(), Next::Stop(Exit::Restart));
    }

    /// A rollback to a build without the verb: the job ends for good (exit 0, which launchd
    /// does not restart) and takes its login item with it.
    #[test]
    fn a_replacement_without_the_verb_ends_the_job_and_its_login_item() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let mut mug = scratch_mug(dir.path());
        std::fs::write(&mug.origin, b"build 1").expect("exe");
        launchd::set_startup(&mug.paths, &mug.label, &mug.origin, true).expect("startup on");
        assert!(launchd::startup_enabled(&mug.paths, &mug.label));
        mug.watch = Some(Watch::start(mug.origin.clone()));
        mug.origin_serves = |_| Some(false);
        assert_eq!(mug.tick(NOW), Next::Tick(TICK_SECS));
        std::fs::write(&mug.origin, b"an older build").expect("exe");
        assert_eq!(mug.tick(NOW + 60), Next::Stop(Exit::Quit));
        assert!(!launchd::startup_enabled(&mug.paths, &mug.label));
    }

    /// The registered class is what the runtime reports: its name, its superclass, the
    /// `NSObject` protocol, and every selector AppKit and the observers send it.
    #[test]
    fn the_registered_class_is_what_the_runtime_reports() {
        let cls = CaffeineTarget::class();
        assert!(!cls.is_null());
        assert_eq!(class(c"ATermCaffeineTarget"), cls);
        // SAFETY: `cls` is the class this module registered: it and its superclass are
        // live, immortal class objects; the queries are side-effect free.
        unsafe {
            assert_eq!(class_name(cls), c"ATermCaffeineTarget");
            assert_eq!(class_name(aterm_objc::superclass_of(cls)), c"NSObject");
            let proto = aterm_objc::protocol(c"NSObject");
            assert!(!proto.is_null());
            assert!(aterm_objc::send::send_bool_id(
                cls.as_id(),
                sel!(conformsToProtocol:),
                Id::from_ptr(proto.as_ptr()),
            ));
            for sel in [
                sel!(statusClicked:),
                sel!(openMenu:),
                sel!(menuAction:),
                sel!(tick:),
                sel!(workspaceDidWake:),
                sel!(stateChanged:),
                sel!(stopRun:),
            ] {
                assert!(
                    aterm_objc::send::send_bool_sel(
                        cls.as_id(),
                        sel!(instancesRespondToSelector:),
                        sel
                    ),
                    "{sel:?}"
                );
                // Every one is `- (void)x:(id)y`: what a button's or a menu item's action,
                // a delayed perform and a notification observer are sent as.
                assert_eq!(method_types(cls, sel).as_deref(), Some("v@:@"), "{sel:?}");
            }
            // The accessibility action's: `- (BOOL)x:(NSAccessibilityCustomAction *)action`.
            assert_eq!(
                method_types(cls, sel!(showMenuAction:)).as_deref(),
                Some(format!("{}@:@", <Bool as Encode>::ENCODING).as_str())
            );
            assert_eq!(method_types(cls, sel!(dealloc)).as_deref(), Some("v@:"));
        }
    }

    /// `ret@:args`, the offset-free encoding a prototype has.
    fn types(ret: &str, args: &[&str]) -> String {
        format!("{ret}@:{}", args.concat())
    }

    /// Every prototype this file sends with, against what the runtime says the selector
    /// takes on the class it is sent to (a class method's on the metaclass).
    #[test]
    fn every_send_here_has_the_prototype_the_runtime_reports() {
        use aterm_objc::{Bool, Encode};
        let (v, id, b, sel_, q, uq, d, s, pt) = (
            <() as Encode>::ENCODING,
            Id::ENCODING,
            Bool::ENCODING,
            Sel::ENCODING,
            isize::ENCODING,
            usize::ENCODING,
            f64::ENCODING,
            i16::ENCODING,
            CGPoint::ENCODING,
        );
        let rows: &[(&'static std::ffi::CStr, Sel, bool, String)] = &[
            (
                c"NSApplication",
                sel!(sharedApplication),
                true,
                types(id, &[]),
            ),
            (
                c"NSApplication",
                sel!(setActivationPolicy:),
                false,
                types(b, &[q]),
            ),
            (c"NSApplication", sel!(currentEvent), false, types(id, &[])),
            (c"NSApplication", sel!(run), false, types(v, &[])),
            (c"NSApplication", sel!(stop:), false, types(v, &[id])),
            (
                c"NSApplication",
                sel!(postEvent:atStart:),
                false,
                types(v, &[id, b]),
            ),
            (
                c"NSEvent",
                sel!(otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:),
                true,
                types(id, &[uq, pt, uq, d, q, id, s, q, q]),
            ),
            (c"NSEvent", sel!(type), false, types(uq, &[])),
            (c"NSEvent", sel!(modifierFlags), false, types(uq, &[])),
            (c"NSMenuItem", sel!(tag), false, types(q, &[])),
            (c"NSWorkspace", sel!(sharedWorkspace), true, types(id, &[])),
            (
                c"NSWorkspace",
                sel!(notificationCenter),
                false,
                types(id, &[]),
            ),
            (
                c"NSNotificationCenter",
                sel!(addObserver:selector:name:object:),
                false,
                types(v, &[id, sel_, id, id]),
            ),
            (
                c"NSNotificationCenter",
                sel!(removeObserver:),
                false,
                types(v, &[id]),
            ),
            (
                c"NSObject",
                sel!(performSelector:withObject:afterDelay:),
                false,
                types(v, &[sel_, id, d]),
            ),
            (
                c"NSObject",
                sel!(performSelector:withObject:afterDelay:inModes:),
                false,
                types(v, &[sel_, id, d, id]),
            ),
            (
                c"NSArray",
                sel!(arrayWithObjects:count:),
                true,
                // `const id *`: the runtime keeps the `r` qualifier.
                types(id, &["r^@", uq]),
            ),
            (
                c"NSObject",
                sel!(cancelPreviousPerformRequestsWithTarget:selector:object:),
                true,
                types(v, &[id, sel_, id]),
            ),
            (
                c"NSObject",
                sel!(cancelPreviousPerformRequestsWithTarget:),
                true,
                types(v, &[id]),
            ),
            (c"NSProcessInfo", sel!(processInfo), true, types(id, &[])),
            (
                c"NSProcessInfo",
                sel!(beginActivityWithOptions:reason:),
                false,
                types(id, &[uq, id]),
            ),
            (c"NSProcessInfo", sel!(endActivity:), false, types(v, &[id])),
        ];
        for (name, sel, class_method, want) in rows {
            let cls = class(name);
            assert!(!cls.is_null(), "{name:?}");
            // SAFETY: `cls` is a live, registered class, and so is its metaclass; reading a
            // method's encoding has no side effects.
            let got = unsafe {
                let cls = if *class_method {
                    aterm_objc::class_of(cls.as_id())
                } else {
                    cls
                };
                method_types(cls, *sel).map(|t| aterm_objc::strip_method_offsets(&t))
            };
            assert_eq!(got.as_deref(), Some(want.as_str()), "{name:?} {sel:?}");
        }
    }
}
