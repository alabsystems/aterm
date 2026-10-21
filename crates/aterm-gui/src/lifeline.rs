// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `--lifeline-fd <n>`: a harness's headless instance ends with the process that started it.
//! The descriptor protocol lives in `aterm_uds::lifeline`; this is the window's wiring:
//!
//! 1. [`check_or_exit`], right after the command line is parsed: a descriptor that cannot be
//!    a lifeline is a usage error before anything runs. A harness that asked for a lifeline
//!    and silently did not get one would leak the instance.
//! 2. [`adopt_or_exit`], after the boot apply: the descriptor moves to a private close-on-exec
//!    copy (a stdio number gets `/dev/null` back), and any inherited copy of the launcher's end
//!    is closed (the instance would otherwise wait on itself).
//! 3. [`watch`], once the event loop's proxy exists: a thread blocks in `read` until
//!    end-of-file and posts [`crate::Wake::LifelineCut`]; the event loop answers with the
//!    ordinary quit (`App::on_lifeline_cut` → `on_quit_requested`), which unlinks the socket
//!    and hangs up every shell. A process still alive [`SHUTDOWN_GRACE`] later leaves through
//!    `crash_signal::clean_exit_now`, so a wedged main thread does not leak.
//!
//! The lifeline crosses the instance's own relaunches. The boot swap's re-exec and the
//! crash-loop revert run inside the boot apply, which is why adoption waits until after it:
//! the new image must find the launcher's number intact. (The apply's helpers run with
//! `/dev/null` on stdin; a read-end copy they hold keeps nobody alive.) The cold lane's `exec`
//! runs later, so adoption keeps a second close-on-exec copy ([`relaunch_descriptor`]) that
//! the lane names in the successor's argv and makes inheritable for the `exec` alone
//! ([`inherit_across_exec`]).

use std::os::fd::OwnedFd;
use std::time::Duration;

use aterm_window::event_loop::EventLoopProxy;

/// How long a cut lifeline waits for the ordinary quit before the backstop ends the process.
/// Generous: the teardown can wait on a video export's cleanup; the backstop is for a main
/// thread that is not coming back.
pub(crate) const SHUTDOWN_GRACE: Duration = Duration::from_secs(30);

/// An adopted lifeline: the private copy, and the number the launcher named (for
/// the words that report it).
pub(crate) struct Adopted {
    fd: OwnedFd,
    number: i32,
}

/// The copy a relaunch of this image hands on: a second close-on-exec descriptor of the same
/// read end, kept for the process's life. A read end keeps no launcher alive, so holding it
/// does not delay the cut.
static RELAUNCH_COPY: std::sync::OnceLock<OwnedFd> = std::sync::OnceLock::new();

/// Exit 2 with [`adopt_or_exit`]'s usage error now if descriptor `number` could not be
/// adopted as a lifeline; changes nothing otherwise (`aterm_uds::lifeline::check`). Runs before
/// the boot apply, so a bad flag cannot first let the apply swap in a staged build, re-exec and
/// count a trial launch of it.
pub(crate) fn check_or_exit(number: i32) {
    if let Err(e) = aterm_uds::lifeline::check(number) {
        crate::logging::stderr_line!("aterm-gui: {e} (try --help)");
        std::process::exit(2);
    }
}

