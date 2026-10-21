// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The careful writer and aterm.toml's, against a scratch directory.

use super::*;
use std::os::unix::fs::PermissionsExt as _;

/// A scratch directory of this test's own, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "aterm-uds-atomic-{tag}-{}-{}",
            std::process::id(),
            crate::rand::hex_token::<4>().expect("entropy")
        ));
        std::fs::create_dir_all(&dir).expect("scratch");
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

thread_local! {
    /// The racing writer this thread's next window runs ([`racing`]).
    static RACER: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run this thread's racing writer, once: called at each window between a check and the use
/// it guards, where only a race could otherwise act.
pub(super) fn race() {
    if let Some(racer) = RACER.with(|r| r.borrow_mut().take()) {
        racer();
    }
}

/// Have `racer` act at the next window this thread reaches.
fn racing(racer: impl FnOnce() + 'static) {
    RACER.with(|r| *r.borrow_mut() = Some(Box::new(racer)));
}

/// The permission and set-id bits.
fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o7777
}

/// A write lands whole at its mode, in a directory it makes; the `check` runs
/// last before the rename, the temp already holding the new bytes at their
/// mode, and one that refuses leaves the old bytes and no temp; and a link at
/// the path is replaced, never written through.
#[test]
fn a_write_is_whole_or_nothing_and_never_through_a_link() {
    let scratch = Scratch::new("write");
    let path = scratch.0.join("made/key");
    write(&path, b"one\n", 0o600).expect("write");
    assert_eq!(std::fs::read(&path).expect("read"), b"one\n");
    assert_eq!(mode_of(&path), 0o600);

    let refused = write_checked(&path, b"two\n", 0o664, || {
        let temps: Vec<_> = std::fs::read_dir(scratch.0.join("made"))
            .expect("list")
            .map(|entry| entry.expect("entry").path())
            .filter(|temp| *temp != path)
            .map(|temp| (std::fs::read(&temp).expect("the temp"), mode_of(&temp)))
            .collect();
        assert_eq!(
            temps,
            [(b"two\n".to_vec(), 0o664)],
            "checked before the write"
        );
        Err(io::Error::other("changed meanwhile"))
    });
    assert!(refused.is_err());
    assert_eq!(std::fs::read(&path).expect("read"), b"one\n");
    let names: Vec<_> = std::fs::read_dir(scratch.0.join("made"))
        .expect("list")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert_eq!(names, ["key"], "no temp is left behind");

    let elsewhere = scratch.0.join("elsewhere");
    std::fs::write(&elsewhere, "theirs\n").expect("seed");
    let link = scratch.0.join("made/link");
    std::os::unix::fs::symlink(&elsewhere, &link).expect("symlink");
    write(&link, b"mine\n", 0o600).expect("write");
    assert_eq!(
        std::fs::read_to_string(&elsewhere).expect("read"),
        "theirs\n"
    );
    assert!(
        !std::fs::symlink_metadata(&link)
            .expect("metadata")
            .file_type()
            .is_symlink(),
        "the link was replaced"
    );
}

