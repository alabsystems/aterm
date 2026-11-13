// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-uds: the portable stream type behind aterm's control socket, plus the platform helpers the control channel needs (the `latest` alias, CSPRNG bytes, pid liveness and, on Unix, `SCM_RIGHTS` descriptor passing authenticated by the peer pid).

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]
// Trust's std hands a spawned child its stdio and nothing else; a pass-through command
// (`spawnfd::transparent`) asks for upstream's inheritance back where std spawns it (`build.rs`).
#![cfg_attr(
    all(trust_std, unix, not(target_vendor = "apple")),
    feature(trust_process_inherit_fds)
)]
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_types,
        reason = "test code: a stand-in child may inherit descriptors"
    )
)]

#[cfg(unix)]
pub type CtlStream = std::os::unix::net::UnixStream;
#[cfg(unix)]
pub type CtlListener = std::os::unix::net::UnixListener;

/// Unix-domain and TCP connections bounded by a deadline, each socket kept out of children from
/// its creation ([`kept_out`]) with no spawn held back for the connect.
#[cfg(unix)]
pub mod connect;

/// Connect to `addr` within `timeout`, as `TcpStream::connect_timeout` does: every TCP dial of
/// aterm's, refused before any socket is made where a debug build's partition cut lists `addr`
/// ([`partition::check`]). On Unix the socket is kept out of every child from its creation, with
/// no spawn held back for the connect ([`connect::tcp_within`]).
///
/// # Errors
/// As [`connect::tcp_within`].
#[cfg(unix)]
pub fn connect_tcp(
    addr: &std::net::SocketAddr,
    timeout: std::time::Duration,
) -> std::io::Result<std::net::TcpStream> {
    connect::tcp_within(addr, timeout)
}

/// Elsewhere: the partition cut, then `TcpStream::connect_timeout`, as a socket reaches a child
/// only when it is made inheritable.
///
/// # Errors
/// The cut (`HostUnreachable`), or `TcpStream::connect_timeout`'s.
#[cfg(not(unix))]
pub fn connect_tcp(
    addr: &std::net::SocketAddr,
    timeout: std::time::Duration,
) -> std::io::Result<std::net::TcpStream> {
    partition::check(*addr)?;
    std::net::TcpStream::connect_timeout(addr, timeout)
}

/// Connect with an owned caller's stop. On Unix, every pending socket and broker exchange
/// waits beside the stop bell; cancellation closes its local owned descriptors. Elsewhere
/// the existing bounded connect is checked before and after, without instantaneous cancellation.
/// # Errors
/// As [`connect_tcp`], or `Interrupted` when the stop is set.
pub fn connect_tcp_stopped(
    addr: &std::net::SocketAddr,
    timeout: std::time::Duration,
    stop: &stop::Stop,
) -> std::io::Result<std::net::TcpStream> {
    #[cfg(unix)]
    {
        connect::tcp_within_stopped(addr, timeout, stop)
    }
    #[cfg(not(unix))]
    {
        let check = || {
            if stop.is_set() {
                Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "socket preparation cancelled",
                ))
            } else {
                Ok(())
            }
        };
        check()?;
        let connected = connect_tcp(addr, timeout);
        check()?;
        connected
    }
}

/// Request that a supporting kernel exclude a socket from forked children (`FD_CLOFORK`).
///
/// [`kept_out`] makes this request before a concurrent spawn can copy the descriptor table.
/// Some Apple kernels accept but ignore the flag: this best-effort wrapper does not establish
/// fork exclusion there. The spawn's close-on-exec policy remains necessary. Callers requiring
/// an observed capability result can use `astream_sys::close_on_fork` directly. Off Apple the
/// request does nothing.
///
/// # Errors
/// The `fcntl(2)` errno of reading or setting the descriptor's flags.
#[cfg(unix)]
pub fn keep_out_of_children(socket: &impl std::os::fd::AsFd) -> std::io::Result<()> {
    astream_sys::close_on_fork(socket.as_fd()).map(|_| ())
}

