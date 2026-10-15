// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{RootAnswer, leaf_program};
use crate::quit_safety::SessionRoot;
use crate::{App, WindowId, pane, restore};
use aterm_uds::spawnfd::InheritNothing as _;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::time::{Duration, Instant};

fn root(comm: &str, children: &[i32]) -> RootAnswer {
    RootAnswer::Read(Some(SessionRoot {
        comm: comm.to_string(),
        children: children.to_vec(),
    }))
}

/// Every branch of the decision, each with its negative control.
#[test]
fn a_leaf_ran_a_program_when_its_job_its_root_or_a_child_says_so() {
    let sh = root("zsh", &[]);
    // NEGATIVE CONTROL: a shell at its prompt, holding its own foreground,
    // with no child, ran nothing — and a login shell's `-zsh` is a shell.
    assert_eq!(leaf_program(100, 100, &sh, Some((100, "zsh"))), None);
    assert_eq!(leaf_program(100, 100, &root("-zsh", &[]), None), None);
    // A foreground job, named only for its own group (the quit's test).
    assert_eq!(
        leaf_program(200, 100, &sh, Some((200, "vim"))).as_deref(),
        Some("vim")
    );
    assert_eq!(
        leaf_program(200, 100, &sh, Some((100, "zsh"))).as_deref(),
        Some("")
    );
    // R1: the root is the program (`aterm -e claude`, `exec vim`).
    assert_eq!(
        leaf_program(100, 100, &root("2.1.281", &[]), Some((100, "claude"))).as_deref(),
        Some("claude")
    );
    assert_eq!(
        leaf_program(100, 100, &root("vim", &[]), Some((100, "zsh"))).as_deref(),
        Some(""),
        "a name resolved before the exec still says the shell: unnamed"
    );
    // R2: a background or suspended job leaves the foreground at the shell.
    assert_eq!(
        leaf_program(100, 100, &root("bash", &[4242]), None).as_deref(),
        Some("")
    );
    // The table could not answer: loud, unnamed.
    assert_eq!(
        leaf_program(100, 100, &RootAnswer::Read(None), None).as_deref(),
        Some("")
    );
    // A platform whose job test covers the root's jobs is not asked.
    assert_eq!(leaf_program(100, 100, &RootAnswer::Unasked, None), None);
}

/// A PTY pair with BOTH ends close-on-exec from birth
/// (`aterm_pty::open_pty_pair`). Never `openpty(3)`, whose ends are
/// inheritable: the leader spawned onto its pair kept a copy of its own
/// master, so closing the test's copy never hung the terminal up, and at
/// the leader's exit the tty waited for output whose only reader was the
/// exiting process. MEASURED 2026-09-30: eleven `bash` / `sh -i` / `sleep
/// 30` processes sat in `?E` for a day, each holding its own pty's
/// `/dev/ptmx`; a `tcflush` of the slave's output released them.
fn open_pty_pair() -> (i32, i32) {
    aterm_pty::open_pty_pair().expect("open_pty_pair")
}

/// `argv` as the session leader of a fresh PTY whose controlling
/// terminal it is — a spawned tab's shape. Answers `(master, pid)`.
/// Not waited here: the pid stays reserved until the test's `Session`
/// drops, whose hang-up and reaper own it, exactly as a spawned tab's.
fn spawn_leader(argv: &[&str]) -> (i32, i32) {
    spawn_leader_on(open_pty_pair(), argv)
}

/// [`spawn_leader`] onto the pair `(master, slave)` the caller opened; the
/// slave is consumed. Answers `(master, pid)`.
fn spawn_leader_on(pair: (i32, i32), argv: &[&str]) -> (i32, i32) {
    spawn_leader_inheriting(pair, argv, None)
}