/// aterm.toml is written as the config service writes it: through a link to
/// the file it names, the previous text kept as `.bak`, the file's own
/// permission bits kept (on the descriptor: 0664 is past the umask) and never
/// a set-id bit, and never over bytes that changed, appeared or went after
/// they were read.
#[test]
fn aterm_toml_is_written_through_its_link_and_never_over_a_change() {
    let scratch = Scratch::new("config");
    let dotfiles = scratch.0.join("dotfiles-aterm.toml");
    std::fs::write(&dotfiles, "theme = \"Nord\"\n").expect("seed");
    std::fs::set_permissions(&dotfiles, std::fs::Permissions::from_mode(0o4664)).expect("mode");
    let config = scratch.0.join("cfg/aterm/aterm.toml");
    std::fs::create_dir_all(config.parent().expect("parent")).expect("cfg");
    std::os::unix::fs::symlink(&dotfiles, &config).expect("link");

    let file = ConfigFile::open(&config).expect("open");
    assert_eq!(file.text().expect("text"), Some("theme = \"Nord\"\n"));
    let bak = file
        .lock()
        .and_then(|edit| edit.replace("theme = \"Dracula\"\n"))
        .expect("replace")
        .expect("a .bak");
    assert!(
        std::fs::symlink_metadata(&config)
            .expect("metadata")
            .file_type()
            .is_symlink(),
        "the link stays a link"
    );
    assert_eq!(
        std::fs::read_to_string(&dotfiles).expect("read"),
        "theme = \"Dracula\"\n"
    );
    assert_eq!(
        std::fs::read_to_string(&bak).expect("bak"),
        "theme = \"Nord\"\n"
    );
    assert_eq!(mode_of(&dotfiles), 0o664, "the file keeps its own mode");
    assert_eq!(mode_of(&bak), 0o664, "and so does its .bak");

    let file = ConfigFile::open(&config).expect("open");
    std::fs::write(&dotfiles, "theme = \"Solarized\"\n").expect("the other writer");
    assert!(
        file.lock()
            .and_then(|edit| edit.replace("theme = \"Gruvbox\"\n"))
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(&dotfiles).expect("read"),
        "theme = \"Solarized\"\n",
        "an edit made meanwhile is never reverted"
    );
    assert_eq!(
        std::fs::read_to_string(&bak).expect("bak"),
        "theme = \"Nord\"\n",
        "an edit that never lands replaces no .bak"
    );

    let went = scratch.0.join("went.toml");
    std::fs::write(&went, "a\n").expect("seed");
    let file = ConfigFile::open(&went).expect("open");
    std::fs::remove_file(&went).expect("the other writer removes it");
    assert!(file.lock().and_then(|edit| edit.replace("b\n")).is_err());
    assert!(!went.exists(), "a file removed meanwhile is not made again");
    let appeared = scratch.0.join("appeared.toml");
    let file = ConfigFile::open(&appeared).expect("open");
    std::fs::write(&appeared, "theirs\n").expect("the other writer makes it");
    assert!(file.lock().and_then(|edit| edit.replace("mine\n")).is_err());
    assert_eq!(
        std::fs::read_to_string(&appeared).expect("read"),
        "theirs\n",
        "a file made meanwhile is not replaced"
    );

    let fresh = scratch.0.join("cfg/new/aterm.toml");
    let file = ConfigFile::open(&fresh).expect("open");
    assert_eq!(file.text().expect("text"), None);
    let bak = file
        .lock()
        .and_then(|edit| edit.replace("theme = \"Nord\"\n"))
        .expect("replace");
    assert_eq!(bak, None, "no .bak for a file nobody made");
    assert_eq!(mode_of(&fresh), 0o600, "a new file is private");
}

/// What cannot be read whole is never edited: a link that names no file, a
/// directory, and text that is not UTF-8.
#[test]
fn aterm_toml_that_cannot_be_read_is_never_edited() {
    let scratch = Scratch::new("unreadable");
    let dangling = scratch.0.join("dangling.toml");
    std::os::unix::fs::symlink(scratch.0.join("nobody-made-this"), &dangling).expect("link");
    assert!(ConfigFile::open(&dangling).is_err(), "a link to nothing");
    assert!(ConfigFile::open(&scratch.0).is_err(), "a directory");
    let binary = scratch.0.join("binary.toml");
    std::fs::write(&binary, [0xff, 0xfe]).expect("seed");
    let file = ConfigFile::open(&binary).expect("open");
    assert_eq!(
        file.text().err().map(|e| e.kind()),
        Some(io::ErrorKind::InvalidData)
    );
}

