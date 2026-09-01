// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! INHERIT TWO DESCRIPTORS INTO A CHILD AT FIXED NUMBERS — the one thing the
//! fabric bridge's launch needs that `std::process::Command` cannot express.
//!
//! aterm launches its `aterm-link serve` child holding the far ends of two
//! `socketpair(AF_UNIX)`s at **fds 3 and 4** (DESIGN-aterm-fabric.md §11.2). The
//! authority of that child is the DESCRIPTOR and nothing else — no token file
//! exists to steal, and the only way to be the bridge is to be the process aterm
//! spawned. std can place an inherited descriptor at 0, 1 or 2 (`Stdio::from`)
//! and nowhere else, so putting one at 3 needs `dup2(2)` between `fork` and
//! `exec`.
//!
//! ## Why this lives HERE
//!
//! aterm's doctrine confines `unsafe` to cordoned modules, and this crate is
//! already the one that owns raw-descriptor syscalls: [`crate::fdpass`] does
//! `sendmsg`/`recvmsg`/`fcntl` over `SCM_RIGHTS`, [`crate::process`] does
//! `kill(pid, 0)`. Adding a third descriptor primitive beside them keeps the
//! whole fd surface auditable in one small, dependency-free crate; adding it to
//! `aterm-gui` would open a new unsafe site in a 100 000-line frontend crate for
//! the sake of one syscall.
//!
//! ## The async-signal-safety argument, which is the whole safety argument
//!
//! `pre_exec` runs in the child after `fork(2)` and before `exec`. In a
//! multi-threaded parent (aterm is very much one) that child holds exactly one
//! thread and whatever locks the other threads happened to be holding at the
//! moment of the fork — so the closure may call ONLY async-signal-safe
//! functions. This one calls `fcntl(2)`, `dup2(2)` and `close(2)` and nothing
//! else. It allocates nothing, takes no lock, reads no `static` that another
//! thread could have been mutating, and formats no error string — a failure
//! comes back as the plain `errno` `std::io::Error::last_os_error` wraps.
//!
//! `fcntl(2)`, `dup2(2)` and `close(2)` are all on POSIX's async-signal-safe
//! list. That is the whole of the obligation, and it is met by construction
//! rather than by review.
//!
//! ## FD_CLOEXEC is the other half of "inherited"
//!
//! Placing a descriptor at a number is only half the job: it must also survive
//! `exec`. `dup2(old, new)` clears `FD_CLOEXEC` on `new` — but ONLY when it
//! actually duplicates. POSIX specifies `dup2(fd, fd)` as a no-op that returns
//! `fd` and changes nothing, close-on-exec included, and `UnixStream::pair()`
//! creates both ends close-on-exec. So a source that already sits on its own
//! target is the one shape where `dup2` succeeds and the child still execs with
//! that number CLOSED. [`spawn_with_two_fds`] therefore moves every source OFF
//! both target numbers before it places either one.

#![cfg(unix)]

use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::process::Command;

/// The two descriptor numbers the fabric bridge inherits: a verb connection and
/// a push (`subscribe`) connection. Named here rather than in the launcher so
/// the child and the parent read the SAME constants — a bridge that listened on
/// the wrong number would look exactly like a bridge that never started.
pub const BRIDGE_VERB_FD: RawFd = 3;
pub const BRIDGE_PUSH_FD: RawFd = 4;

