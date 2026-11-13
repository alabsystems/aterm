// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[test]
fn an_open_file_scan_follows_the_inode_after_its_name_is_replaced() {
    struct Child(crate::spawnfd::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let name = dir.path().join("gate");
    std::fs::write(&name, b"original").expect("original gate");
    let original = std::fs::File::open(&name).expect("open gate");
    let program = crate::deadline::stand_in(std::path::Path::new("/bin/sleep"));
    let child = Child(
        crate::spawnfd::command(program)
            .arg("600")
            .stdin(original.try_clone().expect("child's descriptor"))
            .stdout(crate::spawnfd::Stdio::null())
            .stderr(crate::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn holder"),
    );
    let started = std::time::Instant::now();
    while !super::scan_open_file_holders(&original, &[])
        .expect("original inode's holders")
        .holds(child.0.id())
    {
        assert!(started.elapsed() < std::time::Duration::from_secs(60));
        crate::deadline::poll_yield();
    }
    std::fs::rename(&name, dir.path().join("old-gate")).expect("move original");
    std::fs::write(&name, b"replacement").expect("replace name");
    assert!(
        super::scan_open_file_holders(&original, &[])
            .expect("pinned inode's holders")
            .holds(child.0.id())
    );
    assert!(
        !super::scan_file_holders(&name, &[])
            .expect("replacement inode's holders")
            .holds(child.0.id())
    );
}

/// Leave a socket file after closing its listener. On Apple, exclude a parallel test's
/// spawn across bind and drop, even on a kernel without close-on-fork.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn stale_socket(path: &std::path::Path) {
    crate::spawn_lock::creating(
        || std::os::unix::net::UnixListener::bind(path).map(drop),
        |()| Ok(()),
    )
    .expect("a listener closed, its file left");
}

/// A listing that still fills its buffer at the cap is refused, never cut short; one that
/// leaves a slot free is read whole, a non-positive slot dropped.
#[test]
fn a_listing_that_fills_its_buffer_at_the_cap_is_refused() {
    let mut rooms = Vec::new();
    let full = super::listed_growing(|pids| {
        rooms.push(pids.len());
        pids.fill(7);
        Ok(pids.len())
    });
    assert!(
        full.is_err(),
        "a listing cut at the cap: {:?}",
        full.map(|p| p.len())
    );
    assert_eq!(rooms.last(), Some(&super::MAX_LISTED_PIDS), "{rooms:?}");
    let mut asked = 0;
    let grown = super::listed_growing(|pids| {
        asked += 1;
        let n = 1500.min(pids.len());
        for (at, slot) in pids.iter_mut().take(n).enumerate() {
            *slot = i32::try_from(at).expect("small");
        }
        Ok(n)
    })
    .expect("a listing with room to spare");
    assert_eq!(asked, 2, "1024 slots filled, then 4096 with room");
    assert_eq!(grown.len(), 1499, "pid 0 is no process");
    assert!(super::listed_growing(|_| Err(std::io::Error::other("refused"))).is_err());
}

/// The effective uid, as the kernel names this process's owner.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn euid() -> u32 {
    // SAFETY: `geteuid` is a side-effect-free getter.
    unsafe { libc::geteuid() }
}

/// A uid's listing holds that uid's processes and no other's: this process is in its own
/// uid's list and not in root's, where launchd or init (pid 1) is.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[test]
fn pids_of_uid_lists_the_uid_it_is_given() {
    let me = std::process::id();
    let mine = super::pids_of_uid(euid()).expect("this uid's processes");
    assert!(mine.contains(&me), "this process in its own uid's list");
    assert!(
        super::uid_pids()
            .expect("the own-uid listing")
            .contains(&me),
        "uid_pids is this uid's list"
    );
    if euid() != 0 {
        let root = super::pids_of_uid(0).expect("root's processes");
        assert!(root.contains(&1), "pid 1 is root's: {} pids", root.len());
        assert!(!root.contains(&me), "this process is not root's");
        assert!(!mine.contains(&1), "pid 1 is not this uid's");
    }
}

