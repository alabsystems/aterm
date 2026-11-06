// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::wait::MUST_HAPPEN;

fn mkfifo(path: &Path) {
    use std::os::unix::ffi::OsStrExt as _;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("path");
    // SAFETY: `c` is a NUL-terminated path that lives across the call.
    assert_eq!(
        unsafe { libc::mkfifo(c.as_ptr(), 0o600) },
        0,
        "mkfifo {}",
        path.display()
    );
}

fn birth_of(pid: i32) -> u64 {
    read_birth(pid).expect("a running process's birth").0
}

/// The sweep releases a gone, reused or zombie owner's FIFO and leaves a live or unread owner's,
/// a name it does not parse, and a file that is no FIFO.
#[test]
fn the_sweep_releases_only_the_fifos_whose_owner_has_ended() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let at = |name: &str| dir.path().join(name);
    let link = |to: &str, sid: &str| std::os::unix::fs::symlink(to, at(sid)).expect("link");
    let sleeper = || {
        aterm_uds::spawnfd::command("/bin/sleep")
            .arg("600")
            .spawn()
            .expect("sleep")
    };
    let mut gone = sleeper();
    let gone_pid = i32::try_from(gone.id()).expect("pid");
    let gone_birth = birth_of(gone_pid);
    gone.kill().expect("kill");
    gone.wait().expect("reap");
    let mut zombie = sleeper();
    let zombie_pid = i32::try_from(zombie.id()).expect("pid");
    let zombie_birth = birth_of(zombie_pid);
    zombie.kill().expect("kill");
    aterm_spec::wait::until_within(MUST_HAPPEN, "the killed child reads exited", || {
        (aterm_uds::process::process_presence(zombie_pid)
            == aterm_uds::process::ProcessPresence::Exited)
            .then_some(())
    });
    let me = i32::try_from(std::process::id()).expect("pid");
    let my_birth = birth_of(me);

    let dead = format!("{gone_pid}-{gone_birth}-s-0a");
    mkfifo(&at(&dead));
    link(&dead, "s-0a");
    mkfifo(&at(&format!("{me}-{}-s-0b", my_birth + 1)));
    let live = format!("{me}-{my_birth}-s-0c");
    mkfifo(&at(&live));
    link(&live, "s-0c");
    mkfifo(&at("s-0d"));
    let not_fifo = format!("{gone_pid}-{gone_birth}-s-0e");
    std::fs::write(at(&not_fifo), b"").expect("file");
    let unread = format!("{me}-0-s-0f");
    mkfifo(&at(&unread));
    mkfifo(&at(&format!("{gone_pid}-0-s-10")));
    mkfifo(&at(&format!("{zombie_pid}-{zombie_birth}-s-11")));

    sweep_dead(dir.path());
    zombie.wait().expect("reap");
    let mut left: Vec<String> = std::fs::read_dir(dir.path())
        .expect("read")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    let mut kept = vec![live, "s-0c".to_owned(), "s-0d".to_owned(), not_fifo, unread];
    kept.sort();
    assert_eq!(left, kept);
}

#[test]
fn unread_owners_keep_their_fifos_until_exit_or_reuse_is_proved() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let rows = [
        (11, 41, Some(Birth(41)), ProcessPresence::Unknown, true),
        (12, 41, Some(Birth(42)), ProcessPresence::Present, false),
        (13, 41, None, ProcessPresence::Present, true),
        (14, 41, None, ProcessPresence::Unknown, true),
        (15, 41, None, ProcessPresence::Exited, false),
        (16, 41, None, ProcessPresence::Absent, false),
        (17, 0, None, ProcessPresence::Present, true),
        (18, 0, None, ProcessPresence::Unknown, true),
        (19, 0, None, ProcessPresence::Exited, false),
        (20, 0, None, ProcessPresence::Absent, false),
    ];
    for (pid, birth, ..) in rows {
        let sid = format!("s-{pid:x}");
        let name = format!("{pid}-{birth}-{sid}");
        mkfifo(&dir.path().join(&name));
        std::os::unix::fs::symlink(&name, dir.path().join(&sid)).expect("link");
    }
    sweep_dead_with(
        dir.path(),
        |pid| {
            let row = rows.iter().find(|r| r.0 == pid).expect("known pid");
            assert_ne!(row.1, 0, "a birthless name only asks presence");
            row.2
        },
        |pid| {
            let row = rows.iter().find(|r| r.0 == pid).expect("known pid");
            assert!(row.2.is_none(), "a read birth already decides identity");
            row.3
        },
    );
    let mut expected = Vec::new();
    for (pid, birth, _, _, keep) in rows {
        let sid = format!("s-{pid:x}");
        let name = format!("{pid}-{birth}-{sid}");
        let path = dir.path().join(&name);
        let link = dir.path().join(&sid);
        if keep {
            use std::os::unix::fs::FileTypeExt as _;
            assert!(
                std::fs::symlink_metadata(&path)
                    .unwrap()
                    .file_type()
                    .is_fifo()
            );
            assert_eq!(std::fs::read_link(&link).unwrap(), Path::new(&name));
            expected.extend([name, sid]);
        } else {
            assert!(std::fs::symlink_metadata(&path).is_err());
            assert!(std::fs::symlink_metadata(&link).is_err());
        }
    }
    let mut actual: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_str().unwrap().to_owned())
        .collect();
    actual.sort();
    expected.sort();
    assert_eq!(actual, expected);
}

