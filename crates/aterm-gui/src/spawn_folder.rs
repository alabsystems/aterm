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
//! by then keeps a folder on a local volume (a starved CPU can hold a local
//! disk that long), and the child's `chdir` decides. On a volume that is NOT
//! local — SMB, NFS, AFP, WebDAV, a user-space file system — the same silence
//! is [`Fault::Unresponsive`] and the shell starts in the home folder: the
//! child would `chdir` there before `exec`, where a hung mount holds it past
//! the PTY spawn's exec-status wait (10 s, then the reap) — on the event loop
//! for a new window or a restore; a new tab or split spawns on the spawner
//! thread (`tab_spawn`). Which volume holds the folder is read from the kernel's mount
//! table, which asks no file system ([`remote_volume`]). `stat` and
//! `access(X_OK)` inside a macOS protected folder answer without asking
//! (docs/DESIGN-macos-tcc-prompts-2026-08-30.md §1.5), and a privacy denial
//! (`EPERM`) keeps the folder too.

use std::path::Path;
use std::sync::Mutex;
#[cfg(unix)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::time::Duration;

/// Why a shell cannot start in a folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    /// Nothing is at that path, what is there is not a folder, or the path
    /// cannot name one (a symlink loop, a name too long).
    Missing,
    /// The folder is there, and this user may not enter it.
    Shut,
    /// The folder's check did not answer in time, and the folder is on a
    /// volume that is not local: a mount that stopped answering. Only a
    /// spawn judges this ([`start_fault`]); [`folder_fault`] never says it.
    Unresponsive,
}

impl Fault {
    /// Every fault, in the order their rows are posted.
    pub(crate) const ALL: [Self; 3] = [Self::Missing, Self::Shut, Self::Unresponsive];
}

/// How long a spawn waits for the folder check. A local folder answers in
/// microseconds; a hung network mount would hold the thread for its whole
/// timeout.
#[cfg(unix)]
const CHECK_BUDGET: Duration = Duration::from_millis(250);

/// The most checks left waiting on a hung volume at once: past this, a spawn
/// does not start another and judges its folder as one that did not answer
/// ([`start_fault`]), so a mount that stopped answering costs a bounded number
/// of parked threads.
#[cfg(unix)]
const CHECKS_IN_FLIGHT_MAX: usize = 4;

#[cfg(unix)]
static CHECKS_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Why a shell cannot start in `dir`, or `None` when it can or when that
/// could not be decided in time (the child's `chdir` then decides, as it
/// always did). Off unix nothing is judged here — a Windows spawn hands the
/// folder to `CreateProcessW`, and a WSL tab's POSIX folder is no Windows path.
pub(crate) fn folder_fault(dir: &str) -> Option<Fault> {
    #[cfg(unix)]
    {
        within(&CHECKS_IN_FLIGHT, CHECK_BUDGET, dir, fault_now).flatten()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        None
    }
}

/// Why a fresh shell cannot start in `dir`: [`folder_fault`], except that a
/// check that did not answer in time — or was not started, the cap being
/// reached — is [`Fault::Unresponsive`] for a folder on a volume that is not
/// local ([`remote_volume`]). A folder on a local volume still stands.
pub(crate) fn start_fault(dir: &str) -> Option<Fault> {
    #[cfg(unix)]
    {
        start_fault_in(
            &CHECKS_IN_FLIGHT,
            CHECK_BUDGET,
            dir,
            fault_now,
            remote_volume,
        )
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        None
    }
}

/// [`start_fault`] over its check, its cap and its mount table.
#[cfg(unix)]
fn start_fault_in(
    in_flight: &'static AtomicUsize,
    budget: Duration,
    dir: &str,
    check: fn(&str) -> Option<Fault>,
    remote: impl FnOnce(&str) -> bool,
) -> Option<Fault> {
    within(in_flight, budget, dir, check)
        .unwrap_or_else(|| remote(dir).then_some(Fault::Unresponsive))
}

