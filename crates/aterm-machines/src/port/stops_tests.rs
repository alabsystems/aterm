// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Signal dispositions and [`CAUGHT`] belong to the process. Run each real-handler case
/// in its own exact-filter child so its stop cannot cancel another test's live capture.
fn isolated(case: &str) -> bool {
    let args: Vec<_> = std::env::args_os().collect();
    if std::env::var_os("ATERM_STOP_CASE").is_some_and(|value| value == case)
        && args
            .windows(2)
            .any(|pair| pair[0] == "--exact" && pair[1] == case)
    {
        return false;
    }
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().expect("test executable"));
    child
        .args(["--exact", case, "--nocapture"])
        .env("ATERM_STOP_CASE", case);
    let output = aterm_uds::deadline::output_within(&mut child, std::time::Duration::from_secs(60))
        .expect("the owned signal-test child must finish");
    assert!(
        output.status.success(),
        "{case}: {}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    true
}

/// A signal's disposition set to `to` until this drops, then put back.
struct SetTo(c_int, libc::sigaction);

impl SetTo {
    fn new(sig: c_int, to: libc::sighandler_t) -> Self {
        // SAFETY: as in `catch`.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = to;
        // SAFETY: as in `catch`.
        let mut was: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: a valid disposition for a catchable signal; `was` receives the one it
        // replaces.
        assert_eq!(unsafe { libc::sigaction(sig, &action, &mut was) }, 0);
        Self(sig, was)
    }
}

impl Drop for SetTo {
    fn drop(&mut self) {
        // SAFETY: puts back the whole action `new` replaced.
        unsafe { libc::sigaction(self.0, &self.1, std::ptr::null_mut()) };
    }
}

fn hang_up() {
    #[expect(
        clippy::disallowed_methods,
        reason = "a test sends the hang-up `catch` notes, never a core-dump signal"
    )]
    // SAFETY: `raise` takes an integer and touches no memory of ours.
    let raised = unsafe { libc::raise(libc::SIGHUP) };
    assert_eq!(raised, 0);
}

#[test]
fn a_stop_is_noted_while_caught_and_the_old_dispositions_come_back_after() {
    if isolated(
        "port::stops::tests::a_stop_is_noted_while_caught_and_the_old_dispositions_come_back_after",
    ) {
        return;
    }
    // Whatever this test binary inherited (a background job's shell ignores SIGINT and
    // SIGQUIT), every stop here is one nobody ignores.
    let _default = STOPS.map(|sig| SetTo::new(sig, libc::SIG_DFL));
    let before = STOPS.map(disposition);
    let held = catch();
    // A Ctrl-C, a Ctrl-\, a kill and a closed tab are each caught. Only the hang-up is
    // raised: SIGQUIT's default dumps core, and the catch is what this reads.
    for sig in [libc::SIGINT, libc::SIGQUIT, libc::SIGTERM, libc::SIGHUP] {
        let now = disposition(sig);
        assert!(
            now != libc::SIG_DFL && now != libc::SIG_IGN,
            "{} is caught",
            name(sig)
        );
    }
    assert_eq!(caught(), None);
    hang_up();
    assert_eq!(caught(), Some(libc::SIGHUP));
    hang_up();
    assert_eq!(caught(), Some(libc::SIGHUP));
    drop(held);
    assert_eq!(STOPS.map(disposition), before);
    assert_eq!(caught(), None);
}

/// A hang-up this process ignores (`nohup`) is no stop: it stays ignored while the stops are
/// caught, is never noted, and is still ignored after.
#[test]
fn an_ignored_hang_up_stays_ignored_and_is_no_stop() {
    if isolated("port::stops::tests::an_ignored_hang_up_stays_ignored_and_is_no_stop") {
        return;
    }
    let _ignored = SetTo::new(libc::SIGHUP, libc::SIG_IGN);
    let held = catch();
    assert_eq!(disposition(libc::SIGHUP), libc::SIG_IGN);
    hang_up();
    assert_eq!(caught(), None);
    drop(held);
    assert_eq!(disposition(libc::SIGHUP), libc::SIG_IGN);

    // Becoming a detached watch is permanent even if another step already owns a catch.
    let _pipe = SetTo::new(libc::SIGPIPE, disposition(libc::SIGPIPE));
    let _own = SetTo::new(
        libc::SIGHUP,
        heard as extern "C" fn(c_int) as libc::sighandler_t,
    );
    let held = catch();
    let later = catch();
    ignore_hangups();
    drop(held);
    drop(later);
    let after = disposition(libc::SIGHUP);
    assert_eq!(after, libc::SIG_IGN, "a later drop cannot undo detachment");
}