/// A pty's terminal end, opened without making it this process's controlling terminal, and
/// the master that keeps it open. XNU's lost allocation race (`EREDRIVEOPEN`) is opened
/// again, within the one-minute hang detector.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn a_terminal() -> (std::os::fd::OwnedFd, std::fs::File) {
    use std::os::fd::FromRawFd as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    const EREDRIVEOPEN: i32 = -6;
    let started = std::time::Instant::now();
    let raw = loop {
        // SAFETY: `posix_openpt` returns a fresh descriptor this test owns.
        let raw = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
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
            started.elapsed() < std::time::Duration::from_secs(60),
            "posix_openpt lost every race"
        );
    };
    // SAFETY: wraps the fresh descriptor this test owns.
    let master = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
    // SAFETY: `grantpt` and `unlockpt` read the master this test owns.
    unsafe {
        assert_eq!(libc::grantpt(raw), 0, "grantpt");
        assert_eq!(libc::unlockpt(raw), 0, "unlockpt");
    }
    // The name is written into this buffer, never `ptsname`'s process-wide one, which a
    // test on another thread could overwrite.
    let mut buf = [0 as libc::c_char; 128];
    #[cfg(target_vendor = "apple")]
    // SAFETY: `TIOCPTYGNAME` writes a NUL-terminated name of at most 128 bytes into `buf`,
    // which has exactly that many.
    let rc = unsafe { libc::ioctl(raw, libc::TIOCPTYGNAME as libc::c_ulong, buf.as_mut_ptr()) };
    #[cfg(not(target_vendor = "apple"))]
    // SAFETY: `ptsname_r` writes a NUL-terminated name of at most `buf.len()` bytes.
    let rc = unsafe { libc::ptsname_r(raw, buf.as_mut_ptr(), buf.len()) };
    assert_eq!(
        rc,
        0,
        "the terminal end's name: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: `rc == 0`: `buf` holds a NUL-terminated name.
    let name = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    let terminal = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(&name)
        .unwrap_or_else(|e| panic!("open {name}: {e}"));
    (master, terminal)
}

/// Set in the copy of this test binary that runs one test alone (`runs_alone`).
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
const ALONE_ENV: &str = "ATERM_UDS_TEST_PTY_ALONE";

/// How long the copy that runs one test alone may run: a hang detector past the copy's own,
/// never a budget. A test that runs alone waits in turn on at most three things that must
/// happen, each under its one-minute hang detector (Linux's terminal test: its pty, its
/// session leader's line, the leader's loss of the terminal), and a fourth minute covers the
/// copy's start and its end, so a copy whose own wait hangs fails by its own message.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
const ALONE_HANG: std::time::Duration =
    std::time::Duration::from_secs(4 * crate::deadline::ECHO_HANG.as_secs());

/// Whether this is the copy of this test binary that runs the test `name` names alone; when
/// it is not, that copy is run (`--exact`, one thread) and must say it passed. A pty test
/// runs there: a fork another test takes while a master is open holds a copy of it until
/// that child execs or exits (one that never execs, or one held in `execve` while macOS
/// checks a fresh binary), so a master the test closed would stay open. The wait on the
/// copy is its hang detector (`ALONE_HANG`, `alone_within`).
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn runs_alone(name: &str) -> bool {
    alone_within(name, ALONE_HANG)
}

/// `runs_alone`, its wait on the copy ended at `hang` ([`crate::deadline::ends_within`]): a
/// copy macOS's program check still holds then ends the test COULD NOT RUN, and any other
/// fails it.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[track_caller]
fn alone_within(name: &str, hang: std::time::Duration) -> bool {
    if std::env::var_os(ALONE_ENV).is_some() {
        return true;
    }
    let out = crate::deadline::ends_within(
        crate::spawnfd::command(std::env::current_exe().expect("this test binary"))
            .args(["--exact", name, "--test-threads=1"])
            .env(ALONE_ENV, "1"),
        hang,
    );
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && said.contains("test result: ok. 1 passed"),
        "the copy: {said}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    false
}

/// A copy run alone that never ends fails its test at the wait's hang detector, killed and
/// reaped by then, and never hangs it. The copy parks until the binary that waits on it is
/// gone, or past the outer wait's own detector, so the 1 s detector runs out by
/// construction; the outer wait is the one-minute hang detector. NEGATIVE CONTROL: a copy
/// that ends and passes is `a_ptys_master_is_held_while_it_is_open`'s (macOS) and
/// `a_terminal_leaves_its_session_once_its_master_closes`'s (Linux).
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[test]
fn a_copy_run_alone_that_never_ends_fails_at_the_hang_detector() {
    const NAME: &str =
        "holders::listing_tests::a_copy_run_alone_that_never_ends_fails_at_the_hang_detector";
    if std::env::var_os(ALONE_ENV).is_some() {
        let waiter = std::os::unix::process::parent_id();
        let parked = std::time::Instant::now();
        while std::os::unix::process::parent_id() == waiter
            && parked.elapsed() < 2 * crate::deadline::ECHO_HANG
        {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        return;
    }
    let (tx, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let waited =
            std::panic::catch_unwind(|| alone_within(NAME, std::time::Duration::from_secs(1)));
        let _ = tx.send(waited.map_err(|why| why.downcast_ref::<String>().cloned()));
    });
    let waited = ended
        .recv_timeout(crate::deadline::ECHO_HANG)
        .expect("the wait on a copy that never ends ended within the one-minute hang detector");
    let why = waited
        .expect_err("a copy that never ends fails its test")
        .expect("the wait fails with its words");
    assert!(
        why.contains("did not finish in time: not within its 1s hang detector"),
        "{why}"
    );
    let copy = why
        .split_once("(pid ")
        .and_then(|(_, rest)| rest.split_once(')'))
        .and_then(|(pid, _)| pid.parse().ok())
        .unwrap_or_else(|| panic!("the words name the copy: {why}"));
    assert!(
        !crate::process::pid_alive(copy),
        "the copy (pid {copy}) was killed and reaped by the time its test failed"
    );
}