/// Run `check(dir)` on a thread of its own and wait at most `budget` for it:
/// `Some` of its answer, or `None` when it did not answer in time, when
/// [`CHECKS_IN_FLIGHT_MAX`] checks counted in `in_flight` are already waiting,
/// or when no thread could be started. Every spawn counts in
/// [`CHECKS_IN_FLIGHT`]; a test counts in its own, so checks other tests leave
/// waiting cannot fill its cap.
#[cfg(unix)]
fn within(
    in_flight: &'static AtomicUsize,
    budget: Duration,
    dir: &str,
    check: fn(&str) -> Option<Fault>,
) -> Option<Option<Fault>> {
    if in_flight.fetch_add(1, Ordering::AcqRel) >= CHECKS_IN_FLIGHT_MAX {
        in_flight.fetch_sub(1, Ordering::AcqRel);
        return None;
    }
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let owned = dir.to_owned();
    let spawned = crate::qos::spawn_lane_thread(
        "aterm-folder-check".to_string(),
        crate::qos::Role::Interactive,
        move || {
            // The spawn that asked waits on this answer, on the event loop or
            // the spawner thread a person watches: rank with it.
            let fault = check(&owned);
            in_flight.fetch_sub(1, Ordering::AcqRel);
            let _ = tx.send(fault);
        },
    );
    if spawned.is_err() {
        in_flight.fetch_sub(1, Ordering::AcqRel);
        return None;
    }
    rx.recv_timeout(budget).ok()
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

/// Whether `dir` is on a volume that is not local, by the kernel's mount
/// table ([`mount_table`]) — the one question about a folder whose check hangs
/// that cannot hang too.
#[cfg(unix)]
fn remote_volume(dir: &str) -> bool {
    let table = mount_table();
    on_remote_mount(dir, table.iter().map(|(on, local)| (on.as_path(), *local)))
}

/// PURE: whether the mount that holds `dir` — the longest mount point that is
/// a whole-component prefix of it, as written — is not local. A symlink into a
/// network volume is judged by the folder it is in (resolving it would ask the
/// volume), and a path no mount holds is local.
#[cfg(unix)]
fn on_remote_mount<'a>(dir: &str, mounts: impl IntoIterator<Item = (&'a Path, bool)>) -> bool {
    let dir = Path::new(dir);
    mounts
        .into_iter()
        .filter(|(on, _)| dir.starts_with(on))
        .max_by_key(|(on, _)| on.components().count())
        .is_some_and(|(_, local)| !local)
}

// `<sys/mount.h>`: `int getfsstat(struct statfs *buf, int bufsize, int mode)`,
// answering the number of mounts (written, or with a null `buf` mounted), or
// -1. Declared here because `crates/aterm-libc` is generated and does not carry
// it; Intel spells the 64-bit-inode `statfs` it fills (`libc::statfs`) as
// `getfsstat$INODE64`, as `libc::statfs` itself is `statfs$INODE64` there.
#[cfg(target_os = "macos")]
unsafe extern "C" {
    #[cfg_attr(target_arch = "x86_64", link_name = "getfsstat$INODE64")]
    fn getfsstat(buf: *mut libc::statfs, bufsize: libc::c_int, mode: libc::c_int) -> libc::c_int;
}

