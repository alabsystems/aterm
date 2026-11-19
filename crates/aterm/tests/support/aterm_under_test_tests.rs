// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The sweep of the folder of copies of aterm the suites run ([`sweep`]): on a scratch folder, at
//! a clock the test sets, with a host whose processes and copies the test names, and with this
//! host's own reading of a process that runs a copy; the record and the marks of a copy's use
//! ([`snapshot`], [`remark`]); and the proof a copy ran ([`prove`]) on real programs.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::launch_isolation::aterm_under_test::{
    Host, LEAVING, REMARK, ThisHost, UNPROVEN, USED, aged, aterm_executable, prove, remark,
    snapshot, sweep, user_name,
};

/// The file name of a copy in its folder.
const ATERM: &str = "aterm";

/// A host whose answers the test names.
#[derive(Default)]
struct Answers {
    /// The pids its listing of this account's processes holds; `None`: it cannot list them.
    listed: Option<BTreeSet<u32>>,
    /// The folders a live process uses.
    using: Vec<PathBuf>,
    /// Whether its readings of the processes are not whole.
    unread: bool,
    /// The copies that write a line when run.
    printing: Vec<PathBuf>,
    /// The copies that write a line when run and leave a process running from their folder.
    lingering: Vec<PathBuf>,
    /// The folders a use marks, each at its time, while a sweep reads the processes: after the
    /// sweep read their age and before it takes them out of use.
    marking: Vec<(PathBuf, SystemTime)>,
    /// Every copy it was asked to run, in order.
    run: Vec<PathBuf>,
    /// The folders of each reading of the processes it was asked for, in order.
    read: Vec<Vec<PathBuf>>,
}

impl Host for Answers {
    fn listed(&mut self) -> Option<BTreeSet<u32>> {
        self.listed.clone()
    }

    fn uses(&mut self, _listed: &BTreeSet<u32>, folders: &[PathBuf]) -> Option<BTreeSet<PathBuf>> {
        self.read.push(folders.to_vec());
        for (folder, at) in &self.marking {
            set_used(folder, *at);
        }
        let used: BTreeSet<PathBuf> = folders
            .iter()
            .filter(|folder| self.using.contains(folder))
            .cloned()
            .collect();
        (!self.unread).then_some(used)
    }

    fn proves(&mut self, copy: &Path) -> bool {
        self.run.push(copy.to_path_buf());
        if self.lingering.iter().any(|lingering| lingering == copy) {
            self.using.extend(copy.parent().map(Path::to_path_buf));
            return true;
        }
        self.printing.iter().any(|printing| printing == copy)
    }
}

/// This host's own reading ([`ThisHost`]) of the processes `.0` alone, so what this account's
/// other processes do changes nothing here; nothing is run to prove a copy.
struct Narrowed(BTreeSet<u32>);

impl Host for Narrowed {
    fn listed(&mut self) -> Option<BTreeSet<u32>> {
        Some(self.0.clone())
    }

    fn uses(&mut self, listed: &BTreeSet<u32>, folders: &[PathBuf]) -> Option<BTreeSet<PathBuf>> {
        ThisHost.uses(listed, folders)
    }

    fn proves(&mut self, copy: &Path) -> bool {
        panic!("{copy:?} showed it ran, so nothing runs it again")
    }
}

/// Sets the time of the last use of the copy in `folder` ([`USED`]) to `at`.
fn set_used(folder: &Path, at: SystemTime) {
    let mark = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(folder.join(USED))
        .expect("the use's mark");
    mark.set_modified(at).expect("the use's time");
}

/// A copy's folder `name` in `root` as a suite leaves it: a copy (some bytes, which nothing here
/// runs), the records `users`, the mark that a child ran from the copy when `ran`, and its use
/// marked at `used`.
fn made(root: &Path, name: &str, used: SystemTime, ran: bool, users: &[String]) -> PathBuf {
    let folder = root.join(name);
    std::fs::create_dir(&folder).expect("a copy's folder");
    std::fs::write(folder.join(ATERM), b"a copy").expect("a copy");
    for user in users {
        std::fs::write(folder.join(user), b"").expect("a record");
    }
    if ran {
        aterm_uds::deadline::try_mark_ran(&folder).expect("the mark that the copy ran");
    }
    set_used(&folder, used);
    folder
}