/// [`spawn_leader_on`], with `inherit` — a close-on-exec descriptor of the
/// caller's — made inheritable in the forked child ALONE, so the leader
/// holds it across its exec while no other test's child, forked from this
/// process meanwhile, ever finds it inheritable (`openpty(3)`'s shape,
/// confined to the one child that is its subject).
#[allow(clippy::zombie_processes)]
fn spawn_leader_inheriting(
    (master, slave): (i32, i32),
    argv: &[&str],
    inherit: Option<i32>,
) -> (i32, i32) {
    // SAFETY: `slave` is an open fd the caller handed over; owned here.
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    // std's Command on purpose: the leader needs `pre_exec` (setsid + TIOCSCTTY) and may inherit `inherit`.
    // The strip first: the leader holds its terminal, `inherit` (made inheritable by the hook
    // below, after the strip) and nothing else of this process.
    let mut cmd = std::process::Command::new(argv[0]);
    cmd.inherit_nothing()
        .args(&argv[1..])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("PS1", "$ ")
        .env("TERM", "dumb")
        .stdin(slave.try_clone().expect("dup slave"))
        .stdout(slave.try_clone().expect("dup slave"))
        .stderr(slave);
    // SAFETY: `setsid`, `ioctl` and `fcntl` are async-signal-safe; the
    // closure runs in the forked child after its stdio is the slave.
    unsafe {
        cmd.pre_exec(move || {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if let Some(fd) = inherit {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let child = cmd.spawn().expect("spawn the session leader");
    let pid = i32::try_from(child.id()).expect("pid");
    (master, pid)
}

fn type_line(master: i32, line: &str) {
    // SAFETY: `master` is this test's open pty master; the buffer is live.
    let n = unsafe { libc::write(master, line.as_ptr().cast(), line.len()) };
    assert_eq!(usize::try_from(n).ok(), Some(line.len()), "write(master)");
}

/// Wait for what must happen — a shell reading its line, a child forking —
/// as a hang detector ([`aterm_spec::wait::MUST_HAPPEN`]), never a latency
/// budget: a loaded host only slows it.
#[track_caller]
fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, what, || {
        ready().then_some(())
    });
}

fn children(pid: i32) -> Vec<i32> {
    crate::quit_safety::session_root(pid).map_or_else(Vec::new, |r| r.children)
}

/// A real session `id` over `(master, pid)`, as a new tab of window 0.
fn add_tab(app: &mut App, id: u64, (master, pid): (i32, i32)) {
    let mut session = crate::stub_session(id);
    session.master = master;
    session.pid = pid;
    App::register_session(&app.store, &session, None);
    app.pool.insert(session);
    app.next_session_id = app.next_session_id.max(id + 1);
    let tree = pane::PaneTree::new(id);
    let tab = crate::register_terminal_tab(&mut app.tab_ids, &mut app.view_store, &tree)
        .expect("tab identity");
    let ws = app.windows.get_mut(&WindowId(0)).expect("window 0");
    ws.layouts.push(tree);
    ws.tabs.add();
    ws.tab_set.push(tab).expect("fresh tab id");
    app.resync_active_or_window(WindowId(0));
}

fn name_group(app: &App, id: u64, pgid: i32, program: &str) {
    let session = app.pool.get(id).expect("session");
    let mut timeline = session
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    timeline.note_foreground_group(pgid);
    timeline.set_program(pgid, Some(program.to_string()));
}

/// Kill everything this test started, by `SIGKILL` (AGENTS.md).
fn kill_all(pids: &[i32], masters: &[i32]) {
    for pid in pids {
        // SAFETY: each pid is a process this test started (or its job),
        // still unreaped by anyone but this test's sessions.
        unsafe { libc::kill(*pid, libc::SIGKILL) };
    }
    for master in masters {
        // SAFETY: each is this test's own pty master, closed once.
        unsafe { libc::close(*master) };
    }
}

/// `fd`'s device (`st_rdev`): what [`aterm_uds::holders`] finds a
/// descriptor by in another process's table.
#[cfg(target_os = "macos")]
fn rdev_of(fd: i32) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    // SAFETY: `fd` is open across this call, which only duplicates it.
    let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
    std::fs::File::from(fd.try_clone_to_owned().expect("dup"))
        .metadata()
        .expect("fstat")
        .rdev()
}

/// A leader this test leaves reaped, never stuck in its exit: unless it
/// was reaped already, dropping it kills it (`SIGKILL`) and flushes the
/// output its exit's drain would wait on through this test's own copy of
/// its slave (`tcflush(TCOFLUSH)`, what released the measured
/// stragglers) until it is reaped, for at most a hang detector's minute.
#[cfg(target_os = "macos")]
struct Leader {
    pid: i32,
    slave: OwnedFd,
    reaped: bool,
}

#[cfg(target_os = "macos")]
impl Leader {
    /// Whether the leader exits by itself within `budget`, reaped here.
    fn exits_within(&mut self, budget: Duration) -> bool {
        let pid = self.pid;
        self.reaped = aterm_spec::wait::within(budget, Duration::from_millis(20), || {
            let mut status = 0;
            // SAFETY: a non-blocking reap of this test's own child.
            (unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) } == pid).then_some(())
        })
        .is_some();
        self.reaped
    }
}

