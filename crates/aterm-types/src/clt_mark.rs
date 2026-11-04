// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The record of aterm's own ask for Apple's Command Line Tools: a small file in this account's
//! aterm state ([`path`], [`FILE`]), so each account on the Mac keeps its own, with one
//! `opened <build>` line per build of aterm ([`crate::version::build_id`]) that asked by itself for
//! the Mac password to install them, the newest [`BUILDS`] kept. Its place reads no variable a
//! shell may export and an app opened from the Dock lacks (`XDG_CONFIG_HOME`), so the window and a
//! terminal's `aterm init` always find one file.
//!
//! Two askers share it, reading and writing it through this one code: System State's automatic
//! Mac Setup tab, which claims its build's ask as the tab lands, and `aterm init` in a run
//! aterm decided (the follow-up of a person's own `aterm pkg install` or `update`), which claims it
//! right before `sudo` asks. A claim ([`claim`]) adds the build's line only where the record does
//! not name it yet, under a lock beside the record ([`Files::lock`]) that every change of it
//! takes, and says whether this caller added it: of two askers that claim one build at once, one
//! adds the line and asks, and the other learns it lost and asks nothing. A build it names asks by
//! itself no more, by either asker: a person who declined (closed the tab, pressed Ctrl-C, closed
//! Apple's installer window without them) is not asked again by that build, nor by another build
//! that shares this account's state and asked already (a release beside a dev build). A build it
//! does not name, an update's, asks once more, so updating aterm retries unfinished setup once. A
//! bare `opened`, the record an aterm that kept no builds wrote, names none. An ask that installed
//! nothing and that no person declined (a failed install, Low Data Mode holding the download back,
//! a tab whose program never started) takes its build's line back ([`forget`]), so the next launch
//! or run asks again; only the caller whose claim added the line takes it back. A person's press of
//! Install Command Line Tools… and an `aterm init` they type ask every time, whatever it says.
//!
//! A record that cannot be read, one too long or malformed to be one, and a build no line could
//! name each count as an ask on every build ([`Asked::Unread`]), and such a record is never
//! rewritten: aterm asks no one twice for want of a reading. The file's I/O is a [`Files`]: this
//! Mac's ([`Live`]) or a test double's.

use std::io;
use std::path::{Path, PathBuf};

/// The record's file name, in this account's aterm state ([`crate::dirs::account_state_dir`]).
pub const FILE: &str = "command-line-tools-tab";

/// The builds the record names: past them the oldest build's line goes, so an account that runs
/// more builds than this (a dev build per commit) may see an old one ask once more.
pub const BUILDS: usize = 8;

/// The longest build id a line names; aterm's are far shorter (a version, `+g` and a commit).
pub const BUILD_BYTES: usize = 96;

/// The longest record read or written: [`BUILDS`] `opened <build>` lines.
pub const BYTES: usize = BUILDS * ("opened ".len() + BUILD_BYTES + 1);

/// How long a change of the record waits for another asker's change to end ([`Live`]'s lock): a
/// change is a short read and one durable write, so a holder past this is stuck, and the change
/// fails rather than hold the window's loop.
#[cfg(all(not(target_family = "wasm"), not(test)))]
pub const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// [`LOCK_WAIT`] under this crate's tests, whose other asker always lets go once its durable write
/// is done: the one-minute hang detector, so a write a loaded disk slows is not read as a stuck
/// holder.
#[cfg(all(not(target_family = "wasm"), test))]
pub const LOCK_WAIT: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;

/// This account's record: [`FILE`] in its aterm state root ([`crate::dirs::account_state_dir`]:
/// `~/Library/Application Support/aterm` on macOS, which `ATERM_STATE_HOME` isolates for a lab
/// instance). It reads no `XDG_CONFIG_HOME`, which a shell may export while the window, opened by
/// launchd, has none, so the window and the follow-up `aterm init` of a person's own package
/// install or update, each in its own environment, claim in one file under one lock. `None` where
/// the state root has no place (no home).
#[must_use]
pub fn path() -> Option<PathBuf> {
    crate::dirs::account_state_dir().map(|dir| dir.join(FILE))
}

