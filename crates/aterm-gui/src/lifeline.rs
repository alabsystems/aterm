// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LIFELINE — `--lifeline-fd <n>`: a harness's headless instance ends with the
//! process that started it.
//!
//! A test run or a script that boots `aterm --headless` used to end it only from
//! its own cleanup, and a killed process runs none: one such instance ran for
//! eleven days holding a shell, reachable by no `aterm ctl ls` (gap #36). The
//! descriptor protocol and both of its ends live in `aterm_uds::lifeline`; this
//! is the window's half of the wiring, in three steps, each at the one place in
//! the launch where it is safe:
//!
//! 1. [`adopt_or_exit`], right after the boot apply (below; [`check_or_exit`]
//!    refuses a descriptor that cannot be one before the apply runs): the inherited
//!    descriptor moves to a private close-on-exec copy — no shell, helper or
//!    successor this instance spawns carries it — and `/dev/null` goes back on
//!    stdin; any copy of the launcher's end the instance inherited at another
//!    number is closed, since the instance would otherwise wait on itself. A
//!    descriptor that cannot be a lifeline stops the launch with a
//!    usage error before any session runs: a harness that asked for a lifeline and
//!    silently did not get one would leak exactly the instance it asked about.
//! 2. [`watch`], once the event loop's proxy exists: one thread blocks in `read`
//!    until end-of-file, then posts [`crate::Wake::LifelineCut`].
//! 3. The event loop answers it with the ordinary quit
//!    (`App::on_lifeline_cut` → `on_quit_requested`): the teardown Cmd-Q runs —
//!    the socket and its token unlinked, the discovery entries withdrawn, and the
//!    PTYs closed with the process, so every shell gets its hangup.
//!
//! A wedged main thread must not turn a cut lifeline back into a leak, so the
//! watcher keeps a BACKSTOP: a process still here [`SHUTDOWN_GRACE`] after the
//! wake leaves through `crash_signal::clean_exit_now`. Only an instance started
//! with the flag has a watcher at all; every other launch is untouched.
//!
//! THE LIFELINE CROSSES THE INSTANCE'S OWN RELAUNCHES (round eight of the update
//! audit, #28). Three paths replace this image with another one of the same
//! launch, and each used to tell the new image `--lifeline-fd <n>` for a number
//! step 1 had already retired — a usage error and exit 2, so the harness lost its
//! instance to an update:
//!
//! * the BOOT SWAP's re-exec and the CRASH-LOOP REVERT's run inside the boot apply,
//!   so step 1 now runs after it (`main_entry`): until then the number is the
//!   launcher's, exactly as the new image must find it. The apply's helpers run
//!   with `/dev/null` on stdin, and a copy of a lifeline's READ end they hold for
//!   their bounded run keeps nobody alive;
//! * the COLD LANE's `exec` runs long after step 1, so step 1 keeps a second
//!   close-on-exec copy ([`relaunch_descriptor`]) that the lane names in the
//!   successor's argv and makes inheritable for the `exec` alone
//!   ([`inherit_across_exec`]).

use std::os::fd::OwnedFd;
use std::time::Duration;

use aterm_window::event_loop::EventLoopProxy;

/// How long a cut lifeline waits for the ordinary quit before the backstop ends
/// the process. Generous on purpose: the quit's teardown can wait on a video
/// export's cleanup, and the backstop is for a main thread that is not coming
/// back, not for a slow one.
pub(crate) const SHUTDOWN_GRACE: Duration = Duration::from_secs(30);

/// An adopted lifeline: the private copy, and the number the launcher named (for
/// the words that report it).
pub(crate) struct Adopted {
    fd: OwnedFd,
    number: i32,
}

/// The copy of the lifeline a relaunch of this image hands on (see the module
/// doc): a second close-on-exec descriptor of the same read end, kept for the
/// process's life. A READ end keeps no launcher alive, so holding it changes
/// nothing about when the lifeline is cut; a successor handed it reads the same
/// end-of-file the watcher would have.
static RELAUNCH_COPY: std::sync::OnceLock<OwnedFd> = std::sync::OnceLock::new();