static HEARD: AtomicI32 = AtomicI32::new(0);

extern "C" fn heard(sig: c_int) {
    HEARD.store(sig, Ordering::SeqCst);
}

/// A deferred stop reaches the process's own disposition once the handle drops, after every
/// disposition is back; a plain catch forgets it.
#[test]
fn a_deferred_stop_is_sent_again_once_the_handle_drops() {
    if isolated("port::stops::tests::a_deferred_stop_is_sent_again_once_the_handle_drops") {
        return;
    }
    let _own = SetTo::new(
        libc::SIGHUP,
        heard as extern "C" fn(c_int) as libc::sighandler_t,
    );
    HEARD.store(0, Ordering::SeqCst);
    let held = defer();
    hang_up();
    assert_eq!(caught(), Some(libc::SIGHUP));
    assert_eq!(HEARD.load(Ordering::SeqCst), 0, "held while caught");
    drop(held);
    assert_eq!(
        HEARD.load(Ordering::SeqCst),
        libc::SIGHUP,
        "sent again to the process's own handler"
    );
    assert_eq!(caught(), None);

    HEARD.store(0, Ordering::SeqCst);
    let held = catch();
    hang_up();
    drop(held);
    assert_eq!(
        HEARD.load(Ordering::SeqCst),
        0,
        "a plain catch sends nothing again"
    );
    let held = defer();
    drop(held);
    assert_eq!(
        HEARD.load(Ordering::SeqCst),
        0,
        "a deferral that noted nothing sends nothing"
    );

    // Both lifetimes are real process catches. A later owner cannot erase the first
    // stop, and the owner that requested deferral may finish before the other one.
    for outer_defers in [true, false] {
        for outer_drops_first in [true, false] {
            let restore = STOPS.map(|sig| SetTo::new(sig, disposition(sig)));
            HEARD.store(0, Ordering::SeqCst);
            let outer = if outer_defers { defer() } else { catch() };
            hang_up();
            let inner = if outer_defers { catch() } else { defer() };
            let after_arm = caught();
            let before_last = if outer_drops_first {
                drop(outer);
                let observed = (caught(), HEARD.load(Ordering::SeqCst));
                drop(inner);
                observed
            } else {
                drop(inner);
                let observed = (caught(), HEARD.load(Ordering::SeqCst));
                drop(outer);
                observed
            };
            let after_last = (caught(), HEARD.load(Ordering::SeqCst));
            // Restore every disposition even on the old producer's out-of-order-drop
            // path, before any assertion can unwind this test.
            drop(catch());
            drop(restore);
            assert_eq!(
                after_arm,
                Some(libc::SIGHUP),
                "a new owner must retain the first stop"
            );
            assert_eq!(before_last, (Some(libc::SIGHUP), 0));
            assert_eq!(after_last, (None, libc::SIGHUP));
        }
    }
}

