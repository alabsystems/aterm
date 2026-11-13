// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::atomic::AtomicBool;
use std::sync::mpsc;

use super::attach_reader_inner;
use crate::{App, Wake, WindowId};

#[test]
fn the_tab_ends_when_its_shell_exits_while_a_job_holds_the_terminal() {
    let app = App::headless_for_test();
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    let mut shell = aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", "read _"])
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn the shell");
    let mut session = crate::stub_session(91);
    session.master = master;
    session.pid = i32::try_from(shell.id()).expect("pid fits i32");
    session.shell_identity = aterm_pty::record_spawned_shell(session.pid);
    let (posted, wakes) = mpsc::channel::<&'static str>();
    let post = std::sync::Arc::new(move |wake: Wake| {
        let _ = posted.send(match wake {
            Wake::Ready { .. } => "ready",
            Wake::Exit { .. } => "exit",
            _ => "other",
        });
        true
    });
    attach_reader_inner(&mut session, WindowId(0), post, &app.session_factory, None)
        .expect("attach the real reader");
    let patience = aterm_spec::wait::MUST_HAPPEN;
    let next = |what: &str| loop {
        match wakes.recv_timeout(patience) {
            Ok(w) if w == what => return true,
            Ok(_) => continue,
            Err(_) => return false,
        }
    };
    assert!(next("ready"), "the reader's gather started");
    // SAFETY: a write of a live buffer to the test-owned slave.
    let wrote = unsafe { libc::write(slave, b"bye".as_ptr().cast(), 3) };
    assert_eq!(wrote, 3, "write the shell's last words");
    drop(shell.stdin.take());
    let exited = next("exit");
    let _ = shell.wait();
    let row = |session: &crate::Session| {
        crate::term_lock(&session.term)
            .row_text(0)
            .unwrap_or_default()
    };
    let shown = row(&session);
    // SAFETY: a write of a live buffer to the test-owned slave.
    let wrote = unsafe { libc::write(slave, b"late".as_ptr().cast(), 4) };
    assert_eq!(wrote, 4, "write the job's later output");
    let mut late_shown = row(&session).starts_with("byelate");
    let until = std::time::Instant::now() + patience;
    while exited && !late_shown && std::time::Instant::now() < until {
        aterm_uds::deadline::poll_yield();
        late_shown = row(&session).starts_with("byelate");
    }
    aterm_pty::close_fd(slave);
    let reader_ended = session.reader_join.as_ref().is_some_and(|reader| {
        let until = std::time::Instant::now() + patience;
        while !reader.is_finished() && std::time::Instant::now() < until {
            aterm_uds::deadline::poll_yield();
        }
        reader.is_finished()
    });
    let _ = super::park_reader(
        &mut session,
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    );
    let exits_after = wakes.try_iter().filter(|wake| *wake == "exit").count();
    let master = std::mem::replace(&mut session.master, -1);
    aterm_pty::close_fd(master);
    assert!(
        exited,
        "the shell exited while a job held its terminal, and the tab stayed open"
    );
    assert!(
        shown.starts_with("bye"),
        "what the shell wrote before its exit reaches the screen: {shown:?}"
    );
    assert!(
        late_shown,
        "what the job wrote after the exit reaches the screen: {:?}",
        row(&session)
    );
    assert!(reader_ended, "the job let go, and the reader read on");
    assert_eq!(exits_after, 0, "the job's letting go is not a second exit");
}

fn exit_rig(
    app: &App,
    id: u64,
) -> (
    crate::Session,
    i32,
    aterm_uds::spawnfd::Child,
    mpsc::Receiver<bool>,
) {
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    let shell = sh("read _; exit 3");
    let pid = shell.id();
    let mut session = crate::stub_session(id);
    session.master = master;
    session.pid = i32::try_from(pid).expect("pid fits i32");
    session.shell_identity = aterm_pty::record_spawned_shell(session.pid);
    let (ready_tx, ready) = mpsc::channel::<()>();
    let (exit_tx, exits) = mpsc::channel::<bool>();
    let post = std::sync::Arc::new(move |wake: Wake| {
        match wake {
            Wake::Ready { .. } => {
                let _ = ready_tx.send(());
            }
            Wake::Exit { .. } => {
                let _ = exit_tx.send(collectable(pid));
            }
            _ => {}
        }
        true
    });
    attach_reader_inner(&mut session, WindowId(0), post, &app.session_factory, None)
        .expect("attach the real reader");
    ready
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the reader's gather started");
    (session, slave, shell, exits)
}

