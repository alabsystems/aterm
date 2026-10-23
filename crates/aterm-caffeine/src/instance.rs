// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One mug per user: a running mug holds `LOCK_EX` on [`crate::paths::Paths::lock_file`] for its whole life with its pid and its job's label written inside, so a second `serve` sees it and leaves, and `status`/`quit` find it and its job ([`running_mug`] checks that the pid still names that mug).

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::Path;

/// The variable naming the descriptor a re-exec handed its new image the lock on ([`Instance::handover`], [`Instance::adopt`]).
pub const LOCK_FD_ENV: &str = "ATERM_CAFFEINE_LOCK_FD";

/// The descriptor a re-exec hands the lock on: the new image's stdin, the one slot of the three it inherits that a launchd job does not read (it is `/dev/null` there).
pub const HANDOVER_FD: i32 = 0;

/// The longest lock file read: a pid, a label and their newlines.
const HOLDER_CAP: u64 = 128;

/// A running mug's hold on the lock file; dropping it lets go (`LOCK_UN`).
#[derive(Debug)]
pub struct Instance {
    file: std::fs::File,
    /// The job's label written beside the pid, once [`Instance::set_label`] named it.
    label: Option<String>,
    /// Let go with the pid and the label left in the file ([`Instance::release_named`]).
    leave_named: bool,
}

impl Drop for Instance {
    fn drop(&mut self) {
        // Emptied first while this description still holds it, so a lock nobody holds names
        // no pid: a probe holding it for an instant must not make this exited mug's pid read
        // as a running one. (A mug killed outright leaves its pid; `running_mug` checks it.)
        if !self.leave_named && matches!(take(&self.file), Ok(true)) {
            let _ = self.file.set_len(0);
        }
        let _ = self.file.unlock();
    }
}

/// `flock(LOCK_EX|LOCK_NB)` on `file`'s description: `Ok(true)` when it is ours now (it was free,
/// or this description already held it), `Ok(false)` when another description holds it.
fn take(file: &std::fs::File) -> io::Result<bool> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard's own take: its Drop releases by LOCK_UN"
    )]
    let taken = file.try_lock();
    match taken {
        Ok(()) => Ok(true),
        Err(std::fs::TryLockError::WouldBlock) => Ok(false),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

impl Instance {
    /// Take the lock at `path` without waiting and write this process's pid into it.
    /// # Errors
    /// The file cannot be created or locked, or is not a private, singly linked regular file.
    pub fn try_hold(path: &Path) -> io::Result<Option<Self>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(path)?;
        if !private_lock_file(&file.metadata()?) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{} is not a private, singly linked regular file",
                    path.display()
                ),
            ));
        }
        if !take(&file)? {
            return Ok(None);
        }
        let held = Self {
            file,
            label: None,
            leave_named: false,
        };
        held.write_holder()?;
        Ok(Some(held))
    }

    /// Name the launchd job this mug is (`label`), written beside its pid: `quit` boots out that job, and `status` reports it, whichever build asks.
    /// # Errors
    /// The file cannot be rewritten.
    pub fn set_label(&mut self, label: &str) -> io::Result<()> {
        self.label = Some(label.to_string());
        self.write_holder()
    }

    /// Replace the file's text with this process's pid, and the label once named.
    fn write_holder(&self) -> io::Result<()> {
        use std::io::{Seek as _, Write as _};
        let mut text = format!("{}\n", std::process::id());
        if let Some(label) = &self.label {
            text.push_str(label);
            text.push('\n');
        }
        let mut file = &self.file;
        file.set_len(0)?;
        file.rewind()?;
        file.write_all(text.as_bytes())
    }

    /// Let go of the lock with this mug's pid and label left in the file, as a mug that ends for an update restart does: launchd is about to start its job again, and until the new mug holds the lock, a `quit` from any build still finds that job in it ([`last_holder`]) and boots it out.
    pub fn release_named(mut self) {
        self.leave_named = true;
    }

    /// A second descriptor on this hold's open file description, for a re-exec to place at [`HANDOVER_FD`] in the new image (with [`LOCK_FD_ENV`] naming it), which takes it back with [`Instance::adopt`].
    /// # Errors
    /// The descriptor cannot be duplicated.
    pub fn handover(&self) -> io::Result<std::fs::File> {
        self.file.try_clone()
    }

    /// The hold a re-exec handed this image ([`Instance::handover`]): the descriptor [`LOCK_FD_ENV`] names, when it is open on the very file at `path` and its lock is this description's.
    #[must_use]
    pub fn adopt(path: &Path) -> Option<Self> {
        let named = aterm_log::env::take(LOCK_FD_ENV)?;
        let fd: i32 = named.to_str()?.parse().ok()?;
        adopt_fd(fd, path)
    }
}

