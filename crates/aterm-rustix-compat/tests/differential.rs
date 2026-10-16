// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Each function beside rustix 1.1.4's, on a live Linux kernel.
//!
//! Neither side is trusted to describe itself: every comparison reads back
//! what the KERNEL then reports about the descriptor or the call (close-on-exec,
//! status flags, seals, bytes, readiness, errno, ids), through rustix 1.1.4
//! wherever a reader is needed, so a facade call that silently did nothing
//! shows up as a state the oracle's own call does not produce. Every test also
//! holds a state change in both directions (on/off, data/empty, ok/error), so
//! two calls that both failed to act cannot agree their way to a pass.
//!
//! The consumers' own call shapes are used where they exist: calloop's
//! `set_nonblocking` and eventfd ping, sctk's memfd and shm fallback, winit's
//! two-descriptor poll, `is_main_thread` and `_NET_WM_PID`/`WM_CLIENT_MACHINE`.

#![cfg(target_os = "linux")]

use std::ffi::CStr;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use rustix1::io::FdFlags;

/// A wait for a call that must return: a hang detector, never a latency
/// budget (AGENTS.md).
const HANG: Duration = Duration::from_secs(60);

/// Close-on-exec, as the kernel reports it.
fn cloexec(fd: impl AsFd) -> bool {
    rustix1::io::fcntl_getfd(fd)
        .expect("F_GETFD")
        .contains(FdFlags::CLOEXEC)
}

/// The file status and access-mode flags, as the kernel reports them.
fn status(fd: impl AsFd) -> u32 {
    rustix1::fs::fcntl_getfl(fd).expect("F_GETFL").bits()
}

/// What a consumer can observe about a fresh descriptor.
fn shape(fd: impl AsFd) -> (bool, u32) {
    let fd = fd.as_fd();
    (cloexec(fd), status(fd))
}

fn nonblocking(fd: impl AsFd) -> bool {
    status(fd) & rustix1::fs::OFlags::NONBLOCK.bits() != 0
}

/// The seals on a memfd, as the kernel reports them.
fn seals(fd: impl AsFd) -> u32 {
    rustix1::fs::fcntl_get_seals(fd)
        .expect("F_GET_SEALS")
        .bits()
}

/// What `/proc/self/fd/<n>` names: for a memfd, `/memfd:<name> (deleted)`.
fn proc_name(fd: impl AsFd) -> String {
    let link = format!("/proc/self/fd/{}", fd.as_fd().as_raw_fd());
    std::fs::read_link(link)
        .expect("readlink /proc/self/fd")
        .to_string_lossy()
        .into_owned()
}

/// Two results agree: both `Ok` (compared by `same`), or the same errno.
fn agree<A, B>(
    ours: &Result<A, rustix::io::Errno>,
    theirs: &Result<B, rustix1::io::Errno>,
    what: &str,
    same: impl FnOnce(&A, &B) -> bool,
) {
    match (ours, theirs) {
        (Ok(a), Ok(b)) => assert!(same(a, b), "{what}: both succeeded, differently"),
        (Err(a), Err(b)) => assert_eq!(a.raw_os_error(), b.raw_os_error(), "{what}: errno"),
        (Ok(_), Err(b)) => panic!("{what}: ours succeeded, rustix 1.1.4 failed with {b:?}"),
        (Err(a), Ok(_)) => panic!("{what}: ours failed with {a:?}, rustix 1.1.4 succeeded"),
    }
}

/// Both calls failed, with the same errno; returns ours.
fn same_error<A: std::fmt::Debug, B: std::fmt::Debug>(
    ours: Result<A, rustix::io::Errno>,
    theirs: Result<B, rustix1::io::Errno>,
    what: &str,
) -> rustix::io::Errno {
    match (ours, theirs) {
        (Err(a), Err(b)) => {
            assert_eq!(a.raw_os_error(), b.raw_os_error(), "{what}: errno");
            a
        }
        (a, b) => panic!("{what}: expected two failures, got {a:?} and {b:?}"),
    }
}

