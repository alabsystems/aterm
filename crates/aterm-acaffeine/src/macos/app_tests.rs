// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_objc::{ClassType, Encode, class_name, method_types};

use super::*;
use crate::model::{Choice, HOUR};

const NOW: Clocks = Clocks {
    since_boot: 300_000,
    wall: 1_790_000_000,
};

fn scratch_paths(dir: &std::path::Path) -> Paths {
    let paths = Paths {
        state_dir: dir.join("state"),
        home: dir.join("home"),
        logs_dir: dir.join("logs"),
        local_volume: |_| true,
    };
    std::fs::create_dir_all(&paths.state_dir).expect("state dir");
    paths
}

/// A `serve` run off the process main thread (here a test's) cannot run the menu-bar mug,
/// and no start a minute later changes that: it ends 0, which launchd does not start again,
/// with why recorded for `aterm doctor` and the window; it lets its lock go, and a bridge an
/// update restart left holds on.
#[test]
fn a_mug_that_cannot_start_off_the_main_thread_is_not_started_every_minute() {
    assert!(
        MainThread::new().is_none(),
        "the test harness runs each test on a thread of its own"
    );
    let dir = crate::test_scratch_dir("app");
    let paths = scratch_paths(dir.path());
    std::fs::write(paths.bridge_file(), "4242\n").expect("bridge file");
    let instance = crate::instance::Instance::try_hold(&paths.lock_file())
        .expect("hold")
        .expect("free");
    let code = run(ServeContext {
        paths: paths.clone(),
        label: "com.aterm.aterm.acaffeine.test".into(),
        origin: dir.path().join("aterm"),
        version: "0.0.0".into(),
        restart_on_update: false,
        instance,
    });
    assert_eq!(code, ExitCode::SUCCESS, "never started again by launchd");
    assert_eq!(
        crate::trouble::read(&paths).map(|trouble| trouble.why),
        Some(Unstarted::NotMainThread.why().to_string())
    );
    assert!(paths.bridge_file().exists(), "the bridge holds on");
    assert!(
        crate::instance::Instance::try_hold(&paths.lock_file())
            .expect("hold")
            .is_some(),
        "the lock is let go"
    );
}

/// Of the starts that fail, only a save that failed (a full disk) is started again by
/// launchd ([`launchd::EXIT_RETRY`]); AppKit, the runtime and the menu bar end 0. Each says
/// why in the trouble.
#[test]
fn a_start_that_fails_is_started_again_only_when_it_can_clear_up() {
    for (unstarted, code) in [
        (Unstarted::NotMainThread, ExitCode::SUCCESS),
        (Unstarted::NoApplication, ExitCode::SUCCESS),
        (Unstarted::TargetRefused, ExitCode::SUCCESS),
        (Unstarted::MenuBarRefused, ExitCode::SUCCESS),
        (Unstarted::SaveFailed, ExitCode::from(launchd::EXIT_RETRY)),
    ] {
        let dir = crate::test_scratch_dir("app");
        let paths = scratch_paths(dir.path());
        assert_eq!(unstarted.exit(&paths), code, "{unstarted:?}");
        assert_eq!(
            crate::trouble::read(&paths).map(|trouble| trouble.why),
            Some(unstarted.why().to_string()),
            "{unstarted:?}"
        );
    }
}

/// A quit from outside the menu (a person's SIGTERM, Activity Monitor) stops the mug and
/// leaves the saved mode on, forever or a span, so the window's watch or the next login starts
/// it again; a restart an update asked for gives way to it. Only the menu's Quit saves off.
#[test]
fn a_quit_from_outside_stops_the_mug_and_keeps_the_saved_mode() {
    let hour = Mode::chosen(Choice::Hours1, NOW);
    for (choice, mode) in [(Choice::Forever, Mode::Forever), (Choice::Hours1, hour)] {
        for restarting in [false, true] {
            let dir = crate::test_scratch_dir("app");
            let mut mug = scratch_mug(dir.path());
            let _ = mug.choose(MenuAction::Activate(choice), NOW);
            assert_eq!(saved(&mug), Some(mode));
            if restarting {
                mug.exit = Some(Exit::Restart);
            }
            let mug = RefCell::new(mug);
            let (next, stops) = stop_from_outside(&mug);
            let mug = mug.into_inner();
            assert_eq!(
                next,
                (!restarting).then_some(Next::Stop(Exit::Quit)),
                "{choice:?}"
            );
            assert!(
                stops,
                "{choice:?}, {restarting}: the delegate cancels -terminate:"
            );
            assert_eq!(
                mug.exit,
                Some(Exit::Quit),
                "{choice:?}, {restarting}: it stops"
            );
            assert_eq!(saved(&mug), Some(mode), "{choice:?}, {restarting}: kept");
            assert!(
                state::load(&mug.paths.state_file(), NOW).is_on(NOW),
                "{choice:?}, {restarting}: the next start keeps the Mac awake"
            );
        }
    }
    let dir = crate::test_scratch_dir("app");
    let mut mug = scratch_mug(dir.path());
    let _ = mug.choose(MenuAction::Activate(Choice::Forever), NOW);
    let mug = RefCell::new(mug);
    let held = mug.borrow_mut();
    assert_eq!(
        stop_from_outside(&mug),
        (None, false),
        "a borrowed mug is left to its holder"
    );
    drop(held);
    let mut mug = mug.into_inner();
    assert_eq!(mug.exit, None);
    assert_eq!(mug.choose(MenuAction::Quit, NOW), Next::Stop(Exit::Quit));
    assert_eq!(
        saved(&mug),
        Some(Mode::Off),
        "the menu's Quit is the person's off"
    );
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
    assert_eq!(Exit::Retry.code(), ExitCode::from(launchd::EXIT_RETRY));
}

