// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The folder a fresh shell starts in.

use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::Mutex;
#[cfg(unix)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::time::{Duration, Instant};

/// Why a shell cannot start in a folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    /// Nothing is at that path, what is there is not a folder, or the path cannot name one (a symlink loop, a name too long).
    Missing,
    /// The folder is there, and this user may not enter it.
    Shut,
    /// The folder's check did not answer in time, and the folder is on a volume that is not local: a mount that stopped answering.
    Unresponsive,
    /// As [`Self::Unresponsive`], and the home folder that would stand in for the folder did not answer either (or is the folder, or the PTY's own default for a shell asked to start in none).
    HomeUnresponsive,
}

impl Fault {
    /// Every fault, in the order their rows are posted.
    pub(crate) const ALL: [Self; 4] = [
        Self::Missing,
        Self::Shut,
        Self::Unresponsive,
        Self::HomeUnresponsive,
    ];
}

/// Where a shell starts when neither its folder nor the home folder answers.
pub(crate) const LOCAL_STAND_IN: &str = "/";

/// How long a spawn waits for the folder check. A local folder answers in
/// microseconds; a hung network mount would hold the thread for its whole
/// timeout.
#[cfg(all(unix, not(test)))]
const CHECK_BUDGET: Duration = Duration::from_millis(250);

/// This crate's tests judge local scratch folders, whose check must answer:
/// they wait it as a hang detector, since a loaded parallel run starved a
/// check of 250 ms and read its folder as kept (the restore and spawn suites
/// on the Linux lane).
#[cfg(all(unix, test))]
const CHECK_BUDGET: Duration = aterm_spec::wait::MUST_HAPPEN;

/// The dev seam that sets, in milliseconds, [`check_budget`] for a test of the binary: the folders
/// a live test's instance checks are local scratch too, whose check must answer.
#[cfg(unix)]
const CHECK_BUDGET_SEAM: &str = "ATERM_DEBUG_FOLDER_CHECK_WITHIN_MS";

/// How long a spawn waits for the folder check: [`CHECK_BUDGET`], or what [`CHECK_BUDGET_SEAM`]
/// names, read once.
#[cfg(unix)]
fn check_budget() -> Duration {
    static BUDGET: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *BUDGET.get_or_init(|| crate::fleet_listen::seam_or(CHECK_BUDGET_SEAM, CHECK_BUDGET))
}

/// The most checks left waiting on a hung volume at once: past this, a spawn
/// does not start another and judges its folder as one that did not answer
/// ([`start_fault`]), so a mount that stopped answering costs a bounded number
/// of parked threads.
#[cfg(unix)]
const CHECKS_IN_FLIGHT_MAX: usize = if cfg!(test) { 64 } else { 4 };

#[cfg(unix)]
static CHECKS_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Each check still running, by its folder and when it started. A folder whose
/// check has run past the budget did not answer, and the next spawn asked to
/// start there — the control socket's `spawn cwd=` checks before the spawn
/// does, a person presses New Tab again — is answered at once ([`within`]),
/// instead of waiting out the budget again beside it: half the wait for a
/// `spawn cwd=` into a hung share (515 ms for both checks, measured on the
/// Linux lane), and none for every New Tab after the first.
#[cfg(unix)]
static CHECKS_RUNNING: Mutex<Vec<(String, Instant)>> = Mutex::new(Vec::new());

/// Why a shell cannot start in `dir`, or `None` when it can or when that could not be decided in time (the child's `chdir` then decides).
pub(crate) fn folder_fault(dir: &str) -> Option<Fault> {
    #[cfg(unix)]
    {
        folder_fault_in(
            &CHECKS_IN_FLIGHT,
            check_budget(),
            dir,
            fault_now,
            remote_mount_here(),
        )
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        None
    }
}

