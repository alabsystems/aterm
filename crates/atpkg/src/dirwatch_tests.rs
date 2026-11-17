// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

const NOW: Option<Duration> = Some(Duration::ZERO);

#[test]
fn a_directory_entry_names_its_watch_alone() {
    let root = crate::testdir::dir("atpkg-dirwatch-dir-");
    let (a, b) = (root.join("a"), root.join("b"));
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    let watcher = Watcher::new().unwrap();
    let wa = watcher.add_dir(&a).unwrap();
    let wb = watcher.add_dir(&b).unwrap();
    assert_ne!(wa, wb);
    assert!(
        watcher.wait(NOW).unwrap().is_empty(),
        "CONTROL: nothing yet"
    );
    std::fs::write(root.join("staged"), b"x").unwrap();
    std::fs::rename(root.join("staged"), a.join("row")).unwrap();
    assert_eq!(watcher.wait(NOW).unwrap(), [wa]);
    std::fs::remove_file(a.join("row")).unwrap();
    assert_eq!(watcher.wait(NOW).unwrap(), [wa]);
    std::fs::remove_dir(&b).unwrap();
    assert_eq!(watcher.wait(NOW).unwrap(), [wb], "the directory itself");
}

#[test]
fn a_file_written_in_place_names_the_file_and_not_its_directory() {
    use std::io::Write as _;

    let root = crate::testdir::dir("atpkg-dirwatch-file-");
    let row = root.join("4242.json");
    std::fs::write(&row, b"").unwrap();
    let watcher = Watcher::new().unwrap();
    let dir = watcher.add_dir(&root).unwrap();
    let file = watcher.add_file(&row).unwrap();
    let mut out = std::fs::OpenOptions::new().append(true).open(&row).unwrap();
    out.write_all(b"{\"pid\":4242}").unwrap();
    drop(out);
    assert_eq!(watcher.wait(NOW).unwrap(), [file], "not {dir:?}");
    let mut out = std::fs::OpenOptions::new().append(true).open(&row).unwrap();
    out.write_all(b"\n").unwrap();
    drop(out);
    assert_eq!(watcher.wait(NOW).unwrap(), [file]);
}

#[test]
fn permission_changes_name_the_watched_file_and_directory() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = crate::testdir::dir("atpkg-dirwatch-permissions-");
    let parent = root.join("source");
    std::fs::create_dir(&parent).unwrap();
    let path = parent.join("input");
    std::fs::write(&path, b"inert input").unwrap();
    let mode = |path: &Path, value| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(value)).unwrap();
    };
    mode(&parent, 0o700);
    mode(&path, 0o600);
    let watcher = Watcher::new().unwrap();
    let directory = watcher.add_dir(&parent).unwrap();
    let file = watcher.add_file(&path).unwrap();
    for (path, value, expected) in [
        (&path, 0o400, file),
        (&path, 0o600, file),
        (&parent, 0o500, directory),
        (&parent, 0o700, directory),
    ] {
        mode(path, value);
        assert_eq!(
            watcher.wait(Some(aterm_spec::wait::MUST_HAPPEN)).unwrap(),
            [expected],
            "an input's permissions can change its availability without replacing it"
        );
    }
}

#[test]
fn a_read_only_sample_does_not_notify_attribute_watchers() {
    let root = crate::testdir::dir("atpkg-dirwatch-read-only-");
    let path = root.join("input");
    std::fs::write(&path, b"inert input").unwrap();
    std::fs::File::open(&path)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_accessed(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
        )
        .unwrap();
    let watcher = Watcher::new().unwrap();
    watcher.add_dir(&root).unwrap();
    watcher.add_file(&path).unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"inert input");
    assert!(
        watcher.wait(NOW).unwrap().is_empty(),
        "a local read must not keep waking its own input watch"
    );
}

#[test]
fn an_edge_after_registration_is_kept_for_a_later_wait() {
    let root = crate::testdir::dir("atpkg-dirwatch-kept-");
    std::fs::write(root.join("before"), b"").unwrap();
    let watcher = Watcher::new().unwrap();
    let id = watcher.add_dir(&root).unwrap();
    assert!(
        watcher.wait(NOW).unwrap().is_empty(),
        "before: not reported"
    );
    std::fs::write(root.join("after"), b"").unwrap();
    assert_eq!(
        watcher.wait(Some(aterm_spec::wait::MUST_HAPPEN)).unwrap(),
        [id]
    );
}

