// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::os::unix::fs::MetadataExt as _;
use std::time::{Duration, Instant};

use super::{ffi, hook};
use crate::exitwatch::ExitWatch;

const HANG: Duration = Duration::from_secs(60);

#[test]
fn a_valid_native_listener_survives_scheduling_delay() {
    let dir = aterm_tempfile::Builder::new().tempdir_in("/tmp").unwrap();
    let path = dir.path().join("l.sock");
    let listener = crate::kept_out(|| std::os::unix::net::UnixListener::bind(&path)).unwrap();
    let actual = super::serving_pid_at(
        &path,
        super::SCAN_DEADLINE,
        Instant::now() - Duration::from_secs(11),
    );
    assert_eq!(actual.unwrap(), Some(std::process::id()));
    assert_eq!(
        super::serving_pid_at(
            &path,
            super::SCAN_DEADLINE,
            Instant::now() - Duration::from_secs(61)
        )
        .unwrap_err()
        .kind(),
        std::io::ErrorKind::TimedOut
    );
    drop(listener);
}

unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
    fn posix_openpt(flags: i32) -> i32;
    fn fcntl(fd: i32, cmd: i32, ...) -> i32;
}

const LOW: i32 = 210;
const HIGH: i32 = 240;

fn dup_at(fd: &std::os::fd::OwnedFd, at: i32) -> std::os::fd::OwnedFd {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    const F_DUPFD_CLOEXEC: i32 = 67;
    // SAFETY: `F_DUPFD_CLOEXEC` duplicates a valid descriptor we own.
    let high = unsafe { fcntl(fd.as_raw_fd(), F_DUPFD_CLOEXEC, at) };
    assert!(
        high >= at,
        "F_DUPFD_CLOEXEC at {at}: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: wraps the newly duplicated descriptor.
    unsafe { std::os::fd::OwnedFd::from_raw_fd(high) }
}

const EREDRIVEOPEN: i32 = -6;

/// A fresh PTY master (a device no other process holds) at the lowest free number at or
/// above `at`, and its rdev.
pub(super) fn a_master_at(at: i32) -> (std::os::fd::OwnedFd, u64) {
    use std::os::fd::FromRawFd as _;
    const O_RDWR: i32 = 2;
    const O_NOCTTY: i32 = 0x20000;
    const O_CLOEXEC: i32 = 0x0100_0000;
    let started = Instant::now();
    let raw = loop {
        // SAFETY: `posix_openpt` returns a fresh descriptor owned by this test.
        let raw = unsafe { posix_openpt(O_RDWR | O_NOCTTY | O_CLOEXEC) };
        if raw >= 0 {
            break raw;
        }
        let err = std::io::Error::last_os_error();
        assert_eq!(
            err.raw_os_error(),
            Some(EREDRIVEOPEN),
            "posix_openpt: {err}"
        );
        assert!(
            started.elapsed() < HANG,
            "posix_openpt lost every race for {HANG:?}"
        );
    };
    // SAFETY: wraps the fresh descriptor owned by this test.
    let low = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
    let fd = dup_at(&low, at);
    drop(low);
    let rdev = std::fs::File::from(fd.try_clone().expect("dup"))
        .metadata()
        .expect("fstat")
        .rdev();
    (fd, rdev)
}

#[test]
fn a_descriptor_closed_between_the_listing_and_its_read_is_not_held() {
    use std::os::fd::AsRawFd as _;
    let me = std::process::id();
    let (fd, rdev) = a_master_at(LOW);
    let n = fd.as_raw_fd();
    assert_eq!(super::process_holds_rdev(me, rdev), Some(true), "held");
    let named = std::rc::Rc::new(std::cell::Cell::new(false));
    let seen = std::rc::Rc::clone(&named);
    hook::after_next_listing(move |_, fds| {
        seen.set(fds.contains(&(n, ffi::PROX_FDTYPE_VNODE)));
        drop(fd);
    });
    assert_eq!(
        super::process_holds_rdev(me, rdev),
        Some(false),
        "closed after the listing: not held, and not unknown"
    );
    assert!(named.get(), "the listing named the descriptor it closed");

    let (low, rdev) = a_master_at(LOW);
    let kept = dup_at(&low, HIGH);
    let (n, k) = (low.as_raw_fd(), kept.as_raw_fd());
    let order = std::rc::Rc::new(std::cell::Cell::new(None));
    let seen = std::rc::Rc::clone(&order);
    hook::after_next_listing(move |_, fds| {
        let at = |fd| fds.iter().position(|e| *e == (fd, ffi::PROX_FDTYPE_VNODE));
        seen.set(Some((at(n), at(k))));
        drop(low);
    });
    assert_eq!(
        super::process_holds_rdev(me, rdev),
        Some(true),
        "the closed descriptor's EBADF ends only its own read"
    );
    match order.get() {
        Some((Some(closed), Some(open))) => assert!(
            closed < open,
            "the scan meets the closed descriptor before the open one"
        ),
        other => panic!("the listing named both descriptors: {other:?}"),
    }
    drop(kept);

    use super::Failed;
    let vnode = super::Vnode {
        dev: 7,
        ino: 9,
        rdev: 11,
    };
    let unasked = |_: &[i32]| -> bool { panic!("a refusal confirmed where none is asked") };
    for lookup in [super::Lookup::BY_FILE, super::Lookup::BY_RDEV] {
        let wanted = (lookup.key)(&vnode);
        let fds = || vec![(3, ffi::PROX_FDTYPE_VNODE)];
        assert_eq!(
            super::read_listed_table(&[wanted], lookup, fds(), |_| Err(Failed::Refused), unasked),
            [super::TableRead::Unreadable],
            "EPERM is unknown for every unconfirmed key"
        );
        assert_eq!(
            super::read_listed_table(&[wanted], lookup, fds(), |_| Ok(vnode), unasked),
            [super::TableRead::Holds],
            "a positive match remains a positive match"
        );
        assert_eq!(
            super::read_listed_table(
                &[wanted, wanted + 1],
                lookup,
                vec![(3, ffi::PROX_FDTYPE_VNODE), (4, ffi::PROX_FDTYPE_VNODE)],
                |fd| if fd == 3 {
                    Err(Failed::Refused)
                } else {
                    Ok(vnode)
                },
                |_| false,
            ),
            [super::TableRead::Holds, super::TableRead::Unreadable],
            "a refusal obscures only keys not positively found"
        );
    }
}

#[test]
fn errno_is_classified_into_holder_failure_kinds() {
    use super::Failed;
    assert_eq!(Failed::of(3), Failed::Exited);
    assert_eq!(Failed::of(9), Failed::Closed);
    assert_eq!(Failed::of(2), Failed::Dead);
    assert_eq!(Failed::of(1), Failed::Refused);
    for other in [0, 12, 13, 22] {
        assert_eq!(Failed::of(other), Failed::Other, "errno {other}");
    }
}

#[test]
fn a_refusal_shown_to_be_the_vnodes_is_not_the_file() {
    use super::{Failed, Lookup, TableRead};
    const OTHER: super::Vnode = super::Vnode {
        dev: 7,
        ino: 9,
        rdev: 11,
    };
    const SOUGHT: super::Vnode = super::Vnode {
        ino: 10,
        rdev: 12,
        ..OTHER
    };
    let fds = || vec![(3, ffi::PROX_FDTYPE_VNODE), (4, ffi::PROX_FDTYPE_VNODE)];
    let read: fn(i32) -> Result<super::Vnode, Failed> = |fd| {
        if fd == 3 {
            Err(Failed::Refused)
        } else {
            Ok(OTHER)
        }
    };
    let fold = |lookup: Lookup, read: fn(i32) -> Result<super::Vnode, Failed>, shown| {
        let asked = std::cell::Cell::new(None);
        let answer = super::read_listed_table(
            &[(lookup.key)(&SOUGHT)],
            lookup,
            fds(),
            read,
            |refused: &[i32]| {
                asked.set(Some(refused.to_vec()));
                shown
            },
        );
        (answer, asked.take())
    };
    assert_eq!(
        fold(Lookup::BY_FILE, read, true),
        (vec![TableRead::Clear], Some(vec![3])),
        "the vnode's refusal: not the file"
    );
    assert_eq!(
        fold(Lookup::BY_FILE, read, false),
        (vec![TableRead::Unreadable], Some(vec![3])),
        "a refusal not shown to be the vnode's"
    );
    assert_eq!(
        fold(Lookup::BY_FILE, |_| Err(Failed::Refused), true),
        (vec![TableRead::Unreadable], None),
        "no descriptor described: the process may have refused them all"
    );
    assert_eq!(
        fold(Lookup::BY_RDEV, read, true),
        (vec![TableRead::Unreadable], None),
        "a device's scan keeps a refusal unknown"
    );
    let held: fn(i32) -> Result<super::Vnode, Failed> = |fd| {
        if fd == 3 {
            Err(Failed::Refused)
        } else {
            Ok(SOUGHT)
        }
    };
    assert_eq!(
        fold(Lookup::BY_FILE, held, false),
        (vec![TableRead::Holds], None),
        "found elsewhere: nothing to confirm"
    );
}

#[test]
fn a_refusal_is_the_vnodes_only_when_every_read_says_so() {
    use super::{Failed, Vnode, refusal_shown};
    const V: u32 = ffi::PROX_FDTYPE_VNODE;
    const PIPE: u32 = 6;
    const FILE: Vnode = Vnode {
        dev: 1,
        ino: 2,
        rdev: 0,
    };
    let listed = || Ok(vec![(3, V), (4, V), (5, PIPE)]);
    let reads = |three: Result<Vnode, Failed>, four: Result<Vnode, Failed>| {
        move |fd: i32| match fd {
            3 => three,
            4 => four,
            other => panic!("descriptor {other} is no vnode the table lists"),
        }
    };
    for (refused, three, four, why) in [
        (
            &[3][..],
            Err(Failed::Refused),
            Ok(FILE),
            "refused again, another described",
        ),
        (&[3], Err(Failed::Closed), Ok(FILE), "closed since"),
        (&[3], Err(Failed::Dead), Ok(FILE), "dead since"),
        (&[9], Ok(FILE), Err(Failed::Refused), "no longer listed"),
    ] {
        assert!(
            refusal_shown(refused, listed(), reads(three, four)),
            "{why}"
        );
    }
    for failed in [Failed::Refused, Failed::Exited, Failed::Other] {
        assert!(
            !refusal_shown(&[3], Err(failed), reads(Ok(FILE), Ok(FILE))),
            "the listing {failed:?}"
        );
    }
    for (refused, three, four, why) in [
        (&[3][..], Ok(FILE), Ok(FILE), "described now"),
        (&[3], Err(Failed::Exited), Ok(FILE), "exited"),
        (&[3], Err(Failed::Other), Ok(FILE), "failed otherwise"),
        (
            &[3],
            Err(Failed::Refused),
            Err(Failed::Refused),
            "no other described",
        ),
        (
            &[3],
            Err(Failed::Refused),
            Err(Failed::Closed),
            "the other closed",
        ),
        (
            &[3, 4],
            Err(Failed::Refused),
            Err(Failed::Refused),
            "every vnode refused",
        ),
    ] {
        assert!(
            !refusal_shown(refused, listed(), reads(three, four)),
            "{why}"
        );
    }
    assert!(
        !refusal_shown(
            &[3],
            Ok(vec![(3, V), (5, PIPE)]),
            reads(Err(Failed::Refused), Ok(FILE))
        ),
        "only a pipe beside it"
    );
}

fn file_key(path: &std::path::Path) -> u128 {
    let meta = std::fs::metadata(path).expect("stat");
    super::Vnode {
        dev: u32::try_from(meta.dev() & u64::from(u32::MAX)).expect("a Darwin dev_t"),
        ino: meta.ino(),
        rdev: 0,
    }
    .file_key()
}

#[test]
fn a_table_left_unknown_is_outside_the_scan_once_its_process_runs_as_another_user() {
    use super::TableRead::{Clear, Exited, Holds, Left, Unreadable};
    let read = vec![Holds, Unreadable, Clear, Exited];
    assert_eq!(
        super::left_the_user(read.clone(), Some(0), 501),
        [Holds, Left, Clear, Exited],
        "another user's now: unknown keys leave, a key it held stays held"
    );
    assert_eq!(
        super::left_the_user(read.clone(), Some(501), 501),
        read,
        "still this user's: unknown stays unknown"
    );
    assert_eq!(
        super::left_the_user(read.clone(), None, 501),
        read,
        "its user unread: unknown stays unknown"
    );
}

/// A child killed and reaped when it drops, so a failed assertion or a hang detector leaves
/// nothing running (`top -l 0` samples forever).
struct KilledOnDrop(crate::spawnfd::Child);

impl Drop for KilledOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A process of this user's that becomes a set-user-ID program after a scan lists it
/// (`/bin/ps` run from a shell, measured leaving every scan unknown under a `ps` loop) is
/// outside the scan, as it is once listed so; `top` is one that stays to be read.
/// Negative control: without [`super::left_the_user`] its read is `Unreadable`.
#[test]
fn a_process_that_became_a_set_user_id_program_reads_left_not_unknown() {
    use super::{Lookup, TableRead};
    // SAFETY: `geteuid` is a side-effect-free getter.
    let me = unsafe { ffi::geteuid() };
    assert_ne!(me, 0, "a scan as root reads every process");
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let sought = tmp.path().join("sought");
    std::fs::write(&sought, b"sought").expect("write");
    let top = KilledOnDrop(
        crate::spawnfd::command("/usr/bin/top")
            .args(["-l", "0", "-s", "60"])
            .stdin(crate::spawnfd::Stdio::null())
            .stdout(crate::spawnfd::Stdio::null())
            .stderr(crate::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn top"),
    );
    let pid = top.0.id();
    let started = Instant::now();
    while crate::process::read_short_info(i32::try_from(pid).expect("a pid"))
        .is_none_or(|info| info.pbsi_uid == me)
    {
        assert!(started.elapsed() < HANG, "top never ran as its owner");
        crate::deadline::poll_yield();
    }
    let key = file_key(&sought);
    let read = super::read_table(pid, &[key], Lookup::BY_FILE);
    let asked = super::process_holds_rdev(pid, u64::from(u32::MAX - 1));
    drop(top);
    assert_eq!(read, [TableRead::Left], "outside this user's scan");
    assert_eq!(asked, None, "asked of that process alone, unknown");
}

#[test]
fn a_refusal_is_cleared_only_once_libproc_shows_it_is_the_vnodes() {
    use super::{Lookup, TableRead};
    use std::io::ErrorKind;
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let (held, sought, kept) = (
        tmp.path().join("held"),
        tmp.path().join("sought"),
        tmp.path().join("kept"),
    );
    for file in [&held, &sought, &kept] {
        std::fs::write(file, b"file").expect("write");
    }
    let open = |file: &std::path::Path| {
        std::os::fd::OwnedFd::from(std::fs::File::open(file).expect("open"))
    };
    let mut sleep = crate::spawnfd::LaneCommand::new("/bin/sleep");
    sleep
        .arg("600")
        .stdin(crate::spawnfd::ChildStdio::Fd(open(&held)))
        .stdout(crate::spawnfd::ChildStdio::Null)
        .stderr(crate::spawnfd::ChildStdio::Null);
    // `kept` at fds 3 and 4 from the spawn on, for the child's whole life.
    let mut child =
        crate::spawnfd::spawn_with_two_fds(sleep, open(&kept), open(&kept)).expect("spawn");
    let pid = child.id();
    let started = Instant::now();
    while super::read_table(pid, &[file_key(&held)], Lookup::BY_FILE) != [TableRead::Holds] {
        assert!(started.elapsed() < HANG, "the child never showed the file");
        crate::deadline::poll_yield();
    }
    let key = file_key(&sought);
    // "Nothing described" refuses every number up to 255, far past any the child holds, not
    // just 0, 1 and 2: the child holds `kept` at fds 3 and 4 for its whole life, and while
    // dyld starts `sleep` it holds vnodes of its own (fd 3 and up, gone within milliseconds,
    // measured 2026-10-29 under load). One left unrefused reads, and the refusal is then
    // rightly found to be the vnodes'.
    let every: Vec<i32> = (0..256).collect();
    let table = |fds: &[i32], reads: usize, key: u128, lookup: Lookup| {
        hook::refusing(pid, fds, reads, || super::read_table(pid, &[key], lookup))
    };
    let file = Lookup::BY_FILE;
    assert_eq!(
        table(&[0], hook::ALWAYS, key, file),
        [TableRead::Clear],
        "the vnode's"
    );
    assert_eq!(
        table(&[], hook::ALWAYS, key, file),
        [TableRead::Clear],
        "none refused"
    );
    assert_eq!(
        table(&[0], 1, key, file),
        [TableRead::Unreadable],
        "it did not last"
    );
    assert_eq!(
        table(&every, hook::ALWAYS, key, file),
        [TableRead::Unreadable],
        "nothing described"
    );
    let unheld = u128::from(u32::MAX - 1);
    assert_eq!(
        table(&[0], hook::ALWAYS, unheld, Lookup::BY_RDEV),
        [TableRead::Unreadable],
        "a device's scan"
    );
    let scan = |reads| {
        hook::refusing(pid, &[0], reads, || {
            super::scan_file_holders(&sought, &[]).expect("scan")
        })
    };
    let cleared = scan(hook::ALWAYS);
    assert!(
        !cleared.unreadable.contains(&pid) && !cleared.holds(pid),
        "{cleared:?}"
    );
    let unknown = scan(1);
    assert!(unknown.unreadable.contains(&pid), "{unknown:?}");
    let files = |fds: &[i32], reads| {
        hook::refusing(pid, fds, reads, || super::open_files(pid)).map_err(|e| e.kind())
    };
    let named = |p: &std::path::PathBuf| p.file_name() == Some(std::ffi::OsStr::new("held"));
    let shown = files(&[0], hook::ALWAYS).expect("its open files");
    assert!(
        !shown.iter().any(named) && shown.iter().any(|p| p.as_os_str() == "/dev/null"),
        "{shown:?}"
    );
    assert!(
        files(&[], hook::ALWAYS)
            .expect("unrefused")
            .iter()
            .any(named)
    );
    assert_eq!(
        files(&[0], 1),
        Err(ErrorKind::PermissionDenied),
        "it did not last"
    );
    assert_eq!(
        files(&every, hook::ALWAYS),
        Err(ErrorKind::PermissionDenied),
        "nothing described"
    );
    assert!(
        !super::refusal_is_the_vnodes(1, &[3], Lookup::BY_FILE),
        "a table refused to this uid"
    );
    let _ = child.kill();
    let _ = child.wait();
}

fn a_holder_of_dev_null() -> (crate::spawnfd::Child, u64) {
    let rdev = std::fs::metadata("/dev/null").expect("stat").rdev();
    let child = crate::spawnfd::command("/bin/sleep")
        .arg("600")
        .stdin(crate::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn");
    let started = Instant::now();
    while super::process_holds_rdev(child.id(), rdev) != Some(true) {
        assert!(
            started.elapsed() < HANG,
            "the child never showed the device"
        );
        crate::deadline::poll_yield();
    }
    (child, rdev)
}

/// SIGKILL `pid` and return once the kernel reports its exit: it is a
/// zombie then, until its parent (this test) reaps it.
pub(super) fn kill_unreaped(pid: u32) {
    let watch = ExitWatch::watch(pid).expect("watch the child");
    // SAFETY: sends `SIGKILL` to the child process spawned by this test.
    unsafe { kill(pid as i32, 9) };
    let started = Instant::now();
    while watch.exit_status().is_none() {
        assert!(started.elapsed() < HANG, "the child never exited");
        crate::deadline::poll_yield();
    }
}

#[test]
fn an_exited_process_still_listed_holds_nothing() {
    let (mut child, rdev) = a_holder_of_dev_null();
    let pid = child.id();
    kill_unreaped(pid);
    assert!(
        super::uid_pids().expect("listing").contains(&pid),
        "the zombie is still listed: the scan meets it"
    );
    assert_eq!(super::process_holds_rdev(pid, rdev), Some(false));
    let scan = super::scan_rdev_holders(rdev, &[]).expect("scan");
    assert!(!scan.holds(pid), "{scan:?}");
    assert!(!scan.unreadable.contains(&pid), "{scan:?}");
    let _ = child.wait();
    assert_eq!(
        super::process_holds_rdev(1, rdev),
        None,
        "a refused table is unknown"
    );
}

#[test]
fn a_process_that_exits_after_its_listing_holds_nothing() {
    let (mut child, rdev) = a_holder_of_dev_null();
    let pid = child.id();
    let listed = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let seen = std::rc::Rc::clone(&listed);
    hook::after_next_listing(move |at, fds| {
        assert_eq!(at, pid as i32);
        seen.set(
            fds.iter()
                .filter(|(_, kind)| *kind == ffi::PROX_FDTYPE_VNODE)
                .count(),
        );
        kill_unreaped(pid);
    });
    assert_eq!(super::process_holds_rdev(pid, rdev), Some(false));
    assert!(
        listed.get() > 0,
        "the listing named its vnodes, read after its exit"
    );
    let _ = child.wait();
}

#[test]
fn the_scan_finds_a_child_holding_a_character_device() {
    let rdev = std::fs::metadata("/dev/null").expect("stat").rdev();
    assert_ne!(rdev, 0, "/dev/null is a character device");
    let mut child = crate::spawnfd::command("/bin/sleep")
        .arg("600")
        .stdin(crate::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn");
    let started = std::time::Instant::now();
    loop {
        if super::process_holds_rdev(child.id(), rdev) == Some(true) {
            break;
        }
        assert!(
            started.elapsed().as_secs() < 60,
            "the child never showed the device"
        );
        crate::deadline::poll_yield();
    }
    let scan = super::scan_rdev_holders(rdev, &[]).expect("scan");
    assert!(scan.holds(child.id()), "{scan:?}");
    assert!(scan.scanned > 0);
    let skipped = super::scan_rdev_holders(rdev, &[child.id()]).expect("scan");
    assert!(!skipped.holds(child.id()), "skip is honoured");
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(
        super::process_holds_rdev(child.id(), rdev),
        Some(false),
        "gone: holds nothing"
    );
}

#[test]
fn one_walk_answers_for_every_device_asked() {
    let me = std::process::id();
    let (mut child, null) = a_holder_of_dev_null();
    let (master, pty) = a_master_at(220);
    let nobody = 0xfeed_0002;
    let asked = [null, pty, nobody, pty];
    let scans = super::scan_holders_of(&asked, &[]).expect("scan");
    assert_eq!(scans.len(), asked.len());
    assert!(scans[0].holds(child.id()), "{:?}", scans[0]);
    assert!(scans[1].holds(me) && scans[3].holds(me), "{scans:?}");
    assert!(
        !scans[1].holds(child.id()),
        "the child holds no PTY of ours"
    );
    assert!(scans[2].holders.is_empty(), "{:?}", scans[2]);
    for (rdev, scan) in asked.iter().zip(&scans) {
        let alone = super::scan_rdev_holders(*rdev, &[]).expect("scan");
        let pids = if *rdev == null {
            vec![child.id()]
        } else {
            vec![me, child.id()]
        };
        for pid in pids {
            assert_eq!(
                scan.holds(pid),
                alone.holds(pid),
                "rdev {rdev:#x}, pid {pid}"
            );
        }
    }
    let skipped = super::scan_holders_of(&asked, &[me, child.id()]).expect("scan");
    assert!(
        skipped.iter().all(|s| !s.holds(me) && !s.holds(child.id())),
        "{skipped:?}"
    );
    assert!(
        super::scan_holders_of(&[], &[])
            .expect("nothing")
            .is_empty()
    );
    drop(master);
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn a_shared_memory_descriptor_is_named_with_its_size_and_nothing_else_is() {
    use std::os::fd::AsFd as _;
    assert_eq!(ffi::PSHM_FDINFO_SIZE, 1192);
    assert_eq!(ffi::PSHM_VST_SIZE, 112);
    for len in [crate::shm::PAGE, 5 * crate::shm::PAGE] {
        // SAFETY: this private custody is queried only for metadata.
        let (_region, custody) = unsafe { crate::shm::Region::create(len) }.expect("a region");
        assert_eq!(super::fd_is_pshm(custody.as_fd()), Some(len as u64));
        // SAFETY: this private writable descriptor is queried only for metadata.
        let (_twin, writable) =
            unsafe { crate::shm::testing::create_with_writable_custody(len) }.expect("a twin");
        assert_eq!(super::fd_is_pshm(writable.as_fd()), Some(len as u64));
    }
    let file = std::fs::File::open("/dev/null").expect("/dev/null");
    assert_eq!(super::fd_is_pshm(file.as_fd()), None, "a vnode");
    let (a, _b) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
    assert_eq!(super::fd_is_pshm(a.as_fd()), None, "a socket");
    let (r, _w) = crate::pipe().expect("a pipe");
    assert_eq!(super::fd_is_pshm(r.as_fd()), None, "a pipe");
}

#[test]
fn a_process_s_shared_memory_descriptors_are_listed_with_their_sizes() {
    let len = 13 * crate::shm::PAGE;
    let mine = || {
        super::pshm_sizes_of(std::process::id())
            .expect("our own table")
            .into_iter()
            .filter(|s| *s == len as u64)
            .count()
    };
    assert_eq!(mine(), 0);
    // SAFETY: the fixture only inspects the descriptor table and drops these owners.
    let (region, custody) = unsafe { crate::shm::Region::create(len) }.expect("a region");
    assert_eq!(mine(), 1, "the custody descriptor");
    drop(region);
    assert_eq!(mine(), 1, "the mapping was no descriptor");
    drop(custody);
    assert_eq!(mine(), 0, "closed");
}

/// One table read's rows name the holders: a live listener of this user's at the target, once,
/// or at its private sibling with its pid the one the kernel names for the listener the path
/// reaches, whatever pid the sibling's name holds; not another user's, a dead binder's, a
/// skipped one's, one bound elsewhere, or another pid's sibling (one whose name was removed
/// beside the broker's, or any when nothing listening is reached). The kernel is asked once,
/// and not at all with no such sibling row. A read that is no table, or one without the
/// check's own listener, is refused, and so is one whose connection did not answer, or
/// reached a listener whose pid no row binds at the path or a sibling, live or not (a broker
/// started after the read): whether a sibling holds the path is then unknown. A reached pid
/// that such a row names, or that is skipped, leaves the rules above to answer. NEGATIVE
/// CONTROLS: take any sibling, and 47 and 48 join; read a connection that did not answer as
/// nothing listening, and the scan names 41; drop the check of the reached pid, and a broker
/// the read does not hold leaves a scan naming 41 and 44 alone.
#[test]
fn a_table_read_names_its_live_binders_once_it_holds_its_own_listener() {
    use aterm_sysprobe::jobs::darwin::UnixListenerRow as Row;
    let row = |pid, uid, path: &'static [u8]| Row { pid, uid, path };
    let own = row(7, 501, b"/t/aterm-pcb-7-0.sock");
    let bus = b"/nowhere-folder/bus.sock";
    let target = super::SocketPath::of(std::path::Path::new("/nowhere-folder/bus.sock"));
    let rows = [
        own,
        row(41, 501, bus),
        row(41, 501, bus),
        row(42, 0, bus),
        row(43, 501, bus),
        row(44, 501, bus),
        row(45, 501, b"/nowhere-folder/other.sock"),
        row(46, 501, b"/nowhere-folder/bus.sock.46.0"),
        row(47, 501, b"/nowhere-folder/bus.sock.47.0"),
        row(48, 501, b"/nowhere-folder/bus.sock.46.1"),
        row(49, 501, b"/nowhere-folder/other.sock.49.0"),
        row(50, 0, b"/nowhere-folder/bus.sock.50.0"),
    ];
    let live = [7, 41, 42, 44, 45, 46, 47, 48, 49, 50];
    let asked = std::cell::Cell::new(0);
    let read = |rows: &[Row<'_>], served: Option<u32>| {
        let serving = || {
            asked.set(asked.get() + 1);
            Ok(served)
        };
        super::listener_holders(Some(rows), own, &target, serving, &live, &[44])
            .expect("a read holding its own listener")
    };
    let scan = read(&rows, Some(46));
    assert_eq!(scan.holders, [41, 46], "the broker's sibling: {scan:?}");
    assert_eq!(asked.get(), 1, "the kernel is asked once");
    assert_eq!(scan.scanned, 9, "the live processes but the skipped one");
    assert!(
        scan.unreadable.is_empty() && scan.sealed.is_empty(),
        "{scan:?}"
    );
    assert_eq!(
        read(&rows, None).holders,
        [41],
        "no sibling when nothing listening is reached"
    );
    assert_eq!(
        read(&rows, Some(48)).holders,
        [41, 48],
        "the pid in a sibling's name is not compared"
    );
    asked.set(0);
    assert_eq!(read(&rows[..7], Some(46)).holders, [41]);
    assert_eq!(asked.get(), 0, "no sibling row, no question");
    for refused in [None, Some(&rows[1..])] {
        let e = super::listener_holders(refused, own, &target, || Ok(None), &live, &[])
            .expect_err("a misread table");
        assert_eq!(e.kind(), std::io::ErrorKind::Unsupported, "{e}");
    }
    let unanswered = || Err(std::io::Error::from(std::io::ErrorKind::TimedOut));
    let e = super::listener_holders(Some(&rows), own, &target, unanswered, &live, &[44])
        .expect_err("a connection that did not answer");
    assert_eq!(e.kind(), std::io::ErrorKind::TimedOut, "{e}");
    let no_sibling =
        super::listener_holders(Some(&rows[..7]), own, &target, unanswered, &live, &[44])
            .expect("no sibling row, no question");
    assert_eq!(no_sibling.holders, [41], "{no_sibling:?}");
    for (served, why) in [
        (99, "a pid with no row, live since the listing or not"),
        (45, "a pid whose one row binds elsewhere"),
        (49, "a pid whose one row binds another path's sibling"),
    ] {
        for live in [&live[..], &[7, 41, 45, 46, 99]] {
            let e =
                super::listener_holders(Some(&rows), own, &target, || Ok(Some(served)), live, &[])
                    .expect_err(why);
            assert_eq!(e.kind(), std::io::ErrorKind::Other, "{why}: {e}");
        }
    }
    for served in [42, 43, 50] {
        let scan =
            super::listener_holders(Some(&rows), own, &target, || Ok(Some(served)), &live, &[])
                .expect("a pid a row binds at the path or a sibling");
        assert_eq!(scan.holders, [41, 44], "{served}: {scan:?}");
    }
    let skipped = super::listener_holders(Some(&rows), own, &target, || Ok(Some(99)), &live, &[99])
        .expect("a skipped pid");
    assert_eq!(skipped.holders, [41, 44], "{skipped:?}");
}

/// A connection whose `socket(2)` waits on the spawn lock past its hang detector answers
/// `TimedOut`, unknown, though a listener is there to take it; with the lock free the same
/// connection names that listener's binder. The lock is held until the first answer comes, so
/// only the detector can end its wait. NEGATIVE CONTROL: read the detector's expiry as no
/// listener (`Ok(None)`), and the held connection answers that nobody listens.
#[test]
fn a_connection_past_its_hang_detector_is_unknown() {
    use std::os::unix::net::{UnixListener, UnixStream};
    let dir = aterm_tempfile::Builder::new()
        .tempdir_in("/tmp")
        .expect("a scratch folder short enough for sun_path");
    let path = dir.path().join("l.sock");
    let listener = UnixListener::bind(&path).expect("bind");
    let (held_tx, held) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let creation = std::thread::spawn(move || {
        crate::kept_out(|| {
            held_tx.send(()).expect("the test waits for the hold");
            let _ = released.recv_timeout(HANG);
            UnixStream::pair()
        })
    });
    held.recv_timeout(HANG)
        .expect("the creation holds the spawn lock");
    let late = super::serving_pid(&path, Duration::from_millis(50));
    release
        .send(())
        .expect("the creation waits for its release");
    drop(
        creation
            .join()
            .expect("the creation")
            .expect("a kept-out pair"),
    );
    assert_eq!(
        late.map_err(|e| e.kind()),
        Err(std::io::ErrorKind::TimedOut),
        "a connection held past its hang detector"
    );
    assert_eq!(
        super::serving_pid(&path, HANG).expect("a connection with the lock free"),
        Some(std::process::id())
    );
    drop(listener);
}

/// A refused connection is no listener's only where the probe finds nothing bound to the
/// file; a stream socket there, a datagram socket that took the probe, or a probe that failed
/// leave it unknown (`ConnectionRefused`). NEGATIVE CONTROL: read every refusal as unknown
/// whatever the probe found, and the stale file's refusal is unknown.
#[test]
fn a_refusal_is_no_listeners_only_where_nothing_is_bound() {
    use super::Bound;
    let path = std::path::Path::new("/nowhere-folder/bus.sock");
    let refused = std::io::Error::from_raw_os_error(libc::ECONNREFUSED);
    assert_eq!(
        super::refused_by(path, &refused, Ok(Bound::Nothing)).map_err(|e| e.to_string()),
        Ok(None)
    );
    for (bound, what) in [
        (Ok(Bound::Stream), "a stream socket bound there"),
        (Ok(Bound::Datagram), "a datagram socket that took the probe"),
        (
            Err(std::io::Error::from_raw_os_error(libc::EACCES)),
            "a probe that failed",
        ),
    ] {
        let e = super::refused_by(path, &refused, bound).expect_err(what);
        assert_eq!(
            e.kind(),
            std::io::ErrorKind::ConnectionRefused,
            "{what}: {e}"
        );
    }
}

/// The check binds in a folder of its own, made owner-only, under the first absolute folder
/// that takes it: a relative one, a missing one and one too deep for `sun_path` pass it on and
/// keep nothing. With none that takes it the last refusal is the answer.
#[test]
fn the_table_check_binds_in_a_folder_of_its_own_under_the_first_that_takes_it() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::Builder::new()
        .tempdir_in("/tmp")
        .expect("a scratch folder short enough for sun_path");
    let base = dir.path().to_path_buf();
    let deep = base.join("d".repeat(120));
    std::fs::create_dir(&deep).expect("a folder too deep for sun_path");
    let missing = base.join("missing");
    let relative = std::path::PathBuf::from(".");
    let check = super::TableCheck::bind_in(&[
        relative.clone(),
        missing.clone(),
        deep.clone(),
        base.clone(),
    ])
    .expect("bound in the folder that takes it");
    assert_eq!(check.path.parent(), Some(check.dir.as_path()));
    assert_eq!(
        check.dir.parent(),
        Some(base.as_path()),
        "{}",
        check.path.display()
    );
    let mode = std::fs::symlink_metadata(&check.dir)
        .expect("its folder")
        .permissions()
        .mode();
    assert_eq!(mode & 0o7777, 0o700, "{mode:o}");
    assert_eq!(
        std::fs::read_dir(&deep).expect("the deep folder").count(),
        0,
        "the folder whose bind failed is gone"
    );
    drop(check);
    assert!(super::TableCheck::bind_in(&[relative, missing, deep]).is_err());
}

/// The check's listener is this process's at its path while it lives, and its file and folder
/// are gone once it is dropped.
#[test]
fn the_table_check_listener_leaves_no_file() {
    let check = super::TableCheck::bind().expect("a check listener");
    let (path, folder) = (check.path.clone(), check.dir.clone());
    assert!(
        std::fs::symlink_metadata(&path).is_ok(),
        "{}",
        path.display()
    );
    let table = super::unix_stream_table().expect("the table");
    let rows = aterm_sysprobe::jobs::darwin::unix_listeners(&table).expect("read as laid out");
    // SAFETY: `geteuid` is a side-effect-free getter.
    let uid = unsafe { ffi::geteuid() };
    assert!(
        rows.iter().any(|row| row.pid == std::process::id()
            && row.uid == uid
            && row.path == path.as_os_str().as_encoded_bytes()),
        "the table names the check's listener"
    );
    drop(check);
    assert!(
        std::fs::symlink_metadata(&path).is_err() && std::fs::symlink_metadata(&folder).is_err(),
        "{}",
        path.display()
    );
}

#[test]
fn actual_unix_stream_inventory_survives_unrelated_capacity_churn() {
    let check = super::TableCheck::bind().unwrap();
    let mut calls = 0;
    let table = super::unix_stream_table_with(
        || {
            calls += 1;
            if calls <= 4 {
                Err(std::io::Error::from_raw_os_error(libc::ENOMEM))
            } else {
                super::unix_stream_table_once()
            }
        },
        std::time::Instant::now(),
    )
    .unwrap();
    assert!(calls >= 5);
    let rows = aterm_sysprobe::jobs::darwin::unix_listeners(&table).unwrap();
    assert!(rows.iter().any(|row| row.pid == std::process::id()
        && row.path == check.path.as_os_str().as_encoded_bytes()));
    let mut called = false;
    assert_eq!(
        super::unix_stream_table_with(
            || {
                called = true;
                Ok(Vec::new())
            },
            std::time::Instant::now() - std::time::Duration::from_secs(61)
        )
        .unwrap_err()
        .kind(),
        std::io::ErrorKind::TimedOut
    );
    assert!(!called);
    let mut failures = 0;
    let error = super::unix_stream_table_with(
        || {
            failures += 1;
            Err(std::io::Error::from_raw_os_error(libc::EACCES))
        },
        std::time::Instant::now(),
    )
    .unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EACCES));
    assert_eq!(failures, 1);
}