/// SPAWN `cmd` with `first` and `second` inherited at [`BRIDGE_VERB_FD`] and
/// [`BRIDGE_PUSH_FD`].
///
/// TAKES THE COMMAND BY VALUE, and that is load-bearing rather than tidy: the
/// two descriptors are moved into the `pre_exec` closure the `Command` owns, so
/// the PARENT's copies live exactly as long as the `Command` does. Dropping it
/// here — after the spawn, on the success and the failure path alike — is what
/// closes them, and closing them is what lets the parent see the child's EOF
/// when the bridge dies. A caller that kept the `Command` alive would hold the
/// far ends open forever and the fail-closed halt would never fire.
///
/// # Errors
///
/// The ordinary [`Command::spawn`] errors, plus the child's `dup2` failure,
/// which `std` reports through the same channel.
pub fn spawn_with_two_fds(
    mut cmd: Command,
    first: OwnedFd,
    second: OwnedFd,
) -> io::Result<std::process::Child> {
    // SAFETY: the closure runs in the forked child before `exec` and calls only
    // `fcntl(2)`, `dup2(2)` and `close(2)`, all on POSIX's async-signal-safe
    // list. It allocates nothing, takes no lock, and reads no state another
    // thread of the parent could have been mutating at the moment of the fork —
    // the two descriptor numbers come from `OwnedFd`s the closure itself owns,
    // so they are live for every call and are closed by nobody else, and the
    // only other numbers it touches are the two copies it made itself.
    unsafe {
        std::os::unix::process::CommandExt::pre_exec(&mut cmd, move || {
            let mut a = first.as_raw_fd();
            let mut b = second.as_raw_fd();
            // MOVE EVERY SOURCE OFF EVERY TARGET, BEFORE PLACING EITHER. Four
            // collisions live in these four numbers, and the obvious one is not
            // the dangerous one:
            //
            //   * `b == BRIDGE_VERB_FD` — `dup2(a, VERB)` would close the second
            //     source out from under the next line. (The one this used to
            //     handle.)
            //   * `a == BRIDGE_VERB_FD`, or `b == BRIDGE_PUSH_FD` — `dup2(fd,
            //     fd)` is specified to do NOTHING: it returns `fd` and leaves
            //     `FD_CLOEXEC` exactly as it found it, where a real duplication
            //     would have cleared it. `UnixStream::pair()` creates both ends
            //     close-on-exec, so the child would exec with that number
            //     CLOSED — a bridge that starts, blocks on an empty descriptor
            //     and never speaks, which is the least diagnosable failure this
            //     module has.
            //   * `a == BRIDGE_PUSH_FD` — harmless on its own, moved anyway: one
            //     rule that cannot be reasoned about case by case is worth more
            //     here than three branches that each have to be right.
            //
            // `F_DUPFD` with a floor of `BRIDGE_PUSH_FD + 1` is what makes it one
            // rule: the copy lands strictly above both targets, so afterwards
            // neither `dup2` can be a self-copy and neither can close the other's
            // source.
            let mut moved_a: RawFd = -1;
            let mut moved_b: RawFd = -1;
            if a == BRIDGE_VERB_FD || a == BRIDGE_PUSH_FD {
                a = fcntl(a, F_DUPFD, BRIDGE_PUSH_FD + 1);
                if a == -1 {
                    return Err(io::Error::last_os_error());
                }
                moved_a = a;
            }
            if b == BRIDGE_VERB_FD || b == BRIDGE_PUSH_FD {
                b = fcntl(b, F_DUPFD, BRIDGE_PUSH_FD + 1);
                if b == -1 {
                    return Err(io::Error::last_os_error());
                }
                moved_b = b;
            }
            if dup2(a, BRIDGE_VERB_FD) == -1 {
                return Err(io::Error::last_os_error());
            }
            if dup2(b, BRIDGE_PUSH_FD) == -1 {
                return Err(io::Error::last_os_error());
            }
            // CLOSE THE TEMPORARIES. `F_DUPFD` clears `FD_CLOEXEC` on the copy
            // (only `F_DUPFD_CLOEXEC` keeps it), so one left open would survive
            // `exec` as a THIRD live end of a socketpair inside the child — and
            // the parent's EOF, which is what fires the fail-closed halt when the
            // bridge dies, would then never come. A failure to close a descriptor
            // this closure made itself is not actionable and must not fail the
            // spawn.
            if moved_a != -1 {
                close(moved_a);
            }
            if moved_b != -1 {
                close(moved_b);
            }
            Ok(())
        });
    }
    cmd.spawn()
}

// The whole FFI surface of this module, in one block. `fcntl(2)`, `dup2(2)` and
// `close(2)` are POSIX and identically shaped on every Unix aterm builds for;
// `fcntl` is variadic by declaration, which is not cosmetic — on arm64 Darwin a
// variadic argument is passed differently from a fixed one, so declaring it
// non-variadic would be an ABI mismatch rather than a tidier signature.
unsafe extern "C" {
    fn fcntl(fd: RawFd, cmd: i32, ...) -> i32;
    fn dup2(old: RawFd, new: RawFd) -> RawFd;
    fn close(fd: RawFd) -> i32;
}