/// An exclusive hold on the lock beside a record ([`Files::lock`]), let go when it drops.
pub struct Held {
    _guard: Box<dyn Send>,
}

impl Held {
    /// A hold that `guard` keeps until it drops.
    #[must_use]
    pub fn new(guard: impl Send + 'static) -> Self {
        Self {
            _guard: Box::new(guard),
        }
    }
}

/// The record's file I/O: this Mac's ([`Live`]), or a test double's own files.
pub trait Files {
    /// The text of the regular file at `path`, at most `max` bytes, read without following a link
    /// or waiting on a pipe; `None` where there is none.
    ///
    /// # Errors
    /// Anything else at `path` (a link, a folder, a FIFO), a file longer than `max` bytes or not
    /// UTF-8, and the read's own error.
    fn read(&self, path: &Path, max: usize) -> io::Result<Option<String>>;

    /// Replace the file at `path` whole with `text`, durably, its directory entry included, making
    /// its folder where there is none.
    ///
    /// # Errors
    /// A path held by anything but a regular file, which is left as it is, and the write's own
    /// error, which leaves the file as it was.
    fn write(&self, path: &Path, text: &str) -> io::Result<()>;

    /// Remove the file at `path`; none there is no error.
    ///
    /// # Errors
    /// The removal's own.
    fn remove(&self, path: &Path) -> io::Result<()>;

    /// Hold the lock that every change of the record at `path` is made under ([`claim`],
    /// [`forget`]), waiting while another asker, in this process or another, holds it, until the
    /// hold drops. Its folder is made where there is none.
    ///
    /// # Errors
    /// No hold within the wait, and the lock's own error.
    fn lock(&self, path: &Path) -> io::Result<Held>;
}

/// What the record says of one build ([`reading`]).
#[derive(Debug)]
pub enum Asked {
    /// It names the build: that build asked by itself already.
    Named,
    /// It does not: no record, a bare `opened`, or one naming only other builds.
    NotNamed,
    /// It cannot say: the record cannot be read, is too long or malformed to be one, or no line
    /// could name the build, as the error says. Each holds the ask, as a record naming the build
    /// does.
    Unread(io::Error),
}

/// What the record at `path` says of `build`, as `files` read it: [`Asked::NotNamed`] for no
/// record, a bare `opened` and one naming only other builds, so a newer build asks once while
/// repeated launches and runs of one build do not ask again.
#[must_use]
pub fn reading(files: &dyn Files, path: &Path, build: &str) -> Asked {
    if !nameable(build) {
        return Asked::Unread(unnameable());
    }
    match files.read(path, BYTES) {
        Ok(None) => Asked::NotNamed,
        Ok(Some(text)) => match builds(&text) {
            Some(builds) if builds.contains(&build) => Asked::Named,
            Some(_) => Asked::NotNamed,
            None => Asked::Unread(malformed()),
        },
        Err(error) => Asked::Unread(error),
    }
}

/// Whether the record at `path` holds `build`'s ask, as `files` read it ([`reading`]): it names
/// the build, or it cannot say.
#[must_use]
pub fn opened_on_build(files: &dyn Files, path: &Path, build: &str) -> bool {
    !matches!(reading(files, path, build), Asked::NotNamed)
}

/// Claim `build`'s ask in the record at `path`: its line is added as the newest, the newest
/// [`BUILDS`] kept, only where the record does not name it yet, all under the record's lock
/// ([`Files::lock`]), so of two askers that claim one build at once exactly one adds it. `true`
/// when this call added the line (and so may ask, and may [`forget`] it later), `false` when the
/// record named the build already, which this call leaves as it was. The write is durable, its
/// directory entry included, and replaces the file whole ([`Files::write`]).
///
/// # Errors
/// A record that cannot be read or is malformed, which is left as found, a `build` no line could
/// name, the lock's own error, and the write's.
pub fn claim(files: &dyn Files, path: &Path, build: &str) -> io::Result<bool> {
    change(files, path, build, Change::Claim)
}

/// Take `build`'s line out of the record at `path`, under its lock, keeping the other builds it
/// names; a record left naming none goes, and one that does not name `build` is left as it is.
/// Only an asker whose [`claim`] added the line takes it back.
///
/// # Errors
/// As [`claim`], and the removal's own error.
pub fn forget(files: &dyn Files, path: &Path, build: &str) -> io::Result<()> {
    change(files, path, build, Change::Forget).map(|_| ())
}