/// Windows: nothing to do, as a handle reaches a child only when it is made inheritable.
///
/// # Errors
/// None.
#[cfg(windows)]
pub fn keep_out_of_children<T>(_socket: &T) -> std::io::Result<()> {
    Ok(())
}

/// Create descriptors and request close-on-fork before a concurrent spawn can copy them:
/// `create` makes them (a connect, a bind, an `accept`, a `socketpair`, a `try_clone`) and each
/// receives [`keep_out_of_children`] under [`spawn_lock`]. Kernels that ignore the requested
/// flag retain ordinary fork inheritance and the spawn's close-on-exec policy.
/// `create` must not block (an `accept` is on a nonblocking listener, a
/// connect is a Unix-domain one or only the `socket(2)`), and must not spawn: every spawn of this
/// process waits for it. A descriptor made here still reaches a child it is handed to through
/// [`spawnfd`] (stdio or a lane), which unmarks it for that child alone.
///
/// # Errors
/// `create`'s error, or the mark's `fcntl(2)` errno (the descriptors are then closed).
#[cfg(unix)]
pub fn kept_out<T: spawn_lock::Descriptors, E: From<std::io::Error>>(
    create: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    spawn_lock::creating(create, |made| made.mark().map_err(E::from))
}

/// Whether [`accept`] waits for a connection before answering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcceptWait {
    /// Answer `WouldBlock` when no connection is already queued.
    Ready,
    /// Wait until a connection is queued.
    Forever,
    /// Wait until this absolute deadline. Expiry answers `TimedOut`.
    Until(std::time::Instant),
}

/// The blocking mode [`accept`] explicitly gives the accepted stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcceptedMode {
    /// Reads and writes wait, subject to their socket timeouts.
    Blocking,
    /// Reads and writes answer `WouldBlock` instead of waiting.
    Nonblocking,
}

#[cfg(any(unix, windows))]
mod raw_accept {
    use super::AcceptedMode;

    pub trait RawAccept<Stream, Peer> {
        fn nonblocking(&self) -> std::io::Result<()>;
        fn accept_now(&self) -> std::io::Result<(Stream, Peer)>;
        fn accepted_mode(stream: &Stream, mode: AcceptedMode) -> std::io::Result<()>;
    }
}

/// A listener [`accept`] takes connections from: a Unix-domain or a TCP one. Sealed so the raw
/// accept operation stays private to the sanctioned helper.
#[cfg(any(unix, windows))]
pub trait Listens: raw_accept::RawAccept<Self::Stream, Self::Peer> {
    /// The connection one accept answers.
    type Stream;
    /// The peer's address, answered beside it.
    type Peer;
}

#[cfg(any(unix, windows))]
macro_rules! listens {
    ($listener:ty => $stream:ty, $peer:ty) => {
        impl Listens for $listener {
            type Stream = $stream;
            type Peer = $peer;
        }
        impl raw_accept::RawAccept<$stream, $peer> for $listener {
            fn nonblocking(&self) -> std::io::Result<()> {
                self.set_nonblocking(true)
            }
            fn accept_now(&self) -> std::io::Result<($stream, $peer)> {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "the one sanctioned accept normalizes the accepted stream's mode"
                )]
                self.accept()
            }
            fn accepted_mode(stream: &$stream, mode: AcceptedMode) -> std::io::Result<()> {
                stream.set_nonblocking(matches!(mode, AcceptedMode::Nonblocking))
            }
        }
    };
}

#[cfg(any(unix, windows))]
listens!(std::net::TcpListener => std::net::TcpStream, std::net::SocketAddr);
#[cfg(unix)]
listens!(std::os::unix::net::UnixListener => std::os::unix::net::UnixStream, std::os::unix::net::SocketAddr);