fn end_rig(mut session: crate::Session) {
    let _ = super::park_reader(
        &mut session,
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    );
    let master = std::mem::replace(&mut session.master, -1);
    aterm_pty::close_fd(master);
}

#[test]
fn the_gather_posts_the_exit_at_its_terminals_eof_only_once_the_shell_is_retired() {
    let app = App::headless_for_test();
    let (session, slave, mut shell, exits) = exit_rig(&app, 95);
    assert!(
        !collectable(shell.id()),
        "a running shell is not collectable"
    );
    aterm_pty::close_fd(slave);
    let stdin = shell.stdin.take().expect("its input");
    let closer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        drop(stdin);
    });
    let retired = exits.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    closer.join().expect("the closer");
    let code = shell.wait().expect("its owner reaps it").code();
    end_rig(session);
    assert_eq!(
        retired,
        Ok(true),
        "the exit was posted before the shell could be collected"
    );
    assert_eq!(code, Some(3), "and the status was left for its owner");
}

#[test]
fn a_cancelled_retirement_wait_wakes_the_reader_without_posting_a_shell_exit() {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    use super::{GatherMsg, ShellExit, ShellRetirement};

    let pipe = || {
        let (rd, wr) = aterm_pty::make_wake_pipe().expect("owned wake pipe");
        // SAFETY: both fresh descriptors are transferred to one owner each.
        unsafe { (OwnedFd::from_raw_fd(rd), OwnedFd::from_raw_fd(wr)) }
    };
    let (wake_rd, wake_wr) = pipe();
    aterm_pty::wake(wake_wr.as_raw_fd());
    for (watched, watch_ready) in [(false, false), (true, false), (true, true)] {
        let (watch_rd, watch_wr) = pipe();
        if watch_ready {
            aterm_pty::wake(watch_wr.as_raw_fd());
        }
        let exit = ShellExit {
            watch: watched.then_some(watch_rd),
            pid: std::process::id(),
        };
        let result = exit.await_retired_at_exit(wake_rd.as_raw_fd(), &NO_STOP);
        assert_eq!(result, ShellRetirement::Cancelled);
        assert!(!NO_STOP.load(std::sync::atomic::Ordering::Acquire));
        for ending in [GatherMsg::Exited, GatherMsg::Eof] {
            assert!(matches!(result.gather_message(ending), GatherMsg::Wake));
        }
    }
    for watched in [false, true] {
        let (watch_rd, _watch_wr) = pipe();
        let exit = ShellExit {
            watch: watched.then_some(watch_rd),
            pid: std::process::id(),
        };
        assert_eq!(
            exit.await_retired_at_exit(-1, &STOPPED),
            ShellRetirement::Cancelled,
            "the stop flag also cancels without a wake pipe"
        );
        assert_eq!(
            exit.await_retired(std::time::Duration::ZERO, -1, &NO_STOP),
            ShellRetirement::Unconfirmed,
            "timeout or an unwaitable pid retains the existing exit policy"
        );
    }
    for result in [ShellRetirement::Retired, ShellRetirement::Unconfirmed] {
        assert!(matches!(
            result.gather_message(GatherMsg::Exited),
            GatherMsg::Exited
        ));
        assert!(matches!(
            result.gather_message(GatherMsg::Eof),
            GatherMsg::Eof
        ));
    }
}