/// Take over descriptor `number` as this instance's lifeline, or end the launch
/// with a usage error (exit 2) that says why. Called while the launch is still
/// single-threaded, before any session could be spawned holding the number.
pub(crate) fn adopt_or_exit(number: i32) -> Adopted {
    let fd = match aterm_uds::lifeline::adopt(number) {
        Ok(fd) => fd,
        Err(e) => {
            crate::logging::stderr_line!("aterm-gui: {e} (try --help)");
            std::process::exit(2);
        }
    };
    // A copy of the launcher's end inherited at another number would keep this instance
    // waiting on itself (bash 3.2 leaks one into an `exec`'d program). The sweep must run
    // single-threaded, as the launch still is here.
    let copies = match aterm_uds::lifeline::close_inherited_copies(&fd) {
        Ok(closed) if closed.is_empty() => String::new(),
        Ok(closed) => {
            let numbers: Vec<String> = closed.iter().map(ToString::to_string).collect();
            format!(
                " (closed the copies of it this instance inherited at fd {}: held here, they \
                 would have kept it from ever ending)",
                numbers.join(", ")
            )
        }
        // The lifeline still works unless this process holds a copy of the launcher's end,
        // which is what could not be checked.
        Err(e) => format!(
            " (could not check for inherited copies of it: {e}; one held here would keep it \
             from ending)"
        ),
    };
    // The relaunch copy is made after the sweep above, which would close it too. `try_clone`
    // is `F_DUPFD_CLOEXEC`, so it is close-on-exec from birth. Without a copy a relaunch drops
    // the flag (`running_window_successor_args`) and the successor runs unwatched.
    match fd.try_clone() {
        Ok(copy) => {
            let _ = RELAUNCH_COPY.set(copy);
        }
        Err(e) => aterm_log::warn!(
            "lifeline: no copy for a relaunch ({e}); an update's relaunch of this instance \
             runs without one"
        ),
    }
    // Said once, so a harness can read in its instance's log that the lifeline took.
    crate::logging::stderr_line!(
        "aterm-gui: lifeline armed on fd {number}: this instance shuts down when the process \
         that started it is gone{copies}"
    );
    Adopted { fd, number }
}

/// The descriptor a relaunch of this image names with `--lifeline-fd` — the copy
/// [`adopt_or_exit`] kept — or `None` when this instance has no lifeline.
pub(crate) fn relaunch_descriptor() -> Option<i32> {
    RELAUNCH_COPY.get().map(std::os::fd::AsRawFd::as_raw_fd)
}

/// Set by the watcher as soon as its read returns, before the quit wake. macOS hands a FIFO's
/// end-of-file to one reader only, so a lifeline already cut here must not be handed on.
static CUT_SEEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The relaunch copy made inheritable until the guard drops: taken right before an `exec`
/// that hands it on; dropping it after a failed `exec` restores close-on-exec. `None` with no
/// lifeline, with one already cut (a successor would wait for an end-of-file already read; it
/// is told a number it will not find instead, and exits), or when the flag could not be
/// cleared.
pub(crate) fn inherit_across_exec() -> Option<InheritedAcrossExec> {
    if CUT_SEEN.load(std::sync::atomic::Ordering::Acquire) {
        return None;
    }
    let fd = relaunch_descriptor()?;
    // Lazy: `then_some` would build the guard on a refusal too, and its drop
    // would run another `fcntl`.
    set_close_on_exec(fd, false).then(|| InheritedAcrossExec { fd })
}

/// See [`inherit_across_exec`].
pub(crate) struct InheritedAcrossExec {
    fd: i32,
}

impl Drop for InheritedAcrossExec {
    fn drop(&mut self) {
        let _ = set_close_on_exec(self.fd, true);
    }
}

/// `FD_CLOEXEC` on `fd` set to `on`; `false` when `fcntl` refused.
fn set_close_on_exec(fd: i32, on: bool) -> bool {
    // SAFETY: `F_GETFD`/`F_SETFD` read and write one descriptor's flags; `fd` is
    // the relaunch copy, open for the process's life (`RELAUNCH_COPY` is never
    // dropped), so no other owner's number is touched.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags == -1 {
            return false;
        }
        let flags = if on {
            flags | libc::FD_CLOEXEC
        } else {
            flags & !libc::FD_CLOEXEC
        };
        libc::fcntl(fd, libc::F_SETFD, flags) != -1
    }
}

