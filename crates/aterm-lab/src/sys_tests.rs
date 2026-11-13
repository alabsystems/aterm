// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

struct Dir(crate::TestScratch);

fn dir(name: &str) -> Dir {
    Dir(crate::TestScratch::new(name))
}

#[test]
fn tty_name_names_a_pseudo_terminal_from_its_number_alone() {
    use std::os::unix::fs::MetadataExt as _;
    let ptmx = |minor: i32| (16 << 24) | minor;
    assert_eq!(tty_name(None), "??");
    assert_eq!(tty_name(Some(0)), "??");
    assert_eq!(tty_name(Some(ptmx(0))), "ttys000");
    assert_eq!(tty_name(Some(ptmx(31))), "ttys031");
    assert_eq!(tty_name(Some(ptmx(511))), "ttys511");
    // The kernel's own encoding, read off a node that is no terminal: /dev/null
    // is major 3, minor 2.
    let null = std::fs::metadata("/dev/null").expect("/dev/null");
    let dev = i32::try_from(null.rdev()).expect("/dev/null's device number");
    assert_eq!(tty_name(Some(dev)), "dev3.2");
}

#[test]
fn put_preserves_occupied_stages_and_source_refusals() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
    let scratch = aterm_tempfile::Builder::new()
        .prefix("lab-put-owned-")
        .suffix(".noindex")
        .tempdir()
        .expect("exclusive scratch");
    let root = scratch.path().canonicalize().expect("physical scratch");
    let stage = |dst: &Path, nonce: u32| {
        dst.with_file_name(format!(
            ".{}.lab-{}-{nonce:08x}",
            dst.file_name().expect("leaf").to_string_lossy(),
            std::process::id()
        ))
    };
    let stamp = |path: &Path| {
        std::fs::symlink_metadata(path).ok().map(|m| {
            let bytes = if m.file_type().is_symlink() {
                std::fs::read_link(path)
                    .expect("link")
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec()
            } else {
                std::fs::read(path).expect("standing regular file")
            };
            (m.dev(), m.ino(), m.mode(), bytes)
        })
    };
    let destination = root.join("ordinary");
    put_with_nonce(&destination, 0o640, &PutSrc::Bytes(b"ordinary"), || 1).expect("ordinary write");
    assert_eq!(std::fs::read(&destination).unwrap(), b"ordinary");
    assert_eq!(
        std::fs::metadata(&destination)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o640
    );
    assert!(std::fs::symlink_metadata(stage(&destination, 1)).is_err());
    let source = root.join("source");
    std::fs::write(&source, b"source bytes").unwrap();
    let copy = root.join("copy");
    put_with_nonce(&copy, 0o600, &PutSrc::File(&source), || 2).expect("file clone or copy");
    assert_eq!(std::fs::read(&copy).unwrap(), b"source bytes");
    let replace = root.join("replace-link");
    symlink(&source, &replace).unwrap();
    put_with_nonce(&replace, 0o600, &PutSrc::Bytes(b"replacement"), || 3).unwrap();
    assert_eq!(std::fs::read(&replace).unwrap(), b"replacement");
    assert!(
        !std::fs::symlink_metadata(&replace)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read(&source).unwrap(), b"source bytes");
    let directory = root.join("directory");
    std::fs::create_dir(&directory).unwrap();
    assert!(put_with_nonce(&directory, 0o600, &PutSrc::Bytes(b"no"), || 4).is_err());
    assert!(directory.is_dir());
    assert!(std::fs::symlink_metadata(stage(&directory, 4)).is_err());
    let parent_link = root.join("via");
    symlink(&directory, &parent_link).unwrap();
    assert!(
        put_with_nonce(
            &parent_link.join("child"),
            0o600,
            &PutSrc::Bytes(b"no"),
            || 5
        )
        .is_err()
    );
    assert!(std::fs::read_dir(&directory).unwrap().next().is_none());

    let missing = root.join("missing-source");
    let source_link = root.join("source-link");
    symlink(&source, &source_link).unwrap();
    let cases = [
        ("bytes/regular", PutSrc::Bytes(&b"new"[..]), false),
        ("bytes/link", PutSrc::Bytes(&b"new"[..]), true),
        ("file/regular", PutSrc::File(source.as_path()), false),
        ("file/link", PutSrc::File(source.as_path()), true),
        ("missing source", PutSrc::File(missing.as_path()), false),
        ("linked source", PutSrc::File(source_link.as_path()), false),
        ("directory source", PutSrc::File(directory.as_path()), false),
    ];
    let mut observed = Vec::new();
    for (i, (what, src, link)) in cases.into_iter().enumerate() {
        let dst = root.join(format!("destination-{i}"));
        std::fs::write(&dst, b"standing destination").unwrap();
        let nonce = 100 + u32::try_from(i).unwrap();
        let tmp = stage(&dst, nonce);
        let other = root.join(format!("other-{i}"));
        std::fs::write(&other, b"standing other").unwrap();
        if link {
            symlink(&other, &tmp).unwrap();
        } else {
            std::fs::write(&tmp, b"standing stage").unwrap();
        }
        let before = stamp(&tmp).expect("occupied stage");
        let refused = put_with_nonce(&dst, 0o600, &src, || nonce).is_err();
        observed.push((
            what,
            refused,
            stamp(&tmp).as_ref() == Some(&before),
            std::fs::read(&dst).unwrap() == b"standing destination",
            std::fs::read(&other).unwrap() == b"standing other",
        ));
    }
    assert!(
        observed
            .iter()
            .all(|(_, refused, stage, dst, other)| *refused && *stage && *dst && *other),
        "put never cleans up a stage it did not create: {observed:?}"
    );
}

