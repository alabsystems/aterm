// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Resolves the `aterm` executable launched by integration test fixtures, and keeps the folder of
//! copies they run ([`snapshot`]) down to the copies still in use ([`sweep`]).

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use aterm_uds::file_lock::LockFile;

/// How long a copy no use has marked stays: an hour, longer than a landing check's whole run of
/// the suites (some 38 minutes), so no run of them loses its copy between two test binaries.
pub(crate) const UNUSED: Duration = Duration::from_secs(60 * 60);
/// How often a process that keeps using its copy marks the use again ([`remark`]).
pub(crate) const REMARK: Duration = Duration::from_secs(60);
/// The hang detector on [`LOCK`] for a use's mark: a sweep holds it only while it renames.
const MARK_HANG: Duration = Duration::from_secs(60);
/// How long a sweep waits for [`LOCK`] before it leaves every folder for a later sweep.
const LEAVE_WAIT: Duration = Duration::from_secs(1);
/// The most copies one sweep runs to prove them ([`Host::proves`]): each run is a first exec,
/// which macOS's program check may hold for up to
/// [`FIRST_EXEC_HANG`](aterm_uds::deadline::FIRST_EXEC_HANG), so copies nothing proved yet are
/// run one at each process's sweep, never back to back.
const PROOFS: usize = 1;
/// The most times the wait of a copy no run proved doubles ([`aged`]): to 2^7 hours, some five
/// days between runs of a copy that never writes a line.
const DOUBLINGS: u32 = 7;

/// The lock, in the folder of copies, a use is marked under and a copy is taken out of use under.
const LOCK: &str = ".lock";
/// The lock one [`sweep`] at a time holds.
const SWEEP: &str = ".sweep";
/// The file in a copy's folder whose modification time is the copy's last use.
pub(crate) const USED: &str = ".used";
/// The start of the name of the record a process that resolved a copy keeps in its folder
/// ([`user_name`]).
const USER: &str = ".user.";
/// The file in a copy's folder whose length is how many runs of its copy wrote no line
/// ([`prove`]).
pub(crate) const UNPROVEN: &str = ".unproven";
/// The start of the name of a folder a sweep took out of use and removes.
pub(crate) const LEAVING: &str = ".leaving.";

/// Returns the canonical path of the executable named by `ATERM_TEST_EXECUTABLE`, or of this
/// test's snapshot of this package's `aterm` ([`snapshot`]) when unset.
///
/// Canonical because aterm names itself by its physical path (`current_exe`, `Image::at`), and a
/// release clone reaches its target through a symlink (`target -> target.noindex`): a test that
/// compared a spelling through the link failed only in a release's verify.
pub fn aterm_executable() -> PathBuf {
    let Some(selected) = std::env::var_os("ATERM_TEST_EXECUTABLE") else {
        return resolved();
    };
    let selected = PathBuf::from(selected);
    assert!(
        selected.is_absolute(),
        "ATERM_TEST_EXECUTABLE must name an absolute owned artifact: {selected:?}"
    );
    let metadata = std::fs::metadata(&selected)
        .unwrap_or_else(|error| panic!("ATERM_TEST_EXECUTABLE {selected:?}: {error}"));
    assert!(
        metadata.is_file(),
        "ATERM_TEST_EXECUTABLE must name a regular file: {selected:?}"
    );
    std::fs::canonicalize(&selected)
        .unwrap_or_else(|error| panic!("ATERM_TEST_EXECUTABLE {selected:?}: {error}"))
}

/// A copy this process resolved: its canonical path, the folder of copies and its own folder in
/// it, its file name, and when this process last marked its use.
pub(crate) struct Snapshot {
    pub(crate) path: PathBuf,
    root: PathBuf,
    pub(crate) folder: PathBuf,
    name: OsString,
    pub(crate) marked: Instant,
}

/// The copy this process runs: made on the first call ([`snapshot`]), which also starts this
/// process's [`upkeep`]; a later call marks its use again ([`remark`]).
fn resolved() -> PathBuf {
    static RESOLVED: std::sync::Mutex<Option<Snapshot>> = std::sync::Mutex::new(None);
    let mut held = RESOLVED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(copy) = held.as_mut() {
        remark(copy, Instant::now());
        return copy.path.clone();
    }
    let built = aterm_tempfile::bin_exe("aterm");
    let built = std::fs::canonicalize(&built)
        .unwrap_or_else(|error| panic!("the package's aterm {built:?}: {error}"));
    let copy = snapshot(&built);
    let path = copy.path.clone();
    upkeep(&copy);
    *held = Some(copy);
    path
}