/// [`folder_fault`] over its check, its cap and its mount table: not judged,
/// at once, for a folder beside a check that did not answer
/// ([`beside_silent`]).
#[cfg(unix)]
fn folder_fault_in(
    in_flight: &'static AtomicUsize,
    budget: Duration,
    dir: &str,
    check: fn(&str) -> Option<Fault>,
    mount: impl Fn(&str) -> Option<PathBuf>,
) -> Option<Fault> {
    if beside_silent(dir, budget, mount) {
        return None;
    }
    within(in_flight, budget, dir, check).flatten()
}

/// Why a fresh shell cannot start in `dir`.
pub(crate) fn start_fault(dir: &str) -> Option<Fault> {
    #[cfg(unix)]
    {
        start_fault_in(
            &CHECKS_IN_FLIGHT,
            check_budget(),
            dir,
            fault_now,
            remote_mount_here(),
        )
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        None
    }
}

/// [`start_fault`] over its check, its cap and its mount table (`mount`: the
/// mount point holding a folder on a volume that is not local).
#[cfg(unix)]
fn start_fault_in(
    in_flight: &'static AtomicUsize,
    budget: Duration,
    dir: &str,
    check: fn(&str) -> Option<Fault>,
    mount: impl Fn(&str) -> Option<PathBuf>,
) -> Option<Fault> {
    if beside_silent(dir, budget, &mount) {
        return Some(Fault::Unresponsive);
    }
    within(in_flight, budget, dir, check)
        .unwrap_or_else(|| mount(dir).is_some().then_some(Fault::Unresponsive))
}

/// Whether reading `dir` can go ahead without holding its reader on a network volume that stopped answering.
pub(crate) fn volume_answers(dir: &Path) -> bool {
    #[cfg(unix)]
    {
        dir.to_str().is_none_or(|dir| {
            answers_in(
                &CHECKS_IN_FLIGHT,
                check_budget(),
                dir,
                look_now,
                remote_mount_here(),
            )
        })
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        true
    }
}

/// Whether `dir` is on a volume that is not local, by the kernel's mount table ([`remote_volume`]).
pub(crate) fn on_network_volume(dir: &Path) -> bool {
    #[cfg(unix)]
    {
        dir.to_str().is_some_and(remote_volume)
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        false
    }
}

/// [`volume_answers`] over its look, its cap and its mount table.
#[cfg(unix)]
fn answers_in(
    in_flight: &'static AtomicUsize,
    budget: Duration,
    dir: &str,
    look: fn(&str),
    mount: impl Fn(&str) -> Option<PathBuf>,
) -> bool {
    mount(dir).is_none()
        || !(beside_silent(dir, budget, &mount) || within(in_flight, budget, dir, look).is_none())
}

/// [`volume_answers`]'s look without the time limit: what it finds is not the
/// question, only that the volume answered.
#[cfg(unix)]
fn look_now(dir: &str) {
    let _ = std::fs::symlink_metadata(dir);
}

/// Whether `dir` sits on a volume that is not local (`mount` names the mount
/// point holding a folder on one, `None` for a local one) beside a check that
/// has run past `budget`: a check of another folder on that same mount,
/// still running.
#[cfg(unix)]
fn beside_silent(dir: &str, budget: Duration, mount: impl Fn(&str) -> Option<PathBuf>) -> bool {
    let silent: Vec<String> = CHECKS_RUNNING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|(_, since)| since.elapsed() >= budget)
        .map(|(at, _)| at.clone())
        .collect();
    if silent.is_empty() {
        return false;
    }
    let Some(here) = mount(dir) else {
        return false;
    };
    silent.iter().any(|at| mount(at).as_ref() == Some(&here))
}

/// The canonical form of `dir` (`realpath`), or `None` when it has none or when that could not be read in time.
pub(crate) fn canonical_folder(dir: &str) -> Option<String> {
    #[cfg(unix)]
    {
        canonical_in(
            &CHECKS_IN_FLIGHT,
            check_budget(),
            dir,
            canonical_now,
            remote_mount_here(),
        )
    }
    #[cfg(not(unix))]
    {
        canonical_now(dir)
    }
}