#[test]
fn put_writes_by_a_rename_and_never_through_a_link() {
    let d = dir("put");
    let p = d.0.join("a/b/file");
    put(&p, 0o640, &PutSrc::Bytes(b"one")).unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), b"one");
    use std::os::unix::fs::PermissionsExt as _;
    assert_eq!(
        std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
        0o640
    );
    let outside = d.0.join("outside");
    std::fs::write(&outside, "keep").unwrap();
    let at = d.0.join("a/link");
    std::os::unix::fs::symlink(&outside, &at).unwrap();
    put(&at, 0o600, &PutSrc::Bytes(b"two")).unwrap();
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep");
    assert!(
        !std::fs::symlink_metadata(&at)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let elsewhere = d.0.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, d.0.join("via")).unwrap();
    assert!(put(&d.0.join("via/x"), 0o600, &PutSrc::Bytes(b"no")).is_err());
    assert!(std::fs::read_dir(&elsewhere).unwrap().next().is_none());
    put(&d.0.join("copy"), 0o644, &PutSrc::File(&p)).unwrap();
    assert_eq!(std::fs::read(d.0.join("copy")).unwrap(), b"one");
    assert!(
        put(
            &d.0.join("copy2"),
            0o644,
            &PutSrc::File(&at.with_file_name("no-such"))
        )
        .is_err()
    );
    std::os::unix::fs::symlink(&p, d.0.join("srclink")).unwrap();
    assert!(
        put(
            &d.0.join("copy3"),
            0o644,
            &PutSrc::File(&d.0.join("srclink"))
        )
        .is_err()
    );
    assert!(put(Path::new("relative/x"), 0o600, &PutSrc::Bytes(b"")).is_err());
    for prefix in ["a", "not-created"] {
        let traversal = d.0.join(prefix).join("../outside");
        assert_eq!(
            put(&traversal, 0o600, &PutSrc::Bytes(b"no"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
    assert!(!d.0.join("not-created").exists());
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep");
}

/// A folder made through no link: each missing part made, an existing one kept. NEGATIVE
/// CONTROL: a link on the way refuses it, and nothing is made where the link points.
#[test]
fn scratch_dirs_makes_each_part_and_follows_no_link() {
    let d = dir("dirs");
    let made = d.0.join("a/b/c");
    scratch_dirs(&made).unwrap();
    assert!(std::fs::symlink_metadata(&made).unwrap().is_dir());
    scratch_dirs(&made).unwrap();
    let elsewhere = d.0.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, d.0.join("via")).unwrap();
    assert!(std::fs::create_dir_all(d.0.join("via/probe")).is_ok());
    assert!(elsewhere.join("probe").is_dir(), "a plain mkdir follows it");
    std::fs::remove_dir(elsewhere.join("probe")).unwrap();
    assert!(scratch_dirs(&d.0.join("via/x")).is_err());
    assert!(std::fs::read_dir(&elsewhere).unwrap().next().is_none());
    std::os::unix::fs::symlink(&elsewhere, d.0.join("a/b/last")).unwrap();
    assert!(scratch_dirs(&d.0.join("a/b/last")).is_err());
    assert!(scratch_dirs(&d.0.join("a/../a/b")).is_err());
    assert!(scratch_dirs(Path::new("relative/x")).is_err());
}

use std::path::Path;

#[test]
fn scratch_read_takes_only_the_users_regular_file_with_one_name() {
    let d = dir("read");
    let f = d.0.join("f");
    std::fs::write(&f, "text").unwrap();
    assert_eq!(scratch_read(&f).unwrap().as_deref(), Some(&b"text"[..]));
    assert_eq!(scratch_read(&d.0.join("missing")).unwrap(), None);
    std::os::unix::fs::symlink(&f, d.0.join("l")).unwrap();
    assert!(scratch_read(&d.0.join("l")).is_err(), "a link at the name");
    std::os::unix::fs::symlink(&d.0, d.0.join("dl")).unwrap();
    assert!(
        scratch_read(&d.0.join("dl/f")).is_err(),
        "a link on the way"
    );
    std::fs::hard_link(&f, d.0.join("h")).unwrap();
    assert!(scratch_read(&f).is_err(), "a second name");
    assert!(scratch_read(&d.0).is_err(), "a directory");
    let fifo = d.0.join("fifo");
    let name = cpath(&fifo).unwrap();
    // SAFETY: a NUL-terminated path in this test's own private scratch.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(
        scratch_read(&fifo).is_err(),
        "a FIFO is refused without waiting for a writer"
    );
}

#[test]
fn scratch_dir_lists_files_and_dirs_through_no_link() {
    let d = dir("list");
    std::fs::write(d.0.join("f"), "text").unwrap();
    std::fs::create_dir(d.0.join("sub")).unwrap();
    std::os::unix::fs::symlink(d.0.join("f"), d.0.join("l")).unwrap();
    let mut got = scratch_dir(&d.0).unwrap().unwrap();
    got.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        got,
        [
            ("f".into(), EntryKind::File),
            ("sub".into(), EntryKind::Dir)
        ],
        "the link is never listed"
    );
    assert_eq!(scratch_dir(&d.0.join("missing")).unwrap(), None);
    std::os::unix::fs::symlink(d.0.join("sub"), d.0.join("dl")).unwrap();
    assert!(scratch_dir(&d.0.join("dl")).is_err(), "a link at the name");
    std::os::unix::fs::symlink(&d.0, d.0.join("up")).unwrap();
    assert!(
        scratch_dir(&d.0.join("up/sub")).is_err(),
        "a link on the way"
    );
    assert!(scratch_dir(&d.0.join("f")).is_err(), "a file");
}