#[cfg(target_os = "macos")]
impl Drop for Leader {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd as _;
        if self.reaped {
            return;
        }
        // SAFETY: `pid` is this test's own unreaped child, so its number
        // cannot have been reused.
        unsafe { libc::kill(self.pid, libc::SIGKILL) };
        let (pid, slave) = (self.pid, self.slave.as_raw_fd());
        self.reaped = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            Duration::from_millis(20),
            || {
                let mut status = 0;
                // SAFETY: a flush of this test's own open slave, then a
                // non-blocking reap of its own child.
                unsafe {
                    libc::tcflush(slave, libc::TCOFLUSH);
                    (libc::waitpid(pid, &mut status, libc::WNOHANG) != 0).then_some(())
                }
            },
        )
        .is_some();
    }
}

/// THE LEADER HOLDS ITS TERMINAL, NEVER ITS MASTER (2026-09-30). The
/// helper's pair is close-on-exec from birth, so the `sh -i` spawned onto
/// it holds its slave and no copy of its master — read off the child's
/// own descriptor table, by device — and the test's master is the only
/// one: closing it hangs the terminal up, and the shell, whose prompt
/// nobody read, exits and is reaped within the hang detector's minute.
///
/// NEGATIVE CONTROL: the same spawn with the master inheritable — the
/// shape `openpty(3)` gave, which the helper replaced — leaves the leader
/// holding its own master: the copy that kept the measured stragglers in
/// `?E`. The control's pair is close-on-exec from birth too, its master
/// made inheritable only in the leader's forked child: a raw `openpty`
/// pair, inheritable in the whole process while the control ran, went
/// into every child another test spawned meanwhile, which then held both
/// ends of it (the review of 2026-09-30) — and the control's `SIGKILL`
/// of its session leader revokes that slave, so a table holding it reads
/// `ENOENT` there (measured 2026-10-01) and its scan answers "unknown".
/// That leader is a `sleep` with nothing queued, and it is killed and
/// its slave's output flushed before either end is closed, so the
/// control cannot strand a process of its own.
///
/// ALONE ([`crate::test_alone`]; it failed once in 12 loaded runs of the
/// shared lib-test process). Its subject is process-wide: closing the
/// test's master hangs the terminal up only once no other copy is open,
/// and in the shared process every child a sibling test forks holds a
/// copy until it execs; the leaders' tables are read whole, and inherit
/// whatever a sibling left inheritable at their fork. Measured the same
/// day at a load of ~110 on 18 cores, 450 tries each: the close-to-exit
/// wait was at most 36–40 ms alone and 0.34–1.65 s in the shared
/// process.
#[cfg(target_os = "macos")]
#[test]
fn a_spawned_leader_holds_its_terminal_and_never_its_master() {
    if !crate::test_alone::alone() {
        return;
    }
    use aterm_uds::holders::process_holds_rdev;
    // This test's own close-on-exec copy of a slave, for the release.
    let keep = |slave: i32| {
        // SAFETY: `slave` is open across this call, which only duplicates it.
        unsafe { std::os::fd::BorrowedFd::borrow_raw(slave) }
            .try_clone_to_owned()
            .expect("dup slave")
    };
    let id = |pid: i32| u32::try_from(pid).expect("pid");

    let (master, slave) = open_pty_pair();
    let (master_rdev, slave_rdev) = (rdev_of(master), rdev_of(slave));
    assert_ne!(
        master_rdev, slave_rdev,
        "PRECONDITION: the two ends are two devices"
    );
    let kept = keep(slave);
    let (master, pid) = spawn_leader_on((master, slave), &["/bin/sh", "-i"]);
    let mut leader = Leader {
        pid,
        slave: kept,
        reaped: false,
    };
    assert_eq!(
        process_holds_rdev(id(pid), slave_rdev),
        Some(true),
        "PRECONDITION: the scan reads the leader's table, and the leader is on its terminal"
    );
    assert_eq!(
        process_holds_rdev(id(pid), master_rdev),
        Some(false),
        "the leader holds no copy of its own master"
    );
    aterm_pty::close_fd(master);
    assert!(
        leader.exits_within(aterm_spec::wait::MUST_HAPPEN),
        "with the test's master the only one, closing it hangs the terminal up \
         and the leader exits"
    );
    drop(leader);

    let (control_master, control_slave) = open_pty_pair();
    let control_master_rdev = rdev_of(control_master);
    let kept = keep(control_slave);
    let (control_master, control_pid) = spawn_leader_inheriting(
        (control_master, control_slave),
        &["/bin/sleep", "30"],
        Some(control_master),
    );
    let leaked = process_holds_rdev(id(control_pid), control_master_rdev);
    drop(Leader {
        pid: control_pid,
        slave: kept,
        reaped: false,
    });
    aterm_pty::close_fd(control_master);
    assert_eq!(
        leaked,
        Some(true),
        "NEGATIVE CONTROL: a master the leader can inherit, as openpty(3)'s was, \
         is held by the leader spawned onto it"
    );
}