/// Watch `adopted` on its own thread: at end-of-file (or a failed read) post
/// [`crate::Wake::LifelineCut`] through `proxy`, then hold the backstop.
///
/// Fails safe: if the thread cannot start the instance exits (EX_OSERR) rather than run with
/// a lifeline nobody reads.
pub(crate) fn watch(adopted: Adopted, proxy: EventLoopProxy<crate::Wake>) {
    let Adopted { fd, number } = adopted;
    let spawned = std::thread::Builder::new()
        .name("aterm-lifeline".to_string())
        .spawn(move || {
            // Never Housekeeping: the backstop is a deadline, and a starved watcher would leak
            // the process it exists to end (`qos::Role::Housekeeping`).
            crate::qos::set_self(crate::qos::Role::Background);
            let cut = aterm_uds::lifeline::wait_for_cut(fd);
            CUT_SEEN.store(true, std::sync::atomic::Ordering::Release);
            let why = match cut {
                aterm_uds::lifeline::Cut::Closed => format!("fd {number} reached end-of-file"),
                aterm_uds::lifeline::Cut::Failed(e) => {
                    format!("fd {number} could no longer be read ({e})")
                }
            };
            aterm_log::info!(
                "lifeline cut ({why}): the process that started this headless instance is \
                 gone; shutting down"
            );
            crate::logging::stderr_line!("aterm-gui: lifeline cut ({why}); shutting down");
            // A failed send means the event loop is already gone; the backstop covers it.
            let _ = proxy.send_event(crate::Wake::LifelineCut);
            std::thread::sleep(SHUTDOWN_GRACE);
            aterm_log::error!(
                "lifeline cut {} s ago and the quit has not finished; exiting now",
                SHUTDOWN_GRACE.as_secs()
            );
            crate::logging::stderr_line!(
                "aterm-gui: lifeline cut {} s ago and the quit has not finished; exiting now",
                SHUTDOWN_GRACE.as_secs()
            );
            crate::crash_signal::clean_exit_now(0)
        });
    if let Err(e) = spawned {
        aterm_log::error!("the lifeline watcher could not start ({e}); refusing to run unwatched");
        crate::logging::stderr_line!(
            "aterm-gui: the lifeline watcher could not start ({e}); refusing to run unwatched"
        );
        // EX_OSERR: a resource failure, not a usage error.
        crate::crash_signal::clean_exit_now(71);
    }
}

/// `--ready-fd <n>`: a harness's headless instance says, once,
/// that its control socket listens, so the harness waits on an event instead of
/// polling the socket. The harness hands the write end of a pipe (a FIFO's, as
/// `aterm_agent::ready` makes it) at number `n`; this instance writes one byte to
/// it once the control listener is bound ([`signal_ready`], from the
/// `Wake::ControlPrepared` the control thread posts after its bind), then closes
/// it. Closed with nothing written, the instance has no socket to offer — the
/// control service did not start, or the launch disabled it — and a harness
/// reads end-of-file; one that died before either reads end-of-file too, the
/// kernel having closed its descriptors. Three steps, as the lifeline's:
///
/// 1. [`check_ready_or_exit`] before the boot apply: a number that is not a
///    pipe's write end is a usage error before anything runs.
/// 2. [`adopt_ready_or_exit`] after the boot apply (whose re-exec must find the
///    number as the harness left it): the descriptor becomes close-on-exec, so
///    no shell, helper or successor this instance spawns holds it — a held copy
///    would keep a dead instance's harness from reading end-of-file.
/// 3. [`signal_ready`]: the byte, or none, and the close.
///
/// An update's relaunch of a running instance is never told the number
/// (`cli::ready_request` drops it): its first image already signalled.
static READY: std::sync::Mutex<Option<OwnedFd>> = std::sync::Mutex::new(None);

/// Whether descriptor `number` can carry the ready signal: open, a pipe or FIFO,
/// and open for writing. Nothing is changed.
fn ready_end(number: i32) -> std::io::Result<()> {
    // SAFETY: `stat` is plain data for which all-zero is a valid value.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `fstat` writes into the live local `st`; a closed number answers -1.
    if unsafe { libc::fstat(number, &mut st) } != 0 {
        return Err(std::io::Error::other(format!(
            "--ready-fd {number} is not an open descriptor ({})",
            std::io::Error::last_os_error()
        )));
    }
    if st.st_mode & libc::S_IFMT != libc::S_IFIFO {
        return Err(std::io::Error::other(format!(
            "--ready-fd {number} is not a pipe: the signal is one byte down a pipe's write end"
        )));
    }
    // SAFETY: `F_GETFL` only reads the descriptor's status flags.
    let flags = unsafe { libc::fcntl(number, libc::F_GETFL) };
    if flags == -1 || flags & libc::O_ACCMODE != libc::O_WRONLY {
        return Err(std::io::Error::other(format!(
            "--ready-fd {number} is not a pipe's write end"
        )));
    }
    Ok(())
}

