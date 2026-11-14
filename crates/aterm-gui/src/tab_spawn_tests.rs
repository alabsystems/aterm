// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::test_support::book;
use super::*;
use crate::SinkWriter;
use aterm_window::event::Ime;
use std::sync::Arc;

#[test]
fn a_tab_command_override_is_scoped_to_its_own_spawn_factory() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    let mut app = App::headless_for_test();
    let inherited = vec![
        OsString::from("configured-program"),
        OsString::from("configured arg"),
    ];
    app.session_factory.exec_command = Some(inherited.clone());
    app.session_factory.reader_after_install = false;
    app.session_factory.integrate = true;
    let command = vec![
        OsString::from("/bin/sh"),
        OsString::from("argument with spaces"),
        OsString::from_vec(vec![b'a', 0xff]),
    ];
    let explicit = tab_spawn_factory(app.session_factory.clone(), Some(command.clone()), false);
    assert_eq!(explicit.exec_command.as_ref(), Some(&command));
    assert!(explicit.reader_after_install);
    assert!(!explicit.integrate);
    assert_eq!(app.session_factory.exec_command.as_ref(), Some(&inherited));
    assert!(!app.session_factory.reader_after_install);
    assert!(app.session_factory.integrate);

    let ordinary = tab_spawn_factory(app.session_factory.clone(), None, false);
    assert_eq!(ordinary.exec_command.as_ref(), Some(&inherited));
    assert!(ordinary.reader_after_install);
    assert!(ordinary.integrate);
    app.session_factory.exec_command = None;
    assert!(
        tab_spawn_factory(app.session_factory.clone(), None, false)
            .exec_command
            .is_none()
    );
}

#[test]
fn a_worker_uses_its_own_integrated_shell_without_changing_the_command_instance() {
    use std::ffi::OsString;

    let mut app = App::headless_for_test();
    let command = vec![OsString::from("/bin/cat")];
    app.session_factory.exec_command = Some(command.clone());
    app.session_factory.integrate = true;
    app.session_factory.shell_override = Some("/bin/zsh".into());
    app.session_factory.shell_args = Some(vec!["-l".into(), "-i".into()]);
    app.session_factory.cwd = Some("/work".into());

    let worker = tab_spawn_factory(app.session_factory.clone(), None, true);
    assert!(worker.exec_command.is_none());
    assert!(worker.integrates_shell());
    assert_eq!(worker.shell_override, app.session_factory.shell_override);
    assert_eq!(worker.shell_args, app.session_factory.shell_args);
    assert_eq!(worker.cwd, app.session_factory.cwd);
    assert_eq!(app.session_factory.exec_command, Some(command.clone()));
    assert!(!app.session_factory.integrates_shell());

    let ordinary = tab_spawn_factory(app.session_factory.clone(), None, false);
    assert_eq!(ordinary.exec_command, Some(command.clone()));
    assert!(!ordinary.integrates_shell());

    let explicit = tab_spawn_factory(app.session_factory.clone(), Some(command.clone()), true);
    assert_eq!(explicit.exec_command, Some(command));
    assert!(!explicit.integrates_shell());

    app.session_factory.integrate = false;
    let disabled = tab_spawn_factory(app.session_factory.clone(), None, true);
    assert!(disabled.exec_command.is_none());
    assert!(!disabled.integrates_shell());
}

#[test]
fn a_plain_drive_shell_is_fresh_and_clears_only_its_inherited_command() {
    let mut app = App::headless_for_test();
    app.session_factory.exec_command = Some(vec!["/bin/cat".into()]);
    app.session_factory.integrate = true;
    let ask = SpawnAsk {
        cwd: Some("/private/tmp".to_owned()),
        ..SpawnAsk::default()
    }
    .for_fleet_shell();
    assert!(!can_adopt_spare(&ask));
    assert!(ask.task.is_none(), "a plain shell has no artificial task");
    let factory = tab_spawn_factory(app.session_factory.clone(), None, ask.shell);
    assert!(factory.exec_command.is_none());
    assert!(factory.integrates_shell());
    assert!(
        app.session_factory.exec_command.is_some(),
        "the host's command stays its own"
    );
    assert!(
        can_adopt_spare(&SpawnAsk::default()),
        "ordinary tabs keep their existing policy"
    );
}

/// WHAT ANOTHER MAC'S DRIVE STARTS CARRIES THAT MAC'S MARK: a tab a fleet drive asked for, a
/// worker's or a plain shell's, runs everything with the mark of another machine's request in
/// its environment, once, whatever the host's own environment said of it, and never adopts a
/// shell that started without it. So a sign-in started there opens nothing on this Mac's
/// desk. CONTROL: a tab this Mac asked for carries none.
#[test]
fn a_fleet_drives_tab_carries_the_mark_of_another_machines_request() {
    let mark = aterm_agent::harness::remote_renew::FOR_PEER_ENV;
    let mut app = App::headless_for_test();
    let marks = |factory: &crate::spawn::SessionFactory| -> Vec<String> {
        factory
            .env_add
            .iter()
            .filter(|(name, _)| name == mark)
            .map(|(_, value)| value.clone())
            .collect()
    };
    let ordinary = tab_spawn_factory(app.session_factory.clone(), None, false);
    assert!(marks(&ordinary).is_empty(), "this Mac's own tab");
    assert_eq!(marks(&for_fleet_drive(ordinary)), ["1"]);
    // A host that itself carried another value hands the tab the mark once.
    app.session_factory
        .env_add
        .push((mark.to_owned(), String::new()));
    let fleet = for_fleet_drive(tab_spawn_factory(app.session_factory.clone(), None, true));
    assert_eq!(marks(&fleet), ["1"]);
    for ask in [
        SpawnAsk {
            fleet: true,
            ..SpawnAsk::default()
        },
        SpawnAsk {
            fleet: true,
            cwd: Some("/private/tmp".to_owned()),
            ..SpawnAsk::default()
        }
        .for_fleet_shell(),
    ] {
        assert!(
            !can_adopt_spare(&ask),
            "a shell already running has no mark"
        );
    }
}

fn piped_session(id: u64) -> (Session, std::fs::File) {
    let (read, write) = aterm_uds::pipe().expect("pipe");
    let read = std::fs::File::from(std::os::fd::OwnedFd::from(read));
    let sink = Arc::new(SinkWriter::new_owned(write.into()));
    (crate::stub_session_with_sink(id, sink), read)
}

pub(crate) fn drain(read: &std::fs::File) -> Vec<u8> {
    use std::io::Read as _;
    use std::os::fd::AsRawFd as _;
    // SAFETY: `read` is an open descriptor this test owns.
    unsafe { libc::fcntl(read.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
    let mut out = Vec::new();
    let mut buf = [0u8; 256];
    let mut r = read;
    while let Ok(n) = r.read(&mut buf) {
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    out
}

pub(crate) fn book_admitted(
    app: &mut App,
    wid: WindowId,
    admission: SpawnAdmission,
    then: TabSpawnThen,
) -> u64 {
    let id = book(app, wid, then);
    app.tab_spawns.in_flight.last_mut().unwrap().admission = admission;
    id
}

#[test]
fn summarize_waits_for_pending_spawn_settlement() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(!app.tab_spawns.summary_pending());
    let (tx, rx) = channel();
    let id = book_admitted(
        &mut app,
        wid,
        SpawnAdmission::Summary,
        TabSpawnThen::Reply {
            reply: tx.into(),
            raise: false,
        },
    );
    assert!(
        app.tab_spawns.summary_pending(),
        "a second Summarize must not book another worker"
    );
    assert!(!app.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Err(std::io::Error::other("exec refused")),
        reader_gate: None
    }));
    assert_eq!(rx.try_recv(), Ok(Err("exec refused".to_string())));
    assert!(
        !app.tab_spawns.summary_pending(),
        "failure releases the reservation"
    );
    let gone = WindowId(77);
    let id = book_admitted(
        &mut app,
        gone,
        SpawnAdmission::Summary,
        TabSpawnThen::Nothing,
    );
    assert!(app.tab_spawns.summary_pending());
    assert!(!app.install_spawned_tab(TabSpawned {
        id,
        window: gone,
        session: Ok(crate::stub_session(id)),
        reader_gate: None
    }));
    assert!(
        !app.tab_spawns.summary_pending(),
        "closed host cancels the reservation"
    );
}

#[test]
fn a_tab_landing_behind_tells_the_front_nothing_and_the_newborn_it_is_unfocused() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let enable = |app: &mut App, id: u64| {
        crate::term_lock(&app.pool.get(id).expect("landed").term).process(b"\x1b[?1004h");
        app.on_output_wake(id, Instant::now());
    };
    let front = book(&mut app, wid, TabSpawnThen::Nothing);
    let (session, front_pty) = piped_session(front);
    assert!(app.install_spawned_tab(TabSpawned {
        id: front,
        window: wid,
        session: Ok(session),
        reader_gate: None
    }));
    enable(&mut app, front);
    assert!(
        drain(&front_pty).is_empty(),
        "the focused program hears nothing"
    );

    let behind = book(&mut app, wid, TabSpawnThen::Nothing);
    app.tab_spawns
        .in_flight
        .last_mut()
        .expect("booked")
        .background = true;
    let (session, behind_pty) = piped_session(behind);
    assert!(app.install_spawned_tab(TabSpawned {
        id: behind,
        window: wid,
        session: Ok(session),
        reader_gate: None
    }));
    assert_eq!(app.windows[&wid].tab_set.len(), 3, "the tab is in");
    assert_eq!(
        app.focused_session_id(wid),
        Some(front),
        "it landed behind the front"
    );
    assert!(
        drain(&front_pty).is_empty(),
        "the front's focus never moved: no ESC[O, ESC[I pair"
    );

    enable(&mut app, behind);
    assert_eq!(drain(&behind_pty), b"\x1b[O", "the newborn is unfocused");
    assert!(drain(&front_pty).is_empty());
}

/// [`book`] for a split of `wid`'s focused pane in `dir`.
pub(crate) fn book_split(
    app: &mut App,
    wid: WindowId,
    dir: crate::pane::SplitDir,
    then: TabSpawnThen,
) -> u64 {
    let id = book(app, wid, then);
    let theme = app.inherited_theme(wid, SpawnPlace::Split(dir), None);
    if let Some(t) = app.tab_spawns.in_flight.last_mut() {
        t.place = SpawnPlace::Split(dir);
        t.theme = theme;
    }
    id
}

struct Holdout {
    identity: aterm_pty::ShellIdentity,
    master: Option<std::os::fd::OwnedFd>,
}

impl Holdout {
    fn wait_until_absent(&self) -> bool {
        let until = Instant::now() + Duration::from_secs(60);
        while self.identity.verify() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        !self.identity.verify()
    }
}

impl Drop for Holdout {
    fn drop(&mut self) {
        if self.identity.verify() {
            // SAFETY: `self.identity.pid` is our verified child's process group.
            unsafe { libc::killpg(self.identity.pid, libc::SIGKILL) };
        }
        aterm_pty::reap_shell(self.identity);
    }
}

