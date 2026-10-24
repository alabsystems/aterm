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

#[test]
fn a_leaf_ran_a_program_when_its_job_its_root_or_a_child_says_so() {
    let sh = root("zsh", &[]);
    assert_eq!(leaf_program(100, 100, &sh, Some((100, "zsh"))), None);
    assert_eq!(leaf_program(100, 100, &root("-zsh", &[]), None), None);
    assert_eq!(
        leaf_program(200, 100, &sh, Some((200, "vim"))).as_deref(),
        Some("vim")
    );
    assert_eq!(
        leaf_program(200, 100, &sh, Some((100, "zsh"))).as_deref(),
        Some("")
    );
    assert_eq!(
        leaf_program(100, 100, &root("2.1.281", &[]), Some((100, "claude"))).as_deref(),
        Some("claude")
    );
    assert_eq!(
        leaf_program(100, 100, &root("vim", &[]), Some((100, "zsh"))).as_deref(),
        Some(""),
        "a name resolved before the exec still says the shell: unnamed"
    );
    assert_eq!(
        leaf_program(100, 100, &root("bash", &[4242]), None).as_deref(),
        Some("")
    );
    assert_eq!(
        leaf_program(100, 100, &RootAnswer::Read(None), None).as_deref(),
        Some("")
    );
    assert_eq!(leaf_program(100, 100, &RootAnswer::Unasked, None), None);
}

fn open_pty_pair() -> (i32, i32) {
    aterm_pty::open_pty_pair().expect("open_pty_pair")
}

/// Spawn `argv` as the session leader of a fresh PTY with a hermetic `HOME` and `HISTFILE=/dev/null`.
#[allow(clippy::zombie_processes)]
fn spawn_leader(argv: &[&str], home: &std::path::Path) -> (i32, i32) {
    let (master, slave) = open_pty_pair();
    // SAFETY: `slave` is an open fd the pair handed over; owned here.
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    let mut cmd = std::process::Command::new(argv[0]);
    cmd.inherit_nothing()
        .args(&argv[1..])
        .env_clear()
        .envs(aterm_spec::host::hermetic_env(home))
        .env("HISTFILE", "/dev/null")
        .env("PATH", "/usr/bin:/bin")
        .env("PS1", "$ ")
        .env("TERM", "dumb")
        .stdin(slave.try_clone().expect("dup slave"))
        .stdout(slave.try_clone().expect("dup slave"))
        .stderr(slave);
    // SAFETY: `setsid` and `ioctl` are async-signal-safe; the closure runs in the forked child after its stdio is the slave.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn().expect("spawn the session leader");
    let pid = i32::try_from(child.id()).expect("pid");
    (master, pid)
}

/// Read from `master` until `want` appears.
#[track_caller]
fn read_until(master: i32, want: &str) -> String {
    let mut seen = Vec::new();
    wait_for(want, || {
        let mut ready = libc::pollfd {
            fd: master,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one live `pollfd` this frame owns, for a bounded wait.
        if unsafe { libc::poll(&raw mut ready, 1, 50) } > 0 {
            let mut buf = [0u8; 4096];
            // SAFETY: `master` is this test's open pty master; the buffer is live and its length is the one passed.
            let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
            if let Ok(n) = usize::try_from(n) {
                seen.extend_from_slice(&buf[..n]);
            }
        }
        String::from_utf8_lossy(&seen).contains(want)
    });
    String::from_utf8_lossy(&seen).into_owned()
}

#[test]
fn a_leader_shell_has_a_scratch_home_and_saves_no_history() {
    let home = aterm_tempfile::tempdir().expect("scratch home");
    let (master, pid) = spawn_leader(&["/bin/sh", "-i"], home.path());
    struct Leader(i32, i32);
    impl Drop for Leader {
        fn drop(&mut self) {
            kill_all(&[self.1], &[self.0]);
        }
    }
    let _leader = Leader(master, pid);
    type_line(master, "printf '<%s|%s>\\n' \"$HOME\" \"$HISTFILE\"\n");
    let want = format!("<{}|/dev/null>", home.path().display());
    let seen = read_until(master, &want);
    assert!(seen.contains(&want), "{seen:?}");
}

fn type_line(master: i32, line: &str) {
    // SAFETY: `master` is this test's open pty master; the buffer is live.
    let n = unsafe { libc::write(master, line.as_ptr().cast(), line.len()) };
    assert_eq!(usize::try_from(n).ok(), Some(line.len()), "write(master)");
}

#[track_caller]
fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, what, || {
        ready().then_some(())
    });
}

fn children(pid: i32) -> Vec<i32> {
    crate::quit_safety::session_root(pid).map_or_else(Vec::new, |r| r.children)
}

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

fn kill_all(pids: &[i32], masters: &[i32]) {
    for pid in pids {
        // SAFETY: each pid is a process this test started (or its job), still unreaped by anyone but this test's sessions.
        unsafe { libc::kill(*pid, libc::SIGKILL) };
    }
    for master in masters {
        // SAFETY: each is this test's own pty master, closed once.
        unsafe { libc::close(*master) };
    }
}

#[test]
fn the_journal_names_the_tabs_whose_programs_a_death_would_take() {
    let home = aterm_tempfile::tempdir().expect("scratch home");
    let mut app = App::headless_for_test();
    let idle = spawn_leader(&["/bin/sh", "-i"], home.path());
    let fg = spawn_leader(&["/bin/sh", "-i"], home.path());
    let bg = spawn_leader(&["/bin/sh", "-i"], home.path());
    let exe = spawn_leader(&["/bin/sleep", "30"], home.path());
    let exec = spawn_leader(&["/bin/sh", "-i"], home.path());
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

    let mut manifest = app.capture_restore_manifest();
    let hosted = restore::AgentRestore {
        word: String::new(),
        pid: u32::try_from(fg_group).unwrap_or(0),
        start: "Sat Sep 27 01:02:03 2026".into(),
        program: "/opt/claude/bin/claude".into(),
        argv: vec!["/opt/claude/bin/claude".into()],
        session: Some("0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c".into()),
        cwd: "/".into(),
        version: None,
        home: None,
        login: None,
        run: None,
        cause: None,
        tab: None,
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

#[test]
fn a_held_panes_folder_that_is_not_here_is_not_asked_for_or_counted() {
    let home = aterm_tempfile::tempdir().expect("scratch home");
    let mut app = App::headless_for_test();
    let idle = spawn_leader(&["/bin/sh", "-i"], home.path());
    let held = spawn_leader(&["/bin/sh", "-i"], home.path());
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
        stored_login: false,
        profile: None,
        tab: sid.clone(),
        pid: 4242,
        start: "Sat Sep 27 01:02:03 2026".into(),
        shell: 4241,
        program: "/opt/claude/bin/claude".into(),
        argv: vec!["/opt/claude/bin/claude".into()],
        session: None,
        cwd: remote.clone(),
        version: None,
        run: aterm_agent::harness::relaunch::AgentRun::Claude,
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

#[test]
fn a_foreground_change_owes_the_journal_a_capture() {
    let mut app = App::headless_for_test();
    assert!(app.config.tab_status_or_default(), "the sweep must run");
    let home = aterm_tempfile::tempdir().expect("scratch home");
    let shell = spawn_leader(&["/bin/sh", "-i"], home.path());
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