#[test]
fn errno_values_conversions_and_text_match() {
    use rustix::io::Errno as Ours;
    use rustix1::io::Errno as Theirs;
    let pairs = [
        (Ours::AGAIN, Theirs::AGAIN),
        (Ours::WOULDBLOCK, Theirs::WOULDBLOCK),
        (Ours::EXIST, Theirs::EXIST),
        (Ours::INTR, Theirs::INTR),
        (Ours::INVAL, Theirs::INVAL),
        (Ours::NAMETOOLONG, Theirs::NAMETOOLONG),
        (Ours::NOSYS, Theirs::NOSYS),
        (Ours::PROTO, Theirs::PROTO),
    ];
    for (ours, theirs) in pairs {
        assert_eq!(ours.raw_os_error(), theirs.raw_os_error(), "{theirs:?}");
        assert_eq!(ours.kind(), theirs.kind(), "{theirs:?}");
        assert_eq!(ours.to_string(), theirs.to_string());
        assert_eq!(format!("{ours:?}"), format!("{theirs:?}"));
        // calloop and sctk's `e.into()` / `Errno::PROTO.into()`.
        let io: std::io::Error = ours.into();
        assert_eq!(io.raw_os_error(), Some(theirs.raw_os_error()));
        assert_eq!(Ours::from_io_error(&io), Some(ours));
        assert_eq!(Ours::from_raw_os_error(theirs.raw_os_error()), ours);
    }
    // The named values are distinct where the kernel's are (AGAIN and
    // WOULDBLOCK are one value on Linux, for both).
    assert_eq!(Ours::AGAIN, Ours::WOULDBLOCK);
    assert_ne!(Ours::AGAIN, Ours::INTR);
    assert_ne!(Ours::EXIST, Ours::NOSYS);
    assert_ne!(Ours::PROTO, Ours::INVAL);
    // Not an OS error: neither has an errno for it.
    let other = std::io::Error::other("not an os error");
    assert_eq!(Ours::from_io_error(&other), None);
    assert_eq!(Theirs::from_io_error(&other), None);
}

#[test]
fn read_and_write_move_the_same_bytes() {
    use rustix1::pipe::PipeFlags;
    let (r, w) = rustix1::pipe::pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK).unwrap();
    let mut buf = [0u8; 16];

    // Ours writes, theirs reads; then the other way round.
    assert_eq!(rustix::io::write(&w, b"ours"), Ok(4));
    assert_eq!(rustix1::io::read(&r, &mut buf[..]).unwrap(), 4);
    assert_eq!(&buf[..4], b"ours");
    assert_eq!(rustix1::io::write(&w, b"theirs!").unwrap(), 7);
    assert_eq!(rustix::io::read(&r, &mut buf[..]), Ok(7));
    assert_eq!(&buf[..7], b"theirs!");

    // A short buffer takes only what fits, and the rest stays queued.
    assert_eq!(rustix::io::write(&w, b"0123456789"), Ok(10));
    assert_eq!(rustix::io::read(&r, &mut buf[..3]), Ok(3));
    assert_eq!(&buf[..3], b"012");
    assert_eq!(rustix1::io::read(&r, &mut buf[..]).unwrap(), 7);
    assert_eq!(&buf[..7], b"3456789");

    // Empty and non-blocking: AGAIN from both (calloop's drain loop).
    let ours = rustix::io::read(&r, &mut buf[..]);
    let theirs = rustix1::io::read(&r, &mut buf[..]);
    let e = same_error(ours, theirs, "read of an empty pipe");
    assert_eq!(e, rustix::io::Errno::AGAIN);

    // A closed write end is end of file for both.
    drop(w);
    assert_eq!(rustix::io::read(&r, &mut buf[..]), Ok(0));
    assert_eq!(rustix1::io::read(&r, &mut buf[..]).unwrap(), 0);

    // A closed read end: EPIPE from both (the test harness ignores SIGPIPE,
    // as every Rust program does).
    let (r, w) = rustix1::pipe::pipe_with(PipeFlags::CLOEXEC).unwrap();
    drop(r);
    let ours = rustix::io::write(&w, b"x");
    let theirs = rustix1::io::write(&w, b"x");
    let e = same_error(ours, theirs, "write to a widowed pipe");
    assert_eq!(e.raw_os_error(), rustix1::io::Errno::PIPE.raw_os_error());
}