fn hup_ignoring_session(app: &App, id: u64) -> (Session, Holdout) {
    use std::os::fd::{AsRawFd, FromRawFd};
    let command: Vec<std::ffi::OsString> = [
        "/bin/sh",
        "-c",
        "trap '' HUP; printf 'OWNED-HOLDOUT-READY\\n'; exec /bin/sleep 600",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    let shell = aterm_pty::spawn_shell_with_pid_cell_px(
        24,
        80,
        &app.session_factory.spawn_cap,
        &app.session_factory.sandbox_cap,
        &[],
        None,
        None,
        None,
        Some(&command),
        None,
        None,
        aterm_sandbox::Limits::inherit(),
        None,
        None,
    )
    .expect("the private owned PTY child starts");
    let mut holdout = Holdout {
        identity: aterm_pty::record_spawned_shell(shell.pid),
        // SAFETY: the new PTY master has no other owner yet.
        master: Some(unsafe { std::os::fd::OwnedFd::from_raw_fd(shell.master) }),
    };
    aterm_pty::set_nonblocking(shell.master, true).unwrap();
    let mut seen = Vec::new();
    let until = Instant::now() + Duration::from_secs(60);
    while Instant::now() < until
        && !seen
            .windows(b"OWNED-HOLDOUT-READY".len())
            .any(|s| s == b"OWNED-HOLDOUT-READY")
    {
        let mut bytes = [0; 128];
        let n = aterm_pty::read(shell.master, &mut bytes);
        if n > 0 {
            seen.extend_from_slice(&bytes[..n as usize]);
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    assert!(
        seen.windows(b"OWNED-HOLDOUT-READY".len())
            .any(|s| s == b"OWNED-HOLDOUT-READY"),
        "the owned child never proved readiness"
    );
    assert!(holdout.identity.verify());
    let master = holdout.master.take().unwrap();
    let raw = master.as_raw_fd();
    let sink = Arc::new(SinkWriter::new_owned(master));
    let mut session = crate::stub_session_with_sink(id, sink);
    session.master = raw;
    session.pid = shell.pid;
    session.shell_identity = holdout.identity;
    (session, holdout)
}

#[test]
fn quitting_reaps_a_hup_ignoring_tab_still_in_the_completion_queue() {
    let mut app = App::headless_for_test();
    let (tx, rx) = channel();
    let id = book(
        &mut app,
        WindowId(0),
        TabSpawnThen::Reply {
            reply: tx.into(),
            raise: false,
        },
    );
    let (session, holdout) = hup_ignoring_session(&app, id);
    let completion = Arc::clone(&app.tab_spawns.in_flight.last().unwrap().completion);
    let mut queued = None;
    publish_completion(
        &completion,
        TabSpawned {
            id,
            window: WindowId(0),
            session: Ok(session),
            reader_gate: None,
        },
        |wake| {
            queued = Some(wake);
            true
        },
    );
    let queued = queued.expect("the worker queued its successful completion");
    app.pool.retire_at_quit(&[]);
    assert!(
        holdout.identity.verify(),
        "the test's queued child must be alive outside the pool"
    );
    app.shutdown_tab_spawns();
    assert!(
        aterm_pty::wait_for_reaps(Duration::from_secs(60)),
        "owned exit reaps did not settle"
    );
    assert!(
        holdout.wait_until_absent(),
        "Quit abandoned the uninstalled HUP-ignoring child"
    );
    assert!(
        matches!(rx.recv_timeout(Duration::from_secs(60)), Ok(Err(_))),
        "the pending wire spawn was not refused at Quit"
    );
    assert!(
        matches!(queued, Wake::TabSpawned(_)),
        "the completion remains queued, not installed"
    );
}

#[test]
fn a_spawn_finishing_after_quit_drops_its_owned_child_before_join() {
    let mut app = App::headless_for_test();
    let id = book(&mut app, WindowId(0), TabSpawnThen::Nothing);
    let completion = Arc::clone(&app.tab_spawns.in_flight.last().unwrap().completion);
    let (session, holdout) = hup_ignoring_session(&app, id);
    let (finish, ready) = channel();
    let (posted, wakes) = channel();
    let worker = std::thread::spawn(move || {
        ready.recv_timeout(Duration::from_secs(60)).unwrap();
        publish_completion(
            &completion,
            TabSpawned {
                id,
                window: WindowId(0),
                session: Ok(session),
                reader_gate: None,
            },
            |wake| posted.send(wake).is_ok(),
        );
    });
    app.shutdown_tab_spawns();
    finish.send(()).unwrap();
    worker.join().unwrap();
    assert!(
        wakes.try_recv().is_err(),
        "a cancelled spawn posted a completion after Quit"
    );
    assert!(aterm_pty::wait_for_reaps(Duration::from_secs(60)));
    assert!(
        holdout.wait_until_absent(),
        "the joined worker left its HUP-ignoring child alive"
    );
}

/// A candidate's spawn that finishes before its Commit waits for that Commit and lands: its
/// completion alone clears the booking, and a booking refuses every later update. A held
/// wake past the bound is no reason to lose it.
#[cfg(unix)]
#[test]
fn a_spawn_finishing_before_the_commit_lands_after_it() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let (shell, _shell_pty) = piped_session(app.next_session_id);
    app.push_stub_tab(wid, shell);
    app.incoming_handoff_pending = true;
    app.handoff_committed_wakes = (0..crate::MAX_COMMITTED_DEFERRED_WAKES)
        .map(|_| Wake::Snapshot)
        .collect();

    let (session, _pty) = piped_session(app.next_session_id);
    let id = book(&mut app, wid, TabSpawnThen::Nothing);
    let completion = Arc::clone(&app.tab_spawns.in_flight.last().unwrap().completion);
    let mut queued = None;
    publish_completion(
        &completion,
        TabSpawned {
            id,
            window: wid,
            session: Ok(session),
            reader_gate: None,
        },
        |wake| {
            queued = Some(wake);
            true
        },
    );
    assert!(
        app.admit_wake_before_commit(queued.expect("the worker queued its completion"))
            .is_none(),
        "not landed ahead of the Commit"
    );
    assert!(!app.tab_spawns.is_empty(), "still booked");
    let Some(Wake::TabSpawned(held)) = app.handoff_committed_wakes.pop() else {
        panic!("the completion was dropped, so its booking would refuse updates for good");
    };

    app.incoming_handoff_pending = false;
    assert!(app.install_spawned_tab(held.take().expect("the session it finished with")));
    assert!(
        app.tab_spawns.is_empty(),
        "nothing is left to refuse an update"
    );
}

/// A menu press swallowed before the Commit holds no later key or click for its mark.
#[test]
fn a_menu_press_dropped_before_the_commit_holds_no_input() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let winit_id = WinitWindowId::from(0u64);
    app.winit_to_window.insert(winit_id, wid);
    app.incoming_handoff_pending = true;
    let mark = app.tab_spawns.menu.mint(Instant::now());
    assert!(app.drops_wake_before_commit(&Wake::MenuAction {
        action: crate::menu::MenuAction::Copy,
        mark,
        origin: None,
        gesture: GestureMarks::default(),
    }));
    app.incoming_handoff_pending = false;
    assert!(
        app.defer_tab_spawn_input(wid, winit_id, typed("x"))
            .is_some(),
        "the swallowed press must not hold the key typed after the Commit"
    );
}

fn said_dropped(app: &App, what: &str) -> bool {
    app.messages.live_rows().any(|l| {
        l.msg.title == "Keystrokes dropped" && l.msg.detail.iter().any(|d| d.contains(what))
    })
}

fn typed(text: &str) -> WindowEvent {
    WindowEvent::Ime(Ime::Commit(text.to_string()))
}

fn replay(app: &mut App, wid: WindowId, events: Vec<Released>) {
    for (_, event, origin) in events {
        if let WindowEvent::Ime(ime) = event {
            app.dispatch_ime_event(wid, ime, origin);
        }
    }
}

/// A tab nobody asked for ([`SpawnAsk::unasked`]) holds none of its window's input: keys
/// typed while it spawns go where they were typed, never into the new tab's shell.
#[test]
fn an_unasked_tab_holds_none_of_its_windows_input_while_it_spawns() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let winit_id = WinitWindowId::from(0u64);
    let (old, _old_pty) = piped_session(app.next_session_id);
    app.push_stub_tab(wid, old);
    book(&mut app, wid, TabSpawnThen::Nothing);
    app.tab_spawns.in_flight.last_mut().unwrap().unasked = true;
    assert!(
        app.defer_tab_spawn_input(wid, winit_id, typed("x"))
            .is_some(),
        "an unasked tab's window keeps its keys"
    );
    app.tab_spawns.in_flight.last_mut().unwrap().unasked = false;
    assert!(
        app.defer_tab_spawn_input(wid, winit_id, typed("y"))
            .is_none(),
        "a person's own new tab holds them to replay"
    );
}

#[test]
fn a_remote_plain_shell_does_not_hold_the_persons_in_flight_typing() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let winit_id = WinitWindowId::from(0u64);
    let (old, _old_pty) = piped_session(app.next_session_id);
    app.push_stub_tab(wid, old);
    let ask = SpawnAsk::default().for_fleet_shell();
    super::test_support::book_as(&mut app, wid, &ask, TabSpawnThen::Nothing);
    assert!(
        app.defer_tab_spawn_input(wid, winit_id, typed("x"))
            .is_some()
    );
    assert!(
        app.tab_spawns.held.is_empty(),
        "the remote spawn retains no physical key"
    );
}

#[test]
fn a_tab_spawned_off_the_loop_lands_registered_and_gets_what_was_typed_meanwhile() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let winit_id = WinitWindowId::from(0u64);
    let (old, old_pty) = piped_session(app.next_session_id);
    app.push_stub_tab(wid, old);
    let tabs = app.windows[&wid].tab_set.len();
    let (tx, rx) = channel();
    let id = book(
        &mut app,
        wid,
        TabSpawnThen::Reply {
            reply: tx.into(),
            raise: false,
        },
    );
    for text in ["l", "s", "\r"] {
        assert!(
            app.defer_tab_spawn_input(wid, winit_id, typed(text))
                .is_none()
        );
    }
    assert!(
        app.defer_tab_spawn_input(wid, winit_id, WindowEvent::RedrawRequested)
            .is_some(),
        "only keys, modifiers and IME wait"
    );
    assert!(
        rx.try_recv().is_err(),
        "nothing is answered before the tab lands"
    );

    let (new, new_pty) = piped_session(id);
    let sid = new.ctx.self_id.clone();
    let completion = Arc::clone(&app.tab_spawns.in_flight.last().unwrap().completion);
    let mut queued = None;
    publish_completion(
        &completion,
        TabSpawned {
            id,
            window: wid,
            session: Ok(new),
            reader_gate: None,
        },
        |wake| {
            queued = Some(wake);
            true
        },
    );
    let Wake::TabSpawned(completion) = queued.unwrap() else {
        panic!("the worker posted something other than its tab completion");
    };
    assert!(app.install_spawned_tab(completion.take().unwrap()));
    assert_eq!(rx.try_recv(), Ok(Ok(sid.as_str().to_string())));
    {
        let store = app.store.read().unwrap_or_else(|p| p.into_inner());
        assert_eq!(
            store.by_sid(&sid).map(|h| h.local_id),
            Some(id),
            "registered"
        );
    }
    assert_eq!(
        app.focused_session_id(wid),
        Some(id),
        "the new tab is the active one"
    );
    assert_eq!(
        app.windows[&wid].tab_set.len(),
        tabs + 1,
        "one tab appended"
    );

    let (events, repair) = app.settle_tab_spawn_input(wid, true);
    assert_eq!(repair, None);
    replay(&mut app, wid, events);
    assert_eq!(drain(&new_pty), b"ls\r", "in order, to the new tab");
    assert!(
        drain(&old_pty).is_empty(),
        "never to the tab the person left"
    );
    assert!(app.tab_spawns.held.is_empty() && app.tab_spawns.in_flight.is_empty());
}

#[test]
fn a_failed_spawn_lands_nothing_and_drops_what_was_typed_for_it() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let winit_id = WinitWindowId::from(0u64);
    let (old, old_pty) = piped_session(app.next_session_id);
    app.push_stub_tab(wid, old);
    let tabs = app.windows[&wid].tab_set.len();
    let (tx, rx) = channel();
    let id = book(
        &mut app,
        wid,
        TabSpawnThen::Reply {
            reply: tx.into(),
            raise: false,
        },
    );
    let mods = WindowEvent::ModifiersChanged(aterm_window::event::Modifiers::default());
    assert!(
        app.defer_tab_spawn_input(wid, winit_id, typed("rm"))
            .is_none()
    );
    assert!(app.defer_tab_spawn_input(wid, winit_id, mods).is_none());

    let refused = std::io::Error::other("exec failed");
    assert!(!app.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Err(refused),
        reader_gate: None
    }));
    assert_eq!(rx.try_recv(), Ok(Err("exec failed".to_string())));
    assert_eq!(app.windows[&wid].tab_set.len(), tabs, "no tab");
    assert!(
        app.messages
            .live_rows()
            .any(|l| l.msg.title == "Couldn't open a new tab"),
        "the failure is said"
    );

    let (events, _) = app.settle_tab_spawn_input(wid, false);
    assert!(
        events
            .iter()
            .all(|(_, e, _)| matches!(e, WindowEvent::ModifiersChanged(_))),
        "only the modifier changes replay: {events:?}"
    );
    assert_eq!(events.len(), 1);
    replay(&mut app, wid, events);
    assert!(
        drain(&old_pty).is_empty(),
        "the keys never reach the tab left behind"
    );
    assert!(
        said_dropped(&app, "retype the last key"),
        "the dropped key is said"
    );
}

