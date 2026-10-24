// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The menu-bar app.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aterm_objc::send::{
    send_bool_isize, send_f64, send_id, send_id_idptr_usize,
    send_id_usize_point_usize_f64_isize_id_i16_isize_isize, send_isize, send_u32, send_usize,
    send_v, send_v_id, send_v_id_bool, send_v_id_sel_id_id, send_v_sel_id_f64,
};
use aterm_objc::{Bool, CGPoint, Id, MainThread, Obj, Retained, Sel, autoreleasepool, class, sel};

use super::ServeContext;
use super::awake::{AWAKE_REASON, Awake};
use super::bridge;
use super::login_items;
use super::term;
use super::ui::{self, StatusUi};
use super::watch::Watch;
use crate::launchd::{self, Startup, StartupDoors};
use crate::menu::{self, Click, MenuAction, Row};
use crate::model::{Clocks, Mode};
use crate::notify::ChangedObserver;
use crate::paths::Paths;
use crate::state;

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

/// `NSApplicationActivationPolicyAccessory`.
const ACTIVATION_POLICY_ACCESSORY: isize = 1;
/// `NSEventTypeRightMouseUp`.
const EVENT_TYPE_RIGHT_MOUSE_UP: usize = 4;
/// `NSEventTypeApplicationDefined`.
const EVENT_TYPE_APPLICATION_DEFINED: usize = 15;
/// `NSEventModifierFlagControl`.
const MODIFIER_FLAG_CONTROL: usize = 1 << 18;
/// `NSControlStateValueOn`, a checked menu item's `-state`.
const CONTROL_STATE_ON: isize = 1;
/// How often the mug looks again (the time left it shows, a deadline, the update watch).
const TICK_SECS: u64 = 60;
/// `NSTerminateCancel`.
const TERMINATE_CANCEL: usize = 0;
/// `NSTerminateNow`.
const TERMINATE_NOW: usize = 1;
/// `keyAEQuitReason`, `'why?'`.
const KEY_QUIT_REASON: u32 = u32::from_be_bytes(*b"why?");
/// The quit reasons that are the system's.
const SYSTEM_REASONS: [[u8; 4]; 6] = [*b"logo", *b"rlgo", *b"rrst", *b"rest", *b"rsdn", *b"shut"];
/// `NSWorkspaceDidWakeNotification`, spelled as its value (AppKit's symbol holds it).
const DID_WAKE: &str = "NSWorkspaceDidWakeNotification";
/// `NSRunLoopCommonModes`, spelled as its value (Foundation's symbol holds it).
const COMMON_MODES: &str = "kCFRunLoopCommonModes";

/// How the app ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Exit {
    /// Quit: exit 0, which ends the launchd job.
    Quit,
    /// The origin's code changed ([`launchd::EXIT_RESTART`]).
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

/// The seconds to the next tick at `now` in `mode` ([`TICK_SECS`]).
const fn next_tick_secs(mode: Mode, now: Clocks) -> u64 {
    match mode.remaining(now) {
        Some(left) if left < TICK_SECS => left + 1,
        _ => TICK_SECS,
    }
}

/// What a click with the event `(type, modifier flags)` means.
fn click_of(event: Option<(usize, usize)>) -> Click {
    let (kind, flags) = event.unwrap_or((0, 0));
    menu::classify_click(
        kind == EVENT_TYPE_RIGHT_MOUSE_UP,
        flags & MODIFIER_FLAG_CONTROL != 0,
    )
}

/// An event as [`click_from`] tells one from another.
#[derive(Clone, Copy, Debug, PartialEq)]
struct EventKey {
    kind: usize,
    flags: usize,
    at: f64,
}

/// What a press of the button means when the app's current event is `event`.
fn click_from(event: Option<EventKey>, stale: Option<EventKey>) -> Click {
    let fresh = event.filter(|event| Some(*event) != stale);
    click_of(fresh.map(|event| (event.kind, event.flags)))
}

/// Both clocks, read now ([`state::clocks`]).
fn now() -> Clocks {
    state::clocks().unwrap_or_default()
}

