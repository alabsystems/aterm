// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(target_vendor = "apple")]
use super::running;
use super::{PathBuf, placed, scan_path_users};
use std::time::{Duration, Instant};

const HANG: Duration = Duration::from_secs(60);

struct MappedFile {
    address: *mut libc::c_void,
    len: usize,
}

impl MappedFile {
    fn new(path: &std::path::Path) -> Self {
        use std::os::fd::AsRawFd as _;
        let file = std::fs::File::open(path).expect("inert mapping input");
        let len =
            usize::try_from(file.metadata().expect("input metadata").len()).expect("mapping size");
        // SAFETY: the live file descriptor names at least `len` bytes, offset zero is
        // aligned, and the private read-only mapping outlives no access made here.
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                file.as_raw_fd(),
                0,
            )
        };
        assert_ne!(address, libc::MAP_FAILED, "map the inert input");
        drop(file);
        Self { address, len }
    }
}

impl Drop for MappedFile {
    fn drop(&mut self) {
        // SAFETY: this object owns the successful mapping, whose unchanged address and
        // length are passed once; nothing reads it after this destructor.
        let _ = unsafe { libc::munmap(self.address, self.len) };
    }
}

#[test]
fn a_mapped_file_with_its_descriptor_closed_keeps_the_directory_in_use() {
    let world = aterm_tempfile::tempdir().expect("private mapping fixture");
    let dir = world.path().canonicalize().expect("canonical fixture");
    let path = dir.join("inert mapped input with spaces");
    std::fs::write(&path, [0x5au8; 4096]).expect("inert mapping input");
    let mapping = MappedFile::new(&path);
    // SAFETY: the successful read-only mapping still covers the input's first byte.
    assert_eq!(
        unsafe { std::ptr::read(mapping.address.cast::<u8>()) },
        0x5a
    );
    let me = std::process::id();
    assert!(
        super::open_files(me)
            .expect("own descriptors")
            .iter()
            .all(|file| placed(file, std::slice::from_ref(&dir)).is_none()),
        "the original descriptor is closed"
    );
    let scan =
        super::scan_path_users_of(&[me], std::slice::from_ref(&dir), &[]).expect("own path uses");
    assert!(scan.unreadable.is_empty(), "{scan:?}");
    assert!(
        scan.users
            .iter()
            .any(|user| user.pid == me && user.how == "map"),
        "the mapping alone must retain the cache: {scan:?}"
    );
    let lock = dir.join("own cache lock");
    std::fs::write(&lock, b"inert lock custody").expect("own lock fixture");
    let custody = std::fs::File::open(&lock).expect("own lock custody");
    let scan = super::scan_path_users_of(&[me], std::slice::from_ref(&dir), &[])
        .expect("own path uses with lock custody");
    assert!(
        scan.users
            .iter()
            .any(|user| user.pid == me && user.how == "map"),
        "ignorable own lock descriptors must not hide a mapping: {scan:?}"
    );
    drop(custody);
    drop(mapping);
    let scan = super::scan_path_users_of(&[me], std::slice::from_ref(&dir), &[])
        .expect("own path uses after unmapping");
    assert!(
        scan.users.is_empty() && scan.unreadable.is_empty(),
        "{scan:?}"
    );
}

/// A new directory of this run's own: never one an earlier run with this pid kept, which may hold a copy that never showed it ran.
fn fresh(name: &str) -> PathBuf {
    let d = aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-uds-paths-{name}-"))
        .tempdir()
        .expect("mkdir")
        .keep();
    std::fs::canonicalize(&d).expect("canonical")
}