/// [`canonical_folder`] over its check, its cap, its budget and its mount
/// table: `None` where the check found no canonical form, did not answer
/// within `budget`, could not start with the cap full, or sits beside a check
/// that did not answer ([`beside_silent`]).
#[cfg(unix)]
fn canonical_in(
    in_flight: &'static AtomicUsize,
    budget: Duration,
    dir: &str,
    check: fn(&str) -> Option<String>,
    mount: impl Fn(&str) -> Option<PathBuf>,
) -> Option<String> {
    if beside_silent(dir, budget, mount) {
        return None;
    }
    within(in_flight, budget, dir, check).flatten()
}

fn canonical_now(dir: &str) -> Option<String> {
    let canonical = std::fs::canonicalize(dir).ok()?;
    let canonical = canonical.to_string_lossy().into_owned();
    #[cfg(windows)]
    let canonical = without_verbatim(&canonical).unwrap_or(canonical);
    Some(canonical)
}

/// `canonical` without the verbatim prefix Windows' `canonicalize` answers
/// with, where the path has a spelling without it: `\\?\C:\x` is `C:\x`, and
/// `\\?\UNC\srv\share\x` is `\\srv\share\x`.
#[cfg(any(windows, test))]
fn without_verbatim(canonical: &str) -> Option<String> {
    let rest = canonical.strip_prefix(r"\\?\")?;
    if let Some(share) = rest.strip_prefix(r"UNC\") {
        return Some(format!(r"\\{share}"));
    }
    matches!(rest.as_bytes(), [drive, b':', ..] if drive.is_ascii_alphabetic())
        .then(|| rest.to_owned())
}

/// Run `check(dir)` on a thread of its own and wait at most `budget` for it:
/// `Some` of its answer, or `None` when it did not answer in time, when a
/// check of `dir` started earlier is still running past `budget`
/// ([`CHECKS_RUNNING`]: it did not answer either), when
/// [`CHECKS_IN_FLIGHT_MAX`] checks counted in `in_flight` are already waiting,
/// or when no thread could be started.
#[cfg(unix)]
fn within<T: Send + 'static>(
    in_flight: &'static AtomicUsize,
    budget: Duration,
    dir: &str,
    check: fn(&str) -> T,
) -> Option<T> {
    let running = || {
        CHECKS_RUNNING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    if running()
        .iter()
        .any(|(at, since)| at == dir && since.elapsed() >= budget)
    {
        return None;
    }
    if in_flight.fetch_add(1, Ordering::AcqRel) >= CHECKS_IN_FLIGHT_MAX {
        in_flight.fetch_sub(1, Ordering::AcqRel);
        return None;
    }
    let started = (dir.to_owned(), Instant::now());
    running().push(started.clone());
    let finished = move |started: &(String, Instant)| {
        let mut running = running();
        if let Some(at) = running.iter().position(|check| check == started) {
            running.swap_remove(at);
        }
        in_flight.fetch_sub(1, Ordering::AcqRel);
    };
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let ran = started.clone();
    let spawned = crate::qos::spawn_lane_thread(
        "aterm-folder-check".to_string(),
        crate::qos::Role::Interactive,
        move || {
            let answer = check(&ran.0);
            finished(&ran);
            let _ = tx.send(answer);
        },
    );
    if spawned.is_err() {
        finished(&started);
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
    // SAFETY: `path` is a NUL-terminated string that outlives the call; `access` only reads it.
    if unsafe { libc::access(path.as_ptr(), libc::X_OK) } == 0 {
        return None;
    }
    judged(&std::io::Error::last_os_error())
}

/// Whether `dir` is on a volume that is not local, by the kernel's mount
/// table ([`mount_table`]) read through this machine's path aliases
/// ([`Aliases::here`]) — the one question about a folder whose check hangs
/// that cannot hang too.
#[cfg(unix)]
fn remote_volume(dir: &str) -> bool {
    remote_mount_here()(dir).is_some()
}

/// The mount point holding a folder on a volume that is not local, by this
/// machine's mount table ([`mount_table`], read at the first ask and kept for
/// the rest of the closure's life) and path aliases ([`remote_mount_of`]).
#[cfg(unix)]
fn remote_mount_here() -> impl Fn(&str) -> Option<PathBuf> {
    let table = std::cell::OnceCell::new();
    move |dir| {
        let table: &Vec<(PathBuf, bool)> = table.get_or_init(mount_table);
        remote_mount_of(
            dir,
            table.iter().map(|(on, local)| (on.as_path(), *local)),
            Aliases::here(),
        )
    }
}

/// PURE: the mount point, spelled one way by `aliases`, of the mount that
/// holds `dir` — the longest mount point that is a whole-component prefix of
/// it, both so spelled, and of equal ones the last listed, the one mounted on
/// top — when that mount is not local; `None` for a folder on a local one.
#[cfg(unix)]
fn remote_mount_of<'a>(
    dir: &str,
    mounts: impl IntoIterator<Item = (&'a Path, bool)>,
    aliases: &Aliases,
) -> Option<PathBuf> {
    let dir = aliases.spelled(Path::new(dir));
    mounts
        .into_iter()
        .map(|(on, local)| (aliases.spelled(on), local))
        .filter(|(on, _)| dir.starts_with(on))
        .max_by_key(|(on, _)| on.components().count())
        .and_then(|(on, local)| (!local).then_some(on))
}

/// The path aliases macOS fixes at boot, so a folder and the mount that holds it are compared spelled one way, without asking any volume.
#[cfg(unix)]
#[derive(Debug, Default)]
pub(crate) struct Aliases {
    /// `/<name>` → its target, absolute.
    links: Vec<(PathBuf, PathBuf)>,
    /// A firmlink's path → the same folder on the data volume.
    firmlinks: Vec<(PathBuf, PathBuf)>,
}

/// Where macOS mounts the data volume the firmlinks point into.
#[cfg(unix)]
const DATA_VOLUME: &str = "/System/Volumes/Data";

/// How many root links one spelling follows: a synthetic link can point
/// through another (`/home`), never round in a loop the kernel would refuse.
#[cfg(unix)]
const LINK_HOPS: usize = 4;

#[cfg(unix)]
impl Aliases {
    fn of(links: impl IntoIterator<Item = (PathBuf, PathBuf)>, firmlinks: &str) -> Self {
        Self {
            links: links
                .into_iter()
                .map(|(at, to)| (at, Path::new("/").join(to)))
                .collect(),
            firmlinks: firmlinks
                .lines()
                .filter_map(|line| line.split_once('\t'))
                .filter(|(at, _)| at.starts_with('/'))
                .map(|(at, to)| (PathBuf::from(at), Path::new(DATA_VOLUME).join(to.trim())))
                .collect(),
        }
    }

    fn spelled(&self, path: &Path) -> PathBuf {
        let mut path = path.to_path_buf();
        for _ in 0..LINK_HOPS {
            match Self::through(&self.links, &path) {
                Some(next) => path = next,
                None => break,
            }
        }
        Self::through(&self.firmlinks, &path).unwrap_or(path)
    }

    fn through(aliases: &[(PathBuf, PathBuf)], path: &Path) -> Option<PathBuf> {
        aliases
            .iter()
            .filter_map(|(at, to)| path.strip_prefix(at).ok().map(|rest| (at, to, rest)))
            .max_by_key(|(at, _, _)| at.components().count())
            .map(|(_, to, rest)| {
                if rest.as_os_str().is_empty() {
                    to.clone()
                } else {
                    to.join(rest)
                }
            })
    }

    fn here() -> &'static Self {
        static HERE: std::sync::OnceLock<Aliases> = std::sync::OnceLock::new();
        HERE.get_or_init(|| {
            let firmlinks = if cfg!(target_os = "macos") {
                aterm_uds::atomic_file::read_regular_text(
                    Path::new("/usr/share/firmlinks"),
                    64 * 1024,
                )
                .unwrap_or_default()
            } else {
                String::new()
            };
            Self::of(root_links(), &firmlinks)
        })
    }
}

/// Read this machine's path aliases ([`Aliases::here`]) now, on the calling thread.
pub(crate) fn warm() {
    #[cfg(unix)]
    let _ = Aliases::here();
}

/// The root's symlinks and their targets as read: entries of `/`, the root
/// volume (sealed on macOS), each typed from its directory record, so nothing
/// here asks a network volume mounted on one of them.
#[cfg(unix)]
fn root_links() -> Vec<(PathBuf, PathBuf)> {
    let Ok(entries) = std::fs::read_dir("/") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_symlink()))
        .filter_map(|entry| {
            let at = entry.path();
            std::fs::read_link(&at).ok().map(|to| (at, to))
        })
        .collect()
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    #[cfg_attr(target_arch = "x86_64", link_name = "getfsstat$INODE64")]
    fn getfsstat(buf: *mut libc::statfs, bufsize: libc::c_int, mode: libc::c_int) -> libc::c_int;
}