#[test]
fn ioctl_fionbio_sets_what_fcntl_reads_back() {
    let (r, _w) = rustix1::pipe::pipe().unwrap();
    assert!(!nonblocking(&r));
    rustix::io::ioctl_fionbio(&r, true).unwrap();
    assert!(nonblocking(&r));
    rustix::io::ioctl_fionbio(&r, false).unwrap();
    assert!(!nonblocking(&r));

    // Its twin through rustix 1.1.4 lands in the same state.
    let (t, _tw) = rustix1::pipe::pipe().unwrap();
    rustix1::io::ioctl_fionbio(&t, true).unwrap();
    rustix::io::ioctl_fionbio(&r, true).unwrap();
    assert_eq!(status(&r), status(&t));
}

#[test]
fn fcntl_getfl_and_setfl_match_on_every_kind_of_descriptor() {
    use rustix::fs::OFlags;
    let (r, w) = rustix1::pipe::pipe().unwrap();
    let memfd =
        rustix1::fs::memfd_create(c"aterm-rustix-getfl", rustix1::fs::MemfdFlags::CLOEXEC).unwrap();
    let event = rustix1::event::eventfd(0, rustix1::event::EventfdFlags::NONBLOCK).unwrap();
    for fd in [r.as_fd(), w.as_fd(), memfd.as_fd(), event.as_fd()] {
        let ours = rustix::fs::fcntl_getfl(fd).map(|f| f.bits());
        let theirs = rustix1::fs::fcntl_getfl(fd).map(|f| f.bits());
        agree(&ours, &theirs, "F_GETFL", |a, b| a == b);
    }
    // The access mode is in there: the write end reads WRONLY, the read end not.
    assert!(
        rustix::fs::fcntl_getfl(&w)
            .unwrap()
            .contains(OFlags::WRONLY)
    );
    assert!(
        !rustix::fs::fcntl_getfl(&r)
            .unwrap()
            .contains(OFlags::WRONLY)
    );

    // calloop's set_nonblocking, on twins: ours on one, rustix 1.1.4 on the other.
    let (a, _aw) = rustix1::pipe::pipe().unwrap();
    let (b, _bw) = rustix1::pipe::pipe().unwrap();
    for on in [true, false, true] {
        let previous = rustix::fs::fcntl_getfl(&a).unwrap();
        let new = if on {
            previous | OFlags::NONBLOCK
        } else {
            previous & !OFlags::NONBLOCK
        };
        if new != previous {
            rustix::fs::fcntl_setfl(&a, new).unwrap();
        }
        let previous = rustix1::fs::fcntl_getfl(&b).unwrap();
        let new = if on {
            previous | rustix1::fs::OFlags::NONBLOCK
        } else {
            previous & !rustix1::fs::OFlags::NONBLOCK
        };
        rustix1::fs::fcntl_setfl(&b, new).unwrap();
        assert_eq!(status(&a), status(&b));
        assert_eq!(nonblocking(&a), on);
        assert_eq!(
            rustix::fs::fcntl_getfl(&a)
                .unwrap()
                .contains(OFlags::NONBLOCK),
            on
        );
    }
    // APPEND is settable too, and lands the same.
    rustix::fs::fcntl_setfl(&a, OFlags::APPEND).unwrap();
    rustix1::fs::fcntl_setfl(&b, rustix1::fs::OFlags::APPEND).unwrap();
    assert_eq!(status(&a), status(&b));
    assert!(!nonblocking(&a));
}