/// A change of the record ([`change`]).
#[derive(Clone, Copy)]
enum Change {
    /// Add the build's line where there is none ([`claim`]).
    Claim,
    /// Take the build's line out ([`forget`]).
    Forget,
}

/// Make `how` to `build`'s line in the record at `path` under the record's lock: `true` when the
/// record changed.
fn change(files: &dyn Files, path: &Path, build: &str, how: Change) -> io::Result<bool> {
    use std::fmt::Write as _;
    if !nameable(build) {
        return Err(unnameable());
    }
    let _held = files.lock(path)?;
    let text = files.read(path, BYTES)?;
    let mut builds = match text.as_deref() {
        None => Vec::new(),
        Some(text) => builds(text).ok_or_else(malformed)?,
    };
    let named = builds.contains(&build);
    match how {
        Change::Claim if named => return Ok(false),
        Change::Claim => builds.push(build),
        Change::Forget if !named => return Ok(false),
        Change::Forget => builds.retain(|&kept| kept != build),
    }
    if builds.is_empty() {
        files.remove(path)?;
        return Ok(true);
    }
    let keep = builds.len().saturating_sub(BUILDS);
    let mut lines = String::new();
    for named in &builds[keep..] {
        let _ = writeln!(lines, "opened {named}");
    }
    files.write(path, &lines)?;
    Ok(true)
}

/// Why a build no line could name has no reading and no change.
fn unnameable() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "no line of the record can name this build",
    )
}

/// Why a malformed record has no reading and is left as found.
fn malformed() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "the record is malformed and is left as found",
    )
}

/// The builds a record's `text` names, oldest first: one `opened <build>` line each, and a bare
/// `opened` naming none. `None` for any other text, an empty one included, and for more lines
/// than a record keeps.
fn builds(text: &str) -> Option<Vec<&str>> {
    let mut lines = 0;
    let mut builds = Vec::new();
    for line in text.lines() {
        lines += 1;
        match line.split_once(' ') {
            None if line == "opened" => {}
            Some(("opened", build)) if nameable(build) => builds.push(build),
            _ => return None,
        }
    }
    (1..=BUILDS).contains(&lines).then_some(builds)
}

/// Whether a line can name `build`: one word, no longer than [`BUILD_BYTES`].
fn nameable(build: &str) -> bool {
    (1..=BUILD_BYTES).contains(&build.len()) && !build.contains(char::is_whitespace)
}

/// The lock file beside the record at `path`: its name and `.lock`.
#[cfg(not(target_family = "wasm"))]
fn lock_path(path: &Path) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the record has no name"))?;
    Ok(path.with_file_name(format!("{name}.lock")))
}

/// The folder the record at `path` sits in (`.` for a bare name).
#[cfg(not(target_family = "wasm"))]
fn folder(path: &Path) -> &Path {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    }
}

/// This Mac's files: a read bounded to a regular file, never through a link and never waiting on
/// a pipe ([`aterm_uds::runs::read_bounded_regular_text`]), a write through a file of this
/// process's own beside the record, flushed durably and renamed over it, then its folder flushed,
/// and a lock that is an exclusive `flock` of a file of its own beside the record (its name and
/// `.lock`, [`aterm_uds::file_lock::LockFile`]: the record itself is replaced by each write, so
/// its own inode holds no lock), waited for up to [`LOCK_WAIT`].
#[cfg(not(target_family = "wasm"))]
#[derive(Debug, Clone, Copy, Default)]
pub struct Live;