/// Every mount point, and whether its volume is local (`MNT_LOCAL`), as the
/// kernel last recorded them: `MNT_NOWAIT` asks no file system to refresh its
/// numbers, so a hung mount is listed without being touched.
#[cfg(target_os = "macos")]
fn mount_table() -> Vec<(std::path::PathBuf, bool)> {
    use std::os::unix::ffi::OsStringExt as _;
    const MNT_NOWAIT: libc::c_int = 2;
    const MNT_LOCAL: u32 = 0x0000_1000;
    // SAFETY: a null buffer asks only for the count; nothing is written.
    let Ok(count) = usize::try_from(unsafe { getfsstat(std::ptr::null_mut(), 0, MNT_NOWAIT) })
    else {
        return Vec::new();
    };
    // SAFETY: `statfs` is integers and arrays of them: all-zero is a value.
    let blank: libc::statfs = unsafe { std::mem::zeroed() };
    let mut table = vec![blank; count + 4];
    let Ok(bytes) = libc::c_int::try_from(std::mem::size_of_val(table.as_slice())) else {
        return Vec::new();
    };
    // SAFETY: `table` is `bytes` writable bytes of whole `statfs` records, and getfsstat writes at most that many, answering how many it wrote.
    let written = unsafe { getfsstat(table.as_mut_ptr(), bytes, MNT_NOWAIT) };
    let Ok(written) = usize::try_from(written) else {
        return Vec::new();
    };
    table
        .iter()
        .take(written)
        .map(|record| {
            let on = record.f_mntonname.iter().take_while(|&&c| c != 0);
            let on: Vec<u8> = on.map(|&c| c.to_ne_bytes()[0]).collect();
            (
                std::ffi::OsString::from_vec(on).into(),
                record.f_flags & MNT_LOCAL != 0,
            )
        })
        .collect()
}

