// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The menu-bar app.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use aterm_macos_ui::appkit::class;
use aterm_objc::send::{
    send_bool_isize, send_f64, send_id, send_id_idptr_usize,
    send_id_usize_point_usize_f64_isize_id_i16_isize_isize, send_isize, send_u32, send_usize,
    send_v, send_v_id, send_v_id_bool, send_v_id_sel_id_id, send_v_sel_id_f64,
};
use aterm_objc::{Bool, CGPoint, Id, MainThread, Obj, Retained, Sel, autoreleasepool, sel};

use super::ServeContext;
use super::awake::{AWAKE_REASON, Awake};
use super::bridge;
use super::login_items;
use super::term;
use super::ui::{self, StatusUi};
use super::watch::Watch;
use crate::instance::Instance;
use crate::launchd::{self, Startup, StartupDoors};
use crate::menu::{self, Click, Face, MenuAction, Row, Shown};
use crate::model::{Clocks, Hold, Machine, Mode};
use crate::notify::ChangedObserver;
use crate::paths::Paths;
use crate::state;

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
/// How soon the mug looks again while a wake settles ([`crate::model::settling`]).
const SETTLE_TICK_SECS: u64 = 1;
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
/// `NSApplicationDidChangeScreenParametersNotification`, spelled as its value: a display came or
/// went, as the built-in one goes when the lid closes with an external display attached (macOS
/// then runs on, in clamshell mode, rather than sleep).
const SCREENS_CHANGED: &str = "NSApplicationDidChangeScreenParametersNotification";
/// `NSRunLoopCommonModes`, spelled as its value (Foundation's symbol holds it).
const COMMON_MODES: &str = "kCFRunLoopCommonModes";

/// How the app ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Exit {
    /// Quit: exit 0, which ends the launchd job.
    Quit,
    /// The origin's code changed ([`launchd::EXIT_RESTART`]).
    Restart,
    /// A save failed ([`launchd::EXIT_RETRY`]): launchd starts the mug again, which reads the
    /// mode the file still holds.
    Retry,
}

impl Exit {
    fn code(self) -> ExitCode {
        match self {
            Self::Quit => ExitCode::SUCCESS,
            Self::Restart => ExitCode::from(launchd::EXIT_RESTART),
            Self::Retry => ExitCode::from(launchd::EXIT_RETRY),
        }
    }
}

/// What the target does once the mug has handled something.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Next {
    /// Look again in this many seconds.
    Tick(u64),
    /// Look again when the tick already armed comes: what was done changed no mode, and a tick
    /// armed again would hold back the time the menu bar shows by up to a minute.
    Keep,
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