/// Marks the use of `copy` again ([`USED`]) when a call at `now` comes [`REMARK`] or more after
/// its last mark, so a copy's last use is within a minute of its process's last call. The
/// record the first mark made ([`mark`]) stands while the process lives.
pub(crate) fn remark(copy: &mut Snapshot, now: Instant) {
    if now.saturating_duration_since(copy.marked) >= REMARK {
        let _ = touch(&copy.folder.join(USED), SystemTime::now());
        copy.marked = now;
    }
}

/// `built`, copied once into a folder beside it (`.ubd/aterm-under-test.noindex/<content>/`,
/// named by its length and its modification time), canonical. Every instance a test starts runs
/// that copy, whose name nothing writes again, and every process of the test (a stand-in the test
/// binary runs as included) finds the same copy.
///
/// `built` is Cargo's uplifted copy of the binary (`<target>/debug/aterm`), which every Cargo
/// command on the target directory writes again as a new file, even when the binary is fresh. An
/// instance started from it then runs an image its own path no longer names: aterm's process
/// observation (`native_act::Process::observe`) rightly refuses an image whose path names another
/// file, so its harness cannot vouch for itself as a native operation's actor and refuses the
/// operation, and its relaunch helper (`<path> harness native-launch`) runs another build. A
/// parallel suite's Cargo command did exactly that under a running fabric_world instance.
///
/// A copy is published whole (made under a private name, then linked to its own name, which a
/// second publisher's link leaves as it is) and never replaced. Its use is then marked under the
/// folder of copies' [`LOCK`] ([`mark`]); a copy a [`sweep`] took out of use before the mark is
/// published again.
pub(crate) fn snapshot(built: &Path) -> Snapshot {
    let root = built
        .parent()
        .expect("the binary's folder")
        .join(".ubd")
        .join("aterm-under-test.noindex");
    std::fs::create_dir_all(&root).unwrap_or_else(|error| panic!("{root:?}: {error}"));
    let name = built.file_name().expect("the binary's file name");
    // A wall bound: what moves `built` is a Cargo command of another suite, no process this one
    // started, so no reading of this binary's tree measures it.
    let deadline = Instant::now() + Duration::from_secs(60);
    // A copy taken while Cargo writes `built` again is refused by the stamp check: the same
    // file, unchanged, before and after, and the copy as long as it.
    let (folder, copy) = loop {
        let before = stamp(built);
        if let Some((_, _, len, Some(modified))) = before {
            let folder = root.join(content_key(len, modified));
            let copy = folder.join(name);
            if !published(&copy, len) {
                std::fs::create_dir_all(&folder)
                    .unwrap_or_else(|error| panic!("{folder:?}: {error}"));
                let private = folder.join(format!(
                    ".{}.{}",
                    std::process::id(),
                    name.to_string_lossy()
                ));
                let _ = std::fs::remove_file(&private);
                let copied = std::fs::copy(built, &private).ok();
                if before == stamp(built) && copied == Some(len) {
                    let linked = std::fs::hard_link(&private, &copy);
                    // A second publisher's link stands; a folder a sweep took out of use
                    // meanwhile is made again on the next round.
                    if let Err(error) = linked
                        && !matches!(error.kind(), ErrorKind::AlreadyExists | ErrorKind::NotFound)
                    {
                        panic!("publish {copy:?}: {error}");
                    }
                }
                let _ = std::fs::remove_file(&private);
            }
            if published(&copy, len) && mark(&root, &folder, &copy, len) {
                break (folder, copy);
            }
        }
        assert!(
            Instant::now() < deadline,
            "{built:?} did not hold still for one copy within a minute"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    let path = std::fs::canonicalize(&copy).unwrap_or_else(|error| panic!("{copy:?}: {error}"));
    Snapshot {
        path,
        root,
        folder,
        name: name.to_owned(),
        marked: Instant::now(),
    }
}

/// Marks this process's use of the copy at `copy` (published `len` bytes long) under [`LOCK`]:
/// its folder's [`USED`] time, and this process's record there ([`user_name`]), which keeps the
/// copy while this process lives. False when a sweep took the folder out of use first, so the
/// caller publishes the copy again.
fn mark(root: &Path, folder: &Path, copy: &Path, len: u64) -> bool {
    let lock = root.join(LOCK);
    let busy = "a sweep held the lock on the copies of aterm for a minute";
    // A wall bound handed to the lock: its holder is another process's sweep, not one this
    // process started.
    let held = LockFile::take(&lock, Instant::now() + MARK_HANG, busy);
    let _held = held.unwrap_or_else(|error| panic!("lock {lock:?}: {error}"));
    if !published(copy, len) {
        return false;
    }
    let used = folder.join(USED);
    touch(&used, SystemTime::now()).unwrap_or_else(|error| panic!("mark {used:?}: {error}"));
    let pid = std::process::id();
    let user = folder.join(user_name(pid, serial_of(pid)));
    let recorded = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&user);
    recorded.unwrap_or_else(|error| panic!("record {user:?}: {error}"));
    true
}

/// Whether the file at `copy` is a published copy `len` bytes long.
fn published(copy: &Path, len: u64) -> bool {
    stamp(copy).is_some_and(|published| published.2 == len)
}

/// Sets the modification time of the file at `path` to `at`, made empty when there is none.
fn touch(path: &Path, at: SystemTime) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .set_modified(at)
}