#[test]
fn memfd_create_and_seals_match() {
    use rustix::fs::{MemfdFlags, SealFlags};
    use rustix1::fs::{MemfdFlags as TheirMemfd, SealFlags as TheirSeal};

    // sctk's call: a `&CStr` name, ALLOW_SEALING | CLOEXEC.
    let name: &CStr = c"smithay-client-toolkit";
    let ours = rustix::fs::memfd_create(name, MemfdFlags::ALLOW_SEALING | MemfdFlags::CLOEXEC);
    let theirs = rustix1::fs::memfd_create(name, TheirMemfd::ALLOW_SEALING | TheirMemfd::CLOEXEC);
    let (ours, theirs) = (ours.unwrap(), theirs.unwrap());
    assert_eq!(shape(&ours), shape(&theirs));
    assert!(cloexec(&ours));
    assert_eq!(proc_name(&ours), proc_name(&theirs));
    assert_eq!(proc_name(&ours), "/memfd:smithay-client-toolkit (deleted)");
    assert_eq!(seals(&ours), seals(&theirs));
    assert_eq!(seals(&ours), 0);

    // sctk's seals, then one more after F_SEAL_SEAL: refused by both.
    rustix::fs::fcntl_add_seals(&ours, SealFlags::SHRINK | SealFlags::SEAL).unwrap();
    rustix1::fs::fcntl_add_seals(&theirs, TheirSeal::SHRINK | TheirSeal::SEAL).unwrap();
    assert_eq!(seals(&ours), seals(&theirs));
    assert_eq!(seals(&ours), (TheirSeal::SHRINK | TheirSeal::SEAL).bits());
    let a = rustix::fs::fcntl_add_seals(&ours, SealFlags::GROW);
    let b = rustix1::fs::fcntl_add_seals(&theirs, TheirSeal::GROW);
    let e = same_error(a, b, "add_seals after SEAL");
    assert_eq!(e.raw_os_error(), rustix1::io::Errno::PERM.raw_os_error());

    // Without ALLOW_SEALING the file starts sealed against sealing.
    let ours = rustix::fs::memfd_create("aterm-rustix-plain", MemfdFlags::empty()).unwrap();
    let theirs = rustix1::fs::memfd_create("aterm-rustix-plain", TheirMemfd::empty()).unwrap();
    assert_eq!(shape(&ours), shape(&theirs));
    assert!(!cloexec(&ours));
    assert_eq!(seals(&ours), seals(&theirs));
    assert_eq!(seals(&ours), TheirSeal::SEAL.bits());

    // A `&str` name, and a name with an interior NUL: INVAL from both.
    let ours = rustix::fs::memfd_create("aterm\0rustix", MemfdFlags::CLOEXEC);
    let theirs = rustix1::fs::memfd_create("aterm\0rustix", TheirMemfd::CLOEXEC);
    let e = same_error(ours, theirs, "interior NUL");
    assert_eq!(e, rustix::io::Errno::INVAL);
}

#[test]
fn pipe_and_pipe_with_make_the_same_descriptors() {
    use rustix::pipe::PipeFlags;
    use rustix1::pipe::PipeFlags as Theirs;
    let cases = [
        (PipeFlags::empty(), Theirs::empty()),
        (PipeFlags::CLOEXEC, Theirs::CLOEXEC),
        (PipeFlags::NONBLOCK, Theirs::NONBLOCK),
        (
            PipeFlags::CLOEXEC | PipeFlags::NONBLOCK,
            Theirs::CLOEXEC | Theirs::NONBLOCK,
        ),
        (PipeFlags::DIRECT, Theirs::DIRECT),
    ];
    let mut shapes = Vec::new();
    for (ours, theirs) in cases {
        let o = rustix::pipe::pipe_with(ours);
        let t = rustix1::pipe::pipe_with(theirs);
        agree(&o, &t, "pipe2", |(or, ow), (tr, tw)| {
            shape(or) == shape(tr) && shape(ow) == shape(tw)
        });
        if let Ok((r, w)) = o {
            // The ends are the right way round and connected.
            assert_eq!(rustix1::io::write(&w, b"p").unwrap(), 1);
            let mut b = [0u8; 1];
            assert_eq!(rustix1::io::read(&r, &mut b[..]).unwrap(), 1);
            shapes.push(shape(&r));
        }
    }
    // The flags reached the kernel: four different read-end shapes at least.
    shapes.sort_unstable();
    shapes.dedup();
    assert!(shapes.len() >= 4, "pipe2 flags had no effect: {shapes:?}");

    // `pipe()`: no flags at all, for both.
    let (or, ow) = rustix::pipe::pipe().unwrap();
    let (tr, tw) = rustix1::pipe::pipe().unwrap();
    assert_eq!(shape(&or), shape(&tr));
    assert_eq!(shape(&ow), shape(&tw));
    assert!(!cloexec(&or) && !nonblocking(&or));
}