/// `fcntl(2)`'s "duplicate to the lowest free descriptor at or above the third
/// argument". POSIX-defined and 0 on every supported Unix, so it is not per-ABI.
const F_DUPFD: i32 = 0;

#[cfg(test)]
mod tests {
    use super::*;

    /// The two numbers are the protocol. A launcher and a bridge that disagreed
    /// about them would produce a child that starts, blocks on an empty
    /// descriptor and never speaks — the least diagnosable failure available
    /// here — so they are pinned rather than commented.
    #[test]
    fn the_inherited_numbers_are_three_and_four() {
        assert_eq!(BRIDGE_VERB_FD, 3);
        assert_eq!(BRIDGE_PUSH_FD, 4);
    }

    /// END TO END, through a real fork+exec: a child that reads fd 3 and writes
    /// fd 4 sees the parent's socketpair ends at those exact numbers. `/bin/sh`
    /// is the child because it is the one interpreter every Unix this builds for
    /// has, and `<&3` / `>&4` are plain POSIX redirections.
    #[test]
    fn a_child_reads_fd_three_and_writes_fd_four() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;

        let (mut near_verb, far_verb) = UnixStream::pair().expect("socketpair");
        let (mut near_push, far_push) = UnixStream::pair().expect("socketpair");
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("read line <&3; echo \"got:$line\" >&4");
        let mut child = spawn_with_two_fds(cmd, OwnedFd::from(far_verb), OwnedFd::from(far_push))
            .expect("spawn /bin/sh");