/// End the launch with a usage error (exit 2) now if descriptor `number` cannot
/// carry the ready signal ([`ready_end`]), changing nothing when it can. Called
/// right after the command line is parsed, before the boot apply, as
/// [`check_or_exit`] is.
pub(crate) fn check_ready_or_exit(number: i32) {
    if let Err(e) = ready_end(number) {
        crate::logging::stderr_line!("aterm-gui: {e} (try --help)");
        std::process::exit(2);
    }
}

/// Take descriptor `number` over as this instance's ready signal: checked again
/// ([`ready_end`]), made close-on-exec, kept for [`signal_ready`]. Called while
/// the launch is still single-threaded, after the boot apply and before any
/// session could be spawned holding the number.
pub(crate) fn adopt_ready_or_exit(number: i32) {
    check_ready_or_exit(number);
    // SAFETY: `number` was just checked open; this process owns it from here
    // (the harness handed it over, and nothing else here names it).
    let fd = unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(number) };
    if !set_close_on_exec(number, true) {
        crate::logging::stderr_line!(
            "aterm-gui: --ready-fd {number} could not be made close-on-exec ({}); a shell \
             holding it would keep a dead instance's harness waiting (try --help)",
            std::io::Error::last_os_error()
        );
        std::process::exit(2);
    }
    *READY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(fd);
}

/// Say whether the control socket listens: one byte down the adopted ready
/// descriptor when `listening`, none when not, then close it. Once — later calls,
/// and every call in an instance launched without `--ready-fd`, do nothing. A
/// harness that is gone makes the write fail (`EPIPE`; Rust ignores `SIGPIPE`),
/// which changes nothing here.
pub(crate) fn signal_ready(listening: bool) {
    let Some(fd) = READY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    else {
        return;
    };
    if listening {
        let mut end = std::fs::File::from(fd);
        let _ = std::io::Write::write_all(&mut end, b"R");
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::{AsRawFd as _, IntoRawFd as _};
    use std::os::unix::fs::MetadataExt as _;

    /// The node `fd` names, read from the descriptor itself, never by its `/dev/fd` path:
    /// on Darwin that lookup reads an open descriptor as closed while anything else looks
    /// the number up.
    fn node(fd: i32) -> Option<(u64, u64)> {
        aterm_uds::spawnfd::descriptor_metadata(fd)
            .ok()
            .map(|meta| (meta.dev(), meta.ino()))
    }

    fn close_on_exec(fd: i32) -> bool {
        // SAFETY: `F_GETFD` only reads one descriptor's flags.
        unsafe { libc::fcntl(fd, libc::F_GETFD) & libc::FD_CLOEXEC != 0 }
    }

    /// Round eight of the update audit, #28: the cold lane hands a lifelined
    /// instance's successor the copy [`super::adopt_or_exit`] keeps. It is the
    /// same FIFO as the adopted lifeline, it outlives the adoption's sweep of that
    /// FIFO's other descriptors (made BEFORE the sweep, the sweep closed it), and
    /// it is close-on-exec except inside [`super::inherit_across_exec`].
    #[test]
    fn the_relaunch_copy_is_the_lifeline_and_crosses_only_an_exec() {
        let dir = std::env::temp_dir().join(format!("aterm-lifeline-r8-28-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (held, far) = aterm_uds::lifeline::Lifeline::open(&dir).unwrap();
        // The launcher's end is in this process, so the sweep closes it (as a bash 3.2 leak);
        // it is never dropped, even on unwind, or its reused number would be closed a second
        // time under another test.
        let _held = std::mem::ManuallyDrop::new(held);
        // The instance's end is handed over by number.
        let number = far.into_raw_fd();
        let adopted = super::adopt_or_exit(number);
        let copy = super::relaunch_descriptor().expect("a relaunch copy is kept");
        assert_ne!(copy, adopted.fd.as_raw_fd());
        assert!(
            node(copy).is_some() && node(copy) == node(adopted.fd.as_raw_fd()),
            "the copy is open, and the lifeline's own FIFO"
        );
        assert!(close_on_exec(copy), "no spawn carries it");
        let inherited = super::inherit_across_exec().expect("inheritable for an exec");
        assert!(!close_on_exec(copy), "the exec carries it");
        drop(inherited);
        assert!(
            close_on_exec(copy),
            "an exec that returned leaves it close-on-exec"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
