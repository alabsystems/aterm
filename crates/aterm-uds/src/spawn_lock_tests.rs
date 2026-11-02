// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `spawn_lock`'s tests.

/// The kernel's observed close-on-fork capability, independent of the UDS wrapper.
#[cfg(target_vendor = "apple")]
pub(crate) fn clofork_supported() -> bool {
    use std::os::fd::AsFd as _;
    static SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        let (socket, _peer) = std::os::unix::net::UnixStream::pair().expect("a capability probe");
        astream_sys::close_on_fork(socket.as_fd()).expect("the kernel answers the flag request")
    })
}

/// A creation always nests: an inner [`crate::kept_out`] on the thread inside an outer one's
/// `create` takes nothing more, and every descriptor gets the supported marks.
#[cfg(unix)]
#[test]
fn a_nested_creation_requests_each_descriptors_supported_marks() {
    use std::os::unix::net::UnixStream;
    let (a, b) = crate::kept_out(|| crate::kept_out(UnixStream::pair)).expect("a nested pair");
    #[cfg(target_vendor = "apple")]
    for end in [&a, &b] {
        assert_eq!(
            apple_test::fd_flags(end) & apple_test::FD_CLOFORK,
            if clofork_supported() {
                apple_test::FD_CLOFORK
            } else {
                0
            },
            "each end agrees with the kernel's supported flags"
        );
    }
    drop((a, b));
}

/// [`crate::accept`] waits for a connection (on a listener made blocking, as std makes one) and
/// answers it blocking and kept out of children, leaving the listener nonblocking: its wait is a
/// poll outside the lock, and only the nonblocking accept and the mark hold it.
#[cfg(unix)]
#[test]
fn accept_answers_a_blocking_kept_out_connection_once_one_comes() {
    use std::io::{Read as _, Write as _};
    use std::os::fd::AsRawFd as _;
    use std::os::unix::net::{UnixListener, UnixStream};
    let root = aterm_tempfile::tempdir().expect("a folder");
    let path = root.path().join("accept.sock");
    let listener = UnixListener::bind(&path).expect("bind");
    let server = std::thread::spawn(move || {
        let accepted = crate::accept(&listener).map(|(stream, _)| stream);
        (accepted, listener)
    });
    let mut client = UnixStream::connect(&path).expect("connect");
    let (accepted, listener) = server.join().expect("the server");
    let mut accepted = accepted.expect("a connection");
    // SAFETY: each `fcntl` reads one live test descriptor's flags and touches no memory.
    let (status, listening, descriptor) = unsafe {
        (
            libc::fcntl(accepted.as_raw_fd(), libc::F_GETFL),
            libc::fcntl(listener.as_raw_fd(), libc::F_GETFL),
            libc::fcntl(accepted.as_raw_fd(), libc::F_GETFD),
        )
    };
    assert!(
        status >= 0 && status & libc::O_NONBLOCK == 0,
        "a blocking connection"
    );
    assert!(
        listening >= 0 && listening & libc::O_NONBLOCK != 0,
        "a nonblocking listener"
    );
    #[cfg(target_vendor = "apple")]
    assert_eq!(
        descriptor & apple_test::FD_CLOFORK,
        if clofork_supported() {
            apple_test::FD_CLOFORK
        } else {
            0
        },
        "the accepted connection agrees with the kernel's supported flags"
    );
    #[cfg(not(target_vendor = "apple"))]
    assert!(descriptor >= 0);
    client.write_all(b"x").expect("write");
    let mut byte = [0];
    accepted.read_exact(&mut byte).expect("read");
    assert_eq!(&byte, b"x");
}

#[cfg(target_vendor = "apple")]
mod apple_test {
    use std::os::fd::{AsRawFd, RawFd};
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    pub(super) const FD_CLOFORK: i32 = 2;
    /// A wait for what must happen: a hang detector, never a latency budget.
    const HANG: Duration = Duration::from_secs(60);

