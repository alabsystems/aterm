// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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

    static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
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

    static FULL: AtomicUsize = AtomicUsize::new(super::CHECKS_IN_FLIGHT_MAX);
    static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
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
/// volume (not local), so a network home is not judged by `/`; a
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

#[test]
fn a_start_that_fell_back_reaches_the_queue_and_one_that_did_not_says_nothing() {
    if !crate::message_inbox::lane_zero_alone() {
        return;
    }
    let _ = take_noted();
    let fault = |dir: &str| (dir != "/Users//me" && dir != "/work/here").then_some(Fault::Missing);
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