#[test]
fn only_a_writers_on_after_a_saved_quit_restarts_the_mug() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = crate::test_scratch_dir("app");
    let quit = ExitCode::SUCCESS;
    let restart = Exit::Restart.code();
    let mut mug = scratch_mug(dir.path());
    let (file, lock, job) = (mug.paths.state_file(), mug.paths.lock_file(), Some(LABEL));
    let held = || Instance::try_hold(&lock).expect("hold").expect("free");
    assert!(
        mug.handled(|m| m.choose(MenuAction::Activate(Choice::Forever), NOW))
            .is_some()
    );
    assert_eq!(
        mug.handled(|m| m.choose(MenuAction::Quit, NOW)),
        Some(Next::Stop(Exit::Quit))
    );
    assert!(mug.quit_off, "the Quit saved off");
    assert_eq!(
        let_go(quit, job, mug.quit_off, held(), &mug.paths, || NOW),
        quit,
        "nobody wrote since"
    );
    state::save_at(&file, Mode::Forever, NOW + 1).expect("a window's forever");
    assert_eq!(
        let_go(quit, job, mug.quit_off, held(), &mug.paths, || NOW + 1),
        restart
    );
    assert_eq!(
        let_go(quit, None, mug.quit_off, held(), &mug.paths, || NOW + 1),
        quit,
        "a mug run by hand has no job to start it again"
    );
    let unbooted = |mode| state::encode(&state::Saved { mode, boot: None }, NOW);
    std::fs::write(&file, unbooted(Mode::Off)).expect("an off with no boot");
    assert_eq!(
        let_go(quit, job, mug.quit_off, held(), &mug.paths, || NOW + 1),
        quit,
        "the Quit's own off, saved with no boot id, is read as off, not restored to forever"
    );
    std::fs::write(&file, unbooted(Mode::Forever)).expect("a forever with no boot");
    assert_eq!(
        let_go(quit, job, mug.quit_off, held(), &mug.paths, || NOW + 1),
        restart
    );

    let mut mug = scratch_mug(dir.path());
    assert!(
        mug.handled(|m| m.choose(MenuAction::Activate(Choice::Forever), NOW))
            .is_some()
    );
    let state_dir = file.parent().expect("state dir");
    std::fs::set_permissions(state_dir, std::fs::Permissions::from_mode(0o555)).expect("ro");
    let ended = mug.handled(|m| m.choose(MenuAction::Quit, NOW));
    std::fs::set_permissions(state_dir, std::fs::Permissions::from_mode(0o755)).expect("rw");
    assert_eq!(ended, Some(Next::Stop(Exit::Quit)));
    assert!(!mug.quit_off, "the Quit's save failed");
    assert_eq!(saved(&mug), Some(Mode::Forever), "the earlier mode stands");
    assert_eq!(
        let_go(quit, job, mug.quit_off, held(), &mug.paths, || NOW),
        quit,
        "the mug's own earlier forever is no writer's"
    );

    let mut mug = scratch_mug(dir.path());
    mug.origin_serves = |_| Some(false);
    std::fs::write(&mug.origin, b"build 1").expect("exe");
    mug.watch = Some(Watch::start(mug.origin.clone(), &mug.origin));
    std::fs::write(&mug.origin, b"build two").expect("exe");
    assert!(
        mug.handled(|m| m.choose(MenuAction::Activate(Choice::Forever), NOW))
            .is_some()
    );
    assert_eq!(
        mug.handled(|m| m.tick(NOW + 1, false)),
        Some(Next::Stop(Exit::Quit)),
        "an origin with no acaffeine verb retires the mug"
    );
    assert!(!mug.quit_off, "a retirement is no person's Quit");
    assert_eq!(
        let_go(quit, job, mug.quit_off, held(), &mug.paths, || NOW + 1),
        quit
    );
}