/// Every mount point, and whether its volume is local (`MNT_LOCAL`), as the
/// kernel last recorded them: `MNT_NOWAIT` asks no file system to refresh its
/// numbers, so a hung mount is listed without being touched. Empty when the
/// table cannot be read, which judges every folder local.
#[cfg(target_os = "macos")]
fn mount_table() -> Vec<(std::path::PathBuf, bool)> {
    use std::os::unix::ffi::OsStringExt as _;
    /// `<sys/mount.h>`.
    const MNT_NOWAIT: libc::c_int = 2;
    const MNT_LOCAL: u32 = 0x0000_1000;
    // SAFETY: a null buffer asks only for the count; nothing is written.
    let Ok(count) = usize::try_from(unsafe { getfsstat(std::ptr::null_mut(), 0, MNT_NOWAIT) })
    else {
        return Vec::new();
    };
    // SAFETY: `statfs` is integers and arrays of them: all-zero is a value.
    let blank: libc::statfs = unsafe { std::mem::zeroed() };
    // Room for a few mounted between the two calls; one mounted past that is
    // left out, and a folder on it is judged local.
    let mut table = vec![blank; count + 4];
    let Ok(bytes) = libc::c_int::try_from(std::mem::size_of_val(table.as_slice())) else {
        return Vec::new();
    };
    // SAFETY: `table` is `bytes` writable bytes of whole `statfs` records, and
    // getfsstat writes at most that many, answering how many it wrote.
    let written = unsafe { getfsstat(table.as_mut_ptr(), bytes, MNT_NOWAIT) };
    let Ok(written) = usize::try_from(written) else {
        return Vec::new();
    };
    table
        .iter()
        .take(written)
        .map(|record| {
            // The kernel NUL-terminates the mount point inside its array.
            let on = record.f_mntonname.iter().take_while(|&&c| c != 0);
            let on: Vec<u8> = on.map(|&c| c.to_ne_bytes()[0]).collect();
            (
                std::ffi::OsString::from_vec(on).into(),
                record.f_flags & MNT_LOCAL != 0,
            )
        })
        .collect()
}

/// No mount table is read off macOS: every folder is judged local, and a check
/// that does not answer keeps its folder, as it always did there.
#[cfg(all(unix, not(target_os = "macos")))]
fn mount_table() -> Vec<(std::path::PathBuf, bool)> {
    Vec::new()
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
                    Fault::Unresponsive => "did not answer",
                }
            );
            note(dir, fault);
        }
    }
}

/// [`start_folder_in`] against the filesystem and the person's home folder.
pub(crate) fn start_folder(wanted: Option<&str>) -> StartFolder {
    let home = aterm_types::dirs::home_dir();
    start_folder_in(wanted, home.as_deref(), start_fault)
}