/// A lock may be rewritten only through an owned regular inode with no second name.
fn private_lock_file(meta: &std::fs::Metadata) -> bool {
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        meta.uid() == aterm_uds::peer::our_uid() && meta.nlink() == 1
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// [`Instance::adopt`] of descriptor `fd`. On success `fd` itself is retired (closed, or
/// `/dev/null` placed on it when it is a stdio slot) and the hold lives on a copy; otherwise
/// `fd` is left as it was, unless it was open on the lock file (then it is retired too: a
/// stdin on the lock file is never wanted).
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn adopt_fd(fd: i32, path: &Path) -> Option<Instance> {
    use std::os::fd::{AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd};
    use std::os::unix::fs::MetadataExt as _;
    if fd < 0 {
        return None;
    }
    // SAFETY: `F_GETFD` reads one descriptor's flags and touches no memory; a number that is not open answers -1.
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
        return None;
    }
    // SAFETY: `fd` is open (just checked) and stays open for this borrow: nothing closes it before the copy below is made.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    let file = std::fs::File::from(borrowed.try_clone_to_owned().ok()?);
    let opened = file.metadata().ok()?;
    let named = std::fs::symlink_metadata(path).ok()?;
    if !private_lock_file(&opened)
        || !named.is_file()
        || (opened.dev(), opened.ino()) != (named.dev(), named.ino())
    {
        return None;
    }
    if fd <= 2 {
        let null = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/null")
            .ok()?;
        // SAFETY: two open descriptors; `dup2` replaces slot `fd` with `/dev/null` atomically.
        if unsafe { libc::dup2(null.as_raw_fd(), fd) } < 0 {
            return None;
        }
    } else {
        // SAFETY: the caller handed `fd` over (the re-exec placed it for this image alone), and nothing else in this process uses the number.
        drop(unsafe { OwnedFd::from_raw_fd(fd) });
    }
    if !take(&file).ok()? {
        return None;
    }
    let held = Instance {
        file,
        label: None,
        leave_named: false,
    };
    held.write_holder().ok()?;
    Some(held)
}

/// Off Unix there are no descriptors to hand over.
#[cfg(not(unix))]
fn adopt_fd(_fd: i32, _path: &Path) -> Option<Instance> {
    None
}

/// Who holds the lock, as its file says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holder {
    /// The holder's pid.
    pub pid: u32,
    /// Its launchd job's label, once it has named it ([`Instance::set_label`]).
    pub label: Option<String>,
}

impl Holder {
    /// Whether a process running `exe` (canonical) with the arguments `argv` is this holder's mug: it runs `… caffeine serve`, from where this holder's job runs it: its copy under `code_dir` (canonical; `None` when there is none) in the label's directory, or else an executable whose own label ([`crate::launchd::label_for`]) is the label.
    #[must_use]
    pub fn is_run_by(&self, exe: &Path, argv: &[OsString], code_dir: Option<&Path>) -> bool {
        let serve = crate::launchd::SERVE_ARGS;
        let serves = argv.len() > serve.len()
            && argv
                .iter()
                .rev()
                .zip(serve.iter().rev())
                .all(|(have, want)| have.as_os_str() == OsStr::new(want));
        if !serves {
            return false;
        }
        let Some(label) = &self.label else {
            return true;
        };
        match code_dir.and_then(|dir| exe.strip_prefix(dir).ok()) {
            Some(rest) => rest
                .components()
                .next()
                .is_some_and(|first| first.as_os_str() == OsStr::new(label)),
            None => crate::launchd::label_for(exe) == *label,
        }
    }
}