#[test]
fn a_failed_wake_open_keeps_both_names_for_a_later_release() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let name = "7-1-s-0a";
    let path = dir.path().join(name);
    let link = dir.path().join("s-0a");
    // A real read/write-open error, independent of uid or descriptor limits. This models
    // the failure boundary, not the EMFILE cause that can also reach it for a real FIFO.
    std::fs::create_dir(&path).unwrap();
    std::os::unix::fs::symlink(name, &link).unwrap();
    assert!(release(&path, &link, write_end).is_none());
    assert!(std::fs::symlink_metadata(&path).unwrap().is_dir());
    assert_eq!(std::fs::read_link(&link).unwrap(), Path::new(name));

    std::fs::remove_dir(&path).unwrap();
    std::fs::write(&path, b"not a fifo").unwrap();
    assert!(release(&path, &link, write_end).is_none());
    assert!(std::fs::symlink_metadata(&path).unwrap().is_file());
    assert_eq!(std::fs::read_link(&link).unwrap(), Path::new(name));
    std::fs::remove_file(&path).unwrap();

    let outside = dir.path().join("outside-fifo");
    mkfifo(&outside);
    std::os::unix::fs::symlink(&outside, &path).unwrap();
    assert!(release(&path, &link, write_end).is_none());
    assert!(
        std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(std::fs::symlink_metadata(&outside).is_ok());
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_file(&outside).unwrap();

    mkfifo(&path);
    let wake = release(&path, &link, write_end).expect("the FIFO's read/write wake");
    assert!(std::fs::symlink_metadata(&path).is_err());
    assert!(std::fs::symlink_metadata(&link).is_err());
    drop(wake);
}

/// A FIFO some other writer filled never holds the release: its wake is non-blocking, so the line
/// that does not fit fails to write and the release returns at once, with both names gone and no
/// wake to keep (a shell that got through has the stray bytes to read already). A blocking wake
/// would wait in that write for ever, and with it the sweep's caller, aterm's spare thread.
#[test]
fn a_full_pipe_never_holds_the_release() {
    use std::io::{Read as _, Write as _};
    use std::os::unix::fs::OpenOptionsExt as _;
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("7-1-s-0a");
    mkfifo(&path);
    let link = dir.path().join("s-0a");
    std::os::unix::fs::symlink("7-1-s-0a", &link).expect("link");
    // Read-write, so the bytes it writes stay in the pipe; it reads them only to free a release
    // left waiting, so the test ends at its assertion rather than in a thread that never returns.
    let stray = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&path)
        .expect("the stray writer");
    for chunk in [&[b'x'; 512][..], &b"x"[..]] {
        loop {
            match (&stray).write(chunk) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => panic!("fill the pipe: {e}"),
            }
        }
    }
    let (released, released_rx) = std::sync::mpsc::channel();
    let releasing = std::thread::spawn({
        let (path, link) = (path.clone(), link.clone());
        move || {
            let _ = released.send(release(&path, &link, write_end).is_some());
        }
    });
    let got = released_rx.recv_timeout(MUST_HAPPEN);
    if got.is_err() {
        while (&stray).read(&mut [0u8; 512]).is_ok_and(|n| n > 0) {}
    }
    releasing.join().expect("the release's thread");
    drop(stray);
    assert_eq!(
        got,
        Ok(false),
        "the release returned without its line: it did not wait on the full pipe"
    );
    assert_eq!(
        names_in(dir.path()),
        Vec::<String>::new(),
        "both names are gone"
    );
}

/// Lines written through a read-write open of `late`, held until the caller drops it: a reader
/// left waiting at any step of [`read_late`], or of a test's own read, gets through, so no thread
/// is left blocked when a test fails.
fn let_through(late: &Path) -> Option<std::fs::File> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let rescue = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(late)
        .ok()?;
    let _ = (&rescue).write_all(b"\n\n");
    Some(rescue)
}