/// The names in `root` that do not start with a dot, in order.
fn names(root: &Path) -> Vec<String> {
    let entries = std::fs::read_dir(root).expect("the scratch folder");
    let mut names: Vec<String> = entries
        .map(|entry| entry.expect("an entry").file_name())
        .filter_map(|name| name.into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    names
}

/// How many runs of the copy in `folder` wrote no line ([`UNPROVEN`]).
fn unproven(folder: &Path) -> u64 {
    std::fs::metadata(folder.join(UNPROVEN)).map_or(0, |meta| meta.len())
}

/// `minutes` minutes.
const fn minutes(minutes: u64) -> Duration {
    Duration::from_secs(minutes * 60)
}

/// A copy of aterm the suites ran leaves once no use has marked it for an hour, no process that
/// resolved it lives, no live process uses it and a child showed it ran from it (or, run now,
/// writes a line and leaves nothing using it); any other copy stays. One a live process
/// resolved or uses, or kept for want of a whole reading or of a proof, is marked used, as is
/// every aged copy while this account's processes cannot be listed. A use marked while the
/// sweep reads keeps its copy. One reading of the processes covers every aged copy, and a
/// sweep runs one unproven copy, leaving the next for the next sweep. A real copy of aterm
/// proves it ran; a program that writes nothing proves nothing.
///
/// Negative control: a sweep whose wait was a day keeps `unused`, `printing`, `reused` and
/// `cut-short`; one that skipped the hour removes `fresh`; one that skipped the live use removes
/// `running`; one that skipped the proof removes `silent`; one that ran every unproven copy runs
/// `silent` in the first sweep; one that took a listed pid's record as live without its serial
/// keeps `reused`; one that skipped the check under the lock removes `marked`; one that read
/// the processes for each folder reads seven times; one that judged without a listing removes
/// `unlisted`, and one that took a reading that was not whole removes `unread`; one that marked
/// nothing leaves `running`, `resolved`, `silent`, `unlisted` and `unread` aged. A proof that took
/// a run for a line passes `/usr/bin/true` (macOS), which writes nothing. Each fails an
/// assertion here.
#[test]
fn an_unused_copy_of_aterm_leaves_after_an_hour_and_one_a_live_process_runs_stays() {
    let scratch = aterm_tempfile::tempdir().expect("a scratch folder");
    let root = scratch.path();
    let now = SystemTime::now();
    let old = now - minutes(61);
    let recent = now - minutes(59);
    let pid = std::process::id();
    let serial = aterm_uds::process::process_serial(i32::try_from(pid).expect("a pid"));
    let serial = serial.expect("this process's serial");
    let this = [user_name(pid, Some(serial))];
    let gone = [
        user_name(pid, Some(serial + 1)),
        user_name(u32::MAX, Some(1)),
    ];

    let unused = made(root, "unused", old, true, &[]);
    let running = made(root, "running", old, true, &[]);
    let fresh = made(root, "fresh", recent, true, &[]);
    let printing = made(root, "printing", old, false, &[]);
    let silent = made(root, "silent", old, false, &[]);
    let resolved = made(root, "resolved", old, true, &this);
    let reused = made(root, "reused", old, true, &gone);
    let marked = made(root, "marked", old, true, &[]);
    let own = made(root, "own", old, false, &[]);
    // A publish cut short: no copy and no mark of a use, the folder last written over an hour ago.
    let cut_short = root.join("cut-short");
    std::fs::create_dir(&cut_short).expect("a folder");
    std::fs::File::open(&cut_short)
        .and_then(|folder| folder.set_modified(old))
        .expect("the folder's time");
    let killed = root.join(format!("{LEAVING}gone.1"));
    std::fs::create_dir(&killed).expect("a folder a killed sweep left");
    std::fs::write(killed.join(ATERM), b"a copy").expect("its copy");

    let mut host = Answers {
        listed: Some(BTreeSet::from([pid])),
        using: vec![running.clone()],
        printing: vec![printing.join(ATERM)],
        marking: vec![(marked.clone(), now)],
        ..Answers::default()
    };
    sweep(root, &own, OsStr::new(ATERM), now, &mut host);
    assert_eq!(
        names(root),
        ["fresh", "marked", "own", "resolved", "running", "silent"],
        "the copies that stay"
    );
    assert!(!killed.exists(), "a folder a killed sweep left is removed");
    let every = vec![
        cut_short,
        marked.clone(),
        printing.clone(),
        reused,
        running.clone(),
        silent.clone(),
        unused,
    ];
    assert_eq!(
        host.read,
        [every, vec![printing.clone()]],
        "one reading of every aged folder no live process resolved, and one after the proof"
    );
    assert_eq!(
        host.run,
        [printing.join(ATERM)],
        "one unproven copy nothing uses is run, and never this process's own"
    );
    assert!(
        aged(&silent, now),
        "the copy left for the next sweep is not marked"
    );
    for kept in [&running, &resolved, &marked] {
        assert!(!aged(kept, now), "{kept:?} is marked used");
    }
    assert!(!aged(&fresh, now), "a copy used within the hour");

    host.marking.clear();
    sweep(root, &own, OsStr::new(ATERM), now, &mut host);
    assert_eq!(
        host.run,
        [printing.join(ATERM), silent.join(ATERM)],
        "the next sweep runs the next unproven copy"
    );
    assert!(silent.is_dir(), "a copy that wrote nothing stays");
    assert!(
        !silent.join(aterm_uds::deadline::RAN).exists(),
        "and is not marked as run"
    );
    assert!(!aged(&silent, now), "but is marked used");
    assert_eq!(unproven(&silent), 1, "and its run counted");

    let unlisted = made(root, "unlisted", old, true, &[]);
    let mut blind = Answers::default();
    sweep(root, &own, OsStr::new(ATERM), now, &mut blind);
    assert!(
        unlisted.is_dir(),
        "no copy leaves while no listing reads this account's processes"
    );
    assert!(!aged(&unlisted, now), "each aged copy is marked used");
    assert!(
        blind.run.is_empty() && blind.read.is_empty(),
        "and none is read or run"
    );

    let unread = made(root, "unread", old, true, &[]);
    let mut partial = Answers {
        listed: Some(BTreeSet::from([pid])),
        unread: true,
        ..Answers::default()
    };
    sweep(root, &own, OsStr::new(ATERM), now, &mut partial);
    assert!(
        unread.is_dir(),
        "no copy leaves on a reading that is not whole"
    );
    assert!(!aged(&unread, now), "each aged copy is marked used");

    assert!(
        prove(&aterm_executable()),
        "the aterm under test writes its version"
    );
    assert!(
        !prove(&root.join("absent")),
        "a program that is not there proves nothing"
    );
    #[cfg(target_vendor = "apple")]
    assert!(
        !prove(Path::new("/usr/bin/true")),
        "a program that runs and writes nothing proves nothing"
    );
}

/// A copy whose runs wrote no line is run again only once its wait, an hour, has doubled for
/// each such run (two hours after the first, four after the second), and leaves once a run
/// writes a line.
///
/// Negative control: a sweep that did not double the wait runs the copy again an hour after
/// its first run, and one that did not count the runs runs it again two hours after its second.
#[test]
fn a_copy_no_run_proved_is_run_again_only_after_a_doubled_wait() {
    let scratch = aterm_tempfile::tempdir().expect("a scratch folder");
    let root = scratch.path();
    let now = SystemTime::now();
    let own = root.join("own");
    let silent = made(root, "silent", now - minutes(61), false, &[]);
    let copy = silent.join(ATERM);
    let mut host = Answers {
        listed: Some(BTreeSet::from([std::process::id()])),
        ..Answers::default()
    };
    let sweep_at = |at: SystemTime, host: &mut Answers| {
        sweep(root, &own, OsStr::new(ATERM), at, host);
    };
    sweep_at(now, &mut host);
    assert_eq!((host.run.len(), unproven(&silent)), (1, 1));
    sweep_at(now + minutes(61), &mut host);
    assert_eq!(host.run.len(), 1, "an hour on, the wait is two");
    sweep_at(now + minutes(121), &mut host);
    assert_eq!((host.run.len(), unproven(&silent)), (2, 2));
    sweep_at(now + minutes(121 + 239), &mut host);
    assert_eq!(
        host.run.len(),
        2,
        "after the second run, the wait is four hours"
    );
    host.printing.push(copy.clone());
    sweep_at(now + minutes(121 + 241), &mut host);
    assert_eq!(host.run, [copy.clone(), copy.clone(), copy]);
    assert!(!silent.exists(), "a run that wrote a line lets it leave");
}

/// A copy whose run, its proof, left a process running from its folder stays while that process
/// does, marked used and marked as run, and leaves without being run again once the process
/// ended.
///
/// Negative control: a sweep that judged the copy by the reading it took before the run removes
/// the folder that process runs from.
#[test]
fn a_copy_its_proof_left_running_stays_while_that_process_lives() {
    let scratch = aterm_tempfile::tempdir().expect("a scratch folder");
    let root = scratch.path();
    let now = SystemTime::now();
    let own = root.join("own");
    let lingering = made(root, "lingering", now - minutes(61), false, &[]);
    let mut host = Answers {
        listed: Some(BTreeSet::from([std::process::id()])),
        lingering: vec![lingering.join(ATERM)],
        ..Answers::default()
    };
    sweep(root, &own, OsStr::new(ATERM), now, &mut host);
    assert!(lingering.is_dir(), "the process its run left keeps it");
    assert!(lingering.join(aterm_uds::deadline::RAN).is_file());
    assert!(!aged(&lingering, now), "it is marked used");
    host.using.clear();
    sweep(root, &own, OsStr::new(ATERM), now + minutes(61), &mut host);
    assert!(!lingering.exists(), "once that process ended, it leaves");
    assert_eq!(host.run.len(), 1, "it is not run again");
}

/// A copy a live process runs stays while this host's own reading of that process finds it
/// there ([`ThisHost`]: the path the kernel names the process's executable by, its mappings,
/// descriptors and working directory), however long ago its use was marked, and is marked used
/// then; once the process ended, the copy leaves. The copy is a signed copy of `cat`, named
/// `cat` (a multicall build of it picks its tool by that name), which showed it ran from its
/// file by echoing a line ([`aterm_uds::deadline::cat_echoes`]) before anything here may remove
/// it.
///
/// Negative control: a reading that found no use of the folder (`ThisHost::uses` answering
/// none) removes it while `cat` runs from it, and one that kept every folder keeps it once `cat`
/// ended.
#[test]
fn a_copy_a_live_process_runs_stays_by_this_hosts_own_reading() {
    use aterm_uds::spawnfd::Stdio;

    const CAT: &str = "cat";
    let scratch = aterm_tempfile::tempdir().expect("a scratch folder");
    // The kernel names an executable by its physical path (under `/private` on macOS).
    let root = std::fs::canonicalize(scratch.path()).expect("the scratch folder's path");
    let mut kept = aterm_uds::deadline::KeptUntilRan::new(scratch.keep());
    let now = SystemTime::now();
    let folder = root.join("running");
    std::fs::create_dir(&folder).expect("a copy's folder");
    aterm_uds::deadline::ad_hoc_copy(Path::new("/bin/cat"), &folder.join(CAT));
    let mut cat = aterm_uds::spawnfd::command(folder.join(CAT))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("the copy of cat");
    let input = aterm_uds::deadline::cat_echoes(&mut cat);
    kept.ran();
    aterm_uds::deadline::try_mark_ran(&folder).expect("the mark that it ran");
    set_used(&folder, now - minutes(61));
    let own = root.join("own");
    let mut host = Narrowed(BTreeSet::from([cat.id(), std::process::id()]));
    sweep(&root, &own, OsStr::new(CAT), now, &mut host);
    assert!(
        folder.join(CAT).is_file(),
        "a copy a live process runs stays"
    );
    assert!(!aged(&folder, now), "and is marked used");

    drop(input);
    let mut patience = crate::patience::Patience::new(Duration::from_secs(60));
    while !cat.exited().expect("cat's state") {
        if let Some(why) = patience.spent() {
            panic!("cat never ended at its input's end ({why})");
        }
        aterm_uds::deadline::poll_yield();
    }
    let ended = cat.wait().expect("cat's end");
    assert!(ended.success(), "{ended:?}");
    sweep(&root, &own, OsStr::new(CAT), now + minutes(61), &mut host);
    assert!(!folder.exists(), "once nothing runs it, it leaves");
}

/// A process's first resolution of its copy records the process in the copy's folder
/// (`.user.<pid>.<serial>`) and marks the use, and a later call marks the use again once a
/// minute has passed since the last mark, not before ([`remark`]). The copy here is a file of
/// bytes nothing runs.
///
/// Negative control: a mark that made no record leaves no record file; a call that marked the
/// use at every call marks it within the minute, and one that never marked it again leaves it
/// two hours old a minute on.
#[test]
fn a_use_is_recorded_with_its_mark_and_marked_again_after_a_minute() {
    let scratch = aterm_tempfile::tempdir().expect("a scratch folder");
    let built = scratch.path().join("build").join(ATERM);
    std::fs::create_dir(scratch.path().join("build")).expect("its folder");
    std::fs::write(&built, b"a build nothing runs").expect("a build");
    let mut copy = snapshot(&built);
    let bytes = std::fs::read(&copy.path).expect("the copy");
    assert_eq!(bytes, b"a build nothing runs");
    let pid = std::process::id();
    let serial = aterm_uds::process::process_serial(i32::try_from(pid).expect("a pid"));
    let record = copy.folder.join(user_name(pid, serial));
    assert!(record.is_file(), "the use's record {record:?}");
    assert!(!aged(&copy.folder, SystemTime::now()), "the use's mark");

    set_used(&copy.folder, SystemTime::now() - minutes(120));
    let marked = copy.marked;
    remark(&mut copy, marked + REMARK - Duration::from_millis(1));
    assert!(
        aged(&copy.folder, SystemTime::now()),
        "a call within the minute marks nothing"
    );
    remark(&mut copy, marked + REMARK);
    assert!(
        !aged(&copy.folder, SystemTime::now()),
        "a call a minute on marks the use again"
    );
    assert_eq!(copy.marked, marked + REMARK);
}