#[test]
fn a_tab_whose_window_closed_is_dropped_and_its_waiter_told() {
    let mut app = App::headless_for_test();
    let (tx, rx) = channel();
    let gone = WindowId(77);
    let id = app.next_session_id;
    app.next_session_id += 1;
    app.tab_spawns.in_flight.push(InFlight {
        id,
        alt_id: None,
        window: gone,
        place: SpawnPlace::Tab,
        asked: Instant::now(),
        parent: None,
        colors: app.engine_color_fanouts,
        grid: app.spawn_grid(gone),
        then: TabSpawnThen::Reply {
            reply: tx.into(),
            raise: false,
        },
        admission: SpawnAdmission::Plain,
        background: false,
        parks: false,
        fleet: false,
        refuses_worker_split: false,
        unasked: false,
        completion: Arc::new(TabSpawnCompletion::default()),
        theme: None,
        acting: None,
    });
    let (session, _pty) = piped_session(id);
    let sid = session.ctx.self_id.clone();
    assert!(!app.install_spawned_tab(TabSpawned {
        id,
        window: gone,
        session: Ok(session),
        reader_gate: None
    }));
    assert!(matches!(rx.try_recv(), Ok(Err(why)) if why.contains("window closed")));
    let store = app.store.read().unwrap_or_else(|p| p.into_inner());
    assert!(store.by_sid(&sid).is_none(), "never registered");
}

#[test]
fn input_waits_only_for_a_tab_in_flight_to_its_window_and_within_the_hold() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let winit_id = WinitWindowId::from(0u64);
    assert!(
        app.defer_tab_spawn_input(wid, winit_id, typed("a"))
            .is_some(),
        "no tab in flight"
    );
    let id = book(&mut app, wid, TabSpawnThen::Nothing);
    assert!(
        app.defer_tab_spawn_input(WindowId(9), winit_id, typed("b"))
            .is_some(),
        "another window's input is its own"
    );
    let stale = Instant::now().checked_sub(SPAWN_HOLD + Duration::from_secs(1));
    if let Some(stale) = stale {
        app.tab_spawns.in_flight[0].asked = stale;
        assert!(
            app.defer_tab_spawn_input(wid, winit_id, typed("c"))
                .is_some(),
            "past the hold, input goes to what the window shows"
        );
    }
    assert_eq!(app.tab_spawns.in_flight[0].id, id);
}

#[test]
fn held_input_is_bounded_and_a_drop_or_focus_loss_repairs_the_modifiers() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let winit_id = WinitWindowId::from(0u64);
    let id = book(&mut app, wid, TabSpawnThen::Nothing);
    for n in 0..MAX_HELD_INPUT + 3 {
        assert!(
            app.defer_tab_spawn_input(wid, winit_id, typed(&format!("{n}")))
                .is_none()
        );
    }
    assert_eq!(app.tab_spawns.held.len(), MAX_HELD_INPUT);
    app.tab_spawns.in_flight.retain(|t| t.id != id);
    let (events, repair) = app.settle_tab_spawn_input(wid, true);
    assert_eq!(events.len(), MAX_HELD_INPUT, "the kept prefix replays");
    assert!(matches!(&events[0].1, WindowEvent::Ime(Ime::Commit(t)) if t == "0"));
    assert_eq!(repair, Some(wid), "a drop repairs the modifiers");
    assert!(said_dropped(&app, "retype the last 3 keys"));

    let id = book(&mut app, wid, TabSpawnThen::Nothing);
    assert!(
        app.defer_tab_spawn_input(wid, winit_id, typed("x"))
            .is_none()
    );
    app.tab_spawns.note_focus_loss(wid);
    app.tab_spawns.in_flight.retain(|t| t.id != id);
    let (_, repair) = app.settle_tab_spawn_input(wid, true);
    assert_eq!(
        repair,
        Some(wid),
        "a focus loss with input held repairs the modifiers"
    );
}

#[test]
fn a_tab_lands_with_the_scheme_installed_while_it_was_in_flight() {
    use aterm_types::Appearance;
    let land = |bump: bool| {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let id = book(&mut app, wid, TabSpawnThen::Nothing);
        let (session, _pty) = piped_session(id);
        crate::term_lock(&session.term).set_color_scheme(Appearance::Dark);
        let tc = app.session_factory.terminal_config.get_or_insert_default();
        tc.color_scheme = Some(Appearance::Light);
        if bump {
            app.engine_color_fanouts += 1;
        }
        let term = Arc::clone(&session.term);
        assert!(app.install_spawned_tab(TabSpawned {
            id,
            window: wid,
            session: Ok(session),
            reader_gate: None
        }));
        crate::term_lock(&term).color_scheme()
    };
    assert_eq!(land(true), Appearance::Light);
    assert_eq!(land(false), Appearance::Dark);
}

#[test]
fn a_tab_lands_at_the_grid_its_window_took_while_it_was_in_flight() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let (rows, cols) = (app.windows[&wid].rows, app.windows[&wid].cols);
    let id = book(&mut app, wid, TabSpawnThen::Nothing);
    let (session, _pty) = piped_session(id);
    let term = Arc::clone(&session.term);
    {
        let mut t = crate::term_lock(&term);
        t.resize(rows, cols);
    }
    let (later_rows, later_cols) = (rows + 7, cols + 11);
    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.rows = later_rows;
        ws.cols = later_cols;
    }
    assert!(app.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Ok(session),
        reader_gate: None
    }));
    let t = crate::term_lock(&term);
    assert_eq!(
        (t.rows(), t.cols()),
        (later_rows, later_cols),
        "the window's grid, not the one it was asked at"
    );
}

#[test]
fn held_input_waits_for_the_last_tab_in_flight_to_its_window() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let winit_id = WinitWindowId::from(0u64);
    let first = book(&mut app, wid, TabSpawnThen::Nothing);
    let second = book(&mut app, wid, TabSpawnThen::Nothing);
    assert!(
        app.defer_tab_spawn_input(wid, winit_id, typed("k"))
            .is_none()
    );
    let (one, _pty1) = piped_session(first);
    assert!(app.install_spawned_tab(TabSpawned {
        id: first,
        window: wid,
        session: Ok(one),
        reader_gate: None
    }));
    let (events, _) = app.settle_tab_spawn_input(wid, true);
    assert!(events.is_empty(), "the second is still in flight");
    let (two, pty2) = piped_session(second);
    assert!(app.install_spawned_tab(TabSpawned {
        id: second,
        window: wid,
        session: Ok(two),
        reader_gate: None
    }));
    let (events, _) = app.settle_tab_spawn_input(wid, true);
    replay(&mut app, wid, events);
    assert_eq!(app.focused_session_id(wid), Some(second));
    assert_eq!(drain(&pty2), b"k");
}

/// Lands a booked `id` with a stub program.
fn land(app: &mut App, wid: WindowId, id: u64) -> std::fs::File {
    let (session, pty) = piped_session(id);
    assert!(app.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Ok(session),
        reader_gate: None
    }));
    pty
}

#[test]
fn a_worker_ask_lands_parked_and_behind_whichever_tab_shows() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let front = book(&mut app, wid, TabSpawnThen::Nothing);
    let _front_pty = land(&mut app, wid, front);
    let worker = SpawnAsk {
        task: Some(crate::task_launch::TaskSeed::new(b"work".to_vec())),
        ..SpawnAsk::default()
    };
    let id = super::test_support::book_as(&mut app, wid, &worker, TabSpawnThen::Nothing);
    let _worker_pty = land(&mut app, wid, id);
    assert!(app.session_is_parked(id), "a task on a tab landing parks");
    assert_eq!(app.focused_session_id(wid), Some(front), "the front stays");

    // The same landing with no task is a person's tab.
    let id = book(&mut app, wid, TabSpawnThen::Nothing);
    let _shown_pty = land(&mut app, wid, id);
    assert!(!app.session_is_parked(id));
    assert_eq!(app.focused_session_id(wid), Some(id));
    assert!(app.windows[&wid].tab_set.invariant_holds(&app.view_store));
}

/// A fleet drive's ask, a plain shell with no task among them, lands parked behind the front
/// and parented to nothing, never to the tab the person had focused (the requests design
/// (2026-11-09) §6, decision 5). NEGATIVE CONTROL: the same ask from this Mac is a person's
/// tab, parented to the focused one.
#[test]
fn a_fleet_drives_spawn_lands_parked_and_unparented() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let front = book(&mut app, wid, TabSpawnThen::Nothing);
    let _front_pty = land(&mut app, wid, front);
    let front_sid = app.pool.get(front).map(|s| s.ctx.self_id.clone());
    assert!(front_sid.is_some());
    let parent_of = |app: &App, id: u64| {
        app.store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_local(id)
            .map(|h| h.parent.clone())
    };
    for (task, what) in [
        (None, "a plain shell"),
        (
            Some(crate::task_launch::TaskSeed::new(b"work".to_vec())),
            "a worker",
        ),
    ] {
        let driven = SpawnAsk {
            task,
            fleet: true,
            ..SpawnAsk::default()
        }
        .for_fleet_shell();
        assert!(driven.parks(), "{what}");
        let id = super::test_support::book_as(&mut app, wid, &driven, TabSpawnThen::Nothing);
        let _pty = land(&mut app, wid, id);
        assert!(app.session_is_parked(id), "{what} parks");
        assert_eq!(
            app.focused_session_id(wid),
            Some(front),
            "{what}: the front stays"
        );
        assert_eq!(parent_of(&app, id), Some(None), "{what}: no parent");
        // Its restore record says whose it is, so a restart or a seamless update brings it
        // back where the drive put it, a plain shell's tab holding no task record included.
        let tab = app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .find(|tab| app.tab_holds_session(tab, id))
            .expect("its tab");
        assert!(tab.fleet, "{what}: a drive's");
        let record = app.tab_restore_descriptor(tab).expect("captured");
        assert!(record.parked && record.fleet, "{what}: {record:?}");
    }
    let plain = app.windows[&wid]
        .tab_set
        .parked()
        .first()
        .expect("the plain shell");
    assert!(
        !app.tab_has_task(plain),
        "PRECONDITION: a plain shell holds no task, so only its fleet bit keeps it parked \
         through a restore (app_restore_tests' a_restored_workers_tab_keeps_its_bits_only_with_a_record)"
    );
    let local = SpawnAsk::default();
    assert!(!local.parks(), "CONTROL");
    let id = super::test_support::book_as(&mut app, wid, &local, TabSpawnThen::Nothing);
    let _pty = land(&mut app, wid, id);
    assert!(!app.session_is_parked(id), "CONTROL: a person's tab");
    assert!(
        app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .all(|tab| !tab.fleet || tab.parked),
        "CONTROL: no person's tab is a drive's"
    );
    assert_eq!(
        parent_of(&app, id),
        Some(front_sid),
        "CONTROL: the focused tab's child"
    );
    assert!(app.windows[&wid].tab_set.invariant_holds(&app.view_store));
}

#[test]
fn a_summary_lands_a_persons_tab_and_takes_the_front() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let front = book(&mut app, wid, TabSpawnThen::Nothing);
    let _front_pty = land(&mut app, wid, front);
    let summary = SpawnAsk {
        admission: SpawnAdmission::Summary,
        task: Some(crate::task_launch::TaskSeed::new(b"summarize".to_vec())),
        ..SpawnAsk::default()
    };
    assert!(!summary.parks(), "the person asked for a new tab");
    let id = super::test_support::book_as(&mut app, wid, &summary, TabSpawnThen::Nothing);
    let _pty = land(&mut app, wid, id);
    assert!(!app.session_is_parked(id));
    assert_eq!(app.focused_session_id(wid), Some(id), "it takes the front");
    assert_eq!(app.windows[&wid].tab_set.parked().len(), 0);
    assert_eq!(app.windows[&wid].tab_set.home(), None, "no list opened");
}

