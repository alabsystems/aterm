// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FOLDER A FRESH SHELL STARTS IN (audit #7 finding 48).
//!
//! A fresh shell is asked to start in a folder by a restored pane (its saved
//! folder), a New Tab or split (the focused pane's), a new window, a profile or
//! the `-d` flag. The PTY child `chdir`s there and ignores the result, so a
//! folder that is gone, or one it may not enter, left the shell in aterm's own
//! folder — `/` for an app started from Finder — and nothing said so. Now
//! [`start_folder`] decides before the spawn: such a folder becomes the
//! person's home folder, and `spawn_session` notes the folder here once the
//! shell is up ([`StartFolder::say`]). The next park folds every noted folder
//! into one Messages row per [`Fault`] (`App::fold_folder_faults`). `spawn
//! cwd=` from the control socket is refused instead (`control_media`), because
//! its caller named the folder and can act on a refusal.
//!
//! The check runs on a thread of its own and is waited for at most
//! [`CHECK_BUDGET`]: a folder on a network volume that stopped answering
//! must not stall the event loop or the launch. A check that has not answered
//! by then keeps the folder, and the child's `chdir` decides, inside the PTY
//! spawn's own time limit. `stat` and `access(X_OK)` inside a macOS protected
//! folder answer without asking (docs/DESIGN-macos-tcc-prompts-2026-08-30.md
//! §1.5), and a privacy denial (`EPERM`) keeps the folder too.

use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

/// Why a shell cannot start in a folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    /// Nothing is at that path, what is there is not a folder, or the path
    /// cannot name one (a symlink loop, a name too long).
    Missing,
    /// The folder is there, and this user may not enter it.
    Shut,
}

impl Fault {
    /// Both faults, in the order their rows are posted.
    pub(crate) const ALL: [Self; 2] = [Self::Missing, Self::Shut];
}

/// How long a spawn waits for the folder check. A local folder answers in
/// microseconds; a hung network mount would hold the thread for its whole
/// timeout.
const CHECK_BUDGET: Duration = Duration::from_millis(250);

/// The most checks left waiting on a hung volume at once: past this, a spawn
/// does not start another and keeps its folder, so a mount that stopped
/// answering costs a bounded number of parked threads.
const CHECKS_IN_FLIGHT_MAX: usize = 4;

static CHECKS_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Why a shell cannot start in `dir`, or `None` when it can or when that
/// could not be decided in time (the child's `chdir` then decides, as it
/// always did). Off unix nothing is judged here — a Windows spawn hands the
/// folder to `CreateProcessW`, and a WSL tab's POSIX folder is no Windows path.
pub(crate) fn folder_fault(dir: &str) -> Option<Fault> {
    #[cfg(unix)]
    {
        within(&CHECKS_IN_FLIGHT, CHECK_BUDGET, dir, fault_now)
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        None
    }
}

/// Run `check(dir)` on a thread of its own and wait at most `budget` for it.
/// `None` when it did not answer in time, when [`CHECKS_IN_FLIGHT_MAX`] checks
/// counted in `in_flight` are already waiting, or when no thread could be
/// started. Every spawn counts in [`CHECKS_IN_FLIGHT`]; a test counts in its
/// own, so checks other tests leave waiting cannot fill its cap.
#[cfg(unix)]
fn within(
    in_flight: &'static AtomicUsize,
    budget: Duration,
    dir: &str,
    check: fn(&str) -> Option<Fault>,
) -> Option<Fault> {
    if in_flight.fetch_add(1, Ordering::AcqRel) >= CHECKS_IN_FLIGHT_MAX {
        in_flight.fetch_sub(1, Ordering::AcqRel);
        return None;
    }
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let owned = dir.to_owned();
    let spawned = std::thread::Builder::new()
        .name("aterm-folder-check".to_string())
        .spawn(move || {
            // The event loop is blocked on this answer: rank with it.
            crate::qos::set_self(crate::qos::Role::Interactive);
            let fault = check(&owned);
            in_flight.fetch_sub(1, Ordering::AcqRel);
            let _ = tx.send(fault);
        });
    if spawned.is_err() {
        in_flight.fetch_sub(1, Ordering::AcqRel);
        return None;
    }
    rx.recv_timeout(budget).unwrap_or(None)
}