/// The name of the record process `pid` keeps in the folder of a copy it resolved: [`USER`], its
/// pid and, where this host reads one, its serial ([`aterm_uds::process::process_serial`]), which
/// no later process given the same pid shares.
pub(crate) fn user_name(pid: u32, serial: Option<u64>) -> String {
    match serial {
        Some(serial) => format!("{USER}{pid}.{serial}"),
        None => format!("{USER}{pid}"),
    }
}

/// The serial of the process `pid` names now ([`aterm_uds::process::process_serial`]).
fn serial_of(pid: u32) -> Option<u64> {
    i32::try_from(pid)
        .ok()
        .and_then(aterm_uds::process::process_serial)
}

/// The pid and serial a record's file name holds ([`user_name`]); `None` for any other name.
fn user_of(name: &str) -> Option<(u32, Option<u64>)> {
    let named = name.strip_prefix(USER)?;
    match named.split_once('.') {
        Some((pid, serial)) => Some((pid.parse().ok()?, Some(serial.parse().ok()?))),
        None => Some((named.parse().ok()?, None)),
    }
}

/// Starts this process's upkeep of the folder of copies on a thread of its own: one [`sweep`]
/// of this host, which leaves this process's copy alone. A thread that cannot start leaves the
/// sweep to the next process.
fn upkeep(copy: &Snapshot) {
    let (root, own, name) = (copy.root.clone(), copy.folder.clone(), copy.name.clone());
    let _ = std::thread::Builder::new()
        .name("aterm-under-test-upkeep".to_owned())
        .spawn(move || sweep(&root, &own, &name, SystemTime::now(), &mut ThisHost));
}

/// What a [`sweep`] reads of this account's processes and asks of a copy: [`ThisHost`] reads this
/// host, and a test answers for one.
pub(crate) trait Host {
    /// Every process of this account, by pid; `None` when they could not be listed.
    fn listed(&mut self) -> Option<BTreeSet<u32>>;
    /// Which of `folders` one of the processes `listed` runs, maps, holds open or works in
    /// something under; `None` when one could not be read and is no platform service.
    fn uses(&mut self, listed: &BTreeSet<u32>, folders: &[PathBuf]) -> Option<BTreeSet<PathBuf>>;
    /// Whether the copy at `copy`, run, wrote a line ([`prove`]).
    fn proves(&mut self, copy: &Path) -> bool;
}

/// This host: its processes as [`aterm_uds::holders`] reads them (the disk watch's reading
/// before it removes a build directory), and a real run of a copy. A live process that runs a
/// copy is known by its executable's path as the kernel names it (`proc_pidpath` on macOS,
/// `/proc/<pid>/exe` on Linux), which shows that it runs from the copy, not that macOS finished
/// assessing its exec: that use keeps the copy, and only a line a run wrote proves it ran.
pub(crate) struct ThisHost;

impl Host for ThisHost {
    fn listed(&mut self) -> Option<BTreeSet<u32>> {
        let pids = aterm_uds::holders::uid_pids().ok()?;
        Some(pids.into_iter().collect())
    }