/// A pty's master is held while it is open and not once it closes, read from its
/// terminal's number, which is what a controlling terminal is numbered by (`e_tdev`, the
/// terminal's `st_rdev`). It runs alone (`runs_alone`). After the close it reads free, or
/// unknown while any table of this uid does not read, so there it is held only to not
/// reading held; which scan reads free is `master_held_by_reads_free_only_once_every_table_read`'s
/// to say. NEGATIVE CONTROL: another pty's master stays open throughout, so a reader that
/// took any master for this one's still reads it held.
#[cfg(target_vendor = "apple")]
#[test]
fn a_ptys_master_is_held_while_it_is_open() {
    use std::os::unix::fs::MetadataExt as _;
    if !runs_alone("holders::listing_tests::a_ptys_master_is_held_while_it_is_open") {
        return;
    }
    let (_other_master, _other) = a_terminal();
    let (master, terminal) = a_terminal();
    let number = terminal.metadata().expect("fstat the terminal").rdev() & 0xffff_ffff;
    assert_eq!(
        super::pty_master_held(number),
        Some(true),
        "its master is open"
    );
    drop(master);
    assert_ne!(
        super::pty_master_held(number),
        Some(true),
        "its master closed"
    );
}

/// A scan for a master reads it held once a table holds it, free only once every table read
/// and none did, and unknown while a table did not read or only root reads one. NEGATIVE
/// CONTROL: a fold that read no holder as unknown passes the live test above, and fails here.
#[test]
fn master_held_by_reads_free_only_once_every_table_read() {
    let scan = |holders: &[u32], unreadable: &[u32], sealed: &[u32]| super::HolderScan {
        holders: holders.to_vec(),
        unreadable: unreadable.to_vec(),
        sealed: sealed.to_vec(),
        scanned: 4,
    };
    assert_eq!(super::master_held_by(&scan(&[], &[], &[])), Some(false));
    assert_eq!(super::master_held_by(&scan(&[7], &[8], &[9])), Some(true));
    assert_eq!(super::master_held_by(&scan(&[], &[8], &[])), None);
    assert_eq!(super::master_held_by(&scan(&[], &[], &[9])), None);
}