/// Whether `save`, of `file`, was made; a failure is logged, and the mug stops on it, for
/// launchd to start it again ([`Exit::Retry`]).
fn saved(file: &Path, save: std::io::Result<()>) -> bool {
    match save {
        Ok(()) => true,
        Err(err) => {
            aterm_log::warn!(
                "acaffeine: could not save {}: {err}; the mug stops, and launchd starts it again",
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
    /// The keep-awake activity, held while the mode is on and not paused ([`Hold::activity`]).
    awake: Option<Awake>,
    /// The one activity-acquisition door.
    awake_begin: fn(&str) -> Option<Awake>,
    /// Keep Awake with Lid Closed, as last set or read ([`state::load_lid`]).
    lid_awake: bool,
    /// The `PreventSystemSleep` assertion, held while Keep Awake with Lid Closed keeps the mode
    /// on ([`Hold::system`]).
    lid_hold: Option<aterm_sysprobe::power::Hold>,
    /// The one door to that assertion ([`hold_lid`]).
    lid_begin: fn(&str) -> Option<aterm_sysprobe::power::Hold>,
    /// The machine as it reads now: the lid and a dark wake ([`crate::held::machine_now`]); a
    /// test's arm reads an open lid in a full wake.
    machine: fn() -> Machine,
    /// The system announced it will sleep, and no wake has come since ([`Mug::power`]).
    going_to_sleep: bool,
    /// When the last wake was heard, on the boot clock, unless a will-sleep came after it
    /// ([`Mug::woke`]): a pause read soon after it is looked at again in a second.
    woke_at: Option<u64>,
    /// What [`Mug::apply`] last decided to hold ([`crate::model::hold`]).
    hold: Hold,
    /// Restore saved state, including any corrective save-back.
    take_saved: fn(&Path, Clocks) -> std::io::Result<Mode>,
    /// Whether a replaced origin still has the `acaffeine` verb ([`crate::code::serves_acaffeine`]).
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
    /// The person's Quit saved off, so a mode on saved in the file as the mug lets go of its lock
    /// is a writer's from after it ([`let_go`]).
    quit_off: bool,
    /// What [`Mug::apply`] last gave the item in the menu bar to show, kept with no item too: the
    /// full mug while the activity is held (`true`), else the empty one, and the tooltip.
    shown: Option<(bool, String)>,
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

    /// Hold what the mode, Keep Awake with Lid Closed and the machine call for
    /// ([`crate::model::hold`]) and show its actual state: the activity while on, with the
    /// `PreventSystemSleep` assertion besides when the person opted in, and nothing while
    /// paused (the lid closed without the opt-in, or a going-to-sleep or a dark wake either
    /// way), the mode staying as it is. What macOS refused leaves the mode as it is, which is
    /// the person's: the mug shows itself so, records why ([`crate::trouble`]) and asks again at
    /// the next tick. Once nothing it asked for is refused, the trouble is cleared and a
    /// restart's bridge lets go.
    fn apply(&mut self, now: Clocks) {
        let machine = Machine {
            going_to_sleep: self.going_to_sleep,
            ..(self.machine)()
        };
        let hold = crate::model::hold(self.mode.is_on(now), self.lid_awake, machine);
        if hold != self.hold
            && (matches!(hold, Hold::Paused(_)) || matches!(self.hold, Hold::Paused(_)))
        {
            aterm_log::info!(
                "acaffeine: {:?}, which held {:?}, now holds {hold:?}{}",
                machine,
                self.hold,
                self.after_wake(now)
            );
        }
        self.hold = hold;
        if !hold.activity() {
            self.awake = None;
        } else if self.awake.is_none() {
            self.awake = (self.awake_begin)(AWAKE_REASON);
            if self.awake.is_none() {
                aterm_log::warn!(
                    "acaffeine: the keep-awake activity did not begin; asking again in a minute"
                );
                crate::trouble::note(&self.paths, "macOS refused the keep-awake activity");
            }
        }
        if !hold.system() {
            self.lid_hold = None;
        } else if self.lid_hold.is_none() {
            self.lid_hold = (self.lid_begin)(AWAKE_REASON);
            if self.lid_hold.is_none() {
                aterm_log::warn!(
                    "acaffeine: the lid-closed assertion was refused; asking again in a minute"
                );
                crate::trouble::note(
                    &self.paths,
                    "macOS refused the assertion that keeps this Mac running with its lid closed",
                );
            }
        }
        let refused =
            (hold.activity() && self.awake.is_none()) || (hold.system() && self.lid_hold.is_none());
        if !refused {
            crate::trouble::clear(&self.paths);
            bridge::release(&self.paths);
        }
        let lit = self.awake.is_some();
        let face = match hold {
            Hold::Paused(pause) => Face::Paused(pause),
            _ if refused => Face::Refused,
            held => Face::Held { lid: held.system() },
        };
        let face = (lit, menu::tooltip(self.mode, now, face));
        if let Some(ui) = self.ui.as_mut() {
            ui.show(face.0, &face.1);
        }
        self.shown = Some(face);
    }

    /// Apply `mode` and save it.
    fn set(&mut self, mode: Mode, now: Clocks) -> Next {
        self.mode = mode.settled(now);
        self.apply(now);
        if !self.save(now) {
            return Next::Stop(Exit::Retry);
        }
        Next::Tick(self.next_tick(now))
    }

    /// A left click: on forever when off, off when on.
    fn toggle(&mut self, now: Clocks) -> Next {
        self.set(self.mode.toggled(now), now)
    }

    /// A menu choice.
    fn choose(&mut self, action: MenuAction, now: Clocks) -> Next {
        match action {
            MenuAction::Activate(choice) => self.set(Mode::chosen(choice, now), now),
            MenuAction::ToggleLid => self.set_lid(!self.lid_awake, now),
            MenuAction::ToggleStartup => {
                let shown = self.startup() == Startup::On;
                self.set_startup(!shown)
            }
            MenuAction::Quit => {
                self.quit_off = matches!(self.set(Mode::Off, now), Next::Tick(_));
                Next::Stop(Exit::Quit)
            }
        }
    }

    /// Turn Keep Awake with Lid Closed on or off, as the person chose: saved
    /// ([`state::save_lid`]) and applied, leaving the tick armed as it is ([`Next::Keep`]). A
    /// save that fails is logged and changes nothing, so the menu goes on showing what the file
    /// holds.
    fn set_lid(&mut self, on: bool, now: Clocks) -> Next {
        let file = self.paths.lid_file();
        match state::save_lid(&file, on) {
            Ok(()) => self.lid_awake = on,
            Err(err) => aterm_log::warn!("acaffeine: could not save {}: {err}", file.display()),
        }
        self.apply(now);
        Next::Keep
    }

    /// What the system announced ([`aterm_uds::power::listen`]): a will-sleep lets go of
    /// everything the mug holds before the system is told it may sleep (the listener acknowledges
    /// once this returns), and a wake looks again ([`Mug::woke`]), holding again unless the lid is
    /// still closed without Keep Awake with Lid Closed, or this is a dark wake (which pauses it
    /// with the option on too). A [`aterm_uds::power::Power::Woke`] with no will-sleep before it
    /// is an idle sleep the system pondered and dropped (`kIOMessageSystemWillNotSleep`), no wake:
    /// it looks as a tick does, with no settling after it.
    fn power(&mut self, power: aterm_uds::power::Power, now: Clocks, menu_open: bool) -> Next {
        match power {
            aterm_uds::power::Power::WillSleep => {
                self.going_to_sleep = true;
                self.woke_at = None;
                self.apply(now);
                Next::Keep
            }
            aterm_uds::power::Power::Woke if self.going_to_sleep => {
                self.going_to_sleep = false;
                self.woke(now, menu_open)
            }
            aterm_uds::power::Power::Woke => self.tick(now, menu_open),
        }
    }

    /// A wake was heard (the system's, or `NSWorkspaceDidWakeNotification`): look again as a tick
    /// does, and while what that holds is a pause, every second until the wake has settled
    /// ([`crate::model::settling`]), so a lid's open holds again once its wake has graphics
    /// rather than at the next tick.
    /// The wake's look is logged with what it read, so a wake that only a later look ends the
    /// pause of says so in `aterm.log` with how long that took ([`Mug::after_wake`]).
    fn woke(&mut self, now: Clocks, menu_open: bool) -> Next {
        self.woke_at = Some(now.since_boot);
        let next = self.tick(now, menu_open);
        aterm_log::info!(
            "acaffeine: woke: {:?} holds {:?}",
            Machine {
                going_to_sleep: self.going_to_sleep,
                ..(self.machine)()
            },
            self.hold
        );
        next
    }

    /// `" 3 s after the wake"` while a wake is recorded ([`Mug::woke`]), else nothing.
    fn after_wake(&self, now: Clocks) -> String {
        self.woke_at.map_or_else(String::new, |at| {
            format!(" {} s after the wake", now.since_boot.saturating_sub(at))
        })
    }

    /// The seconds to the next look at `now`: in a second while a wake settles, else
    /// [`next_tick_secs`].
    fn next_tick(&self, now: Clocks) -> u64 {
        if crate::model::settling(self.hold, self.woke_at, now.since_boot) {
            SETTLE_TICK_SECS
        } else {
            next_tick_secs(self.mode, now)
        }
    }

    /// What Enable at Startup does at the next login ([`launchd::startup`]).
    fn startup(&self) -> Startup {
        launchd::startup(&self.paths, &self.label, &self.doors)
    }

    /// Turn Enable at Startup on or off, as the person chose ([`launchd::choose_startup`]),
    /// leaving the tick armed as it is ([`Next::Keep`]).
    fn set_startup(&mut self, on: bool) -> Next {
        if let Err(err) = launchd::choose_startup(&self.paths, &self.label, &self.origin, on) {
            let verb = if on { "turn on" } else { "turn off" };
            aterm_log::warn!("acaffeine: could not {verb} Enable at Startup: {err}");
        }
        if on
            && self.startup() == Startup::Disabled
            && let Err(err) = (self.doors.enable)(&self.label)
        {
            aterm_log::warn!(
                "acaffeine: could not lift launchd's disable of {}: {err}",
                self.label
            );
        }
        if on && self.startup() == Startup::Blocked {
            (self.open_login_items)();
        }
        Next::Keep
    }

    /// The tick, and a wake from sleep. A timed mode that has ended is taken from the file again
    /// ([`Mug::reload`]): off, saved back, unless a writer's change not yet notified is there,
    /// which is kept.
    fn tick(&mut self, now: Clocks, menu_open: bool) -> Next {
        if self.mode.settled(now) == self.mode {
            self.apply(now);
            self.refresh(now);
        } else if let stop @ Next::Stop(_) = self.reload(now) {
            return stop;
        }
        if !menu_open && self.watch.as_mut().is_some_and(Watch::replaced) {
            return self.restart_or_retire();
        }
        Next::Tick(self.next_tick(now))
    }

    /// The origin was replaced.
    fn restart_or_retire(&mut self) -> Next {
        if (self.origin_serves)(&self.origin) != Some(false) {
            return Next::Stop(Exit::Restart);
        }
        aterm_log::warn!(
            "acaffeine: {} has no `acaffeine` verb now; the mug ends and Enable at Startup is off",
            self.origin.display()
        );
        if launchd::startup_enabled(&self.paths, &self.label)
            && let Err(err) = launchd::set_startup(&self.paths, &self.label, &self.origin, false)
        {
            aterm_log::warn!("acaffeine: could not turn off Enable at Startup: {err}");
        }
        Next::Stop(Exit::Quit)
    }

    /// Save a timed mode again once the date has changed since its save ([`state::refresh`]), so
    /// the file's `deadline` says the mug's end on the wall clock within a tick, as a read after a
    /// restart takes it. A failure is logged and the mug runs on: a read in this boot takes
    /// `ends`, which is right either way.
    fn refresh(&self, now: Clocks) {
        let file = self.paths.state_file();
        if let Err(err) = state::refresh(&file, self.mode, now) {
            aterm_log::warn!(
                "acaffeine: could not save {} again after a change of the date: {err}",
                file.display()
            );
        }
    }

    /// Read Keep Awake with Lid Closed ([`state::load_lid`]), and take the saved mode at `now`,
    /// saving it back when the file is one to ([`state::take`]), so
    /// no later read of it (`status`, a bridge, the next mug) is off the mug's mode by more than
    /// [`state::DRIFT_SECS`]; a writer's change that lands first is taken instead
    /// ([`state::take_saving`]). Whether the mug runs on what it took: the save, if one was due,
    /// was made, or it failed on a file that reads forever as it stands (a writer's forever not
    /// yet notified, say), which `status`, a bridge and the next mug read as forever too, so the
    /// mug holds forever. Otherwise the mug keeps its own mode, settled at `now`.
    fn take(&mut self, now: Clocks) -> bool {
        self.lid_awake = state::load_lid(&self.paths.lid_file());
        let file = self.paths.state_file();
        match (self.take_saved)(&file, now) {
            Ok(mode) => {
                self.mode = mode;
                true
            }
            Err(err) if state::load(&file, now) == Mode::Forever => {
                aterm_log::warn!(
                    "acaffeine: could not save {} again: {err}; it reads forever as it is, and the \
                     mug keeps this Mac awake",
                    file.display()
                );
                self.mode = Mode::Forever;
                true
            }
            Err(err) => {
                self.mode = self.mode.settled(now);
                saved(&file, Err(err))
            }
        }
    }

    /// Take the file's mode, a writer's or the mug's own ([`Mug::take`]), and apply it. When the
    /// take could not save and the mug stops, the mug's own mode settled at `now` is applied
    /// first, so a span of its own that has ended lets go of the activity at once rather than
    /// when the app stops, which waits for an open menu ([`schedule_stop`]).
    fn reload(&mut self, now: Clocks) -> Next {
        let took = self.take(now);
        self.apply(now);
        if !took {
            return Next::Stop(Exit::Retry);
        }
        Next::Tick(self.next_tick(now))
    }

    /// The app has stopped.
    fn leave(&mut self) -> Option<Exit> {
        self.ui = None;
        if self.exit == Some(Exit::Restart) && self.awake.is_some() && !(self.bridge_start)() {
            aterm_log::warn!("acaffeine: no bridge keeps the Mac awake while the mug restarts");
        }
        self.awake = None;
        self.lid_hold = None;
        self.exit
    }

    /// The menu's rows at `now`.
    fn rows(&self, now: Clocks) -> Vec<Row> {
        let shown = Shown {
            startup: self.startup(),
            lid_awake: self.lid_awake,
            paused: match self.hold {
                Hold::Paused(pause) => Some(pause),
                _ => None,
            },
            version: &self.version,
        };
        menu::compose(self.mode, now, shown)
    }

    /// Show the mode at `now` in the open menu ([`StatusUi::retitle_menu`]), unless how the app
    /// ends is decided: a tick, a wake or a writer's change that lands while the menu is open
    /// moves its checkmark and its time left there too, rather than when it next opens. Rows of
    /// another shape wait for that opening, which rebuilds the menu.
    fn retitle_open_menu(&mut self, now: Clocks) {
        if self.exit.is_some() {
            return;
        }
        let rows = self.rows(now);
        if let Some(ui) = self.ui.as_mut() {
            let _ = ui.retitle_menu(rows);
        }
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
            if action == MenuAction::ToggleStartup {
                // SAFETY: as `-tag`; `-state` is `q16@0:8`.
                let shown = unsafe { send_isize(sender, sel!(state)) } == CONTROL_STATE_ON;
                self.handle_choice(|mug| mug.set_startup(!shown));
            } else {
                let now = now();
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
                self.handle(|mug| mug.woke(now, menu_open));
            }
        }

        /// `NSApplicationDidChangeScreenParametersNotification`: the lid may have closed or opened
        /// on a Mac that runs on with an external display.
        @sel(screensChanged:)
        fn screens_changed(&self, _note: Id) {
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

        /// As the app's delegate, what `-terminate:` asks first ([`Self::stop_from_outside`]).
        @sel(applicationShouldTerminate:)
        fn application_should_terminate(&self, _app: Id) -> usize {
            if MainThread::new().is_none() || terminate_is_the_systems() {
                return TERMINATE_NOW;
            }
            if self.stop_from_outside() { TERMINATE_CANCEL } else { TERMINATE_NOW }
        }

        /// As the app's delegate, what `-terminate:` posts just before it exits the process from
        /// inside `-run` (a logout, a restart or a shutdown): [`run`] never gets back to its
        /// discard of the copy, so it is made here. `-stop:` (the menu's Quit, an update's
        /// restart) posts nothing.
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
    /// Run `f` on the mug, and while the menu is open show what it did there
    /// ([`Mug::retitle_open_menu`]).
    fn handle(&self, f: impl FnOnce(&mut Mug) -> Next) {
        let open = self.ivars().tracking.get();
        let next = autoreleasepool(|_| {
            let mut mug = self.ivars().mug.try_borrow_mut().ok()?;
            let next = mug.handled(f);
            if open {
                mug.retitle_open_menu(now());
            }
            next
        });
        self.follow(next);
    }

    /// [`Self::handle`] for what the person does ([`Mug::chosen`]).
    fn handle_choice(&self, f: impl FnOnce(&mut Mug) -> Next) {
        let next = autoreleasepool(|_| self.ivars().mug.try_borrow_mut().ok()?.chosen(f));
        self.follow(next);
    }

    /// A quit from outside the menu ([`stop_from_outside`]): whether the mug stops.
    fn stop_from_outside(&self) -> bool {
        let (next, stops) = autoreleasepool(|_| stop_from_outside(&self.ivars().mug));
        self.follow(next);
        stops
    }

    /// Arm the next tick or schedule the stop `next` asks for.
    fn follow(&self, next: Option<Next>) {
        match next {
            Some(Next::Tick(secs)) => arm_tick(self.as_id(), secs),
            Some(Next::Stop(_)) => schedule_stop(self.as_id()),
            Some(Next::Keep) | None => {}
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

/// A quit from outside the menu (a SIGTERM a process sent, Activity Monitor's Quit, another
/// app's `-terminate:`), as a choice is made ([`Mug::chosen`]): the mug stops, and the saved mode
/// stays as it is, so the window's watch, the converge pass or the next login starts it again.
/// Only the person's own off (a click on the mug, the menu's Quit, `aterm acaffeine off` or
/// `quit`, which save it first, or a span's end) lasts. A restart an update asked for gives way to
/// it: the mug stops rather than run the new code at once. Answers what to follow and whether the
/// mug stops: nothing to follow when it is already ending, and nothing at all when it is borrowed.
fn stop_from_outside(mug: &RefCell<Mug>) -> (Option<Next>, bool) {
    let Ok(mut held) = mug.try_borrow_mut() else {
        return (None, false);
    };
    let next = held.chosen(|_| Next::Stop(Exit::Quit));
    (next, held.exit == Some(Exit::Quit))
}

thread_local! {
    /// The running target, on the main thread: the one a person's SIGTERM stops
    /// ([`quit_by_sigterm`]) and the system's sleep and wake reach ([`heard_power`]).
    static TARGET: RefCell<Option<Retained<CaffeineTarget>>> = const { RefCell::new(None) };
}

/// A person's SIGTERM ([`term::watch`]).
fn quit_by_sigterm(sender: libc::pid_t) {
    aterm_objc::run_on_main(move |_| {
        let target = TARGET.with_borrow(|target| target.as_ref().map(Retained::clone_retained));
        let Some(target) = target else {
            return;
        };
        aterm_log::info!(
            "acaffeine: pid {sender} sent SIGTERM; the mug stops and keeps the saved mode"
        );
        if !target.stop_from_outside() {
            term::end_by_sigterm();
        }
    });
}

/// What the system announced, heard on the power listener's thread ([`aterm_uds::power::listen`]):
/// handed to the mug on the main thread, which it waits for ([`aterm_objc::run_on_main`]), so a
/// will-sleep is acknowledged only once the mug has let go.
fn heard_power(power: aterm_uds::power::Power) {
    aterm_objc::run_on_main(move |_| {
        let target = TARGET.with_borrow(|target| target.as_ref().map(Retained::clone_retained));
        if let Some(target) = target {
            let menu_open = target.ivars().tracking.get();
            target.handle(|mug| mug.power(power, now(), menu_open));
        }
    });
}

/// The `PreventSystemSleep` assertion Keep Awake with Lid Closed holds, named `reason`.
fn hold_lid(reason: &str) -> Option<aterm_sysprobe::power::Hold> {
    aterm_sysprobe::power::hold(crate::held::LID_ASSERTION, reason)
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

/// The app's default notification center (+0), where AppKit posts its screen changes.
fn default_center() -> Id {
    // SAFETY: `+defaultCenter` is `@16@0:8`, main thread; the center lives for the process.
    unsafe { send_id(class(c"NSNotificationCenter").as_id(), sel!(defaultCenter)) }
}

/// Send `target` `selector` whenever `center` posts `name`; whether it was registered. The center
/// holds `target` unretained: [`stop_observing`] removes it before it goes.
fn observe(center: Id, name: &str, target: Id, selector: Sel) -> bool {
    let Some(name) = aterm_objc::ns_string(name) else {
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
            selector,
            name.id(),
            Id::NIL,
        );
    }
    true
}

/// Undo [`observe`] on `center`.
fn stop_observing(center: Id, target: Id) {
    if !center.is_null() {
        // SAFETY: `-removeObserver:` is `v24@0:8@16`.
        unsafe { send_v_id(center, sel!(removeObserver:), target) };
    }
}

/// Why the mug did not start ([`run`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unstarted {
    /// `serve` ran off the process main thread.
    NotMainThread,
    /// `+sharedApplication` answered nil.
    NoApplication,
    /// The runtime made no target.
    TargetRefused,
    /// The mode the mug read could not be saved back (a full disk).
    SaveFailed,
    /// The status bar made no item.
    MenuBarRefused,
}

impl Unstarted {
    /// What the mug says and records.
    const fn why(self) -> &'static str {
        match self {
            Self::NotMainThread => "the menu-bar mug runs only on the process main thread",
            Self::NoApplication => "AppKit gave no application",
            Self::TargetRefused => "the runtime refused the mug's target",
            Self::SaveFailed => "the mug could not save the mode it read",
            Self::MenuBarRefused => "the menu bar refused the mug",
        }
    }

    /// Whether a start again can cure it: a save can succeed once the disk has room, while the
    /// thread, AppKit, the runtime and the menu bar answer a start a minute later as they answered
    /// this one.
    const fn clears_up(self) -> bool {
        matches!(self, Self::SaveFailed)
    }

    /// Record it ([`crate::trouble`]) and end: [`retry`] when it [`Self::clears_up`], else
    /// [`stall`].
    fn exit(self, paths: &Paths) -> ExitCode {
        if self.clears_up() {
            retry(paths, self.why())
        } else {
            stall(paths, self.why())
        }
    }
}

/// The one stderr line of a mug that could not start, recorded as its trouble
/// ([`crate::trouble`]), and [`Exit::Retry`]'s status after [`launchd::pause_before_retry`]:
/// launchd's KeepAlive starts it again. A bridge an update restart left holds on meanwhile.
fn retry(paths: &Paths, why: &str) -> ExitCode {
    eprintln!("aterm acaffeine: {why}; launchd starts it again");
    crate::trouble::note(paths, why);
    launchd::pause_before_retry();
    Exit::Retry.code()
}

/// The one stderr line of a mug that cannot start for a reason no start again cures, recorded as
/// its trouble ([`crate::trouble`]) for the window's watch, `aterm doctor` and the converge pass
/// to report, and exit 0, which launchd's KeepAlive does not start again. Its copy of the app
/// stays, for the next start a window or a pass makes to run again
/// ([`crate::code::prepare_copy`]); a bridge an update restart left holds on, to its own end.
fn stall(paths: &Paths, why: &str) -> ExitCode {
    eprintln!("aterm acaffeine: {why}; a window or a converge pass starts it again");
    crate::trouble::note(paths, why);
    ExitCode::SUCCESS
}

/// Run the menu-bar mug until Quit (exit 0), an update restart ([`launchd::EXIT_RESTART`]), which
/// also ends, as its launchd job, a Quit that a writer's on overtook ([`let_go`]), or a failure to
/// start ([`Unstarted::exit`]): one that can clear up launchd starts again
/// ([`launchd::EXIT_RETRY`]); one that cannot ends 0 with its trouble recorded.
pub(super) fn run(ctx: ServeContext) -> ExitCode {
    let Some(main) = MainThread::new() else {
        return Unstarted::NotMainThread.exit(&ctx.paths);
    };
    aterm_macos_ui::appkit::load();
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
    let ending = paths.clone();
    let job = restart_on_update.then(|| label.clone());
    let mug = Mug {
        mode: Mode::Off,
        paths,
        label,
        origin,
        version,
        awake: None,
        awake_begin: Awake::begin,
        lid_awake: false,
        lid_hold: None,
        lid_begin: hold_lid,
        machine: crate::held::machine_now,
        going_to_sleep: false,
        woke_at: None,
        hold: Hold::Off,
        take_saved: state::take_saving,
        origin_serves: crate::code::serves_acaffeine,
        bridge_start: bridge::start,
        doors: super::STARTUP_DOORS,
        open_login_items: login_items::open_settings,
        ui: None,
        watch,
        exit: None,
        quit_off: false,
        shown: None,
    };
    let Ended { code, quit_off } = match autoreleasepool(|_| run_app(main, mug)) {
        Ok(ended) => ended,
        Err(unstarted) => return unstarted.exit(&ending),
    };
    let_go(code, job.as_deref(), quit_off, instance, &ending, now)
}

/// How the mug that ended its run with `code` lets go of its lock (`instance`), and the code it
/// exits with. An update restart ([`Exit::Restart`]) leaves its pid and label in the lock
/// ([`Instance::release_named`]); every other end removes the copy the mug ran from, on a clean
/// exit ([`discard_own_copy`]), then empties the lock and lets go. A mug that is the launchd job
/// `job` ([`super::ServeContext::restart_on_update`]) ending after the person's Quit saved off
/// (`quit_off`) first takes the turn a window's start, `on` and `quit` take
/// ([`crate::cli::change`]), and holds it while it removes its copy, lets go, and reads the mode
/// saved in [`Paths::state_file`] as written ([`state::saved_on`]) at `now()`: on, a writer's from
/// after the Quit, it ends for a restart, and launchd starts the job again, which reads that mode;
/// it names the job in the lock again first ([`name_again`]), as an update restart leaves it, so
/// a `quit` of another build run before the new mug holds the lock still finds the job and boots
/// it out. A writer that looked for a mug before the turn found this one, its copy still there,
/// holding the lock, and started nothing, and its on, saved before the read, ends this mug for
/// the restart; one after it finds the lock free, waits for this process to end, and starts the
/// job if launchd has not (the verbs' `ensure`). A writer that takes no turn (an older build) and
/// saves between the let-go and the read is read as well. A mug run by hand has no job to start it
/// again and ends as it would; a Quit whose save failed leaves the file's earlier mode, no
/// writer's. `aterm acaffeine quit` boots the job out, which no restart survives and whose SIGTERM
/// ends the mug before this read: that verb reads the file again itself.
fn let_go(
    code: ExitCode,
    job: Option<&str>,
    quit_off: bool,
    instance: Instance,
    paths: &Paths,
    now: impl FnOnce() -> Clocks,
) -> ExitCode {
    let_go_with(
        code,
        job,
        quit_off,
        instance,
        paths,
        Ending {
            now,
            turn: || crate::cli::change(paths, |why| aterm_log::warn!("acaffeine: {why}")),
            discard: || discard_own_copy(&paths.code_dir()),
            pause: || std::thread::sleep(NAME_POLL),
            name_within: NAME_AGAIN,
        },
    )
}

/// What [`let_go_with`] reads the clocks with (`now`), takes the turn with (`turn`), removes the
/// mug's copy with (`discard`), waits with between tries of the lock (`pause`) and how long it
/// tries the lock (`name_within`, [`name_again`]).
struct Ending<N, T, D, P> {
    now: N,
    turn: T,
    discard: D,
    pause: P,
    name_within: Duration,
}

/// [`let_go`], through `ending`.
fn let_go_with(
    code: ExitCode,
    job: Option<&str>,
    quit_off: bool,
    instance: Instance,
    paths: &Paths,
    ending: Ending<
        impl FnOnce() -> Clocks,
        impl FnOnce() -> Option<state::Held>,
        impl FnOnce(),
        impl FnMut(),
    >,
) -> ExitCode {
    let Ending {
        now,
        turn,
        discard,
        pause,
        name_within,
    } = ending;
    if code == Exit::Restart.code() {
        instance.release_named();
        return code;
    }
    let after_quit = job.filter(|_| code == ExitCode::SUCCESS && quit_off);
    let _turn = after_quit.and_then(|_| turn());
    if code == ExitCode::SUCCESS {
        discard();
    }
    drop(instance);
    let Some(label) = after_quit else {
        return code;
    };
    if !state::saved_on(&paths.state_file(), now()) {
        return code;
    }
    aterm_log::info!("acaffeine: the mode was set on while the mug quit; launchd starts it again");
    name_again(&paths.lock_file(), label, name_within, pause);
    Exit::Restart.code()
}

/// How long a mug ending for a restart tries to take its lock again ([`name_again`]): another's
/// probe holds it for an instant, another build's mug for good.
const NAME_AGAIN: Duration = Duration::from_secs(1);

/// How often [`name_again`] tries the lock.
const NAME_POLL: Duration = Duration::from_millis(20);

/// Take the lock at `lock` again, up to `within` (the live mug's [`NAME_AGAIN`]) with `pause`
/// between tries, and let go of it with this process's pid and the job `label` in it
/// ([`Instance::release_named`]), for a mug that let go emptied and then found it must end for a
/// restart. A lock another mug holds is left to it, as is one whose file cannot be taken or
/// rewritten (a failed rewrite lets go emptied).
fn name_again(lock: &Path, label: &str, within: Duration, mut pause: impl FnMut()) {
    let until = Instant::now() + within;
    loop {
        match Instance::try_hold(lock) {
            Ok(Some(mut again)) => {
                if again.set_label(label).is_ok() {
                    again.release_named();
                }
                return;
            }
            Ok(None) if Instant::now() < until => pause(),
            Ok(None) | Err(_) => return,
        }
    }
}

/// Remove the copy under `code_dir` this mug runs from, with every other one that may go
/// ([`crate::code::discard_copies`]), when it runs from one; a copy kept and a failure are said
/// on stderr.
fn discard_own_copy(code_dir: &Path) {
    if !crate::code::runs_from_copy(code_dir) {
        return;
    }
    match crate::code::discard_copies(code_dir) {
        Ok(kept) => kept.say(),
        Err(e) => eprintln!("aterm acaffeine: {}: {e}", code_dir.display()),
    }
}

/// How [`run_app`] ended.
struct Ended {
    code: ExitCode,
    /// The person's Quit saved off ([`Mug::quit_off`]).
    quit_off: bool,
}

impl From<ExitCode> for Ended {
    fn from(code: ExitCode) -> Self {
        Self {
            code,
            quit_off: false,
        }
    }
}

/// [`run`]'s AppKit half: how the app ended, or why it did not start.
fn run_app(main: MainThread, mug: Mug) -> Result<Ended, Unstarted> {
    let app = shared_app();
    if app.is_null() {
        return Err(Unstarted::NoApplication);
    }
    // SAFETY: `-setActivationPolicy:` is `B24@0:8q16`.
    let _ =
        unsafe { send_bool_isize(app, sel!(setActivationPolicy:), ACTIVATION_POLICY_ACCESSORY) };
    let Some(target) = CaffeineTarget::alloc_init(
        main,
        TargetIvars {
            mug: RefCell::new(mug),
            stale: Cell::new(None),
            tracking: Cell::new(false),
        },
    ) else {
        return Err(Unstarted::TargetRefused);
    };
    let changed = {
        let target = target.clone_retained();
        // SAFETY (in the hook): `stateChanged:` is `v@:@` on the live target it retains.
        ChangedObserver::register(move || unsafe {
            send_v_id(target.as_id(), sel!(stateChanged:), Id::NIL);
        })
    };
    if changed.is_none() {
        aterm_log::warn!(
            "acaffeine: not watching the state file; `aterm acaffeine on|off` waits for a restart"
        );
    }
    let now = now();
    let first_tick = {
        let Ok(mut mug) = target.ivars().mug.try_borrow_mut() else {
            return Ok(ExitCode::FAILURE.into());
        };
        if !mug.take(now) {
            return Err(Unstarted::SaveFailed);
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
            return Err(Unstarted::MenuBarRefused);
        }
        mug.apply(now);
        mug.next_tick(now)
    };
    let woke = observe(
        workspace_center(),
        DID_WAKE,
        target.as_id(),
        sel!(workspaceDidWake:),
    );
    let screens = observe(
        default_center(),
        SCREENS_CHANGED,
        target.as_id(),
        sel!(screensChanged:),
    );
    if !screens {
        aterm_log::warn!(
            "acaffeine: display changes not watched; a lid closed on an external display is \
             seen at the next tick"
        );
    }
    arm_tick(target.as_id(), first_tick);
    // SAFETY: `-setDelegate:` is `v24@0:8@16`; the app holds its delegate unretained, and the
    // target outlives `-run`, after which it is taken back.
    unsafe { send_v_id(app, sel!(setDelegate:), target.as_id()) };
    TARGET.set(Some(target.clone_retained()));
    let power = aterm_uds::power::listen(Box::new(heard_power));
    if power.is_none() {
        aterm_log::warn!(
            "acaffeine: the system's sleep is not heard; the mug looks at the lid each tick"
        );
    }
    let sigterm = term::watch(quit_by_sigterm);
    if sigterm.is_none() {
        aterm_log::warn!(
            "acaffeine: SIGTERM not watched; it keeps the disposition the mug started with \
             (under launchd its default action, so a quit from Activity Monitor restarts the mug)"
        );
    }
    // SAFETY: `-run` is `v16@0:8` on the live app, on the main thread.
    unsafe { send_v(app, sel!(run)) };
    drop(sigterm);
    drop(power);
    TARGET.set(None);
    cancel_performs(target.as_id());
    // SAFETY: as the `-setDelegate:` above, with nil.
    unsafe { send_v_id(app, sel!(setDelegate:), Id::NIL) };
    if woke {
        stop_observing(workspace_center(), target.as_id());
    }
    if screens {
        stop_observing(default_center(), target.as_id());
    }
    drop(changed);
    let (exit, quit_off) = match target.ivars().mug.try_borrow_mut() {
        Ok(mut mug) => (mug.leave(), mug.quit_off),
        Err(_) => (None, false),
    };
    Ok(Ended {
        code: exit.unwrap_or(Exit::Quit).code(),
        quit_off,
    })
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;