#[test]
fn a_removed_watch_names_nothing() {
    let root = crate::testdir::dir("atpkg-dirwatch-removed-");
    let (a, b) = (root.join("a"), root.join("b"));
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    let watcher = Watcher::new().unwrap();
    let wa = watcher.add_dir(&a).unwrap();
    let wb = watcher.add_dir(&b).unwrap();
    watcher.remove(wa);
    watcher.remove(wa);
    std::fs::write(a.join("x"), b"").unwrap();
    std::fs::write(b.join("x"), b"").unwrap();
    assert_eq!(watcher.wait(NOW).unwrap(), [wb]);
    let again = watcher.add_dir(&a).unwrap();
    assert!(again != wa && again != wb, "ids are never reused");
    std::fs::write(a.join("y"), b"").unwrap();
    assert_eq!(watcher.wait(NOW).unwrap(), [again], "CONTROL: kept");
}

#[test]
fn a_watch_whose_object_is_removed_is_gone() {
    let root = crate::testdir::dir("atpkg-dirwatch-gone-");
    let (dir, kept, row) = (root.join("dir"), root.join("kept"), root.join("row"));
    std::fs::create_dir(&dir).unwrap();
    std::fs::create_dir(&kept).unwrap();
    std::fs::write(&row, b"").unwrap();
    let watcher = Watcher::new().unwrap();
    let wd = watcher.add_dir(&dir).unwrap();
    let wk = watcher.add_dir(&kept).unwrap();
    let wf = watcher.add_file(&row).unwrap();
    assert!(!watcher.is_gone(wd) && !watcher.is_gone(wk) && !watcher.is_gone(wf));
    std::fs::remove_dir(&dir).unwrap();
    std::fs::write(kept.join("x"), b"").unwrap();
    std::fs::remove_file(&row).unwrap();
    assert_eq!(
        watcher.wait(Some(aterm_spec::wait::MUST_HAPPEN)).unwrap(),
        [wd, wk, wf]
    );
    assert!(watcher.is_gone(wd), "the removed directory");
    assert!(watcher.is_gone(wf), "the removed file");
    assert!(
        !watcher.is_gone(wk),
        "CONTROL: an entry changed, not the object"
    );
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("y"), b"").unwrap();
    assert!(
        watcher.wait(NOW).unwrap().is_empty(),
        "the new directory is not watched"
    );
    assert!(watcher.is_gone(wd));
    watcher.remove(wk);
    assert!(watcher.is_gone(wk), "a removed watch");
    assert!(watcher.is_gone(WatchId(u64::MAX)), "an id never handed out");
}

/// A FILE REPLACED BY A RENAME OVER ITS PATH NAMES ITS WATCH, GONE: what an atomic writer (a
/// tap reading, git's `HEAD.lock`) leaves is a new file at the path, and the old one's watch is
/// told once, so its owner places a watch on the new one. Both kinds of file watch hear it.
#[test]
fn a_file_renamed_over_names_its_watch_gone() {
    let root = crate::testdir::dir("atpkg-dirwatch-renamed-over-");
    let (whole, contents) = (root.join("whole"), root.join("contents"));
    std::fs::write(&whole, b"old").unwrap();
    std::fs::write(&contents, b"old").unwrap();
    let watcher = Watcher::new().unwrap();
    let ww = watcher.add_file(&whole).unwrap();
    let wc = watcher.add_file_contents(&contents).unwrap();
    for path in [&whole, &contents] {
        let staged = root.join("staged");
        std::fs::write(&staged, b"new").unwrap();
        std::fs::rename(&staged, path).unwrap();
    }
    let mut fired = Vec::new();
    while fired.len() < 2 {
        let more = watcher.wait(Some(aterm_spec::wait::MUST_HAPPEN)).unwrap();
        assert!(!more.is_empty(), "a rename over a watched file names it");
        fired.extend(more);
        fired.sort_unstable();
        fired.dedup();
    }
    assert_eq!(fired, [ww, wc]);
    assert!(
        watcher.is_gone(ww) && watcher.is_gone(wc),
        "the file each was on went"
    );
}

