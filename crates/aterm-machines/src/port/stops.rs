// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`super::System::catch_stops`]'s and [`super::System::defer_stops`]'s live arms: catches
//! SIGINT, SIGTERM, SIGHUP and SIGQUIT, and leaves one already ignored (`nohup`, `trap '' HUP`)
//! ignored. [`say_on_stop`] says one line at such a stop and lets it end the process.

use std::ffi::c_int;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, Ordering};

const STOPS: [c_int; 4] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT];

static CAUGHT: AtomicI32 = AtomicI32::new(0);

/// Only ordinary code owns this mutex. The signal handlers touch atomics, never this state.
static OWNERS: Mutex<Owners> = Mutex::new(Owners {
    catches: 0,
    sayings: 0,
    resend: false,
    was: None,
});

struct Owners {
    catches: usize,
    sayings: usize,
    /// A deferring owner may finish before the last catching owner.
    resend: bool,
    was: Option<[(c_int, Option<libc::sigaction>); 4]>,
}

impl Owners {
    /// Select the process handler from the live owners, preserving the original ignored
    /// signals. An owner changing underneath a catch never replaces the catch's handler.
    fn select(&mut self) {
        let handler = if self.catches != 0 {
            note as extern "C" fn(c_int)
        } else if self.sayings != 0 {
            say_then_stop as extern "C" fn(c_int)
        } else {
            if let Some(was) = self.was.take() {
                put_back(&was);
            }
            return;
        };
        // SAFETY: `sigaction` is plain data for which all-zero is a valid value (an empty
        // mask, no flags).
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = handler as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        if let Some(was) = &self.was {
            for (sig, previous) in was {
                if previous.is_some() {
                    // SAFETY: selects a valid handler for a signal this lifetime replaced;
                    // the original action remains in `was` until its last owner is gone.
                    unsafe { libc::sigaction(*sig, &action, std::ptr::null_mut()) };
                }
            }
        } else {
            self.was = Some(STOPS.map(|sig| replace_unless_ignored(sig, &action)));
        }
    }
}

/// The handler: keep the first signal, nothing else (async-signal-safe).
extern "C" fn note(sig: c_int) {
    let _ = CAUGHT.compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst);
}

/// The line [`say_on_stop`]'s handler writes, one pointer the handler reads whole.
static SAID: AtomicPtr<&'static str> = AtomicPtr::new(std::ptr::null_mut());

/// Whether [`say_on_stop`]'s handler wrote its line since it was armed: a second stop that
/// meets the handler before the first ends the process says nothing more.
static SPOKEN: AtomicBool = AtomicBool::new(false);

/// [`say_on_stop`]'s handler (async-signal-safe: atomics, `write`, `sigaction`, `raise` and
/// `_exit` only): the line on standard error once, a terminal gone with its tab being no error,
/// then the stop again. The signal's disposition goes back to its default first, so the stop
/// sent again (held until the handler returns, as the kernel holds a signal while its own
/// handler runs) ends the process as it would have; a default SIGQUIT would dump core and write
/// a crash report, so it exits with the shell's status for it, as [`resend`] does.
extern "C" fn say_then_stop(sig: c_int) {
    let said = SAID.load(Ordering::SeqCst);
    if !said.is_null() && !SPOKEN.swap(true, Ordering::SeqCst) {
        // SAFETY: a non-null `SAID` is a leaked `&'static str` [`say_on_stop`] stored, never
        // freed or written through; its bytes live as long as the process and are only read.
        let line: &'static str = unsafe { *said };
        // SAFETY: `write` reads `line.len()` bytes from `line`, which holds them.
        let _ = unsafe { libc::write(libc::STDERR_FILENO, line.as_ptr().cast(), line.len()) };
    }
    if sig == libc::SIGQUIT {
        // SAFETY: `_exit` takes an integer and ends the process at once.
        unsafe { libc::_exit(128 + sig) };
    }
    // SAFETY: as in `Owners::select`.
    let mut default: libc::sigaction = unsafe { std::mem::zeroed() };
    default.sa_sigaction = libc::SIG_DFL;
    // SAFETY: the default disposition for `sig`; nothing reads the one it replaces.
    let _ = unsafe { libc::sigaction(sig, &default, std::ptr::null_mut()) };
    #[expect(
        clippy::disallowed_methods,
        reason = "the stop sent again to its default disposition, which for SIGINT, SIGTERM and \
                  SIGHUP dumps no core; SIGQUIT exits above"
    )]
    // SAFETY: `raise` takes an integer and touches no memory of ours.
    let _ = unsafe { libc::raise(sig) };
}