/// Who holds the lock at `path`, if anyone does, as its file says (the pid unchecked: a mug killed outright leaves its pid behind, which a probe holding the lock for an instant then reads as live; [`running_mug`] checks it).
#[must_use]
pub fn running_holder(path: &Path) -> Option<Holder> {
    if aterm_uds::ownerlock::probe(path) != aterm_uds::ownerlock::Owner::Live {
        return None;
    }
    last_holder(path)
}

/// Who the lock file at `path` names, held or not: the running mug, or one that died without letting go (a crash, a signal), whose pid and label stay in the file.
#[must_use]
pub fn last_holder(path: &Path) -> Option<Holder> {
    use std::io::Read as _;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(HOLDER_CAP + 1).read_to_string(&mut text).ok()?;
    if text.len() as u64 > HOLDER_CAP {
        return None;
    }
    parse_holder(&text)
}

/// The pid [`running_holder`] reads.
#[must_use]
pub fn running_pid(path: &Path) -> Option<u32> {
    running_holder(path).map(|holder| holder.pid)
}

/// Wait, up to `within`, until no pid of `pids` holds the lock at `path` ([`running_holder`]): a holder's flock goes with its process, so this is the moment a signalled mug has ended, not the moment it was told to.
/// # Errors
/// The holder's pid, when one of `pids` still holds the lock after `within`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn await_released(path: &Path, pids: &[u32], within: std::time::Duration) -> Result<(), u32> {
    let start = std::time::Instant::now();
    loop {
        let Some(holder) = running_holder(path).filter(|holder| pids.contains(&holder.pid)) else {
            return Ok(());
        };
        if start.elapsed() >= within {
            return Err(holder.pid);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// The mug holding the lock at `lock`: [`running_holder`], when the process its pid names is that mug ([`Holder::is_run_by`], read off the live process), so a stale pid now another process's is never taken for a mug.
#[must_use]
pub fn running_mug(lock: &Path, code_dir: &Path) -> Option<Holder> {
    let holder = running_holder(lock)?;
    let pid = i32::try_from(holder.pid).ok()?;
    let exe = aterm_uds::process::read_process_path(pid)?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let argv = aterm_uds::process::read_process_argv(pid)?;
    let code_dir = code_dir.canonicalize().ok();
    holder
        .is_run_by(&exe, &argv, code_dir.as_deref())
        .then_some(holder)
}

/// Who a lock file's text names: `<pid>`, then the label on a second line once named; the pid one
/// positive decimal number, the label one [`crate::launchd::is_label`] accepts, the last newline
/// optional.
fn parse_holder(text: &str) -> Option<Holder> {
    let text = text.strip_suffix('\n').unwrap_or(text);
    let (digits, label) = match text.split_once('\n') {
        Some((digits, label)) => (digits, Some(label)),
        None => (text, None),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let pid = digits.parse().ok().filter(|pid| *pid > 0)?;
    if label.is_some_and(|label| !crate::launchd::is_label(label)) {
        return None;
    }
    Some(Holder {
        pid,
        label: label.map(str::to_string),
    })
}

/// This boot of the Mac (`kern.bootsessionuuid`, as 128 bits, big-endian); `None` where it cannot be read, or reads as the all-zero id a damaged answer would, which must never pass as a boot (two equal damaged answers would read as one boot, and a mode saved before a restart would come back).
#[must_use]
#[cfg(target_os = "macos")]
pub fn boot_id() -> Option<u128> {
    astream_sys::boot_session().map(u128::from_be_bytes)
}

/// This boot of the Mac: not read off macOS.
#[must_use]
#[cfg(not(target_os = "macos"))]
pub fn boot_id() -> Option<u128> {
    None
}

/// A test that reads or writes a held lock's bytes through a second handle is
/// `cfg(unix)`: there the lock is flock's advisory one, while Windows'
/// `LockFileEx` bars every other handle from the locked bytes (os error 33), and
/// `aterm caffeine` runs only on macOS.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_hold_in_this_process_is_refused_until_the_first_lets_go() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state").join("caffeine.lock");
        let first = Instance::try_hold(&path).expect("hold").expect("free");
        assert!(Instance::try_hold(&path).expect("hold").is_none());
        drop(first);
        let again = Instance::try_hold(&path).expect("hold");
        assert!(again.is_some(), "the lock is free after the drop");
    }

    #[test]
    #[cfg(unix)]
    fn the_running_pid_is_the_holders_while_it_holds() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("caffeine.lock");
        assert_eq!(running_pid(&path), None, "no file");
        let held = Instance::try_hold(&path).expect("hold").expect("free");
        assert_eq!(running_pid(&path), Some(std::process::id()));
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            format!("{}\n", std::process::id())
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        drop(held);
        assert_eq!(running_pid(&path), None, "a lock nobody holds");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "",
            "the drop leaves no pid behind"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_release_is_awaited_only_of_the_pids_named() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("caffeine.lock");
        let me = std::process::id();
        let hang = std::time::Duration::from_secs(60);
        assert_eq!(await_released(&path, &[me], hang), Ok(()), "no file");
        let held = Instance::try_hold(&path).expect("hold").expect("free");
        assert_eq!(
            await_released(&path, &[me], std::time::Duration::ZERO),
            Err(me),
            "still held"
        );
        assert_eq!(
            await_released(&path, &[me.wrapping_add(1)], hang),
            Ok(()),
            "another pid's hold is not awaited"
        );
        let letting_go = std::thread::spawn(move || drop(held));
        assert_eq!(await_released(&path, &[me], hang), Ok(()));
        letting_go.join().expect("join");
    }

    #[test]
    #[cfg(unix)]
    fn a_named_job_is_written_beside_the_pid_and_read_back() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("caffeine.lock");
        let mut held = Instance::try_hold(&path).expect("hold").expect("free");
        assert_eq!(
            running_holder(&path),
            Some(Holder {
                pid: std::process::id(),
                label: None
            })
        );
        let label = "com.aterm.aterm.caffeine.dev.0123abcd";
        held.set_label(label).expect("label");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            format!("{}\n{label}\n", std::process::id())
        );
        assert_eq!(
            running_holder(&path),
            Some(Holder {
                pid: std::process::id(),
                label: Some(label.to_string())
            })
        );
        drop(held);
        assert_eq!(running_holder(&path), None);
        assert_eq!(last_holder(&path), None, "a mug that lets go names nothing");

        let mut held = Instance::try_hold(&path).expect("hold").expect("free");
        held.set_label(label).expect("label");
        held.release_named();
        assert_eq!(running_holder(&path), None, "let go");
        assert_eq!(
            last_holder(&path).and_then(|holder| holder.label),
            Some(label.to_string())
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_held_lock_with_garbage_inside_names_no_pid() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("caffeine.lock");
        let held = Instance::try_hold(&path).expect("hold").expect("free");
        for text in ["", "x\n", "12 34\n", "-5\n", "0\n", "99999999999\n"] {
            std::fs::write(&path, text).expect("write");
            assert_eq!(running_pid(&path), None, "{text:?}");
        }
        std::fs::write(&path, "4242").expect("write");
        assert_eq!(running_pid(&path), Some(4242), "no newline is fine");
        std::fs::write(&path, format!("1\n{}\n", "a".repeat(200))).expect("write");
        assert_eq!(running_pid(&path), None, "too long");
        drop(held);
    }

    #[test]
    #[cfg(unix)]
    fn a_stale_pid_in_a_free_lock_is_not_a_running_mug() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("caffeine.lock");
        std::fs::write(&path, "4242\n").expect("write");
        assert_eq!(running_pid(&path), None);
        std::fs::write(&path, "123456789\n").expect("write");
        let held = Instance::try_hold(&path).expect("hold").expect("free");
        assert_eq!(running_pid(&path), Some(std::process::id()));
        drop(held);
    }

    /// A pid left behind (a mug killed outright), read while something holds the lock (a
    /// probe's instant, a `serve` that has not written its own yet), is not taken for a mug
    /// when the process it now names is not one: here it is this test process, live but no
    /// `caffeine serve`.
    #[test]
    #[cfg(unix)]
    fn a_stale_pid_under_a_held_lock_is_not_a_running_mug() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("caffeine.lock");
        let held = Instance::try_hold(&path).expect("hold").expect("free");
        std::fs::write(
            &path,
            format!("{}\ncom.aterm.aterm.caffeine\n", std::process::id()),
        )
        .expect("write");
        assert!(running_holder(&path).is_some(), "the file names a holder");
        assert_eq!(running_mug(&path, &dir.path().join("code")), None);
        drop(held);
    }

    #[test]
    fn a_holder_is_its_mug_only_running_serve_from_its_jobs_place() {
        let serve = |exe: &str| -> Vec<OsString> {
            [exe, "caffeine", "serve"].map(OsString::from).to_vec()
        };
        let code = Path::new("/s/caffeine-code.noindex");
        let installed = Holder {
            pid: 7,
            label: Some(crate::launchd::INSTALLED_LABEL.to_string()),
        };
        let copy = code
            .join(crate::launchd::INSTALLED_LABEL)
            .join("aterm.app/Contents/MacOS/aterm");
        let bundle = Path::new(crate::launchd::INSTALLED_EXECUTABLE);
        for exe in [copy.as_path(), bundle] {
            let argv = serve(&exe.to_string_lossy());
            assert!(
                installed.is_run_by(exe, &argv, Some(code)),
                "{}",
                exe.display()
            );
        }
        let other = code
            .join("com.aterm.aterm.caffeine.dev.0123abcd")
            .join("aterm.app/Contents/MacOS/aterm");
        let bare = Path::new("/w/target/debug/aterm");
        for exe in [other.as_path(), bare] {
            let argv = serve(&exe.to_string_lossy());
            assert!(
                !installed.is_run_by(exe, &argv, Some(code)),
                "{}",
                exe.display()
            );
        }
        let dev = Holder {
            pid: 7,
            label: Some(crate::launchd::label_for(bare)),
        };
        assert!(dev.is_run_by(bare, &serve("aterm"), None));
        for argv in [
            vec![OsString::from("aterm")],
            ["aterm", "caffeine", "status"].map(OsString::from).to_vec(),
            ["caffeine", "serve"].map(OsString::from).to_vec(),
            ["aterm", "serve", "caffeine"].map(OsString::from).to_vec(),
        ] {
            assert!(!dev.is_run_by(bare, &argv, None), "{argv:?}");
        }
        let unnamed = Holder {
            pid: 7,
            label: None,
        };
        assert!(unnamed.is_run_by(&other, &serve("aterm"), Some(code)));
        assert!(!unnamed.is_run_by(&other, &[OsString::from("aterm")], Some(code)));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_at_the_lock_path_is_refused_not_followed() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "keep me\n").expect("victim");
        let path = dir.path().join("caffeine.lock");
        std::os::unix::fs::symlink(&victim, &path).expect("link");
        assert!(Instance::try_hold(&path).is_err());
        assert_eq!(std::fs::read_to_string(&victim).expect("read"), "keep me\n");
        assert_eq!(running_pid(&path), None);

        use std::os::fd::{FromRawFd as _, IntoRawFd as _};
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::remove_file(&path).expect("unlink");
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o640)).expect("mode");
        std::fs::hard_link(&victim, &path).expect("hard link");
        assert!(Instance::try_hold(&path).is_err());
        let raw = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open")
            .into_raw_fd();
        let adopted = adopt_fd(raw, &path);
        if adopted.is_none() {
            // SAFETY: refused adoption leaves this handed-over descriptor open and owned by this test; it is closed exactly once here.
            drop(unsafe { std::fs::File::from_raw_fd(raw) });
        }
        assert!(adopted.is_none(), "a hard-linked inode is not adopted");
        assert_eq!(std::fs::read_to_string(&victim).expect("read"), "keep me\n");
        assert_eq!(
            std::fs::metadata(&victim)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o640,
            "refused lock operations preserve the other file's mode"
        );
    }

    /// A FIFO planted at the lock path is opened without waiting for a writer and refused as
    /// not a regular file; no pid is read from it.
    #[cfg(unix)]
    #[test]
    fn a_fifo_at_the_lock_path_is_refused_without_waiting() {
        use std::os::unix::ffi::OsStrExt as _;
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("caffeine.lock");
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("no NUL");
        // SAFETY: a NUL-terminated path the call reads and does not retain.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0, "mkfifo");
        let err = Instance::try_hold(&path).expect_err("a FIFO");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
        assert_eq!(running_pid(&path), None);
    }

    #[test]
    fn adopt_without_the_variable_is_none() {
        assert!(std::env::var_os(LOCK_FD_ENV).is_none());
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        assert!(Instance::adopt(&dir.path().join("caffeine.lock")).is_none());
    }

    /// A closed copy of the hold's descriptor (what the exec does to the old image's) leaves the
    /// lock held; a handed-over one is adopted as the hold, with the lock held throughout, and
    /// the adopted hold's drop lets it go (`LOCK_UN` is on the shared description).
    #[cfg(unix)]
    #[test]
    fn a_handed_over_descriptor_is_adopted_with_its_lock() {
        use std::os::fd::IntoRawFd as _;
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("caffeine.lock");
        let first = Instance::try_hold(&path).expect("hold").expect("free");
        drop(first.handover().expect("handover"));
        assert!(Instance::try_hold(&path).expect("hold").is_none());
        let raw = first.handover().expect("handover").into_raw_fd();
        assert!(raw > 2, "a copy is never a stdio slot");
        let adopted = adopt_fd(raw, &path).expect("adopted");
        assert_eq!(running_pid(&path), Some(std::process::id()));
        assert!(Instance::try_hold(&path).expect("hold").is_none());
        drop(adopted);
        let again = Instance::try_hold(&path).expect("hold");
        assert!(again.is_some(), "the adopted hold's drop let it go");
        drop(again);
        drop(first);
    }

    #[cfg(unix)]
    #[test]
    fn a_descriptor_on_another_file_is_not_adopted() {
        use std::os::fd::{FromRawFd as _, IntoRawFd as _};
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("caffeine.lock");
        let held = Instance::try_hold(&path).expect("hold").expect("free");
        let other = dir.path().join("other");
        std::fs::write(&other, "x").expect("other");
        let raw = std::fs::File::open(&other).expect("open").into_raw_fd();
        assert!(adopt_fd(raw, &path).is_none());
        // SAFETY: F_GETFD on a number this test still owns.
        assert!(
            unsafe { libc::fcntl(raw, libc::F_GETFD) } >= 0,
            "left as it was"
        );
        // SAFETY: `raw` is this test's own open descriptor, closed once here.
        drop(unsafe { std::fs::File::from_raw_fd(raw) });
        assert!(adopt_fd(-1, &path).is_none());
        assert!(adopt_fd(i32::MAX, &path).is_none(), "not open");
        drop(held);
    }

    /// A descriptor on the lock file that does not hold it, while another does, is refused.
    #[cfg(unix)]
    #[test]
    fn a_descriptor_on_the_lock_another_holds_is_not_adopted() {
        use std::os::fd::IntoRawFd as _;
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("caffeine.lock");
        let held = Instance::try_hold(&path).expect("hold").expect("free");
        let raw = std::fs::File::open(&path).expect("open").into_raw_fd();
        assert!(adopt_fd(raw, &path).is_none());
        assert_eq!(running_pid(&path), Some(std::process::id()));
        drop(held);
    }

    #[test]
    fn holders_parse_strictly() {
        let pid = |pid| Some(Holder { pid, label: None });
        assert_eq!(parse_holder("1\n"), pid(1));
        assert_eq!(parse_holder("4242"), pid(4242));
        let named = Some(Holder {
            pid: 9,
            label: Some("com.aterm.aterm.caffeine".to_string()),
        });
        assert_eq!(parse_holder("9\ncom.aterm.aterm.caffeine\n"), named);
        assert_eq!(parse_holder("9\ncom.aterm.aterm.caffeine"), named);
        for bad in [
            "",
            "\n",
            "0",
            " 1\n",
            "1 \n",
            "1\n\n",
            "+1",
            "x",
            "4294967296",
            "9\ncom.example.other\n",
            "9\ncom.aterm.aterm.caffeine\nextra\n",
            "\ncom.aterm.aterm.caffeine\n",
        ] {
            assert_eq!(parse_holder(bad), None, "{bad:?}");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn this_boot_reads_as_one_id() {
        let boot = boot_id().expect("kern.bootsessionuuid");
        assert_ne!(boot, 0);
        assert_eq!(boot_id(), Some(boot));
        assert_eq!(
            crate::state::parse_boot(&crate::state::boot_text(boot)),
            Some(boot)
        );
    }
}