/// What a shell that resolved its FIFO as `late` before a release, and opens it only now, reads:
/// its first line, then what the read after it returns (0 is end-of-file: nothing holds the FIFO
/// open to write any more). A one-minute hang detector bounds the reads.
fn read_late(
    late: &Path,
) -> Result<Result<(String, usize), std::io::ErrorKind>, std::sync::mpsc::RecvTimeoutError> {
    use std::io::{BufRead as _, Read as _};
    let (got, got_rx) = std::sync::mpsc::channel();
    let shell = std::thread::spawn({
        let late = late.to_owned();
        move || {
            let read = std::fs::File::open(&late)
                .map(std::io::BufReader::new)
                .and_then(|mut fifo| {
                    let mut line = String::new();
                    fifo.read_line(&mut line)?;
                    let after = fifo.read(&mut [0u8; 8])?;
                    Ok((line, after))
                });
            let _ = got.send(read.map_err(|e| e.kind()));
        }
    });
    let read = got_rx.recv_timeout(MUST_HAPPEN);
    let rescue = read.is_err().then(|| let_through(late));
    shell.join().expect("the shell's thread");
    drop(rescue);
    read
}

/// The names left in `dir`.
fn names_in(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .expect("read")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

/// A dead owner's FIFO swept while no reader holds it: a shell that resolved its link before the
/// unlinks and enters the FIFO's open only once the sweep has returned gets through, reads the
/// line the sweep left there, and then end-of-file, once the sweep's wake has closed because the
/// FIFO is empty.
#[test]
fn a_shell_opening_its_fifo_after_the_sweep_reads_the_end_line_then_end_of_file() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("7-1-s-0a");
    mkfifo(&path);
    std::os::unix::fs::symlink("7-1-s-0a", dir.path().join("s-0a")).expect("link");
    // The FIFO as the shell holds it once it has resolved the link: a name the sweep keeps.
    let late = dir.path().join("late");
    std::fs::hard_link(&path, &late).expect("hard link");

    sweep_dead_with(dir.path(), |_| None, |_| ProcessPresence::Absent);
    assert_eq!(
        names_in(dir.path()),
        ["late"],
        "the sweep unlinked the link and the FIFO"
    );
    assert_eq!(
        read_late(&late),
        Ok(Ok(("end\n".to_owned(), 0))),
        "the shell's open finds the sweep's wake, reads its line, then the end of the file"
    );
}

/// A shell that opens its FIFO as the release starts, so it waits in the open or enters it while
/// the wake is held, is let through by the wake before the line is written, and the line comes
/// only once both names are gone: read sooner, it would send the shell back to a name still
/// there, to wait on the kept wake for a line that never comes.
#[test]
fn a_shell_in_its_fifos_open_reads_the_line_only_once_both_names_are_gone() {
    use std::io::BufRead as _;
    use std::sync::mpsc::channel;
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("7-1-s-0a");
    mkfifo(&path);
    let link = dir.path().join("s-0a");
    std::os::unix::fs::symlink("7-1-s-0a", &link).expect("link");
    let late = dir.path().join("late");
    std::fs::hard_link(&path, &late).expect("hard link");
    let (opened, opened_rx) = channel();
    let (line, line_rx) = channel();
    let shell = std::thread::spawn({
        let late = late.clone();
        move || {
            let read = std::fs::File::open(&late).and_then(|fifo| {
                let _ = opened.send(());
                let mut got = String::new();
                std::io::BufReader::new(fifo)
                    .read_line(&mut got)
                    .map(|_| got)
            });
            let _ = line.send(read.map_err(|e| e.kind()));
        }
    });

    let mut at_the_line = None;
    let wake = release(&path, &link, |wake| {
        at_the_line = Some((
            opened_rx.recv_timeout(MUST_HAPPEN).is_ok(),
            std::fs::symlink_metadata(&link).is_err() && std::fs::symlink_metadata(&path).is_err(),
        ));
        write_end(wake)
    });
    let got = line_rx.recv_timeout(MUST_HAPPEN);
    let rescue = got.is_err().then(|| let_through(&late));
    shell.join().expect("the shell's thread");
    drop((rescue, wake));
    assert_eq!(
        at_the_line,
        Some((true, true)),
        "when the line is written the wake has let the shell through, and both names are gone"
    );
    assert_eq!(got, Ok(Ok("end\n".to_owned())), "the shell reads the line");
}