/// R3 — the capture over REAL sessions, in its window and tab places
/// (the ones `Reopened::lost` counts): tab 0 is a stub with no process
/// (skipped), tab 1 a shell at its prompt (NEGATIVE CONTROL: no entry),
/// tab 2 a foreground `sleep` (named for its group), tab 3 a `sleep &`
/// (R2: unnamed), tab 4 `sleep` as the session root as `aterm -e` runs it
/// (R1: named), tab 5 a shell that `exec`ed `sleep` while the sweep's
/// name still says the shell (R1: unnamed).
#[test]
fn the_journal_names_the_tabs_whose_programs_a_death_would_take() {
    let mut app = App::headless_for_test();
    let idle = spawn_leader(&["/bin/sh", "-i"]);
    let fg = spawn_leader(&["/bin/sh", "-i"]);
    let bg = spawn_leader(&["/bin/sh", "-i"]);
    let exe = spawn_leader(&["/bin/sleep", "30"]);
    let exec = spawn_leader(&["/bin/sh", "-i"]);
    for (id, pair) in [(1, idle), (2, fg), (3, bg), (4, exe), (5, exec)] {
        add_tab(&mut app, id, pair);
    }
    assert!(app.structural_invariants_ok());
    type_line(fg.0, "/bin/sleep 30\n");
    type_line(bg.0, "/bin/sleep 30 &\n");
    type_line(exec.0, "exec /bin/sleep 30\n");
    let mut fg_group = -1;
    wait_for("the foreground sleep", || {
        fg_group = crate::quit_safety::foreground_pgrp(fg.0);
        fg_group > 0 && fg_group != fg.1
    });
    wait_for("the background sleep", || !children(bg.1).is_empty());
    wait_for("the exec", || {
        crate::quit_safety::session_root(exec.1).is_some_and(|r| r.comm == "sleep")
    });
    name_group(&app, 2, fg_group, "sleep");
    name_group(&app, 4, exe.1, "sleep");
    name_group(&app, 5, exec.1, "sh");
    let bg_jobs = children(bg.1);

    // Ruling 293: tab 2's leaf carries the agent its session hosts (as
    // the journal's capture fills it), so its entry is marked as that
    // agent; every other entry is not (the negative control).
    let mut manifest = app.capture_restore_manifest();
    let hosted = restore::AgentRestore {
        pid: u32::try_from(fg_group).unwrap_or(0),
        start: "Sat Sep 27 01:02:03 2026".into(),
        program: "/opt/claude/bin/claude".into(),
        argv: vec!["/opt/claude/bin/claude".into()],
        session: Some("0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c".into()),
        cwd: "/".into(),
        version: None,
        home: None,
        login: None,
        codex: None,
        cause: None,
    };
    manifest.fill_agents(&|id| (id == 2).then(|| hosted.clone()));
    let programs = app.crash_journal_programs(&manifest);
    let entry = |tab: u32, program: &str| crate::crash_journal::LeafProgram {
        window: 0,
        tab,
        program: program.to_string(),
        agent: tab == 2,
        again: false,
    };
    assert_eq!(
        programs,
        vec![
            entry(2, "sleep"),
            entry(3, ""),
            entry(4, "sleep"),
            entry(5, "")
        ],
        "tabs 0 (no process) and 1 (a shell at its prompt) have none"
    );

    // TAB 2'S, 4'S AND 3'S SESSIONS SHOWN AGAIN IN A SECOND WINDOW (`Open
    // Session in New Window`; restore audit, 2026-10-02). The agent comes
    // back once, at its first place, and is lost in neither tab that
    // showed it; `sleep` was running in both of its tabs; the `sleep &`
    // counts in both of its tabs and as one program. RED before: the
    // capture counted each session once, so the second window's `sleep`
    // tab went unsaid — and an image with an entry per leaf (an older
    // writer's) said the relaunched agent was lost in the second; a
    // per-leaf capture without the mark counted the unnamed job twice.
    let mut coview = manifest.windows[0].clone();
    coview.restored_tabs = vec![
        coview.restored_tabs[2].clone(),
        coview.restored_tabs[4].clone(),
        coview.restored_tabs[3].clone(),
    ];
    coview.tabs = Vec::new();
    manifest.windows.push(coview);
    let programs = app.crash_journal_programs(&manifest);
    let again: Vec<(u32, u32)> = programs
        .iter()
        .filter(|entry| entry.again)
        .map(|entry| (entry.window, entry.tab))
        .collect();
    assert_eq!(
        again,
        vec![(1, 0), (1, 1), (1, 2)],
        "each session's first leaf is captured, its later ones marked"
    );
    let reopened = crate::crash_journal::Reopened {
        manifest,
        class: crate::crash_journal::DeathClass::Killed,
        sources: Vec::new(),
        writers: Vec::new(),
        handed: Vec::new(),
        programs: Some(programs),
        second_chance: false,
    };
    assert_eq!(reopened.resumed(true), vec![(0, 2)], "relaunched once");
    assert_eq!(
        reopened.lost_given(true),
        Some(crate::crash_journal::Lost {
            tabs: 5,
            named: vec!["sleep".to_string()],
            unnamed: 2,
        }),
        "tabs 3, 4 and 5, and the second window's `sleep` and `sleep &`"
    );

    let mut pids = vec![fg_group, idle.1, fg.1, bg.1, exe.1, exec.1];
    pids.extend(bg_jobs);
    kill_all(&pids, &[idle.0, fg.0, bg.0, exe.0, exec.0]);
}