/// A temporary name already taken, by a planted link or by a file, is never
/// opened: the next salt is tried, a clash on every try refuses as
/// `AlreadyExists`, and any other failure of the create is its own.
#[test]
fn a_taken_temporary_name_is_never_opened() {
    let scratch = Scratch::new("temp");
    let path = scratch.0.join("key");
    let salted = |salt: &'static str| create_temp_salted(&path, 0o600, move || Ok(salt.into()));
    let (planted, _) = salted("link").expect("a free name");
    std::fs::remove_file(&planted).expect("free it");
    let elsewhere = scratch.0.join("elsewhere");
    std::os::unix::fs::symlink(&elsewhere, &planted).expect("plant a link");
    let (theirs, _) = salted("theirs").expect("a free name");
    std::fs::write(&theirs, "theirs\n").expect("seed");

    let mut salts = ["link", "theirs", "free"].into_iter();
    let (mine, _) = create_temp_salted(&path, 0o600, || {
        Ok(salts.next().expect("three tries").into())
    })
    .expect("the third name");
    assert!(mine != planted && mine != theirs, "{mine:?}");
    assert_eq!(mode_of(&mine), 0o600, "born at its mode, before any byte");
    assert!(
        !elsewhere.exists(),
        "the planted link was not written through"
    );
    assert_eq!(std::fs::read_to_string(&theirs).expect("read"), "theirs\n");

    let clash = salted("theirs").expect_err("every try clashes");
    assert_eq!(clash.kind(), io::ErrorKind::AlreadyExists);
    let missing =
        create_temp_beside(&scratch.0.join("missing/key"), 0o600).expect_err("no directory");
    assert_eq!(missing.kind(), io::ErrorKind::NotFound);
}

/// The config write lock: a holder makes an edit wait, and one held past the
/// wait refuses it as busy; a lock file already there is taken and made 0600
/// again (no other account may hold it open), and the lock is let go by
/// `LOCK_UN` before the close, so a child forked while it was held keeps
/// nothing. A link or a directory planted at the lock's name is refused as no
/// regular file, and what a link names is neither opened nor made private.
#[test]
fn the_write_lock_waits_refuses_past_its_wait_and_is_let_go() {
    let scratch = Scratch::new("lock");
    let victim = scratch.0.join("victim");
    std::fs::write(&victim, "theirs\n").expect("seed");
    std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).expect("mode");
    let planted = scratch.0.join("planted.toml");
    let planted_lock = scratch.0.join(".planted.toml.aterm-write.lock");
    for dir in [false, true] {
        if dir {
            std::fs::create_dir(&planted_lock).expect("plant a directory");
        } else {
            std::os::unix::fs::symlink(&victim, &planted_lock).expect("plant a link");
        }
        let refused = ConfigWriteLock::take_within(&planted, Duration::ZERO, || {}).err();
        assert_eq!(
            refused.map(|e| e.kind()),
            Some(io::ErrorKind::InvalidData),
            "directory {dir}"
        );
        assert_eq!(mode_of(&victim), 0o644, "what the link names is untouched");
        if dir {
            std::fs::remove_dir(&planted_lock).expect("unplant");
        } else {
            std::fs::remove_file(&planted_lock).expect("unplant");
        }
    }

    let target = scratch.0.join("aterm.toml");
    let lock = scratch.0.join(".aterm.toml.aterm-write.lock");
    std::fs::write(&lock, "").expect("seed");
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).expect("mode");
    let holder = std::fs::File::open(&lock).expect("open");
    #[expect(
        clippy::disallowed_methods,
        reason = "the test's stand-in holder lets go by unlock before its close"
    )]
    holder.try_lock().expect("LOCK_EX|LOCK_NB");

    let busy = ConfigWriteLock::take_within(&target, Duration::ZERO, || {})
        .err()
        .expect("held past the wait");
    assert_eq!(busy.kind(), io::ErrorKind::WouldBlock);
    let mut holder = Some(holder);
    let taken = ConfigWriteLock::take_within(&target, Duration::from_secs(60), || {
        if let Some(holder) = holder.take() {
            holder.unlock().expect("LOCK_UN");
        }
    })
    .expect("taken once the holder let go");
    assert!(holder.is_none(), "the edit met the holder and waited");
    assert_eq!(mode_of(&lock), 0o600);

    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    let parked = crate::ownerlock::tests::Parked::park();
    drop(taken);
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    {
        use crate::ownerlock::{Owner, probe};
        assert_eq!(probe(&lock), Owner::Dead, "the forked child kept the lock");
        let status = parked.release();
        assert!(status.success(), "{status}");
    }
}