/// A job's mug ending after the person's Quit takes the turn while it still holds its lock,
/// then, under the turn, removes its copy, empties and lets go of the lock, and reads the mode
/// ([`let_go`]): an on saved just after the let-go, when a writer's start finds no mug in the
/// lock and waits for this one to end, still ends it for a restart, and the lock names the job
/// again for it, past another's hold, as it does for an update restart. Every other end
/// empties the lock, reads nothing, and a failure keeps its code.
#[test]
fn a_quitting_job_lets_go_of_its_lock_before_it_reads_the_mode() {
    let dir = crate::test_scratch_dir("app");
    let paths = scratch_mug(dir.path()).paths;
    let (lock, file, session) = (paths.lock_file(), paths.state_file(), paths.session_dir());
    let held = || {
        let mut held = Instance::try_hold(&lock).expect("hold").expect("free");
        held.set_label(LABEL).expect("label");
        held
    };
    let named = || crate::instance::last_holder(&lock);
    let label = || named().and_then(|holder| holder.label);
    let turn_free = || {
        std::fs::create_dir_all(&session).expect("session folder");
        state::hold_dir(&session, Duration::ZERO, || {})
            .expect("turn")
            .is_some()
    };
    let lock_held = || Instance::try_hold_unnamed(&lock).expect("probe").is_none();
    state::save_at(&file, Mode::Off, NOW).expect("the Quit's off");
    let seen = RefCell::new(Vec::new());
    let another = RefCell::new(None);
    let code = let_go_with(
        ExitCode::SUCCESS,
        Some(LABEL),
        true,
        held(),
        &paths,
        Ending {
            turn: || {
                seen.borrow_mut().push(("turn", turn_free(), lock_held()));
                crate::cli::change(&paths, |why| panic!("{why}"))
            },
            discard: || {
                seen.borrow_mut()
                    .push(("discard", turn_free(), lock_held()))
            },
            now: || {
                seen.borrow_mut().push(("read", turn_free(), lock_held()));
                assert_eq!(named(), None, "let go naming no mug");
                state::save_at(&file, Mode::Forever, NOW + 1).expect("a writer's on");
                *another.borrow_mut() = Instance::try_hold_unnamed(&lock).expect("probe");
                NOW + 1
            },
            pause: || {
                let held = another.borrow_mut().take().is_some();
                seen.borrow_mut().push(("pause", turn_free(), held));
            },
            // The first pause lets the lock go, so naming the job again must happen: the
            // one-minute hang detector, not the live mug's second.
            name_within: Duration::from_secs(60),
        },
    );
    assert_eq!(
        *seen.borrow(),
        [
            ("turn", true, true),
            ("discard", false, true),
            ("read", false, false),
            ("pause", false, true),
        ],
        "the turn taken with the lock held; under it the copy removed, the lock let go and \
         the mode read, then the lock tried again past another's hold"
    );
    assert_eq!(code, Exit::Restart.code(), "the on after the let-go");
    assert_eq!(
        label().as_deref(),
        Some(LABEL),
        "the job named again for the restart"
    );
    assert!(
        Instance::try_hold_unnamed(&lock).expect("hold").is_some(),
        "and let go"
    );
    assert!(turn_free(), "the turn let go");

    state::save_at(&file, Mode::Off, NOW).expect("the Quit's off");
    let read_in_turn = std::cell::Cell::new(None);
    assert_eq!(
        let_go(ExitCode::SUCCESS, Some(LABEL), true, held(), &paths, || {
            read_in_turn.set(Some(!turn_free()));
            NOW
        }),
        ExitCode::SUCCESS
    );
    assert_eq!(
        read_in_turn.get(),
        Some(true),
        "the real let_go reads under the verbs' turn"
    );
    assert!(turn_free(), "and lets it go");
    assert_eq!(named(), None, "emptied, read off");
    assert_eq!(
        let_go(
            Exit::Restart.code(),
            Some(LABEL),
            false,
            held(),
            &paths,
            || NOW
        ),
        Exit::Restart.code()
    );
    assert_eq!(label().as_deref(), Some(LABEL), "an update restart");
    for (code, job, quit_off) in [
        (ExitCode::SUCCESS, Some(LABEL), false),
        (ExitCode::SUCCESS, None, true),
        (ExitCode::FAILURE, Some(LABEL), true),
    ] {
        state::save_at(&file, Mode::Forever, NOW).expect("on");
        assert_eq!(
            let_go(code, job, quit_off, held(), &paths, || panic!("no read")),
            code
        );
        assert_eq!(named(), None, "emptied");
    }
}

/// The label of a job in tests: a dev bundle's, which [`crate::launchd::is_label`] accepts.
const LABEL: &str = "com.aterm.aterm.acaffeine.dev.0123abcd";

fn scratch_mug(dir: &std::path::Path) -> Mug {
    Mug {
        paths: Paths {
            state_dir: dir.join("state"),
            home: dir.join("home"),
            logs_dir: dir.join("logs"),
            local_volume: |_| true,
        },
        label: "com.aterm.aterm.acaffeine.test".into(),
        origin: dir.join("aterm"),
        version: "0.0.0".into(),
        mode: Mode::Off,
        awake: None,
        awake_begin: Awake::begin,
        lid_awake: false,
        lid_hold: None,
        lid_begin: |_| panic!("only Keep Awake with Lid Closed asks for its assertion"),
        machine: Machine::default,
        going_to_sleep: false,
        woke_at: None,
        hold: Hold::Off,
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
        quit_off: false,
        shown: None,
    }
}