/// Where a shell asked to start in `wanted` starts, PURE over `fault`: the
/// folder itself when a shell can start there; else `home`, naming the folder
/// and why. An empty `wanted` asks for nothing. With no home folder to go to
/// the request stands as it was (the child's `chdir` fails and the shell stays
/// in aterm's folder), and nothing claims the home folder — except for a
/// folder that did not answer, which no `chdir` is handed: the PTY's own
/// default stands instead. (For an aterm started from `/` that default is
/// `$HOME`, so a network home that stopped answering still holds the child,
/// as it holds every shell that asks for no folder: a new tab or split waits
/// on the spawner thread (`tab_spawn`), a new window or a restore on the event
/// loop.)
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
    // The folder that failed IS the home folder (a pane at `~` on a network
    // home): asking again would only fail again — for one that did not answer,
    // after a second wait.
    match home
        .and_then(Path::to_str)
        .filter(|home| *home != dir && fault(home).is_none())
    {
        Some(home) => StartFolder {
            cwd: Some(home.to_owned()),
            fell_back: Some((dir.to_owned(), why)),
        },
        None if why == Fault::Unresponsive => StartFolder {
            cwd: None,
            fell_back: None,
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

/// What [`inherited`] judges a pane's folder by, read where the pane is (the
/// event loop: the reported folder and one `tcgetpgrp`), so the folder check
/// itself, up to [`CHECK_BUDGET`] for a held pane, can run where a spawn may
/// wait (the spawner thread, `tab_spawn`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneFolder {
    /// The folder the pane reported.
    pub(crate) reported: String,
    /// Whether a program other than the pane's shell held the pane then.
    pub(crate) held: bool,
}

impl PaneFolder {
    /// The folder a spawn takes from the pane ([`inherited`]).
    pub(crate) fn resolve(self) -> Option<String> {
        let Self { reported, held } = self;
        inherited(reported, || held)
    }

    /// The folder the first of `panes`, in order, gives a spawn
    /// ([`Self::resolve`]): a pane whose folder is not taken hands over to the
    /// next, as the caller would have asked it next.
    pub(crate) fn first_taken(panes: impl IntoIterator<Item = Self>) -> Option<String> {
        panes.into_iter().find_map(Self::resolve)
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
    unresponsive: Named,
}

impl RowFolders {
    pub(crate) fn of(&mut self, fault: Fault) -> &mut Named {
        match fault {
            Fault::Missing => &mut self.missing,
            Fault::Shut => &mut self.shut,
            Fault::Unresponsive => &mut self.unresponsive,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Fault, StartFolder, inherited_in, start_folder_in, take_noted};
    #[cfg(unix)]
    use super::{PaneFolder, folder_fault, on_remote_mount, remote_volume, start_fault_in, within};
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

    /// A CHECK THAT DOES NOT ANSWER KEEPS A LOCAL FOLDER: `within` gives up on
    /// a check still running, so a hung volume costs the wait, never the event
    /// loop, and a spawn keeps a folder on a local volume. On a volume that is
    /// not local the same silence is `Unresponsive`. The negative control is a
    /// check that answers.
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
        let budget = Duration::from_millis(50);
        assert_eq!(
            start_fault_in(&IN_FLIGHT, budget, "/x", hangs, |_| false),
            None,
            "a local folder that did not answer stands"
        );
        assert_eq!(
            start_fault_in(&IN_FLIGHT, budget, "/x", hangs, |_| true),
            Some(Fault::Unresponsive),
            "a folder on a volume that is not local, silent, is no place to start"
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
            Some(Some(Fault::Missing)),
            "a check that answers is heard"
        );
        assert_eq!(
            start_fault_in(&IN_FLIGHT, Duration::from_secs(60), "/x", answers, |_| true),
            Some(Fault::Missing),
            "an answer wins over the volume"
        );
    }

    /// WITH THE CAP FULL NO CHECK STARTS, and the folder is judged as one that
    /// did not answer: a folder on a volume that is not local is
    /// `Unresponsive`, never handed to the child's `chdir` unchecked; a local
    /// one stands. The check would have answered `Missing`, so neither verdict
    /// is its.
    #[cfg(unix)]
    #[test]
    fn a_full_cap_judges_the_folder_by_its_volume() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
        IN_FLIGHT.store(super::CHECKS_IN_FLIGHT_MAX, Ordering::SeqCst);
        fn answers(_: &str) -> Option<Fault> {
            Some(Fault::Missing)
        }
        let budget = Duration::from_secs(60);
        assert_eq!(
            start_fault_in(&IN_FLIGHT, budget, "/Volumes/share/x", answers, |_| true),
            Some(Fault::Unresponsive)
        );
        assert_eq!(
            start_fault_in(&IN_FLIGHT, budget, "/Users//me/x", answers, |_| false),
            None
        );
        assert_eq!(
            IN_FLIGHT.load(Ordering::SeqCst),
            super::CHECKS_IN_FLIGHT_MAX,
            "a refused check leaves the count as it found it"
        );
    }

    /// THE VOLUME IS THE LONGEST MOUNT POINT HOLDING THE FOLDER, by whole path
    /// components: a network share under a local root is not local, a local
    /// mount inside a share is, `/Volumes/shared` is not inside
    /// `/Volumes/share`, and a path no mount holds is local.
    #[cfg(unix)]
    #[test]
    fn a_folder_is_judged_by_the_longest_mount_that_holds_it() {
        let table = [
            ("/", true),
            ("/Volumes/share", false),
            ("/Volumes/share/disk", true),
        ];
        let remote = |dir: &str| {
            on_remote_mount(dir, table.iter().map(|(on, local)| (Path::new(on), *local)))
        };
        assert!(remote("/Volumes/share"));
        assert!(remote("/Volumes/share/proj/src"));
        assert!(
            remote("/Volumes/share//proj/"),
            "as written, separators aside"
        );
        assert!(!remote("/Volumes/share/disk/proj"));
        assert!(!remote("/Volumes/shared/proj"));
        assert!(!remote("/Users//me/src"));
        assert!(!on_remote_mount("/Volumes/share/x", []), "no table");
    }

    /// THIS MACHINE'S MOUNT TABLE READS: a scratch folder and the root are on
    /// local volumes, so a folder there that does not answer in time keeps
    /// today's behaviour.
    #[cfg(unix)]
    #[test]
    fn a_scratch_folder_is_on_a_local_volume() {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("aterm-start-volume")
            .tempdir()
            .expect("scratch dir");
        assert!(!remote_volume(scratch.path().to_str().expect("utf-8")));
        assert!(!remote_volume("/"));
        // The table is really read (an empty one would judge every folder
        // local too): the root is in it, local.
        #[cfg(target_os = "macos")]
        assert!(
            super::mount_table()
                .iter()
                .any(|(on, local)| on == Path::new("/") && *local),
            "{:?}",
            super::mount_table()
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
            "/Volumes/hung" => Some(Fault::Unresponsive),
            _ => None,
        };
        let home = Some(Path::new("/Users//me"));
        for (dir, why) in [
            ("/work/gone", Fault::Missing),
            ("/work/shut", Fault::Shut),
            ("/Volumes/hung", Fault::Unresponsive),
        ] {
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
            // A folder that did not answer is never handed to a `chdir`.
            let unasked = start_folder_in(Some("/Volumes/hung"), no_home, fault);
            assert_eq!(
                (unasked.cwd, unasked.fell_back),
                (None, None),
                "{no_home:?}"
            );
        }

        // A failed folder that is the home folder itself is asked about once:
        // a second check could only fail again, after a second wait.
        for (dir, want) in [("/Volumes/hung", None), ("/work/gone", Some("/work/gone"))] {
            let asked = std::cell::Cell::new(0);
            let counted = |at: &str| {
                asked.set(asked.get() + 1);
                fault(at)
            };
            let start = start_folder_in(Some(dir), Some(Path::new(dir)), counted);
            assert_eq!((start.cwd.as_deref(), start.fell_back), (want, None));
            assert_eq!(asked.get(), 1, "{dir}: checked once");
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

    /// A pane's folder read on the event loop and judged later, where the
    /// spawn waits: a held pane's folder that is gone is not taken, one that is
    /// here is, and a folder the pane's own shell reported is taken either way;
    /// of several panes, the first whose folder is taken gives it.
    #[cfg(unix)]
    #[test]
    fn a_pane_folder_read_now_is_judged_when_the_spawn_runs() {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("aterm-pane-folder")
            .tempdir()
            .expect("scratch dir");
        let here = scratch
            .path()
            .to_str()
            .expect("utf-8 scratch path")
            .to_owned();
        let gone = scratch.path().join("gone").to_str().unwrap().to_owned();
        let pane = |reported: &str, held| PaneFolder {
            reported: reported.to_owned(),
            held,
        };
        assert_eq!(pane(&gone, true).resolve(), None, "held, and not here");
        assert_eq!(
            pane(&here, true).resolve(),
            Some(here.clone()),
            "held, here"
        );
        assert_eq!(
            pane(&gone, false).resolve(),
            Some(gone.clone()),
            "the pane's own shell reported it"
        );
        // A connected preset's origin first, then the focused pane: the next
        // pane is asked only when the first's folder is not taken.
        assert_eq!(
            PaneFolder::first_taken([pane(&gone, true), pane(&here, true)]),
            Some(here.clone()),
            "a held origin's missing folder hands over to the focused pane's"
        );
        assert_eq!(
            PaneFolder::first_taken([pane(&gone, false), pane(&here, true)]),
            Some(gone),
            "an origin's own shell's folder is taken"
        );
        assert_eq!(PaneFolder::first_taken([]), None, "no pane, no folder");
    }
}