        near_verb.write_all(b"hello\n").expect("write fd 3");
        let mut got = String::new();
        // A bounded read so a wedged child fails as a test rather than a hang.
        near_push
            .set_read_timeout(Some(std::time::Duration::from_secs(30)))
            .expect("bound the read");
        near_push.read_to_string(&mut got).expect("read fd 4");
        assert_eq!(got.trim_end(), "got:hello");
        let status = child.wait().expect("reap the child");
        assert!(status.success(), "child exited {status:?}");
    }

    // -----------------------------------------------------------------------
    // The collision cases, staged deterministically in a child of this test
    // -----------------------------------------------------------------------

    /// Selects a staged run. Present in the environment means "you ARE the
    /// staged child": do the one scenario it names and let libtest's exit code
    /// carry the verdict back.
    const STAGE_ENV: &str = "ATERM_UDS_SPAWNFD_STAGE";

    /// `fcntl(2)`'s descriptor-flag getter/setter and the one flag there is.
    /// Same numbers on every supported Unix, as in [`crate::fdpass`].
    const F_GETFD: i32 = 1;
    const F_SETFD: i32 = 2;
    const FD_CLOEXEC: i32 = 1;

    /// `F_GETFD` on `fd`, or `-1` when the number is not open.
    fn flags(fd: RawFd) -> i32 {
        // SAFETY: `F_GETFD` reads one descriptor's flags and takes no varargs.
        // An invalid number is an `EBADF` return, not undefined behaviour, which
        // is exactly what the freeness assertion below reads.
        unsafe { fcntl(fd, F_GETFD) }
    }

    /// Put `fd` at exactly `number`, close-on-exec, and give back the ownership.
    ///
    /// THE ONE THING A TEST CANNOT STAGE WITHOUT A SYSCALL. The defect these
    /// tests pin is about a source descriptor that already sits on its own
    /// target number, and which numbers `UnixStream::pair()` returns is the
    /// kernel's business, not the caller's. So the staged child places them
    /// itself — asserting first that the number is free (it is a fresh `exec`
    /// that opened nothing but stdio) and that `pair()` really did hand over a
    /// close-on-exec descriptor, since the whole defect rests on that flag.
    fn place(fd: OwnedFd, number: RawFd) -> OwnedFd {
        use std::os::fd::FromRawFd;
        assert_eq!(
            flags(number),
            -1,
            "fd {number} is already open in this staged child; the staging would clobber it"
        );
        assert_eq!(
            flags(fd.as_raw_fd()) & FD_CLOEXEC,
            FD_CLOEXEC,
            "UnixStream::pair() no longer returns close-on-exec ends; this test reproduces \
             that flag and the defect it guards depends on it"
        );
        // SAFETY: `fd` is live and owned here, `number` was just shown to be
        // free, and the `from_raw_fd` takes ownership of exactly the descriptor
        // `dup2` created — the original is dropped (closed) before it does, so no
        // number is owned twice.
        unsafe {
            assert_ne!(
                dup2(fd.as_raw_fd(), number),
                -1,
                "dup2 onto the staged number"
            );
            assert_ne!(
                fcntl(number, F_SETFD, FD_CLOEXEC),
                -1,
                "restore the close-on-exec `pair()` had set"
            );
            drop(fd);
            OwnedFd::from_raw_fd(number)
        }
    }

    /// One staged scenario, run INSIDE the re-exec'd child.
    fn staged(which: &str) {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;

        let (near_verb, far_verb) = UnixStream::pair().expect("socketpair");
        let (mut near_push, far_push) = UnixStream::pair().expect("socketpair");
        let mut near_verb = near_verb;
        let (far_verb, far_push) = (OwnedFd::from(far_verb), OwnedFd::from(far_push));
        let (far_verb, far_push) = match which {
            // The VERB lane's far end already sits on `BRIDGE_VERB_FD`. In a
            // fresh child `pair()` hands out the lowest free numbers, so the
            // near end is holding 3 and has to move first.
            "verb" => {
                near_verb = UnixStream::from(place(OwnedFd::from(near_verb), 10));
                (place(far_verb, BRIDGE_VERB_FD), far_push)
            }
            // The PUSH lane's far end already sits on `BRIDGE_PUSH_FD`.
            "push" => {
                let verb = place(far_verb, 11);
                (verb, place(far_push, BRIDGE_PUSH_FD))
            }
            other => panic!("unknown stage {other:?}"),
        };
        // THE STAGING IS THE TEST. Assert it took, so a future `pair()` that
        // allocates differently fails here rather than passing vacuously.
        match which {
            "verb" => assert_eq!(far_verb.as_raw_fd(), BRIDGE_VERB_FD),
            _ => assert_eq!(far_push.as_raw_fd(), BRIDGE_PUSH_FD),
        }

        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("read line <&3; echo \"got:$line\" >&4");
        let mut child = spawn_with_two_fds(cmd, far_verb, far_push).expect("spawn /bin/sh");
        near_verb.write_all(b"hello\n").expect("write fd 3");
        let mut got = String::new();
        near_push
            .set_read_timeout(Some(std::time::Duration::from_secs(30)))
            .expect("bound the read");
        near_push.read_to_string(&mut got).expect("read fd 4");
        assert_eq!(
            got.trim_end(),
            "got:hello",
            "stage {which}: the child lost an inherited descriptor at exec"
        );
        let status = child.wait().expect("reap the child");
        assert!(status.success(), "stage {which}: child exited {status:?}");
    }

    /// **A SOURCE THAT ALREADY SITS ON ITS OWN TARGET STILL REACHES THE CHILD.**
    /// `dup2(fd, fd)` does nothing — it does not clear `FD_CLOEXEC` — and
    /// `UnixStream::pair()` creates its ends close-on-exec, so the two cases
    /// staged here are the ones where the old code succeeded and the child
    /// nevertheless exec'd with the descriptor closed.
    ///
    /// IT RUNS IN A CHILD OF ITSELF, and that is not ceremony: this process's
    /// fd table belongs to `cargo test` (3 and 4 are usually taken, which is the
    /// only reason the happy-path test above never caught this), while a fresh
    /// `exec` of the same binary has 3 and 4 free and can place descriptors
    /// exactly without clobbering anything a harness owns. No sleeps, no
    /// probing: the layout is staged, asserted, then exercised.
    #[test]
    fn a_source_already_on_its_target_number_still_reaches_the_child() {
        if let Ok(stage) = std::env::var(STAGE_ENV) {
            staged(&stage);
            return;
        }
        for stage in ["verb", "push"] {
            let out = Command::new(std::env::current_exe().expect("the test binary"))
                .arg("a_source_already_on_its_target_number_still_reaches_the_child")
                .arg("--nocapture")
                .arg("--test-threads=1")
                .env(STAGE_ENV, stage)
                .output()
                .expect("re-exec this test binary");
            assert!(
                out.status.success(),
                "stage {stage}: {:?}\nstdout: {}\nstderr: {}",
                out.status,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}