/// A terminal leaves its session once its master closes, so Linux reads every devpts
/// terminal a live process still has held (`pty_master_held`): a session leader that ignores
/// `SIGHUP` takes the pty's terminal end as its controlling terminal by opening it, its
/// `tty_nr` names that terminal, and once the master closes the leader still runs with
/// none. It runs alone (`runs_alone`). NEGATIVE CONTROL: while the master is open the
/// leader's terminal reads as its own, so a lineage that never read a terminal fails it.
#[cfg(target_os = "linux")]
#[test]
fn a_terminal_leaves_its_session_once_its_master_closes() {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::MetadataExt as _;
    /// The leader, ended and reaped however the test ends.
    struct Ended(crate::spawnfd::Child);
    impl Drop for Ended {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    if !runs_alone("holders::listing_tests::a_terminal_leaves_its_session_once_its_master_closes") {
        return;
    }
    let (master, terminal) = a_terminal();
    let number = terminal.metadata().expect("fstat the terminal").rdev() & 0xffff_ffff;
    let name = std::fs::read_link(format!("/proc/self/fd/{}", terminal.as_raw_fd()))
        .expect("the terminal end's name");
    let mut leader = crate::spawnfd::command("/bin/sh");
    leader
        .args([
            "-c",
            "trap '' HUP; exec 3<>\"$0\"; echo ran; exec /bin/sleep 60",
        ])
        .arg(&name)
        .setsid(true)
        .stdin(crate::spawnfd::Stdio::null())
        .stdout(crate::spawnfd::Stdio::piped());
    let mut leader = Ended(leader.spawn().expect("spawn the session leader"));
    crate::deadline::prints(&mut leader.0, "ran", crate::deadline::ECHO_HANG);
    let pid = i32::try_from(leader.0.id()).expect("a pid");
    let tty = crate::process::read_lineage(pid)
        .expect("the leader's lineage")
        .tty;
    assert_eq!(tty.map(u64::from), Some(number), "{name:?} is its terminal");
    assert_eq!(
        super::pty_master_held(number),
        Some(true),
        "its master is open"
    );
    drop(master);
    let deadline = std::time::Instant::now() + crate::deadline::ECHO_HANG;
    loop {
        let lineage = crate::process::read_lineage(pid).expect("the leader still runs");
        if lineage.tty.is_none() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the leader kept {name:?} a minute after its master closed"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Set in the children [`a_child_s_standard_input_is_a_terminal_only_on_a_pty`] spawns
/// of this test binary.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
const STDIN_CHILD_ENV: &str = "ATERM_UDS_TEST_STDIN_CHILD";

/// A child's standard input is a terminal when it is a pty's terminal end, and none when it
/// is `/dev/null` or a pipe; a descriptor the child does not hold is none. Each child shows
/// it runs by a line before it is read. The children are this test binary staged by
/// [`STDIN_CHILD_ENV`], never a shell: a shell started with a terminal for its input names
/// that terminal (`ttyname`) while it starts, which scans `/dev` and hangs on one wedged
/// node — the read under test is the parent's, and no child behavior past holding its
/// descriptors is.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[test]
fn a_child_s_standard_input_is_a_terminal_only_on_a_pty() {
    use crate::spawnfd::{Child, Stdio, command};
    const NAME: &str =
        "holders::listing_tests::a_child_s_standard_input_is_a_terminal_only_on_a_pty";
    if std::env::var_os(STDIN_CHILD_ENV).is_some() {
        println!("ran");
        std::thread::sleep(std::time::Duration::from_secs(60));
        return;
    }
    /// The children, each ended and reaped however the test ends.
    struct Ended(Vec<Child>);
    impl Drop for Ended {
        fn drop(&mut self) {
            for child in &mut self.0 {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let exe = std::env::current_exe().expect("the test binary");
    let (_master, terminal) = a_terminal();
    let mut children = Ended(Vec::new());
    for stdin in [Stdio::from(terminal), Stdio::null(), Stdio::piped()] {
        // `--quiet` keeps the child's `ran` on its own line for [`crate::deadline::prints`]:
        // the harness's progress fragment has no newline for it to land on otherwise.
        let child = command(&exe)
            .args([
                "--exact",
                NAME,
                "--nocapture",
                "--quiet",
                "--test-threads=1",
            ])
            .env(STDIN_CHILD_ENV, "1")
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the child");
        children.0.push(child);
        let child = children.0.last_mut().expect("the child just kept");
        crate::deadline::prints(child, "ran", crate::deadline::ECHO_HANG);
    }
    let reads: Vec<Option<bool>> = children
        .0
        .iter()
        .map(|child| super::fd_is_terminal(child.id(), 0))
        .collect();
    let unheld = super::fd_is_terminal(children.0[0].id(), 37);
    drop(children);
    assert_eq!(
        reads,
        [Some(true), Some(false), Some(false)],
        "a pty, /dev/null, a pipe"
    );
    assert_eq!(unheld, Some(false), "a descriptor the child does not hold");
}

/// A descriptor's vnode read answers whether it is a terminal only from a whole record or a
/// closed descriptor: a revoked vnode, a process gone or refused, another failure and a short
/// record prove nothing.
#[cfg(target_vendor = "apple")]
#[test]
fn a_vnode_read_answers_for_a_terminal_only_when_it_reads() {
    use super::Failed;
    use aterm_sysprobe::jobs::darwin;
    let record = |path: &[u8]| {
        let mut b = vec![0u8; darwin::VNODE_PATH_SIZE];
        let mode = (darwin::S_IFCHR | 0o620).to_ne_bytes();
        b[darwin::VNODE_MODE_AT..darwin::VNODE_MODE_AT + mode.len()].copy_from_slice(&mode);
        b[darwin::VNODE_PATH_AT..darwin::VNODE_PATH_AT + path.len()].copy_from_slice(path);
        b
    };
    let (pty, null) = (record(b"/dev/ttys003"), record(b"/dev/null"));
    assert_eq!(super::vnode_terminal(Ok(&pty)), Some(true), "a pty");
    assert_eq!(super::vnode_terminal(Ok(&null)), Some(false), "/dev/null");
    assert_eq!(
        super::vnode_terminal(Ok(&pty[..darwin::VNODE_PATH_SIZE - 1])),
        None,
        "a short record"
    );
    assert_eq!(
        super::vnode_terminal(Err(Failed::Closed)),
        Some(false),
        "a closed descriptor, a pipe or a socket"
    );
    for failed in [Failed::Dead, Failed::Exited, Failed::Refused, Failed::Other] {
        assert_eq!(super::vnode_terminal(Err(failed)), None, "{failed:?}");
    }
}

/// A Linux `fd/<n>` link names a terminal by the job probe's rule; a terminal the kernel marks
/// ` (deleted)` (a pty whose master closed) proves nothing, as a revoked vnode on macOS; a
/// file is none, deleted or not.
#[cfg(unix)]
#[test]
fn a_descriptor_link_is_a_terminal_by_its_name() {
    for (link, terminal) in [
        ("/dev/pts/3", Some(true)),
        ("/dev/pts/3 (deleted)", None),
        ("/dev/null", Some(false)),
        ("pipe:[4711]", Some(false)),
        ("/tmp/in (deleted)", Some(false)),
    ] {
        assert_eq!(super::link_terminal(Some(link)), terminal, "{link}");
    }
    assert_eq!(
        super::link_terminal(None),
        Some(false),
        "a link that is no UTF-8 names no terminal"
    );
}

/// A descriptor `/proc` no longer lists is closed only while its process, read after it, is
/// not exiting: an exit closes every descriptor before the process is a zombie.
#[cfg(unix)]
#[test]
fn a_missing_descriptor_is_closed_only_while_its_process_is_not_exiting() {
    let stat = |flags: u32| {
        format!("4711 (aterm) S 1 4711 4711 0 -1 {flags} 0 0 0 0 1 2 0 0 20 0 1 0 900 0 0")
    };
    assert_eq!(
        super::closed_answer(Some(&stat(0x0040_0000))),
        Some(false),
        "a live process closed it"
    );
    assert_eq!(
        super::closed_answer(Some(&stat(0x0040_0004))),
        None,
        "an exiting process closes them all"
    );
    assert_eq!(super::closed_answer(Some("4711 (cut")), None, "a torn line");
    assert_eq!(super::closed_answer(None), None, "the process is gone");
}

/// A uid with no live process lists none: an answer, never an error.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[test]
fn a_uid_with_no_process_lists_none() {
    let unused = 2_000_000_011;
    assert_eq!(
        super::pids_of_uid(unused).map_err(|e| e.to_string()),
        Ok(Vec::new())
    );
}

/// A listener is found by the path it is bound at, read from the socket's own record, and by
/// a spelling whose folder resolves to that one; a listener bound through a link is found by
/// that spelling alone. A listener bound elsewhere, or closed in every process holding it, is
/// not found.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[test]
fn this_process_holds_the_listener_it_bound() {
    use std::os::unix::net::{UnixListener, UnixStream};
    let me = std::process::id();
    let dir = aterm_tempfile::Builder::new()
        .tempdir_in("/tmp")
        .expect("a scratch folder short enough for sun_path");
    let base = dir
        .path()
        .canonicalize()
        .expect("the scratch folder's own spelling");
    let (real, link) = (base.join("real"), base.join("link"));
    std::fs::create_dir(&real).expect("the real folder");
    std::os::unix::fs::symlink(&real, &link).expect("a link to it");
    let (a, b, c) = (
        real.join("a.sock"),
        real.join("b.sock"),
        link.join("c.sock"),
    );
    let l1 = UnixListener::bind(&a).expect("bind a");
    let l2 = UnixListener::bind(&b).expect("bind b");
    let l3 = UnixListener::bind(&c).expect("bind c through the link");
    let client = UnixStream::connect(&a).expect("connect a");
    let (accepted, _) = crate::accept(
        &l1,
        crate::AcceptWait::Forever,
        crate::AcceptedMode::Blocking,
    )
    .expect("accept");
    let scan = |path: &std::path::Path| {
        super::scan_listener_holders(path, &[]).expect("the listener scan")
    };
    let found = scan(&a);
    assert!(found.holds(me), "{found:?}");
    assert!(found.scanned > 0);
    assert!(
        scan(&link.join("a.sock")).holds(me),
        "the same socket through a link to its folder"
    );
    assert!(scan(&b).holds(me), "the other listener");
    assert!(
        scan(&c).holds(me),
        "a listener bound through a link, by that spelling"
    );
    assert!(
        !scan(&real.join("c.sock")).holds(me),
        "a listener bound through a link is not resolved to its folder's own spelling"
    );
    assert!(
        !super::scan_listener_holders(&a, &[me])
            .expect("the listener scan")
            .holds(me),
        "skip is honoured"
    );
    drop((client, accepted, l1));
    std::fs::remove_file(&a).expect("unlink a");
    // A child another test is spawning holds a copy of the listener until its exec closes
    // it, and the socket, its binder named, lives until every copy is closed: the wait is a
    // hang detector.
    let started = std::time::Instant::now();
    let gone = loop {
        let gone = scan(&a);
        if !gone.holds(me) || started.elapsed() >= std::time::Duration::from_secs(60) {
            break gone;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(
        !gone.holds(me),
        "a closed listener is held by nobody, with another still listening here: {gone:?}"
    );
    assert!(scan(&b).holds(me), "the other listener still");
    drop((l2, l3));
}

/// A private sibling is `<path>.<pid>.<n>` exactly, `path` as `names` takes it and each
/// number one or more digits. The pid is not read, so a leading zero or a number past `u32`
/// still has the shape; a path's own words take no sibling.
#[test]
fn a_private_sibling_is_its_paths_name_and_two_numbers() {
    let target = super::SocketPath::of(std::path::Path::new("/nowhere-folder/bus.sock"));
    let shown = |name: &[u8]| String::from_utf8_lossy(name).into_owned();
    for sibling in [
        &b"/nowhere-folder/bus.sock.812.0"[..],
        b"/nowhere-folder/bus.sock.812.37",
        b"/nowhere-folder/bus.sock.0812.0",
        b"/nowhere-folder/bus.sock.0.0",
        b"/nowhere-folder/bus.sock.99999999999.0",
    ] {
        assert!(target.is_sibling(sibling), "{}", shown(sibling));
    }
    for other in [
        &b"/nowhere-folder/bus.sock"[..],
        b"/nowhere-folder/bus.sock.812",
        b"/nowhere-folder/bus.sock.812.",
        b"/nowhere-folder/bus.sock..0",
        b"/nowhere-folder/bus.sock.x.0",
        b"/nowhere-folder/bus.sock.812.0.1",
        b"/nowhere-folder/bus.sockx.812.0",
        b"/nowhere-folder/bus.sock.wake",
        b"/elsewhere/bus.sock.812.0",
    ] {
        assert!(!target.is_sibling(other), "{}", shown(other));
    }
    assert!(!target.names(b"/nowhere-folder/bus.sock.812.0"));
}

/// A listener this process binds at `<path>.<pid>.<n>`, links to `path` and whose private name
/// it removes, as an astream broker binds (`Broker::bind_unix`), holds `path`, whatever pid
/// the name holds. A sibling never linked into place holds no path, left at its name or that
/// name removed: with nothing at the path, beside a datagram socket there, or beside a socket
/// file whose listener is gone, which macOS's connection is refused at as a full accept
/// queue's would be and its probe finds nothing bound to. NEGATIVE CONTROLS, on macOS's rule:
/// match by `names` alone, and the broker's path has no holder; take any sibling whose name
/// is gone, and one with nothing at its path holds it; take one whose name is gone with a
/// socket at the path, and one beside a datagram socket holds it; read every refusal as
/// unknown whatever the probe finds, and the scan beside the stale file fails.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[test]
fn a_listener_linked_into_place_from_its_private_name_holds_its_path() {
    use std::os::unix::net::{UnixDatagram, UnixListener};
    let me = std::process::id();
    let dir = aterm_tempfile::Builder::new()
        .prefix("held-")
        .tempdir_in("/tmp")
        .expect("a scratch folder short enough for sun_path");
    let at = |name: &str| dir.path().join(name);
    let scan =
        |name: &str| super::scan_listener_holders(&at(name), &[]).expect("the listener scan");
    let private = |path: &str| at(&format!("{path}.{me}.0"));
    let sibling = |path: &str| UnixListener::bind(private(path)).expect("bind the private name");
    let unnamed =
        |path: &str| std::fs::remove_file(private(path)).expect("remove the private name");
    let linked = |path: &str, name: &str| {
        let listener = UnixListener::bind(at(name)).expect("bind the private name");
        std::fs::hard_link(at(name), at(path)).expect("link into place");
        std::fs::remove_file(at(name)).expect("remove the private name");
        listener
    };
    let held = |path: &str, what: &str| {
        let found = scan(path);
        assert!(found.holds(me), "{what}: {found:?}");
    };
    let free = |path: &str, what: &str| {
        let found = scan(path);
        assert!(!found.holds(me), "{what}: {found:?}");
    };

    let broker = linked("b.sock", &format!("b.sock.{me}.0"));
    held("b.sock", "a broker's listener holds its path");
    let foreign = linked("e.sock", "e.sock.1.0");
    held(
        "e.sock",
        "a sibling that names another pid, linked into place",
    );

    let left = sibling("c.sock");
    free("c.sock", "a sibling left at its name, nothing at the path");
    let gone = sibling("x.sock");
    unnamed("x.sock");
    free("x.sock", "a sibling's name removed, nothing at the path");
    let datagram = UnixDatagram::bind(at("d.sock")).expect("a datagram socket at d");
    let beside = sibling("d.sock");
    free("d.sock", "a sibling left beside a datagram socket");
    unnamed("d.sock");
    free(
        "d.sock",
        "a sibling's name removed beside a datagram socket",
    );
    stale_socket(&at("f.sock"));
    let stale = sibling("f.sock");
    unnamed("f.sock");
    free(
        "f.sock",
        "a sibling's name removed beside a socket file whose listener is gone",
    );
    drop((broker, foreign, left, gone, datagram, beside, stale));
}

/// A listener the socket dump names holds a path when bound there, or bound at its private
/// sibling, whatever pid that names, with its file's inode the one at the path by the low 32
/// bits the record keeps. A sibling whose file is not the one at the path, with nothing
/// there, or with no file in its record holds nothing, nor does another name of that file.
/// NEGATIVE CONTROL: match by `names` alone, and the sibling linked into place holds nothing.
#[test]
fn a_dump_record_holds_its_path_by_its_name_or_its_siblings_inode() {
    use aterm_sysprobe::linux::{BoundFile, UnixDiagListener};
    let target = super::SocketPath::of(std::path::Path::new("/nowhere-folder/bus.sock"));
    let record = |path: &[u8], file| UnixDiagListener {
        inode: 41001,
        path: path.to_vec(),
        file,
    };
    let file = Some(BoundFile { ino: 0x1234 });
    let sibling = record(b"/nowhere-folder/bus.sock.812.0", file);
    let holds =
        |listener: &UnixDiagListener, placed| super::diag_listener_holds(&target, placed, listener);
    assert!(
        holds(&record(b"/nowhere-folder/bus.sock", None), None),
        "bound at the path"
    );
    assert!(holds(&sibling, Some(0x1234)), "a sibling linked into place");
    assert!(
        holds(
            &record(b"/nowhere-folder/bus.sock.1.0", file),
            Some(0x7_0000_1234)
        ),
        "by the inode's low 32 bits, whatever pid the name holds"
    );
    for (listener, placed, what) in [
        (&sibling, Some(0x1235), "another file at the path"),
        (&sibling, None, "nothing at the path"),
        (
            &record(b"/nowhere-folder/bus.sock.812.0", None),
            Some(0x1234),
            "a record with no file",
        ),
        (
            &record(b"/nowhere-folder/other.sock", file),
            Some(0x1234),
            "another name of that file",
        ),
    ] {
        assert!(!holds(listener, placed), "{what}");
    }
}

#[cfg(unix)]
#[test]
fn unreadable_target_metadata_keeps_a_private_listener_unknown() {
    use aterm_sysprobe::linux::{BoundFile, UnixDiagListener};
    let target = super::SocketPath::of(std::path::Path::new("/nowhere-folder/bus.sock"));
    let listener = UnixDiagListener {
        inode: 41001,
        path: b"/nowhere-folder/bus.sock.812.0".to_vec(),
        file: Some(BoundFile { ino: 0x1234 }),
    };
    let holds = |metadata| -> std::io::Result<bool> {
        let placed = super::listener_target_inode(metadata)?;
        Ok(super::diag_listener_holds(&target, placed, &listener))
    };
    assert!(
        holds(Ok((0x1234, false))).unwrap(),
        "the linked listener is held"
    );
    for kind in [
        std::io::ErrorKind::NotFound,
        std::io::ErrorKind::NotADirectory,
    ] {
        assert!(!holds(Err(std::io::Error::from(kind))).unwrap());
    }
    for errno in [libc::EIO, libc::EACCES, libc::ELOOP] {
        let error = holds(Err(std::io::Error::from_raw_os_error(errno)))
            .expect_err("unreadable does not establish that no listener holds the target");
        assert_eq!(error.raw_os_error(), Some(errno));
    }
    // The former `.ok().map(...)` loses EIO and incorrectly reports a known absence.
    let erased = Err::<u64, _>(std::io::Error::from_raw_os_error(libc::EIO)).ok();
    assert!(!super::diag_listener_holds(&target, erased, &listener));
}

#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[test]
fn a_socket_leaf_alias_is_unknown_and_cannot_report_an_absent_private_listener() {
    use std::os::unix::net::UnixListener;
    let dir = aterm_tempfile::Builder::new().tempdir_in("/tmp").unwrap();
    let real = dir.path().join("b.sock");
    let private = dir.path().join(format!("b.sock.{}.0", std::process::id()));
    let listener = UnixListener::bind(&private).unwrap();
    std::fs::hard_link(&private, &real).unwrap();
    std::fs::remove_file(&private).unwrap();
    assert!(
        super::scan_listener_holders(&real, &[])
            .unwrap()
            .holds(std::process::id())
    );
    let alias = dir.path().join("alias.sock");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let target = super::SocketPath::of(&alias);
    assert!(
        !target.is_sibling(private.as_os_str().as_encoded_bytes()),
        "the folder-only matcher cannot prove this differently named leaf alias"
    );
    let error = super::scan_listener_holders(&alias, &[])
        .expect_err("an aliased live listener is unknown, never a successful empty scan");
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    drop(listener);
}

/// A datagram probe finds nothing bound where there is no file, a folder on the path is
/// missing or no folder, the file is no socket, or the socket file's listener closed; a
/// stream socket where a listener is, its accept queue full or not, and through a link to
/// it; a datagram socket where one is. The probe queues nothing at the listener it finds,
/// and a path too long for `sun_path` is no answer. NEGATIVE CONTROL: read `EPROTOTYPE` as
/// nothing bound, as `ECONNREFUSED` is, and the live listener reads as nothing bound.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[test]
fn a_probe_tells_a_live_listener_from_a_stale_socket_file() {
    use super::Bound;
    use std::os::fd::AsRawFd as _;
    use std::os::unix::net::{UnixDatagram, UnixListener};
    let dir = aterm_tempfile::Builder::new()
        .tempdir_in("/tmp")
        .expect("a scratch folder short enough for sun_path");
    let at = |name: &str| dir.path().join(name);
    let bound = |name: &str| super::bound_at(&at(name)).expect("the probe answers");
    std::fs::write(at("plain"), b"").expect("a plain file");
    stale_socket(&at("stale.sock"));
    for (name, what) in [
        ("none.sock", "no file"),
        ("missing/x.sock", "a missing folder"),
        ("plain/x.sock", "a file where a folder is"),
        ("plain", "a file that is no socket"),
        ("stale.sock", "a socket file whose listener closed"),
    ] {
        assert_eq!(bound(name), Bound::Nothing, "{what}");
    }
    let live = UnixListener::bind(at("live.sock")).expect("a listener");
    assert_eq!(bound("live.sock"), Bound::Stream, "a live listener");
    let queued = crate::accept(
        &live,
        crate::AcceptWait::Ready,
        crate::AcceptedMode::Blocking,
    )
    .map(drop)
    .map_err(|e| e.kind());
    assert_eq!(
        queued,
        Err(std::io::ErrorKind::WouldBlock),
        "the probe queued nothing"
    );
    std::os::unix::fs::symlink(at("live.sock"), at("alias.sock")).expect("a link to it");
    assert_eq!(bound("alias.sock"), Bound::Stream, "through a link");
    let full = UnixListener::bind(at("full.sock")).expect("a listener");
    // SAFETY: the descriptor is `full`'s listening socket, open while `full` lives.
    let listened = unsafe { libc::listen(full.as_raw_fd(), 1) };
    assert_eq!(listened, 0, "{}", std::io::Error::last_os_error());
    let hang = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut held = Vec::new();
    while let Ok(stream) = crate::connect::until(&at("full.sock"), hang) {
        held.push(stream);
        assert!(held.len() < 64, "the accept queue never filled");
    }
    assert_eq!(bound("full.sock"), Bound::Stream, "a full accept queue");
    let datagram = UnixDatagram::bind(at("d.sock")).expect("a datagram socket");
    assert_eq!(bound("d.sock"), Bound::Datagram, "a datagram socket");
    let long = at(&"l".repeat(120));
    assert!(super::bound_at(&long).is_err(), "a path past sun_path");
    drop((live, full, held, datagram));
}

/// A socket bound at a name holding a newline and a whole `/proc/net/unix` row, which names
/// another path and this process's own listener's inode, names no listener at that other
/// path: the kernel's socket dump carries each name by its length. The row's path holds `/`s,
/// so the name runs through folders, which are made before the bind. NEGATIVE CONTROL: read the
/// listeners off `/proc/net/unix`'s text instead, and the forged row makes this process the
/// holder of a path nothing is bound at.
#[cfg(target_os = "linux")]
#[test]
fn a_socket_name_holding_a_forged_row_names_no_other_listener() {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::net::UnixListener;
    let me = std::process::id();
    // The forged name holds the folder's path twice, so both names stay short.
    let dir = aterm_tempfile::Builder::new()
        .prefix("f")
        .tempdir_in("/tmp")
        .expect("a scratch folder short enough for sun_path");
    let base = dir
        .path()
        .canonicalize()
        .expect("the scratch folder's own spelling");
    let (a, v) = (base.join("a"), base.join("v"));
    let real = UnixListener::bind(&a).expect("bind a");
    let link = std::fs::read_link(format!("/proc/self/fd/{}", real.as_raw_fd()))
        .expect("the listener's descriptor link");
    let aterm_sysprobe::jobs::linux::FdLink::Socket(inode) =
        aterm_sysprobe::jobs::linux::parse_fd_link(&link.to_string_lossy())
    else {
        panic!("the listener's link names no socket: {link:?}");
    };
    let forged = base.join(format!(
        "x\n0: 0 0 10000 1 1 {inode} {}",
        v.to_str().expect("a UTF-8 scratch path")
    ));
    assert!(
        forged.as_os_str().len() < 108,
        "the forged name fits sun_path: {forged:?}"
    );
    std::fs::create_dir_all(forged.parent().expect("the forged name's folder"))
        .expect("the folders the forged row's path runs through");
    let decoy = UnixListener::bind(&forged).expect("bind the forged name");
    let scan = |path: &std::path::Path| {
        super::scan_listener_holders(path, &[]).expect("the listener scan")
    };
    let at_v = scan(&v);
    assert!(!at_v.holds(me), "nothing is bound at {v:?}: {at_v:?}");
    assert!(scan(&a).holds(me), "the real listener");
    assert!(scan(&forged).holds(me), "the forged name is one name");
    drop((real, decoy));
}