/// AUDIT #8 — A PANE A PROGRAM HELD DOES NOT COME BACK SAYING ANOTHER
/// MACHINE'S FOLDER IS MISSING. Two real sessions report a folder that is
/// not here, under another host's name, as ssh's remote shell does: tab 2
/// with a foreground `sleep` standing in for ssh, tab 1 a shell at its
/// prompt (the NEGATIVE CONTROL). The durable capture marks tab 2's leaf
/// held and not tab 1's; the seamless update's layout marks neither, and
/// nor does the durable capture once tab 2's program is the agent aterm
/// hosts there. Through the wire, the held leaf's restore does not ask
/// for that folder and the reopened row counts it apart, while the
/// shell's pane still asks (its spawn falls back to the home folder and
/// says so) and is counted as gone. A held pane whose folder IS here
/// keeps it.
#[test]
fn a_held_panes_folder_that_is_not_here_is_not_asked_for_or_counted() {
    let mut app = App::headless_for_test();
    let idle = spawn_leader(&["/bin/sh", "-i"]);
    let held = spawn_leader(&["/bin/sh", "-i"]);
    add_tab(&mut app, 1, idle);
    add_tab(&mut app, 2, held);
    wait_for("the idle shell's own group", || {
        crate::quit_safety::foreground_pgrp(idle.0) == idle.1
    });
    type_line(held.0, "/bin/sleep 30\n");
    let mut group = -1;
    wait_for("the foreground sleep", || {
        group = crate::quit_safety::foreground_pgrp(held.0);
        group > 0 && group != held.1
    });
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-held-folder")
        .tempdir()
        .expect("scratch dir");
    let here = scratch
        .path()
        .to_str()
        .expect("utf-8 scratch path")
        .to_owned();
    let remote = format!("{here}/on-the-remote-host/proj");
    let report = |app: &App, id: u64, dir: &str| {
        crate::term_lock(&app.pool.get(id).expect("live session").term)
            .process(format!("\x1b]7;file://build-box{dir}\x07").as_bytes());
    };
    report(&app, 1, &remote);
    report(&app, 2, &remote);
    let leaf = |manifest: &restore::RestoreManifest, id: u64| {
        manifest.windows[0]
            .restored_tabs
            .iter()
            .find_map(|tab| match &tab.root {
                restore::RestoredSplitTree::Leaf {
                    view: restore::RestoredView::Terminal(leaf),
                } if leaf.local_id == Some(id) => Some(leaf.clone()),
                _ => None,
            })
            .expect("the tab's terminal leaf")
    };

    let durable = app.capture_restore_manifest();
    assert!(
        !leaf(&durable, 1).held,
        "NEGATIVE CONTROL: a shell at its prompt"
    );
    assert!(leaf(&durable, 2).held, "a program holds the pane");
    assert_eq!(leaf(&durable, 2).cwd.as_deref(), Some(remote.as_str()));
    let handoff = app.capture_handoff_layout();
    assert!(
        !leaf(&handoff, 1).held && !leaf(&handoff, 2).held,
        "the update's layout records no program"
    );
    // The agent aterm hosts in tab 2 runs on this machine: the pane is not
    // held, so its folder is asked for (and said when it is not found),
    // in a person's own quit's layout too, which carries no agent.
    let sid = {
        let store = app.store.read().unwrap_or_else(|p| p.into_inner());
        store
            .by_local(2)
            .expect("registered")
            .sid
            .as_str()
            .to_string()
    };
    let agent = aterm_agent::harness::relaunch::Snapshot {
        tab: sid.clone(),
        pid: 4242,
        start: "Sat Sep 27 01:02:03 2026".into(),
        shell: 4241,
        program: "/opt/claude/bin/claude".into(),
        argv: vec!["/opt/claude/bin/claude".into()],
        session: None,
        cwd: remote.clone(),
        version: None,
        codex: None,
        dialect: None,
        login: None,
        home: None,
        claude_dir: None,
    };
    crate::harness_host::publish_snapshot_for_test(&sid, Some(&agent));
    let hosted = app.capture_restore_manifest();
    crate::harness_host::publish_snapshot_for_test(&sid, None);
    assert!(
        !leaf(&hosted, 2).held && leaf(&hosted, 2).agent.is_none(),
        "the hosted agent's pane, in a layout that carries no agent"
    );
    assert_eq!(
        leaf(&hosted, 2).start_cwd().as_deref(),
        Some(remote.as_str()),
        "the hosted agent's folder is asked for"
    );

    let carried = restore::RestoreManifest::from_toml(&durable.to_toml().expect("serializes"))
        .expect("parses");
    assert!(leaf(&carried, 2).held, "held survives the wire");
    assert_eq!(
        leaf(&carried, 2).start_cwd(),
        None,
        "the held pane's folder is not asked for"
    );
    assert_eq!(
        leaf(&carried, 1).start_cwd().as_deref(),
        Some(remote.as_str()),
        "the shell's own folder is asked for"
    );
    let reopened = crate::crash_journal::Reopened {
        manifest: carried,
        class: crate::crash_journal::DeathClass::Killed,
        sources: vec![crate::crash_journal::JournalId { pid: 9, nanos: 9 }],
        writers: vec![9],
        handed: Vec::new(),
        programs: None,
        second_chance: false,
    };
    assert_eq!(
        reopened.panes_without_folder(|dir| crate::spawn_folder::folder_fault(dir).is_some()),
        crate::crash_journal::PanesWithoutFolder {
            gone: 1,
            unasked: 1
        },
        "the shell's pane is counted, the held one apart"
    );

    report(&app, 2, &here);
    let durable = app.capture_restore_manifest();
    assert_eq!(
        leaf(&durable, 2).start_cwd().as_deref(),
        Some(here.as_str()),
        "a held pane's folder that is here is kept"
    );
    kill_all(&[group, idle.1, held.1], &[idle.0, held.0]);
}