#[test]
fn eventfd_counts_and_flags_match() {
    use rustix::event::EventfdFlags;
    use rustix1::event::EventfdFlags as Theirs;

    // calloop's Linux ping: eventfd(0, CLOEXEC | NONBLOCK).
    let ours = rustix::event::eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK).unwrap();
    let theirs = rustix1::event::eventfd(0, Theirs::CLOEXEC | Theirs::NONBLOCK).unwrap();
    assert_eq!(shape(&ours), shape(&theirs));
    assert!(cloexec(&ours) && nonblocking(&ours));

    // Empty: AGAIN from both. Then calloop's increments (2 = ping, 1 = close)
    // sum in the counter, and one 8-byte read drains it.
    let mut buf = [0u8; 8];
    let a = rustix::io::read(&ours, &mut buf);
    let b = rustix1::io::read(&theirs, &mut buf[..]);
    let e = same_error(a, b, "read of an empty eventfd");
    assert_eq!(e, rustix::io::Errno::AGAIN);
    assert_eq!(rustix::io::write(&ours, &2u64.to_ne_bytes()), Ok(8));
    assert_eq!(rustix::io::write(&ours, &1u64.to_ne_bytes()), Ok(8));
    assert_eq!(rustix::io::read(&ours, &mut buf), Ok(8));
    assert_eq!(u64::from_ne_bytes(buf), 3);

    // The initial value is the counter's.
    let ours = rustix::event::eventfd(5, EventfdFlags::NONBLOCK).unwrap();
    let theirs = rustix1::event::eventfd(5, Theirs::NONBLOCK).unwrap();
    assert_eq!(shape(&ours), shape(&theirs));
    assert!(!cloexec(&ours));
    assert_eq!(rustix1::io::read(&ours, &mut buf[..]).unwrap(), 8);
    assert_eq!(u64::from_ne_bytes(buf), 5);

    // SEMAPHORE: each read takes one.
    let ours = rustix::event::eventfd(2, EventfdFlags::SEMAPHORE | EventfdFlags::NONBLOCK).unwrap();
    let theirs = rustix1::event::eventfd(2, Theirs::SEMAPHORE | Theirs::NONBLOCK).unwrap();
    for _ in 0..2 {
        assert_eq!(rustix::io::read(&ours, &mut buf), Ok(8));
        assert_eq!(u64::from_ne_bytes(buf), 1);
        assert_eq!(rustix1::io::read(&theirs, &mut buf[..]).unwrap(), 8);
        assert_eq!(u64::from_ne_bytes(buf), 1);
    }
    let a = rustix::io::read(&ours, &mut buf);
    let b = rustix1::io::read(&theirs, &mut buf[..]);
    let e = same_error(a, b, "read of a drained semaphore");
    assert_eq!(e, rustix::io::Errno::AGAIN);
}