/// [`say_on_stop`]'s logical owner, underneath any live catches.
struct Saying;

impl Drop for Saying {
    fn drop(&mut self) {
        let mut owners = OWNERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        owners.sayings -= 1;
        if owners.catches == 0 && owners.sayings == 0 {
            owners.select();
        }
    }
}

/// Until the returned handle drops, a stop ([`STOPS`]) writes `line` (its newline included) to
/// standard error and then ends the process as that stop would have. A signal this process
/// ignores already stays ignored, as [`catch`] leaves it. A catch armed inside it
/// ([`super::System::catch_stops`], [`super::System::defer_stops`]) replaces it while it lives
/// and puts it back as it drops, so the stop a deferral sends again says the line too.
pub fn say_on_stop(line: &'static str) -> Box<dyn std::any::Any> {
    let mut owners = OWNERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // One leaked reference per arm (a process arms it once), so the handler reads the line
    // through one atomic pointer, never a pointer and a length a second arm could split.
    SAID.store(Box::into_raw(Box::new(line)), Ordering::SeqCst);
    SPOKEN.store(false, Ordering::SeqCst);
    owners.sayings += 1;
    if owners.catches == 0 && owners.sayings == 1 {
        owners.select();
    }
    Box::new(Saying)
}

/// Set `sig`'s disposition to `action` unless this process ignores it: the one it replaced, or
/// `None` where it is ignored or could not be set.
fn replace_unless_ignored(
    sig: c_int,
    action: &libc::sigaction,
) -> (c_int, Option<libc::sigaction>) {
    // SAFETY: as in `Owners::select`.
    let mut was: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: reads `sig`'s action only.
    if unsafe { libc::sigaction(sig, std::ptr::null(), &mut was) } != 0
        || was.sa_sigaction == libc::SIG_IGN
    {
        return (sig, None);
    }
    // SAFETY: a valid disposition for `sig`; the one it replaces is `was`, read above.
    let set = unsafe { libc::sigaction(sig, action, std::ptr::null_mut()) } == 0;
    (sig, set.then_some(was))
}

/// Put back each disposition [`replace_unless_ignored`] replaced.
fn put_back(was: &[(c_int, Option<libc::sigaction>)]) {
    for (sig, was) in was {
        if let Some(was) = was {
            // SAFETY: puts back the whole action `replace_unless_ignored` replaced for `sig`.
            unsafe { libc::sigaction(*sig, was, std::ptr::null_mut()) };
        }
    }
}

struct Catching;

impl Drop for Catching {
    fn drop(&mut self) {
        let mut owners = OWNERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        owners.catches -= 1;
        if owners.catches != 0 {
            return;
        }
        owners.select();
        // Read once every disposition is back: a stop noted before its own came back is here,
        // and one after it met that disposition itself.
        let noted = CAUGHT.swap(0, Ordering::SeqCst);
        if std::mem::take(&mut owners.resend) && noted != 0 {
            // Keep lifecycle ownership through the resend: a new catch cannot clear the
            // noted stop or intercept it between restoration and delivery. Neither signal
            // handler accesses this mutex.
            resend(noted);
        }
    }
}

