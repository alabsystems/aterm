// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::wait::MUST_HAPPEN;
use std::path::PathBuf;

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
    assert!(release(&path, &link).is_none());
    assert!(std::fs::symlink_metadata(&path).unwrap().is_dir());
    assert_eq!(std::fs::read_link(&link).unwrap(), Path::new(name));

    std::fs::remove_dir(&path).unwrap();
    mkfifo(&path);
    let wake = release(&path, &link).expect("the FIFO's read/write wake");
    assert!(std::fs::symlink_metadata(&path).is_err());
    assert!(std::fs::symlink_metadata(&link).is_err());
    drop(wake);
}

/// A dead owner's FIFO released while no reader holds it: a shell that resolved its link before
/// the release and opens the FIFO while the release holds its wake gets through at once, and reads
/// the end when the wake closes.
#[test]
fn a_shell_opening_its_fifo_during_the_release_reads_the_end() {
    use std::io::Read as _;
    use std::sync::mpsc::channel;
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("7-1-s-0a");
    mkfifo(&path);
    let link = dir.path().join("s-0a");
    std::os::unix::fs::symlink("7-1-s-0a", &link).expect("link");
    // The FIFO as the shell holds it once it has resolved the link: a name the release keeps.
    let resolved: PathBuf = dir.path().join("resolved");
    std::fs::hard_link(&path, &resolved).expect("hard link");

    let wake = release(&path, &link);
    assert!(
        std::fs::symlink_metadata(&link).is_err() && std::fs::symlink_metadata(&path).is_err(),
        "the release unlinked the link and the FIFO"
    );
    let (opened, opened_rx) = channel();
    let (read, read_rx) = channel();
    let shell = std::thread::spawn({
        let resolved = resolved.clone();
        move || {
            let mut fifo = std::fs::File::open(&resolved).expect("open the FIFO");
            let _ = opened.send(());
            let mut buf = [0u8; 8];
            let _ = read.send(fifo.read(&mut buf).map_err(|e| e.kind()));
        }
    });
    if opened_rx.recv_timeout(MUST_HAPPEN).is_err() {
        // Let the waiting open through, so no thread is left blocked in it.
        drop(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&resolved),
        );
        panic!("the shell's open waited: the release held no wake on the FIFO");
    }
    drop(wake);
    assert_eq!(
        read_rx.recv_timeout(MUST_HAPPEN),
        Ok(Ok(0)),
        "the shell reads the end once the wake closes"
    );
    shell.join().expect("the shell's thread");
}