#[test]
fn scratch_remove_unlinks_by_name_through_no_link() {
    let d = dir("remove");
    let f = d.0.join("sub/f");
    std::fs::create_dir_all(d.0.join("sub")).unwrap();
    std::fs::write(&f, "text").unwrap();
    scratch_remove(&f).unwrap();
    assert!(std::fs::symlink_metadata(&f).is_err());
    scratch_remove(&f).unwrap();
    scratch_remove(&d.0.join("missing/f")).unwrap();
    let outside = d.0.join("outside");
    std::fs::write(&outside, "keep").unwrap();
    std::os::unix::fs::symlink(&outside, d.0.join("sub/l")).unwrap();
    scratch_remove(&d.0.join("sub/l")).unwrap();
    assert!(std::fs::symlink_metadata(d.0.join("sub/l")).is_err());
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep");
    let elsewhere = d.0.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("f"), "keep").unwrap();
    std::os::unix::fs::symlink(&elsewhere, d.0.join("via")).unwrap();
    assert!(scratch_remove(&d.0.join("via/f")).is_err());
    assert_eq!(
        std::fs::read_to_string(elsewhere.join("f")).unwrap(),
        "keep"
    );
}

#[test]
fn remove_if_same_unlinks_only_the_file_it_was_through_no_link() {
    use std::os::unix::fs::{MetadataExt as _, symlink};
    let d = dir("same");
    std::fs::create_dir_all(d.0.join("sub")).unwrap();
    let f = d.0.join("sub/f");
    std::fs::write(&f, "text").unwrap();
    let m = std::fs::symlink_metadata(&f).unwrap();
    let st = entry_stat(&f).unwrap().unwrap();
    assert_eq!(
        (st.dev, st.ino, st.kind, st.uid),
        (m.dev(), m.ino(), libc::S_IFREG, m.uid())
    );
    assert!(st.born > 0);
    assert_eq!(
        entry_stat(&d.0.join("sub")).unwrap().map(|s| s.kind),
        Some(libc::S_IFDIR)
    );
    assert!(
        !remove_if_same(&f, st.dev, st.ino + 1).unwrap(),
        "another inode"
    );
    assert!(
        !remove_if_same(&f, st.dev.wrapping_add(1), st.ino).unwrap(),
        "another device"
    );
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "text");
    assert!(remove_if_same(&f, st.dev, st.ino).unwrap());
    assert!(std::fs::symlink_metadata(&f).is_err());
    assert_eq!(entry_stat(&f).unwrap(), None);
    assert!(!remove_if_same(&f, st.dev, st.ino).unwrap(), "already gone");
    let target = d.0.join("target");
    std::fs::write(&target, "keep").unwrap();
    let l = d.0.join("sub/l");
    symlink(&target, &l).unwrap();
    let ls = entry_stat(&l).unwrap().unwrap();
    let ts = entry_stat(&target).unwrap().unwrap();
    assert_eq!(ls.kind, libc::S_IFLNK);
    assert!(
        !remove_if_same(&l, ts.dev, ts.ino).unwrap(),
        "the target's identity is not the link's"
    );
    assert!(remove_if_same(&l, ls.dev, ls.ino).unwrap());
    assert!(std::fs::symlink_metadata(&l).is_err());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
    let elsewhere = d.0.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("f"), "keep").unwrap();
    let es = entry_stat(&elsewhere.join("f")).unwrap().unwrap();
    symlink(&elsewhere, d.0.join("via")).unwrap();
    assert!(entry_stat(&d.0.join("via/f")).is_err());
    assert!(remove_if_same(&d.0.join("via/f"), es.dev, es.ino).is_err());
    assert_eq!(
        std::fs::read_to_string(elsewhere.join("f")).unwrap(),
        "keep"
    );
    assert_eq!(entry_stat(&d.0.join("missing/f")).unwrap(), None);
    assert!(!remove_if_same(&d.0.join("missing/f"), es.dev, es.ino).unwrap());
    assert_eq!(entry_stat(&d.0.join("sub/none")).unwrap(), None);
}