/// R3 — a program starting in a tab marks the journal's lane dirty even
/// when nothing else would wake a capture (the status sweep's foreground
/// group change). NEGATIVE CONTROL: a sweep that sees the same group
/// leaves the lane clean.
#[test]
fn a_foreground_change_owes_the_journal_a_capture() {
    let mut app = App::headless_for_test();
    assert!(app.config.tab_status_or_default(), "the sweep must run");
    let shell = spawn_leader(&["/bin/sh", "-i"]);
    add_tab(&mut app, 1, shell);
    wait_for("the shell's own group", || {
        crate::quit_safety::foreground_pgrp(shell.0) == shell.1
    });
    let t0 = Instant::now();
    let _ = app.observe_session_statuses(t0);
    let _ = app.crash_journal.take_dirty();
    let _ = app.observe_session_statuses(t0 + Duration::from_secs(2));
    assert!(
        !app.crash_journal.take_dirty(),
        "NEGATIVE CONTROL: the same foreground owes nothing"
    );
    type_line(shell.0, "/bin/sleep 30\n");
    let mut group = -1;
    wait_for("the foreground sleep", || {
        group = crate::quit_safety::foreground_pgrp(shell.0);
        group > 0 && group != shell.1
    });
    let _ = app.observe_session_statuses(t0 + Duration::from_secs(4));
    assert!(
        app.crash_journal.take_dirty(),
        "a program started in the tab: the journal owes a capture"
    );
    kill_all(&[group, shell.1], &[shell.0]);
}