#[test]
fn only_a_worker_nobody_asked_to_see_parks_behind_the_front() {
    use crate::control::control_media::{SpawnBooking, placed_ask};
    let task = || Some(crate::task_launch::TaskSeed::new(b"work".to_vec()));
    let booking = |admission, split, raise| SpawnBooking {
        cwd: None,
        split,
        raise,
        admission,
        fleet: false,
    };
    let plain = placed_ask(
        SpawnAsk {
            task: task(),
            ..SpawnAsk::default()
        },
        booking(SpawnAdmission::Plain, None, true),
    );
    assert!(plain.parks() && plain.background && plain.unasked);

    let summary = placed_ask(
        SpawnAsk {
            task: task(),
            ..SpawnAsk::default()
        },
        crate::summarize::summary_booking(std::path::Path::new("/tmp/summary")),
    );
    assert!(!summary.parks());
    assert!(!summary.background, "it takes the front");
    assert!(!summary.unasked, "what the person types next goes to it");

    let split = placed_ask(
        SpawnAsk {
            task: task(),
            ..SpawnAsk::default()
        },
        booking(
            SpawnAdmission::Plain,
            Some(crate::pane::SplitDir::Vertical),
            true,
        ),
    );
    assert!(!split.background && !split.unasked && split.refuses_worker_split);

    let shell = placed_ask(
        SpawnAsk::default(),
        booking(SpawnAdmission::Plain, None, true),
    );
    assert!(!shell.parks() && !shell.background && !shell.refuses_worker_split);
}

#[test]
fn a_wire_split_landing_where_a_worker_came_to_the_front_is_refused() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let front = book(&mut app, wid, TabSpawnThen::Nothing);
    let _front_pty = land(&mut app, wid, front);
    let worker = SpawnAsk {
        task: Some(crate::task_launch::TaskSeed::new(b"work".to_vec())),
        ..SpawnAsk::default()
    };
    let parked = super::test_support::book_as(&mut app, wid, &worker, TabSpawnThen::Nothing);
    let _worker_pty = land(&mut app, wid, parked);
    let (reply, answer) = std::sync::mpsc::channel();
    let id = book_split(
        &mut app,
        wid,
        crate::pane::SplitDir::Vertical,
        TabSpawnThen::Run(Box::new(move |_, landed| {
            let _ = reply.send(landed);
        })),
    );
    app.tab_spawns
        .in_flight
        .last_mut()
        .expect("booked")
        .refuses_worker_split = true;
    assert!(app.focus_session_preferring(Some(wid), parked));
    let (session, _pty) = piped_session(id);
    assert!(!app.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Ok(session),
        reader_gate: None
    }));
    assert_eq!(
        answer.recv().expect("answered"),
        Err(WORKER_SPLIT_REFUSED.to_string())
    );
    let ws = &app.windows[&wid];
    assert_eq!(
        ws.tab_set.parked()[0].root.leaves().len(),
        1,
        "no pane landed"
    );

    // CONTROL: a person's own split of the worker in front of them lands.
    let id = book_split(
        &mut app,
        wid,
        crate::pane::SplitDir::Vertical,
        TabSpawnThen::Nothing,
    );
    let _split_pty = land(&mut app, wid, id);
    assert_eq!(app.windows[&wid].tab_set.parked()[0].root.leaves().len(), 2);
}

#[test]
fn a_worker_ask_on_a_split_stays_a_pane_of_the_tab_it_split() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let id = book_split(
        &mut app,
        wid,
        crate::pane::SplitDir::Vertical,
        TabSpawnThen::Nothing,
    );
    app.tab_spawns.in_flight.last_mut().expect("booked").parks = true;
    let _pty = land(&mut app, wid, id);
    assert!(!app.session_is_parked(id));
    assert_eq!(app.windows[&wid].tab_set.parked().len(), 0);
    assert_eq!(app.focused_session_id(wid), Some(id));
}

#[test]
fn a_split_spawned_off_the_loop_lands_as_a_focused_pane_sized_to_its_half() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let (rows, cols) = (app.windows[&wid].rows, app.windows[&wid].cols);
    let (tx, rx) = channel();
    let id = book_split(
        &mut app,
        wid,
        crate::pane::SplitDir::Vertical,
        TabSpawnThen::Reply {
            reply: tx.into(),
            raise: false,
        },
    );
    assert!(
        rx.try_recv().is_err(),
        "nothing is answered before it lands"
    );
    let (session, _pty) = piped_session(id);
    let sid = session.ctx.self_id.clone();
    let term = Arc::clone(&session.term);
    crate::term_lock(&term).resize(rows, cols);
    assert!(app.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Ok(session),
        reader_gate: None
    }));
    assert_eq!(rx.try_recv(), Ok(Ok(sid.as_str().to_string())));
    assert_eq!(
        app.active_tree(wid).map(crate::pane::PaneTree::len),
        Some(2)
    );
    assert_eq!(
        app.focused_session_id(wid),
        Some(id),
        "the new pane is focused"
    );
    {
        let store = app.store.read().unwrap_or_else(|p| p.into_inner());
        assert_eq!(
            store.by_sid(&sid).map(|h| h.local_id),
            Some(id),
            "registered"
        );
    }
    let t = crate::term_lock(&term);
    assert!(
        t.cols() <= cols / 2 && t.rows() <= rows,
        "the newborn is sized to its half, not the window: {}x{} of {cols}x{rows}",
        t.cols(),
        t.rows()
    );
    drop(t);
    assert!(app.structural_invariants_ok());
}

#[test]
fn split_inherits_tab_does_not() {
    use crate::session_theme::{SessionTheme, SessionThemeOrigin};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let hearth = aterm_types::scheme::builtin("Hearth").expect("built-in");
    app.set_session_theme(
        0,
        Some(SessionTheme {
            selector: "Hearth".to_string(),
            origin: SessionThemeOrigin::Ctl,
        }),
    );
    let land = |app: &mut App, id: u64| {
        let (session, _pty) = piped_session(id);
        assert!(app.install_spawned_tab(TabSpawned {
            id,
            window: wid,
            session: Ok(session),
            reader_gate: None
        }));
    };
    let split = book_split(
        &mut app,
        wid,
        crate::pane::SplitDir::Vertical,
        TabSpawnThen::Nothing,
    );
    land(&mut app, split);
    assert_eq!(
        app.pool.theme(split),
        Some(&SessionTheme {
            selector: "Hearth".to_string(),
            origin: SessionThemeOrigin::Split,
        })
    );
    assert_eq!(
        crate::term_lock(&app.pool.get(split).expect("landed").term).default_background(),
        hearth.background
    );
    let asked = book_split(
        &mut app,
        wid,
        crate::pane::SplitDir::Horizontal,
        TabSpawnThen::Nothing,
    );
    app.tab_spawns.in_flight.last_mut().expect("booked").theme = Some(SessionTheme {
        selector: "Linen".to_string(),
        origin: SessionThemeOrigin::Spawn,
    });
    land(&mut app, asked);
    assert_eq!(
        app.pool
            .theme(asked)
            .map(|t| (t.selector.as_str(), t.origin)),
        Some(("Linen", SessionThemeOrigin::Spawn))
    );
    let tab = book(&mut app, wid, TabSpawnThen::Nothing);
    land(&mut app, tab);
    assert_eq!(app.pool.theme(tab), None, "a new tab takes none");
}

#[test]
fn a_split_inherits_what_the_pane_had_when_it_was_booked() {
    use crate::session_theme::{SessionTheme, SessionThemeOrigin};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let split = book_split(
        &mut app,
        wid,
        crate::pane::SplitDir::Vertical,
        TabSpawnThen::Nothing,
    );
    app.set_session_theme(
        0,
        Some(SessionTheme {
            selector: "Hearth".to_string(),
            origin: SessionThemeOrigin::Ctl,
        }),
    );
    let (session, _pty) = piped_session(split);
    assert!(app.install_spawned_tab(TabSpawned {
        id: split,
        window: wid,
        session: Ok(session),
        reader_gate: None
    }));
    assert_eq!(
        app.pool.theme(split),
        None,
        "booked unthemed, lands unthemed"
    );
}

#[test]
fn a_split_the_window_can_no_longer_take_is_refused_as_it_lands() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let (tx, rx) = channel();
    let id = book_split(
        &mut app,
        wid,
        crate::pane::SplitDir::Vertical,
        TabSpawnThen::Reply {
            reply: tx.into(),
            raise: false,
        },
    );
    assert!(app.apply_term_resize(wid, 24, 32), "geometry applies");
    let (session, _pty) = piped_session(id);
    let sid = session.ctx.self_id.clone();
    assert!(!app.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Ok(session),
        reader_gate: None
    }));
    assert!(
        matches!(rx.try_recv(), Ok(Err(why)) if why.starts_with("Split refused: this pane is 32x24 cells")),
        "the waiter hears the refusal"
    );
    assert_eq!(
        app.active_tree(wid).map(crate::pane::PaneTree::len),
        Some(1)
    );
    let store = app.store.read().unwrap_or_else(|p| p.into_inner());
    assert!(store.by_sid(&sid).is_none(), "never registered");
    drop(store);
    assert!(
        app.messages
            .live_rows()
            .any(|l| l.msg.title == "Couldn't split the pane"),
        "the refusal is said"
    );
    assert!(app.tab_spawns.in_flight.is_empty());
    assert!(app.structural_invariants_ok());
}

#[test]
fn a_tab_new_is_counted_once_its_tab_is_in_the_window() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let tabs = app.windows[&wid].tab_set.len();
    let (tx, rx) = channel();
    let id = book(&mut app, wid, TabSpawnThen::Counts(tx));
    assert!(rx.try_recv().is_err(), "nothing is counted before it lands");
    let (session, _pty) = piped_session(id);
    assert!(app.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Ok(session),
        reader_gate: None
    }));
    assert_eq!(
        rx.try_recv(),
        Ok(Ok((tabs, tabs + 1))),
        "the new last tab is active"
    );

    let (tx, rx) = channel();
    let id = book(&mut app, wid, TabSpawnThen::Counts(tx));
    assert!(!app.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Err(std::io::Error::other("exec refused")),
        reader_gate: None
    }));
    assert_eq!(rx.try_recv(), Ok(Err("exec refused".to_string())));
    assert_eq!(app.windows[&wid].tab_set.len(), tabs + 1, "no tab added");
}

#[test]
fn a_row_acting_on_the_newborn_runs_with_its_registered_sid() {
    use std::cell::RefCell;
    use std::rc::Rc;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let seen: Rc<RefCell<Vec<Result<bool, String>>>> = Rc::default();
    let run = |seen: &Rc<RefCell<Vec<Result<bool, String>>>>| {
        let seen = Rc::clone(seen);
        TabSpawnThen::Run(Box::new(move |app: &mut App, landed| {
            let registered = landed.map(|sid| {
                let store = app.store.read().unwrap_or_else(|p| p.into_inner());
                store.by_sid(&aterm_session::SessionId::new(sid)).is_some()
            });
            seen.borrow_mut().push(registered);
        }))
    };
    let id = book(&mut app, wid, run(&seen));
    assert!(
        seen.borrow().is_empty(),
        "nothing runs before the tab lands"
    );
    let (session, _pty) = piped_session(id);
    assert!(app.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Ok(session),
        reader_gate: None
    }));
    let id = book(&mut app, wid, run(&seen));
    assert!(!app.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Err(std::io::Error::other("exec refused")),
        reader_gate: None
    }));
    assert_eq!(
        *seen.borrow(),
        [Ok(true), Err("exec refused".to_string())],
        "once each: the registered sid, then the reason"
    );
}

