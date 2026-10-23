// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `--lifeline-fd <n>`: a harness's headless instance ends with the process that started it.

use std::os::fd::OwnedFd;
use std::time::Duration;

use aterm_window::event_loop::EventLoopProxy;

/// How long a cut lifeline waits for the ordinary quit before the backstop ends the process.
pub(crate) const SHUTDOWN_GRACE: Duration = Duration::from_secs(30);

/// An adopted lifeline: the private copy, and the number the launcher named.
pub(crate) struct Adopted {
    fd: OwnedFd,
    number: i32,
}

/// The copy a relaunch of this image hands on: a second close-on-exec descriptor of the same
/// read end, kept for the process's life.
static RELAUNCH_COPY: std::sync::OnceLock<OwnedFd> = std::sync::OnceLock::new();

/// Exit 2 with [`adopt_or_exit`]'s usage error now if descriptor `number` could not be adopted as.
pub(crate) fn check_or_exit(number: i32) {
    if let Err(e) = aterm_uds::lifeline::check(number) {
        crate::logging::stderr_line!("aterm-gui: {e} (try --help)");
        std::process::exit(2);
    }
}

/// Take over descriptor `number` as this instance's lifeline.
pub(crate) fn adopt_or_exit(number: i32) -> Adopted {
    let fd = match aterm_uds::lifeline::adopt(number) {
        Ok(fd) => fd,
        Err(e) => {
            crate::logging::stderr_line!("aterm-gui: {e} (try --help)");
            std::process::exit(2);
        }
    };
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
        Err(e) => format!(
            " (could not check for inherited copies of it: {e}; one held here would keep it \
             from ending)"
        ),
    };
    match fd.try_clone() {
        Ok(copy) => {
            let _ = RELAUNCH_COPY.set(copy);
        }
        Err(e) => aterm_log::warn!(
            "lifeline: no copy for a relaunch ({e}); an update's relaunch of this instance \
             runs without one"
        ),
    }
    crate::logging::stderr_line!(
        "aterm-gui: lifeline armed on fd {number}: this instance shuts down when the process \
         that started it is gone{copies}"
    );
    Adopted { fd, number }
}

/// The descriptor a relaunch of this image names with `--lifeline-fd`.
pub(crate) fn relaunch_descriptor() -> Option<i32> {
    RELAUNCH_COPY.get().map(std::os::fd::AsRawFd::as_raw_fd)
}

/// Set by the watcher as soon as its read returns, before the quit wake. macOS hands a FIFO's
/// end-of-file to one reader only, so a lifeline already cut here must not be handed on.
static CUT_SEEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The relaunch copy made inheritable until the guard drops.
pub(crate) fn inherit_across_exec() -> Option<InheritedAcrossExec> {
    if CUT_SEEN.load(std::sync::atomic::Ordering::Acquire) {
        return None;
    }
    let fd = relaunch_descriptor()?;
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
    // SAFETY: `F_GETFD`/`F_SETFD` read and write one descriptor's flags; `fd` is the relaunch.
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

/// Watch `adopted` on its own thread.
pub(crate) fn watch(adopted: Adopted, proxy: EventLoopProxy<crate::Wake>) {
    let Adopted { fd, number } = adopted;
    let spawned = std::thread::Builder::new()
        .name("aterm-lifeline".to_string())
        .spawn(move || {
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
        crate::crash_signal::clean_exit_now(71);
    }
}

/// `--ready-fd <n>`: a harness's headless instance says, once,
/// that its control socket listens, so the harness waits on an event instead of
/// polling the socket.
static READY: std::sync::Mutex<Option<OwnedFd>> = std::sync::Mutex::new(None);

/// Whether descriptor `number` can carry the ready signal: open, a pipe or FIFO,
/// and open for writing.
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

/// End the launch with a usage error (exit 2) now if descriptor `number` cannot carry the ready.
pub(crate) fn check_ready_or_exit(number: i32) {
    if let Err(e) = ready_end(number) {
        crate::logging::stderr_line!("aterm-gui: {e} (try --help)");
        std::process::exit(2);
    }
}

/// Take descriptor `number` over as this instance's ready signal.
pub(crate) fn adopt_ready_or_exit(number: i32) {
    check_ready_or_exit(number);
    // SAFETY: `number` was just checked open; this process owns it from here (the harness handed.
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

/// Say whether the control socket listens.
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

    /// The copy kept for the cold lane is the lifeline's own FIFO, survives the adoption's sweep,
    /// and is close-on-exec except inside [`super::inherit_across_exec`].
    #[test]
    fn the_relaunch_copy_is_the_lifeline_and_crosses_only_an_exec() {
        let dir = std::env::temp_dir().join(format!("aterm-lifeline-r8-28-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (held, far) = aterm_uds::lifeline::Lifeline::open(&dir).unwrap();
        let _held = std::mem::ManuallyDrop::new(held);
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