#[test]
fn waiting_for_the_shells_retirement_reaps_nothing() {
    let mut shell = aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", "read _; exit 3"])
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn the shell");
    let pid = shell.id();
    let exit = super::ShellExit {
        watch: Some(aterm_uds::exitwatch::exit_fd(pid).expect("the shell is watched")),
        pid,
    };
    exit.await_retired(std::time::Duration::from_millis(1), -1, &NO_STOP);
    assert!(
        shell.try_wait().expect("a look at the shell").is_none(),
        "a shell still running is left running"
    );
    drop(shell.stdin.take());
    exit.await_retired(aterm_spec::wait::MUST_HAPPEN, -1, &NO_STOP);
    // SAFETY: a zeroed `siginfo_t` out-param for a non-reaping peek.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: `WNOWAIT` leaves the child waitable; `WNOHANG` never blocks.
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            pid,
            &mut info,
            libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
        )
    };
    assert_eq!(rc, 0, "waitid on our own child");
    // SAFETY: `waitid` filled the siginfo (or left it zeroed).
    let collectable = unsafe { info.si_pid() };
    assert_eq!(
        u32::try_from(collectable).ok(),
        Some(pid),
        "collectable the moment the wait ends"
    );
    assert_eq!(
        shell.wait().expect("its owner reaps it").code(),
        Some(3),
        "and the status was left for its owner"
    );
}

fn collectable(pid: u32) -> bool {
    // SAFETY: a zeroed `siginfo_t` out-param for a non-reaping peek.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: `WNOWAIT` leaves the child waitable; `WNOHANG` never blocks.
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            pid,
            &mut info,
            libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
        )
    };
    // SAFETY: `waitid` filled the siginfo (or left it zeroed).
    rc == 0 && u32::try_from(unsafe { info.si_pid() }).ok() == Some(pid)
}

#[test]
fn the_wait_for_a_shells_retirement_outlasts_a_slow_exit_and_a_stop_ends_it() {
    use std::os::fd::IntoRawFd;
    let mut shell = aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", "read _; exit 3"])
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn the shell");
    let pid = shell.id();
    let exit = super::ShellExit {
        watch: Some(aterm_uds::exitwatch::exit_fd(pid).expect("the shell is watched")),
        pid,
    };
    let wake = aterm_uds::pipe()
        .map(|(r, w)| [r.into_raw_fd(), w.into_raw_fd()])
        .expect("the wake pipe");
    // SAFETY: one byte from a live buffer into the pipe's write end.
    assert_eq!(unsafe { libc::write(wake[1], [1u8].as_ptr().cast(), 1) }, 1);
    let (ended_tx, ended) = std::sync::mpsc::channel();
    let wake_rd = wake[0];
    let waiter = std::thread::spawn(move || {
        exit.await_retired(std::time::Duration::from_secs(600), wake_rd, &NO_STOP);
        let _ = ended_tx.send(());
        exit
    });
    ended
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the stop's wake ended the wait");
    let exit = waiter.join().expect("the waiter");
    aterm_pty::close_fd(wake[0]);
    aterm_pty::close_fd(wake[1]);
    assert!(
        shell.try_wait().expect("a look at the shell").is_none(),
        "a stop leaves the shell running"
    );
    let stdin = shell.stdin.take().expect("its input");
    let closer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        drop(stdin);
    });
    exit.await_retired_at_exit(-1, &NO_STOP);
    let retired = collectable(pid);
    closer.join().expect("the closer");
    assert!(retired, "the wait outlasted the shell's slow exit");
    assert_eq!(
        shell.wait().expect("its owner reaps it").code(),
        Some(3),
        "and the status was left for its owner"
    );
}

fn sh(script: &str) -> aterm_uds::spawnfd::Child {
    aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", script])
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn the shell")
}

static NO_STOP: AtomicBool = AtomicBool::new(false);
static STOPPED: AtomicBool = AtomicBool::new(true);

fn ended_at_once(
    exit: super::ShellExit,
    within: std::time::Duration,
    wake_rd: i32,
    stop: &'static AtomicBool,
) -> bool {
    let (ended_tx, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        exit.await_retired(within, wake_rd, stop);
        let _ = ended_tx.send(());
    });
    ended.recv_timeout(aterm_spec::wait::MUST_HAPPEN).is_ok()
}