/// [`folder_fault`] without the time limit: `stat`, then `access(X_OK)` for a
/// folder that is there.
#[cfg(unix)]
fn fault_now(dir: &str) -> Option<Fault> {
    let judged = |error: &std::io::Error| match error.raw_os_error() {
        Some(libc::ENOENT | libc::ENOTDIR | libc::ELOOP | libc::ENAMETOOLONG) => {
            Some(Fault::Missing)
        }
        Some(libc::EACCES) => Some(Fault::Shut),
        _ => None,
    };
    match std::fs::metadata(dir) {
        Ok(meta) if !meta.is_dir() => return Some(Fault::Missing),
        Ok(_) => {}
        Err(error) => return judged(&error),
    }
    let path = std::ffi::CString::new(dir).ok()?;
    // SAFETY: `path` is a NUL-terminated string that outlives the call;
    // `access` only reads it.
    if unsafe { libc::access(path.as_ptr(), libc::X_OK) } == 0 {
        return None;
    }
    judged(&std::io::Error::last_os_error())
}

/// Where a fresh shell starts: `cwd` for the PTY (`None`: the PTY's own
/// default), and the folder it was asked for with why it could not start
/// there, when the home folder stands in for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StartFolder {
    pub(crate) cwd: Option<String>,
    pub(crate) fell_back: Option<(String, Fault)>,
}

impl StartFolder {
    /// Once the shell is up: log the stand-in and note the folder for the
    /// next park's row. Nothing when the folder asked for was used.
    pub(crate) fn say(self) {
        if let Some((dir, fault)) = self.fell_back {
            crate::logging::stderr_line!(
                "aterm-gui: {dir} {}; the shell started in the home folder",
                match fault {
                    Fault::Missing => "was not found",
                    Fault::Shut => "could not be opened",
                }
            );
            note(dir, fault);
        }
    }
}

/// [`start_folder_in`] against the filesystem and the person's home folder.
pub(crate) fn start_folder(wanted: Option<&str>) -> StartFolder {
    let home = aterm_types::dirs::home_dir();
    start_folder_in(wanted, home.as_deref(), folder_fault)
}

/// Where a shell asked to start in `wanted` starts, PURE over `fault`: the
/// folder itself when a shell can start there; else `home`, naming the folder
/// and why. An empty `wanted` asks for nothing. With no home folder to go to
/// the request stands as it was (the child's `chdir` fails and the shell stays
/// in aterm's folder), and nothing claims the home folder.
pub(crate) fn start_folder_in(
    wanted: Option<&str>,
    home: Option<&Path>,
    fault: impl Fn(&str) -> Option<Fault>,
) -> StartFolder {
    let Some(dir) = wanted.filter(|dir| !dir.is_empty()) else {
        return StartFolder {
            cwd: None,
            fell_back: None,
        };
    };
    let stands = StartFolder {
        cwd: Some(dir.to_owned()),
        fell_back: None,
    };
    let Some(why) = fault(dir) else {
        return stands;
    };
    match home
        .and_then(Path::to_str)
        .filter(|home| fault(home).is_none())
    {
        Some(home) => StartFolder {
            cwd: Some(home.to_owned()),
            fell_back: Some((dir.to_owned(), why)),
        },
        None => stands,
    }
}

