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

/// Allowance for the lock release required by an explicitly requested quit.
#[cfg(any(target_os = "macos", all(test, unix)))]
pub(crate) const QUIT_HANG: std::time::Duration = std::time::Duration::from_secs(60);

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
        // no pid: another mug taking it, before it writes its own, must not make this exited
        // mug's pid read as a running one. (A mug killed outright leaves its pid; `running_mug`
        // checks it.)
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
        let Some(held) = Self::take_at(path)? else {
            return Ok(None);
        };
        held.write_holder()?;
        Ok(Some(held))
    }

    /// [`Instance::try_hold`] writing nothing into the file, and leaving its text as it was when
    /// let go: a cleanup's hold, in which a mug that finds the lock held reads no mug's pid.
    /// # Errors
    /// The file cannot be created or locked, or is not a private, singly linked regular file.
    pub fn try_hold_unnamed(path: &Path) -> io::Result<Option<Self>> {
        Ok(Self::take_at(path)?.map(|mut held| {
            held.leave_named = true;
            held
        }))
    }

    /// The open, the private-file check and the take of [`Instance::try_hold`].
    fn take_at(path: &Path) -> io::Result<Option<Self>> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            crate::paths::ensure_real_dir(parent)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
        }
        let file = aterm_uds::kept_out(|| options.open(path))?;
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
        Ok(Some(Self {
            file,
            label: None,
            leave_named: false,
        }))
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
        let digits = named.to_str()?;
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let fd: i32 = digits.parse().ok()?;
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
/// `/dev/null` placed on it when it is a stdio slot) and the hold lives on a copy. Otherwise `fd`
/// is left as it was, unless a copy of it and both stats showed it open on the lock file, a
/// private one ([`private_lock_file`]), and, for a stdio slot, `/dev/null` opens and takes its
/// place: then it is retired too, since a stdin on the lock file is never wanted.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn adopt_fd(fd: i32, path: &Path) -> Option<Instance> {
    use std::os::fd::{AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd};
    use std::os::unix::fs::MetadataExt as _;
    if fd < 0 || !crate::paths::parent_is_real_dir(path) {
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
        use std::os::unix::fs::OpenOptionsExt as _;
        let null = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
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
    aterm_uds::keep_out_of_children(&file).ok()?;
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
    /// Whether a process running `exe` (canonical) with the arguments `argv` is this holder's mug: it runs `… acaffeine serve`, from where this holder's job runs it: its copy under `code_dir` (canonical; `None` when there is none) in the label's directory, or else an executable whose own label ([`crate::launchd::label_for`]) is the label.
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

/// Who holds the lock at `path`, if anyone does, as its file says (the pid unchecked: a mug killed outright leaves its pid behind, which reads as live while another mug takes the lock, before it writes its own; [`running_mug`] checks it).
#[must_use]
pub fn running_holder(path: &Path) -> Option<Holder> {
    if !crate::paths::parent_is_real_dir(path)
        || aterm_uds::ownerlock::probe(path) != aterm_uds::ownerlock::Owner::Live
    {
        return None;
    }
    last_holder(path)
}

/// Who the lock file at `path` names, held or not: the running mug, or one that died without letting go (a crash, a signal), whose pid and label stay in the file.
#[must_use]
pub fn last_holder(path: &Path) -> Option<Holder> {
    use std::io::Read as _;
    if !crate::paths::parent_is_real_dir(path) {
        return None;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options.open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > HOLDER_CAP {
        return None;
    }
    let mut text =
        String::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
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
    await_released_started(path, pids, within, std::time::Instant::now(), || {})
}

fn await_released_started(
    path: &Path,
    pids: &[u32],
    within: std::time::Duration,
    start: std::time::Instant,
    mut held: impl FnMut(),
) -> Result<(), u32> {
    loop {
        let Some(holder) = running_holder(path).filter(|holder| pids.contains(&holder.pid)) else {
            return Ok(());
        };
        if start.elapsed() >= within {
            return Err(holder.pid);
        }
        held();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Send `pid` SIGTERM (never pid 0 or 1, nor a negative group); one already gone is no error.
/// # Errors
/// `pid` is 0, 1 or past `pid_t`, or `kill` refused (another user's process).
#[cfg(unix)]
pub fn terminate(pid: u32) -> Result<(), String> {
    let pid = libc::pid_t::try_from(pid)
        .ok()
        .filter(|pid| *pid > 1)
        .ok_or_else(|| format!("{pid} is not a mug's pid"))?;
    // SAFETY: two integers; `pid` is positive and not launchd's, so this signals one process.
    if unsafe { libc::kill(pid, libc::SIGTERM) } == 0 {
        return Ok(());
    }
    let e = std::io::Error::last_os_error();
    if e.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(format!("kill {pid}: {e}"))
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
/// `aterm acaffeine` runs only on macOS.
#[cfg(test)]
#[path = "instance_tests.rs"]
mod tests;