/// A CONTENTS WATCH HEARS ITS FILE'S BYTES AND NOT ITS TIMES OR MODE
/// ([`Watcher::add_file_contents`]): a `touch` (opened for writing, its time moved, closed
/// unwritten, as a usage tap's re-report is) and a `chmod` name nothing, where a whole-file
/// watch on the same file is named by each; an append and a rewrite in place name both.
///
/// NEGATIVE CONTROL (run 2026-11-15): with `NOTE_ATTRIB` in the contents arm's flags, the test
/// fails at "a touch names no contents watch" (`left: [WatchId(1), WatchId(2)]`).
#[test]
fn a_contents_watch_hears_no_touch() {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;

    let root = crate::testdir::dir("atpkg-dirwatch-contents-");
    let path = root.join("reading.json");
    std::fs::write(&path, b"{}").unwrap();
    let watcher = Watcher::new().unwrap();
    let contents = watcher.add_file_contents(&path).unwrap();
    let whole = watcher.add_file(&path).unwrap();
    let touch = || {
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(std::time::SystemTime::now() + Duration::from_secs(5))
            .unwrap();
    };
    touch();
    assert_eq!(
        watcher.wait(Some(aterm_spec::wait::MUST_HAPPEN)).unwrap(),
        [whole],
        "a touch names no contents watch"
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        watcher.wait(Some(aterm_spec::wait::MUST_HAPPEN)).unwrap(),
        [whole],
        "nor a chmod"
    );
    let mut out = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    out.write_all(b"\n").unwrap();
    drop(out);
    assert_eq!(watcher.wait(NOW).unwrap(), [contents, whole], "a write");
    std::fs::write(&path, b"{\"rewritten\":true}").unwrap();
    assert_eq!(
        watcher.wait(NOW).unwrap(),
        [contents, whole],
        "a rewrite in place"
    );
    assert!(watcher.add_file_contents(&root).is_err(), "a directory");
}

#[test]
fn one_directory_watched_twice_keeps_the_other_watch() {
    let root = crate::testdir::dir("atpkg-dirwatch-twice-");
    let watcher = Watcher::new().unwrap();
    let first = watcher.add_dir(&root).unwrap();
    let second = watcher.add_dir(&root).unwrap();
    std::fs::write(root.join("x"), b"").unwrap();
    assert_eq!(watcher.wait(NOW).unwrap(), [first, second]);
    watcher.remove(first);
    std::fs::write(root.join("y"), b"").unwrap();
    assert_eq!(watcher.wait(NOW).unwrap(), [second]);
}

#[test]
fn a_wake_ends_the_wait() {
    let watcher = std::sync::Arc::new(Watcher::new().unwrap());
    let waiting = || {
        let (tx, rx) = std::sync::mpsc::channel();
        let watcher = std::sync::Arc::clone(&watcher);
        std::thread::spawn(move || tx.send(watcher.wait(None).unwrap()));
        rx
    };
    watcher.wake().unwrap();
    let kept = waiting()
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("a wake given before the wait ends it");
    assert!(kept.is_empty());
    let rx = waiting();
    watcher.wake().unwrap();
    let woke = rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the woken wait returns");
    assert!(woke.is_empty());
    watcher.wake().unwrap();
    assert!(watcher.wait(Some(Duration::MAX)).unwrap().is_empty());
}

#[test]
fn a_symlink_or_the_wrong_kind_is_refused() {
    let root = crate::testdir::dir("atpkg-dirwatch-kind-");
    let (dir, file) = (root.join("dir"), root.join("file"));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(&file, b"").unwrap();
    std::os::unix::fs::symlink(&dir, root.join("dir-link")).unwrap();
    std::os::unix::fs::symlink(&file, root.join("file-link")).unwrap();
    let watcher = Watcher::new().unwrap();
    assert!(watcher.add_dir(&root.join("dir-link")).is_err());
    assert!(watcher.add_file(&root.join("file-link")).is_err());
    assert!(watcher.add_dir(&file).is_err());
    assert!(watcher.add_file(&dir).is_err());
    assert!(watcher.add_dir(&root.join("missing")).is_err());
    assert!(watcher.add_dir(&dir).is_ok() && watcher.add_file(&file).is_ok());
}