fn until(dirs: &[PathBuf], pid: u32, want: impl Fn(Option<&str>) -> bool) -> Option<String> {
    let start = Instant::now();
    loop {
        let scan = scan_path_users(dirs, &[std::process::id()]).expect("scan");
        let how = scan
            .users
            .iter()
            .find(|u| u.pid == pid)
            .map(|u| u.how.to_owned());
        if want(how.as_deref()) || start.elapsed() > HANG {
            return how;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_child_working_or_holding_a_file_under_the_directory_uses_it() {
    let dir = fresh("cwd");
    let deep = dir.join("debug/deps");
    std::fs::create_dir_all(&deep).expect("deps");
    let mut sleeper = crate::spawnfd::command("/bin/sleep")
        .arg("120")
        .current_dir(&deep)
        .spawn()
        .expect("sleep");
    let how = until(std::slice::from_ref(&dir), sleeper.id(), |h| h.is_some());
    let _ = sleeper.kill();
    let _ = sleeper.wait();
    assert_eq!(how.as_deref(), Some("cwd"));

    let file = dir.join("held.rlib");
    std::fs::write(&file, b"x").expect("rlib");
    let mut holder = crate::spawnfd::command("/bin/sh")
        .arg("-c")
        .arg("exec 3<\"$1\"; exec /bin/sleep 120")
        .arg("sh")
        .arg(&file)
        .current_dir("/")
        .spawn()
        .expect("holder");
    let how = until(std::slice::from_ref(&dir), holder.id(), |h| h == Some("fd"));
    let _ = holder.kill();
    let _ = holder.wait();
    assert_eq!(how.as_deref(), Some("fd"));

    let empty = fresh("nobody");
    let scan = scan_path_users(std::slice::from_ref(&empty), &[std::process::id()]).expect("scan");
    assert!(scan.users.is_empty(), "{:?}", scan.users);
    assert!(scan.scanned > 0);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

#[cfg(target_vendor = "apple")]
#[test]
fn a_process_whose_executable_was_deleted_is_read_through_its_working_directory() {
    let dir = fresh("unlinked");
    let (bin, work) = (dir.join("catter"), dir.join("work"));
    std::fs::create_dir_all(&work).expect("work");
    crate::deadline::ad_hoc_copy(std::path::Path::new("/bin/cat"), &bin);
    let mut catter = crate::spawnfd::command(&bin)
        .current_dir(&work)
        .stdin(crate::spawnfd::Stdio::piped())
        .stdout(crate::spawnfd::Stdio::piped())
        .spawn()
        .expect("the copied cat");
    let to = crate::deadline::cat_echoes(&mut catter);
    assert_eq!(
        until(std::slice::from_ref(&dir), catter.id(), |h| h.is_some()).as_deref(),
        Some("exe"),
        "running from the directory, before the delete"
    );
    std::fs::remove_file(&bin).expect("delete the executable");
    let how = until(std::slice::from_ref(&dir), catter.id(), |h| {
        h == Some("cwd")
    });
    let scan = scan_path_users(std::slice::from_ref(&dir), &[std::process::id()]).expect("scan");
    let _ = catter.kill();
    let _ = catter.wait();
    drop(to);
    assert_eq!(how.as_deref(), Some("cwd"));
    assert!(
        !scan.unreadable.iter().any(|(pid, _)| *pid == catter.id()),
        "the deleted executable made it unreadable: {:?}",
        scan.unreadable
    );
    let _ = std::fs::remove_dir_all(&dir);
}

const FORK_UNDER_A_TRANSLATOR: &str = "the kernel names the translator as the executable \
    of this process, of its un-exec'd fork and of every program the translator runs \
    (binfmt_misc), so a fork cannot be told from a child that exec'd another program; the \
    fork is kept as a user instead (asserted), and a real x86_64 CPU decides the skip";

#[test]
fn a_fork_of_this_process_that_has_not_execd_is_skipped_with_it() {
    unsafe extern "C" {
        fn fork() -> i32;
        fn pause() -> i32;
        fn _exit(code: i32) -> !;
        fn kill(pid: i32, sig: i32) -> i32;
        fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    }
    let dir = fresh("fork");
    let held = std::fs::File::open(&dir).expect("a handle on the directory");
    // SAFETY: the child only calls async-signal-safe `pause`/`_exit`.
    let child = unsafe { fork() };
    if child == 0 {
        // SAFETY: as above.
        unsafe {
            pause();
            _exit(0)
        }
    }
    assert!(child > 0, "fork");
    let me = std::process::id();
    let child_pid = u32::try_from(child).expect("pid");
    let skipped = scan_path_users(std::slice::from_ref(&dir), &[me]).expect("scan");
    let unskipped = scan_path_users(std::slice::from_ref(&dir), &[]).expect("scan");
    // SAFETY: our own child; SIGKILL, then reaped.
    unsafe {
        kill(child, 9);
        waitpid(child, std::ptr::null_mut(), 0);
    }
    drop(held);
    let kept = skipped.users.iter().any(|u| u.pid == child_pid);
    if let Some(translator) = crate::spawnfd::tests::translator() {
        assert!(
            kept,
            "under {translator} the fork is kept: {:?}",
            skipped.users
        );
        println!(
            "NOT RUN under {translator}: holders::paths::tests::\
             a_fork_of_this_process_that_has_not_execd_is_skipped_with_it: the skip: \
             {FORK_UNDER_A_TRANSLATOR}"
        );
    } else {
        assert!(!kept, "{:?}", skipped.users);
    }
    assert!(
        unskipped.users.iter().any(|u| u.pid == child_pid),
        "{:?}",
        unskipped.users
    );
    let _ = std::fs::remove_dir_all(&dir);
}

const PARK_ENV: &str = "ATERM_UDS_PATHS_PARK";

const PARKED: &str = "parked";

#[test]
fn a_child_that_execd_another_program_is_a_user_with_this_process_skipped() {
    const NAME: &str = "holders::paths::tests::\
        a_child_that_execd_another_program_is_a_user_with_this_process_skipped";
    if std::env::var_os(PARK_ENV).is_some() {
        // Keep the readiness proof on its own line even after libtest's single-thread prefix.
        println!("\n{PARKED}");
        let _ = std::io::Read::read(&mut std::io::stdin(), &mut [0u8; 1]);
        return;
    }
    let dir = fresh("execd");
    let bin_dir = fresh("execd-bin");
    let bin = bin_dir.join("other");
    // Off Apple a child writes the copy: a fork another test makes while this process holds
    // the copy open for writing (the paused fork beside this test) keeps it busy past the
    // spawn's retries, and Linux refuses its exec (`ETXTBSY`) until that fork goes.
    crate::deadline::copy_regular_fresh(&std::env::current_exe().expect("the test binary"), &bin)
        .expect("a copy of the test binary");
    let mut other = crate::spawnfd::command(&bin)
        .args([
            "--exact",
            NAME,
            "--nocapture",
            "--quiet",
            "--test-threads=1",
        ])
        .env(PARK_ENV, "1")
        .current_dir(&dir)
        .stdin(crate::spawnfd::Stdio::piped())
        .stdout(crate::spawnfd::Stdio::piped())
        .stderr(crate::spawnfd::Stdio::null())
        .spawn()
        .expect("the copy");
    crate::deadline::prints(&mut other, PARKED, crate::deadline::FIRST_EXEC_HANG);
    let how = until(std::slice::from_ref(&dir), other.id(), |h| h.is_some());
    let (mine, its) = (
        super::executable(std::process::id()),
        super::executable(other.id()),
    );
    drop(other.stdin.take());
    let _ = other.kill();
    let _ = other.wait();
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&bin_dir);
    if let Some(translator) = crate::spawnfd::tests::translator() {
        assert_eq!(
            mine, its,
            "PRECONDITION: under {translator} both run the translator"
        );
    } else {
        assert_ne!(
            mine, its,
            "PRECONDITION: natively the copy is another executable"
        );
    }
    assert_eq!(
        how.as_deref(),
        Some("cwd"),
        "this process's {mine:?}, its {its:?}"
    );
}

#[cfg(target_vendor = "apple")]
fn exec_done(child: &crate::spawnfd::Child) -> u32 {
    running(child, "/bin/sleep")
}

#[cfg(target_vendor = "apple")]
fn bare_child(cwd: &std::path::Path) -> (crate::spawnfd::Child, i32) {
    const WUNTRACED: i32 = 2;
    let child = crate::spawnfd::command("/bin/sh")
        .args(["-c", "exec 0<&- 1>&- 2>&-; kill -STOP $$"])
        .current_dir(cwd)
        .stdin(crate::spawnfd::Stdio::null())
        .stdout(crate::spawnfd::Stdio::null())
        .stderr(crate::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn the bare shell");
    let pid = i32::try_from(child.id()).expect("pid");
    let mut status = 0;
    // SAFETY: `waitpid(WUNTRACED)` waits for our child to stop without reaping it.
    let got = unsafe { proc_ffi::waitpid(pid, &raw mut status, WUNTRACED) };
    assert_eq!(got, pid, "waitpid");
    assert_eq!(status & 0xff, 0x7f, "stopped, not exited: {status:#x}");
    (child, pid)
}

#[cfg(target_vendor = "apple")]
fn reap(mut child: crate::spawnfd::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(target_vendor = "apple")]
mod proc_ffi {
    unsafe extern "C" {
        pub fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
        pub fn _exit(code: i32) -> !;
        pub fn grantpt(fd: i32) -> i32;
        pub fn unlockpt(fd: i32) -> i32;
        pub fn ioctl(fd: i32, request: core::ffi::c_ulong, ...) -> i32;
        pub fn revoke(path: *const core::ffi::c_char) -> i32;
    }
}

#[cfg(target_vendor = "apple")]
fn a_pty() -> (std::os::fd::OwnedFd, PathBuf) {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;
    const TIOCPTYGNAME: core::ffi::c_ulong = 0x4080_7453;
    let (master, _) = super::super::tests::a_master_at(230);
    let m = master.as_raw_fd();
    // SAFETY: `m` is this test's open master.
    assert_eq!(unsafe { proc_ffi::grantpt(m) }, 0, "grantpt");
    // SAFETY: as above.
    assert_eq!(unsafe { proc_ffi::unlockpt(m) }, 0, "unlockpt");
    let mut name = [0u8; 128];
    // SAFETY: TIOCPTYGNAME writes at most 128 bytes, NUL-terminated.
    let rc = unsafe { proc_ffi::ioctl(m, TIOCPTYGNAME, name.as_mut_ptr()) };
    assert_eq!(rc, 0, "TIOCPTYGNAME");
    let len = name.iter().position(|b| *b == 0).expect("NUL-terminated");
    let slave = std::ffi::OsStr::from_bytes(&name[..len]);
    (master, PathBuf::from(slave))
}

/// A PTY's master is named from its slave's number ([`super::super::pty_master_rdev`],
/// what a process's `e_tdev` gives), and a scan of that number finds this process, which
/// holds the master. NEGATIVE CONTROL: the slave's own number is another device, so a
/// mapping that handed it back unchanged (a scan of the slave) fails the first assertion.
#[cfg(target_vendor = "apple")]
#[test]
fn a_ptys_master_is_named_from_its_slave() {
    use std::os::unix::fs::MetadataExt as _;
    const DEV: u64 = 0xffff_ffff;
    let me = std::process::id();
    let (master, slave) = a_pty();
    let slave_rdev = std::fs::metadata(&slave).expect("stat the slave").rdev() & DEV;
    let master_rdev = std::fs::File::from(master.try_clone().expect("dup"))
        .metadata()
        .expect("fstat the master")
        .rdev()
        & DEV;
    let named = super::super::pty_master_rdev(slave_rdev).expect("/dev/ptmx reads");
    let held = super::super::scan_rdev_holders(named, &[]).expect("scan");
    drop(master);
    assert_eq!(named, master_rdev, "the master's number, from the slave's");
    assert_ne!(slave_rdev, master_rdev, "the slave is another device");
    assert!(held.holds(me), "this process holds the master: {held:?}");
}

#[cfg(target_vendor = "apple")]
#[test]
fn a_descriptor_on_a_revoked_terminal_holds_nothing() {
    use crate::spawnfd::Stdio;
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::MetadataExt as _;
    let me = std::process::id();
    let (master, slave) = a_pty();
    let rdev = std::fs::metadata(&slave).expect("stat the slave").rdev();
    let dirs = std::slice::from_ref(&slave);
    let mut child = crate::spawnfd::command("/bin/sh")
        .arg("-c")
        .arg("exec 3<>\"$1\"; exec /bin/sleep 600")
        .arg("sh")
        .arg(&slave)
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("a holder of the slave");
    let pid = exec_done(&child);
    let n = pid as i32;
    let before = (
        super::scan_path_users_of(&[pid], dirs, &[me]).expect("scan"),
        super::super::process_holds_rdev(pid, rdev),
    );
    let path = std::ffi::CString::new(slave.as_os_str().as_bytes()).expect("a path");
    // SAFETY: a NUL-terminated path, to the slave of this test's own PTY.
    let revoked = unsafe { proc_ffi::revoke(path.as_ptr()) };
    let revoke_err = std::io::Error::last_os_error();
    let fds = super::super::list_fds(n);
    let read = super::super::vnode_of(n, 3, super::super::Lookup::BY_RDEV);
    let after = (
        super::scan_path_users_of(&[pid], dirs, &[me]).expect("scan"),
        super::super::process_holds_rdev(pid, rdev),
    );
    let old = super::super::hook::reading_dead_as_unknown(|| {
        (
            super::scan_path_users_of(&[pid], dirs, &[me]).expect("scan"),
            super::super::process_holds_rdev(pid, rdev),
        )
    });
    let _ = child.kill();
    let _ = child.wait();
    drop(master);

    assert_eq!(
        before.0.users,
        [super::PathUse {
            pid,
            how: "fd",
            path: slave.clone()
        }],
        "before the revoke, a user through the slave"
    );
    assert_eq!(before.1, Some(true), "before the revoke, holding it");
    assert_eq!(revoked, 0, "revoke: {revoke_err}");
    let fds = fds.expect("its table, read");
    assert!(
        fds.contains(&(3, super::super::ffi::PROX_FDTYPE_VNODE)),
        "the descriptor is still open: {fds:?}"
    );
    assert_eq!(
        read,
        Err(super::super::Failed::Dead),
        "its read answers ENOENT"
    );
    assert_eq!(
        (after.0.scanned, after.0.users, after.0.unreadable),
        (1, Vec::new(), Vec::new()),
        "read, and no user"
    );
    assert_eq!(after.1, Some(false), "it holds no device: known");
    assert_eq!(
        old.0.unreadable.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
        [pid],
        "the old reading: unreadable"
    );
    assert_eq!(old.1, None, "the old reading: unknown");
}

#[cfg(target_vendor = "apple")]
#[test]
fn a_dead_working_directory_is_no_working_directory() {
    use super::super::hook::{reading_cwd_dead, reading_dead_as_unknown};
    use crate::spawnfd::Stdio;
    let me = std::process::id();
    let base = fresh("deadcwd");
    let (mnt, held) = (base.join("mnt"), base.join("held"));
    std::fs::create_dir_all(&mnt).expect("the folder standing in for the volume");
    std::fs::create_dir_all(&held).expect("the held file's directory");
    let file = held.join("open.rlib");
    std::fs::write(&file, b"x").expect("a file to hold");
    let mut child = crate::spawnfd::command("/bin/sh")
        .arg("-c")
        .arg("exec 3<\"$1\"; exec /bin/cat")
        .arg("sh")
        .arg(&file)
        .current_dir(&mnt)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("a child working in the folder");
    let pid = running(&child, "/bin/cat");
    let scan = |dir: &PathBuf| {
        super::scan_path_users_of(&[pid], std::slice::from_ref(dir), &[me]).expect("scan")
    };
    let n = pid as i32;
    let before = scan(&mnt);
    let read = reading_cwd_dead(n, || super::mac::cwd(n));
    let after = reading_cwd_dead(n, || scan(&mnt));
    let through_fd = reading_cwd_dead(n, || scan(&held));
    let old = reading_dead_as_unknown(|| reading_cwd_dead(n, || scan(&mnt)));
    let _ = child.kill();
    let _ = child.wait();

    assert_eq!(
        before.users,
        [super::PathUse {
            pid,
            how: "cwd",
            path: mnt.clone()
        }],
        "a user through its working directory while it reads"
    );
    assert_eq!(
        read,
        Err(super::super::Failed::Dead),
        "its working directory reads ENOENT"
    );
    assert_eq!(
        (after.scanned, after.users, after.unreadable),
        (1, Vec::new(), Vec::new()),
        "read, and no user"
    );
    assert_eq!(
        through_fd.users,
        [super::PathUse {
            pid,
            how: "fd",
            path: file.clone()
        }],
        "read on, through its descriptors"
    );
    assert_eq!(
        old.unreadable.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
        [pid],
        "the old reading: unreadable"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[cfg(target_vendor = "apple")]
#[test]
fn a_process_with_no_descriptor_is_read_and_holds_nothing() {
    let dir = fresh("bare");
    let dirs = std::slice::from_ref(&dir);
    let null = {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata("/dev/null").expect("stat").rdev()
    };
    let (away_child, away) = bare_child(std::path::Path::new("/"));
    let fds = super::super::list_fds(away);
    let scan = super::scan_path_users_of(&[away as u32], dirs, &[]).expect("scan");
    let holds = super::super::process_holds_rdev(away as u32, null);
    let (inside_child, inside) = bare_child(&dir);
    let from_inside = super::scan_path_users_of(&[inside as u32], dirs, &[]).expect("scan");
    reap(away_child);
    reap(inside_child);
    assert_eq!(fds, Ok(Vec::new()), "an empty table, read");
    assert_eq!(
        (scan.scanned, scan.users, scan.unreadable),
        (1, Vec::new(), Vec::new()),
        "read, and no user"
    );
    assert_eq!(holds, Some(false), "it holds no device: known, not unknown");
    assert_eq!(
        from_inside.users.iter().map(|u| u.how).collect::<Vec<_>>(),
        ["cwd"]
    );
    let refused = super::scan_path_users_of(&[1], dirs, &[]).expect("scan");
    assert_eq!(
        (refused.scanned, refused.users, refused.unreadable),
        (1, Vec::new(), Vec::new()),
        "launchd's refused table: root's process, outside this user's scan"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(target_vendor = "apple")]
#[test]
fn a_process_that_exited_after_the_listing_uses_nothing() {
    let me = std::process::id();
    let dir = fresh("exited");
    let dirs = std::slice::from_ref(&dir);
    let read = |pid: u32| super::scan_path_users_of(&[pid], dirs, &[me]).expect("scan");
    let gone = |scan: &super::PathScan| scan.users.is_empty() && scan.unreadable.is_empty();

    let mut worker = crate::spawnfd::command("/bin/sleep")
        .arg("120")
        .current_dir(&dir)
        .spawn()
        .expect("a worker");
    let pid = exec_done(&worker);
    let alive = read(pid);
    super::super::tests::kill_unreaped(pid);
    assert!(
        super::super::uid_pids().expect("listing").contains(&pid),
        "the zombie is still listed: a scan meets it"
    );
    let zombie = read(pid);
    let _ = worker.wait();
    let reaped = read(pid);
    assert_eq!(
        alive.users.iter().map(|u| u.how).collect::<Vec<_>>(),
        ["cwd"]
    );
    assert!(gone(&zombie), "a zombie: {zombie:?}");
    assert!(gone(&reaped), "reaped: {reaped:?}");

    let file = dir.join("held.rlib");
    std::fs::write(&file, b"x").expect("rlib");
    let mut holder = crate::spawnfd::command("/bin/sh")
        .arg("-c")
        .arg("exec 3<\"$1\"; exec /bin/sleep 120")
        .arg("sh")
        .arg(&file)
        .current_dir("/")
        .spawn()
        .expect("a holder");
    let pid = exec_done(&holder);
    let holding = read(pid);
    let listed = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let seen = std::rc::Rc::clone(&listed);
    super::super::hook::after_next_listing(move |at, fds| {
        assert_eq!(at, pid as i32);
        seen.set(fds.len());
        super::super::tests::kill_unreaped(pid);
    });
    let mid_read = read(pid);
    let _ = holder.wait();
    assert_eq!(
        holding.users.iter().map(|u| u.how).collect::<Vec<_>>(),
        ["fd"]
    );
    assert!(
        listed.get() > 0,
        "its descriptors were listed, then read after its exit"
    );
    assert!(gone(&mid_read), "killed mid-read: {mid_read:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_data_volume_path_matches_its_firmlinked_form() {
    let dirs = [PathBuf::from("/Users//x/proj")];
    let at = |p: &str| placed(std::path::Path::new(p), &dirs);
    assert_eq!(
        at("/System/Volumes/Data/Users//x/proj/target"),
        Some(PathBuf::from("/Users//x/proj/target"))
    );
    assert_eq!(at("/Users//x/proj"), Some(PathBuf::from("/Users//x/proj")));
    assert_eq!(at("/Users//x/project"), None);
    assert_eq!(at("/System/Volumes/Data/Users//x/project"), None);
}