#[test]
fn evicted_pages_leave_the_page_cache_and_the_file_unchanged() {
    let d = dir("pages");
    let f = d.0.join("sub/f");
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    let bytes: Vec<u8> = (0..4 << 20).map(|i: u32| (i % 251) as u8).collect();
    std::fs::write(&f, &bytes).unwrap();
    aterm_durable::ordered(&std::fs::File::open(&f).unwrap()).unwrap();
    assert_eq!(std::fs::read(&f).unwrap(), bytes);
    let (resident, pages) = resident_pages(&d.0).unwrap();
    assert!(
        pages > 0 && resident == pages,
        "{resident}/{pages} after a read"
    );
    evict_pages(&d.0).unwrap();
    assert_eq!(resident_pages(&d.0).unwrap(), (0, pages));
    assert_eq!(std::fs::read(&f).unwrap(), bytes);
}

#[test]
fn the_user_temp_dir_is_named_and_this_process_is_in_no_jail() {
    assert!(user_temp_dir().is_some_and(|t| t.starts_with('/')));
    let none = jail_pids("lab.jail.test.0000000000000000").unwrap();
    assert!(none.is_empty(), "{none:?}");
    for bad_pid in [-1, 0, 1] {
        assert_eq!(
            signal_probe(bad_pid, 0).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
    // Both fail at the first `sigaction` read (EINVAL on macOS, SIGKILL included), before any pipe.
    for bad in [-1, libc::SIGKILL] {
        assert_eq!(
            SignalPipe::install(&[bad]).err().map(|e| e.kind()),
            Some(io::ErrorKind::InvalidInput)
        );
    }
}

#[test]
fn a_listing_that_fails_is_an_error_and_never_an_empty_jail() {
    assert_eq!(
        jail_members(Err(io::Error::other("refused")), 7, |_| true),
        Err("jailpids: the process list could not be read: refused".to_string())
    );
    assert_eq!(
        jail_members(Ok(vec![40, 9, 1, 7, 12, 9]), 7, |pid| pid != 40),
        Ok(vec![9, 12]),
        "pid 1, this process and what the jail does not hold are left out"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn await_exit_waits_through_a_stop_and_a_continue_for_the_exit() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    let mut child = aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", "kill -STOP $$; exit 7"])
        .spawn()
        .expect("sh");
    let pid = child.id();
    let spid = i32::try_from(pid).expect("pid");
    let done = Arc::new(AtomicBool::new(false));
    let detector = {
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            if crate::stop::sleep_unless(Duration::from_mins(1), &done) != Ok(true) {
                let _ = kill(spid, libc::SIGKILL);
            }
        })
    };
    let stopped = waitid_once(pid, libc::WEXITED | libc::WSTOPPED | libc::WNOWAIT)
        .expect("waitid")
        .expect("a report");
    assert_eq!(
        stopped.si_code,
        libc::CLD_STOPPED,
        "the child stopped itself"
    );
    let continued = Arc::new(AtomicBool::new(false));
    let resumer = {
        let continued = Arc::clone(&continued);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            continued.store(true, Ordering::SeqCst);
            kill(spid, libc::SIGCONT).expect("SIGCONT");
        })
    };
    await_exit(pid).expect("await_exit");
    let resumed_first = continued.load(Ordering::SeqCst);
    resumer.join().expect("the resumer");
    let status = child.wait().expect("wait");
    done.store(true, Ordering::SeqCst);
    crate::stop::ring();
    detector.join().expect("the hang detector");
    assert!(
        resumed_first,
        "await_exit returned while the child was still stopped"
    );
    assert_eq!(status.code(), Some(7), "{status}");
}
