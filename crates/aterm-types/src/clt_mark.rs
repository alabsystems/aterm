// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The account's durable record of an administrator-password attempt. A valid record holds
//! every automatic asker, across launches and software updates. Explicit person retries remain
//! available. `opened <build>` retains the exact claim owner for a proven preprompt release;
//! historical multi-line and bare `opened` records also retain the hold.
//!
//! Window and terminal callers share one record and one lock. A claim succeeds only when the
//! record is absent, so concurrent askers from different builds cannot each open a prompt.
//! Cancellation, refused passwords, failed installs and unknown child outcomes keep the record.
//! Only the exact owner whose child provably never reached the prompt may release its line.
//! Unreadable or malformed records hold automatic asks and are never rewritten.

use std::io;
use std::path::{Path, PathBuf};

/// The record's file name, in this account's aterm state ([`crate::dirs::account_state_dir`]).
pub const FILE: &str = "command-line-tools-tab";

/// The maximum historical record length; new claims write one owner line.
pub const LINES: usize = 8;

/// The longest build id a line names; aterm's are far shorter (a version, `+g` and a commit).
pub const BUILD_BYTES: usize = 96;

/// The longest record read or written: [`LINES`] `opened <build>` lines.
pub const BYTES: usize = LINES * ("opened ".len() + BUILD_BYTES + 1);

/// Hang detector while a change of the record waits for another asker's durable write
/// ([`Live`]'s lock). Scheduling delay alone must not spend the automatic ask.
#[cfg(not(target_family = "wasm"))]
pub const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

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

/// What the account's record says about automatic password requests.
#[derive(Debug)]
pub enum Asked {
    /// A previous attempt consumed the automatic ask, including on another build.
    Spent,
    /// No record exists.
    Available,
    /// The record cannot be read safely; automatic requests remain held.
    Unread(io::Error),
}

/// Read the durable episode, independently of the running build.
#[must_use]
pub fn reading(files: &dyn Files, path: &Path) -> Asked {
    match files.read(path, BYTES) {
        Ok(None) => Asked::Available,
        Ok(Some(text)) => match builds(&text) {
            Some(_) => Asked::Spent,
            None => Asked::Unread(malformed()),
        },
        Err(error) => Asked::Unread(error),
    }
}

/// Whether a previous attempt, or an unreadable record, holds automatic requests.
#[must_use]
pub fn spent(files: &dyn Files, path: &Path) -> bool {
    !matches!(reading(files, path), Asked::Available)
}

/// Claim the absent automatic-ask episode under the shared lock. The durable owner is `build`.
/// Returns false without rewriting any existing valid record, regardless of its owner.
///
/// # Errors
/// An unsafe or malformed record, an invalid owner, or a lock/write error.
pub fn claim(files: &dyn Files, path: &Path, build: &str) -> io::Result<bool> {
    change(files, path, build, Change::Claim)
}

/// Record an explicit retry before it can ask. An absent episode receives an exact owner and
/// returns true. An existing episode becomes an anonymous sticky hold and returns false: no
/// older in-flight asker's preprompt release may erase this person's attempt.
///
/// # Errors
/// As [`claim`]. Explicit callers keep their person-granted authority on recording failure.
pub fn explicit(files: &dyn Files, path: &Path, build: &str) -> io::Result<bool> {
    change(files, path, build, Change::Explicit)
}

/// Release exactly `build`'s line after proving its attempt never reached a password prompt.
/// Other historical owners and anonymous holds are retained.
///
/// # Errors
/// An unsafe or malformed record, an invalid owner, or a lock/write/removal error.
pub fn forget(files: &dyn Files, path: &Path, build: &str) -> io::Result<()> {
    change(files, path, build, Change::Forget).map(|_| ())
}