    fn uses(&mut self, listed: &BTreeSet<u32>, folders: &[PathBuf]) -> Option<BTreeSet<PathBuf>> {
        let pids: Vec<u32> = listed.iter().copied().collect();
        let scan = aterm_uds::holders::scan_path_users_of(&pids, folders, &[]).ok()?;
        let scan = whole(scan)?;
        // A scan names one use of each process, so each process it found is read again for
        // each folder that use was not in.
        let users: BTreeSet<u32> = scan.users.iter().map(|user| user.pid).collect();
        let users: Vec<u32> = users.into_iter().collect();
        let mut used = BTreeSet::new();
        for folder in folders {
            if scan.users.iter().any(|user| user.path.starts_with(folder)) {
                used.insert(folder.clone());
                continue;
            }
            if users.is_empty() {
                continue;
            }
            let dirs = std::slice::from_ref(folder);
            let again = aterm_uds::holders::scan_path_users_of(&users, dirs, &[]).ok()?;
            if !whole(again)?.users.is_empty() {
                used.insert(folder.clone());
            }
        }
        Some(used)
    }

    fn proves(&mut self, copy: &Path) -> bool {
        prove(copy)
    }
}

/// `scan`, where each process it could not read is a platform service, as the build cache's
/// fence reads them ([`aterm_uds::holders::unix_platform_service`]); `None` otherwise.
fn whole(scan: aterm_uds::holders::PathScan) -> Option<aterm_uds::holders::PathScan> {
    let whole = scan
        .unreadable
        .iter()
        .all(|(_, exe)| aterm_uds::holders::unix_platform_service(exe.as_deref()));
    whole.then_some(scan)
}

/// Whether the program at `copy`, run once with `--version` in a scratch environment (as
/// `aterm_spec::wait::first_exec` runs it) and waited for to its end until it made no progress
/// for [`FIRST_EXEC_HANG`](aterm_uds::deadline::FIRST_EXEC_HANG), wrote a line on its stdout or
/// its stderr (its version, a panic's or the loader's words): proof that it ran from its file, so
/// macOS has finished assessing its first exec, since a process its program check holds runs
/// nothing and only it and what it started hold those pipes. A run that could not start, wrote
/// nothing or was ended as stalled proves nothing.
pub(crate) fn prove(copy: &Path) -> bool {
    let Ok(scratch) = aterm_tempfile::tempdir() else {
        return false;
    };
    let mut cmd = aterm_uds::spawnfd::command(copy);
    cmd.arg("--version").env_clear();
    if let Some(search) = std::env::var_os("PATH") {
        cmd.env("PATH", search);
    }
    cmd.env("HOME", scratch.path())
        .env("TMPDIR", scratch.path());
    let Ok(ran) = aterm_uds::deadline::output_while_process_progressing_complete_stdout(
        &mut cmd,
        aterm_uds::deadline::FIRST_EXEC_HANG,
        "a copy's first run",
    ) else {
        return false;
    };
    wrote_a_line(&ran.stdout) || wrote_a_line(&ran.stderr)
}

/// Whether `bytes` hold a line: a byte, then a newline.
fn wrote_a_line(bytes: &[u8]) -> bool {
    bytes
        .split(|&byte| byte == b'\n')
        .rev()
        .skip(1)
        .any(|line| !line.is_empty())
}