/// A link or a FIFO a racing writer swaps in at the lock's name after the check that the name
/// is a regular file, before the open, is refused there too: the link by the no-follow open,
/// what it names neither opened nor made private, and the FIFO by the descriptor's own type,
/// its mode kept.
#[test]
fn a_lock_name_swapped_after_its_check_is_still_refused() {
    let scratch = Scratch::new("swap");
    let victim = scratch.0.join("victim");
    std::fs::write(&victim, "theirs\n").expect("seed");
    std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).expect("mode");
    let target = scratch.0.join("aterm.toml");
    let lock = scratch.0.join(".aterm.toml.aterm-write.lock");

    let (link_to, link) = (victim.clone(), lock.clone());
    racing(move || std::os::unix::fs::symlink(&link_to, &link).expect("swap a link in"));
    let refused = ConfigWriteLock::take_within(&target, Duration::ZERO, || {});
    assert!(refused.is_err(), "the link was followed");
    assert_eq!(mode_of(&victim), 0o644, "what the link names is untouched");
    std::fs::remove_file(&lock).expect("unplant");

    let fifo = lock.clone();
    racing(move || {
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).expect("path");
        // SAFETY: a NUL-terminated path, live for the call.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "mkfifo");
        std::fs::set_permissions(&fifo, std::fs::Permissions::from_mode(0o644)).expect("mode");
    });
    let refused = ConfigWriteLock::take_within(&target, Duration::ZERO, || {}).err();
    assert_eq!(refused.map(|e| e.kind()), Some(io::ErrorKind::InvalidData));
    assert_eq!(mode_of(&lock), 0o644, "the FIFO is not made private");
}

/// A writer that ignores the lock (an editor) and saves after the edit's first check, while
/// its `.bak` and text are written, still makes the edit refuse: its save is never reverted.
#[test]
fn a_save_after_the_first_check_still_refuses_the_edit() {
    let scratch = Scratch::new("late");
    let config = scratch.0.join("aterm.toml");
    std::fs::write(&config, "theme = \"Nord\"\n").expect("seed");
    let file = ConfigFile::open(&config).expect("open");
    let edit = file.lock().expect("lock");
    let editor = config.clone();
    racing(move || std::fs::write(&editor, "theme = \"Solarized\"\n").expect("the editor"));
    assert!(edit.replace("theme = \"Dracula\"\n").is_err());
    assert_eq!(
        std::fs::read_to_string(&config).expect("read"),
        "theme = \"Solarized\"\n"
    );
}

/// A lock refused for any reason but a holder is that refusal, at once: never waited out and
/// reported as another writer's save. Staged by a Seatbelt profile that denies `flock`
/// (`EPERM`), with this test binary run again under `sandbox-exec`.
#[cfg(target_vendor = "apple")]
#[test]
fn a_lock_refused_for_another_reason_is_not_waited_on() {
    const NAME: &str = "atomic_file::tests::a_lock_refused_for_another_reason_is_not_waited_on";
    const STAGE_ENV: &str = "ATERM_UDS_TEST_LOCK_DENIED";
    if let Some(dir) = std::env::var_os(STAGE_ENV) {
        let target = Path::new(&dir).join("aterm.toml");
        let refused = ConfigWriteLock::take_within(&target, Duration::from_secs(60), || {
            panic!("waited on as if another writer held it")
        })
        .err();
        assert_eq!(refused.and_then(|e| e.raw_os_error()), Some(libc::EPERM));
        println!("STAGED denied ok");
        return;
    }
    let scratch = Scratch::new("denied");
    let out = crate::spawnfd::command("/usr/bin/sandbox-exec")
        .args(["-p", "(version 1)(allow default)(deny file-lock)"])
        .arg(std::env::current_exe().expect("the test binary"))
        .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
        .env(STAGE_ENV, &scratch.0)
        .output()
        .expect("run this test binary under sandbox-exec");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("STAGED denied ok"),
        "{:?}\nstdout: {stdout}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}