/// A change of the record ([`change`]).
#[derive(Clone, Copy)]
enum Change {
    /// Add the build's line where there is none ([`claim`]).
    Claim,
    /// Revoke older release capabilities before an explicit retry.
    Explicit,
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
    let named = builds.contains(&Some(build));
    match how {
        Change::Claim if !builds.is_empty() => return Ok(false),
        Change::Claim => builds.push(Some(build)),
        Change::Explicit if !builds.is_empty() => {
            if !builds.contains(&None) {
                // A sticky hold supersedes historical owner lines without growing the bounded
                // record or granting another release capability.
                files.write(path, "opened\n")?;
            }
            return Ok(false);
        }
        Change::Explicit => builds.push(Some(build)),
        Change::Forget if !named => return Ok(false),
        Change::Forget => builds.retain(|&kept| kept != Some(build)),
    }
    if builds.is_empty() {
        files.remove(path)?;
        return Ok(true);
    }
    let mut lines = String::new();
    for named in builds {
        match named {
            Some(named) => {
                let _ = writeln!(lines, "opened {named}");
            }
            None => lines.push_str("opened\n"),
        }
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
fn builds(text: &str) -> Option<Vec<Option<&str>>> {
    let mut lines = 0;
    let mut builds = Vec::new();
    for line in text.lines() {
        lines += 1;
        match line.split_once(' ') {
            None if line == "opened" => builds.push(None),
            Some(("opened", build)) if nameable(build) => builds.push(Some(build)),
            _ => return None,
        }
    }
    (1..=LINES).contains(&lines).then_some(builds)
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
fn lock_started(path: &Path, started: std::time::Instant) -> io::Result<Held> {
    let deadline = started + LOCK_WAIT;
    if std::time::Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Command Line Tools record lock did not finish",
        ));
    }
    let lock = lock_path(path)?;
    std::fs::create_dir_all(folder(path))?;
    aterm_uds::file_lock::LockFile::take(
        &lock,
        deadline,
        "another aterm is changing the record of its Command Line Tools ask",
    )
    .map(Held::new)
}

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
            options
                .mode(0o600)
                .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
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
        lock_started(path, std::time::Instant::now())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_scheduled_record_writer_does_not_spend_the_automatic_ask() {
        let dir = crate::test_scratch_dir("clt-mark");
        let path = dir.path().join(FILE);
        let started = std::time::Instant::now() - std::time::Duration::from_secs(3);
        drop(lock_started(&path, started).unwrap());
        assert!(claim(&Live, &path, "owner").unwrap());
        let held = Live.lock(&path).unwrap();
        assert!(matches!(
            lock_started(&path, std::time::Instant::now() - std::time::Duration::from_secs(61)),
            Err(error) if error.kind() == io::ErrorKind::TimedOut
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "opened owner\n");
        drop(held);
        assert!(!claim(&Live, &path, "later").unwrap());
        forget(&Live, &path, "later").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "opened owner\n");
        forget(&Live, &path, "owner").unwrap();
        assert!(claim(&Live, &path, "later").unwrap());
    }

    /// A consumed episode survives updates and historical anonymous/multi-owner records.
    /// Only its exact owner's proven preprompt release can remove a line; unreadable records
    /// remain held and unchanged.
    #[test]
    fn a_password_episode_survives_updates_and_only_its_owner_can_release_it() {
        let dir = crate::test_scratch_dir("clt-mark");
        let path = dir.path().join(FILE);
        let text = || std::fs::read_to_string(&path).unwrap();
        assert!(!spent(&Live, &path));
        assert!(claim(&Live, &path, "release").unwrap());
        for build in ["release", "dev", "update"] {
            assert!(spent(&Live, &path));
            assert!(!claim(&Live, &path, build).unwrap());
        }
        forget(&Live, &path, "update").unwrap();
        assert_eq!(text(), "opened release\n");
        forget(&Live, &path, "release").unwrap();
        assert!(!spent(&Live, &path));
        assert!(!path.exists());

        for historical in [
            "opened\n",
            "opened release\nopened dev\n",
            "opened\nopened release\n",
        ] {
            std::fs::write(&path, historical).unwrap();
            assert!(spent(&Live, &path));
            assert!(!claim(&Live, &path, "update").unwrap());
            assert_eq!(text(), historical);
        }
        forget(&Live, &path, "release").unwrap();
        assert_eq!(
            text(),
            "opened\n",
            "anonymous historical hold survives exact release"
        );

        let too_many = "opened again\n".repeat(LINES + 1);
        for malformed in [
            "",
            "\n",
            "opened two words\n",
            "closed\n",
            too_many.as_str(),
        ] {
            std::fs::write(&path, malformed).unwrap();
            assert!(spent(&Live, &path), "{malformed:?} holds");
            assert!(claim(&Live, &path, "release").is_err());
            assert!(forget(&Live, &path, "release").is_err());
            assert_eq!(text(), malformed);
        }
        assert!(spent(&Live, dir.path()));
        let target = dir.path().join("elsewhere");
        std::fs::write(&target, "opened dev\n").unwrap();
        let linked = dir.path().join("linked");
        std::os::unix::fs::symlink(&target, &linked).unwrap();
        assert!(spent(&Live, &linked));
        assert!(claim(&Live, &linked, "release").is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "opened dev\n");
        assert!(
            std::fs::symlink_metadata(&linked)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            claim(
                &Live,
                &dir.path().join("none"),
                &"x".repeat(BUILD_BYTES + 1)
            )
            .is_err()
        );
    }

    /// A staged copy an earlier process of this pid left as it died mid-write (killed during the
    /// flush, before its rename) holds no later change, a claim's or a forget's, and a link left
    /// at that name goes without its target being touched.
    /// NEGATIVE CONTROL: a writer that opens the staged name without clearing it first fails every
    /// such change with `AlreadyExists`, the line it should add or take back left as it was.
    #[test]
    fn a_staged_copy_a_dead_process_of_this_pid_left_holds_no_change() {
        let dir = crate::test_scratch_dir("clt-mark");
        let path = dir.path().join(FILE);
        let staged = dir
            .path()
            .join(format!("{FILE}.tmp-{}", std::process::id()));
        let text = || std::fs::read_to_string(&path).unwrap();
        std::fs::write(&staged, "opened hal").unwrap();
        assert!(claim(&Live, &path, "release").unwrap());
        assert_eq!(text(), "opened release\n");
        assert!(!staged.exists(), "the staged copy went with the rename");

        std::fs::write(&path, "opened release\nopened dev\n").unwrap();
        std::fs::write(&staged, "opened hal").unwrap();
        forget(&Live, &path, "release").unwrap();
        assert_eq!(text(), "opened dev\n", "the forget took its line back");

        let target = dir.path().join("elsewhere");
        std::fs::write(&target, "kept\n").unwrap();
        std::os::unix::fs::symlink(&target, &staged).unwrap();
        forget(&Live, &path, "dev").unwrap();
        assert!(claim(&Live, &path, "release").unwrap());
        assert_eq!(text(), "opened release\n");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "kept\n");
        assert!(std::fs::symlink_metadata(&staged).is_err());
    }

    /// Valid historical records and unreadable records both hold, with distinct diagnostics.
    #[test]
    fn the_reading_distinguishes_spent_available_and_unread() {
        let dir = crate::test_scratch_dir("clt-mark");
        let path = dir.path().join(FILE);
        assert!(matches!(reading(&Live, &path), Asked::Available));
        for record in ["opened\n", "opened dev\n"] {
            std::fs::write(&path, record).unwrap();
            assert!(matches!(reading(&Live, &path), Asked::Spent));
        }
        std::fs::write(&path, "closed\n").unwrap();
        assert!(
            matches!(reading(&Live, &path), Asked::Unread(error) if error.kind() == io::ErrorKind::InvalidData)
        );
        let linked = dir.path().join("linked");
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), &linked).unwrap();
        for unread in [linked.as_path(), dir.path()] {
            assert!(matches!(reading(&Live, unread), Asked::Unread(_)));
        }
    }

    /// A claim reads the record under its lock: a claim made while another asker holds the lock
    /// waits for it, then finds the line that asker added and adds none (`false`), the record left
    /// as that asker wrote it; and a claim of a build the record names already rewrites nothing,
    /// so its order stays.
    /// NEGATIVE CONTROL: with no other asker, the same claim adds the line (`true`).
    #[test]
    fn of_two_builds_claiming_one_episode_only_one_asks() {
        let dir = crate::test_scratch_dir("clt-mark");
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
            || claim(&Live, &path, "update").unwrap(),
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
        let dir = crate::test_scratch_dir("clt-mark");
        let path = dir.path().join("state").join("aterm").join(FILE);
        assert!(claim(&Live, &path, "release").unwrap());
        assert!(spent(&Live, &path));
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
        let dir = crate::test_scratch_dir("clt-mark");
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
