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
//! 1. [`adopt_or_exit`], right after the command line is parsed: the inherited
//!    descriptor moves to a private close-on-exec copy — no shell, helper or
//!    successor this instance spawns carries it — and `/dev/null` goes back on
//!    stdin; any copy of the launcher's end the instance inherited at another
//!    number is closed, since the instance would otherwise wait on itself. A
//!    descriptor that cannot be a lifeline stops the launch with a
//!    usage error before anything runs: a harness that asked for a lifeline and
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
//! ONE INTERACTION, stated rather than hidden: a launch whose boot APPLIES a
//! staged update re-execs itself with the same argv, and by then step 1 has
//! moved the lifeline off its number — so the new image refuses `--lifeline-fd`
//! (a usage error naming what the number now is) and exits. The harness sees an
//! instance that did not boot, never one that leaked. It cannot arise in a
//! harness that switches `[update] auto_apply` off, as every in-tree one does,
//! nor from a development build, which never applies.

use std::os::fd::OwnedFd;
use std::time::Duration;

use winit::event_loop::EventLoopProxy;

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

/// Take over descriptor `number` as this instance's lifeline, or end the launch
/// with a usage error (exit 2) that says why. Called while the launch is still
/// single-threaded, before any child could be spawned holding the number.
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
    // Said once, like the headless announcement beside it: a harness that armed a
    // lifeline can read in its instance's log that it took.
    crate::logging::stderr_line!(
        "aterm-gui: lifeline armed on fd {number}: this instance shuts down when the process \
         that started it is gone{copies}"
    );
    Adopted { fd, number }
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
            let why = match aterm_uds::lifeline::wait_for_cut(fd) {
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