#[test]
fn poll_reports_the_same_readiness() {
    use rustix::event::{PollFd, PollFlags};
    use rustix1::event::{PollFd as TheirFd, PollFlags as TheirFlags, Timespec};
    let now = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let (r, w) = rustix1::pipe::pipe_with(rustix1::pipe::PipeFlags::CLOEXEC).unwrap();
    let (idle, _idle_w) = rustix1::pipe::pipe_with(rustix1::pipe::PipeFlags::CLOEXEC).unwrap();

    // Nothing to read: 0 from both, and no events.
    let mut ours = [PollFd::new(&r, PollFlags::IN)];
    let mut theirs = [TheirFd::new(&r, TheirFlags::IN)];
    assert_eq!(rustix::event::poll(&mut ours, 0), Ok(0));
    assert_eq!(rustix1::event::poll(&mut theirs, Some(&now)).unwrap(), 0);
    assert!(ours[0].revents().is_empty());

    // A byte: IN from both.
    rustix1::io::write(&w, b"!").unwrap();
    assert_eq!(rustix::event::poll(&mut ours, 0), Ok(1));
    assert_eq!(rustix1::event::poll(&mut theirs, Some(&now)).unwrap(), 1);
    assert_eq!(ours[0].revents().bits(), theirs[0].revents().bits());
    assert!(ours[0].revents().contains(PollFlags::IN));
    ours[0].clear_revents();
    assert!(ours[0].revents().is_empty());

    // winit's shape: two borrowed descriptors and an infinite timeout, which
    // returns at once because one of them is ready. It runs on its own thread
    // over duplicates of the two read ends (the same open pipes, so the same
    // readiness), so a poll that never returns fails here after HANG instead
    // of wedging the run.
    let (idle_dup, r_dup) = (idle.try_clone().unwrap(), r.try_clone().unwrap());
    let (done, outcome) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut ours = [
            PollFd::from_borrowed_fd(idle_dup.as_fd(), PollFlags::IN),
            PollFd::from_borrowed_fd(r_dup.as_fd(), PollFlags::IN),
        ];
        let n = rustix::event::poll(&mut ours, -1);
        let fd_kept = ours[1].as_fd().as_raw_fd() == r_dup.as_raw_fd();
        let _ = done.send((n, ours[0].revents(), ours[1].revents(), fd_kept));
    });
    let (n, idle_events, ready_events, fd_kept) = outcome
        .recv_timeout(HANG)
        .expect("poll(-1) with a ready descriptor never returned");
    assert_eq!(n, Ok(1));
    assert!(idle_events.is_empty());
    assert!(ready_events.contains(PollFlags::IN));
    assert!(fd_kept);

    // Drained and widowed: HUP, the same bits from both.
    let mut b = [0u8; 1];
    rustix1::io::read(&r, &mut b[..]).unwrap();
    drop(w);
    let mut ours = [PollFd::new(&r, PollFlags::IN)];
    let mut theirs = [TheirFd::new(&r, TheirFlags::IN)];
    assert_eq!(rustix::event::poll(&mut ours, 0), Ok(1));
    assert_eq!(rustix1::event::poll(&mut theirs, Some(&now)).unwrap(), 1);
    assert_eq!(ours[0].revents().bits(), theirs[0].revents().bits());
    assert!(ours[0].revents().contains(PollFlags::HUP));

    // A finite timeout on an idle descriptor expires with 0.
    let mut ours = [PollFd::new(&idle, PollFlags::IN)];
    assert_eq!(rustix::event::poll(&mut ours, 10), Ok(0));
}

/// A shm name no other test or process uses.
fn shm_name(tag: &str) -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    format!("/aterm-rustix-{tag}-{}-{n}", std::process::id())
}

/// Armed only for a name this test created exclusively; the oracle removes
/// it on a panic too, even if the facade's unlink is the broken operation.
struct UnlinkShmOnDrop<'a>(&'a str);

impl Drop for UnlinkShmOnDrop<'_> {
    fn drop(&mut self) {
        let _ = rustix1::shm::unlink(self.0);
    }
}