/// Every mount point of this process's mount namespace, and whether its volume
/// is local, from `/proc/self/mountinfo` as the machine's one mount-table reader
/// reads it ([`aterm_sysprobe::volumes::table`]): the kernel prints it from its own
/// records, asking no file system, so a hung share is listed without being
/// touched.
#[cfg(target_os = "linux")]
fn mount_table() -> Vec<(std::path::PathBuf, bool)> {
    judged(aterm_sysprobe::volumes::table())
}

/// No mount table is read on another Unix: every folder is judged local, and a
/// check that does not answer keeps its folder.
#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn mount_table() -> Vec<(std::path::PathBuf, bool)> {
    Vec::new()
}

/// PURE: `table`'s mount points, each with whether its volume is local. A table
/// that is not whole — cut at the reader's cap, or holding a line it could not
/// read — proves no folder local: a share mounted after the cut is not in it,
/// and the mount listed below it would judge a folder on it local.
#[cfg(any(target_os = "linux", all(test, unix)))]
fn judged(table: aterm_sysprobe::volumes::Table) -> Vec<(PathBuf, bool)> {
    let whole = table.complete;
    table
        .mounts
        .into_iter()
        .map(|mount| (mount.on, whole && mount.local))
        .collect()
}

/// Where a fresh shell starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StartFolder {
    pub(crate) cwd: Option<String>,
    pub(crate) fell_back: Option<(String, Fault)>,
}