/// Catch [`STOPS`] until the last overlapping handle drops, then forget them unless an owner
/// deferred them. A signal this process ignores already stays ignored: whoever started it asked
/// it to outlive that signal (a hang-up under `nohup`), so it is no stop.
pub(super) fn catch() -> Box<dyn std::any::Any> {
    arm(false)
}

/// Catch [`STOPS`] as [`catch`] does, and once every overlapping catch has finished and the
/// underlying dispositions are back, send the one noted again: the run finishes its steps and
/// cleanup, then stops as it would have.
pub(super) fn defer() -> Box<dyn std::any::Any> {
    arm(true)
}

/// Send `sig` to this process again, now that its own disposition is back: a default SIGINT,
/// SIGTERM or SIGHUP ends it as the signal would have, and a handler runs. A default SIGQUIT would
/// dump core and write a crash report, so the process exits with the shell's status for it.
fn resend(sig: c_int) {
    if sig == libc::SIGQUIT && disposition(sig) == libc::SIG_DFL {
        std::process::exit(128 + sig);
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "a deferred stop sent to the process's own disposition, which for SIGINT, \
                  SIGTERM and SIGHUP dumps no core; a default SIGQUIT exits above"
    )]
    // SAFETY: `raise` takes an integer and touches no memory of ours.
    let _ = unsafe { libc::raise(sig) };
}

/// `sig`'s disposition now (`SIG_DFL` when it cannot be read).
fn disposition(sig: c_int) -> libc::sighandler_t {
    // SAFETY: as in `Owners::select`.
    let mut now: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: reads the action only.
    if unsafe { libc::sigaction(sig, std::ptr::null(), &mut now) } != 0 {
        return libc::SIG_DFL;
    }
    now.sa_sigaction
}

fn arm(resend: bool) -> Box<dyn std::any::Any> {
    let mut owners = OWNERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    owners.catches += 1;
    owners.resend |= resend;
    if owners.catches == 1 {
        CAUGHT.store(0, Ordering::SeqCst);
        owners.select();
    }
    Box::new(Catching)
}

/// Set SIGHUP and SIGPIPE to `SIG_IGN` for good ([`super::System::ignore_hangups`]).
pub(super) fn ignore_hangups() {
    let mut owners = OWNERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for sig in [libc::SIGHUP, libc::SIGPIPE] {
        // SAFETY: as in `Owners::select`.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = libc::SIG_IGN;
        // SAFETY: a valid disposition for `sig`; the one it replaces is not wanted back.
        let ignored = unsafe { libc::sigaction(sig, &action, std::ptr::null_mut()) } == 0;
        if ignored && let Some(was) = &mut owners.was {
            // This is permanent, including after owners that predate detachment finish.
            if let Some((_, previous)) = was.iter_mut().find(|(saved, _)| *saved == sig) {
                *previous = None;
            }
        }
    }
}

/// The first signal [`catch`] or [`defer`] noted, while its handle lives.
pub(super) fn caught() -> Option<i32> {
    match CAUGHT.load(Ordering::SeqCst) {
        0 => None,
        sig => Some(sig),
    }
}

/// `SIGINT`, `SIGTERM`, `SIGHUP` or `SIGQUIT` for one of [`STOPS`]; `signal <n>` otherwise.
#[must_use]
pub fn name(sig: i32) -> String {
    match sig {
        libc::SIGINT => "SIGINT".into(),
        libc::SIGTERM => "SIGTERM".into(),
        libc::SIGHUP => "SIGHUP".into(),
        libc::SIGQUIT => "SIGQUIT".into(),
        n => format!("signal {n}"),
    }
}

#[cfg(test)]
mod tests {
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
        let mut child =
            aterm_uds::spawnfd::command(std::env::current_exe().expect("test executable"));
        child
            .args(["--exact", case, "--nocapture"])
            .env("ATERM_STOP_CASE", case);
        let output =
            aterm_uds::deadline::output_within(&mut child, std::time::Duration::from_secs(60))
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
}
