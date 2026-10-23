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
            CHECK_BUDGET,
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

/// Why a fresh shell cannot start in `dir`: [`folder_fault`], except that a
/// check that did not answer in time — or was not started, the cap being
/// reached, or not needed, another folder's check on the same volume having
/// run past the budget ([`beside_silent`]) — is [`Fault::Unresponsive`] for a
/// folder on a volume that is not local ([`remote_volume`]).
pub(crate) fn start_fault(dir: &str) -> Option<Fault> {
    #[cfg(unix)]
    {
        start_fault_in(
            &CHECKS_IN_FLIGHT,
            CHECK_BUDGET,
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
                CHECK_BUDGET,
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
            CHECK_BUDGET,
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

/// [`canonical_folder`] without the time limit. On Windows it is spelled
/// without the verbatim prefix `canonicalize` answers with
/// (`without_verbatim`): the harness compares it with the folder Claude
/// Code's folder-trust dialog shows, `C:\…`, never `\\?\C:\…`.
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

/// THE PATH ALIASES MACOS FIXES AT BOOT, so a folder and the mount that holds it are compared spelled one way, without asking any volume.
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
    /// PURE: the aliases named by the root's symlinks (`links`: a name at `/`
    /// and its target as read, relative ones relative to `/`) and the text of
    /// `/usr/share/firmlinks` (`firmlinks`: a path, a tab, the same folder
    /// relative to the data volume, per line).
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

    /// PURE: `path` spelled one way: root links followed (up to [`LINK_HOPS`]),
    /// then a firmlink's path spelled on the data volume.
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

    /// `path` with the longest of `aliases` it starts with (by whole
    /// components) replaced by that alias's target; `None` when it starts with
    /// none.
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

    /// This machine's aliases, read once (the launch reads them ahead,
    /// [`warm`]).
    fn here() -> &'static Self {
        static HERE: std::sync::OnceLock<Aliases> = std::sync::OnceLock::new();
        HERE.get_or_init(|| {
            let firmlinks = if cfg!(target_os = "macos") {
                std::fs::read_to_string("/usr/share/firmlinks").unwrap_or_default()
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

/// THE PARKED SPARE'S RETARGET: the kernel path of `dir` for a parked spare
/// shell to move to, or `None` where it cannot: a folder judged not local, a
/// read that ran past `CHECK_BUDGET`, or a folder the inherited `$HOME` or
/// `$PWD` spells otherwise (`retarget_in`). `None` on other platforms.
pub(crate) fn retarget_folder(dir: &str) -> Option<String> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        retarget_folder_in(dir, remote_volume, |dir| {
            within(&CHECKS_IN_FLIGHT, CHECK_BUDGET, dir, retarget_now).flatten()
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

/// PURE: [`retarget_folder`]'s rule over the folder's kernel path, `names` (whether a path names the folder) and the inherited `$HOME` and `$PWD`.
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
/// The folders noted since the last park, oldest first, each once. Every
/// entry is a shell that was spawned, so the list is as long as the person's
/// own new tabs, and the park empties it.
static NOTED: Mutex<Vec<(String, Fault)>> = Mutex::new(Vec::new());

/// Note a folder a shell was asked to start in and could not, for the next
/// park's row.
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
mod tests {
    #[cfg(unix)]
    use super::{
        Aliases, PaneFolder, canonical_in, remote_mount_of, remote_volume, start_fault_in, within,
    };
    use super::{
        Fault, LOCAL_STAND_IN, StartFolder, inherited_in, retarget_folder_in, retarget_in,
        start_folder_in, take_noted,
    };
    use super::{canonical_now, without_verbatim};
    use std::path::Path;
    #[cfg(unix)]
    use std::path::PathBuf;
    #[cfg(unix)]
    use std::time::{Duration, Instant};

    /// A WORKER'S TRUSTED FOLDER is spelled as Claude Code's folder-trust
    /// dialog spells it: Windows' verbatim `\\?\` prefix comes off a drive's
    /// path and a share's; a path with no other spelling keeps it, and any
    /// other path is not one this rewrites.
    #[test]
    fn a_canonical_folder_is_spelled_without_the_verbatim_prefix() {
        for (verbatim, plain) in [
            (r"\\?\C:\Users\x\proj", r"C:\Users\x\proj"),
            (r"\\?\c:\", r"c:\"),
            (r"\\?\UNC\srv\share\x", r"\\srv\share\x"),
        ] {
            assert_eq!(without_verbatim(verbatim).as_deref(), Some(plain));
        }
        for kept in [
            r"\\?\Volume{0b1c2d3e}\x",
            r"\\?\",
            r"C:\Users\x",
            r"\\srv\share\x",
            "/Users//x",
        ] {
            assert_eq!(without_verbatim(kept), None, "{kept}");
        }
        let here = std::env::temp_dir();
        let canonical = canonical_now(&here.to_string_lossy()).expect("the temp folder");
        assert!(!canonical.starts_with(r"\\?\"), "{canonical}");
        assert!(Path::new(&canonical).is_absolute(), "{canonical}");
        assert_eq!(canonical_now(&canonical).as_deref(), Some(&*canonical));
    }

    /// Each way a folder is Missing or Shut; the scratch folder itself is the
    /// negative control.
    #[cfg(unix)]
    #[test]
    fn each_folder_a_shell_cannot_start_in_is_judged_and_a_folder_is_not() {
        use super::fault_now;
        use std::os::unix::fs::PermissionsExt as _;
        use std::sync::atomic::AtomicUsize;
        /// This test's own in-flight count: the process-wide one is shared
        /// with every test that spawns a shell in a folder.
        static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
        let folder_fault =
            |dir: &str| within(&IN_FLIGHT, Duration::from_secs(60), dir, fault_now).flatten();
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
        assert_eq!(folder_fault(&at("shut")), None, "the same folder, opened");
        aterm_uds::host::could_not_run_as_root("a folder with no search permission");
        assert_eq!(
            shut,
            Some(Fault::Shut),
            "a folder with no search permission"
        );
    }

    /// A CHECK THAT DOES NOT ANSWER KEEPS A LOCAL FOLDER: `within` gives up on
    /// a check still running, so a hung volume costs the wait, never the event
    /// loop, and a spawn keeps a folder on a local volume.
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
            let _ = bell
                .wait_timeout_while(open, Duration::from_secs(60), |open| !*open)
                .unwrap_or_else(PoisonError::into_inner);
            Some(Fault::Missing)
        }
        fn answers(_: &str) -> Option<Fault> {
            Some(Fault::Missing)
        }
        assert_eq!(
            within(&IN_FLIGHT, Duration::from_millis(50), "/x", hangs),
            None
        );
        let budget = Duration::from_millis(50);
        assert_eq!(
            start_fault_in(&IN_FLIGHT, budget, "/x", hangs, |_| None),
            None,
            "a local folder that did not answer stands"
        );
        assert_eq!(
            start_fault_in(&IN_FLIGHT, budget, "/x", hangs, |_| {
                Some(PathBuf::from("/aterm-test/budget-share"))
            }),
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
            start_fault_in(&IN_FLIGHT, Duration::from_secs(60), "/x", answers, |_| {
                Some(PathBuf::from("/aterm-test/budget-share"))
            }),
            Some(Fault::Missing),
            "an answer wins over the volume"
        );
    }

    /// WITH THE CAP FULL NO CHECK STARTS, and the folder is judged as one that
    /// did not answer: a folder on a volume that is not local is
    /// `Unresponsive`, never handed to the child's `chdir` unchecked; a local
    /// one stands.
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
            start_fault_in(&IN_FLIGHT, budget, "/Volumes/share/x", answers, |_| {
                Some(PathBuf::from("/aterm-test/cap-share"))
            }),
            Some(Fault::Unresponsive)
        );
        assert_eq!(
            start_fault_in(&IN_FLIGHT, budget, "/Users//me/x", answers, |_| None),
            None
        );
        assert_eq!(
            IN_FLIGHT.load(Ordering::SeqCst),
            super::CHECKS_IN_FLIGHT_MAX,
            "a refused check leaves the count as it found it"
        );
    }

    /// A FOLDER WHOSE CHECK IS STILL RUNNING PAST THE BUDGET IS ANSWERED AT
    /// ONCE: the next spawn there starts no second check to wait out beside the
    /// first (the control socket's `spawn cwd=` checked twice, a New Tab again
    /// each time), and judges the folder as one that did not answer.
    #[cfg(unix)]
    #[test]
    fn a_folder_whose_check_has_not_answered_is_not_checked_again_beside_it() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Condvar, Mutex, PoisonError};
        static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
        static GATE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
        fn hangs(_: &str) -> Option<Fault> {
            let (open, bell) = &GATE;
            let open = open.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = bell
                .wait_timeout_while(open, Duration::from_secs(60), |open| !*open)
                .unwrap_or_else(PoisonError::into_inner);
            Some(Fault::Missing)
        }
        let budget = Duration::from_millis(50);
        let hung = "/aterm-test/hung-share/proj";
        assert_eq!(within(&IN_FLIGHT, budget, hung, hangs), None);
        assert_eq!(IN_FLIGHT.load(Ordering::SeqCst), 1, "the first check waits");
        assert_eq!(
            start_fault_in(&IN_FLIGHT, budget, hung, hangs, |_| {
                Some(PathBuf::from("/aterm-test/hung-share"))
            }),
            Some(Fault::Unresponsive),
            "judged as a folder that did not answer"
        );
        assert_eq!(
            IN_FLIGHT.load(Ordering::SeqCst),
            1,
            "no second check of the same folder"
        );
        assert_eq!(
            within(&IN_FLIGHT, budget, "/aterm-test/hung-share/other", hangs),
            None
        );
        assert_eq!(IN_FLIGHT.load(Ordering::SeqCst), 2, "another folder's own");
        *GATE.0.lock().unwrap_or_else(PoisonError::into_inner) = true;
        GATE.1.notify_all();
        let opened = Instant::now();
        while IN_FLIGHT.load(Ordering::SeqCst) > 0 {
            assert!(
                opened.elapsed() < Duration::from_secs(60),
                "the opened checks never ended"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            super::CHECKS_RUNNING
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .all(|(at, _)| !at.starts_with("/aterm-test/hung-share/")),
            "an ended check leaves no record"
        );
    }

    /// A CHECK PAST THE BUDGET ANSWERS FOR ITS WHOLE VOLUME WHEN THAT VOLUME IS
    /// NOT LOCAL: once one folder's check on a network share has run past the
    /// budget, every other folder there is judged at once — a spawn's as one
    /// that did not answer, the restore's count and `spawn cwd=`'s refusal as
    /// not judged, a worker's folder as not trusted, the identities' folder as
    /// not answering — and no check starts beside it.
    #[cfg(unix)]
    #[test]
    fn a_check_past_the_budget_answers_for_its_whole_volume_when_it_is_not_local() {
        use super::{answers_in, beside_silent, folder_fault_in, remote_mount_of};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Condvar, Mutex, PoisonError};
        static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
        static GATE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
        fn gate() {
            let (open, bell) = &GATE;
            let open = open.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = bell
                .wait_timeout_while(open, Duration::from_secs(60), |open| !*open)
                .unwrap_or_else(PoisonError::into_inner);
        }
        fn hangs(_: &str) -> Option<Fault> {
            gate();
            Some(Fault::Missing)
        }
        fn hangs_reading(_: &str) -> Option<String> {
            gate();
            Some("/aterm-test/silent-share/second".to_string())
        }
        fn hangs_looking(_: &str) {
            gate();
        }
        let table: Vec<(PathBuf, bool)> = vec![
            (PathBuf::from("/"), true),
            (PathBuf::from("/aterm-test/silent-share"), false),
            (PathBuf::from("/aterm-test/live-share"), false),
        ];
        let mount = |dir: &str| {
            remote_mount_of(
                dir,
                table.iter().map(|(on, local)| (on.as_path(), *local)),
                &Aliases::default(),
            )
        };
        let budget = Duration::from_millis(50);
        assert_eq!(
            within(&IN_FLIGHT, budget, "/aterm-test/silent-share/first", hangs),
            None
        );
        assert_eq!(IN_FLIGHT.load(Ordering::SeqCst), 1, "the first check waits");
        let beside = "/aterm-test/silent-share/second";
        assert_eq!(
            start_fault_in(&IN_FLIGHT, budget, beside, hangs, mount),
            Some(Fault::Unresponsive),
            "a spawn there starts in the home folder"
        );
        assert_eq!(
            folder_fault_in(&IN_FLIGHT, budget, beside, hangs, mount),
            None
        );
        assert_eq!(
            canonical_in(&IN_FLIGHT, budget, beside, hangs_reading, mount),
            None
        );
        assert!(!answers_in(
            &IN_FLIGHT,
            budget,
            beside,
            hangs_looking,
            mount
        ));
        assert_eq!(
            IN_FLIGHT.load(Ordering::SeqCst),
            1,
            "no check started beside the silent one"
        );
        assert!(beside_silent(beside, budget, mount));
        assert!(
            !beside_silent("/aterm-test/live-share/x", budget, mount),
            "another share is not judged by it"
        );
        assert!(
            !beside_silent("/aterm-test/local/x", budget, mount),
            "nor is a local folder"
        );
        *GATE.0.lock().unwrap_or_else(PoisonError::into_inner) = true;
        GATE.1.notify_all();
        let opened = Instant::now();
        while IN_FLIGHT.load(Ordering::SeqCst) > 0 {
            assert!(
                opened.elapsed() < Duration::from_secs(60),
                "the opened check never ended"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            !beside_silent(beside, budget, mount),
            "with the silent check ended, its share is checked again"
        );
    }

    /// A VOLUME ANSWERS BY A LOOK IN TIME, AND A LOCAL ONE IS READ ANYWAY
    /// (`volume_answers`, the state folder): a look that answers —
    /// whatever it found — says a network volume answers; one that does not
    /// answer in time says it does not, and the same question again is
    /// answered at once, with no second look; a local volume is read with no
    /// look made, since its silence would be a starved CPU's.
    #[cfg(unix)]
    #[test]
    fn a_volume_answers_by_a_look_in_time_and_a_local_one_is_read_anyway() {
        use super::{answers_in, remote_mount_of};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Condvar, Mutex, PoisonError};
        static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
        static GATE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
        fn hangs(_: &str) {
            let (open, bell) = &GATE;
            let open = open.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = bell
                .wait_timeout_while(open, Duration::from_secs(60), |open| !*open)
                .unwrap_or_else(PoisonError::into_inner);
        }
        let table: Vec<(PathBuf, bool)> = vec![
            (PathBuf::from("/"), true),
            (PathBuf::from("/aterm-test/ids-share"), false),
        ];
        let mount = |dir: &str| {
            remote_mount_of(
                dir,
                table.iter().map(|(on, local)| (on.as_path(), *local)),
                &Aliases::default(),
            )
        };
        let minute = Duration::from_secs(60);
        let budget = Duration::from_millis(50);
        let shared = "/aterm-test/ids-share/state/identities";
        assert!(
            answers_in(&IN_FLIGHT, minute, shared, |_| {}, mount),
            "a look that answers"
        );
        assert!(
            !answers_in(&IN_FLIGHT, budget, shared, hangs, mount),
            "a silent look on a volume that is not local"
        );
        let looking = IN_FLIGHT.load(Ordering::SeqCst);
        assert!(
            answers_in(
                &IN_FLIGHT,
                budget,
                "/aterm-test/ids-local/identities",
                hangs,
                mount
            ),
            "a local volume is read anyway"
        );
        assert_eq!(
            IN_FLIGHT.load(Ordering::SeqCst),
            looking,
            "with no look made"
        );
        assert!(!answers_in(&IN_FLIGHT, budget, shared, hangs, mount));
        assert_eq!(
            IN_FLIGHT.load(Ordering::SeqCst),
            looking,
            "asked again: answered at once, no second look"
        );
        *GATE.0.lock().unwrap_or_else(PoisonError::into_inner) = true;
        GATE.1.notify_all();
        let opened = Instant::now();
        while IN_FLIGHT.load(Ordering::SeqCst) > 0 {
            assert!(
                opened.elapsed() < Duration::from_secs(60),
                "the opened looks never ended"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// A WORKER'S FOLDER IS TRUSTED ONLY AS READ IN TIME: its canonical form
    /// read past the budget, or not read at all with the cap full, is `None`
    /// (`spawn agent= cwd=` then trusts nothing, and the harness asks), where
    /// the check would have answered a folder.
    #[cfg(unix)]
    #[test]
    fn a_workers_folder_read_late_or_never_trusts_nothing() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::{Condvar, Mutex, PoisonError};

        /// This test's own in-flight counts (the process-wide one is shared
        /// with every test that spawns a shell in a folder).
        static FULL: AtomicUsize = AtomicUsize::new(super::CHECKS_IN_FLIGHT_MAX);
        static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
        /// The hung check has started (so its `None` is the budget's, not the
        /// cap's), and the gate it waits behind until the verdict.
        static ENTERED: AtomicBool = AtomicBool::new(false);
        static GATE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
        fn answers(_: &str) -> Option<String> {
            Some("/w/proj".to_string())
        }
        fn hangs(dir: &str) -> Option<String> {
            ENTERED.store(true, Ordering::SeqCst);
            let (open, bell) = &GATE;
            let open = open.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = bell
                .wait_timeout_while(open, Duration::from_secs(60), |open| !*open)
                .unwrap_or_else(PoisonError::into_inner);
            answers(dir)
        }
        let minute = Duration::from_secs(60);
        assert_eq!(
            canonical_in(&FULL, minute, "/w/proj", answers, |_| None),
            None,
            "the cap full"
        );
        assert_eq!(
            canonical_in(
                &IN_FLIGHT,
                Duration::from_millis(50),
                "/w/proj",
                hangs,
                |_| None
            ),
            None,
            "read late"
        );
        let started = Instant::now();
        while !ENTERED.load(Ordering::SeqCst) {
            assert!(
                started.elapsed() < minute,
                "the hung check never started: the cap, not the budget, said None"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        *GATE.0.lock().unwrap_or_else(PoisonError::into_inner) = true;
        GATE.1.notify_all();
        assert_eq!(
            canonical_in(&IN_FLIGHT, minute, "/w/proj", answers, |_| None).as_deref(),
            Some("/w/proj")
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
        let none = Aliases::default();
        let remote = |dir: &str| {
            remote_mount_of(
                dir,
                table.iter().map(|(on, local)| (Path::new(on), *local)),
                &none,
            )
            .is_some()
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
        assert!(
            remote_mount_of("/Volumes/share/x", [], &none).is_none(),
            "no table"
        );
    }

    /// A FOLDER BEHIND MACOS'S FIXED ALIASES IS JUDGED BY THE MOUNT BEHIND
    /// THEM, read lexically from the root's links and `/usr/share/firmlinks`
    /// as this machine has them: `/home` is the automounter's map on the data
    /// volume (not local), so a network home is no longer judged by `/`; a
    /// share recorded as `/Volumes/share` holds the same folder spelled on the
    /// data volume; and a local disk mounted inside a firmlinked folder still
    /// wins over a share recorded around it the other way.
    #[cfg(unix)]
    #[test]
    fn a_folder_behind_a_root_link_or_firmlink_is_judged_by_the_mount_behind_it() {
        let aliases = Aliases::of(
            [
                ("/home".into(), "/System/Volumes/Data/home".into()),
                ("/tmp".into(), "private/tmp".into()),
                ("/work".into(), "home/me/work".into()),
            ],
            "/Users\tUsers\n/Volumes\tVolumes\n/private\tprivate\nnot a line\n",
        );
        let table = [
            ("/", true),
            ("/System/Volumes/Data", true),
            ("/System/Volumes/Data/home", false),
            ("/Volumes/share", false),
            ("/System/Volumes/Data/Users//me", false),
            ("/Users//me/disk", true),
        ];
        let mounts = || table.iter().map(|(on, local)| (Path::new(on), *local));
        let remote = |dir: &str| remote_mount_of(dir, mounts(), &aliases).is_some();
        assert!(remote("/home/me"), "a network home under the automounter");
        assert!(remote("/home/me/src"));
        assert!(remote("/work/proj"), "a root link through another");
        assert!(remote("/System/Volumes/Data/Volumes/share/x"));
        assert!(
            remote("/Users//me/src"),
            "a share recorded on the data volume"
        );
        assert!(!remote("/Users//me/disk/src"), "a local disk inside it");
        assert!(!remote("/tmp/x"));
        assert!(!remote("/Users//you"));
        assert!(!remote("/homework"), "whole components only");
        assert!(
            remote_mount_of("/home/me", mounts(), &Aliases::default()).is_none(),
            "lexically, `/` holds `/home`"
        );
    }

    /// LINUX'S MOUNT TABLE, AS `/proc/self/mountinfo` WRITES IT AND THE
    /// MACHINE'S ONE READER READS IT ([`aterm_sysprobe::volumes::parse_mountinfo`]):
    /// a disk, a memory file system, a btrfs subvolume with propagation tags and
    /// a bind mount are local; NFS, SMB, sshfs, NTFS through FUSE, a VM's host
    /// share (its mount point written with an escaped space), WSL's drive share,
    /// the automounter's map and CephFS are not; and a share mounted over a
    /// local folder at one point judges it, being listed after.
    #[cfg(unix)]
    #[test]
    fn linux_mountinfo_judges_shares_and_user_space_volumes_not_local() {
        let text = "\
32 1 253:1 / / rw,relatime shared:1 - ext4 /dev/vda1 rw,discard
30 32 0:27 / /run rw,nosuid shared:5 - tmpfs tmpfs rw,size=1634148k
61 32 0:52 /@home /home rw,relatime shared:2 master:7 - btrfs /dev/nvme0n1p3 rw,subvol=/@home
62 32 253:1 /srv/data /data rw,relatime - ext4 /dev/vda1 rw
70 32 0:60 / /mnt/nfs rw,relatime shared:40 - nfs4 nas.lan:/export rw,vers=4.2
72 32 0:62 / /mnt/smb rw,relatime - cifs //nas/share rw
73 32 0:63 / /mnt/ssh rw,nosuid,nodev - fuse.sshfs me@host:/home/me rw
75 32 8:17 / /media/me/Win rw - fuseblk /dev/sdb1 rw
58 32 0:45 / /Users//me/My\\040Code ro,relatime shared:173 - virtiofs lima-b7bb6e22a55cbfdc ro
76 32 0:66 / /mnt/c rw,noatime - 9p C:\\134 rw,dirsync,aname=drvfs
77 32 0:67 / /net rw,relatime - autofs /etc/auto.net rw,fd=7
81 32 0:71 / /lab/ceph rw,relatime - ceph admin@6f1e2c.cephfs=/ rw,name=admin
79 32 0:69 / /mnt/shared rw - ext4 /dev/vdb1 rw
80 79 0:70 / /mnt/shared rw - nfs4 nas.lan:/shared rw
";
        let none = Aliases::default();
        let remote = |dir: &str, table: &[(std::path::PathBuf, bool)]| {
            remote_mount_of(
                dir,
                table.iter().map(|(on, local)| (on.as_path(), *local)),
                &none,
            )
            .is_some()
        };
        let table = super::judged(aterm_sysprobe::volumes::parse_mountinfo(text, false));
        for local in ["/etc", "/run/user/1000", "/home/me/proj", "/data/x"] {
            assert!(!remote(local, &table), "{local}: {table:?}");
        }
        for share in [
            "/mnt/nfs/proj",
            "/mnt/smb/x",
            "/mnt/ssh/x",
            "/media/me/Win/x",
            "/Users//me/My Code/proj",
            "/mnt/c/Users",
            "/net/host/x",
            "/lab/ceph/x",
        ] {
            assert!(remote(share, &table), "{share}: {table:?}");
        }
        assert!(!remote("/Users//me/My", &table), "the escape is one name");
        assert!(remote("/mnt/shared/x", &table), "the share on top");
        assert!(
            !remote("/mnt/nfs/proj", &[]),
            "CONTROL: the empty table judged the share local"
        );
        let cut = super::judged(aterm_sysprobe::volumes::parse_mountinfo(text, true));
        assert!(
            remote("/home/me/proj", &cut),
            "a cut table proves no folder local: {cut:?}"
        );
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
        for (link, target) in super::root_links() {
            let here = Aliases::here();
            let through = here.spelled(&link.join("x"));
            assert_eq!(
                through,
                here.spelled(&Path::new("/").join(&target).join("x")),
                "{} -> {}: {here:?}",
                link.display(),
                target.display()
            );
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let table = super::mount_table();
            assert!(
                table
                    .iter()
                    .any(|(on, local)| on == Path::new("/") && *local),
                "{table:?}"
            );
        }
        #[cfg(target_os = "macos")]
        {
            let table = super::mount_table();
            if table
                .iter()
                .any(|(on, local)| on == Path::new("/System/Volumes/Data/home") && !*local)
            {
                assert!(remote_volume("/home/nobody"), "{:?}", Aliases::here());
            }
        }
    }

    /// The home folder stands in for a folder a shell cannot start in, naming it
    /// and why; a present folder and an empty ask change nothing, and with no
    /// home folder the request stands.
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
            let fell_back = start_folder_in(Some(dir), home, None, fault, |_| false);
            assert_eq!(fell_back.cwd.as_deref(), Some("/Users//me"));
            assert_eq!(fell_back.fell_back, Some((dir.to_owned(), why)));
        }

        let kept = start_folder_in(Some("/work/here"), home, None, fault, |_| false);
        assert_eq!(kept.cwd.as_deref(), Some("/work/here"));
        assert_eq!(kept.fell_back, None);

        for nothing in [None, Some("")] {
            let asked = start_folder_in(nothing, home, None, fault, |_| false);
            assert_eq!((asked.cwd, asked.fell_back), (None, None), "{nothing:?}");
        }

        for no_home in [None, Some(Path::new("/work/gone"))] {
            let stands = start_folder_in(Some("/work/gone"), no_home, None, fault, |_| false);
            assert_eq!(stands.cwd.as_deref(), Some("/work/gone"), "{no_home:?}");
            assert_eq!(stands.fell_back, None, "no home folder is claimed");
            let unasked = start_folder_in(Some("/Volumes/hung"), no_home, None, fault, |_| false);
            assert_eq!(
                (unasked.cwd, unasked.fell_back),
                (None, None),
                "{no_home:?}"
            );
        }

        for (dir, want) in [("/Volumes/hung", None), ("/work/gone", Some("/work/gone"))] {
            let asked = std::cell::Cell::new(0);
            let counted = |at: &str| {
                asked.set(asked.get() + 1);
                fault(at)
            };
            let start = start_folder_in(Some(dir), Some(Path::new(dir)), None, counted, |_| false);
            assert_eq!((start.cwd.as_deref(), start.fell_back), (want, None));
            assert_eq!(asked.get(), 1, "{dir}: checked once");
        }
    }

    /// A SHELL HANDED NO FOLDER NEVER CHDIRS INTO A HOME THAT STOPPED
    /// ANSWERING (session 0 of an aterm started from Finder): the folder the
    /// PTY goes to unasked, on a volume that is not local by the mount table,
    /// is checked, and silent it starts the shell in `/`, named as the home
    /// that did not answer.
    #[test]
    fn a_home_that_stopped_answering_starts_an_unasked_shell_in_the_root() {
        let fault = |dir: &str| match dir {
            "/home/hung" | "/home/hung/proj" | "/Volumes/hung" => Some(Fault::Unresponsive),
            "/home/gone" => Some(Fault::Missing),
            _ => None,
        };
        let table = [("/", true), ("/home", false), ("/Volumes/hung", false)];
        let remote = |dir: &str| {
            let dir = Path::new(dir);
            table
                .iter()
                .filter(|(on, _)| dir.starts_with(on))
                .max_by_key(|(on, _)| on.len())
                .is_some_and(|(_, local)| !local)
        };
        let asked = std::cell::Cell::new(Vec::new());
        let counted = |at: &str| {
            let mut seen = asked.take();
            seen.push(at.to_owned());
            asked.set(seen);
            fault(at)
        };
        let root = |named: &str| StartFolder {
            cwd: Some(LOCAL_STAND_IN.to_owned()),
            fell_back: Some((named.to_owned(), Fault::HomeUnresponsive)),
        };
        let unchanged = StartFolder {
            cwd: None,
            fell_back: None,
        };
        let hung = Some(Path::new("/home/hung"));
        assert_eq!(
            start_folder_in(None, hung, Some("/home/hung"), counted, remote),
            root("/home/hung")
        );
        assert_eq!(asked.take(), ["/home/hung"], "checked once");
        assert_eq!(
            start_folder_in(
                Some("/home/hung"),
                hung,
                Some("/home/hung"),
                counted,
                remote
            ),
            root("/home/hung"),
            "a pane at `~` on the silent home"
        );
        assert_eq!(asked.take(), ["/home/hung"], "the folder is the home: once");
        assert_eq!(
            start_folder_in(
                Some("/home/hung/proj"),
                hung,
                Some("/home/hung"),
                counted,
                remote
            ),
            root("/home/hung/proj"),
            "a folder in it, with the home that would stand in silent too"
        );
        assert_eq!(asked.take(), ["/home/hung/proj", "/home/hung"]);
        assert_eq!(
            start_folder_in(
                Some("/Volumes/hung"),
                hung,
                Some("/home/hung"),
                fault,
                remote
            ),
            root("/Volumes/hung")
        );

        let stands = |at: &str| StartFolder {
            cwd: Some(at.to_owned()),
            fell_back: None,
        };
        let local = Some(Path::new("/Users//me"));
        assert_eq!(
            start_folder_in(None, local, Some("/Users//me"), counted, remote),
            stands("/Users//me")
        );
        assert!(asked.take().is_empty(), "a local home costs no check");
        let answering = Some(Path::new("/home/me"));
        assert_eq!(
            start_folder_in(None, answering, Some("/home/me"), counted, remote),
            stands("/home/me")
        );
        assert_eq!(asked.take(), ["/home/me"]);
        let gone = Some(Path::new("/home/gone"));
        assert_eq!(
            start_folder_in(None, gone, Some("/home/gone"), fault, remote),
            stands("/home/gone")
        );
        assert_eq!(
            start_folder_in(None, hung, None, counted, remote),
            unchanged
        );
        assert!(
            asked.take().is_empty(),
            "not started from `/`: nothing judged"
        );
        assert_eq!(
            start_folder_in(Some("/Volumes/hung"), hung, None, fault, remote),
            unchanged,
            "a silent folder with no default to go to hands over no folder, as before"
        );
    }

    /// A fall-back said once its shell is up reaches the queue in order, each
    /// folder once, and the queue empties when taken; a start that used its
    /// folder notes nothing.
    #[test]
    fn a_start_that_fell_back_reaches_the_queue_and_one_that_did_not_says_nothing() {
        let _lane = crate::message_inbox::lane_test_guard();
        let _ = take_noted();
        let fault =
            |dir: &str| (dir != "/Users//me" && dir != "/work/here").then_some(Fault::Missing);
        let home = Some(Path::new("/Users//me"));
        start_folder_in(Some("/work/here"), home, None, fault, |_| false).say();
        assert!(take_noted().is_empty(), "a folder that was used");
        for dir in ["/a", "/b", "/a"] {
            start_folder_in(Some(dir), home, None, fault, |_| false).say();
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

    /// A held pane's folder (ssh: maybe another machine's) is taken only when it
    /// is here; the same folder from a pane at its own prompt is taken.
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

    /// A PARKED SPARE'S RETARGET names the one `$PWD` every shell's start
    /// would: the folder's kernel path, unless the inherited `$HOME` or `$PWD`
    /// names the folder spelled otherwise (zsh would take it, bash would not)
    /// or one FIFO line cannot carry it.
    #[test]
    fn a_retarget_names_the_folder_only_as_every_shell_would() {
        use std::ffi::OsStr;
        let names = |alias: &OsStr| alias == "/tmp/x" || alias == "/private/tmp/x";
        let kernel = "/private/tmp/x";
        let os = |s: &'static str| Some(OsStr::new(s));
        assert_eq!(
            retarget_in(kernel, names, [None, None]).as_deref(),
            Some(kernel)
        );
        assert_eq!(
            retarget_in(kernel, names, [os("/Users//me"), os(kernel)]).as_deref(),
            Some(kernel),
            "aliases naming another folder, or this one as the kernel does"
        );
        assert_eq!(
            retarget_in(kernel, names, [os("/tmp/x"), None]),
            None,
            "$HOME names it otherwise"
        );
        assert_eq!(
            retarget_in(kernel, names, [None, os("/tmp/x")]),
            None,
            "$PWD names it otherwise"
        );
        assert_eq!(
            retarget_in("/private/tmp/a\nb", |_| false, [None, None]),
            None,
            "two lines"
        );
        assert_eq!(retarget_in("tmp/x", |_| false, [None, None]), None);
    }

    /// A retarget never moves a spare onto a volume that is not local: not to a
    /// folder judged so (nothing is read there), and not through a person's
    /// own symlink from a local folder into one (the bounded read lands there).
    #[test]
    fn a_retarget_never_lands_on_a_volume_that_is_not_local() {
        let remote = |dir: &str| dir.starts_with("/Volumes/share");
        let read_at = |kernel: &'static str| move |_: &str| Some(kernel.to_owned());
        assert_eq!(
            retarget_folder_in("/Users//me/src", remote, read_at("/Users//me/src")).as_deref(),
            Some("/Users//me/src")
        );
        assert_eq!(
            retarget_folder_in("/Users//me/nas", remote, read_at("/Volumes/share/x")),
            None,
            "a symlink into a share"
        );
        let mut read = false;
        assert_eq!(
            retarget_folder_in("/Volumes/share/x", remote, |_| {
                read = true;
                Some("/Volumes/share/x".to_owned())
            }),
            None
        );
        assert!(!read, "a folder judged not local is never read");
    }

    /// The kernel's path for a folder is what `getcwd()` names there (`pwd -P`
    /// run in it): through the root's link and a firmlink, in the names' own
    /// case.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_kernel_path_is_the_one_getcwd_names() {
        let scratch = aterm_tempfile::tempdir().expect("tempdir");
        let here = scratch
            .path()
            .to_str()
            .expect("utf-8 scratch path")
            .to_owned();
        let getcwd = |dir: &str| {
            let out = std::process::Command::new("/bin/pwd")
                .arg("-P")
                .current_dir(dir)
                .output()
                .expect("pwd");
            String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
        };
        for dir in [
            here.as_str(),
            "/tmp",
            "/System/Volumes/Data/Library",
            "/USR",
        ] {
            assert_eq!(super::kernel_path(dir), Some(getcwd(dir)), "{dir}");
        }
        assert_eq!(
            super::kernel_path("/System/Volumes/Data/Library").as_deref(),
            Some("/Library")
        );
        assert_ne!(
            std::fs::canonicalize("/System/Volumes/Data/Library").unwrap(),
            Path::new("/Library"),
            "realpath keeps the data volume's spelling"
        );
        assert_eq!(super::kernel_path(&format!("{here}/gone")), None);
    }

    /// Linux: the kernel's path for a folder is what `getcwd()` names there
    /// (`pwd -P` run in it), every symlink on the way resolved.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_kernel_path_is_the_one_getcwd_names() {
        let scratch = aterm_tempfile::tempdir().expect("tempdir");
        let real = scratch.path().join("real");
        std::fs::create_dir(&real).expect("folder");
        let link = scratch.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("link");
        let getcwd = |dir: &Path| {
            let mut command = aterm_uds::spawnfd::command("/bin/pwd");
            command.arg("-P").current_dir(dir);
            let out = aterm_agent::harness::align::capture_bounded(
                command,
                aterm_spec::wait::MUST_HAPPEN,
                16 * 1024,
            )
            .expect("pwd within the hang detector");
            assert!(out.success(), "pwd must succeed: {:?}", out.code);
            String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
        };
        for dir in [real.as_path(), link.as_path(), Path::new("/tmp")] {
            let named = super::kernel_path(dir.to_str().expect("utf-8"));
            assert_eq!(named, Some(getcwd(dir)), "{}", dir.display());
        }
        let through_link = link.to_str().expect("utf-8");
        assert_ne!(
            super::kernel_path(through_link).as_deref(),
            Some(through_link),
            "the link's own spelling is no shell's `$PWD`"
        );
        let gone = scratch.path().join("gone");
        std::fs::create_dir(&gone).expect("folder");
        let gone_name = gone.to_str().expect("utf-8").to_owned();
        std::fs::remove_dir(&gone).expect("remove");
        assert_eq!(super::kernel_path(&gone_name), None);
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