#[test]
fn a_booked_spawn_holds_the_status_sweep_until_it_lands_or_the_hold_ends() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let now = Instant::now();
    assert_eq!(app.tab_spawns.status_sweep_held_until(now), None);
    let id = book(&mut app, wid, TabSpawnThen::Nothing);
    let asked = app.tab_spawns.in_flight[0].asked;
    assert_eq!(
        app.tab_spawns.status_sweep_held_until(asked),
        Some(asked + SWEEP_HOLD)
    );
    assert_eq!(
        app.tab_spawns.status_sweep_held_until(asked + SWEEP_HOLD),
        None,
        "the hold ends"
    );
    app.tab_spawns.in_flight[0].asked = asked - SWEEP_HOLD;
    let newer = book(&mut app, wid, TabSpawnThen::Nothing);
    let newest = app.tab_spawns.in_flight[1].asked;
    assert_eq!(
        app.tab_spawns.status_sweep_held_until(newest),
        Some(newest + SWEEP_HOLD),
        "the newest booking's hold"
    );
    for landed in [id, newer] {
        let (session, _pty) = piped_session(landed);
        assert!(app.install_spawned_tab(TabSpawned {
            id: landed,
            window: wid,
            session: Ok(session),
            reader_gate: None
        }));
    }
    assert_eq!(
        app.tab_spawns.status_sweep_held_until(newest),
        None,
        "landed, nothing holds it"
    );
}

#[test]
fn a_held_sweep_arms_no_status_wake_before_the_hold_ends() {
    let mut app = App::headless_for_test();
    book(&mut app, WindowId(0), TabSpawnThen::Nothing);
    let asked = app.tab_spawns.in_flight[0].asked;
    let now = asked + Duration::from_millis(1);
    let held = app.tab_spawns.status_sweep_held_until(now);
    assert_eq!(held, Some(asked + SWEEP_HOLD));
    let past = asked;
    let later = asked + SWEEP_HOLD + Duration::from_millis(5);
    assert_eq!(
        held_status_wake(Some(past), held),
        held,
        "past: the hold's end"
    );
    assert_eq!(held_status_wake(Some(later), held), Some(later));
    assert_eq!(held_status_wake(None, held), None);
    assert_eq!(held_status_wake(Some(past), None), Some(past));
}

#[test]
fn a_spawn_whose_state_folder_did_not_answer_is_refused_by_name() {
    let state = std::path::Path::new("/aterm-test/net-home/Library/Application Support/aterm");
    let refused =
        state_answers_with(Some(state), |_| false, |_| false).expect_err("a silent network folder");
    assert_eq!(refused.kind(), std::io::ErrorKind::TimedOut);
    assert_eq!(
        refused.to_string(),
        format!("{} did not answer", state.display())
    );
    assert!(state_answers_with(Some(state), |_| false, |_| true).is_ok());
    assert!(
        state_answers_with(
            Some(state),
            |_| true,
            |_| panic!("a local folder is not looked at")
        )
        .is_ok()
    );
    assert!(state_answers_with(None, |_| false, |_| false).is_ok());
}

/// [`piped_session`] with a valid master fd so the session registers in the pool.
pub(crate) fn registered_piped_session(id: u64) -> (Session, std::fs::File) {
    let (mut session, read) = piped_session(id);
    session.master = session.ctx.sink.master();
    (session, read)
}

fn said_no_window(app: &App, what: &str) -> bool {
    app.messages.live_rows().any(|l| {
        l.msg.title == "Couldn't open a new window" && l.msg.detail.iter().any(|d| d.contains(what))
    })
}

#[test]
fn a_window_spawned_off_the_loop_lands_frontmost_and_gets_what_was_typed() {
    let mut app = App::headless_for_test();
    let from = WindowId(0);
    let from_winit = WinitWindowId::from(0u64);
    app.winit_to_window.insert(from_winit, from);
    let (old, old_pty) = piped_session(app.next_session_id);
    app.push_stub_tab(from, old);
    let windows = app.windows.len();
    let asked_grid = (app.windows[&from].rows, app.windows[&from].cols);
    let reserved = WindowId(app.next_window_id);
    let (tx, rx) = channel();
    let (id, wid) = test_support::book_window(
        &mut app,
        None,
        SpawnAdmission::Plain,
        TabSpawnThen::Reply {
            reply: tx.into(),
            raise: true,
        },
    );
    assert_eq!(
        wid, reserved,
        "the window's id is minted when it is asked for"
    );
    assert!(
        !app.windows.contains_key(&wid),
        "no window before its session"
    );
    assert_eq!(app.frontmost_window, Some(from));
    for text in ["l", "s", "\r"] {
        assert!(
            app.defer_tab_spawn_input(from, from_winit, typed(text))
                .is_none(),
            "typed into the asking window, for the new one"
        );
    }
    assert!(
        app.defer_tab_spawn_input(WindowId(9), WinitWindowId::from(9u64), typed("x"))
            .is_some(),
        "another window's input is its own"
    );
    if let Some(ws) = app.windows.get_mut(&from) {
        ws.rows += 7;
        ws.cols += 11;
    }
    assert!(
        rx.try_recv().is_err(),
        "nothing is answered before it lands"
    );

    let (new, new_pty) = registered_piped_session(id);
    let sid = new.ctx.self_id.clone();
    assert!(app.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Ok(new),
        reader_gate: None,
    }));
    assert_eq!(rx.try_recv(), Ok(Ok(sid.as_str().to_string())));
    assert_eq!(app.windows.len(), windows + 1, "one window made");
    assert_eq!(
        app.frontmost_window,
        Some(wid),
        "the new window is in front"
    );
    let ws = &app.windows[&wid];
    assert_eq!(ws.tab_set.len(), 1);
    assert_eq!(
        (ws.rows, ws.cols),
        asked_grid,
        "made at the grid its shell was spawned at"
    );
    assert_eq!(app.focused_session_id(wid), Some(id));
    {
        let store = app.store.read().unwrap_or_else(|p| p.into_inner());
        let handle = store.by_sid(&sid).expect("registered");
        assert_eq!((handle.local_id, handle.parent.as_ref()), (id, None));
    }
    assert!(app.structural_invariants_ok());

    let own_winit = WinitWindowId::from(1u64);
    app.winit_to_window.insert(own_winit, wid);
    assert_eq!(app.settle_tab_spawn_input(from, true).0.len(), 0);
    let (events, repair) = app.settle_tab_spawn_input(wid, true);
    assert_eq!(repair, None);
    assert!(
        events.iter().all(|(through, ..)| *through == own_winit),
        "replayed through the new window's own OS window"
    );
    replay(&mut app, wid, events);
    assert_eq!(drain(&new_pty), b"ls\r", "in order, to the new window");
    assert!(
        drain(&old_pty).is_empty(),
        "never to the window left behind"
    );
    assert!(app.tab_spawns.held.is_empty() && app.tab_spawns.in_flight.is_empty());
}

#[test]
fn held_input_is_for_the_latest_ask_and_a_focus_loss_repairs_the_new_window() {
    let mut app = App::headless_for_test();
    let from = WindowId(0);
    let winit_id = WinitWindowId::from(0u64);
    let tab = book(&mut app, from, TabSpawnThen::Nothing);
    assert!(
        app.defer_tab_spawn_input(from, winit_id, typed("t"))
            .is_none()
    );
    let (_, wid) =
        test_support::book_window(&mut app, None, SpawnAdmission::Plain, TabSpawnThen::Nothing);
    assert!(
        app.defer_tab_spawn_input(from, winit_id, typed("w"))
            .is_none()
    );
    let later = book(&mut app, from, TabSpawnThen::Nothing);
    assert!(
        app.defer_tab_spawn_input(from, winit_id, typed("u"))
            .is_none()
    );
    let held: Vec<(WindowId, String)> = app
        .tab_spawns
        .held
        .iter()
        .map(|h| match &h.event {
            WindowEvent::Ime(Ime::Commit(t)) => (h.target, t.clone()),
            other => panic!("only commits were typed: {other:?}"),
        })
        .collect();
    assert_eq!(
        held,
        [
            (from, "t".to_string()),
            (wid, "w".to_string()),
            (from, "u".to_string())
        ]
    );
    app.tab_spawns.note_focus_loss(from);
    app.tab_spawns
        .in_flight
        .retain(|t| t.id != tab && t.id != later && t.window != wid);
    let (events, repair) = app.settle_tab_spawn_input(wid, true);
    assert_eq!(events.len(), 1);
    assert_eq!(
        repair,
        Some(wid),
        "typed under modifiers the asking window's focus loss cleared"
    );
}

#[test]
fn a_window_whose_spawn_failed_is_never_made() {
    let mut app = App::headless_for_test();
    let from = WindowId(0);
    let from_winit = WinitWindowId::from(0u64);
    app.winit_to_window.insert(from_winit, from);
    let (old, old_pty) = piped_session(app.next_session_id);
    app.push_stub_tab(from, old);
    let windows = app.windows.len();
    let (tx, rx) = channel();
    let (id, wid) = test_support::book_window(
        &mut app,
        None,
        SpawnAdmission::Plain,
        TabSpawnThen::Reply {
            reply: tx.into(),
            raise: false,
        },
    );
    let mods = WindowEvent::ModifiersChanged(aterm_window::event::Modifiers::default());
    assert!(
        app.defer_tab_spawn_input(from, from_winit, typed("rm"))
            .is_none()
    );
    assert_eq!(
        app.defer_tab_spawn_input(from, from_winit, mods.clone()),
        Some(mods),
        "held for the new window, and handed to the one typed into now"
    );
    app.tab_spawns.note_focus_loss(from);

    assert!(!app.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Err(std::io::Error::other("the pty could not be opened")),
        reader_gate: None,
    }));
    assert_eq!(rx.try_recv(), Ok(Err(NO_NEW_WINDOW.to_string())));
    assert_eq!(app.windows.len(), windows, "no window");
    assert!(!app.windows.contains_key(&wid));
    assert_eq!(app.frontmost_window, Some(from));
    assert!(said_no_window(&app, "the pty could not be opened"));

    let (events, repair) = app.settle_tab_spawn_input(wid, false);
    assert!(
        events.is_empty(),
        "the modifier change already reached where it was typed: {events:?}"
    );
    assert_eq!(
        repair,
        Some(from),
        "the repair goes back to where the modifier changes were typed"
    );
    replay(&mut app, from, events);
    assert!(
        drain(&old_pty).is_empty(),
        "the keys never reach the window left"
    );
    assert!(said_dropped(&app, "retype the last key"));
}

#[test]
fn a_window_whose_surface_cannot_be_made_rolls_back_and_reaps_its_shell() {
    let mut app = App::headless_for_test();
    let from = WindowId(0);
    let windows = app.windows.len();
    let (tx, rx) = channel();
    let (id, wid) = test_support::book_window(
        &mut app,
        None,
        SpawnAdmission::Plain,
        TabSpawnThen::Reply {
            reply: tx.into(),
            raise: false,
        },
    );
    let (session, holdout) = hup_ignoring_session(&app, id);
    let sid = session.ctx.self_id.clone();
    let no_surface = test_support::InertOs {
        exiting: false,
        attaches: false,
    };
    assert!(!app.install_spawned(
        TabSpawned {
            id,
            window: wid,
            session: Ok(session),
            reader_gate: None,
        },
        &no_surface,
    ));
    assert_eq!(rx.try_recv(), Ok(Err(NO_NEW_WINDOW.to_string())));
    assert_eq!(app.windows.len(), windows);
    assert!(!app.windows.contains_key(&wid));
    assert_eq!(app.frontmost_window, Some(from));
    assert!(app.pool.get(id).is_none());
    assert!(
        app.store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_sid(&sid)
            .is_none()
    );
    assert!(app.structural_invariants_ok());
    assert!(said_no_window(&app, "the window could not be opened"));
    assert!(
        aterm_pty::wait_for_reaps(Duration::from_secs(60)),
        "the rollback's reap did not settle"
    );
    assert!(
        holdout.wait_until_absent(),
        "the rolled-back window's shell outlived it"
    );
}