/// While [`say_on_stop`]'s handle lives, every stop nobody ignores meets the handler that
/// says its line, a hang-up this process ignores stays ignored, a deferral armed inside it
/// puts it back as it drops (so the stop it sends again says the line too), and the old
/// dispositions come back after. No stop is raised: the handler ends the process.
#[test]
fn a_stop_says_its_line_while_held_and_the_old_dispositions_come_back_after() {
    if isolated(
        "port::stops::tests::a_stop_says_its_line_while_held_and_the_old_dispositions_come_back_after",
    ) {
        return;
    }
    let _default = STOPS.map(|sig| SetTo::new(sig, libc::SIG_DFL));
    let _ignored = SetTo::new(libc::SIGHUP, libc::SIG_IGN);
    let before = STOPS.map(disposition);
    let says = say_then_stop as extern "C" fn(c_int) as libc::sighandler_t;
    let caught = [libc::SIGINT, libc::SIGTERM, libc::SIGQUIT];
    let saying = say_on_stop("stopped\n");
    for sig in caught {
        assert_eq!(disposition(sig), says, "{} says the line", name(sig));
    }
    assert_eq!(disposition(libc::SIGHUP), libc::SIG_IGN, "still ignored");
    let held = defer();
    for sig in caught {
        assert_ne!(disposition(sig), says, "{} is the deferral's", name(sig));
    }
    drop(held);
    for sig in caught {
        assert_eq!(disposition(sig), says, "{} says the line again", name(sig));
    }
    drop(saying);
    assert_eq!(STOPS.map(disposition), before);

    let saying = say_on_stop("stopped\n");
    let first = catch();
    let last = defer();
    drop(first);
    let while_held = caught.map(disposition);
    drop(saying);
    let without_saying = caught.map(disposition);
    drop(last);
    let after = STOPS.map(disposition);
    assert!(while_held.iter().all(|action| *action != says));
    assert_eq!(
        without_saying, while_held,
        "the catches still own the handler"
    );
    assert_eq!(
        after, before,
        "the final owner restores the original actions"
    );

    let held = catch();
    let catching = caught.map(disposition);
    let saying = say_on_stop("stopped\n");
    let while_caught = caught.map(disposition);
    drop(held);
    let while_saying = caught.map(disposition);
    drop(saying);
    let after = STOPS.map(disposition);
    assert_eq!(
        while_caught, catching,
        "a new line cannot replace the catch"
    );
    assert_eq!(while_saying, [says; 3]);
    assert_eq!(after, before);
}

/// A stop that meets [`say_on_stop`]'s handler says its line on standard error and then
/// ends the process as that stop would have: a forked child, its standard error on a pipe,
/// sends itself the stop. NEGATIVE CONTROL: with no line armed the child dies the same way
/// and says nothing, so the line is the handler's.
#[test]
fn a_stop_that_meets_the_handler_says_its_line_and_ends_the_process() {
    use std::io::Read as _;
    use std::os::fd::AsRawFd as _;
    use std::os::unix::process::ExitStatusExt as _;
    if isolated(
        "port::stops::tests::a_stop_that_meets_the_handler_says_its_line_and_ends_the_process",
    ) {
        return;
    }
    let _default = STOPS.map(|sig| SetTo::new(sig, libc::SIG_DFL));
    let line = "stopped; setup is not finished\n";
    for (sig, armed) in [
        (libc::SIGINT, true),
        (libc::SIGTERM, true),
        (libc::SIGINT, false),
    ] {
        let saying = armed.then(|| say_on_stop(line));
        let (mut said, to_said) = std::io::pipe().expect("pipe");
        let to_said_fd = to_said.as_raw_fd();
        // SAFETY: the child calls only async-signal-safe functions (`dup2`, `raise`, the
        // handler's `write`, `sigaction` and `_exit`) on a descriptor opened before `fork`,
        // and exits without unwinding.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork: {}", std::io::Error::last_os_error());
        if pid == 0 {
            // SAFETY: as above.
            unsafe { libc::dup2(to_said_fd, libc::STDERR_FILENO) };
            #[expect(
                clippy::disallowed_methods,
                reason = "a forked child sends itself SIGINT or SIGTERM, never a core-dump \
                          signal"
            )]
            // SAFETY: as above.
            let _ = unsafe { libc::raise(sig) };
            // SAFETY: as above; reached only when the stop did not end the child.
            unsafe { libc::_exit(0) };
        }
        drop(to_said);
        drop(saying);
        let mut status: c_int = 0;
        // SAFETY: waits for the child forked above, which ends at its stop.
        let reaped = unsafe { libc::waitpid(pid, &raw mut status, 0) };
        assert_eq!(reaped, pid, "waitpid: {}", std::io::Error::last_os_error());
        let mut out = String::new();
        said.read_to_string(&mut out)
            .expect("read the child's error");
        let status = std::process::ExitStatus::from_raw(status);
        assert_eq!(
            status.signal(),
            Some(sig),
            "{} armed {armed}: {status:?}",
            name(sig)
        );
        let want = if armed { line } else { "" };
        assert_eq!(out, want, "{} armed {armed}", name(sig));
    }
}

#[test]
fn each_stop_has_its_name() {
    assert_eq!(name(libc::SIGINT), "SIGINT");
    assert_eq!(name(libc::SIGTERM), "SIGTERM");
    assert_eq!(name(libc::SIGHUP), "SIGHUP");
    assert_eq!(name(libc::SIGQUIT), "SIGQUIT");
    assert_eq!(name(9), "signal 9");
}