/// Return once `reader`, a non-blocking reader of a FIFO, reads end-of-file: nothing writes the
/// FIFO any more. A child that another test forks holds a copy of each descriptor until it execs,
/// so a closed wake can go on writing for that long; what the FIFO still holds is read and passed
/// over.
fn until_end_of_file(reader: &mut std::fs::File, what: &str) {
    use std::io::Read as _;
    aterm_spec::wait::until_within(MUST_HAPPEN, what, || match reader.read(&mut [0u8; 8]) {
        Ok(0) => Some(()),
        Ok(_) => None,
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => None,
        Err(e) => panic!("{what}: {e}"),
    });
}

/// A kept wake closes once a shell has emptied its FIFO, or once a minute has passed with its line
/// unread, and not before.
#[test]
fn a_kept_wake_closes_once_its_fifo_is_read_or_its_minute_is_up() {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let released = |sid: &str| {
        let path = dir.path().join(format!("7-1-{sid}"));
        mkfifo(&path);
        let late = dir.path().join(format!("late-{sid}"));
        std::fs::hard_link(&path, &late).expect("hard link");
        let wake = release(&path, &dir.path().join(sid), write_end).expect("the wake");
        (wake, late)
    };
    let reader = |late: &Path| {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(late)
            .expect("a reader")
    };
    let mut buf = [0u8; 8];
    let (read_wake, read_late) = released("s-0a");
    let (unread_wake, unread_late) = released("s-0b");
    let t0 = Instant::now();
    let mut keeper = Keeper::default();
    keeper.keep(read_wake, t0);
    keeper.keep(unread_wake, t0);
    let mut shell = reader(&read_late);
    assert_eq!(
        shell
            .read(&mut buf)
            .map(|n| buf[..n].to_vec())
            .map_err(|e| e.kind()),
        Ok(END.to_vec()),
        "a shell reads one FIFO's line"
    );

    keeper.reap(t0);
    assert_eq!(
        keeper.kept.len(),
        1,
        "the emptied FIFO's wake closes; the other's line is unread"
    );
    until_end_of_file(
        &mut shell,
        "the emptied FIFO's closed wake to stop writing it",
    );
    keeper.reap(t0 + KEEP_FOR - Duration::from_millis(1));
    assert_eq!(
        keeper.kept.len(),
        1,
        "the unread line keeps its wake for its minute"
    );
    keeper.reap(t0 + KEEP_FOR);
    assert!(keeper.kept.is_empty(), "its minute up, the wake closes");
    until_end_of_file(
        &mut reader(&unread_late),
        "the unread FIFO's closed wake to stop writing it",
    );
}

/// The hold's two ends: each close-on-exec, so no other child of the host's gets one; the host's
/// writes never wait (`O_NONBLOCK`), and the shell's end blocks, as a hold's `read` needs (a
/// non-blocking read fails at once and spends the retries). The shell's end is never close-on-fork,
/// which the spawn that hands it to the shell would refuse. What the host writes, the shell's end
/// reads, and once the host's end closes it reads end-of-file.
#[test]
fn the_hold_ends_are_close_on_exec_and_only_the_shells_blocks() {
    use std::io::{Read as _, Write as _};
    use std::os::fd::AsRawFd as _;
    /// `FD_CLOFORK`, which Apple's libc crate does not name; elsewhere a bit no descriptor sets.
    const FD_CLOFORK: i32 = 2;
    let HoldEnds { host, shell } = hold_ends().expect("the hold's ends");
    let flags = |fd: i32| {
        // SAFETY: `F_GETFD` and `F_GETFL` read the flags of `fd`, open here; no memory is touched.
        unsafe {
            (
                libc::fcntl(fd, libc::F_GETFD) & libc::FD_CLOEXEC != 0,
                libc::fcntl(fd, libc::F_GETFL) & libc::O_NONBLOCK != 0,
            )
        }
    };
    assert_eq!(
        (flags(host.as_raw_fd()), flags(shell.as_raw_fd())),
        ((true, true), (true, false)),
        "(close-on-exec, non-blocking) of the host's end, then the shell's"
    );
    // SAFETY: `F_GETFD` reads the flags of the shell's end, open here; no memory is touched.
    let shell_fd = unsafe { libc::fcntl(shell.as_raw_fd(), libc::F_GETFD) };
    assert_eq!(shell_fd & FD_CLOFORK, 0, "the shell's end is forkable");
    (&host).write_all(b"go\n").expect("the host writes");
    drop(host);
    let mut read = String::new();
    (&shell)
        .read_to_string(&mut read)
        .expect("the line, then end-of-file");
    assert_eq!(read, "go\n");
}