#[cfg(any(unix, windows))]
fn check_accept_deadline(wait: AcceptWait) -> std::io::Result<()> {
    if let AcceptWait::Until(deadline) = wait
        && std::time::Instant::now() >= deadline
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "accept deadline",
        ));
    }
    Ok(())
}

#[cfg(windows)]
impl Listens for win::CtlListener {
    type Stream = win::CtlStream;
    type Peer = ();
}

#[cfg(windows)]
impl raw_accept::RawAccept<win::CtlStream, ()> for win::CtlListener {
    fn nonblocking(&self) -> std::io::Result<()> {
        self.set_nonblocking(true)
    }

    fn accept_now(&self) -> std::io::Result<(win::CtlStream, ())> {
        self.accept()
    }

    fn accepted_mode(stream: &win::CtlStream, mode: AcceptedMode) -> std::io::Result<()> {
        stream.set_nonblocking(matches!(mode, AcceptedMode::Nonblocking))
    }
}

/// Accept one TCP or Unix-domain connection, with both the wait and the accepted stream's mode
/// stated by the caller. The listener is left nonblocking.
///
/// On Unix the connection is kept out of every child from its creation: a wait is a `poll(2)` that
/// holds nothing ([`ready::one`] / [`ready::until`]), and only the nonblocking `accept(2)` and its
/// mark hold the spawn lock ([`kept_out`]), so no spawn waits for a connection to come. The stream
/// mode is then set explicitly: BSD can inherit `O_NONBLOCK` from the listener and Linux does not.
///
/// # Errors
/// The listener or stream mode, readiness wait, `accept(2)` (other than a retried `EINTR`) or
/// close-on-fork mark. `Ready` answers `WouldBlock` when no connection is queued, and an expired
/// `Until` answers `TimedOut`.
#[cfg(unix)]
pub fn accept<L>(
    listener: &L,
    wait: AcceptWait,
    mode: AcceptedMode,
) -> std::io::Result<(L::Stream, L::Peer)>
where
    L: Listens + std::os::fd::AsFd,
    L::Stream: spawn_lock::Descriptors,
    L::Peer: spawn_lock::Descriptors,
{
    check_accept_deadline(wait)?;
    <L as raw_accept::RawAccept<L::Stream, L::Peer>>::nonblocking(listener)?;
    loop {
        match kept_out(|| <L as raw_accept::RawAccept<L::Stream, L::Peer>>::accept_now(listener)) {
            Ok(accepted) => {
                <L as raw_accept::RawAccept<L::Stream, L::Peer>>::accepted_mode(&accepted.0, mode)?;
                return Ok(accepted);
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && wait == AcceptWait::Ready =>
            {
                return Err(error);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => match wait {
                AcceptWait::Ready => unreachable!("the ready case returned above"),
                AcceptWait::Forever => ready::one(listener.as_fd())?,
                AcceptWait::Until(deadline) => {
                    ready::until(listener.as_fd(), ready::Interest::Read, deadline)?;
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                check_accept_deadline(wait)?;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Windows TCP counterpart of the Unix [`accept`]. A Windows handle reaches a child only when it
/// is made inheritable, so no spawn lock is needed. `WSAPoll` waits without changing the socket or
/// competing with another clone's event registration.
#[cfg(windows)]
pub fn accept<L>(
    listener: &L,
    wait: AcceptWait,
    mode: AcceptedMode,
) -> std::io::Result<(L::Stream, L::Peer)>
where
    L: Listens + std::os::windows::io::AsRawSocket,
{
    check_accept_deadline(wait)?;
    <L as raw_accept::RawAccept<L::Stream, L::Peer>>::nonblocking(listener)?;
    loop {
        match <L as raw_accept::RawAccept<L::Stream, L::Peer>>::accept_now(listener) {
            Ok(accepted) => {
                <L as raw_accept::RawAccept<L::Stream, L::Peer>>::accepted_mode(&accepted.0, mode)?;
                return Ok(accepted);
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && wait == AcceptWait::Ready =>
            {
                return Err(error);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let deadline = match wait {
                    AcceptWait::Ready => unreachable!("the ready case returned above"),
                    AcceptWait::Forever => None,
                    AcceptWait::Until(deadline) => Some(deadline),
                };
                win::wait_readable(listener.as_raw_socket(), deadline)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                check_accept_deadline(wait)?;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Windows: `create`'s answer, as a handle reaches a child only when it is made inheritable.
///
/// # Errors
/// `create`'s error.
#[cfg(windows)]
pub fn kept_out<T, E>(create: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
    create()
}

/// The process-wide lock between a spawn and a descriptor's creation: every spawn and fork
/// holds [`spawn_lock::spawning`] across its descriptor table's copy, and [`kept_out`] creates
/// and marks with none in flight.
pub mod spawn_lock;

/// The Windows AF_UNIX-over-winsock implementation of [`CtlStream`] / [`CtlListener`] (afunix.sys; raw `ws2_32` FFI, no external deps).
#[cfg(windows)]
pub mod win;
#[cfg(windows)]
pub use win::{CtlListener, CtlStream};

/// Leaving a file or a folder out of backups, behind an injected platform field.
pub mod backup;

/// Bounded regular-file reads on every platform; the whole-file writer on Unix and Windows
/// (a new temporary beside it, renamed over the file, and finished by the flush its caller
/// takes: durable for `write`, the one named or none for `write_flushed`; Unix modes apply on
/// Unix), and aterm.toml's Unix editor under the config service's write lock.
pub mod atomic_file;
pub mod quiet_state;

/// This account's own home as the account database has it ([`account::home`]), whatever `$HOME` says: what a launchd job of this login runs with.
#[cfg(unix)]
pub mod account;

/// Test support (test and debug builds, macOS and Linux): the call a thread of this process sleeps in, as the kernel has it ([`asleep::asleep`]), which tells a thread queued for a file lock (`flock(2)`) from one that polls.
#[cfg(all(
    any(test, debug_assertions),
    any(target_os = "macos", target_os = "linux")
))]
pub mod asleep;

/// Test support (test and debug builds, Apple platforms and Linux): a child parked between `fork` and `exec` ([`fork_window::Parked`]), holding a copy of every descriptor, for a test of what a closed descriptor's lock leaves held.
#[cfg(all(
    any(test, debug_assertions),
    any(target_vendor = "apple", target_os = "linux")
))]
pub mod fork_window;

#[cfg(target_os = "macos")]
mod darwin_loader;

#[cfg(unix)]
pub mod childwake;
/// A deadline on a call that can block forever: in this process ([`deadline::Gate`], the shared [`deadline::TRUST`] gate) or in a child ([`deadline::output_until`]).
#[cfg(not(target_arch = "wasm32"))]
pub mod deadline;
/// A debug build's journal of the children it reaps that a signal ended (`ATERM_DEBUG_DEATH_JOURNAL`), the lab's witness of a helper a jail killed.
#[cfg(unix)]
pub mod deaths;
/// Unix directory identities read from one descriptor, with a persistent volume UUID where supported.
#[cfg(unix)]
pub mod directory_identity;
/// Whether this process runs elevated (a Windows administrator's full token), which gives it a control directory of its own ([`control_socket_dir`]).
pub mod elevation;
/// A kernel witness of another process's death with its `wait(2)` status (the PTY keeper's death classifier), and the moment of it ([`exitwatch::ExitMoment`], read by `aterm ctl subscribe`'s redial).
pub mod exitwatch;
pub mod fdpass;
/// Exclusive file-lock waits with bounded contention backoff and deadline rechecks.
#[cfg(any(unix, windows))]
pub mod file_lock;
/// Path-aware file events, with optional debounce and content-stamp filtering.
pub mod file_watch;

/// Descriptor-pinned app-image cloning and confined filesystem seals.
#[cfg(unix)]
pub mod app_image;
/// Test support (test and debug builds): the `git` a fixture runs, isolated from this machine.
#[cfg(all(any(test, debug_assertions), not(target_arch = "wasm32")))]
pub mod fixture_git;
/// Another program's live SQLite database (agy's summaries, Muse's goals, tccd's `TCC.db`), read into memory with no lock, copy or subprocess ([`foreign_db::read_bytes`]), plus the remote immutable URI spelling ([`foreign_db::immutable_uri`]).
pub mod foreign_db;
/// Whether a connected stream's peer has closed entirely (a lane's hangup check).
pub mod hangup;
/// Which live processes of this uid hold a descriptor for one device (the PTY keeper's holder scan), or use a path under a set of directories (the disk watch's check before it removes a build directory whole).
pub mod holders;
pub mod host;
/// Whether this user's default keychain is unlocked, read with no dialog: work nobody asked for runs no keychain-reading program while it is not.
pub mod keychain;
/// aterm's own LAN traffic, through the root LAN broker on macOS (`docs/DESIGN-lan-broker-2026-11-05.md`).
#[cfg(unix)]
pub mod lan;
pub mod latest;
/// Unix-only: the descriptor a throwaway `--headless` instance watches so it ends with the process that started it — both ends.
#[cfg(unix)]
pub mod lifeline;
/// Framed log appends coordinated with atomic replacement by a stable sibling lock.
pub mod line_log;
/// Which listening socket a path names, so a hang-up is judged by the listener that accepted the stream and never by the pid alone.
pub mod listener;
/// Local interface addresses for retry decisions, read without connecting a socket.
#[cfg(unix)]
pub mod network;
/// Unix-only: paths read without following a link anywhere on them (macOS's `O_NOFOLLOW_ANY`): what is at one, an event-only watch descriptor and the kqueue that hears it, and a reader of a regular file alone that never blocks on a FIFO; off macOS, `Unsupported`.
#[cfg(unix)]
pub mod nofollow;
/// Captured subprocess output, optionally delegated to a caller-owned executor.
#[cfg(not(target_arch = "wasm32"))]
pub mod output;
/// Who holds a file's owner lock (the PTY keeper reads a dead window's crash marker with it).
pub mod ownerlock;
/// The partition lane's cut: in a debug build, `ATERM_DEBUG_PARTITION` names the `ip:port`s the one TCP connect funnel, [`connect_tcp`], refuses to dial (fleet dials, a loopback-scope `lan::Connection`, vault requests, atpkg's peer fetches; [`partition::check`]); a LAN-scope `lan::Connection` and a direct `TcpStream` connect are not cut.
pub mod partition;
/// The kernel's record of a socket peer: its uid and (Darwin) audit token.
pub mod peer;
/// This Mac's sleep and wake as the system announces them (`IORegisterForSystemPower`), each handed to a handler on a thread of its own and a will-sleep acknowledged once the handler returned; elsewhere nothing is heard.
pub mod power;
/// Unix-only: strict private files, the user's own regular file directly in the user's own 0700 directory, with no second link, no symlink and no ACL grant (lifted from aterm-net's `fleet_member`).
#[cfg(unix)]
pub mod private_file;
/// `KERN_PROCARGS2`, a process's exec path, argument words and initial environment: the one reader ([`procargs::read`], macOS) and the one parser ([`procargs::parse`]) every crate reads it through, aterm-sysprobe's (its `kproc` reads a process with it, and this crate depends on that one).
#[cfg(any(unix, windows))]
pub use aterm_sysprobe::procargs;
/// The hidden verbs a build's memory claim runs as (BUILD-ADMIT): atpkg's front, which claims and
/// then becomes the build, and the holder it leaves behind; named here so the census, which does
/// not link atpkg, names their processes.
pub mod build_front;
/// The leftover view's reader (`sys left`): which of this uid's processes are candidates, and each one's kernel facts for the pure judge (`aterm_vitals::left`), composed from this crate's readers through one injected seam, never reading a held program's environment or arguments.
pub mod left;
/// Linux's proc connector as one listener for the whole process: a subscription proven by the kernel's ack and a probe child's `exec`, each `exec` routed to the session whose shell leads the process's session (Linux 6.6 on, no capability asked).
#[cfg(unix)]
pub mod proc_connector;
pub mod process;
/// Async-signal-safe capture and off-handler process attribution.
#[cfg(unix)]
pub mod signal_origin;

/// Atomic image-specific signalling available only from actual owned children.
pub mod owned_signal;
/// A kernel witness of a session's `exec`s (card engine §3.3): one kqueue per shell, its forks and the `exec`s and exits of its children and of the terminal's holder.
#[cfg(unix)]
pub mod procwatch;
/// macOS's program check (`syspolicyd`) read from outside: its thread pool, by one bounded setuid `ps` of its pid, for the wedge judge.
#[cfg(not(target_arch = "wasm32"))]
pub mod program_check;
/// Whether a long step on this thread is still going, told only to what that thread asked to tell.
pub mod progress;
/// `posix_spawn` with `POSIX_SPAWN_CLOEXEC_DEFAULT`: a child that inherits nothing but a `/dev/null` stdio (the PTY keeper's one spawn).
pub mod pspawn;
/// macOS-only: the calling thread's requested QoS class, and its lift to the default class while it does short work a thread at a higher class waits on ([`qos::lift`], [`qos::restore`]: a holder of a lock another thread wants, a fork in flight); and the base priority each thread of a process runs at, read from outside it ([`qos::thread_base_priorities`]).
#[cfg(target_os = "macos")]
pub mod qos;
pub mod rand;
/// Unix-only: one wait on two descriptors (or one, [`ready::one`]) with no timeout, ended by whichever is readable, at its end or in error first, reading neither (the standby bridge's wait for its lock or its aterm's word, an [`exitwatch::ExitMoment`]'s for the exit or its cancel, a blocking accept loop's wait before its nonblocking `accept` under [`kept_out`]).
#[cfg(unix)]
pub mod ready;
/// Unix-only: the process-wide reaper of children nobody else waits for (a dropped `spawnfd` child still running, a PTY shell past its deadline).
#[cfg(unix)]
pub mod reaper;
/// Read-only regular-file handles without following a final symlink or Windows reparse point.
pub mod regular_file;
/// Verify's run records read back: their format, the bounded read of an admitted snapshot, the `dead_seen` append, what the live runs claim and the A1–A7 verdict on one process; the window's `sys left` and doctor read them, and aterm-verify writes them.
pub mod runs;
/// Secret bytes: scrubbed on drop, a redacted `Debug`, no `Clone`; and the process's no-core-dump switch.
pub mod secret;
/// macOS's own services' footprints read without root: each resource coalition of the system's accounts alone, for the service-memory judge.
#[cfg(not(target_arch = "wasm32"))]
pub mod services;
/// Unix-only: whether an account other than this one can write a file or directory (its mode bits, its owner, its access ACL, its filesystem and the user-private-group rule).
#[cfg(unix)]
pub mod shared_write;
/// Unix-only: anonymous POSIX shared memory, the crash-restore journal's region: created read-write with a read-only custody descriptor the keeper holds unmapped, and mapped read-only by the window that restores a dead one's sessions.
#[cfg(unix)]
pub mod shm;
/// Which Windows console a child process gets: a windowless one when this process has none (the window), so no console flashes on screen, and nothing changed when it has one or off Windows.
pub mod spawn;
/// Child spawns that inherit only what they are handed: [`spawnfd::command`], the `Command` every child is built with (on Darwin one `posix_spawn`, no fork); the fabric bridge's spawn with two descriptors at fixed numbers; the inheritance strip for a spawn that must fork (the PTY child's), and the one descriptor such a spawn may pass at a fixed number ([`spawnfd::PassAt`]); and [`spawnfd::transparent`], the pass-throughs' spawn, whose child inherits this process's whole table.
#[cfg(unix)]
pub mod spawnfd;
/// Off Unix, [`spawnfd::command`] (there: the Windows console rule of [`spawn`]), [`spawnfd::transparent`], [`spawnfd::InheritNothing`] and std's `Command`, `Stdio` and `Child` under spawnfd's names, so a shared spawn site reads the same on every platform.
#[cfg(not(unix))]
#[path = "spawnfd_nonunix.rs"]
pub mod spawnfd;
/// Explicit, identity-bound macOS stack sampling with a closed wait kind and a retained private report.
pub mod wait_sample;
/// The wall clock's moves (a sleep and wake, a step) as the system announces them, on descriptors a loop polls beside its own (macOS libnotify's `clock_set` and the power domain's wake, Linux's `TFD_TIMER_CANCEL_ON_SET` timer) or, on Windows, an event it waits on (the power manager's suspend and resume callback; no step is announced there); elsewhere none.
#[cfg(any(unix, windows))]
pub mod wall_watch;

/// The soft descriptor floor a process that fans work out over many threads
/// and children raises itself toward ([`spawnfd::raise_soft_nofile`]): a stock
/// macOS shell's 256 is passed by one test binary's threads together, so a run
/// fails `Too many open files` on the caller's limit alone. One value for
/// every raise in the tree (the GUI's successor floor, verify's stage spawn,
/// the generated `targo` shim).
pub const SOFT_NOFILE_FLOOR: u64 = 4096;

/// The raised soft limit for `(soft, hard)`: [`SOFT_NOFILE_FLOOR`] capped at
/// the hard limit, where that raises anything. Never a lowering.
#[must_use]
pub fn raised_soft_nofile_target(soft: u64, hard: u64) -> Option<u64> {
    let target = SOFT_NOFILE_FLOOR.min(hard);
    (target > soft).then_some(target)
}
/// Pure-Rust, read-only SQLite 3 B-tree and WAL snapshot reader (`#![forbid(unsafe_code)]`), paired with [`foreign_db::read_bytes`].
pub mod sqlite_read;
/// A verb's answer on stdout, where a reader that closed the pipe (`| head -1`) ends the answer instead of panicking the process.
pub mod stdout;
/// A latched stop a thread sleeps on between rounds and is woken by at once ([`stop::Stop`]), in place of a flag it reads every few milliseconds.
pub mod stop;
/// The kernel facts a `sudo` askpass relay stands on, and a scrubbed secret for the password in flight (`docs/DESIGN-sudo-askpass-2026-10-15.md`).
pub mod sudo;
/// A Cargo test binary's own per-user root, under which [`control_socket_dir`] and `aterm_types::dirs` answer in a test, so no test reaches the person's live socket, state, logs or config.
pub mod test_binary;
/// A child that ends with the process that started it, killed outright included: Linux's parent-death signal, and this executable's [`tether::VERB`] (every tethered spawn on Darwin, a group leader's on Linux), which leaves a watcher behind as it execs the child ([`spawnfd::Tether`]).
pub mod tether;
/// A zone's offset from UTC at an instant, read in this process: the local clock's by
/// `localtime_r`, a named zone's from its TZif file, never `date +%z`. See [`zone`].
pub mod zone;

/// The per-user directory holding aterm's control socket and token, resolved identically for the server and every client.
#[must_use]
pub fn control_socket_dir() -> Option<std::path::PathBuf> {
    control_socket_dir_inner(test_binary::scratch_root())
}

/// [`control_socket_dir`]'s per-user default when `$XDG_RUNTIME_DIR` is unset, as `~/<that>` for a person to read.
#[cfg(target_os = "macos")]
pub const CONTROL_SOCKET_DIR_DEFAULT: &str = "~/Library/Application Support/aterm";
/// [`control_socket_dir`]'s per-user default when `$XDG_RUNTIME_DIR` is unset, as `~/<that>` for a person to read.
#[cfg(not(target_os = "macos"))]
pub const CONTROL_SOCKET_DIR_DEFAULT: &str = "${XDG_STATE_HOME:-~/.local/state}/aterm/run";

/// Whether [`control_socket_dir`] is private: `$XDG_RUNTIME_DIR` is set, so it is not the per-user default under `$HOME`, or this is a test binary's scratch one.
#[must_use]
pub fn control_socket_dir_is_private() -> bool {
    #[cfg(windows)]
    {
        false
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_RUNTIME_DIR").is_some() || test_binary::scratch_root().is_some()
    }
}

/// The Windows control directory's name under the per-user temp folder for an unelevated process.
#[cfg(windows)]
pub const CONTROL_DIR_NAME: &str = "aterm";

/// The Windows control directory's name under the per-user temp folder for an elevated process ([`control_socket_dir`]).
#[cfg(windows)]
pub const ELEVATED_CONTROL_DIR_NAME: &str = "aterm-elevated";

/// [`control_socket_dir`], with `test_root` (a test binary's scratch root,
/// [`test_binary::scratch_root`]) standing in for the per-user default.
fn control_socket_dir_inner(test_root: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    let default = windows_control_dir(
        [
            std::env::var_os("TMP"),
            std::env::var_os("TEMP"),
            std::env::var_os("LOCALAPPDATA"),
        ],
        elevation::is_elevated(),
    );
    #[cfg(not(windows))]
    let default = {
        use std::path::PathBuf;
        if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
            match test_root {
                Some(root) if test_binary::is_login_runtime_dir(&xdg) => {
                    return Some(root.join("run"));
                }
                _ => return Some(PathBuf::from(xdg).join("aterm")),
            }
        }
        per_user_default(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
    };
    match (default, test_root) {
        (Some(_), Some(root)) => Some(root.join("run")),
        (default, _) => default,
    }
}

/// [`control_socket_dir`] with `$XDG_RUNTIME_DIR` unset, over `$XDG_STATE_HOME`
/// and `$HOME`.
#[cfg(not(windows))]
fn per_user_default(
    xdg_state_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    if cfg!(any(target_os = "macos", not(unix))) {
        let _ = xdg_state_home;
        return Some(
            PathBuf::from(home?)
                .join("Library")
                .join("Application Support")
                .join("aterm"),
        );
    }
    let state = match xdg_state_home.map(PathBuf::from) {
        Some(root) if root.is_absolute() => root,
        _ => PathBuf::from(home?).join(".local").join("state"),
    };
    Some(state.join("aterm").join("run"))
}

/// [`control_socket_dir`] on Windows over `[%TMP%, %TEMP%, %LOCALAPPDATA%]` and
/// whether this process is elevated: the first non-empty temp variable, else
/// `%LOCALAPPDATA%\Temp`, then [`CONTROL_DIR_NAME`] or, elevated,
/// [`ELEVATED_CONTROL_DIR_NAME`].
#[cfg(windows)]
fn windows_control_dir(
    [tmp, temp, local_app_data]: [Option<std::ffi::OsString>; 3],
    elevated: bool,
) -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let non_empty = |v: Option<std::ffi::OsString>| v.filter(|s| !s.is_empty());
    let base = match non_empty(tmp).or_else(|| non_empty(temp)) {
        Some(temp) => PathBuf::from(temp),
        None => PathBuf::from(non_empty(local_app_data)?).join("Temp"),
    };
    Some(base.join(if elevated {
        ELEVATED_CONTROL_DIR_NAME
    } else {
        CONTROL_DIR_NAME
    }))
}

#[cfg(all(test, windows))]
#[path = "lib_windows_control_dir_tests.rs"]
mod windows_control_dir_tests;

#[cfg(all(test, not(windows)))]
#[path = "lib_control_dir_tests.rs"]
mod control_dir_tests;