#[test]
fn shm_open_and_unlink_match() {
    use rustix::shm::{Mode, ShmOFlags};
    use rustix1::fs::Mode as TheirMode;
    use rustix1::shm::OFlags as Theirs;

    // sctk's call: CREATE | EXCL | RDWR, RUSR | WUSR.
    let name = shm_name("open");
    let flags = ShmOFlags::CREATE | ShmOFlags::EXCL | ShmOFlags::RDWR;
    let ours = rustix::shm::shm_open(name.as_str(), flags, Mode::RUSR | Mode::WUSR).unwrap();
    let _cleanup = UnlinkShmOnDrop(name.as_str());
    assert!(cloexec(&ours));

    // rustix 1.1.4 opens the same object and sees what ours wrote.
    let theirs = rustix1::shm::open(name.as_str(), Theirs::RDWR, TheirMode::empty()).unwrap();
    assert_eq!(shape(&ours), shape(&theirs));
    File::from(ours.try_clone().unwrap())
        .write_all(b"shared")
        .unwrap();
    let mut text = String::new();
    File::from(theirs).read_to_string(&mut text).unwrap();
    assert_eq!(text, "shared");
    let mode = File::from(ours).metadata().unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);

    // EXCL again: EXIST from both.
    let a = rustix::shm::shm_open(name.as_str(), flags, Mode::RUSR | Mode::WUSR);
    let b = rustix1::shm::open(
        name.as_str(),
        Theirs::CREATE | Theirs::EXCL | Theirs::RDWR,
        TheirMode::RUSR | TheirMode::WUSR,
    );
    let e = same_error(a, b, "second exclusive create");
    assert_eq!(e, rustix::io::Errno::EXIST);

    // Unlinked by ours: gone for both.
    rustix::shm::shm_unlink(name.as_str()).unwrap();
    let a = rustix::shm::shm_open(name.as_str(), ShmOFlags::RDWR, Mode::empty());
    let b = rustix1::shm::open(name.as_str(), Theirs::RDWR, TheirMode::empty());
    let e = same_error(a, b, "open after unlink");
    assert_eq!(e.raw_os_error(), rustix1::io::Errno::NOENT.raw_os_error());
    let a = rustix::shm::shm_unlink(name.as_str());
    let b = rustix1::shm::unlink(name.as_str());
    let e = same_error(a, b, "second unlink");
    assert_eq!(e.raw_os_error(), rustix1::io::Errno::NOENT.raw_os_error());

    // The longest name both accept (slash plus a 254-byte component), and
    // one byte longer. Keep the PID/serial prefix so independent test
    // processes never contend for the same exclusive creation.
    let prefix = shm_name("long");
    let long = format!("{prefix}{}", "a".repeat(255 - prefix.len()));
    let a = rustix::shm::shm_open(long.as_str(), flags, Mode::RUSR | Mode::WUSR);
    assert!(a.is_ok(), "{a:?}");
    let _cleanup_long = UnlinkShmOnDrop(long.as_str());
    rustix1::shm::unlink(long.as_str()).unwrap();
    let longer = format!("{long}a");
    let a = rustix::shm::shm_open(longer.as_str(), flags, Mode::RUSR | Mode::WUSR);
    let b = rustix1::shm::open(
        longer.as_str(),
        Theirs::CREATE | Theirs::EXCL | Theirs::RDWR,
        TheirMode::RUSR | TheirMode::WUSR,
    );
    // A broken length check may create the object before the assertion
    // catches it. Clean up only if one exclusive creation succeeded.
    let _cleanup_longer = (a.is_ok() || b.is_ok()).then(|| UnlinkShmOnDrop(longer.as_str()));
    let e = same_error(a, b, "256-byte name");
    assert_eq!(e, rustix::io::Errno::NAMETOOLONG);

    // Names upstream refuses before the kernel sees them: INVAL from both.
    for bad in ["", "/", "//", ".", "/.", "..", "/..", "a/b", "/a/", "a\0b"] {
        let a = rustix::shm::shm_open(bad, flags, Mode::RUSR);
        let b = rustix1::shm::open(
            bad,
            Theirs::CREATE | Theirs::EXCL | Theirs::RDWR,
            TheirMode::RUSR,
        );
        assert_eq!(same_error(a, b, bad), rustix::io::Errno::INVAL, "{bad:?}");
        let a = rustix::shm::shm_unlink(bad);
        let b = rustix1::shm::unlink(bad);
        assert_eq!(same_error(a, b, bad), rustix::io::Errno::INVAL, "{bad:?}");
    }
}