impl StartFolder {
    /// Once the shell is up: log the stand-in and note the folder for the next park's row.
    pub(crate) fn say(self) {
        if let Some((dir, fault)) = self.fell_back {
            crate::logging::stderr_line!(
                "aterm-gui: {dir} {}; the shell started in {}",
                match fault {
                    Fault::Missing => "was not found",
                    Fault::Shut => "could not be opened",
                    Fault::Unresponsive | Fault::HomeUnresponsive => "did not answer",
                },
                match fault {
                    Fault::HomeUnresponsive => LOCAL_STAND_IN,
                    _ => "the home folder",
                }
            );
            note(dir, fault);
        }
    }
}

/// [`start_folder_in`] against the filesystem, the person's home folder, the PTY's own default ([`aterm_pty::default_start_folder`]) and the mount table.
pub(crate) fn start_folder(wanted: Option<&str>) -> StartFolder {
    let home = aterm_types::dirs::home_dir();
    #[cfg(unix)]
    let unasked = aterm_pty::default_start_folder();
    #[cfg(unix)]
    let unasked = unasked.as_deref().and_then(std::ffi::OsStr::to_str);
    #[cfg(not(unix))]
    let unasked: Option<&str> = None;
    #[cfg(unix)]
    let remote = remote_volume;
    #[cfg(not(unix))]
    let remote = |_: &str| false;
    start_folder_in(wanted, home.as_deref(), unasked, start_fault, remote)
}

/// Where a shell asked to start in `wanted` starts, PURE over `fault` and `remote` (whether a folder is on a volume that is not local).
pub(crate) fn start_folder_in(
    wanted: Option<&str>,
    home: Option<&Path>,
    unasked: Option<&str>,
    fault: impl Fn(&str) -> Option<Fault>,
    remote: impl Fn(&str) -> bool,
) -> StartFolder {
    let unasked_start = |named: Option<&str>, known: &[(&str, Option<Fault>)]| {
        let Some(at) = unasked else {
            return StartFolder {
                cwd: None,
                fell_back: None,
            };
        };
        let verdict = match known.iter().find(|(dir, _)| *dir == at) {
            Some((_, verdict)) => *verdict,
            None => remote(at).then(|| fault(at)).flatten(),
        };
        if verdict == Some(Fault::Unresponsive) {
            StartFolder {
                cwd: Some(LOCAL_STAND_IN.to_owned()),
                fell_back: Some((named.unwrap_or(at).to_owned(), Fault::HomeUnresponsive)),
            }
        } else {
            StartFolder {
                cwd: Some(at.to_owned()),
                fell_back: None,
            }
        }
    };
    let Some(dir) = wanted.filter(|dir| !dir.is_empty()) else {
        return unasked_start(None, &[]);
    };
    let stands = StartFolder {
        cwd: Some(dir.to_owned()),
        fell_back: None,
    };
    let Some(why) = fault(dir) else {
        return stands;
    };
    let home = home.and_then(Path::to_str);
    let home_fault = home.map(|home| if home == dir { Some(why) } else { fault(home) });
    match (home, home_fault) {
        (Some(home), Some(None)) => StartFolder {
            cwd: Some(home.to_owned()),
            fell_back: Some((dir.to_owned(), why)),
        },
        (home, home_fault) if why == Fault::Unresponsive => {
            let mut known = vec![(dir, Some(why))];
            known.extend(home.zip(home_fault));
            unasked_start(Some(dir), &known)
        }
        _ => stands,
    }
}