/// Whether `save`, of `file`, was made; a failure is logged, and the mug stops on it.
fn saved(file: &Path, save: std::io::Result<()>) -> bool {
    match save {
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
    /// The one activity-acquisition door.
    awake_begin: fn(&str) -> Option<Awake>,
    /// Restore saved state, including any corrective save-back.
    take_saved: fn(&Path, Clocks) -> std::io::Result<Mode>,
    /// Whether a replaced origin still has the `caffeine` verb ([`crate::code::serves_caffeine`]).
    origin_serves: fn(&Path) -> Option<bool>,
    /// Start a bridge that holds the Mac awake while the mug restarts ([`bridge::start`]).
    bridge_start: fn() -> bool,
    /// Startup login-plist doors ([`super::STARTUP_DOORS`]).
    doors: StartupDoors,
    /// Open System Settings at Login Items ([`login_items::open_settings`]).
    open_login_items: fn(),
    /// The item in the menu bar.
    ui: Option<StatusUi>,
    /// The origin's code, watched when the mug restarts on an update.
    watch: Option<Watch>,
    /// How the app ends, once that is decided.
    exit: Option<Exit>,
}

impl Mug {
    /// Run `f` unless how the app ends is already decided.
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

    /// [`Mug::handled`] for what the person does (a click, a menu choice).
    fn chosen(&mut self, f: impl FnOnce(&mut Self) -> Next) -> Option<Next> {
        if self.exit != Some(Exit::Restart) {
            return self.handled(f);
        }
        if f(self) == Next::Stop(Exit::Quit) {
            self.exit = Some(Exit::Quit);
        }
        None
    }

    /// Publish this mug's mode, encoded at `now`.
    fn save(&self, now: Clocks) -> bool {
        let file = self.paths.state_file();
        saved(&file, state::save_at(&file, self.mode, now))
    }

    /// Hold the activity while on and show its actual state.
    fn apply(&mut self, now: Clocks) -> bool {
        if !self.mode.is_on(now) {
            self.awake = None;
        } else if self.awake.is_none() {
            self.awake = (self.awake_begin)(AWAKE_REASON);
            if self.awake.is_none() {
                aterm_log::warn!("caffeine: the keep-awake activity did not begin; turning off");
                self.mode = Mode::Off;
                if !self.save(now) {
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

    /// Apply `mode` and save the resulting state.
    fn set(&mut self, mode: Mode, now: Clocks) -> Next {
        let requested = mode.settled(now);
        self.mode = requested;
        if !self.apply(now) || (self.mode == requested && !self.save(now)) {
            return Next::Stop(Exit::Quit);
        }
        Next::Tick(next_tick_secs(self.mode, now))
    }

    /// A left click: on forever when off, off when on.
    fn toggle(&mut self, now: Clocks) -> Next {
        self.set(self.mode.toggled(now), now)
    }

    /// A menu choice.
    fn choose(&mut self, action: MenuAction, now: Clocks) -> Next {
        match action {
            MenuAction::Activate(choice) => self.set(Mode::chosen(choice, now), now),
            MenuAction::ToggleStartup => {
                let shown = self.startup() == Startup::On;
                self.set_startup(!shown, now)
            }
            MenuAction::Quit => {
                let _ = self.set(Mode::Off, now);
                Next::Stop(Exit::Quit)
            }
        }
    }

    /// What Enable at Startup does at the next login ([`launchd::startup`]).
    fn startup(&self) -> Startup {
        launchd::startup(&self.paths, &self.label, &self.doors)
    }

    /// Turn Enable at Startup on or off.
    fn set_startup(&mut self, on: bool, now: Clocks) -> Next {
        if let Err(err) = launchd::set_startup(&self.paths, &self.label, &self.origin, on) {
            let verb = if on { "turn on" } else { "turn off" };
            aterm_log::warn!("caffeine: could not {verb} Enable at Startup: {err}");
        }
        if on
            && self.startup() == Startup::Disabled
            && let Err(err) = (self.doors.enable)(&self.label)
        {
            aterm_log::warn!(
                "caffeine: could not lift launchd's disable of {}: {err}",
                self.label
            );
        }
        if on && self.startup() == Startup::Blocked {
            (self.open_login_items)();
        }
        Next::Tick(next_tick_secs(self.mode, now))
    }

    /// The tick, and a wake from sleep. A timed mode that has ended is taken from the file again
    /// ([`Mug::reload`]): the mug's own restores off, and a writer's change saved before its
    /// notification is taken is kept.
    fn tick(&mut self, now: Clocks, menu_open: bool) -> Next {
        if self.mode.settled(now) == self.mode {
            if !self.apply(now) {
                return Next::Stop(Exit::Quit);
            }
            self.refresh(now);
        } else if let stop @ Next::Stop(_) = self.reload(now) {
            return stop;
        }
        if !menu_open && self.watch.as_mut().is_some_and(Watch::replaced) {
            return self.restart_or_retire();
        }
        Next::Tick(next_tick_secs(self.mode, now))
    }

    /// The origin was replaced.
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

    /// Save a timed mode again once the date has been changed since its save ([`state::refresh`]),
    /// so that a build before `ends`, which reads the file's end on the wall clock alone, reads
    /// the end this mug holds within a tick of the change. A failure leaves this build's readers
    /// right and the mug running; it is logged.
    fn refresh(&self, now: Clocks) {
        let file = self.paths.state_file();
        if let Err(err) = state::refresh(&file, self.mode, now) {
            aterm_log::warn!(
                "caffeine: could not save {} again after a change of the date: {err}",
                file.display()
            );
        }
    }

    /// Take the saved mode at `now`, saving it back when the file is one to ([`state::take`]):
    /// one on the wall clock alone, or one whose deadline the restore changed, but for one
    /// brought in by no more than [`state::DRIFT_SECS`]. So no later read of the file, by
    /// `status`, a bridge or the next mug, reads another time than the mug holds by more than
    /// those seconds. A writer's change that lands before that save is taken instead
    /// ([`state::take_saving`]). Whether the save, if due, was made; when it was not, the mug
    /// keeps its own mode, settled at `now`, so one that has ended is off.
    fn take(&mut self, now: Clocks) -> bool {
        let file = self.paths.state_file();
        match (self.take_saved)(&file, now) {
            Ok(mode) => {
                self.mode = mode;
                true
            }
            Err(err) => {
                self.mode = self.mode.settled(now);
                saved(&file, Err(err))
            }
        }
    }

    /// Take the file's mode: a writer's, or the mug's own. The mode held is applied even when
    /// the take's save fails and the mug stops, so a span that has ended lets go of the activity
    /// at once, not only when the app stops, which waits for an open menu ([`schedule_stop`]).
    fn reload(&mut self, now: Clocks) -> Next {
        let took = self.take(now);
        if !self.apply(now) || !took {
            return Next::Stop(Exit::Quit);
        }
        Next::Tick(next_tick_secs(self.mode, now))
    }

    /// The first [`Mug::apply`] of the mode read at the start.
    fn begin(&mut self, now: Clocks) -> bool {
        let applied = self.apply(now);
        bridge::release(&self.paths);
        applied
    }

    /// The app has stopped.
    fn leave(&mut self) -> Option<Exit> {
        self.ui = None;
        if self.exit == Some(Exit::Restart) && self.awake.is_some() && !(self.bridge_start)() {
            aterm_log::warn!("caffeine: no bridge keeps the Mac awake while the mug restarts");
        }
        self.awake = None;
        self.exit
    }

    /// The menu's rows at `now`.
    fn rows(&self, now: Clocks) -> Vec<Row> {
        menu::compose(self.mode, now, self.startup(), &self.version)
    }
}

/// The target's Rust state.
struct TargetIvars {
    /// A `RefCell` because AppKit messages the target through `&self`.
    mug: RefCell<Mug>,
    /// Current event when the last press was read or menu closed ([`click_from`]).
    stale: Cell<Option<EventKey>>,
    /// Whether the menu is open ([`ui::pop_up`]).
    tracking: Cell<bool>,
}

aterm_objc::declare_class! {
    /// What the mug's button, menu items, ticks, wake and state-file notifications and, as the
    /// app's delegate, `-terminate:` send to.
    struct CaffeineTarget: NSObject {
        const NAME: &str = "ATermCaffeineTarget";
        type Ivars = TargetIvars;
        protocols: [NSObject];

        /// The button was released.
        @sel(statusClicked:)
        fn status_clicked(&self, _sender: Id) {
            if MainThread::new().is_none() {
                return;
            }
            let now = now();
            let event = autoreleasepool(|_| current_event());
            let click = click_from(event, self.ivars().stale.replace(event));
            match click {
                Click::Toggle => self.handle_choice(|mug| mug.toggle(now)),
                Click::Menu => self.show_menu(now),
            }
        }

        /// The button's [`ui::SHOW_MENU`] accessibility action.
        @sel(showMenuAction:)
        fn show_menu_action(&self, _action: Id) -> Bool {
            if MainThread::new().is_none() {
                return Bool::NO;
            }
            if self.ivars().tracking.get() {
                return Bool::YES;
            }
            cancel_performs_of(self.as_id(), sel!(openMenu:));
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

        /// A menu item was chosen.
        @sel(menuAction:)
        fn menu_action(&self, sender: Id) {
            if sender.is_null() || MainThread::new().is_none() {
                return;
            }
            // SAFETY: `sender` is the live NSMenuItem AppKit passed; `-tag` is `q16@0:8`.
            let tag = unsafe { send_isize(sender, sel!(tag)) };
            let Some(action) = MenuAction::from_tag(tag) else {
                return;
            };
            let now = now();
            if action == MenuAction::ToggleStartup {
                // SAFETY: as `-tag`; `-state` is `q16@0:8`.
                let shown = unsafe { send_isize(sender, sel!(state)) } == CONTROL_STATE_ON;
                self.handle_choice(|mug| mug.set_startup(!shown, now));
            } else {
                self.handle_choice(|mug| mug.choose(action, now));
            }
        }

        /// The delayed perform [`arm_tick`] scheduled.
        @sel(tick:)
        fn tick(&self, _arg: Id) {
            if MainThread::new().is_some() {
                let now = now();
                let menu_open = self.ivars().tracking.get();
                self.handle(|mug| mug.tick(now, menu_open));
            }
        }

        /// `NSWorkspaceDidWakeNotification`.
        @sel(workspaceDidWake:)
        fn workspace_did_wake(&self, _note: Id) {
            if MainThread::new().is_some() {
                let now = now();
                let menu_open = self.ivars().tracking.get();
                self.handle(|mug| mug.tick(now, menu_open));
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

        /// End `-run`: the delayed perform a [`Next::Stop`] scheduled.
        @sel(stopRun:)
        fn stop_run(&self, _arg: Id) {
            if MainThread::new().is_some() {
                autoreleasepool(|_| stop_app());
            }
        }

        /// As the app's delegate, what `-terminate:` asks first ([`Self::quit_from_outside`]).
        @sel(applicationShouldTerminate:)
        fn application_should_terminate(&self, _app: Id) -> usize {
            if MainThread::new().is_none() || terminate_is_the_systems() {
                return TERMINATE_NOW;
            }
            if self.quit_from_outside() { TERMINATE_CANCEL } else { TERMINATE_NOW }
        }

        /// As the app's delegate, what `-terminate:` posts just before it exits the process, from
        /// inside `-run` (a logout, a restart or a shutdown): [`run`] never gets back to its
        /// discard of the copy, so it is made here. `-stop:`, how the menu's Quit and an update's
        /// restart end `-run`, posts nothing.
        @sel(applicationWillTerminate:)
        fn application_will_terminate(&self, _note: Id) {
            if MainThread::new().is_none() {
                return;
            }
            let code_dir = match self.ivars().mug.try_borrow() {
                Ok(mug) => mug.paths.code_dir(),
                Err(_) => return,
            };
            discard_own_copy(&code_dir);
        }
    }
}

impl CaffeineTarget {
    /// Run `f` on the mug.
    fn handle(&self, f: impl FnOnce(&mut Mug) -> Next) {
        let next = autoreleasepool(|_| self.ivars().mug.try_borrow_mut().ok()?.handled(f));
        self.follow(next);
    }

    /// [`Self::handle`] for what the person does ([`Mug::chosen`]).
    fn handle_choice(&self, f: impl FnOnce(&mut Mug) -> Next) {
        let next = autoreleasepool(|_| self.ivars().mug.try_borrow_mut().ok()?.chosen(f));
        self.follow(next);
    }

    /// A quit from outside the menu, taken as the person's.
    fn quit_from_outside(&self) -> bool {
        let now = now();
        self.handle_choice(|mug| mug.choose(MenuAction::Quit, now));
        self.ivars()
            .mug
            .try_borrow()
            .is_ok_and(|mug| mug.exit == Some(Exit::Quit))
    }

    /// Arm the next tick or schedule the stop `next` asks for.
    fn follow(&self, next: Option<Next>) {
        match next {
            Some(Next::Tick(secs)) => arm_tick(self.as_id(), secs),
            Some(Next::Stop(_)) => schedule_stop(self.as_id()),
            None => {}
        }
    }

    /// Bring the menu up to date and pop it up below the button, outside the borrow.
    fn show_menu(&self, now: Clocks) {
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
            cancel_performs_of(self.as_id(), sel!(openMenu:));
            self.ivars().tracking.set(true);
            ui::pop_up(&popup);
            self.ivars().tracking.set(false);
            self.ivars().stale.set(autoreleasepool(|_| current_event()));
            autoreleasepool(|_| drop(popup));
        }
    }
}

thread_local! {
    /// The target a person's SIGTERM quits ([`quit_by_sigterm`]).
    static SIGTERMED: RefCell<Option<Retained<CaffeineTarget>>> = const { RefCell::new(None) };
}

/// A person's SIGTERM ([`term::watch`]).
fn quit_by_sigterm(sender: libc::pid_t) {
    aterm_objc::run_on_main(move |_| {
        let target = SIGTERMED.with_borrow(|target| target.as_ref().map(Retained::clone_retained));
        let Some(target) = target else {
            return;
        };
        aterm_log::info!("caffeine: pid {sender} sent SIGTERM; the mug quits");
        if !target.quit_from_outside() {
            term::end_by_sigterm();
        }
    });
}

/// The running application, which [`run`] made before any of this can be sent.
fn shared_app() -> Id {
    // SAFETY: `+sharedApplication` is `@16@0:8`; every caller is on the main thread, and the app
    // it answers (+0) lives as long as the process.
    unsafe { send_id(class(c"NSApplication").as_id(), sel!(sharedApplication)) }
}

/// The app's current event.
fn current_event() -> Option<EventKey> {
    let app = shared_app();
    if app.is_null() {
        return None;
    }
    // SAFETY: `-currentEvent` is `@16@0:8` on the live app and answers the event (+0) or nil;
    // on it `-type` and `-modifierFlags` are `Q16@0:8` and `-timestamp` `d16@0:8`.
    unsafe {
        let event = send_id(app, sel!(currentEvent));
        (!event.is_null()).then(|| EventKey {
            kind: send_usize(event, sel!(type)),
            flags: send_usize(event, sel!(modifierFlags)),
            at: send_f64(event, sel!(timestamp)),
        })
    }
}

/// `-[NSAppleEventDescriptor attributeDescriptorForKeyword:]`, `@20@0:8I16`.
type AttributeFn = unsafe extern "C-unwind" fn(Id, Sel, u32) -> Id;

/// Whether the `-terminate:` AppKit asks about is the system's ([`SYSTEM_REASONS`]).
fn terminate_is_the_systems() -> bool {
    // SAFETY: main-thread Foundation sends, inside `applicationShouldTerminate:`.
    // `+sharedAppleEventManager` and `-currentAppleEvent` are `@16@0:8` and answer borrowed objects
    // or nil; `AttributeFn` answers a borrowed descriptor or nil; `-enumCodeValue` is `I16@0:8`.
    let reason = autoreleasepool(|_| unsafe {
        let manager = send_id(
            class(c"NSAppleEventManager").as_id(),
            sel!(sharedAppleEventManager),
        );
        if manager.is_null() {
            return None;
        }
        let event = send_id(manager, sel!(currentAppleEvent));
        if event.is_null() {
            return None;
        }
        let attribute: AttributeFn = aterm_objc::checked_msg();
        let reason = attribute(event, sel!(attributeDescriptorForKeyword:), KEY_QUIT_REASON);
        (!reason.is_null()).then(|| send_u32(reason, sel!(enumCodeValue)))
    });
    reason.is_some_and(reason_is_the_systems)
}

/// Whether quit reason `code` is a logout, a restart or a shutdown.
fn reason_is_the_systems(code: u32) -> bool {
    SYSTEM_REASONS
        .iter()
        .any(|reason| u32::from_be_bytes(*reason) == code)
}

/// `+[NSObject cancelPreviousPerformRequestsWithTarget:selector:object:]`, `v40@0:8@16:24@32`.
type CancelFn = unsafe extern "C-unwind" fn(Id, Sel, Id, Sel, Id);

/// `-[NSObject performSelector:withObject:afterDelay:inModes:]`, `v48@0:8:16@24d32@40`.
type PerformInModesFn = unsafe extern "C-unwind" fn(Id, Sel, Sel, Id, f64, Id);

/// An `NSArray` holding [`COMMON_MODES`], retained.
fn common_modes() -> Option<Obj> {
    let mode = aterm_objc::ns_string(COMMON_MODES)?;
    let modes = [mode.id()];
    // SAFETY: `+arrayWithObjects:count:` is `@32@0:8r^@16Q24`; it reads `count` live, non-nil
    // objects and answers the array at +0, which `Obj::retain` retains.
    unsafe {
        Obj::retain(send_id_idptr_usize(
            class(c"NSArray").as_id(),
            sel!(arrayWithObjects:count:),
            modes.as_ptr(),
            modes.len(),
        ))
    }
}

/// Tick `target` in `secs` seconds ([`schedule_stop`], [`Mug::tick`]).
fn arm_tick(target: Id, secs: u64) {
    autoreleasepool(|_| {
        let modes = common_modes();
        cancel_performs_of(target, sel!(tick:));
        // SAFETY: main-thread sends on the live target: `PerformInModesFn`'s prototype, its modes
        // array live across the send, or `-performSelector:withObject:afterDelay:`,
        // `v40@0:8:16@24d32`; `tick:` is a method the target declares.
        unsafe {
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

/// Drop the delayed performs of `selector` (with a nil object) `target` has queued, in any mode.
fn cancel_performs_of(target: Id, selector: Sel) {
    // SAFETY: a main-thread class send; `CancelFn`'s prototype, its target live.
    unsafe {
        let cancel: CancelFn = aterm_objc::checked_msg();
        cancel(
            class(c"NSObject").as_id(),
            sel!(cancelPreviousPerformRequestsWithTarget:selector:object:),
            target,
            selector,
            Id::NIL,
        );
    }
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

/// `[NSApp stop:nil]`, and an application-defined event posted at the front of the queue.
fn stop_app() {
    let app = shared_app();
    if app.is_null() {
        return;
    }
    // SAFETY: main-thread sends on the live app: `-stop:` is `v24@0:8@16`; the event (+0) the
    // helper's prototype answers is retained across `-postEvent:atStart:` (`v28@0:8@16B24`).
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

/// Send `target` `workspaceDidWake:` when the Mac wakes; whether it was registered. The center
/// holds `target` unretained: [`stop_observing_wake`] removes it before it goes.
fn observe_wake(target: Id) -> bool {
    let center = workspace_center();
    let Some(name) = aterm_objc::ns_string(DID_WAKE) else {
        return false;
    };
    if center.is_null() {
        return false;
    }
    // SAFETY: `-addObserver:selector:name:object:` is `v48@0:8@16:24@32@40`; the selector is one
    // the target declares.
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

/// The one stderr line of a mug that will not run, and exit 0.
fn refuse(paths: &Paths, why: &str) -> ExitCode {
    bridge::release(paths);
    eprintln!("aterm caffeine: {why}");
    ExitCode::SUCCESS
}

/// Run the menu-bar mug until Quit (exit 0) or an update restart ([`launchd::EXIT_RESTART`]).
pub(super) fn run(ctx: ServeContext) -> ExitCode {
    let Some(main) = MainThread::new() else {
        return refuse(
            &ctx.paths,
            "the menu-bar mug runs only on the process main thread",
        );
    };
    let ServeContext {
        paths,
        label,
        origin,
        version,
        restart_on_update,
        instance,
    } = ctx;
    let running = std::env::current_exe().unwrap_or_else(|_| origin.clone());
    let watch = restart_on_update.then(|| Watch::start(origin.clone(), &running));
    let code_dir = paths.code_dir();
    let mug = Mug {
        mode: Mode::Off,
        paths,
        label,
        origin,
        version,
        awake: None,
        awake_begin: Awake::begin,
        take_saved: state::take_saving,
        origin_serves: crate::code::serves_caffeine,
        bridge_start: bridge::start,
        doors: super::STARTUP_DOORS,
        open_login_items: login_items::open_settings,
        ui: None,
        watch,
        exit: None,
    };
    let code = autoreleasepool(|_| run_app(main, mug));
    if code == ExitCode::SUCCESS {
        discard_own_copy(&code_dir);
    }
    if code == Exit::Restart.code() {
        instance.release_named();
    } else {
        drop(instance);
    }
    code
}

/// Remove the copy under `code_dir` this mug runs from, with every other one that may go
/// ([`crate::code::discard_copies`]), when it runs from one; a failure is said on stderr.
fn discard_own_copy(code_dir: &Path) {
    if crate::code::runs_from_copy(code_dir)
        && let Err(e) = crate::code::discard_copies(code_dir)
    {
        eprintln!("aterm caffeine: {}: {e}", code_dir.display());
    }
}

/// [`run`]'s AppKit half.
fn run_app(main: MainThread, mug: Mug) -> ExitCode {
    let app = shared_app();
    if app.is_null() {
        return refuse(&mug.paths, "AppKit gave no application");
    }
    // SAFETY: `-setActivationPolicy:` is `B24@0:8q16`.
    let _ =
        unsafe { send_bool_isize(app, sel!(setActivationPolicy:), ACTIVATION_POLICY_ACCESSORY) };
    let paths = mug.paths.clone();
    let Some(target) = CaffeineTarget::alloc_init(
        main,
        TargetIvars {
            mug: RefCell::new(mug),
            stale: Cell::new(None),
            tracking: Cell::new(false),
        },
    ) else {
        return refuse(&paths, "the runtime refused the mug's target");
    };
    let changed = {
        let target = target.clone_retained();
        // SAFETY: `stateChanged:` is `v@:@` on the live target it retains.
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
        if !mug.take(now) {
            return refuse(&mug.paths, "the mug could not save the mode it read");
        }
        let rows = mug.rows(now);
        mug.ui = ui::install(
            target.as_id(),
            sel!(statusClicked:),
            sel!(showMenuAction:),
            sel!(menuAction:),
            rows,
        );
        if mug.ui.is_none() {
            return refuse(&mug.paths, "the menu bar refused the mug");
        }
        if !mug.begin(now) {
            return Exit::Quit.code();
        }
        next_tick_secs(mug.mode, now)
    };
    let woke = observe_wake(target.as_id());
    arm_tick(target.as_id(), first_tick);
    // SAFETY: `-setDelegate:` is `v24@0:8@16`; the app holds its delegate unretained, and the
    // target outlives `-run`, after which it is taken back.
    unsafe { send_v_id(app, sel!(setDelegate:), target.as_id()) };
    SIGTERMED.set(Some(target.clone_retained()));
    let sigterm = term::watch(quit_by_sigterm);
    if sigterm.is_none() {
        aterm_log::warn!(
            "caffeine: SIGTERM not watched; it keeps the disposition the mug started with \
             (under launchd its default action, so a quit from Activity Monitor restarts the mug)"
        );
    }
    // SAFETY: `-run` is `v16@0:8` on the live app, on the main thread.
    unsafe { send_v(app, sel!(run)) };
    drop(sigterm);
    SIGTERMED.set(None);
    cancel_performs(target.as_id());
    // SAFETY: as the `-setDelegate:` above, with nil.
    unsafe { send_v_id(app, sel!(setDelegate:), Id::NIL) };
    if woke {
        stop_observing_wake(target.as_id());
    }
    drop(changed);
    let exit = match target.ivars().mug.try_borrow_mut() {
        Ok(mut mug) => mug.leave(),
        Err(_) => None,
    };
    exit.unwrap_or(Exit::Quit).code()
}

#[cfg(test)]
mod tests {
    use aterm_objc::{ClassType, Encode, class_name, method_types};

    use super::*;
    use crate::model::{Choice, HOUR};

    const NOW: Clocks = Clocks {
        since_boot: 300_000,
        wall: 1_790_000_000,
    };

    #[test]
    fn a_refusal_lets_a_left_bridge_go() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let paths = Paths {
            state_dir: dir.path().join("state"),
            home: dir.path().join("home"),
            logs_dir: dir.path().join("logs"),
        };
        std::fs::create_dir_all(&paths.state_dir).expect("state dir");
        std::fs::write(paths.bridge_file(), "4242\n").expect("bridge file");
        assert_eq!(refuse(&paths, "a test refusal"), ExitCode::SUCCESS);
        assert!(!paths.bridge_file().exists(), "the bridge was left holding");
        assert_eq!(refuse(&paths, "a test refusal"), ExitCode::SUCCESS);
    }

    #[test]
    fn a_tick_comes_every_minute_or_just_after_a_closer_deadline() {
        assert_eq!(next_tick_secs(Mode::Off, NOW), TICK_SECS);
        assert_eq!(next_tick_secs(Mode::Forever, NOW), TICK_SECS);
        let hour = Mode::chosen(Choice::Hours1, NOW);
        assert_eq!(next_tick_secs(hour, NOW), TICK_SECS);
        assert_eq!(next_tick_secs(hour, NOW + (HOUR - 60)), TICK_SECS);
        assert_eq!(next_tick_secs(hour, NOW + (HOUR - 59)), 60);
        assert_eq!(next_tick_secs(hour, NOW + (HOUR - 30)), 31);
        assert_eq!(next_tick_secs(hour, NOW + (HOUR - 1)), 2);
        assert_eq!(next_tick_secs(hour, NOW + HOUR), TICK_SECS);
        for start in 0..HOUR {
            let mut at = start;
            while at < HOUR {
                at += next_tick_secs(hour, NOW + at);
            }
            assert!(at <= HOUR + 1, "from {start}: {at}");
            assert_eq!(hour.settled(NOW + at), Mode::Off);
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
        let others = (1 << 17) | (1 << 19) | (1 << 20);
        assert_eq!(click_of(Some((LEFT_UP, others))), Click::Toggle);
        assert_eq!(click_of(None), Click::Toggle);
    }

    #[test]
    fn a_press_finding_a_stale_event_toggles() {
        let right = |at: f64| EventKey {
            kind: EVENT_TYPE_RIGHT_MOUSE_UP,
            flags: 0,
            at,
        };
        let control = EventKey {
            kind: 2,
            flags: MODIFIER_FLAG_CONTROL,
            at: 9.0,
        };
        assert_eq!(click_from(Some(right(1.0)), None), Click::Menu);
        assert_eq!(click_from(Some(right(2.0)), Some(right(1.0))), Click::Menu);
        assert_eq!(click_from(Some(control), Some(right(1.0))), Click::Menu);
        assert_eq!(
            click_from(Some(right(1.0)), Some(right(1.0))),
            Click::Toggle
        );
        assert_eq!(click_from(Some(control), Some(control)), Click::Toggle);
        assert_eq!(click_from(None, Some(right(1.0))), Click::Toggle);
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
            take_saved: state::take_saving,
            origin_serves: |_| Some(true),
            bridge_start: || panic!("only a restart that holds the activity starts a bridge"),
            doors: StartupDoors {
                blocks: |_| false,
                disabled: |_| false,
                enable: |_| panic!("only a disabled Enable at Startup is enabled"),
            },
            open_login_items: || panic!("only a blocked Enable at Startup opens Login Items"),
            ui: None,
            watch: None,
            exit: None,
        }
    }

    fn saved(mug: &Mug) -> Option<Mode> {
        state::read(&mug.paths.state_file(), NOW).map(|s| s.mode)
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
                crate::cli::status_text(mug.mode, NOW, Some(42), Startup::Off, "l.x")
                    .starts_with("caffeine=off\nuntil=-\n")
            );
            mug.awake_begin = |_| panic!("a refused request does not retry without a new choice");
            assert_eq!(mug.tick(NOW + TICK_SECS, false), Next::Tick(TICK_SECS));
        }
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let mut mug = scratch_mug(dir.path());
        state::save(&mug.paths.state_file(), Mode::Forever).expect("saved request");
        mug.mode = Mode::Forever;
        mug.awake_begin = |_| None;
        assert!(mug.apply(NOW));
        assert_eq!(saved(&mug), Some(Mode::Off));
        let blocked = dir.path().join("not-a-directory");
        std::fs::write(&blocked, b"blocked").expect("blocked state directory");
        mug.paths.state_dir = blocked;
        assert_eq!(mug.handled(|m| m.toggle(NOW)), Some(Next::Stop(Exit::Quit)));
        assert!(mug.awake.is_none());
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
        assert_eq!(mug.tick(NOW + (HOUR - 30), false), Next::Tick(31));
        assert_eq!(mug.mode, hour);
        assert_eq!(mug.tick(NOW + HOUR + 1, false), Next::Tick(TICK_SECS));
        assert_eq!(mug.mode, Mode::Off);
        assert_eq!(saved(&mug), Some(Mode::Off));
        assert!(mug.awake.is_none());
        let _ = mug.choose(MenuAction::Activate(Choice::Forever), NOW);
        assert!(mug.awake.is_some());
        assert_eq!(mug.choose(MenuAction::Quit, NOW), Next::Stop(Exit::Quit));
        assert_eq!(saved(&mug), Some(Mode::Off));
        assert!(mug.awake.is_none());
    }

    /// Ten minutes into Activate for 1 Hour, the wall clock set back a day: the mug still lets
    /// go at the hour's end, and the hold, the file and the tooltip agree.
    #[test]
    fn a_clock_set_back_keeps_the_mac_awake_no_longer_than_the_span() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let mut mug = scratch_mug(dir.path());
        let _ = mug.choose(MenuAction::Activate(Choice::Hours1), NOW);
        let back = Clocks {
            since_boot: NOW.since_boot + 600,
            wall: NOW.wall - 24 * HOUR + 600,
        };
        assert_eq!(mug.tick(back, false), Next::Tick(TICK_SECS));
        assert!(mug.awake.is_some());
        assert_eq!(
            state::load(&mug.paths.state_file(), back).remaining(back),
            Some(HOUR - 600)
        );
        assert_eq!(mug.tick(back + (HOUR - 600), false), Next::Tick(TICK_SECS));
        assert_eq!(mug.mode, Mode::Off, "the hour is up");
        assert!(mug.awake.is_none());
        assert_eq!(saved(&mug), Some(Mode::Off));
    }

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
        assert_eq!(mug.handled(|m| m.tick(NOW + 2, false)), None);
        assert_eq!(mug.handled(|m| m.reload(NOW + 3)), None);
        assert_eq!(mug.mode, Mode::Off);
        assert!(mug.awake.is_none());
        assert_eq!(std::fs::read(&file).expect("read"), before);
        assert_eq!(mug.exit, Some(Exit::Quit));
        let mut mug = scratch_mug(dir.path());
        std::fs::write(&mug.origin, b"build 1").expect("exe");
        mug.watch = Some(Watch::start(mug.origin.clone(), &mug.origin));
        std::fs::write(&mug.origin, b"build two").expect("exe");
        assert_eq!(
            mug.handled(|m| m.tick(NOW, false)),
            Some(Next::Stop(Exit::Restart))
        );
        assert_eq!(mug.exit, Some(Exit::Restart));
        assert_eq!(mug.handled(|m| m.choose(MenuAction::Quit, NOW)), None);
        assert_eq!(mug.handled(|m| m.tick(NOW + 1, false)), None);
        assert_eq!(mug.exit, Some(Exit::Restart));
        assert_eq!(
            mug.chosen(|m| m.choose(MenuAction::Activate(Choice::Hours8), NOW)),
            None,
            "nothing armed: the restart's stop is scheduled"
        );
        assert_eq!(mug.exit, Some(Exit::Restart));
        assert!(saved(&mug).is_some_and(|mode| mode.is_on(NOW)));
        assert_eq!(mug.chosen(|m| m.choose(MenuAction::Quit, NOW)), None);
        assert_eq!(mug.exit, Some(Exit::Quit));
        assert_eq!(saved(&mug), Some(Mode::Off));
        assert_eq!(
            mug.chosen(|m| m.toggle(NOW)),
            None,
            "quitting: nothing more"
        );
        assert_eq!(saved(&mug), Some(Mode::Off));
    }

    #[test]
    fn a_writers_change_is_read_and_not_saved_back() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let mut mug = scratch_mug(dir.path());
        let file = mug.paths.state_file();
        state::save(&file, Mode::Forever).expect("save");
        let mut before = b"# a writer's line\n".to_vec();
        before.extend(std::fs::read(&file).expect("read"));
        std::fs::write(&file, &before).expect("write");
        assert_eq!(mug.reload(NOW), Next::Tick(TICK_SECS));
        assert_eq!(mug.mode, state::load(&file, NOW));
        assert_eq!(mug.awake.is_some(), mug.mode.is_on(NOW));
        assert_eq!(std::fs::read(&file).expect("read"), before);
    }

    /// Activate for 8 Hours an hour from its end, in a file an earlier build wrote, taken at the
    /// start (the update's restart into this build) or from a writer: the mug saves it back on
    /// the boot clock, so a read of the file once the date is set back or forward a day
    /// (`status`, a bridge, the next mug) has the 50 minutes the mug holds.
    #[test]
    fn an_earlier_builds_file_is_saved_back_on_the_boot_clock() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let mut mug = scratch_mug(dir.path());
        let file = mug.paths.state_file();
        std::fs::create_dir_all(&mug.paths.state_dir).expect("state dir");
        let earlier = format!(
            "mode=until\ndeadline={}\nspan={}\nboot={}\n",
            NOW.wall + HOUR,
            8 * HOUR,
            state::boot_text(crate::instance::boot_id().expect("this boot"))
        );
        for from_writer in [false, true] {
            let how = if from_writer {
                "from a writer"
            } else {
                "at the start"
            };
            std::fs::write(&file, &earlier).expect("an earlier build's file");
            let taken = if from_writer {
                mug.reload(NOW) == Next::Tick(TICK_SECS)
            } else {
                mug.take(NOW)
            };
            assert!(taken, "{how}");
            assert_eq!(mug.mode.remaining(NOW), Some(HOUR), "{how}");
            for wall in [NOW.wall - 24 * HOUR + 600, NOW.wall + 24 * HOUR + 600] {
                let later = Clocks {
                    since_boot: NOW.since_boot + 600,
                    wall,
                };
                assert_eq!(
                    state::load(&file, later).remaining(later),
                    Some(HOUR - 600),
                    "{how}, the wall clock at {wall}"
                );
            }
        }
    }

    /// A deadline a day away on a span of an hour (a hand-edited or damaged file), taken at the
    /// start or from a writer: the mug holds the hour and saves it so, and a read half an hour on
    /// (`status`, a bridge, the next mug) has the half hour left the mug holds, not a full hour
    /// again.
    #[test]
    fn a_deadline_past_its_span_is_saved_in_to_it() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let mut mug = scratch_mug(dir.path());
        let file = mug.paths.state_file();
        std::fs::create_dir_all(&mug.paths.state_dir).expect("state dir");
        let far = format!(
            "mode=until\nends={}\nspan={HOUR}\ndeadline={}\nboot={}\n",
            NOW.since_boot + 24 * HOUR,
            NOW.wall + 24 * HOUR,
            state::boot_text(crate::instance::boot_id().expect("this boot"))
        );
        for from_writer in [false, true] {
            let how = if from_writer {
                "from a writer"
            } else {
                "at the start"
            };
            std::fs::write(&file, &far).expect("a far deadline");
            let taken = if from_writer {
                mug.reload(NOW) == Next::Tick(TICK_SECS)
            } else {
                mug.take(NOW)
            };
            assert!(taken, "{how}");
            assert_eq!(mug.mode.remaining(NOW), Some(HOUR), "{how}");
            let later = NOW + HOUR / 2;
            assert_eq!(
                state::load(&file, later).remaining(later),
                Some(HOUR / 2),
                "{how}"
            );
        }
    }

    /// Activate for 8 Hours, then the date set back or forward a day: the next tick saves the
    /// mode again, so the file's wall-clock end, all a build before `ends` reads, is where the
    /// wall clock now puts the end the mug holds. A tick with the date unchanged, and the one
    /// after the save, leave the file alone.
    #[test]
    fn a_change_of_the_date_saves_the_wall_clock_end_again() {
        use std::os::unix::fs::MetadataExt as _;
        let identity = |file: &Path| {
            let meta = std::fs::metadata(file).expect("the state file");
            (meta.dev(), meta.ino())
        };
        let wall_end = |file: &Path| {
            std::fs::read_to_string(file)
                .expect("the state file")
                .lines()
                .find_map(|line| line.strip_prefix("deadline="))
                .map(|end| end.parse::<u64>().expect("a number"))
        };
        for wall in [NOW.wall - 24 * HOUR + 600, NOW.wall + 24 * HOUR + 600] {
            let dir = aterm_tempfile::tempdir().expect("tempdir");
            let mut mug = scratch_mug(dir.path());
            let file = mug.paths.state_file();
            let _ = mug.choose(MenuAction::Activate(Choice::Hours8), NOW);
            assert_eq!(wall_end(&file), Some(NOW.wall + 8 * HOUR));
            let saved = identity(&file);
            assert_eq!(mug.tick(NOW + TICK_SECS, false), Next::Tick(TICK_SECS));
            assert_eq!(identity(&file), saved, "the date unchanged: {wall}");
            let later = Clocks {
                since_boot: NOW.since_boot + 600,
                wall,
            };
            assert_eq!(mug.tick(later, false), Next::Tick(TICK_SECS));
            let left = mug.mode.remaining(later);
            assert_eq!(left, Some(8 * HOUR - 600), "{wall}");
            assert_eq!(
                wall_end(&file),
                left.map(|left| later.wall + left),
                "{wall}"
            );
            let refreshed = identity(&file);
            assert_ne!(refreshed, saved, "{wall}");
            assert_eq!(mug.tick(later + TICK_SECS, false), Next::Tick(TICK_SECS));
            assert_eq!(identity(&file), refreshed, "saved once: {wall}");
        }
    }

    /// A writer's change saved after the mug's, with the date set back a day since and the
    /// writer's notification not yet taken: the tick that comes first saves nothing of the mug's
    /// over it, whatever the writer chose.
    #[test]
    fn a_tick_after_a_change_of_the_date_keeps_a_writers_mode() {
        let later = Clocks {
            since_boot: NOW.since_boot + 600,
            wall: NOW.wall - 24 * HOUR + 600,
        };
        for writers in [
            Mode::Off,
            Mode::Forever,
            Mode::chosen(Choice::Hours1, later),
        ] {
            let dir = aterm_tempfile::tempdir().expect("tempdir");
            let mut mug = scratch_mug(dir.path());
            let file = mug.paths.state_file();
            let _ = mug.choose(MenuAction::Activate(Choice::Hours8), NOW);
            state::save_at(&file, writers, later).expect("the writer's save");
            let text = std::fs::read_to_string(&file).expect("the writer's file");
            assert_eq!(mug.tick(later, false), Next::Tick(TICK_SECS), "{writers:?}");
            assert_eq!(
                std::fs::read_to_string(&file).expect("the state file"),
                text,
                "{writers:?}"
            );
        }
    }

    /// Activate for 1 Hour, and a writer's save as the hour ends, its notification not yet
    /// taken: on with no end, a new hour, or a new hour on clocks a second ahead of the tick's.
    /// The tick that ends the hour takes the writer's mode and leaves its file as it is; with no
    /// writer, it lets go and saves off.
    #[test]
    fn the_tick_that_ends_a_span_keeps_a_writers_mode() {
        let end = NOW + HOUR;
        let at = end + 1;
        for writers in [
            None,
            Some((Mode::Forever, end)),
            Some((Mode::chosen(Choice::Hours1, end), end)),
            Some((Mode::chosen(Choice::Hours1, at + 1), at + 1)),
        ] {
            let dir = aterm_tempfile::tempdir().expect("tempdir");
            let mut mug = scratch_mug(dir.path());
            let file = mug.paths.state_file();
            let _ = mug.choose(MenuAction::Activate(Choice::Hours1), NOW);
            let text = writers.map(|(mode, saved_at)| {
                state::save_at(&file, mode, saved_at).expect("the writer's save");
                std::fs::read_to_string(&file).expect("the writer's file")
            });
            assert_eq!(mug.tick(at, false), Next::Tick(TICK_SECS), "{writers:?}");
            let want = writers.map_or(Mode::Off, |(mode, _)| mode.settled(at));
            assert_eq!(mug.mode, want, "{writers:?}");
            assert_eq!(mug.awake.is_some(), writers.is_some(), "{writers:?}");
            match text {
                Some(text) => assert_eq!(
                    std::fs::read_to_string(&file).expect("the state file"),
                    text,
                    "{writers:?}"
                ),
                None => assert_eq!(saved(&mug), Some(Mode::Off), "CONTROL"),
            }
        }
    }

    /// Activate for 1 Hour, and obstruct the save-back after its expired state is read, the menu
    /// open: the tick stops on the real storage error and lets go of the activity itself, off,
    /// rather than leaving it to the stop, which waits for the menu to close.
    #[test]
    fn a_span_that_ends_on_a_failed_save_lets_go_at_once() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let mut mug = scratch_mug(dir.path());
        let _ = mug.choose(MenuAction::Activate(Choice::Hours1), NOW);
        assert!(mug.awake.is_some());
        let file = mug.paths.state_file();
        let before = std::fs::read(&file).expect("the saved span");
        mug.take_saved = |path, now| {
            let (mode, was) = state::take(path, now);
            assert_eq!(mode, Mode::Off, "the saved span has ended");
            let was = was.expect("the expired span needs a corrective save");
            struct RestoreParent {
                path: PathBuf,
                kept: PathBuf,
                moved: bool,
            }
            impl RestoreParent {
                fn restore(&mut self) -> std::io::Result<()> {
                    if self.moved {
                        match std::fs::remove_file(&self.path) {
                            Ok(()) => {}
                            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                            Err(err) => return Err(err),
                        }
                        std::fs::rename(&self.kept, &self.path)?;
                        self.moved = false;
                    }
                    Ok(())
                }
            }
            impl Drop for RestoreParent {
                fn drop(&mut self) {
                    let _ = self.restore();
                }
            }
            let parent = path.parent().expect("private state directory");
            let mut restore = RestoreParent {
                path: parent.to_path_buf(),
                kept: parent.with_extension("preserved"),
                moved: false,
            };
            assert!(!restore.kept.exists(), "the preservation path is new");
            std::fs::rename(&restore.path, &restore.kept).expect("preserve the state directory");
            restore.moved = true;
            std::fs::write(&restore.path, b"not a directory").expect("obstruct the save parent");
            let failed = state::save_over(path, mode, &was, now);
            assert!(
                failed.is_err(),
                "the real save must fail even with elevated privileges"
            );
            restore.restore().expect("restore the state directory");
            failed.map(|_| mode)
        };
        let next = mug.tick(NOW + HOUR + 1, true);
        assert_eq!(next, Next::Stop(Exit::Quit));
        assert_eq!(mug.mode, Mode::Off);
        assert!(mug.awake.is_none());
        assert_eq!(
            std::fs::read(&file).expect("the original state file"),
            before
        );
        assert!(!mug.paths.state_dir.with_extension("preserved").exists());
    }

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
        let _ = mug.set_startup(true, NOW);
        assert_eq!(mug.set_startup(true, NOW), Next::Tick(TICK_SECS));
        assert!(checked(&mug), "asked on, stays on");
        let _ = mug.choose(MenuAction::ToggleStartup, NOW);
        assert!(!checked(&mug), "the control");
    }

    #[test]
    fn a_blocked_enable_at_startup_opens_login_items() {
        thread_local! {
            static OPENED: Cell<u32> = const { Cell::new(0) };
        }
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let mut mug = scratch_mug(dir.path());
        mug.doors.blocks = |_| true;
        mug.open_login_items = || OPENED.with(|n| n.set(n.get() + 1));
        let plist = mug.paths.startup_plist(&mug.label);
        assert_eq!(mug.set_startup(true, NOW), Next::Tick(TICK_SECS));
        assert!(plist.is_file());
        assert_eq!(mug.startup(), Startup::Blocked);
        assert_eq!(OPENED.with(Cell::get), 1);
        let row = mug.rows(NOW).into_iter().find_map(|row| match row {
            Row::Action {
                label,
                action: MenuAction::ToggleStartup,
                checked,
            } => Some((label, checked)),
            _ => None,
        });
        assert_eq!(row, Some((menu::STARTUP_BLOCKED_LABEL.to_string(), false)));
        let _ = mug.choose(MenuAction::ToggleStartup, NOW);
        assert!(plist.is_file(), "the plist stays");
        assert_eq!(OPENED.with(Cell::get), 2);

        use std::os::unix::fs::PermissionsExt as _;
        let agents = plist.parent().expect("LaunchAgents").to_path_buf();
        std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(0o555)).expect("ro");
        let _ = mug.set_startup(true, NOW);
        std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(0o755)).expect("rw");
        assert_eq!(OPENED.with(Cell::get), 3, "a failed write still opens it");
        let _ = mug.set_startup(false, NOW);
        assert!(!plist.exists());
        assert_eq!(mug.startup(), Startup::Off);
        assert_eq!(OPENED.with(Cell::get), 3, "off opens nothing");
    }

    #[test]
    fn enable_at_startup_lifts_launchds_disable() {
        thread_local! {
            static DISABLED: Cell<bool> = const { Cell::new(true) };
            static LIFTED: Cell<u32> = const { Cell::new(0) };
        }
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let mut mug = scratch_mug(dir.path());
        mug.doors.disabled = |_| DISABLED.with(Cell::get);
        mug.doors.enable = |label| {
            assert_eq!(label, "com.aterm.aterm.caffeine.test");
            LIFTED.with(|n| n.set(n.get() + 1));
            DISABLED.with(|d| d.set(false));
            Ok(())
        };
        let _ = launchd::set_startup(&mug.paths, &mug.label, &mug.origin, true);
        assert_eq!(mug.startup(), Startup::Disabled);
        let row = mug.rows(NOW).into_iter().find_map(|row| match row {
            Row::Action {
                label,
                action: MenuAction::ToggleStartup,
                checked,
            } => Some((label, checked)),
            _ => None,
        });
        assert_eq!(row, Some((menu::STARTUP_LABEL.to_string(), false)));
        let _ = mug.choose(MenuAction::ToggleStartup, NOW);
        assert_eq!(LIFTED.with(Cell::get), 1);
        assert_eq!(mug.startup(), Startup::On);
        let _ = mug.set_startup(true, NOW);
        assert_eq!(LIFTED.with(Cell::get), 1, "nothing to lift once on");
    }

    #[test]
    fn an_update_waits_for_the_menu_and_the_deadline_does_not() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let mut mug = scratch_mug(dir.path());
        std::fs::write(&mug.origin, b"build 1").expect("exe");
        mug.watch = Some(Watch::start(mug.origin.clone(), &mug.origin));
        let hour = Mode::chosen(Choice::Hours1, NOW);
        assert_eq!(
            mug.handled(|m| m.set(hour, NOW)),
            Some(Next::Tick(TICK_SECS))
        );
        assert!(mug.awake.is_some());
        std::fs::write(&mug.origin, b"build two").expect("exe");
        assert_eq!(
            mug.handled(|m| m.tick(NOW + 60, true)),
            Some(Next::Tick(TICK_SECS))
        );
        assert_eq!(mug.exit, None);
        assert_eq!(
            mug.handled(|m| m.tick(NOW + HOUR, true)),
            Some(Next::Tick(TICK_SECS))
        );
        assert!(mug.awake.is_none());
        assert_eq!(mug.mode, Mode::Off);
        assert_eq!(
            mug.handled(|m| m.tick(NOW + HOUR + 60, false)),
            Some(Next::Stop(Exit::Restart))
        );
    }

    #[test]
    fn a_replaced_origin_ends_the_app_for_a_restart() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let mut mug = scratch_mug(dir.path());
        std::fs::write(&mug.origin, b"build 1").expect("exe");
        mug.watch = Some(Watch::start(mug.origin.clone(), &mug.origin));
        assert_eq!(mug.tick(NOW, false), Next::Tick(TICK_SECS));
        std::fs::write(&mug.origin, b"build two").expect("exe");
        assert_eq!(mug.tick(NOW + 60, false), Next::Stop(Exit::Restart));
        mug.origin_serves = |_| None;
        assert_eq!(mug.restart_or_retire(), Next::Stop(Exit::Restart));
    }

    thread_local! {
        static BRIDGED_WHILE_HELD: Cell<Option<bool>> = const { Cell::new(None) };
    }

    #[test]
    fn a_restart_holding_the_activity_hands_it_to_a_bridge_first() {
        const REASON: &str = "aterm caffeine restart bridge test";
        let held = || !super::super::awake::tests::assertions_named(REASON).is_empty();
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let mut mug = scratch_mug(dir.path());
        mug.awake_begin = |_| Awake::begin(REASON);
        assert_eq!(mug.toggle(NOW), Next::Tick(TICK_SECS));
        assert!(held());
        mug.exit = Some(Exit::Restart);
        mug.bridge_start = || {
            BRIDGED_WHILE_HELD.with(|cell| {
                cell.set(Some(
                    !super::super::awake::tests::assertions_named(REASON).is_empty(),
                ));
            });
            true
        };
        assert_eq!(mug.leave(), Some(Exit::Restart));
        assert_eq!(BRIDGED_WHILE_HELD.with(Cell::get), Some(true));
        assert!(mug.awake.is_none());
        assert!(!held(), "the mug's own activity ends once the bridge holds");
        let mut mug = scratch_mug(dir.path());
        mug.awake_begin = |_| Awake::begin(REASON);
        let _ = mug.toggle(NOW);
        mug.exit = Some(Exit::Restart);
        mug.bridge_start = || false;
        assert_eq!(mug.leave(), Some(Exit::Restart));
        assert!(!held());
        let mut mug = scratch_mug(dir.path());
        mug.exit = Some(Exit::Restart);
        assert_eq!(mug.leave(), Some(Exit::Restart));
        let mut mug = scratch_mug(dir.path());
        mug.awake_begin = |_| Awake::begin(REASON);
        let _ = mug.toggle(NOW);
        mug.exit = Some(Exit::Quit);
        assert_eq!(mug.leave(), Some(Exit::Quit));
        assert!(!held());
    }

    #[test]
    fn the_first_apply_lets_a_restarts_bridge_go() {
        for mode in [Mode::Forever, Mode::Off] {
            let dir = aterm_tempfile::tempdir().expect("tempdir");
            let mut mug = scratch_mug(dir.path());
            state::save(&mug.paths.state_file(), mode).expect("saved");
            std::fs::write(mug.paths.bridge_file(), b"4242\n").expect("bridge file");
            mug.mode = state::load(&mug.paths.state_file(), NOW);
            assert!(mug.begin(NOW));
            assert_eq!(mug.awake.is_some(), mode == Mode::Forever, "{mode:?}");
            assert!(!mug.paths.bridge_file().exists(), "{mode:?}");
        }
    }

    #[test]
    fn only_a_logout_a_restart_or_a_shutdown_is_the_systems_quit() {
        for reason in [b"logo", b"rlgo", b"rrst", b"rest", b"rsdn", b"shut"] {
            assert!(
                reason_is_the_systems(u32::from_be_bytes(*reason)),
                "{reason:?}"
            );
        }
        for other in [*b"quit", *b"aevt", *b"why?", [0; 4]] {
            assert!(
                !reason_is_the_systems(u32::from_be_bytes(other)),
                "{other:?}"
            );
        }
        assert_eq!(KEY_QUIT_REASON, 0x7768_793F);
    }

    #[test]
    fn a_replacement_without_the_verb_ends_the_job_and_its_login_item() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let mut mug = scratch_mug(dir.path());
        std::fs::write(&mug.origin, b"build 1").expect("exe");
        launchd::set_startup(&mug.paths, &mug.label, &mug.origin, true).expect("startup on");
        assert!(launchd::startup_enabled(&mug.paths, &mug.label));
        mug.watch = Some(Watch::start(mug.origin.clone(), &mug.origin));
        mug.origin_serves = |_| Some(false);
        assert_eq!(mug.tick(NOW, false), Next::Tick(TICK_SECS));
        std::fs::write(&mug.origin, b"an older build").expect("exe");
        assert_eq!(mug.tick(NOW + 60, false), Next::Stop(Exit::Quit));
        assert!(!launchd::startup_enabled(&mug.paths, &mug.label));
    }

    #[test]
    fn the_registered_class_is_what_the_runtime_reports() {
        let cls = CaffeineTarget::class();
        assert!(!cls.is_null());
        assert_eq!(class(c"ATermCaffeineTarget"), cls);
        // SAFETY: `cls` is the class this module registered: it and its superclass are live.
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
                sel!(applicationWillTerminate:),
            ] {
                assert!(
                    aterm_objc::send::send_bool_sel(
                        cls.as_id(),
                        sel!(instancesRespondToSelector:),
                        sel
                    ),
                    "{sel:?}"
                );
                assert_eq!(method_types(cls, sel).as_deref(), Some("v@:@"), "{sel:?}");
            }
            assert_eq!(
                method_types(cls, sel!(showMenuAction:)).as_deref(),
                Some(format!("{}@:@", <Bool as Encode>::ENCODING).as_str())
            );
            assert_eq!(
                method_types(cls, sel!(applicationShouldTerminate:)).as_deref(),
                Some(format!("{}@:@", <usize as Encode>::ENCODING).as_str())
            );
            assert_eq!(method_types(cls, sel!(dealloc)).as_deref(), Some("v@:"));
        }
    }

    fn types(ret: &str, args: &[&str]) -> String {
        format!("{ret}@:{}", args.concat())
    }

    #[test]
    fn every_send_here_has_the_prototype_the_runtime_reports() {
        use aterm_objc::{Bool, Encode};
        let (v, id, b, sel_, q, uq, d, s, pt, ui) = (
            <() as Encode>::ENCODING,
            Id::ENCODING,
            Bool::ENCODING,
            Sel::ENCODING,
            isize::ENCODING,
            usize::ENCODING,
            f64::ENCODING,
            i16::ENCODING,
            CGPoint::ENCODING,
            u32::ENCODING,
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
            (c"NSApplication", sel!(setDelegate:), false, types(v, &[id])),
            (
                c"NSAppleEventManager",
                sel!(sharedAppleEventManager),
                true,
                types(id, &[]),
            ),
            (
                c"NSAppleEventManager",
                sel!(currentAppleEvent),
                false,
                types(id, &[]),
            ),
            (
                c"NSAppleEventDescriptor",
                sel!(attributeDescriptorForKeyword:),
                false,
                types(id, &[ui]),
            ),
            (
                c"NSAppleEventDescriptor",
                sel!(enumCodeValue),
                false,
                types(ui, &[]),
            ),
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
            (c"NSEvent", sel!(timestamp), false, types(d, &[])),
            (c"NSMenuItem", sel!(tag), false, types(q, &[])),
            (c"NSMenuItem", sel!(state), false, types(q, &[])),
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
            // SAFETY: `cls` is a live, registered class, and so is its metaclass.
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