/// The folder a new tab, split or window takes from a pane, and a restored
/// pane from its saved one ([`crate::restore::TerminalLeafRestore::start_cwd`]):
/// the one the pane reported, unless a program other than the pane's shell
/// holds (or held) the pane (`held`: ssh, a container's shell) and no shell
/// could start in that folder here. Such a folder may be another machine's,
/// so it is not taken and not reported: the spawn starts where it would for a
/// pane that reported none.
pub(crate) fn inherited(reported: String, held: impl FnOnce() -> bool) -> Option<String> {
    if cfg!(unix) {
        inherited_in(reported, held, folder_fault)
    } else {
        Some(reported)
    }
}

/// [`inherited`], PURE over `fault`. `held` is asked first: it is one
/// `tcgetpgrp`, where the folder check starts a thread.
pub(crate) fn inherited_in(
    reported: String,
    held: impl FnOnce() -> bool,
    fault: impl Fn(&str) -> Option<Fault>,
) -> Option<String> {
    if held() && fault(&reported).is_some() {
        None
    } else {
        Some(reported)
    }
}

static NOTED_PENDING: AtomicBool = AtomicBool::new(false);
/// The folders noted since the last park, oldest first, each once. Every
/// entry is a shell that was spawned, so the list is as long as the person's
/// own new tabs, and the park empties it.
static NOTED: Mutex<Vec<(String, Fault)>> = Mutex::new(Vec::new());

/// Note a folder a shell was asked to start in and could not, for the next
/// park's row. Safe from any thread and before `App` exists (session 0 spawns
/// in `main_entry`); poison-tolerant and deduplicated.
fn note(dir: String, fault: Fault) {
    let Ok(mut noted) = NOTED.lock() else {
        return;
    };
    if !noted.iter().any(|(seen, _)| *seen == dir) {
        noted.push((dir, fault));
        NOTED_PENDING.store(true, Ordering::Release);
    }
}

/// Take every noted folder, oldest first (empty when none: one atomic load).
pub(crate) fn take_noted() -> Vec<(String, Fault)> {
    if !NOTED_PENDING.load(Ordering::Acquire) {
        return Vec::new();
    }
    NOTED_PENDING.store(false, Ordering::Relaxed);
    NOTED
        .lock()
        .map(|mut noted| std::mem::take(&mut *noted))
        .unwrap_or_default()
}

/// The folders one fault's row names while it is up: each once, oldest
/// first, and how many a row carried across an update counted without naming
/// them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Named {
    pub(crate) dirs: Vec<String>,
    pub(crate) unnamed: usize,
}

impl Named {
    /// Add `dir` unless the row already names it.
    pub(crate) fn add(&mut self, dir: String) {
        if !self.dirs.contains(&dir) {
            self.dirs.push(dir);
        }
    }
}

/// [`Named`] for each [`Fault`]'s row: the state `App::fold_folder_faults`
/// keeps between parks.
#[derive(Clone, Debug, Default)]
pub(crate) struct RowFolders {
    missing: Named,
    shut: Named,
}

impl RowFolders {
    pub(crate) fn of(&mut self, fault: Fault) -> &mut Named {
        match fault {
            Fault::Missing => &mut self.missing,
            Fault::Shut => &mut self.shut,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Fault, StartFolder, inherited_in, start_folder_in, take_noted};
    #[cfg(unix)]
    use super::{folder_fault, within};
    use std::path::Path;
    #[cfg(unix)]
    use std::time::{Duration, Instant};

    /// A folder that is there stands; one that is gone, a FILE where the
    /// folder was, a path through a file and a symlink loop are Missing; a
    /// folder this user may not enter is Shut. The negative control is the
    /// scratch folder itself.
    #[cfg(unix)]
    #[test]
    fn each_folder_a_shell_cannot_start_in_is_judged_and_a_folder_is_not() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = aterm_tempfile::Builder::new()
            .prefix("aterm-start-folder")
            .tempdir()
            .expect("scratch dir");
        let at = |name: &str| scratch.path().join(name).to_str().unwrap().to_owned();
        let here = scratch.path().to_str().expect("utf-8 scratch path");
        assert_eq!(folder_fault(here), None, "a folder that is there");
        assert_eq!(folder_fault(&at("gone")), Some(Fault::Missing), "nothing");
        std::fs::write(at("file"), b"x").unwrap();
        assert_eq!(folder_fault(&at("file")), Some(Fault::Missing), "a file");
        assert_eq!(
            folder_fault(&at("file/below")),
            Some(Fault::Missing),
            "a path through a file (ENOTDIR)"
        );
        std::os::unix::fs::symlink(at("loop"), at("loop")).unwrap();
        assert_eq!(folder_fault(&at("loop")), Some(Fault::Missing), "ELOOP");
        std::fs::create_dir(at("shut")).unwrap();
        std::fs::set_permissions(at("shut"), std::fs::Permissions::from_mode(0o600)).unwrap();
        let shut = folder_fault(&at("shut"));
        std::fs::set_permissions(at("shut"), std::fs::Permissions::from_mode(0o700)).unwrap();
        // root enters any folder: the verdict is the kernel's, not the mode bits'.
        if !is_root() {
            assert_eq!(
                shut,
                Some(Fault::Shut),
                "a folder with no search permission"
            );
        }
        assert_eq!(folder_fault(&at("shut")), None, "the same folder, opened");
    }