    pub(super) fn fd_flags(fd: &impl AsRawFd) -> i32 {
        // SAFETY: `F_GETFD` reads one descriptor's flags and touches no memory.
        unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) }
    }

    /// A socketpair made between its creation and its mark, held there until `go`: through
    /// [`crate::kept_out`] when `guarded`, else created and then marked as every site did before
    /// it. Answers the first end's number once it exists.
    fn create_held(
        guarded: bool,
    ) -> (
        RawFd,
        mpsc::Sender<()>,
        JoinHandle<(UnixStream, UnixStream)>,
    ) {
        let (created_tx, created) = mpsc::channel();
        let (go, go_rx) = mpsc::channel::<()>();
        let creator = std::thread::spawn(move || {
            let make = || {
                let pair = UnixStream::pair()?;
                created_tx
                    .send(pair.0.as_raw_fd())
                    .expect("the test listens");
                go_rx
                    .recv_timeout(HANG)
                    .expect("the test lets the mark happen");
                Ok::<_, std::io::Error>(pair)
            };
            if guarded {
                crate::kept_out(make).expect("a kept-out pair")
            } else {
                let pair = make().expect("a pair");
                crate::keep_out_of_children(&pair.0).expect("mark");
                crate::keep_out_of_children(&pair.1).expect("mark");
                pair
            }
        });
        let fd = created.recv_timeout(HANG).expect("the pair is created");
        (fd, go, creator)
    }

    /// Fork a child under [`crate::spawn_lock::spawning`], as the PTY spawn does, and answer whether it
    /// holds descriptor `fd`: the child reports it itself, before it would exec.
    fn fork_holds(fd: RawFd) -> bool {
        let mut report = [0; 2];
        // SAFETY: `pipe` fills the two-element array it is handed.
        assert_eq!(unsafe { libc::pipe(report.as_mut_ptr()) }, 0, "report pipe");
        let spawning = crate::spawn_lock::spawning();
        // SAFETY: `fork` takes no arguments; the child calls only `fcntl`, `write` and `_exit`.
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            // SAFETY: async-signal-safe calls on integers and a stack byte, then `_exit`.
            unsafe {
                let held: u8 = u8::from(libc::fcntl(fd, libc::F_GETFD) >= 0);
                libc::write(report[1], std::ptr::addr_of!(held).cast(), 1);
                libc::_exit(0);
            }
        }
        drop(spawning);
        assert!(pid > 0, "fork: {}", std::io::Error::last_os_error());
        let mut held = 2u8;
        // SAFETY: both ends are this function's; the read fills one stack byte; `waitpid` reaps
        // the child this function forked.
        unsafe {
            libc::close(report[1]);
            assert_eq!(
                libc::read(report[0], std::ptr::addr_of_mut!(held).cast(), 1),
                1,
                "the child reports"
            );
            libc::close(report[0]);
            let mut status = 0;
            libc::waitpid(pid, &mut status, 0);
        }
        held == 1
    }

    /// A child forked during an unguarded creation holds a copy. A guarded creation delays
    /// the fork until the mark request completes; its inheritance then follows kernel support.
    #[test]
    fn a_child_waits_for_creation_and_matches_kernel_inheritance() {
        let (fd, go, creator) = create_held(false);
        assert!(
            fork_holds(fd),
            "the negative control: a child forked while the socket is unmarked holds it"
        );
        go.send(()).expect("the creator waits");
        drop(creator.join().expect("the creator"));

        let (fd, go, creator) = create_held(true);
        let spawner = std::thread::spawn(move || fork_holds(fd));
        await_queued(&spawner, creator.thread().id(), "a fork");
        go.send(()).expect("the creator waits");
        let pair = creator.join().expect("the creator");
        assert_eq!(
            spawner.join().expect("the spawner"),
            !super::clofork_supported(),
            "the child inherits the socket exactly when the kernel lacks fork exclusion"
        );
        drop(pair);
    }

    /// Every spawn path takes the lock: each, started while a creation is between its socket
    /// and its mark, waits for it.
    #[test]
    fn every_spawn_waits_for_a_creation_in_flight() {
        type Spawn = fn() -> std::io::Result<()>;
        let paths: [(&str, Spawn); 4] = [
            ("spawnfd::command", || {
                crate::spawnfd::command("/usr/bin/true").status().map(drop)
            }),
            ("spawnfd::transparent", || {
                crate::spawnfd::transparent("/usr/bin/true")
                    .status()
                    .map(drop)
            }),
            ("spawnfd::spawn", || {
                let mut lane = crate::spawnfd::LaneCommand::new("/usr/bin/true");
                lane.stdin(crate::spawnfd::ChildStdio::Null)
                    .stdout(crate::spawnfd::ChildStdio::Null)
                    .stderr(crate::spawnfd::ChildStdio::Null);
                crate::spawnfd::spawn(lane)?.wait().map(drop)
            }),
            ("pspawn::spawn_cloexec_default", || {
                let pid = crate::pspawn::spawn_cloexec_default("/usr/bin/true", &[])?;
                let mut status = 0;
                // SAFETY: reaps the child just spawned; `status` is a live local.
                unsafe { libc::waitpid(i32::try_from(pid).expect("a pid"), &mut status, 0) };
                Ok(())
            }),
        ];
        for (name, spawn) in paths {
            let (_fd, go, creator) = create_held(true);
            let spawner = std::thread::spawn(spawn);
            await_queued(&spawner, creator.thread().id(), name);
            go.send(()).expect("the creator waits");
            drop(creator.join().expect("the creator"));
            spawner
                .join()
                .expect("the spawner")
                .unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }

    /// A lane made through [`crate::kept_out`] reaches the child it is handed to: `posix_spawn`
    /// copies no `FD_CLOFORK` descriptor, so without the spawn's unmark the lane's `dup2` action
    /// answers `EBADF` and nothing starts (the negative control, run by hand).
    #[test]
    fn a_kept_out_lane_reaches_its_own_child() {
        use std::io::{Read as _, Write as _};
        let (mut near_verb, far_verb) = crate::kept_out(UnixStream::pair).expect("a verb lane");
        let (mut near_push, far_push) = crate::kept_out(UnixStream::pair).expect("a push lane");
        for end in [&far_verb, &far_push] {
            assert_eq!(
                fd_flags(end) & FD_CLOFORK,
                if super::clofork_supported() {
                    FD_CLOFORK
                } else {
                    0
                },
                "the lane agrees with the kernel's supported flags"
            );
        }
        let mut cmd = crate::spawnfd::LaneCommand::new("/bin/sh");
        cmd.args(["-c", "read line <&3; printf 'got:%s' \"$line\" >&4"])
            .stdin(crate::spawnfd::ChildStdio::Null)
            .stdout(crate::spawnfd::ChildStdio::Null)
            .stderr(crate::spawnfd::ChildStdio::Inherit);
        let mut child = crate::spawnfd::spawn_with_two_fds(cmd, far_verb.into(), far_push.into())
            .expect("the kept-out lanes reach their child");
        near_push
            .set_read_timeout(Some(HANG))
            .expect("a hang detector");
        near_verb.write_all(b"hello\n").expect("the verb lane");
        let mut got = String::new();
        near_push.read_to_string(&mut got).expect("the push lane");
        assert!(child.wait().expect("reap the child").success());
        assert_eq!(got, "got:hello", "the lanes reached the child");
    }

    /// A retained mark requires an exclusive hold while handing the descriptor to its own
    /// child; another fork waits for restoration. Without kernel support, ordinary inheritance
    /// remains available under the shared hold.
    #[test]
    fn a_handing_guard_restores_supported_marks_and_matches_kernel_inheritance() {
        let (lane, _peer) = crate::kept_out(UnixStream::pair).expect("a kept-out pair");
        let fd = lane.as_raw_fd();
        let handing = crate::spawn_lock::handing(&[fd]);
        assert_eq!(
            fd_flags(&lane) & FD_CLOFORK,
            0,
            "unmarked for its own child"
        );
        let spawner = std::thread::spawn(move || fork_holds(fd));
        if super::clofork_supported() {
            await_queued(
                &spawner,
                std::thread::current().id(),
                "a fork beside a spawn handing an unmarked lane",
            );
        }
        drop(handing);
        assert_eq!(
            fd_flags(&lane) & FD_CLOFORK,
            if super::clofork_supported() {
                FD_CLOFORK
            } else {
                0
            },
            "the original supported flags are restored"
        );
        assert_eq!(
            spawner.join().expect("the spawner"),
            !super::clofork_supported(),
            "fork inheritance agrees with kernel support after the handing guard drops"
        );
    }

    /// A spawn handing nothing marked takes the shared hold, as every other spawn does, so spawns
    /// never wait for each other; one handing a marked descriptor holds the lock exclusively.
    /// (Read off the guard: a test that held the shared hold across another thread's spawn could
    /// wait forever behind a creation queued by a test running beside it.)
    #[test]
    fn only_a_spawn_handing_a_marked_descriptor_holds_the_lock_exclusively() {
        let (plain, _peer) = UnixStream::pair().expect("a pair");
        let shared = crate::spawn_lock::handing(&[plain.as_raw_fd()]);
        assert!(shared._shared.is_some() && shared._exclusive.is_none());
        assert!(shared.unmarked.is_empty(), "nothing to unmark");
        drop(shared);
        let (kept, _peer) = crate::kept_out(UnixStream::pair).expect("a kept-out pair");
        let exclusive = crate::spawn_lock::handing(&[plain.as_raw_fd(), kept.as_raw_fd()]);
        let supported = super::clofork_supported();
        assert_eq!(exclusive._exclusive.is_some(), supported);
        assert_eq!(exclusive._shared.is_some(), !supported);
        assert_eq!(
            exclusive.unmarked.len(),
            usize::from(supported),
            "only the marked one is unmarked"
        );
        drop(exclusive);
        assert_eq!(
            fd_flags(&kept) & FD_CLOFORK,
            if supported { FD_CLOFORK } else { 0 },
            "the original supported flags are restored"
        );
        assert_eq!(
            fd_flags(&plain) & FD_CLOFORK,
            0,
            "the unmarked one left as it was"
        );
    }

    /// Wait until `spawner` waits in [`crate::spawn_lock::spawning`] while `holder` holds the lock
    /// exclusively; fail at once if it ended first (it spawned while a socket was unmarked).
    /// Naming the holder keeps the check this test's own: in a parallel run another test's
    /// creation can hold the lock, and a spawn queued behind it says nothing of this one's.
    fn await_queued<T>(spawner: &JoinHandle<T>, holder: std::thread::ThreadId, what: &str) {
        use crate::spawn_lock::seam::{exclusive_holder, spawn_waits};
        let started = Instant::now();
        while !(spawn_waits(spawner.thread().id()) && exclusive_holder() == Some(holder)) {
            assert!(
                !spawner.is_finished(),
                "{what} spawned while a kept-out socket was unmarked"
            );
            assert!(started.elapsed() < HANG, "{what} never reached the lock");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