/// One sweep of the folder of copies at `root`, at `now`, by one process at a time (the [`SWEEP`]
/// lock; a process that finds it held leaves the sweep to its holder). A copy's folder other than
/// `own` leaves when all of these hold, and stays otherwise:
///
/// - no use has marked it for [`UNUSED`], doubled for each run of its copy that wrote no line
///   while nothing showed it ran ([`aged`]);
/// - no process that resolved it lives: no record ([`user_name`]) names a listed pid with the
///   serial it recorded;
/// - no live process of this account runs, maps, holds open or works in anything under it, by a
///   whole reading ([`Host::uses`]), one reading of every aged folder at once;
/// - nothing in it may still be in its first exec's assessment: it holds no copy named `name`,
///   or a child showed it ran from its copy ([`RAN`](aterm_uds::deadline::RAN)), or the copy,
///   run now ([`Host::proves`]), wrote a line, which marks it so, and a reading taken after that
///   run finds no live process that resolved or uses it (one the run left running from it). A
///   sweep runs at most [`PROOFS`] copies, and leaves another unproven one as it is for a later
///   sweep. A copy that writes nothing, or is ended at the deadline, stays: macOS may still be
///   assessing it, and a file unlinked mid-assessment crashes macOS 27.0.1's syspolicyd.
///
/// A folder kept because a live process resolved or uses it, or for want of a whole reading or
/// of a proof, is marked used at `now`, so a sweep looks at it again an hour after (later, after
/// a run of its copy that wrote nothing) rather than at every process's start, and one a
/// process used to its end leaves an hour after that end. With this account's processes
/// unlisted, no folder leaves and each aged one is marked so. The folders that pass leave under
/// [`LOCK`], where each is checked again (a use marked since keeps it) and renamed out of use
/// ([`LEAVING`]); once the lock is released they are removed, with any a killed sweep left.
pub(crate) fn sweep(root: &Path, own: &Path, name: &OsStr, now: SystemTime, host: &mut dyn Host) {
    let sweeping = LockFile::take(&root.join(SWEEP), Instant::now(), "another sweep");
    let Ok(_sweeping) = sweeping else {
        return;
    };
    let due: Vec<PathBuf> = folders(root)
        .into_iter()
        .filter(|folder| folder != own && aged(folder, now))
        .collect();
    if !due.is_empty() {
        if let Some(listed) = host.listed() {
            let (passed, listed) = passing(due, name, listed, now, host);
            take_out_of_use(root, &passed, name, &listed, now);
        } else {
            for folder in &due {
                keep(folder, now);
            }
        }
    }
    for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
        if entry.file_name().to_string_lossy().starts_with(LEAVING) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// The copies' folders in `root`, in name order: its directories but those whose names start
/// with a dot (the locks, and folders taken out of use).
fn folders(root: &Path) -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect();
    folders.sort();
    folders
}

/// Whether no use has marked the copy in `folder` for longer than its wait at `now`: [`UNUSED`],
/// doubled for each run of its copy that wrote no line ([`UNPROVEN`], at most [`DOUBLINGS`]
/// times) while nothing showed it ran ([`RAN`](aterm_uds::deadline::RAN)). Its last use is its
/// [`USED`] file's modification time, or the folder's own when it has none (a publish cut short).
pub(crate) fn aged(folder: &Path, now: SystemTime) -> bool {
    let last_use = std::fs::metadata(folder.join(USED))
        .or_else(|_| std::fs::symlink_metadata(folder))
        .and_then(|meta| meta.modified());
    let ran = std::fs::symlink_metadata(folder.join(aterm_uds::deadline::RAN))
        .is_ok_and(|meta| meta.is_file());
    let unproven = if ran {
        0
    } else {
        std::fs::metadata(folder.join(UNPROVEN)).map_or(0, |meta| meta.len())
    };
    let doublings = u32::try_from(unproven).map_or(DOUBLINGS, |n| n.min(DOUBLINGS));
    let wait = UNUSED * (1_u32 << doublings);
    last_use.is_ok_and(|at| now.duration_since(at).is_ok_and(|age| age > wait))
}

/// Marks the use of the copy in `folder` at `now`, for a sweep to look at it again once it is
/// [`aged`].
fn keep(folder: &Path, now: SystemTime) {
    let _ = touch(&folder.join(USED), now);
}

/// Whether a process that resolved the copy in `folder` lives: a record there names a pid of
/// `listed` and, where it recorded a serial, that pid's process has it. A folder or record that
/// cannot be read counts as one.
fn resolved_by_a_live_process(folder: &Path, listed: &BTreeSet<u32>) -> bool {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return true;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            return true;
        };
        let Some((pid, serial)) = entry.file_name().to_str().and_then(user_of) else {
            continue;
        };
        if listed.contains(&pid) && serial.is_none_or(|serial| serial_of(pid) == Some(serial)) {
            return true;
        }
    }
    false
}

/// Whether nothing in `folder` may still be in its first exec's assessment, without running
/// anything: it holds no copy named `name`, or the [`RAN`](aterm_uds::deadline::RAN) mark.
fn shown_to_run(folder: &Path, name: &OsStr) -> bool {
    let ran = std::fs::symlink_metadata(folder.join(aterm_uds::deadline::RAN));
    let copy = std::fs::symlink_metadata(folder.join(name));
    ran.is_ok_and(|meta| meta.is_file())
        || copy.is_err_and(|error| error.kind() == ErrorKind::NotFound)
}