#[cfg(not(target_family = "wasm"))]
impl Files for Live {
    fn read(&self, path: &Path, max: usize) -> io::Result<Option<String>> {
        match aterm_uds::runs::read_bounded_regular_text(path, max as u64) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn write(&self, path: &Path, text: &str) -> io::Result<()> {
        use std::io::Write as _;
        let dir = folder(path);
        std::fs::create_dir_all(dir)?;
        if std::fs::symlink_metadata(path).is_ok_and(|meta| !meta.file_type().is_file()) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "the record's path is held by something that is not a file",
            ));
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the record has no name"))?;
        let staged = path.with_file_name(format!("{name}.tmp-{}", std::process::id()));
        // One an earlier process of this pid left as it died mid-write (before its rename) would
        // hold every later change: the name is this live process's own, under the record's lock
        // ([`Files::lock`]), and a link there goes, not where it points.
        let _ = std::fs::remove_file(&staged);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&staged)?;
        let written = file
            .write_all(text.as_bytes())
            .and_then(|()| aterm_durable::durable(&file));
        drop(file);
        let written = written.and_then(|()| std::fs::rename(&staged, path));
        if written.is_err() {
            let _ = std::fs::remove_file(&staged);
        }
        written?;
        // The record is in place; a folder that cannot be flushed here (a network home that takes
        // no full flush) keeps it, as the window's other marks are kept.
        let _ = aterm_durable::durable_dir(dir);
        Ok(())
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        match std::fs::remove_file(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            done => done,
        }
    }

    fn lock(&self, path: &Path) -> io::Result<Held> {
        let lock = lock_path(path)?;
        std::fs::create_dir_all(folder(path))?;
        let deadline = std::time::Instant::now() + LOCK_WAIT;
        aterm_uds::file_lock::LockFile::take(
            &lock,
            deadline,
            "another aterm is changing the record of its Command Line Tools ask",
        )
        .map(Held::new)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// The record names each build that claimed its ask, the newest [`BUILDS`]: a later launch or
    /// run of a build it names asks no more, nor do two builds sharing the account run in turn,
    /// while a build it does not name (an update) asks once more. A bare `opened` (an older
    /// aterm's record) names no build. A record that cannot be read or is malformed, one behind a
    /// link included, and a build no line could name, hold, and such a record is left as found.
    #[test]
    fn each_build_asks_by_itself_at_most_once() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let opened = |build: &str| opened_on_build(&Live, &path, build);
        let text = || std::fs::read_to_string(&path).unwrap();
        assert!(!opened("release"), "no record");
        assert!(claim(&Live, &path, "release").unwrap());
        assert!(opened("release"), "a later launch of that build");
        assert!(!opened("dev"), "another build asks once more");
        assert!(claim(&Live, &path, "dev").unwrap());
        assert!(
            opened("release") && opened("dev"),
            "builds launched in turn"
        );
        assert_eq!(text(), "opened release\nopened dev\n");

        for build in 1..BUILDS {
            assert!(claim(&Live, &path, &format!("update-{build}")).unwrap());
        }
        assert!(!opened("release"), "the oldest gives way");
        assert!(opened("dev") && opened(&format!("update-{}", BUILDS - 1)));
        assert_eq!(text().lines().count(), BUILDS);

        // An ask that installed nothing takes one build's line back and keeps the others'; the
        // file goes with the last one.
        let before = text();
        forget(&Live, &path, "release").unwrap();
        assert_eq!(text(), before, "a build it does not name changes nothing");
        forget(&Live, &path, "dev").unwrap();
        assert!(!opened("dev") && opened("update-1"));
        let only = dir.path().join("only");
        assert!(claim(&Live, &only, "release").unwrap());
        forget(&Live, &only, "release").unwrap();
        assert!(!only.exists());

        std::fs::write(&path, "opened\n").unwrap();
        assert!(
            !opened("release"),
            "an older aterm's bare record names no build"
        );
        assert!(claim(&Live, &path, "release").unwrap());
        assert_eq!(text(), "opened release\n");

        let too_many = "opened again\n".repeat(BUILDS + 1);
        for malformed in [
            "",
            "\n",
            "opened two words\n",
            "closed\n",
            too_many.as_str(),
        ] {
            std::fs::write(&path, malformed).unwrap();
            assert!(opened("release"), "{malformed:?} holds");
            assert!(claim(&Live, &path, "release").is_err(), "{malformed:?}");
            assert!(forget(&Live, &path, "release").is_err(), "{malformed:?}");
            assert_eq!(text(), malformed, "left as found");
        }
        assert!(
            opened_on_build(&Live, dir.path(), "release"),
            "unreadable holds"
        );

        // A record behind a link is never read through it, nor written through it.
        let target = dir.path().join("elsewhere");
        std::fs::write(&target, "opened dev\n").unwrap();
        let linked = dir.path().join("linked");
        std::os::unix::fs::symlink(&target, &linked).unwrap();
        assert!(opened_on_build(&Live, &linked, "release"), "a link holds");
        assert!(claim(&Live, &linked, "release").is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "opened dev\n");
        assert!(
            std::fs::symlink_metadata(&linked)
                .unwrap()
                .file_type()
                .is_symlink(),
            "left as found"
        );

        let unnameable = "x".repeat(BUILD_BYTES + 1);
        assert!(
            opened_on_build(&Live, &dir.path().join("none"), &unnameable),
            "a build no line could name holds"
        );
        assert!(claim(&Live, &dir.path().join("none"), &unnameable).is_err());
    }

    /// A staged copy an earlier process of this pid left as it died mid-write (killed during the
    /// flush, before its rename) holds no later change, a claim's or a forget's, and a link left
    /// at that name goes without its target being touched.
    /// NEGATIVE CONTROL: a writer that opens the staged name without clearing it first fails every
    /// such change with `AlreadyExists`, the line it should add or take back left as it was.
    #[test]
    fn a_staged_copy_a_dead_process_of_this_pid_left_holds_no_change() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let staged = dir
            .path()
            .join(format!("{FILE}.tmp-{}", std::process::id()));
        let text = || std::fs::read_to_string(&path).unwrap();
        std::fs::write(&staged, "opened hal").unwrap();
        assert!(claim(&Live, &path, "release").unwrap());
        assert_eq!(text(), "opened release\n");
        assert!(!staged.exists(), "the staged copy went with the rename");

        assert!(claim(&Live, &path, "dev").unwrap());
        std::fs::write(&staged, "opened hal").unwrap();
        forget(&Live, &path, "release").unwrap();
        assert_eq!(text(), "opened dev\n", "the forget took its line back");

        let target = dir.path().join("elsewhere");
        std::fs::write(&target, "kept\n").unwrap();
        std::os::unix::fs::symlink(&target, &staged).unwrap();
        assert!(claim(&Live, &path, "release").unwrap());
        assert_eq!(text(), "opened dev\nopened release\n");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "kept\n");
        assert!(std::fs::symlink_metadata(&staged).is_err());
    }

    /// The reading tells a record that names the build from one that cannot say: no record, a
    /// bare `opened` and other builds' lines name it not; its own line names it; and an
    /// unreadable record (a folder, a link), a malformed one, and a build no line could name each
    /// cannot say, with why.
    #[test]
    fn the_reading_tells_a_named_build_from_a_record_that_cannot_say() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let read = |path: &Path, build: &str| reading(&Live, path, build);
        assert!(matches!(read(&path, "release"), Asked::NotNamed));
        std::fs::write(&path, "opened\n").unwrap();
        assert!(matches!(read(&path, "release"), Asked::NotNamed));
        std::fs::write(&path, "opened dev\n").unwrap();
        assert!(matches!(read(&path, "release"), Asked::NotNamed));
        assert!(matches!(read(&path, "dev"), Asked::Named));
        std::fs::write(&path, "closed\n").unwrap();
        assert!(matches!(
            read(&path, "dev"),
            Asked::Unread(error) if error.kind() == io::ErrorKind::InvalidData
        ));
        let linked = dir.path().join("linked");
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), &linked).unwrap();
        for unread in [linked.as_path(), dir.path()] {
            assert!(
                matches!(read(unread, "dev"), Asked::Unread(_)),
                "{}",
                unread.display()
            );
        }
        assert!(matches!(
            read(&path, &"x".repeat(BUILD_BYTES + 1)),
            Asked::Unread(error) if error.kind() == io::ErrorKind::InvalidInput
        ));
    }

    /// A claim reads the record under its lock: a claim made while another asker holds the lock
    /// waits for it, then finds the line that asker added and adds none (`false`), the record left
    /// as that asker wrote it; and a claim of a build the record names already rewrites nothing,
    /// so its order stays.
    /// NEGATIVE CONTROL: with no other asker, the same claim adds the line (`true`).
    #[test]
    fn of_two_askers_claiming_one_build_one_adds_it() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let other = std::rc::Rc::new(std::cell::RefCell::new(Some(Live.lock(&path).unwrap())));
        let waited = std::rc::Rc::new(std::cell::Cell::new(false));
        let claimed = aterm_uds::file_lock::with_lock_wait_observer(
            {
                let (other, waited, path) = (other.clone(), waited.clone(), path.clone());
                move |_| {
                    if let Some(held) = other.borrow_mut().take() {
                        // The other asker adds the line while it holds the lock, then lets go.
                        Live.write(&path, "opened release\n").unwrap();
                        drop(held);
                        waited.set(true);
                    }
                }
            },
            || claim(&Live, &path, "release").unwrap(),
        );
        assert!(waited.get(), "the claim waited for the other asker's lock");
        assert!(!claimed, "the other asker added it first");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "opened release\n");

        std::fs::write(&path, "opened release\nopened dev\n").unwrap();
        assert!(!claim(&Live, &path, "release").unwrap());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "opened release\nopened dev\n",
            "a lost claim rewrites nothing"
        );

        let alone = dir.path().join("alone");
        assert!(claim(&Live, &alone, "release").unwrap());
    }

    /// A first record makes the folder aterm's state lives in when there is none yet (a package
    /// install before the window ever launched), and is written whole, readable by this account
    /// alone, beside its lock file.
    #[test]
    fn a_first_record_makes_its_folder() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("state").join("aterm").join(FILE);
        assert!(claim(&Live, &path, "release").unwrap());
        assert!(opened_on_build(&Live, &path, "release"));
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let mut names: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [FILE.to_owned(), format!("{FILE}.lock")],
            "no staged file left beside it"
        );
    }

    /// The window (launchd gives it no `XDG_CONFIG_HOME`) and a terminal's `aterm init` (whose
    /// shell may export one) find one record: its place is this account's aterm state, which no
    /// `XDG_CONFIG_HOME` moves. This test binary runs itself twice over one state root (a lab
    /// instance's `ATERM_STATE_HOME`; each test process has its own root otherwise), with the
    /// variable set and with it unset, and both runs name the same file in that root.
    /// NEGATIVE CONTROL: the config folder the run with the variable set reads is the variable's.
    #[test]
    fn one_record_whether_or_not_a_shell_exports_xdg_config_home() {
        const SAY: &str = "ATERM_TYPES_CLT_MARK_SAY_PATH";
        if std::env::var_os(SAY).is_some() {
            println!(
                "record={}",
                path().expect("a test binary's state root").display()
            );
            println!(
                "config={}",
                crate::dirs::aterm_config_dir()
                    .expect("a test binary's config folder")
                    .display()
            );
            return;
        }
        let dir = aterm_tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let xdg = dir.path().join("xdg");
        let test = format!(
            "{}::one_record_whether_or_not_a_shell_exports_xdg_config_home",
            module_path!().split_once("::").map_or("", |(_, rest)| rest)
        );
        let said = |xdg: Option<&Path>| {
            let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
            child
                .args(["--exact", &test, "--nocapture", "--test-threads=1"])
                .env(SAY, "1")
                .env("ATERM_STATE_HOME", &state)
                .stdin(aterm_uds::spawnfd::Stdio::null());
            match xdg {
                Some(xdg) => child.env("XDG_CONFIG_HOME", xdg),
                None => child.env_remove("XDG_CONFIG_HOME"),
            };
            let out = child.output().expect("re-exec the test binary");
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            assert!(out.status.success(), "the re-exec failed:\n{text}");
            let line = |key: &str| {
                // The harness writes the test's name ahead of its first line.
                text.lines()
                    .find_map(|line| line.split_once(key).map(|(_, value)| value))
                    .map(PathBuf::from)
                    .unwrap_or_else(|| panic!("the re-exec named no {key}:\n{text}"))
            };
            (line("record="), line("config="))
        };
        let (set, config) = said(Some(&xdg));
        let (unset, _) = said(None);
        assert_eq!(set, state.join(FILE), "in the account's aterm state");
        assert_eq!(unset, set, "one file for both environments");
        assert_eq!(
            config,
            xdg.join("aterm"),
            "the config folder is the variable's"
        );
    }
}