/// The folder a new tab, split or window takes from a pane, and a restored pane from its saved one ([`crate::restore::TerminalLeafRestore::start_cwd`]).
pub(crate) fn inherited(reported: String, held: impl FnOnce() -> bool) -> Option<String> {
    if cfg!(unix) {
        inherited_in(reported, held, folder_fault)
    } else {
        Some(reported)
    }
}

/// Snapshot of a pane's reported folder and foreground process group for [`inherited`] to check off the event loop (`tab_spawn`).
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

    /// The folder the first of `panes`, in order, gives a spawn ([`Self::resolve`]).
    pub(crate) fn first_taken(panes: impl IntoIterator<Item = Self>) -> Option<String> {
        panes.into_iter().find_map(Self::resolve)
    }
}

/// [`inherited`], pure over `fault`.
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

/// The parked spare's retarget.
pub(crate) fn retarget_folder(dir: &str) -> Option<String> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        retarget_folder_in(dir, remote_volume, |dir| {
            within(&CHECKS_IN_FLIGHT, check_budget(), dir, retarget_now).flatten()
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = dir;
        None
    }
}

/// PURE: [`retarget_folder`] over `remote` ([`remote_volume`]) and `read`
/// (the bounded read of the kernel path, [`retarget_now`]): nothing for a
/// folder judged not local, and nothing for one whose kernel path is — a
/// person's own symlink into a network volume is judged local by the folder
/// it is in, and only the read, bounded, follows it there.
#[cfg(any(target_os = "macos", target_os = "linux", test))]
fn retarget_folder_in(
    dir: &str,
    remote: impl Fn(&str) -> bool,
    read: impl FnOnce(&str) -> Option<String>,
) -> Option<String> {
    if remote(dir) {
        return None;
    }
    read(dir).filter(|kernel| !remote(kernel))
}

/// [`retarget_folder`] without the time limit, against this process's own
/// `HOME` and `PWD`, which every shell it spawns inherits (`aterm_pty` sets
/// neither).
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn retarget_now(dir: &str) -> Option<String> {
    let kernel = kernel_path(dir)?;
    let names = |alias: &std::ffi::OsStr| {
        use std::os::unix::fs::MetadataExt as _;
        match (std::fs::metadata(alias), std::fs::metadata(dir)) {
            (Ok(a), Ok(d)) => (a.dev(), a.ino()) == (d.dev(), d.ino()),
            _ => false,
        }
    };
    let home = std::env::var_os("HOME");
    let pwd = std::env::var_os("PWD");
    retarget_in(&kernel, names, [home.as_deref(), pwd.as_deref()])
}

/// [`retarget_folder`]'s rule over the folder's kernel path, `names` (whether a path names the folder) and the inherited `$HOME` and `$PWD`.
#[cfg(any(target_os = "macos", target_os = "linux", test))]
pub(crate) fn retarget_in(
    kernel: &str,
    names: impl Fn(&std::ffi::OsStr) -> bool,
    aliases: [Option<&std::ffi::OsStr>; 2],
) -> Option<String> {
    if !kernel.starts_with('/') || kernel.contains('\n') {
        return None;
    }
    let spelled_otherwise = aliases
        .into_iter()
        .flatten()
        .any(|alias| alias != std::ffi::OsStr::new(kernel) && names(alias));
    (!spelled_otherwise).then(|| kernel.to_owned())
}