/// A shell of this process's own already exiting when its reader attaches keeps a watchless
/// wait ([`super::shell_exit_of`]); an adopted one has none. Darwin's alone: Linux's pidfd
/// watches a zombie.
#[cfg(target_vendor = "apple")]
#[test]
fn a_shell_already_exiting_when_its_reader_attaches_keeps_its_wait() {
    let mut gone = sh("exit 3");
    let pid = gone.id();
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while !collectable(pid) {
        assert!(std::time::Instant::now() < deadline, "the shell exited");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(
        aterm_uds::exitwatch::exit_fd(pid).is_none(),
        "the kernel watches no process that has exited"
    );
    let own = i32::try_from(pid).expect("a pid");
    let kept = super::shell_exit_of(aterm_pty::ShellIdentity::from_parts(own, None, true))
        .expect("a shell of this process's own keeps its wait");
    assert!(kept.watch.is_none(), "with no watch");
    assert!(
        super::shell_exit_of(aterm_pty::ShellIdentity::from_parts(own, None, false)).is_none(),
        "an adopted shell no watch can be made for has no wait"
    );
    assert!(
        ended_at_once(kept, super::SHELL_RETIRE_HANG, -1, &NO_STOP),
        "a shell already collectable is not waited for"
    );
    assert_eq!(gone.wait().expect("its owner reaps it").code(), Some(3));
}

#[test]
fn a_shell_that_could_not_be_watched_is_still_waited_for() {
    use std::os::fd::IntoRawFd;
    let mut shell = sh("read _; exit 3");
    let pid = shell.id();
    let unwatched = || super::ShellExit { watch: None, pid };
    let wake = aterm_uds::pipe()
        .map(|(r, w)| [r.into_raw_fd(), w.into_raw_fd()])
        .expect("the wake pipe");
    // SAFETY: one byte from a live buffer into the pipe's write end.
    assert_eq!(unsafe { libc::write(wake[1], [1u8].as_ptr().cast(), 1) }, 1);
    assert!(
        ended_at_once(
            unwatched(),
            std::time::Duration::from_secs(600),
            wake[0],
            &NO_STOP
        ),
        "the stop's wake ended the wait"
    );
    aterm_pty::close_fd(wake[0]);
    aterm_pty::close_fd(wake[1]);
    let parent = super::ShellExit {
        watch: None,
        pid: std::os::unix::process::parent_id(),
    };
    assert!(
        ended_at_once(parent, std::time::Duration::from_secs(600), -1, &NO_STOP),
        "a pid this process cannot wait for is not waited for"
    );
    assert!(
        shell.try_wait().expect("a look at the shell").is_none(),
        "the shell is still running"
    );
    let stdin = shell.stdin.take().expect("its input");
    let closer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        drop(stdin);
    });
    unwatched().await_retired_at_exit(-1, &NO_STOP);
    let retired = collectable(pid);
    closer.join().expect("the closer");
    assert!(
        retired,
        "the wait outlasted the unwatched shell's slow exit"
    );
    assert_eq!(
        shell.wait().expect("its owner reaps it").code(),
        Some(3),
        "and the status was left for its owner"
    );
}

#[test]
fn a_stop_with_no_wake_pipe_ends_the_wait_for_a_shells_retirement() {
    let mut shell = sh("read _; exit 3");
    let pid = shell.id();
    let watched = || super::ShellExit {
        watch: Some(aterm_uds::exitwatch::exit_fd(pid).expect("the shell is watched")),
        pid,
    };
    let unwatched = || super::ShellExit { watch: None, pid };
    let far = std::time::Duration::from_secs(600);
    assert!(
        ended_at_once(watched(), far, -1, &STOPPED),
        "a raised stop ended the watched wait"
    );
    assert!(
        ended_at_once(unwatched(), far, -1, &STOPPED),
        "a raised stop ended the unwatched wait"
    );
    for exit in [watched(), unwatched()] {
        let bound = std::time::Duration::from_millis(300);
        let began = std::time::Instant::now();
        exit.await_retired(bound, -1, &NO_STOP);
        assert!(
            began.elapsed() >= bound,
            "with the stop down the wait lasts its bound (watched: {})",
            exit.watch.is_some()
        );
    }
    assert!(
        shell.try_wait().expect("a look at the shell").is_none(),
        "the shell is still running"
    );
    drop(shell.stdin.take());
    assert_eq!(shell.wait().expect("its owner reaps it").code(), Some(3));
}