fn saved(mug: &Mug) -> Option<Mode> {
    state::read(&mug.paths.state_file(), NOW).map(|s| s.mode)
}

#[test]
fn a_click_keeps_the_mac_awake_until_the_next_and_saves_each() {
    let dir = crate::test_scratch_dir("app");
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

thread_local! {
    /// The lid a test's machine door reads ([`lid_door`]).
    static LID: Cell<bool> = const { Cell::new(false) };
}

/// A machine door that reads [`LID`], in a full wake.
fn lid_door() -> Machine {
    Machine {
        lid_closed: LID.get(),
        ..Machine::default()
    }
}

/// By default a lid's close pauses the mug: it lets go of its activity, the mode and the
/// saved file stay on, the tooltip and the menu say it is paused, and the lid's open holds
/// again; the lid's own assertion is never asked for.
#[test]
fn a_closed_lid_pauses_the_hold_and_keeps_the_mode() {
    let dir = crate::test_scratch_dir("app");
    let mut mug = scratch_mug(dir.path());
    mug.machine = lid_door;
    LID.set(false);
    assert_eq!(mug.set(Mode::Forever, NOW), Next::Tick(TICK_SECS));
    assert!(mug.awake.is_some());
    LID.set(true);
    assert_eq!(mug.tick(NOW + 60, false), Next::Tick(TICK_SECS));
    assert!(mug.awake.is_none(), "the closed lid lets go");
    assert!(mug.lid_hold.is_none());
    assert_eq!(mug.mode, Mode::Forever, "the mode stays on");
    assert_eq!(saved(&mug), Some(Mode::Forever));
    assert_eq!(mug.hold, Hold::Paused(crate::model::Pause::LidClosed));
    let (lit, tip) = mug.shown.clone().expect("shown");
    assert!(!lit);
    assert!(tip.contains("lid is closed"), "{tip}");
    assert!(
        mug.rows(NOW + 60).contains(&Row::Info(
            menu::pause_line(crate::model::Pause::LidClosed).into()
        )),
        "the menu says it"
    );
    assert!(
        crate::trouble::read(&mug.paths).is_none(),
        "a pause is no trouble"
    );
    LID.set(false);
    assert_eq!(mug.tick(NOW + 120, false), Next::Tick(TICK_SECS));
    assert!(mug.awake.is_some(), "the open lid holds again");
    assert_eq!(mug.hold, Hold::Idle);
}

thread_local! {
    /// Whether a test's machine door reads a dark wake ([`wake_door`]).
    static DARK: Cell<bool> = const { Cell::new(false) };
}

/// A machine door that reads [`LID`] and [`DARK`].
fn wake_door() -> Machine {
    Machine {
        lid_closed: LID.get(),
        dark_wake: DARK.get(),
        ..Machine::default()
    }
}

/// The lid's open as an Apple-silicon Mac makes it (2026-11-09, macOS 27.2): every wake
/// signal the mug hears comes while the wake is still dark, and graphics come a fraction of
/// a second later with nothing to say so. The mug looks again each second while the wake
/// settles and holds as soon as it reads a full wake, the mode never having left forever;
/// a pause still there once the wake has settled, a dark wake, goes back to the minute tick,
/// and a will-sleep ends the settling.
/// NEGATIVE CONTROL: `Mug::next_tick` without its settling arm answers [`TICK_SECS`] to the
/// wake, which the first assertion after it refuses (the mug then stayed paused until a
/// click, as it did on every lid open that day).
#[test]
fn a_lid_open_that_wakes_dark_first_holds_again_once_graphics_come() {
    let dir = crate::test_scratch_dir("app");
    let mut mug = scratch_mug(dir.path());
    mug.machine = wake_door;
    LID.set(false);
    DARK.set(false);
    let _ = mug.set(Mode::Forever, NOW);
    assert_eq!(mug.hold, Hold::Idle);
    LID.set(true);
    assert_eq!(
        mug.power(aterm_uds::power::Power::WillSleep, NOW + 10, false),
        Next::Keep
    );
    assert!(mug.awake.is_none(), "let go before the sleep");
    LID.set(false);
    DARK.set(true);
    let woke = NOW + 70;
    assert_eq!(
        mug.power(aterm_uds::power::Power::Woke, woke, false),
        Next::Tick(SETTLE_TICK_SECS),
        "a dark read just after the wake is looked at again in a second"
    );
    assert_eq!(mug.hold, Hold::Paused(crate::model::Pause::Asleep));
    assert_eq!(
        mug.handled(|m| m.woke(woke, false)),
        Some(Next::Tick(SETTLE_TICK_SECS)),
        "NSWorkspaceDidWakeNotification, in the same dark part"
    );
    DARK.set(false);
    assert_eq!(mug.tick(woke + 1, false), Next::Tick(TICK_SECS));
    assert_eq!(mug.hold, Hold::Idle, "graphics came: it holds again");
    assert_eq!(mug.after_wake(woke + 1), " 1 s after the wake");
    assert!(mug.awake.is_some());
    assert_eq!(mug.mode, Mode::Forever);

    DARK.set(true);
    let _ = mug.power(aterm_uds::power::Power::WillSleep, NOW + 200, false);
    let dark = NOW + 300;
    assert_eq!(
        mug.power(aterm_uds::power::Power::Woke, dark, false),
        Next::Tick(SETTLE_TICK_SECS)
    );
    let settled = dark + crate::model::SETTLE_SECS;
    assert_eq!(
        mug.tick(dark + (crate::model::SETTLE_SECS - 1), false),
        Next::Tick(SETTLE_TICK_SECS)
    );
    assert_eq!(
        mug.tick(settled, false),
        Next::Tick(TICK_SECS),
        "a wake still dark once settled is a dark wake: the minute tick reads it"
    );
    assert!(mug.awake.is_none());
    let _ = mug.power(aterm_uds::power::Power::Woke, settled + 10, false);
    assert_eq!(
        mug.power(aterm_uds::power::Power::WillSleep, settled + 11, false),
        Next::Keep
    );
    assert_eq!(mug.woke_at, None, "a will-sleep ends the settling");
    assert_eq!(mug.after_wake(settled + 12), "");
    assert_eq!(mug.next_tick(settled + 12), TICK_SECS);
}

/// A [`aterm_uds::power::Power::Woke`] with no will-sleep before it is an idle sleep the
/// system pondered and dropped (`kIOMessageSystemWillNotSleep`): it looks as a tick does and
/// starts no settling, so a lid closed on an external display, which pauses the mug, is not
/// looked at every second each time the idle sleep is dropped.
/// NEGATIVE CONTROL: `Mug::power` without its `if self.going_to_sleep` guard records a wake,
/// and the first assertion reads [`SETTLE_TICK_SECS`].
#[test]
fn a_dropped_idle_sleep_is_no_wake() {
    let dir = crate::test_scratch_dir("app");
    let mut mug = scratch_mug(dir.path());
    mug.machine = wake_door;
    LID.set(true);
    DARK.set(false);
    let _ = mug.set(Mode::Forever, NOW);
    assert_eq!(
        mug.power(aterm_uds::power::Power::Woke, NOW + 5, false),
        Next::Tick(TICK_SECS)
    );
    assert_eq!(mug.hold, Hold::Paused(crate::model::Pause::LidClosed));
    assert_eq!(mug.woke_at, None);
    LID.set(false);
}

/// Keep Awake with Lid Closed, chosen in the menu, is saved and holds `PreventSystemSleep`
/// besides the activity, the lid closed too; a will-sleep lets go of both, the mode staying
/// on, and the wake holds both again. Turned off with the lid closed, the mug pauses.
#[test]
fn the_lid_option_holds_the_system_assertion_until_macos_sleeps() {
    const REASON: &str = "aterm acaffeine lid option test";
    let dir = crate::test_scratch_dir("app");
    let mut mug = scratch_mug(dir.path());
    mug.machine = lid_door;
    mug.awake_begin = |_| Awake::begin(REASON);
    mug.lid_begin = |_| aterm_sysprobe::power::hold(crate::held::LID_ASSERTION, REASON);
    let listed = || {
        let mut kinds = super::super::awake::tests::assertions_named(REASON);
        kinds.sort();
        kinds
    };
    LID.set(false);
    let _ = mug.set(Mode::Forever, NOW);
    assert_eq!(mug.choose(MenuAction::ToggleLid, NOW), Next::Keep);
    assert!(mug.lid_awake);
    assert!(state::load_lid(&mug.paths.lid_file()), "saved");
    assert!(mug.rows(NOW).contains(&Row::Action {
        label: menu::LID_LABEL.into(),
        action: MenuAction::ToggleLid,
        checked: true,
    }));
    LID.set(true);
    let _ = mug.tick(NOW + 60, false);
    assert_eq!(mug.hold, Hold::LidClosedToo);
    assert_eq!(
        listed(),
        [
            crate::held::LID_ASSERTION,
            "PreventUserIdleDisplaySleep",
            crate::held::SYSTEM_ASSERTION
        ]
    );
    assert_eq!(
        mug.power(aterm_uds::power::Power::WillSleep, NOW + 61, false),
        Next::Keep
    );
    assert!(mug.awake.is_none() && mug.lid_hold.is_none());
    assert_eq!(listed(), Vec::<String>::new(), "let go before the sleep");
    assert_eq!(mug.mode, Mode::Forever);
    let _ = mug.power(aterm_uds::power::Power::Woke, NOW + 600, false);
    assert_eq!(mug.hold, Hold::LidClosedToo, "the wake holds again");
    assert!(mug.awake.is_some() && mug.lid_hold.is_some());
    let _ = mug.choose(MenuAction::ToggleLid, NOW + 601);
    assert!(!state::load_lid(&mug.paths.lid_file()));
    assert_eq!(mug.hold, Hold::Paused(crate::model::Pause::LidClosed));
    assert_eq!(listed(), Vec::<String>::new());
}

/// The lid option a writer saved is what the mug reads when it takes the file again.
#[test]
fn a_writers_lid_choice_is_read_with_the_mode() {
    let dir = crate::test_scratch_dir("app");
    let mut mug = scratch_mug(dir.path());
    state::save_lid(&mug.paths.lid_file(), true).expect("lid on");
    assert!(mug.take(NOW));
    assert!(mug.lid_awake);
    state::save_lid(&mug.paths.lid_file(), false).expect("lid off");
    assert!(mug.take(NOW));
    assert!(!mug.lid_awake);
}

#[test]
fn a_click_on_a_span_that_has_just_ended_turns_it_off() {
    let dir = crate::test_scratch_dir("app");
    let mut mug = scratch_mug(dir.path());
    let _ = mug.choose(MenuAction::Activate(Choice::Hours1), NOW);
    assert!(mug.awake.is_some());
    // Past the deadline, before the tick that ends the span: the mug is still lit, and a click
    // on it turns it off, as the span's end does.
    let end = NOW + HOUR;
    assert!(matches!(mug.mode, Mode::Until { .. }));
    assert_eq!(mug.toggle(end), Next::Tick(TICK_SECS));
    assert_eq!(mug.mode, Mode::Off);
    assert!(mug.awake.is_none());
    assert_eq!(saved(&mug), Some(Mode::Off));
}

/// An activity macOS refused leaves the person's mode as it is, on in the file and in
/// `status`: the mug shows itself empty, records why, and asks again at each tick until it
/// holds, when the trouble goes. A save that fails stops the mug for launchd to start again.
#[test]
fn a_refused_activity_keeps_the_mode_and_is_asked_again_each_tick() {
    thread_local! {
        static GRANT: Cell<bool> = const { Cell::new(false) };
    }
    const REASON: &str = "aterm acaffeine refused-activity retry test";
    for requested in [Mode::Forever, Mode::activate(Some(HOUR), NOW)] {
        let dir = crate::test_scratch_dir("app");
        let mut mug = scratch_mug(dir.path());
        GRANT.set(false);
        mug.awake_begin = |reason| GRANT.get().then(|| Awake::begin(reason)).flatten();
        assert_eq!(mug.set(requested, NOW), Next::Tick(TICK_SECS));
        assert_eq!(mug.mode, requested, "the person's choice stands");
        assert!(mug.awake.is_none());
        assert_eq!(saved(&mug), Some(requested));
        assert_eq!(
            mug.shown,
            Some((false, menu::NOT_HELD_TIP.to_string())),
            "the empty mug, saying it could not"
        );
        let read = state::load(&mug.paths.state_file(), NOW);
        assert!(
            crate::cli::status_text(read, NOW, Some(42), Startup::Off, "l.x", false)
                .starts_with("acaffeine=on\n"),
            "`status` reads the file"
        );
        assert!(
            crate::trouble::read(&mug.paths).is_some(),
            "the refusal is recorded"
        );
        assert_eq!(
            menu::tooltip(mug.mode, NOW, Face::Refused),
            menu::NOT_HELD_TIP
        );
        mug.awake_begin = |_| GRANT.get().then(|| Awake::begin(REASON)).flatten();
        GRANT.set(true);
        assert_eq!(mug.tick(NOW + 60, false), Next::Tick(TICK_SECS));
        assert!(mug.awake.is_some(), "the tick asked again: {requested:?}");
        assert!(
            crate::trouble::read(&mug.paths).is_none(),
            "and the trouble went"
        );
        drop(mug);
    }
    let dir = crate::test_scratch_dir("app");
    let mut mug = scratch_mug(dir.path());
    let blocked = dir.path().join("not-a-directory");
    std::fs::write(&blocked, b"blocked").expect("blocked state directory");
    mug.paths.state_dir = blocked;
    mug.awake_begin = |_| Awake::begin(REASON);
    assert_eq!(
        mug.handled(|m| m.toggle(NOW)),
        Some(Next::Stop(Exit::Retry))
    );
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

/// A span ends in off: the hold, the file and the tooltip let the Mac sleep at its end.
#[test]
fn a_timed_choice_ends_in_off_and_quit_turns_off_and_stops() {
    let dir = crate::test_scratch_dir("app");
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
    assert!(menu::tooltip(mug.mode, NOW + HOUR + 1, Face::Refused).contains("is off"));
    let _ = mug.choose(MenuAction::Activate(Choice::Forever), NOW);
    assert!(mug.awake.is_some());
    assert_eq!(mug.choose(MenuAction::Quit, NOW), Next::Stop(Exit::Quit));
    assert_eq!(saved(&mug), Some(Mode::Off));
    assert!(mug.awake.is_none());
}

/// With the wall clock set back a day mid-span, the hour still ends on the boot clock, and
/// the hold and the file end together.
#[test]
fn a_clock_set_back_ends_the_span_at_the_hours_end() {
    let dir = crate::test_scratch_dir("app");
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
    let dir = crate::test_scratch_dir("app");
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
    let dir = crate::test_scratch_dir("app");
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

/// A file with no `ends`, taken at the start or from a writer, is saved back on the boot clock,
/// so a read after the date moves a day has the time the mug holds.
#[test]
fn a_file_with_no_ends_is_saved_back_on_the_boot_clock() {
    let dir = crate::test_scratch_dir("app");
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
        std::fs::write(&file, &earlier).expect("a file with no ends");
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

/// A deadline a day away on a span of an hour, taken at the start or from a writer, is saved
/// in to the hour, so a read half an hour on has the half hour the mug holds.
#[test]
fn a_deadline_past_its_span_is_saved_in_to_it() {
    let dir = crate::test_scratch_dir("app");
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

/// After the date moves a day, the next tick saves the mode again so the file's wall-clock end
/// is where the wall clock now puts the mug's end; a tick with the date unchanged, and the one
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
        let dir = crate::test_scratch_dir("app");
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

/// A writer's change saved after the mug's and a change of the date, not yet notified: the
/// next tick saves nothing of the mug's over it.
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
        let dir = crate::test_scratch_dir("app");
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

/// A writer's save as the hour ends, not yet notified (on with no end, a new hour, or one on
/// clocks a second ahead): the tick that ends the hour takes the writer's mode and leaves its
/// file as it is; with no writer, it ends in off.
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
        let dir = crate::test_scratch_dir("app");
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
        assert_eq!(mug.awake.is_some(), want.is_on(at), "{writers:?}");
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

/// [`state::take_saving`] whose save-back, which must be due, fails for real: the state folder
/// is moved aside and a file put in its place for the save, then put back.
fn take_with_a_failed_save(path: &Path, now: Clocks) -> std::io::Result<Mode> {
    let (mode, was) = state::take(path, now);
    let was = was.expect("a file to save back");
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
}

/// A span whose save-back fails with the menu open lets go of the activity on the tick
/// itself, not at the stop, which waits for the menu to close.
#[test]
fn a_span_that_ends_on_a_failed_save_lets_go_at_once() {
    let dir = crate::test_scratch_dir("app");
    let mut mug = scratch_mug(dir.path());
    let _ = mug.choose(MenuAction::Activate(Choice::Hours1), NOW);
    assert!(mug.awake.is_some());
    let file = mug.paths.state_file();
    let before = std::fs::read(&file).expect("the saved span");
    mug.take_saved = take_with_a_failed_save;
    let end = NOW + HOUR + 1;
    assert_eq!(
        state::load(&file, end),
        Mode::Off,
        "the saved span has ended"
    );
    assert_eq!(mug.tick(end, true), Next::Stop(Exit::Retry));
    assert_eq!(mug.mode, Mode::Off);
    assert!(mug.awake.is_none());
    assert_eq!(
        std::fs::read(&file).expect("the original state file"),
        before
    );
    assert!(!mug.paths.state_dir.with_extension("preserved").exists());
}

/// The same tick with a writer's span in the file, not yet notified, that the save-back would
/// bring in to its hour (a deadline a day off): it reads a span, not forever, so the mug, which
/// could not save it, stops for launchd to start it again ([`Exit::Retry`]), and lets go of its
/// own ended span on the tick itself, not at the stop, which waits for the menu to close.
#[test]
fn a_failed_save_of_a_writers_span_as_the_mugs_ends_lets_go_at_once() {
    let dir = crate::test_scratch_dir("app");
    let mut mug = scratch_mug(dir.path());
    let _ = mug.choose(MenuAction::Activate(Choice::Hours1), NOW);
    assert!(mug.awake.is_some());
    let file = mug.paths.state_file();
    let end = NOW + HOUR + 1;
    let far = format!(
        "mode=until\nends={}\nspan={HOUR}\ndeadline={}\nboot={}\n",
        end.since_boot + 24 * HOUR,
        end.wall + 24 * HOUR,
        state::boot_text(crate::instance::boot_id().expect("this boot"))
    );
    std::fs::write(&file, &far).expect("a writer's far deadline");
    mug.take_saved = take_with_a_failed_save;
    assert_eq!(
        state::load(&file, end).remaining(end),
        Some(HOUR),
        "it reads a span"
    );
    assert_eq!(mug.tick(end, true), Next::Stop(Exit::Retry));
    assert_eq!(mug.mode, Mode::Off);
    assert!(mug.awake.is_none());
    assert_eq!(
        std::fs::read_to_string(&file).expect("the writer's file"),
        far
    );
    assert!(!mug.paths.state_dir.with_extension("preserved").exists());
}

/// The tick that ends the mug's own span over a file that reads forever as it stands (a
/// writer's forever, not yet notified) and whose take fails to save: `status`, a bridge and
/// the next mug read the file as forever, so the mug holds forever and runs on.
#[test]
fn a_failed_save_on_a_file_that_reads_forever_keeps_the_mac_awake() {
    let dir = crate::test_scratch_dir("app");
    let mut mug = scratch_mug(dir.path());
    let _ = mug.choose(MenuAction::Activate(Choice::Hours1), NOW);
    assert!(mug.awake.is_some());
    let file = mug.paths.state_file();
    std::fs::write(&file, "mode=forever\n").expect("a writer's forever");
    mug.take_saved = |_, _| Err(std::io::Error::other("a full disk"));
    let end = NOW + HOUR + 1;
    assert_eq!(state::load(&file, end), Mode::Forever, "it reads forever");
    assert_eq!(mug.tick(end, true), Next::Tick(TICK_SECS));
    assert_eq!(mug.mode, Mode::Forever);
    assert!(mug.awake.is_some());
    assert_eq!(
        std::fs::read_to_string(&file).expect("the writer's file"),
        "mode=forever\n"
    );
}

#[test]
fn enable_at_startup_writes_and_removes_the_login_plist() {
    let dir = crate::test_scratch_dir("app");
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
    assert_eq!(mug.choose(MenuAction::ToggleStartup, NOW), Next::Keep);
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
    assert!(
        launchd::startup_turned_off(&mug.paths, &mug.label),
        "the person's off is kept, so no launch turns it back on"
    );
    let _ = mug.set_startup(true);
    assert_eq!(mug.set_startup(true), Next::Keep);
    assert!(checked(&mug), "asked on, stays on");
    assert!(!launchd::startup_turned_off(&mug.paths, &mug.label));
    let _ = mug.choose(MenuAction::ToggleStartup, NOW);
    assert!(!checked(&mug), "the control");
}

#[test]
fn a_blocked_enable_at_startup_opens_login_items() {
    thread_local! {
        static OPENED: Cell<u32> = const { Cell::new(0) };
    }
    let dir = crate::test_scratch_dir("app");
    let mut mug = scratch_mug(dir.path());
    mug.doors.blocks = |_| true;
    mug.open_login_items = || OPENED.with(|n| n.set(n.get() + 1));
    let plist = mug.paths.startup_plist(&mug.label);
    assert_eq!(mug.set_startup(true), Next::Keep);
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
    let _ = mug.set_startup(true);
    std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(0o755)).expect("rw");
    assert_eq!(OPENED.with(Cell::get), 3, "a failed write still opens it");
    let _ = mug.set_startup(false);
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
    let dir = crate::test_scratch_dir("app");
    let mut mug = scratch_mug(dir.path());
    mug.doors.disabled = |_| DISABLED.with(Cell::get);
    mug.doors.enable = |label| {
        assert_eq!(label, "com.aterm.aterm.acaffeine.test");
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
    let _ = mug.set_startup(true);
    assert_eq!(LIFTED.with(Cell::get), 1, "nothing to lift once on");
}

#[test]
fn an_update_waits_for_the_menu_and_the_deadline_does_not() {
    let dir = crate::test_scratch_dir("app");
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
    assert!(mug.awake.is_none(), "the hour ended with the menu open");
    assert_eq!(mug.mode, Mode::Off);
    assert_eq!(
        mug.handled(|m| m.tick(NOW + HOUR + 60, false)),
        Some(Next::Stop(Exit::Restart))
    );
}

#[test]
fn a_replaced_origin_ends_the_app_for_a_restart() {
    let dir = crate::test_scratch_dir("app");
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
    const REASON: &str = "aterm acaffeine restart bridge test";
    let held = || !super::super::awake::tests::assertions_named(REASON).is_empty();
    let dir = crate::test_scratch_dir("app");
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

/// The first apply that holds the activity, or finds the mode off, lets a restart's bridge
/// go; one macOS refused keeps it holding until a tick holds it.
#[test]
fn the_first_apply_that_holds_lets_the_bridge_go() {
    for (mode, granted) in [
        (Mode::Forever, true),
        (Mode::Off, true),
        (Mode::Forever, false),
    ] {
        let dir = crate::test_scratch_dir("app");
        let mut mug = scratch_mug(dir.path());
        if !granted {
            mug.awake_begin = |_| None;
        }
        state::save(&mug.paths.state_file(), mode).expect("saved");
        std::fs::write(mug.paths.bridge_file(), b"4242\n").expect("bridge file");
        mug.mode = state::load(&mug.paths.state_file(), NOW);
        mug.apply(NOW);
        let lets_go = granted || mode == Mode::Off;
        assert_eq!(
            mug.awake.is_some(),
            mode == Mode::Forever && granted,
            "{mode:?}"
        );
        assert_eq!(
            mug.paths.bridge_file().exists(),
            !lets_go,
            "{mode:?} {granted}"
        );
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
    let dir = crate::test_scratch_dir("app");
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