#[test]
fn nothing_lands_once_the_loop_is_exiting() {
    let mut app = App::headless_for_test();
    let from = WindowId(0);
    let windows = app.windows.len();
    let tabs = app.windows[&from].tab_set.len();
    let rows = app.messages.live_rows().count();
    let exiting = test_support::InertOs {
        exiting: true,
        attaches: true,
    };
    let (tab_tx, tab_rx) = channel();
    let tab = book(
        &mut app,
        from,
        TabSpawnThen::Reply {
            reply: tab_tx.into(),
            raise: false,
        },
    );
    let (win_tx, win_rx) = channel();
    let (window, wid) = test_support::book_window(
        &mut app,
        None,
        SpawnAdmission::Plain,
        TabSpawnThen::Reply {
            reply: win_tx.into(),
            raise: false,
        },
    );
    for (id, at) in [(tab, from), (window, wid)] {
        assert!(!app.install_spawned(
            TabSpawned {
                id,
                window: at,
                session: Ok(crate::stub_session(id)),
                reader_gate: None,
            },
            &exiting,
        ));
        assert!(app.pool.get(id).is_none());
    }
    assert_eq!(tab_rx.try_recv(), Ok(Err(QUITTING.to_string())));
    assert_eq!(win_rx.try_recv(), Ok(Err(QUITTING.to_string())));
    assert_eq!(app.windows.len(), windows);
    assert_eq!(app.windows[&from].tab_set.len(), tabs);
    assert_eq!(app.messages.live_rows().count(), rows, "no failure is said");
    assert!(app.tab_spawns.in_flight.is_empty());
}

#[test]
fn a_new_window_key_books_its_window_in_its_own_turn() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let next = app.next_window_id;
    app.dispatch_action(wid, crate::keybinding::Action::NewWindow);
    assert_eq!(app.next_window_id, next, "headless ignores it");
    assert!(!said_no_window(&app, "no event loop"));

    app.headless = false;
    app.dispatch_action(wid, crate::keybinding::Action::NewWindow);
    assert_eq!(app.next_window_id, next + 1, "the key minted its window");
    assert!(said_no_window(&app, "no event loop to host the new window"));
    if crate::app_input::HARDCODED_SUPER_CHORDS {
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.mods = ModifiersState::SUPER;
        }
        app.on_key(
            wid,
            aterm_window::event::KeyEvent::synthetic_for_test(
                aterm_window::keyboard::PhysicalKey::Code(aterm_window::keyboard::KeyCode::KeyN),
                aterm_window::keyboard::Key::Character("n".into()),
                Some("n".into()),
                aterm_window::keyboard::KeyLocation::Standard,
                aterm_window::event::ElementState::Pressed,
                false,
            ),
        );
        assert_eq!(app.next_window_id, next + 2, "Cmd-N minted its window");
    }
}

fn moved(x: f64) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: aterm_window::event::DeviceId::dummy(),
        position: aterm_window::dpi::PhysicalPosition::new(x, 1.0),
    }
}

fn click(state: aterm_window::event::ElementState) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: aterm_window::event::DeviceId::dummy(),
        state,
        button: aterm_window::event::MouseButton::Left,
    }
}

fn wheel(lines: f32) -> WindowEvent {
    WindowEvent::MouseWheel {
        device_id: aterm_window::event::DeviceId::dummy(),
        delta: aterm_window::event::MouseScrollDelta::LineDelta(0.0, lines),
        phase: aterm_window::event::TouchPhase::Moved,
    }
}

fn mods() -> WindowEvent {
    WindowEvent::ModifiersChanged(aterm_window::event::Modifiers::default())
}

fn named(events: &[Released]) -> Vec<String> {
    events
        .iter()
        .map(|(_, event, _)| match event {
            WindowEvent::Ime(Ime::Commit(t)) => t.clone(),
            WindowEvent::ModifiersChanged(_) => "mods".to_string(),
            WindowEvent::CursorMoved { position, .. } => format!("move {}", position.x),
            WindowEvent::MouseInput { state, .. } if state.is_pressed() => "press".into(),
            WindowEvent::MouseInput { .. } => "release".to_string(),
            WindowEvent::MouseWheel {
                delta: aterm_window::event::MouseScrollDelta::LineDelta(_, y),
                ..
            } => format!("wheel {y}"),
            other => panic!("not a test event: {other:?}"),
        })
        .collect()
}

#[test]
fn the_pointer_waits_with_the_keys_for_a_tab_landing_under_it() {
    use aterm_window::event::ElementState::{Pressed, Released};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let winit_id = WinitWindowId::from(0u64);
    app.winit_to_window.insert(winit_id, wid);
    assert!(
        app.defer_tab_spawn_input(wid, winit_id, click(Pressed))
            .is_some(),
        "nothing in flight"
    );
    let id = book(&mut app, wid, TabSpawnThen::Nothing);
    for event in [
        typed("a"),
        moved(1.0),
        moved(2.0),
        moved(3.0),
        click(Pressed),
        click(Released),
        wheel(1.0),
        wheel(2.0),
        typed("b"),
    ] {
        assert!(app.defer_tab_spawn_input(wid, winit_id, event).is_none());
    }
    assert!(
        app.defer_tab_spawn_input(WindowId(9), winit_id, click(Pressed))
            .is_some(),
        "another window's pointer is its own"
    );
    app.tab_spawns.in_flight.retain(|t| t.id != id);
    let (events, repair) = app.settle_tab_spawn_input(wid, true);
    assert_eq!(repair, None);
    assert_eq!(
        named(&events),
        ["a", "move 3", "press", "release", "wheel 3", "b"]
    );
    assert!(events.iter().all(|(via, ..)| *via == winit_id));

    let (_, window) =
        test_support::book_window(&mut app, None, SpawnAdmission::Plain, TabSpawnThen::Nothing);
    assert!(
        app.defer_tab_spawn_input(wid, winit_id, click(Pressed))
            .is_some()
    );
    assert_eq!(
        app.defer_tab_spawn_input(wid, winit_id, mods()),
        Some(mods())
    );
    assert!(
        app.tab_spawns
            .held
            .iter()
            .all(|h| h.target == window && h.applied)
    );
}

#[test]
fn a_click_held_on_the_tab_strip_is_dropped_not_replayed_onto_a_moved_chip() {
    use aterm_window::event::ElementState::{Pressed, Released};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let winit_id = WinitWindowId::from(0u64);
    app.winit_to_window.insert(winit_id, wid);
    let (cw, ch) = app.win_cell_size(wid);
    let (cw, ch, pad) = (cw as f64, ch as f64, app.win_pad(wid) as f64);
    let on_strip = (pad + cw * 2.5, pad + ch * 0.5);
    let in_grid = (pad + cw * 10.5, pad + ch * 6.5);
    let at = |(x, y): (f64, f64)| WindowEvent::CursorMoved {
        device_id: aterm_window::event::DeviceId::dummy(),
        position: aterm_window::dpi::PhysicalPosition::new(x, y),
    };
    let clicks = |app: &mut App, strip: bool| {
        app.tab_strip_rows = u16::from(strip);
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.last_cursor_px = on_strip;
        }
        let id = book(app, wid, TabSpawnThen::Nothing);
        for event in [
            click(Pressed),
            click(Released),
            at(in_grid),
            click(Pressed),
            click(Released),
            at(on_strip),
            at(in_grid),
            click(Pressed),
            click(Released),
        ] {
            assert!(app.defer_tab_spawn_input(wid, winit_id, event).is_none());
        }
        app.tab_spawns.in_flight.retain(|t| t.id != id);
        app.settle_tab_spawn_input(wid, true).0
    };
    let strip = clicks(&mut app, true);
    assert!(app.strip_col_at(wid, on_strip.0, on_strip.1).is_some());
    assert!(app.strip_col_at(wid, in_grid.0, in_grid.1).is_none());
    let grid_x = format!("move {}", in_grid.0);
    assert_eq!(
        named(&strip),
        [
            grid_x.as_str(),
            "press",
            "release",
            grid_x.as_str(),
            "press",
            "release"
        ]
    );
    assert_eq!(
        named(&clicks(&mut app, false)),
        [
            "press",
            "release",
            grid_x.as_str(),
            "press",
            "release",
            grid_x.as_str(),
            "press",
            "release"
        ],
        "no strip: all of it (the two moves in a row fold into one)"
    );
}

#[test]
fn keys_typed_for_a_tab_the_spawner_started_late_are_held_until_it_lands() {
    let exec_wait = aterm_pty::START_HANG;
    let fresh = || {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let winit_id = WinitWindowId::from(0u64);
        app.winit_to_window.insert(winit_id, wid);
        let (old, old_pty) = piped_session(app.next_session_id);
        app.push_stub_tab(wid, old);
        let id = book(&mut app, wid, TabSpawnThen::Nothing);
        let picked = app.tab_spawns.in_flight[0].asked + Duration::from_secs(100);
        app.tab_spawns.in_flight[0].completion.note_picked(picked);
        for text in ["l", "s"] {
            assert!(
                app.defer_tab_spawn_input(wid, winit_id, typed(text))
                    .is_none()
            );
        }
        (app, wid, winit_id, id, picked, old_pty)
    };

    let (mut app, wid, winit_id, id, picked, old_pty) = fresh();
    let late = picked + exec_wait + Duration::from_secs(5);
    let (events, _) = app.expire_tab_spawn_input(winit_id, late);
    assert!(events.is_empty(), "nothing settles: {events:?}");
    assert_eq!(app.tab_spawns.held.len(), 2, "the keys are still held");
    assert_eq!(
        app.tab_spawns.hold_target(wid, late),
        Some((wid, picked + SPAWN_HOLD, id)),
        "new keys are held for it too"
    );
    let (new, new_pty) = piped_session(id);
    assert!(app.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Ok(new),
        reader_gate: None,
    }));
    let (events, _) = app.settle_tab_spawn_input(wid, true);
    replay(&mut app, wid, events);
    assert_eq!(drain(&new_pty), b"ls", "in order, to the tab that started");
    assert!(
        drain(&old_pty).is_empty(),
        "never to the tab the person left"
    );

    let (mut app, wid, _, _, picked, _) = fresh();
    let queued = book(&mut app, wid, TabSpawnThen::Nothing);
    assert_eq!(
        app.tab_spawns.hold_target(wid, late),
        Some((wid, picked + SPAWN_HOLD, queued)),
        "held on the clock of the spawn ahead"
    );
    assert_eq!(app.tab_spawns.hold_target(wid, picked + SPAWN_HOLD), None);

    let (mut app, wid, winit_id, id, picked, _) = fresh();
    let (new, new_pty) = piped_session(id);
    let completion = Arc::clone(&app.tab_spawns.in_flight[0].completion);
    assert!(completion.store(TabSpawned {
        id,
        window: wid,
        session: Ok(new),
        reader_gate: None,
    }));
    let landed_late = picked + SPAWN_HOLD + Duration::from_secs(600);
    let (events, _) = app.expire_tab_spawn_input(winit_id, landed_late);
    assert!(events.is_empty(), "nothing settles: {events:?}");
    assert_eq!(app.tab_spawns.held.len(), 2, "held until it lands");
    let done = completion.take().expect("the verdict");
    assert!(app.install_spawned_tab(done));
    let (events, _) = app.settle_tab_spawn_input(wid, true);
    replay(&mut app, wid, events);
    assert_eq!(drain(&new_pty), b"ls");

    let (mut app, wid, winit_id, _, picked, old_pty) = fresh();
    let over = picked + SPAWN_HOLD;
    let (events, _) = app.expire_tab_spawn_input(winit_id, over);
    assert!(events.is_empty(), "keystrokes are dropped: {events:?}");
    assert!(app.tab_spawns.held.is_empty());
    assert!(said_dropped(&app, "retype the last 2 keys"));
    assert_eq!(app.tab_spawns.hold_target(wid, over), None);
    assert!(drain(&old_pty).is_empty());
}