/// The path the kernel records for folder `dir` — what `getcwd()` answers
/// there: through a firmlink (`/Users`, not `/System/Volumes/Data/Users`),
/// with every symlink resolved and the names' own case — from
/// `getattrlist(ATTR_CMN_FULLPATH)`, a metadata read like `stat`.
#[cfg(target_os = "macos")]
fn kernel_path(dir: &str) -> Option<String> {
    #[repr(C)]
    struct AttrList {
        bitmapcount: u16,
        reserved: u16,
        commonattr: u32,
        volattr: u32,
        dirattr: u32,
        fileattr: u32,
        forkattr: u32,
    }
    #[repr(C)]
    struct Reply {
        length: u32,
        offset: i32,
        size: u32,
        path: [u8; PATH_BYTES],
    }
    const PATH_BYTES: usize = 1024 + 8;
    const ATTR_BIT_MAP_COUNT: u16 = 5;
    const ATTR_CMN_FULLPATH: u32 = 0x0800_0000;
    unsafe extern "C" {
        fn getattrlist(
            path: *const libc::c_char,
            list: *mut AttrList,
            buf: *mut Reply,
            size: libc::size_t,
            options: u32,
        ) -> libc::c_int;
    }
    let path = std::ffi::CString::new(dir).ok()?;
    let mut list = AttrList {
        bitmapcount: ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: ATTR_CMN_FULLPATH,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut reply = Reply {
        length: 0,
        offset: 0,
        size: 0,
        path: [0; PATH_BYTES],
    };
    // SAFETY: `path` is NUL-terminated and outlives the call; `list` and
    // `reply` are live, writable and sized as passed; the kernel writes at
    // most `size_of::<Reply>()` bytes into `reply`.
    let rc = unsafe {
        getattrlist(
            path.as_ptr(),
            &raw mut list,
            &raw mut reply,
            std::mem::size_of::<Reply>(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    let start = usize::try_from(reply.offset).ok()?.checked_sub(8)?;
    let bytes = reply.path;
    let text = bytes.get(start..)?;
    let len = text.iter().position(|&b| b == 0)?;
    String::from_utf8(text.get(..len)?.to_vec()).ok()
}

/// See the macOS twin. Linux: the path the kernel names for the folder's own
/// entry, the one `getcwd()` names there (both are `d_path` of it: every
/// symlink resolved, through this process's mounts), read back from a
/// descriptor opened `O_PATH`, which reads nothing of the folder and needs no
/// permission on it.
#[cfg(target_os = "linux")]
fn kernel_path(dir: &str) -> Option<String> {
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::os::unix::io::AsRawFd as _;
    let folder = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(dir)
        .ok()?;
    let named = std::fs::read_link(format!("/proc/self/fd/{}", folder.as_raw_fd())).ok()?;
    let named = named.into_os_string().into_string().ok()?;
    (named.starts_with('/') && !named.ends_with(" (deleted)")).then_some(named)
}

static NOTED_PENDING: AtomicBool = AtomicBool::new(false);
static NOTED: Mutex<Vec<(String, Fault)>> = Mutex::new(Vec::new());

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

/// The folders one fault's row names while it is up.
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

/// [`Named`] for each [`Fault`]'s row: the state `App::fold_folder_faults` keeps between parks.
#[derive(Clone, Debug, Default)]
pub(crate) struct RowFolders {
    missing: Named,
    shut: Named,
    unresponsive: Named,
    home_unresponsive: Named,
}

impl RowFolders {
    pub(crate) const fn of(&mut self, fault: Fault) -> &mut Named {
        match fault {
            Fault::Missing => &mut self.missing,
            Fault::Shut => &mut self.shut,
            Fault::Unresponsive => &mut self.unresponsive,
            Fault::HomeUnresponsive => &mut self.home_unresponsive,
        }
    }
}

#[cfg(test)]
#[path = "spawn_folder_tests.rs"]
mod tests;