/// A child killed and reaped when the test lets go of it, so a failed assertion leaves no
/// `/bin/sleep` holding the test's output open.
#[cfg(target_os = "macos")]
struct KilledOnDrop(aterm_uds::spawnfd::Child);

#[cfg(target_os = "macos")]
impl Drop for KilledOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A PROCESS WATCH NAMES ITS EXEC AND THEN ITS EXIT (macOS, where [`Watcher::HEARS_EXEC`]): a
/// child shell watched twice, once a line it wrote shows it runs as the shell itself (macOS's
/// `/bin/sh` execs the shell it selects, an exec of its own that a watch placed before it would
/// hear), is named by neither watch until it execs `/bin/sleep`, which names both, neither gone;
/// with one removed, the kill names the other alone, gone, and a later wait names nothing. A
/// reaped child's pid is refused as `NotFound`.
///
/// NEGATIVE CONTROL (run 2026-11-15): with every `EVFILT_PROC` event read as an exit in the
/// queue's read (`exited` true whatever its `fflags`), the test fails at "the exec leaves both":
/// the exec marks both watches gone.
#[cfg(target_os = "macos")]
#[test]
fn a_process_watch_names_its_exec_and_then_its_exit() {
    use std::io::{BufRead as _, Write as _};

    let mut child = KilledOnDrop(
        aterm_uds::spawnfd::command("/bin/sh")
            .args(["-c", "echo running; read _; exec /bin/sleep 600"])
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .spawn()
            .expect("a child shell"),
    );
    let mut said = String::new();
    io::BufReader::new(child.0.stdout.take().unwrap())
        .read_line(&mut said)
        .unwrap();
    assert_eq!(said, "running\n", "the shell runs past its own exec");
    let pid = child.0.id();
    let watcher = Watcher::new().unwrap();
    let first = watcher.add_process(pid).unwrap();
    let second = watcher.add_process(pid).unwrap();
    assert!(
        watcher.wait(NOW).unwrap().is_empty(),
        "CONTROL: nothing yet"
    );
    child.0.stdin.take().unwrap().write_all(b"\n").unwrap();
    assert_eq!(
        watcher.wait(Some(aterm_spec::wait::MUST_HAPPEN)).unwrap(),
        [first, second],
        "the exec"
    );
    assert!(
        !watcher.is_gone(first) && !watcher.is_gone(second),
        "the exec leaves both"
    );
    watcher.remove(first);
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    assert_eq!(
        watcher.wait(Some(aterm_spec::wait::MUST_HAPPEN)).unwrap(),
        [second],
        "the exit"
    );
    assert!(watcher.is_gone(second));
    assert!(watcher.wait(NOW).unwrap().is_empty(), "nothing after it");
    assert_eq!(
        watcher.add_process(pid).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
}

/// x86_64's `poll` syscall number (`<asm/unistd_64.h>`), which the generated libc does not carry.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const SYS_POLL: libc::c_long = 7;
/// The `ppoll` syscall number, which the generated libc does not carry: x86_64's
/// (`<asm/unistd_64.h>`) and, elsewhere, asm-generic's, which arm64 uses.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const SYS_PPOLL: libc::c_long = 271;
#[cfg(all(target_os = "linux", not(target_arch = "x86_64")))]
const SYS_PPOLL: libc::c_long = 73;

/// Whether Linux syscall `nr` is the one a wait blocks in: `poll`, which a C library may make as
/// `ppoll` (the only one an arm64 kernel has).
#[cfg(target_os = "linux")]
fn is_poll(nr: libc::c_long) -> bool {
    #[cfg(target_arch = "x86_64")]
    if nr == SYS_POLL {
        return true;
    }
    nr == SYS_PPOLL
}

/// A PROCESS WATCH NAMES ITS EXIT, EVEN ONE PLACED WHILE A WAIT RUNS (Linux: a pidfd, which
/// hears no exec): a wait already blocked in its poll in another thread (the kernel names the
/// thread's syscall, `/proc/self/task/<tid>/syscall`, before the watch is placed) when the watch
/// of a child is placed returns that watch at the child's kill, gone; a later wait no longer
/// polls it; a reaped child's pid is refused as `NotFound`. A kernel with no pidfd (`ENOSYS`,
/// before Linux 5.3) cannot judge the watch, and the test fails saying so rather than pass.
///
/// Compiled for Linux by the linux-arm cell, but not run on this branch's host (macOS); no
/// negative control was run.
#[cfg(target_os = "linux")]
#[test]
fn a_process_watch_names_its_exit_even_one_placed_while_a_wait_runs() {
    let mut child = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("600")
        .spawn()
        .expect("a child");
    let pid = child.id();
    let watcher = std::sync::Arc::new(Watcher::new().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    let (tid_tx, tid_rx) = std::sync::mpsc::channel();
    let waiting = std::sync::Arc::clone(&watcher);
    std::thread::spawn(move || {
        // SAFETY: `gettid` takes no argument and cannot fail.
        let _ = tid_tx.send(unsafe { libc::syscall(libc::SYS_gettid) });
        tx.send(waiting.wait(None).unwrap())
    });
    let tid = tid_rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the waiting thread starts");
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the waiting thread blocks in its poll",
        || {
            let syscall = std::fs::read_to_string(format!("/proc/self/task/{tid}/syscall")).ok()?;
            let nr: libc::c_long = syscall.split_whitespace().next()?.parse().ok()?;
            is_poll(nr).then_some(())
        },
    );
    let id = match watcher.add_process(pid) {
        Ok(id) => id,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            watcher.wake().unwrap();
            if e.raw_os_error() == Some(libc::ENOSYS) {
                panic!(
                    "COULD NOT RUN: pidfd_open returned ENOSYS (a kernel before Linux 5.3 has no \
                     process watch to judge)"
                );
            }
            panic!("watch the child: {e}");
        }
    };
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(
        rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the running wait hears the exit"),
        [id]
    );
    assert!(watcher.is_gone(id));
    assert!(
        watcher.wait(NOW).unwrap().is_empty(),
        "a gone watch is polled no more"
    );
    assert_eq!(
        watcher.add_process(pid).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
}

/// A READ WATCH NAMES ITS DESCRIPTOR WHILE SOMETHING IS THERE TO READ ([`Watcher::add_readable`],
/// what a wall clock's watch is polled through): a pipe with nothing in it names nothing; a byte
/// written names the watch at every wait until it is read off, and then nothing; a removed
/// watch's pipe names nothing however much is written; every writer closed names the watch once
/// more, and it is gone.
///
/// NEGATIVE CONTROL (run 2026-11-15): with the macOS read filter registered `EV_CLEAR`
/// (edge-triggered), the test fails at "named while unread" with `left: [] right: [WatchId(1)]`.
#[test]
fn a_read_watch_names_its_descriptor_until_it_is_read_off() {
    use std::io::{Read as _, Write as _};
    use std::os::fd::AsFd as _;

    let (reader, writer) = aterm_uds::pipe().expect("a pipe");
    let (other, other_writer) = aterm_uds::pipe().expect("a second pipe");
    let watcher = Watcher::new().unwrap();
    let id = watcher.add_readable(reader.as_fd()).unwrap();
    let removed = watcher.add_readable(other.as_fd()).unwrap();
    assert!(
        watcher.wait(NOW).unwrap().is_empty(),
        "CONTROL: nothing to read"
    );
    (&writer).write_all(b"x").unwrap();
    assert_eq!(watcher.wait(NOW).unwrap(), [id]);
    assert_eq!(watcher.wait(NOW).unwrap(), [id], "named while unread");
    let mut byte = [0u8; 1];
    (&reader).read_exact(&mut byte).unwrap();
    assert!(watcher.wait(NOW).unwrap().is_empty(), "read off: nothing");
    watcher.remove(removed);
    (&other_writer).write_all(b"x").unwrap();
    assert!(
        watcher.wait(NOW).unwrap().is_empty(),
        "a removed watch names nothing"
    );
    assert!(!watcher.is_gone(id));
    drop(writer);
    assert_eq!(watcher.wait(NOW).unwrap(), [id], "every writer closed");
    assert!(watcher.is_gone(id), "and gone");
    assert!(watcher.wait(NOW).unwrap().is_empty(), "named once");
    drop((other, other_writer));
}