#[test]
fn the_spawners_start_times_the_hold_on_a_tabs_keys() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let id = book(&mut app, wid, TabSpawnThen::Nothing);
    let asked = app.tab_spawns.in_flight[0].asked;
    let completion = Arc::clone(&app.tab_spawns.in_flight[0].completion);
    let picked = asked + Duration::from_secs(100);
    let later = asked + SPAWN_HOLD + Duration::from_secs(1);
    assert_eq!(
        app.tab_spawns.hold_target(wid, later),
        None,
        "before the start, the hold from the ask has run out"
    );
    assert!(start_picked(&completion, picked), "a job to run");
    assert_eq!(completion.picked(), Some(picked));
    assert_eq!(
        app.tab_spawns.hold_target(wid, later),
        Some((wid, picked + SPAWN_HOLD, id)),
        "held from the spawner's start"
    );
    let cancelled = TabSpawnCompletion::default();
    cancelled.cancel();
    assert!(!start_picked(&cancelled, picked), "a cancelled job");
    assert_eq!(cancelled.picked(), None);
}

#[test]
fn input_held_past_the_hold_is_dropped_never_replayed_late() {
    use aterm_window::event::ElementState::{Pressed, Released};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let winit_id = WinitWindowId::from(0u64);
    app.winit_to_window.insert(winit_id, wid);
    let (new, new_pty) = piped_session(app.next_session_id);
    let id = book(&mut app, wid, TabSpawnThen::Nothing);
    for event in [
        typed("a"),
        mods(),
        moved(4.0),
        click(Pressed),
        click(Released),
    ] {
        assert!(app.defer_tab_spawn_input(wid, winit_id, event).is_none());
    }
    let inside = app.expire_tab_spawn_input(winit_id, Instant::now());
    assert!(inside.0.is_empty(), "inside the hold nothing settles");
    assert_eq!(app.tab_spawns.held.len(), 5);

    let past = Instant::now() + SPAWN_HOLD;
    let (events, repair) = app.expire_tab_spawn_input(winit_id, past);
    assert_eq!(named(&events), ["mods", "move 4"]);
    assert_eq!(repair, None);
    assert!(app.tab_spawns.held.is_empty());
    assert!(said_dropped(&app, "retype the last key"));

    assert!(app.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Ok(new),
        reader_gate: None,
    }));
    let (events, _) = app.settle_tab_spawn_input(wid, true);
    assert!(events.is_empty(), "{events:?}");
    assert!(
        drain(&new_pty).is_empty(),
        "never replayed into the late tab"
    );

    let late = book(&mut app, wid, TabSpawnThen::Nothing);
    assert!(
        app.defer_tab_spawn_input(wid, winit_id, typed("c"))
            .is_none()
    );
    let ended = Instant::now()
        .checked_sub(Duration::from_millis(1))
        .expect("a monotonic clock past its first millisecond");
    app.tab_spawns.held.iter_mut().for_each(|h| h.until = ended);
    app.tab_spawns.in_flight.retain(|t| t.id != late);
    let (events, _) = app.settle_tab_spawn_input(wid, true);
    assert!(
        events.is_empty(),
        "a late landing replays nothing: {events:?}"
    );
}

#[test]
fn only_a_keystroke_refused_past_the_cap_is_counted() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let winit_id = WinitWindowId::from(0u64);
    let id = book(&mut app, wid, TabSpawnThen::Nothing);
    for n in 0..MAX_HELD_INPUT {
        assert!(
            app.defer_tab_spawn_input(wid, winit_id, typed(&format!("{n}")))
                .is_none()
        );
    }
    let pressed = click(aterm_window::event::ElementState::Pressed);
    assert!(app.defer_tab_spawn_input(wid, winit_id, pressed).is_none());
    app.tab_spawns.in_flight.retain(|t| t.id != id);
    let (events, repair) = app.settle_tab_spawn_input(wid, true);
    assert_eq!(events.len(), MAX_HELD_INPUT);
    assert_eq!(repair, Some(wid));
    assert!(
        !app.messages
            .live_rows()
            .any(|l| l.msg.title == "Keystrokes dropped"),
        "a click is no keystroke"
    );
}

/// A HELD KEY REPLAYS AS THE KEY IT ARRIVED AS: whether a key is one an `hwkey` burst posted
/// is read off the event AppKit is dispatching (`-[NSApp currentEvent]`), which by a held
/// key's replay is another one (the person's mouse, or a burst's next key). So it is decided
/// as the key arrives (`crate::arriving_input_origin`, into `App::input_origin`) and the
/// hold carries it: a posted ⌘V held under a pending menu mark (a burst's ⌘T, say) comes back
/// posted, and its replay through the key arm (`App::replay_window_event` runs
/// `App::dispatch_hw_key` with the carried word) pastes no clipboard while nothing posted is
/// being dispatched; the person's ⌘V held beside it comes back the person's and pastes while a
/// burst's key is being dispatched. A key typed before an incoming handoff's Commit carries
/// the word the same way.
/// NEGATIVE CONTROL: with the hold keeping no word (`origin: InputOrigin::Person` in
/// `defer_tab_spawn_input`, which is what the branch's replay that read the current event
/// found here: no posted key under dispatch), the burst's held ⌘V sent `s3cret-pass`.
#[test]
fn a_held_key_replays_as_the_key_it_arrived_as() {
    use aterm_window::event::{DeviceId, ElementState, KeyEvent};
    use aterm_window::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, PhysicalKey, SmolStr};

    use crate::hwkey::{CURRENT_INPUT_STUB, NS_EVENT_TYPE_KEY_DOWN, NativeInput, note_posted};
    const SECRET: &str = "s3cret-pass";
    if !crate::app_input::HARDCODED_SUPER_CHORDS {
        return;
    }
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let winit_id = WinitWindowId::from(0u64);
    app.winit_to_window.insert(winit_id, wid);
    let (mut pty, input) = std::os::unix::net::UnixStream::pair().expect("an input observer");
    pty.set_nonblocking(true).expect("nonblocking");
    let sink = Arc::new(SinkWriter::new_owned(input.into()));
    app.push_stub_tab(
        wid,
        crate::stub_session_with_sink(app.next_session_id, Arc::clone(&sink)),
    );
    // What the front program has been sent, once the sink's ordered egress drained.
    let mut sent = || {
        use std::io::Read as _;
        crate::app_input::paste_order::await_drain_for_test(&sink);
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        while let Ok(n @ 1..) = pty.read(&mut chunk) {
            bytes.extend_from_slice(&chunk[..n]);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    };
    crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = Some(SECRET.into()));
    let cmd_v = || WindowEvent::KeyboardInput {
        device_id: DeviceId::dummy(),
        event: KeyEvent::synthetic_for_test(
            PhysicalKey::Code(KeyCode::KeyV),
            Key::Character(SmolStr::new("v")),
            Some(SmolStr::new("v")),
            KeyLocation::Standard,
            ElementState::Pressed,
            false,
        ),
        is_synthetic: false,
    };
    let key = |stamp: f64, posted: bool| {
        let key = NativeInput {
            kind: NS_EVENT_TYPE_KEY_DOWN,
            keycode: 9,
            timestamp: stamp,
            source_pid: crate::hwkey::HID_SYSTEM,
        };
        if posted {
            note_posted(&[key]).expect("room");
        }
        key
    };
    let dispatching = |current: Option<NativeInput>, f: &mut dyn FnMut()| {
        CURRENT_INPUT_STUB.with(|stub| stub.set(current));
        f();
        CURRENT_INPUT_STUB.with(|stub| stub.set(None));
    };
    // The event as `App::on_window_event` takes it: decided as it arrives, then held.
    let arrive = |app: &mut App, current: NativeInput| {
        let mut origin = InputOrigin::Person;
        dispatching(Some(current), &mut || {
            origin = crate::arriving_input_origin(&cmd_v());
        });
        app.input_origin = origin;
        assert!(app.defer_tab_spawn_input(wid, winit_id, cmd_v()).is_none());
        app.input_origin = InputOrigin::Person;
        origin
    };
    // The replay as `App::replay_window_event` runs it through the key arm.
    let replay = |app: &mut App, released: Released, current: Option<NativeInput>| {
        let (_, WindowEvent::KeyboardInput { event, .. }, origin) = released else {
            panic!("a key");
        };
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::SUPER;
        dispatching(current, &mut || {
            app.dispatch_hw_key(wid, event.clone(), origin);
        });
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::empty();
    };

    let marks = Arc::clone(&app.tab_spawns.menu);
    let mark = marks.mint(Instant::now());
    assert_eq!(
        arrive(&mut app, key(2_000.25, true)),
        InputOrigin::Posted,
        "a burst's ⌘V"
    );
    assert_eq!(
        arrive(&mut app, key(2_000.5, false)),
        InputOrigin::Person,
        "the person's ⌘V"
    );
    assert!(marks.settle(mark));
    let mut back = app.claim_menu_held_input(mark, app.tab_spawns.in_flight.len());
    let words: Vec<InputOrigin> = back.iter().map(|(.., origin)| *origin).collect();
    let persons = back.pop().expect("the person's ⌘V");
    let bursts = back.pop().expect("the burst's ⌘V");
    replay(&mut app, bursts, None);
    let burst_sent = sent();
    assert!(
        !burst_sent.contains(SECRET),
        "the burst's held ⌘V sent {burst_sent:?}"
    );
    replay(&mut app, persons, Some(key(2_000.75, true)));
    let person_sent = sent();
    assert!(
        person_sent.contains(SECRET),
        "the person's held ⌘V sent {person_sent:?}"
    );
    assert_eq!(
        words,
        [InputOrigin::Posted, InputOrigin::Person],
        "each came back as it arrived"
    );

    app.incoming_handoff_pending = true;
    for origin in [
        InputOrigin::Posted,
        InputOrigin::Relayed,
        InputOrigin::Person,
    ] {
        app.input_origin = origin;
        app.queue_pre_commit_input(winit_id, cmd_v());
    }
    app.input_origin = InputOrigin::Person;
    assert_eq!(
        app.handoff_deferred_input
            .iter()
            .map(|(.., origin)| *origin)
            .collect::<Vec<_>>(),
        [
            InputOrigin::Posted,
            InputOrigin::Relayed,
            InputOrigin::Person
        ],
        "a key typed before Commit carries its word"
    );
    crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = None);
}