/// Of the aged folders `due`, those that pass every check of [`sweep`] but the one under
/// [`LOCK`], with the latest listing of this account's processes they passed by (`listed`, or
/// the one read after a proof). A folder a live process resolved or uses, or kept for want of a
/// whole reading or of a proof, is marked used at `now` ([`keep`]); a copy a live process uses
/// is not run to prove it, as that use keeps it anyway; an unproven copy past the sweep's
/// [`PROOFS`] is left unmarked.
fn passing(
    due: Vec<PathBuf>,
    name: &OsStr,
    mut listed: BTreeSet<u32>,
    now: SystemTime,
    host: &mut dyn Host,
) -> (Vec<PathBuf>, BTreeSet<u32>) {
    let (live, unresolved): (Vec<PathBuf>, Vec<PathBuf>) = due
        .into_iter()
        .partition(|folder| resolved_by_a_live_process(folder, &listed));
    for folder in &live {
        keep(folder, now);
    }
    if unresolved.is_empty() {
        return (Vec::new(), listed);
    }
    let Some(used) = host.uses(&listed, &unresolved) else {
        for folder in &unresolved {
            keep(folder, now);
        }
        return (Vec::new(), listed);
    };
    let mut passed = Vec::new();
    let mut proofs = 0;
    for folder in unresolved {
        if used.contains(&folder) {
            keep(&folder, now);
        } else if shown_to_run(&folder, name) {
            passed.push(folder);
        } else if proofs < PROOFS {
            proofs += 1;
            if !proven_now(&folder, name, host) {
                keep(&folder, now);
                continue;
            }
            // The run may have left a process running from the copy, which the reading before
            // it could not see.
            let Some(again) = host.listed() else {
                keep(&folder, now);
                continue;
            };
            let clear = !resolved_by_a_live_process(&folder, &again)
                && host
                    .uses(&again, std::slice::from_ref(&folder))
                    .is_some_and(|used| used.is_empty());
            listed = again;
            if clear {
                passed.push(folder);
            } else {
                keep(&folder, now);
            }
        }
    }
    (passed, listed)
}

/// Whether the copy named `name` in `folder`, run now ([`Host::proves`]), wrote a line; when it
/// did, the folder is marked so ([`RAN`](aterm_uds::deadline::RAN)), and when it did not, the
/// run is counted ([`UNPROVEN`]).
fn proven_now(folder: &Path, name: &OsStr, host: &mut dyn Host) -> bool {
    use std::io::Write as _;
    if host.proves(&folder.join(name)) {
        return aterm_uds::deadline::try_mark_ran(folder).is_ok();
    }
    let counted = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(folder.join(UNPROVEN));
    let _ = counted.and_then(|mut file| file.write_all(b"."));
    false
}

/// Takes each folder of `passed` out of use under [`LOCK`], renaming it into `root` as
/// [`LEAVING`]`<folder>.<pid>`, once it is checked again there: a use marked since (every record
/// is made with one, so a process `listed` misses is caught by it), a live process's record, or a
/// copy published since without its proof keeps it. A lock not taken within [`LEAVE_WAIT`] keeps
/// them all.
fn take_out_of_use(
    root: &Path,
    passed: &[PathBuf],
    name: &OsStr,
    listed: &BTreeSet<u32>,
    now: SystemTime,
) {
    if passed.is_empty() {
        return;
    }
    let held = LockFile::take(&root.join(LOCK), Instant::now() + LEAVE_WAIT, "a mark");
    let Ok(_held) = held else {
        return;
    };
    for folder in passed {
        let still = aged(folder, now)
            && !resolved_by_a_live_process(folder, listed)
            && shown_to_run(folder, name);
        if still && let Some(base) = folder.file_name() {
            let mut leaving = OsString::from(LEAVING);
            leaving.push(base);
            leaving.push(format!(".{}", std::process::id()));
            let _ = std::fs::rename(folder, root.join(leaving));
        }
    }
}

/// The folder name of a build's content: its length and its modification time in nanoseconds,
/// in hex. Cargo's uplift keeps the length and the time of the binary it copies, so each copy of
/// one build has one name.
fn content_key(len: u64, modified: std::time::SystemTime) -> String {
    let nanos = modified
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!("{len:x}-{nanos:x}")
}

/// What says a file is the same, unchanged: its device and inode, its length and its
/// modification time.
fn stamp(path: &Path) -> Option<(u64, u64, u64, Option<std::time::SystemTime>)> {
    let meta = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    let (dev, ino) = {
        use std::os::unix::fs::MetadataExt as _;
        (meta.dev(), meta.ino())
    };
    #[cfg(not(unix))]
    let (dev, ino) = (0, 0);
    meta.is_file()
        .then(|| (dev, ino, meta.len(), meta.modified().ok()))
}