/// End the launch with [`adopt_or_exit`]'s usage error (exit 2) now if descriptor
/// `number` could not be taken over as a lifeline — changing nothing when it could
/// (`aterm_uds::lifeline::check`). Called right after the command line is parsed,
/// BEFORE the boot apply (round eight of the update audit, #28's review): the
/// adoption itself waits until after the apply, whose re-exec must find the
/// number as the launcher left it, and a usage error found only then had already
/// let the apply swap a staged build in, re-exec and count a trial launch of it —
/// three `--lifeline-fd 0` slips at a prompt walked a healthy build into its
/// revert. A usage error stops the launch before anything runs, as it always did.
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
    // A copy of the launcher's end inherited at another number would keep this
    // instance waiting on itself (bash 3.2 leaks one into an `exec`'d program);
    // `close_inherited_copies` says why. Still single-threaded here, as it needs.
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
        // Unsure, so say so: the lifeline still works unless this process holds a
        // copy of its launcher's end, and that is what could not be checked.
        Err(e) => format!(
            " (could not check for inherited copies of it: {e}; one held here would keep it \
             from ending)"
        ),
    };
    // The relaunch copy — AFTER the sweep above, which closes every other
    // descriptor of this FIFO and would take the copy with it. `try_clone` is
    // `F_DUPFD_CLOEXEC`: flagged from birth, numbered 3 or above. Without it a
    // relaunch drops the flag (`running_window_successor_args`): the successor then
    // runs unwatched, which its launcher's cleanup still ends.
    match fd.try_clone() {
        Ok(copy) => {
            let _ = RELAUNCH_COPY.set(copy);
        }
        Err(e) => aterm_log::warn!(
            "lifeline: no copy for a relaunch ({e}); an update's relaunch of this instance \
             runs without one"
        ),
    }
    // Said once, like the headless announcement beside it: a harness that armed a
    // lifeline can read in its instance's log that it took.
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

/// Raised by the watcher the moment its read returns — before the wake that
/// quits. A FIFO hands its end-of-file to ONE read (`aterm_uds::lifeline`'s
/// measurement), so a lifeline already cut here would never read cut in a
/// successor: it must not be handed on.
static CUT_SEEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The relaunch copy made inheritable until the guard drops: taken right before an
/// `exec` that hands it on, dropped when the `exec` returned — which puts
/// close-on-exec back, so no shell or helper this process spawns after a failed
/// relaunch holds it. `None` with no lifeline, with one already cut (this instance
/// is on its way out, and a successor handed it would wait for an end-of-file that
/// was already read — it is told a number it will not find instead, and exits), or
/// when the flag could not be cleared.
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

/// Watch `adopted` on its own thread: at end-of-file (or a read that fails) post
/// [`crate::Wake::LifelineCut`] through `proxy`, then hold the backstop.
///
/// FAIL SAFE when the thread cannot start: an armed lifeline nobody reads is the
/// leak this exists to end, so the instance does not run on without one — it says
/// so and exits, which its harness sees as an instance that failed to boot.
pub(crate) fn watch(adopted: Adopted, proxy: EventLoopProxy<crate::Wake>) {
    let Adopted { fd, number } = adopted;
    let spawned = std::thread::Builder::new()
        .name("aterm-lifeline".to_string())
        .spawn(move || {
            // Never Housekeeping: the backstop is a deadline, and a starved
            // watcher would leak the very process it exists to end (the reapers'
            // rule in `qos::Role::Housekeeping`).
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
            // A send that fails means the event loop is already gone — the process
            // is on its way out, and the backstop below covers it either way.
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

/// THE READY SIGNAL — `--ready-fd <n>`: a harness's headless instance says, once,
/// that its control socket listens, so the harness waits on an event instead of
/// polling the socket. The harness hands the write end of a pipe (a FIFO's, as
/// `aterm_agent::ready` makes it) at number `n`; this instance writes ONE byte to
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

    fn node(fd: i32) -> Option<(u64, u64)> {
        std::fs::metadata(format!("/dev/fd/{fd}"))
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
        // The launcher's end is IN this process, so the adoption's sweep closes it
        // as the copy a bash 3.2 launch leaks: it is never dropped here — not even
        // by a failing assertion's unwind — or its number, closed and then reused
        // by another test's file, would be closed a second time under that test.
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