/// Native delivery can wait until the raw ledger has moved on, and App's spawn hold can
/// delay it again. Both stages must preserve the source verdict, including definitive false.
#[test]
fn held_ime_replays_its_source_after_the_raw_ledger_forgets_it() {
    use crate::hwkey::{
        CURRENT_INPUT_STUB, NS_EVENT_TYPE_KEY_DOWN, NS_EVENT_TYPE_KEY_UP, NativeInput, classify,
        note_posted, with_window_event_origin_for_test,
    };

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let winit_id = WinitWindowId::from(0u64);
    app.winit_to_window.insert(winit_id, wid);
    let (session, pty) = piped_session(app.next_session_id);
    let ctx = Arc::clone(&session.ctx);
    app.push_stub_tab(wid, session);
    let a = NativeInput {
        kind: NS_EVENT_TYPE_KEY_DOWN,
        keycode: 0,
        timestamp: 6_100.0,
        source_pid: crate::hwkey::HID_SYSTEM,
    };
    let b = NativeInput {
        timestamp: 6_101.0,
        ..a
    };
    let up = NativeInput {
        kind: NS_EVENT_TYPE_KEY_UP,
        ..b
    };
    note_posted(&[a, b, up]).expect("room");
    let retained_a = classify(a);
    assert_eq!(
        retained_a,
        InputOrigin::Posted,
        "A was classified at its native source"
    );
    assert_eq!(classify(b), InputOrigin::Posted);
    assert_eq!(classify(up), InputOrigin::Posted);
    assert_eq!(
        classify(a),
        InputOrigin::Person,
        "PRECONDITION: later keys consumed A's raw ledger entry"
    );

    let marks = Arc::clone(&app.tab_spawns.menu);
    let mark = marks.mint(Instant::now());
    let hold = |app: &mut App, origin: InputOrigin, text: &str| {
        with_window_event_origin_for_test(origin, || {
            for ime in [
                Ime::Preedit(text.into(), Some((0, 0))),
                Ime::Commit(text.into()),
            ] {
                let event = WindowEvent::Ime(ime);
                let arrived = crate::arriving_input_origin(&event);
                assert_eq!(arrived, origin);
                app.note_window_input_arrival(&event, arrived);
                app.input_origin = arrived;
                assert!(app.defer_tab_spawn_input(wid, winit_id, event).is_none());
                app.input_origin = InputOrigin::Person;
            }
        });
    };
    CURRENT_INPUT_STUB.with(|current| current.set(None));
    hold(&mut app, retained_a, "日本");
    CURRENT_INPUT_STUB.with(|current| current.set(Some(up)));
    assert_eq!(
        crate::hwkey::current_event_origin(),
        InputOrigin::Posted,
        "the opposite ambient posted key"
    );
    hold(&mut app, InputOrigin::Person, "語");
    CURRENT_INPUT_STUB.with(|current| current.set(None));
    assert!(marks.settle(mark));
    let mut released = app.claim_menu_held_input(mark, app.tab_spawns.in_flight.len());
    assert_eq!(
        released
            .iter()
            .map(|(.., origin)| *origin)
            .collect::<Vec<_>>(),
        [
            InputOrigin::Posted,
            InputOrigin::Posted,
            InputOrigin::Person,
            InputOrigin::Person
        ]
    );
    let physical = released.split_off(2);

    with_window_event_origin_for_test(InputOrigin::Person, || {
        replay(&mut app, wid, released);
    });
    crate::app_input::paste_order::await_drain_for_test(&ctx.sink);
    assert_eq!(drain(&pty), "日本".as_bytes());
    assert_eq!(
        ctx.human_input.counts(),
        crate::human_input::PersonCounts::default()
    );
    assert_eq!(
        ctx.human_input.typed_ms_since(crate::metrics::now_us()),
        None
    );

    CURRENT_INPUT_STUB.with(|current| current.set(Some(up)));
    with_window_event_origin_for_test(InputOrigin::Posted, || {
        replay(&mut app, wid, physical);
    });
    CURRENT_INPUT_STUB.with(|current| current.set(None));
    crate::app_input::paste_order::await_drain_for_test(&ctx.sink);
    assert_eq!(drain(&pty), "語".as_bytes());
    let counts = ctx.human_input.counts();
    assert_eq!((counts.seq, counts.reached), (2, 2));
    assert!(
        ctx.human_input
            .typed_ms_since(crate::metrics::now_us())
            .is_some()
    );
    assert!(!app.person_gesture_refused());

    app.incoming_handoff_pending = true;
    for origin in [InputOrigin::Posted, InputOrigin::Person] {
        app.input_origin = origin;
        app.queue_pre_commit_input(winit_id, WindowEvent::Ime(Ime::Commit("x".into())));
    }
    app.input_origin = InputOrigin::Person;
    assert_eq!(
        app.handoff_deferred_input
            .iter()
            .map(|(.., origin)| *origin)
            .collect::<Vec<_>>(),
        [InputOrigin::Posted, InputOrigin::Person],
        "handoff Commit retains IME provenance too"
    );
}

#[test]
fn input_typed_before_a_menu_command_is_dispatched_waits_for_it() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let winit_id = WinitWindowId::from(0u64);
    app.winit_to_window.insert(winit_id, wid);
    let marks = Arc::clone(&app.tab_spawns.menu);
    assert!(
        app.defer_tab_spawn_input(wid, winit_id, typed("x"))
            .is_some(),
        "no mark, nothing in flight"
    );

    let mark = marks.mint(Instant::now());
    for text in ["l", "s"] {
        assert!(
            app.defer_tab_spawn_input(wid, winit_id, typed(text))
                .is_none()
        );
    }
    let (new, new_pty) = piped_session(app.next_session_id);
    let booked = app.tab_spawns.in_flight.len();
    let id = book(&mut app, wid, TabSpawnThen::Nothing);
    assert!(marks.settle(mark));
    assert!(app.claim_menu_held_input(mark, booked).is_empty());
    assert!(app.tab_spawns.held.iter().all(|h| h.menu == MenuMark::NONE));
    assert!(app.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Ok(new),
        reader_gate: None,
    }));
    let (events, _) = app.settle_tab_spawn_input(wid, true);
    replay(&mut app, wid, events);
    assert_eq!(drain(&new_pty), b"ls", "into the tab the menu opened");

    let origin = app.palette_menu_origin(wid).unwrap();
    let mark = marks.mint(Instant::now());
    for text in ["q", "r"] {
        assert!(
            app.defer_tab_spawn_input(wid, winit_id, typed(text))
                .is_none()
        );
    }
    let booked = app.tab_spawns.in_flight.len();
    app.frontmost_window = None;
    assert!(
        app.menu_action_for_origin(crate::menu::MenuAction::Copy, Some(origin))
            .is_none()
    );
    assert!(marks.settle(mark));
    let back = app.claim_menu_held_input(mark, booked);
    assert_eq!(named(&back), ["q", "r"]);
    assert!(app.tab_spawns.held.is_empty());
    app.frontmost_window = Some(wid);

    let first = marks.mint(Instant::now());
    assert!(
        app.defer_tab_spawn_input(wid, winit_id, typed("a"))
            .is_none()
    );
    let second = marks.mint(Instant::now());
    for event in [typed("b"), mods(), moved(5.0)] {
        assert!(app.defer_tab_spawn_input(wid, winit_id, event).is_none());
    }
    let booked = app.tab_spawns.in_flight.len();
    let (_, one) =
        test_support::book_window(&mut app, None, SpawnAdmission::Plain, TabSpawnThen::Nothing);
    assert!(marks.settle(first));
    assert!(app.claim_menu_held_input(first, booked).is_empty());
    let booked = app.tab_spawns.in_flight.len();
    let (_, two) =
        test_support::book_window(&mut app, None, SpawnAdmission::Plain, TabSpawnThen::Nothing);
    assert!(marks.settle(second));
    let back = app.claim_menu_held_input(second, booked);
    assert_eq!(named(&back), ["mods", "move 5"]);
    let held: Vec<(WindowId, String)> = app
        .tab_spawns
        .held
        .iter()
        .map(|h| {
            (
                h.target,
                named(&[(h.typed_into, h.event.clone(), h.origin)]).remove(0),
            )
        })
        .collect();
    assert_eq!(
        held,
        [
            (one, "a".to_string()),
            (two, "b".to_string()),
            (two, "mods".to_string())
        ]
    );
    app.tab_spawns.held.clear();
    app.tab_spawns.in_flight.clear();

    let stale = Instant::now()
        .checked_sub(MENU_HOLD + Duration::from_secs(1))
        .expect("a monotonic clock past the hold");
    let _ = marks.mint(stale);
    assert!(
        app.defer_tab_spawn_input(wid, winit_id, typed("y"))
            .is_some()
    );
    let _ = marks.mint(Instant::now());
    let handed_back = app.replaying(false, |app| {
        app.defer_tab_spawn_input(wid, winit_id, typed("z"))
    });
    assert!(handed_back.is_some(), "typed before the pending mark");
}

fn redeliver(app: &mut App, wid: WindowId, events: Vec<Released>) {
    app.replaying(true, |app| {
        for (via, event, _) in events {
            if let Some(WindowEvent::Ime(Ime::Commit(text))) =
                app.defer_tab_spawn_input(wid, via, event)
            {
                app.on_ime_commit(wid, text);
            }
        }
    });
}

#[test]
fn replayed_input_goes_to_the_spawn_that_was_newest_when_it_was_typed() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let winit_id = WinitWindowId::from(0u64);
    app.winit_to_window.insert(winit_id, wid);
    let (old, old_pty) = piped_session(app.next_session_id);
    app.push_stub_tab(wid, old);

    let (first, first_pty) = piped_session(app.next_session_id);
    let a = book(&mut app, wid, TabSpawnThen::Nothing);
    for text in ["l", "s"] {
        assert!(
            app.defer_tab_spawn_input(wid, winit_id, typed(text))
                .is_none()
        );
    }
    let (second, second_pty) = piped_session(app.next_session_id);
    let b = book(&mut app, wid, TabSpawnThen::Nothing);
    assert!(
        app.defer_tab_spawn_input(wid, winit_id, typed("pwd"))
            .is_none()
    );
    assert!(app.install_spawned_tab(TabSpawned {
        id: a,
        window: wid,
        session: Ok(first),
        reader_gate: None,
    }));
    let (events, _) = app.settle_landed_spawn_input(wid, Some(a), true);
    redeliver(&mut app, wid, events);
    assert_eq!(drain(&first_pty), b"ls", "the first tab's own keys, now");
    assert!(app.install_spawned_tab(TabSpawned {
        id: b,
        window: wid,
        session: Ok(second),
        reader_gate: None,
    }));
    let (events, _) = app.settle_landed_spawn_input(wid, Some(b), true);
    redeliver(&mut app, wid, events);
    assert_eq!(drain(&second_pty), b"pwd");
    assert!(drain(&old_pty).is_empty(), "the tab left gets none");

    let (third, third_pty) = piped_session(app.next_session_id);
    let c = book(&mut app, wid, TabSpawnThen::Nothing);
    assert!(
        app.defer_tab_spawn_input(wid, winit_id, typed("ls"))
            .is_none()
    );
    let (_, window) =
        test_support::book_window(&mut app, None, SpawnAdmission::Plain, TabSpawnThen::Nothing);
    assert!(
        app.defer_tab_spawn_input(wid, winit_id, typed("top"))
            .is_none()
    );
    assert!(app.install_spawned_tab(TabSpawned {
        id: c,
        window: wid,
        session: Ok(third),
        reader_gate: None,
    }));
    let (events, _) = app.settle_landed_spawn_input(wid, Some(c), true);
    redeliver(&mut app, wid, events);
    assert_eq!(drain(&third_pty), b"ls", "never the window booked after it");
    let waiting: Vec<_> = app.tab_spawns.held.iter().map(|h| h.target).collect();
    assert_eq!(waiting, [window], "`top` still waits for the window");
    app.tab_spawns.held.clear();
    app.tab_spawns.in_flight.clear();

    let d = app.replaying(true, |app| {
        let d = book(app, wid, TabSpawnThen::Nothing);
        assert!(
            app.defer_tab_spawn_input(wid, winit_id, typed("x"))
                .is_none()
        );
        d
    });
    assert_eq!(
        app.tab_spawns
            .held
            .iter()
            .map(|h| h.spawn)
            .collect::<Vec<_>>(),
        [Some(d)]
    );

    app.tab_spawns.held.clear();
    app.tab_spawns.in_flight.clear();
    let same_tick = Instant::now();
    let spare = app.next_session_id;
    app.next_session_id += 1;
    let before = book(&mut app, wid, TabSpawnThen::Nothing);
    app.tab_spawns.in_flight.last_mut().unwrap().asked = same_tick;
    app.replaying(true, |app| {
        assert_eq!(
            app.tab_spawns.hold_target(wid, same_tick),
            None,
            "older booking"
        );
        let fresh = book(app, wid, TabSpawnThen::Nothing);
        app.tab_spawns.in_flight.last_mut().unwrap().asked = same_tick;
        assert_eq!(
            app.tab_spawns.hold_target(wid, same_tick),
            Some((wid, same_tick + SPAWN_HOLD, fresh)),
            "first booking made by this replay, despite the equal clock"
        );
        let fallback = book(app, wid, TabSpawnThen::Nothing);
        let adopted = app.tab_spawns.in_flight.last_mut().unwrap();
        adopted.asked = same_tick;
        adopted.id = spare;
        adopted.alt_id = Some(fallback);
        assert_eq!(
            app.tab_spawns.hold_target(wid, same_tick),
            Some((wid, same_tick + SPAWN_HOLD, spare)),
            "an adopted spare uses the new reservation for admission"
        );
    });
    app.tab_spawns.in_flight.retain(|t| t.id == before);
    app.replaying(false, |app| {
        assert_eq!(
            app.tab_spawns.hold_target(wid, same_tick),
            Some((wid, same_tick + SPAWN_HOLD, before))
        );
    });
    app.tab_spawns.in_flight.clear();
}