    #[cfg(unix)]
    fn is_root() -> bool {
        // SAFETY: getuid has no failure mode and takes no arguments.
        unsafe { libc::getuid() == 0 }
    }

    /// A CHECK THAT DOES NOT ANSWER KEEPS THE FOLDER: `within` gives up on a
    /// check still running, so a hung volume costs the wait, never the event
    /// loop. The negative control is a check that answers.
    #[cfg(unix)]
    #[test]
    fn a_check_that_does_not_answer_in_time_keeps_the_folder() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::{Condvar, Mutex, PoisonError};

        /// This test's own in-flight count: the process-wide one is shared with
        /// every test that spawns a shell in a folder, and four checks those
        /// leave waiting would fill the cap and turn this `None` into the cap's.
        static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
        /// The hung check has started (so a `None` is the budget's, not the
        /// in-flight cap's), and the gate it waits behind until the verdict.
        static ENTERED: AtomicBool = AtomicBool::new(false);
        static GATE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
        fn hangs(_: &str) -> Option<Fault> {
            ENTERED.store(true, Ordering::SeqCst);
            let (open, bell) = &GATE;
            let open = open.lock().unwrap_or_else(PoisonError::into_inner);
            // Answers once the test has its verdict — or after a minute, the hang
            // detector for a `within` that waits for it (whose `Some` then fails).
            let _ = bell
                .wait_timeout_while(open, Duration::from_secs(60), |open| !*open)
                .unwrap_or_else(PoisonError::into_inner);
            Some(Fault::Missing)
        }
        fn answers(_: &str) -> Option<Fault> {
            Some(Fault::Missing)
        }
        // The check CANNOT answer before the gate opens below, so this `None` is
        // `within` giving up on a check still running — an order. What the order
        // does NOT see is WHEN it gave up: a `within` that waited a bounded while
        // past `budget` (under the minute) would pass too. That the wait is
        // `budget` is its one `recv_timeout(budget)`, and no clock here judges
        // it. (It was a 2 s sleep under a 1 s bound: a loaded box could cross it
        // with `within` correct, or let the sleep finish first and hear `Some`.)
        assert_eq!(
            within(&IN_FLIGHT, Duration::from_millis(50), "/x", hangs),
            None
        );
        let started = Instant::now();
        while !ENTERED.load(Ordering::SeqCst) {
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "the hung check never started: the cap, not the budget, said None"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        *GATE.0.lock().unwrap_or_else(PoisonError::into_inner) = true;
        GATE.1.notify_all();
        assert_eq!(
            within(&IN_FLIGHT, Duration::from_secs(60), "/x", answers),
            Some(Fault::Missing),
            "a check that answers is heard"
        );
    }