#[test]
fn process_and_thread_ids_match() {
    let ours = rustix::process::getpid();
    assert_eq!(
        ours.as_raw_nonzero(),
        rustix1::process::getpid().as_raw_nonzero()
    );
    assert_eq!(
        u32::try_from(ours.as_raw_nonzero().get()).unwrap(),
        std::process::id()
    );
    let tid = rustix::thread::gettid();
    assert_eq!(
        tid.as_raw_nonzero(),
        rustix1::thread::gettid().as_raw_nonzero()
    );

    // winit's is_main_thread on a thread that is not the main one: false, and
    // its id is its own.
    let (o, t) = std::thread::spawn(|| (rustix::thread::gettid(), rustix1::thread::gettid()))
        .join()
        .unwrap();
    assert_eq!(o.as_raw_nonzero(), t.as_raw_nonzero());
    assert_ne!(o, rustix::process::getpid());
    assert_ne!(o, tid);

    // The constructors.
    use rustix::process::Pid;
    assert_eq!(Pid::from_raw(0), None);
    assert_eq!(Pid::from_raw(-1), None);
    assert_eq!(Pid::from_raw(1), Some(Pid::INIT));
    assert!(Pid::INIT.is_init());
    assert_eq!(ours.is_init(), ours.as_raw_nonzero().get() == 1);
    assert_eq!(Pid::as_raw(None), 0);
    assert_eq!(Pid::as_raw(Some(ours)), ours.as_raw_nonzero().get());
}

#[test]
fn uname_matches() {
    let ours = rustix::system::uname();
    let theirs = rustix1::system::uname();
    assert_eq!(ours.sysname(), theirs.sysname());
    assert_eq!(ours.nodename(), theirs.nodename());
    assert_eq!(ours.release(), theirs.release());
    assert_eq!(ours.version(), theirs.version());
    assert_eq!(ours.machine(), theirs.machine());
    assert_eq!(ours.domainname(), theirs.domainname());
    assert_eq!(format!("{ours:?}"), format!("{theirs:?}"));
    // Not all empty: the kernel filled it in.
    assert_eq!(ours.sysname().to_bytes(), b"Linux");
    assert!(!ours.release().to_bytes().is_empty());
}

#[test]
fn flag_operators_match_bitflags() {
    use rustix::fs::OFlags;
    use rustix1::fs::OFlags as Theirs;
    let raw = [
        0,
        Theirs::NONBLOCK.bits(),
        (Theirs::NONBLOCK | Theirs::CLOEXEC).bits(),
        Theirs::RDWR.bits() | Theirs::APPEND.bits(),
        0xdead_beef,
        u32::MAX,
    ];
    for &x in &raw {
        let (o, t) = (OFlags::from_bits_retain(x), Theirs::from_bits_retain(x));
        assert_eq!(
            OFlags::from_bits(x).map(|f| f.bits()),
            Theirs::from_bits(x).map(|f| f.bits())
        );
        assert_eq!(
            OFlags::from_bits_truncate(x).bits(),
            Theirs::from_bits_truncate(x).bits()
        );
        assert_eq!((!o).bits(), (!t).bits(), "!{x:#x}");
        assert_eq!(o.is_empty(), t.is_empty());
        for &y in &raw {
            let (p, u) = (OFlags::from_bits_retain(y), Theirs::from_bits_retain(y));
            assert_eq!((o | p).bits(), (t | u).bits());
            assert_eq!((o & p).bits(), (t & u).bits());
            assert_eq!((o - p).bits(), (t - u).bits());
            assert_eq!(o.contains(p), t.contains(u), "{x:#x} contains {y:#x}");
            assert_eq!(o.intersects(p), t.intersects(u));
            let (mut m, mut n) = (o, t);
            m.insert(p);
            n.insert(u);
            assert_eq!(m.bits(), n.bits());
            m.remove(p);
            n.remove(u);
            assert_eq!(m.bits(), n.bits());
        }
    }
    assert_eq!(OFlags::empty().bits(), Theirs::empty().bits());
    assert_eq!(OFlags::all().bits(), Theirs::all().bits());
}