    /// A SPAWN WHOSE FOLDER IS GONE STARTS IN THE HOME FOLDER and names the
    /// folder and why; a folder that is there is untouched (the negative
    /// control); nothing asked is nothing changed; and with no home folder to
    /// go to the request stands and nothing claims one.
    #[test]
    fn a_folder_a_shell_cannot_start_in_starts_it_in_home_and_a_present_one_is_unchanged() {
        let fault = |dir: &str| match dir {
            "/work/gone" => Some(Fault::Missing),
            "/work/shut" => Some(Fault::Shut),
            _ => None,
        };
        let home = Some(Path::new("/Users//me"));
        for (dir, why) in [("/work/gone", Fault::Missing), ("/work/shut", Fault::Shut)] {
            let fell_back = start_folder_in(Some(dir), home, fault);
            assert_eq!(fell_back.cwd.as_deref(), Some("/Users//me"));
            assert_eq!(fell_back.fell_back, Some((dir.to_owned(), why)));
        }

        let kept = start_folder_in(Some("/work/here"), home, fault);
        assert_eq!(kept.cwd.as_deref(), Some("/work/here"));
        assert_eq!(kept.fell_back, None);

        for nothing in [None, Some("")] {
            let asked = start_folder_in(nothing, home, fault);
            assert_eq!((asked.cwd, asked.fell_back), (None, None), "{nothing:?}");
        }

        for no_home in [None, Some(Path::new("/work/gone"))] {
            let stands = start_folder_in(Some("/work/gone"), no_home, fault);
            assert_eq!(stands.cwd.as_deref(), Some("/work/gone"), "{no_home:?}");
            assert_eq!(stands.fell_back, None, "no home folder is claimed");
        }
    }

    /// WHAT `start_folder` DECIDES IS WHAT THE PARK SEES: the fall-back a
    /// spawn says once its shell is up reaches the queue, in order, each
    /// folder once, and empty once taken. The negative control is a start
    /// that used its folder, which notes nothing.
    #[test]
    fn a_start_that_fell_back_reaches_the_queue_and_one_that_did_not_says_nothing() {
        let _lane = crate::message_inbox::lane_test_guard();
        let _ = take_noted();
        let fault =
            |dir: &str| (dir != "/Users//me" && dir != "/work/here").then_some(Fault::Missing);
        let home = Some(Path::new("/Users//me"));
        start_folder_in(Some("/work/here"), home, fault).say();
        assert!(take_noted().is_empty(), "a folder that was used");
        for dir in ["/a", "/b", "/a"] {
            start_folder_in(Some(dir), home, fault).say();
        }
        StartFolder {
            cwd: Some("/Users//me".into()),
            fell_back: Some(("/c".into(), Fault::Shut)),
        }
        .say();
        assert_eq!(
            take_noted(),
            [
                ("/a".to_owned(), Fault::Missing),
                ("/b".to_owned(), Fault::Missing),
                ("/c".to_owned(), Fault::Shut)
            ]
        );
        assert!(take_noted().is_empty(), "drained");
    }

    /// AN SSH PANE'S FOLDER IS NOT TAKEN WHEN IT IS NOT HERE: a program other
    /// than the pane's shell holds the pane, so the folder may be another
    /// machine's. NEGATIVE CONTROLS: the same folder from a pane at its own
    /// prompt is taken (its spawn then says it was not found), and a folder
    /// that is here is taken from a held pane.
    #[test]
    fn a_held_panes_folder_is_taken_only_when_it_is_here() {
        let gone = |dir: &str| (dir == "/home/me/proj").then_some(Fault::Missing);
        let remote = "/home/me/proj".to_owned();
        assert_eq!(inherited_in(remote.clone(), || true, gone), None);
        assert_eq!(
            inherited_in(remote.clone(), || false, gone),
            Some(remote),
            "the pane's own shell reported it"
        );
        let local = "/Users//me/src".to_owned();
        assert_eq!(inherited_in(local.clone(), || true, gone), Some(local));
    }
}
