// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Same-UID process descriptor and path holder scans.

use std::io;

/// Result of scanning same-UID processes for open descriptors.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HolderScan {
    /// Processes holding at least one matching descriptor.
    pub holders: Vec<u32>,
    /// Processes whose descriptor table could not be read.
    pub unreadable: Vec<u32>,
    /// Linux processes whose descriptor table only root reads.
    pub sealed: Vec<u32>,
    /// Total processes scanned.
    pub scanned: usize,
}

/// Unix platform services whose protected descriptor tables do not represent an unknown
/// user's build-cache owner. The disk pass and package-cache sweep share this policy.
#[must_use]
pub fn unix_platform_service(exe: Option<&std::path::Path>) -> bool {
    exe.is_some_and(|path| {
        ["/System/", "/usr/libexec/", "/usr/sbin/", "/sbin/"]
            .iter()
            .any(|root| path.starts_with(root))
    })
}

impl HolderScan {
    /// Whether `pid` holds the target.
    #[must_use]
    pub fn holds(&self, pid: u32) -> bool {
        self.holders.contains(&pid)
    }
}

/// Scan live same-UID processes except `skip` for descriptors matching `rdev`.
/// # Errors
/// The process listing itself failing; `Unsupported` off macOS.
pub fn scan_rdev_holders(rdev: u64, skip: &[u32]) -> io::Result<HolderScan> {
    Ok(scan_holders_of(&[rdev], skip)?.pop().unwrap_or_default())
}

/// The `st_rdev` of the master of the PTY whose slave is `slave`.
/// `None` when `/dev/ptmx` cannot be read, and off macOS.
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn pty_master_rdev(slave: u64) -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    const DEV: u64 = 0xffff_ffff;
    const MINOR: u64 = 0x00ff_ffff;
    let ptmx = std::fs::metadata("/dev/ptmx").ok()?.rdev() & DEV;
    Some((ptmx & !MINOR) | (slave & MINOR))
}

/// See the macOS twin.
#[cfg(not(target_vendor = "apple"))]
#[must_use]
pub fn pty_master_rdev(_slave: u64) -> Option<u64> {
    None
}

/// Whether a live process of this uid holds the master of the PTY whose terminal is `slave`, a
/// live process's controlling terminal as it numbers it (macOS's `e_tdev`): by the master's own
/// number ([`pty_master_rdev`]), read off this uid's descriptor tables ([`master_held_by`]).
/// `None` when the listing or a descriptor table did not read.
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn pty_master_held(slave: u64) -> Option<bool> {
    master_held_by(&scan_rdev_holders(pty_master_rdev(slave)?, &[]).ok()?)
}

/// What a scan for a PTY's master says of it: held once a table holds it, and free only once
/// every table read and none did.
#[cfg(any(target_vendor = "apple", test))]
fn master_held_by(scan: &HolderScan) -> Option<bool> {
    if scan.holders.is_empty() {
        (scan.unreadable.is_empty() && scan.sealed.is_empty()).then_some(false)
    } else {
        Some(true)
    }
}

/// See the macOS twin: `slave` as `/proc/<pid>/stat`'s `tty_nr` numbers it. Every devpts
/// terminal a live process still has reads held and none reads free, since the kernel takes a
/// terminal from its session once its master closes (`holders_procfs_pty.rs`); a terminal that
/// is no PTY's reads `None`.
#[cfg(target_os = "linux")]
#[must_use]
pub fn pty_master_held(slave: u64) -> Option<bool> {
    procfs::pty::pty_master_held_of(slave)
}

/// See the macOS twin: no reader here.
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
#[must_use]
pub fn pty_master_held(_slave: u64) -> Option<bool> {
    None
}

/// Scan live same-UID processes except `skip` for each device in `rdevs`.
/// # Errors
/// Returns an error if the process listing fails.
#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
pub fn scan_holders_of(rdevs: &[u64], skip: &[u32]) -> io::Result<Vec<HolderScan>> {
    let wanted: Vec<u128> = rdevs.iter().map(|r| u128::from(*r)).collect();
    scan(&wanted, Lookup::BY_RDEV, skip)
}

/// Scan live same-UID processes except `skip` for descriptors open on `path`.
/// # Errors
/// `path` cannot be read (`stat`), or the process listing fails; `Unsupported` off macOS.
#[cfg(target_vendor = "apple")]
pub fn scan_file_holders(path: &std::path::Path, skip: &[u32]) -> io::Result<HolderScan> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::metadata(path)?;
    let dev = u32::try_from(meta.dev() & u64::from(u32::MAX)).map_err(io::Error::other)?;
    let file = Vnode {
        dev,
        ino: meta.ino(),
        rdev: 0,
    };
    Ok(scan(&[file.file_key()], Lookup::BY_FILE, skip)?
        .pop()
        .unwrap_or_default())
}

/// Scan live same-UID processes except `skip` for descriptors open on `path`; a table only root
/// reads is in [`HolderScan::sealed`].
/// # Errors
/// `path` cannot be read, the process listing fails, or the reads take longer than 10 s
/// (`TimedOut`: a hung mount).
#[cfg(target_os = "linux")]
pub fn scan_file_holders(path: &std::path::Path, skip: &[u32]) -> io::Result<HolderScan> {
    use std::os::unix::fs::MetadataExt as _;
    let me = std::fs::metadata("/proc/self")?.uid();
    let (path, skip) = (path.to_path_buf(), skip.to_vec());
    within(SCAN_DEADLINE, TABLES, HUNG, move || {
        procfs::scan_file_holders_in(std::path::Path::new(procfs::PROC), me, &path, &skip)
    })
}

/// Scan live same-UID processes except `skip` for descriptors open on `path`.
/// # Errors
/// Always `Unsupported`.
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
pub fn scan_file_holders(_path: &std::path::Path, _skip: &[u32]) -> io::Result<HolderScan> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the holder scan reads libproc (macOS) or /proc (Linux)",
    ))
}

/// Scan live same-UID processes except `skip` for a listening `AF_UNIX` socket bound at `path`, by
/// the socket's own kernel record, never by a process's arguments or descriptors: the kernel's
/// table of unix-domain streams ([`aterm_sysprobe::jobs::darwin::unix_listeners`]), which names
/// each listener's path, its user and the process that bound it. No other process's descriptor is
/// read: a socket read through one (`PROC_PIDFDSOCKETINFO`) that its holder closes during the read
/// fails every blocking wait on that socket from then on, in every process holding it (the drain
/// `aterm_sysprobe::jobs::macos`'s `settled` names). So a holder is the live process the
/// listener's record names ([`aterm_sysprobe::jobs::darwin::UnixListenerRow::pid`]): its binder,
/// even after closing its own copy while another process (a child mid-spawn) holds one, unless a
/// later holder waited on it by `poll` or a `kevent` (a pre-fork server's worker), which is named
/// instead; one that only blocks in `accept` is not. The record keeps a pid number: once the
/// process it names has exited while another holds the listener, the scan names nobody, and,
/// after that number is reused, the unrelated live process of this user that has it. A listener
/// holds `path` when it was bound there ([`SocketPath::names`]), or at its private sibling
/// `<path>.<pid>.<n>` ([`SocketPath::is_sibling`]) with its record's pid the one the kernel names
/// for the listener a connection to `path` reaches (`serving_pid`): an astream broker binds
/// there, links `path` to its socket and removes the name (`Broker::bind_unix`), and the record
/// keeps the name it was bound at and no file a path can be matched with. That connection is made
/// only for a live row of this user's bound at a sibling, and the listener it reaches sees a peer
/// that closes at once. One that does not answer within [`SCAN_DEADLINE`], is refused
/// (`ECONNREFUSED`) while a probe finds a socket bound to the file (`refused_by`: a listener
/// whose accept queue is full is refused as a gone one is, and the probe tells them apart), fails
/// other than as no listener's, or reaches a listener whose pid no row of the table read binds
/// at `path` or a sibling (one bound after the read), leaves the answer unknown, the scan's
/// error, never a scan naming no holder. Each scan finds a listener of its own in the same table
/// read, bound for the check, so a table this build misreads is refused, never read as no holder.
///
/// # Errors
/// The process listing or the table read failing, the check's listener not binding or not found
/// in the table as this build reads it (`Unsupported`), or the connection to `path` not answering
/// within 10 s (`TimedOut`), refused while a socket may be bound there (`ConnectionRefused`),
/// failing other than as no listener's (`serving_pid`), or reaching a listener the table read
/// does not hold (`listener_holders`); unreadable endpoint metadata or a symlink at its leaf
/// leaves ownership unknown.
#[cfg(target_vendor = "apple")]
pub fn scan_listener_holders(path: &std::path::Path, skip: &[u32]) -> io::Result<HolderScan> {
    listener_scan(path, skip, SCAN_DEADLINE)
}

/// [`scan_listener_holders`] whose connection to `path` (macOS) or whole scan (Linux) is waited
/// for at most `hang` in place of [`SCAN_DEADLINE`]. Native owner validation uses its
/// shorter custody bound; a test whose own broker serves `path` passes the one-minute
/// hang detector. A loaded machine can keep the spawn lock's holder and the broker,
/// or Linux's table reads, from finishing within the default scan's 10 s.
///
/// # Errors
/// [`scan_listener_holders`]'s, with `TimedOut` once `hang` is up.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
pub fn scan_listener_holders_within(
    path: &std::path::Path,
    skip: &[u32],
    hang: std::time::Duration,
) -> io::Result<HolderScan> {
    listener_scan(path, skip, hang)
}

/// [`scan_listener_holders`], its connection to `path` waited for at most `hang`
/// ([`serving_pid`]).
#[cfg(target_vendor = "apple")]
fn listener_scan(
    path: &std::path::Path,
    skip: &[u32],
    hang: std::time::Duration,
) -> io::Result<HolderScan> {
    use std::os::unix::fs::MetadataExt as _;
    listener_target_inode(
        std::fs::symlink_metadata(path).map(|m| (m.ino(), m.file_type().is_symlink())),
    )?;
    let check = TableCheck::bind()?;
    let table = unix_stream_table()?;
    // SAFETY: `geteuid` is a side-effect-free getter.
    let me = unsafe { ffi::geteuid() };
    let own = aterm_sysprobe::jobs::darwin::UnixListenerRow {
        pid: std::process::id(),
        uid: me,
        path: check.path.as_os_str().as_encoded_bytes(),
    };
    listener_holders(
        aterm_sysprobe::jobs::darwin::unix_listeners(&table).as_deref(),
        own,
        &SocketPath::of(path),
        || serving_pid(path, hang),
        &uid_pids()?,
        skip,
    )
}

/// How long a scan waits for an answer before its answer is unknown (`TimedOut`, never a scan
/// naming no holder): Linux's whole scan, which a hung mount its `/proc` links reach stalls,
/// macOS's one connection (`serving_pid`) with its probe, and [`bound_at`]'s probe, whose
/// `socket(2)` a spawn holding the spawn lock delays. Each is waited for on a thread of its own
/// ([`within`]). A hang detector, never a budget: none takes long unless something is stuck.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
const SCAN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// What Linux's scans wait for ([`within`]).
#[cfg(target_os = "linux")]
const TABLES: &str = "the descriptor tables";

/// What stalls them.
#[cfg(target_os = "linux")]
const HUNG: &str = "a hung mount";

/// `read`'s answer if it gives one within `deadline`, read on a thread of its own; else
/// `TimedOut`, saying that `what` did not answer and what may stall it (`why`). A read that does
/// not answer keeps its thread (no read can be cancelled), and its answer is dropped when it comes.
///
/// # Errors
///
/// `read`'s error, `TimedOut`, or the thread not starting.
#[cfg(any(target_vendor = "apple", target_os = "linux", all(test, unix)))]
fn within<T: Send + 'static>(
    deadline: std::time::Duration,
    what: &str,
    why: &str,
    read: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    within_at(deadline, std::time::Instant::now(), what, why, read)
}

#[cfg(any(target_vendor = "apple", target_os = "linux", all(test, unix)))]
fn within_at<T: Send + 'static>(
    deadline: std::time::Duration,
    started: std::time::Instant,
    what: &str,
    why: &str,
    read: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    let timed_out = || {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "{what} did not answer within {} s ({why})",
                deadline.as_secs()
            ),
        )
    };
    let until = crate::deadline::saturating_after(started, deadline);
    if std::time::Instant::now() >= until {
        return Err(timed_out());
    }
    let (answer, answered) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("aterm-holder-scan".to_string())
        .spawn(move || {
            let _ = answer.send(read());
        })?;
    let remaining = until
        .checked_duration_since(std::time::Instant::now())
        .ok_or_else(timed_out)?;
    match answered.recv_timeout(remaining) {
        Ok(read) => read,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(timed_out()),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(io::Error::other(format!(
            "{what} gave no answer: its thread ended"
        ))),
    }
}

/// The pid the kernel names for the listener a connection to `path` reaches: the connection's
/// `LOCAL_PEERPID`, which xnu copies from that listener's `last_pid` as `connect(2)` queues it
/// (`sonewconn`), the `so_last_pid` a table row names its listener by. `Ok(None)` when the
/// connect is refused as no listener's ([`no_listener`]), or refused (`ECONNREFUSED`) with
/// nothing bound to the file at `path` ([`refused_by`]). A macOS `connect(2)` to a path is
/// queued or refused at once, and so is the probe a refusal is followed by ([`probe_bound`]),
/// so the one wait is each one's `socket(2)`, which a spawn holding this process's spawn lock
/// delays ([`crate::kept_out`]): the connection and its probe are made on a thread of their own
/// ([`within`]) and waited for at most `hang`, the hang detector [`SCAN_DEADLINE`]. One that
/// answers later keeps its thread until then and is closed unread.
///
/// # Errors
/// `TimedOut` when the connection does not answer within `hang`; `ConnectionRefused` when the
/// connect is refused (`ECONNREFUSED`) while a socket may be bound to the file ([`refused_by`]):
/// a live listener whose accept queue is full (a wedged broker), say; and any other failure to
/// connect or to read the pid but [`no_listener`]'s: whether a listener serves `path` is then
/// unknown.
#[cfg(target_vendor = "apple")]
fn serving_pid(path: &std::path::Path, hang: std::time::Duration) -> io::Result<Option<u32>> {
    serving_pid_at(path, hang, std::time::Instant::now())
}

#[cfg(target_vendor = "apple")]
fn serving_pid_at(
    path: &std::path::Path,
    hang: std::time::Duration,
    started: std::time::Instant,
) -> io::Result<Option<u32>> {
    let deadline = crate::deadline::saturating_after(started, hang);
    let path = path.to_path_buf();
    let why = "a spawn holding the spawn lock";
    within_at(
        hang,
        started,
        "the connection to the listener",
        why,
        move || match crate::connect::until(&path, deadline) {
            Ok(stream) => crate::fdpass::peer_pid(&stream).map(Some),
            Err(e) if no_listener(&e) => Ok(None),
            Err(e) if e.raw_os_error() == Some(libc::ECONNREFUSED) => {
                refused_by(&path, &e, probe_bound(&path))
            }
            Err(e) => Err(e),
        },
    )
}

/// Whether `connect(2)` refused a path with `e` as one no stream listener takes: nothing there
/// (`ENOENT`, `ENOTDIR`), no socket (`ENOTSOCK`), a datagram socket (`EPROTOTYPE`). Not
/// `ECONNREFUSED`, which xnu answers for a live listener whose accept queue is full as for a
/// socket file whose listener is gone ([`refused_by`] tells them apart).
#[cfg(target_vendor = "apple")]
fn no_listener(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(libc::ENOENT | libc::ENOTDIR | libc::ENOTSOCK | libc::EPROTOTYPE)
    )
}

/// What a stream connection to `path` that was refused (`refused`, `ECONNREFUSED`) says of the
/// listener there, by what a probe found bound to its file (`bound`, [`probe_bound`]): none when
/// nothing is, as a socket file whose listener is gone has nothing bound to it. Else which
/// listener serves `path` is unknown: a stream socket is bound there (a listener whose accept
/// queue is full, or one not listening yet), a datagram socket took the probe (the file changed
/// between the two connects), or the probe failed.
///
/// # Errors
/// `ConnectionRefused`, saying which, unless nothing is bound to the file.
#[cfg(target_vendor = "apple")]
fn refused_by(
    path: &std::path::Path,
    refused: &io::Error,
    bound: io::Result<Bound>,
) -> io::Result<Option<u32>> {
    let why = match bound {
        Ok(Bound::Nothing) => return Ok(None),
        Ok(Bound::Stream) => {
            "a stream socket is bound to it: its accept queue is full, or it is not listening yet"
                .to_string()
        }
        Ok(Bound::Datagram) => {
            "a datagram socket took a probe of it: the file changed since".to_string()
        }
        Err(e) => format!("a probe of what is bound to it failed ({e})"),
    };
    Err(io::Error::new(
        io::ErrorKind::ConnectionRefused,
        format!("{} refused a connection ({refused}): {why}", path.display()),
    ))
}

/// What is bound to the file at a path, as a datagram connect to that path finds it
/// ([`bound_at`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bound {
    /// No socket: nothing is at the path (`ENOENT`, `ENOTDIR`), or the file there is one no
    /// socket is bound to (`ECONNREFUSED`: a socket file whose socket closed, or on Linux a file
    /// that is no socket; `ENOTSOCK` on macOS for one).
    Nothing,
    /// A stream socket, which refuses a datagram connect by its type (`EPROTOTYPE`): a
    /// listener, whatever its accept queue holds, or a socket bound and not listening yet.
    Stream,
    /// A datagram socket, which took the connect.
    Datagram,
}

/// What is bound to the file at `path` ([`Bound`]), by a datagram connect that no stream socket
/// queues or wakes for: a stream socket refuses it by its type before any connection is made,
/// so a listener whose accept queue is full, which refuses a stream connection as a socket file
/// whose listener is gone does, is told from that file. A link at `path` is followed, as a
/// connection's would be. astream's broker tells a stale socket file from a live one by the
/// same probe before it replaces one (`claim_socket_path`). The probe waits on a spawn holding
/// the spawn lock ([`probe_bound`]), so it is made on a thread of its own ([`within`]) and
/// waited for at most [`SCAN_DEADLINE`], a hang detector.
///
/// # Errors
/// The probe not answering within 10 s (`TimedOut`), its socket not made, or its connect
/// failing other than by an answer [`Bound`] names (a folder on the path refusing it, a path
/// too long for `sun_path`): what is bound there is then unknown. `Unsupported` off macOS and
/// Linux.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
pub fn bound_at(path: &std::path::Path) -> io::Result<Bound> {
    let path = path.to_path_buf();
    within(
        SCAN_DEADLINE,
        "the probe of the socket file",
        "a spawn holding the spawn lock",
        move || probe_bound(&path),
    )
}

/// See the macOS and Linux twin.
///
/// # Errors
/// Always `Unsupported`.
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
pub fn bound_at(_path: &std::path::Path) -> io::Result<Bound> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the socket file probe is made on macOS and Linux",
    ))
}

/// [`bound_at`]'s probe, unbounded: a nonblocking datagram socket, kept out of children from its
/// creation ([`crate::kept_out`], whose `socket(2)` waits on a spawn holding the spawn lock),
/// connected to `path` and closed.
///
/// # Errors
/// As [`bound_at`]'s, but for the hang detector.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn probe_bound(path: &std::path::Path) -> io::Result<Bound> {
    let probe = crate::kept_out(std::os::unix::net::UnixDatagram::unbound)?;
    probe.set_nonblocking(true)?;
    bound_by(probe.connect(path))
}

/// What a datagram connect's answer (`connected`) says is bound to the file it named
/// ([`Bound`]).
///
/// # Errors
/// `connected`'s error when it is none of the answers [`Bound`] names.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn bound_by(connected: io::Result<()>) -> io::Result<Bound> {
    match connected {
        Ok(()) => Ok(Bound::Datagram),
        Err(e) => match e.raw_os_error() {
            Some(libc::ENOENT | libc::ENOTDIR | libc::ENOTSOCK | libc::ECONNREFUSED) => {
                Ok(Bound::Nothing)
            }
            Some(libc::EPROTOTYPE) => Ok(Bound::Stream),
            _ => Err(e),
        },
    }
}

/// The holders one table read's listeners `rows` name ([`scan_listener_holders`]): the pid of
/// each listener `own`'s user made, once, when it is `live` and not in `skip`, bound at `target`,
/// or at its private sibling with that pid the one `serving` names for the listener a connection
/// to `target` reaches. The pid a sibling's name holds is its binder's own words and is not
/// compared. `serving` is asked at most once, and only for such a row bound at a sibling.
/// `scanned` counts `live` but `skip`.
///
/// # Errors
/// `Unsupported` when `rows` is no table read (`None`), or a read without `own`, the check's
/// listener: a table this build misreads. `serving`'s error, when it is asked: whether that
/// sibling holds `target` is unknown. An error too when `serving` names a pid not in `skip` that
/// no row binds at `target` or a sibling: the listener the connection reached is not in the
/// table read (one bound after it, by a broker started in its predecessor's place, say).
#[cfg(target_vendor = "apple")]
fn listener_holders(
    rows: Option<&[aterm_sysprobe::jobs::darwin::UnixListenerRow<'_>]>,
    own: aterm_sysprobe::jobs::darwin::UnixListenerRow<'_>,
    target: &SocketPath,
    mut serving: impl FnMut() -> io::Result<Option<u32>>,
    live: &[u32],
    skip: &[u32],
) -> io::Result<HolderScan> {
    let rows = rows.filter(|rows| rows.contains(&own)).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "the unix-domain stream table is not laid out as this build reads it",
        )
    })?;
    let mut scan = HolderScan {
        scanned: live.iter().filter(|pid| !skip.contains(pid)).count(),
        ..HolderScan::default()
    };
    let mut served = None;
    for row in rows {
        if row.uid != own.uid
            || !live.contains(&row.pid)
            || skip.contains(&row.pid)
            || scan.holds(row.pid)
        {
            continue;
        }
        if target.names(row.path) {
            scan.holders.push(row.pid);
        } else if target.is_sibling(row.path) {
            let serving = match served {
                Some(pid) => pid,
                None => *served.insert(serving()?),
            };
            if serving == Some(row.pid) {
                scan.holders.push(row.pid);
            }
        }
    }
    if let Some(Some(pid)) = served
        && !skip.contains(&pid)
        && !rows
            .iter()
            .any(|row| row.pid == pid && (target.names(row.path) || target.is_sibling(row.path)))
    {
        return Err(io::Error::other(format!(
            "the listener a connection to the path reaches is pid {pid}'s, which no row of the \
             table read binds there (one bound after the read, say)"
        )));
    }
    Ok(scan)
}

/// Scan live same-UID processes except `skip` for a listening `AF_UNIX` socket bound at `path`,
/// by each socket's own kernel record, never by a process's arguments: the listening sockets the
/// kernel's socket dump names ([`aterm_sysprobe::linux::unix_diag_listeners`], each record
/// and name length-delimited) matched with each process's `/proc/<pid>/fd` links
/// (`holders_procfs.rs`). A listener holds `path` as `diag_listener_holds` says: bound there,
/// or bound at its private sibling and linked to `path` (an astream broker's,
/// `Broker::bind_unix`), by the inode its record names. A table only root reads is in
/// [`HolderScan::sealed`].
///
/// # Errors
/// The dump or the process listing cannot be read: the kernel has no unix socket dump
/// (`CONFIG_UNIX_DIAG`), a sandbox refuses a netlink socket, or the reads take longer than 10 s
/// (`TimedOut`). No holder is then named: `/proc/net/unix`, whose rows a socket name holding a
/// newline forges, is never read instead. Unreadable endpoint metadata or a symlink at its leaf
/// also leaves ownership unknown.
#[cfg(target_os = "linux")]
pub fn scan_listener_holders(path: &std::path::Path, skip: &[u32]) -> io::Result<HolderScan> {
    listener_scan(path, skip, SCAN_DEADLINE)
}

/// [`scan_listener_holders`], its reads waited for at most `hang` ([`within`]).
#[cfg(target_os = "linux")]
fn listener_scan(
    path: &std::path::Path,
    skip: &[u32],
    hang: std::time::Duration,
) -> io::Result<HolderScan> {
    use std::os::unix::fs::MetadataExt as _;
    // SAFETY: `geteuid` is a side-effect-free getter.
    let me = unsafe { libc::geteuid() };
    let (path, skip) = (path.to_path_buf(), skip.to_vec());
    within(hang, TABLES, HUNG, move || {
        let listeners = diag::unix_listeners(hang)?;
        let target = SocketPath::of(&path);
        let placed = listener_target_inode(
            std::fs::symlink_metadata(&path).map(|m| (m.ino(), m.file_type().is_symlink())),
        )?;
        procfs::scan_listener_holders_in(
            std::path::Path::new(procfs::PROC),
            me,
            &listeners,
            |listener| diag_listener_holds(&target, placed, listener),
            &skip,
        )
    })
}

/// A missing target has no bound inode. Any other failed metadata read leaves its
/// listener unknown, so callers cannot treat an unreadable private sibling as absent.
/// A leaf symlink is unknown too: SocketPath resolves folders, not the endpoint itself.
#[cfg(any(target_vendor = "apple", target_os = "linux", all(test, unix)))]
fn listener_target_inode(read: io::Result<(u64, bool)>) -> io::Result<Option<u64>> {
    match read {
        Ok((_, true)) => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "listener ownership through a socket-leaf symlink is unknown",
        )),
        Ok((inode, false)) => Ok(Some(inode)),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Whether a listener the socket dump names holds `target` (Linux's [`scan_listener_holders`]):
/// it was bound there ([`SocketPath::names`]), or at its private sibling with the file its bind
/// made the one at `target`, whose inode number `lstat(2)` read as `placed`. A sibling is bound in
/// `target`'s folder and `link(2)` never crosses a filesystem or a btrfs subvolume, so a socket
/// linked into place keeps its inode number, compared by the low 32 bits the record keeps. No
/// device is compared: btrfs gives `lstat(2)` each subvolume's own device and the dump its
/// superblock's. Nor is the pid in the sibling's name: a binder in another pid namespace writes
/// its own.
#[cfg(any(target_os = "linux", test))]
fn diag_listener_holds(
    target: &SocketPath,
    placed: Option<u64>,
    listener: &aterm_sysprobe::linux::UnixDiagListener,
) -> bool {
    target.names(&listener.path)
        || target.is_sibling(&listener.path)
            && listener
                .file
                .zip(placed)
                .is_some_and(|(bound, ino)| bound.is_inode(ino))
}

/// This uid's processes that hold a listening `AF_UNIX` socket at any path, this one aside, read
/// as [`scan_listener_holders`] reads one path: by each socket's own kernel record, never by a
/// process's arguments, with the check's own listener found in the same table read, so a table
/// this build misreads is refused, never read as no listener. On Apple the kernel's table names
/// each holder, so [`HolderScan::holders`] alone is filled; a process whose descriptors could not
/// be matched (Linux) is in [`HolderScan::unreadable`] or [`HolderScan::sealed`], neither a holder
/// nor clear.
///
/// # Errors
/// As [`scan_listener_holders`]'s.
#[cfg(target_vendor = "apple")]
pub fn scan_unix_listeners() -> io::Result<HolderScan> {
    let check = TableCheck::bind()?;
    let table = unix_stream_table()?;
    // SAFETY: `geteuid` is a side-effect-free getter.
    let me = unsafe { ffi::geteuid() };
    let own = aterm_sysprobe::jobs::darwin::UnixListenerRow {
        pid: std::process::id(),
        uid: me,
        path: check.path.as_os_str().as_encoded_bytes(),
    };
    let rows = aterm_sysprobe::jobs::darwin::unix_listeners(&table)
        .filter(|rows| rows.contains(&own))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "the unix-domain stream table is not laid out as this build reads it",
            )
        })?;
    let live = uid_pids()?;
    let mut holders: Vec<u32> = rows
        .iter()
        .filter(|row| row.uid == me && row.pid != own.pid && live.contains(&row.pid))
        .map(|row| row.pid)
        .collect();
    holders.sort_unstable();
    holders.dedup();
    Ok(HolderScan {
        holders,
        scanned: live.len(),
        ..HolderScan::default()
    })
}

/// See the Apple twin: the listening sockets the kernel's socket dump names, at any bound path,
/// matched with each process's `/proc/<pid>/fd` links.
///
/// # Errors
/// As [`scan_listener_holders`]'s.
#[cfg(target_os = "linux")]
pub fn scan_unix_listeners() -> io::Result<HolderScan> {
    // SAFETY: `geteuid` is a side-effect-free getter.
    let me = unsafe { libc::geteuid() };
    let mine = std::process::id();
    within(SCAN_DEADLINE, TABLES, HUNG, move || {
        let listeners = diag::unix_listeners(SCAN_DEADLINE)?;
        procfs::scan_listener_holders_in(
            std::path::Path::new(procfs::PROC),
            me,
            &listeners,
            |_| true,
            &[mine],
        )
    })
}

/// See the Apple twin.
///
/// # Errors
/// Always `Unsupported`.
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
pub fn scan_unix_listeners() -> io::Result<HolderScan> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the listener scan reads libproc (macOS) or /proc (Linux)",
    ))
}

/// Whether a sandbox profile holds `pid` (macOS's `sandbox_check` with no filter: a lab jail's
/// processes are); `None` when the check fails. Off macOS nothing is held, and `Some(false)`.
#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn sandboxed(pid: u32) -> Option<bool> {
    unsafe extern "C" {
        fn sandbox_check(pid: i32, operation: *const std::ffi::c_char, kind: i32, ...) -> i32;
    }
    let pid = i32::try_from(pid).ok().filter(|p| *p > 0)?;
    // SAFETY: `sandbox_check` reads only its arguments; the operation is a null pointer under
    // `SANDBOX_FILTER_NONE` (0), which takes no further argument.
    match unsafe { sandbox_check(pid, std::ptr::null(), 0) } {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

/// See the Apple twin.
#[cfg(not(target_vendor = "apple"))]
#[must_use]
pub fn sandboxed(_pid: u32) -> Option<bool> {
    Some(false)
}

/// Scan live same-UID processes except `skip` for a listening `AF_UNIX` socket bound at `path`.
///
/// # Errors
/// Always `Unsupported`.
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
pub fn scan_listener_holders(_path: &std::path::Path, _skip: &[u32]) -> io::Result<HolderScan> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the listener scan reads libproc (macOS) or /proc (Linux)",
    ))
}

/// [`scan_listener_holders`] with a caller's wait bound, unsupported on this platform too.
///
/// # Errors
/// Always `Unsupported`.
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
pub fn scan_listener_holders_within(
    path: &std::path::Path,
    skip: &[u32],
    _hang: std::time::Duration,
) -> io::Result<HolderScan> {
    scan_listener_holders(path, skip)
}

/// A socket path, as a path a socket was bound at is compared with it ([`Self::names`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SocketPath {
    /// The path's own bytes.
    bytes: Vec<u8>,
    /// For an absolute path whose folder resolves, its file name in that folder's canonical
    /// spelling.
    canonical: Option<Vec<u8>>,
}

impl SocketPath {
    /// `path`, its folder resolved once here: only this path is ever resolved.
    #[must_use]
    pub fn of(path: &std::path::Path) -> Self {
        let canonical = path
            .file_name()
            .zip(path.parent())
            .filter(|_| path.is_absolute())
            .and_then(|(name, parent)| Some(parent.canonicalize().ok()?.join(name)))
            .map(|placed| placed.into_os_string().into_encoded_bytes());
        Self {
            bytes: path.as_os_str().as_encoded_bytes().to_vec(),
            canonical,
        }
    }

    /// Whether a socket bound at `bound` is bound at this path: `bound` is this path's bytes, or
    /// its canonical spelling. `bound` is another process's choice, so it is compared and never
    /// resolved (a folder there may be a hung mount or one only a person's consent opens); a
    /// socket bound through a link is named by that spelling alone.
    #[must_use]
    pub fn names(&self, bound: &[u8]) -> bool {
        bound == self.bytes || self.canonical.as_deref() == Some(bound)
    }

    /// Whether `bound` is this path's private sibling `<path>.<pid>.<n>`, `path` as
    /// [`Self::names`] takes it and each number one or more decimal digits: the name an astream
    /// broker binds its listener at before it links this path to that socket and removes the name
    /// (`Broker::bind_unix`). Only the shape is read: the pid is its binder's own words, which no
    /// rule compares.
    #[must_use]
    pub fn is_sibling(&self, bound: &[u8]) -> bool {
        fn before_number(name: &[u8]) -> Option<&[u8]> {
            let dot = name.iter().rposition(|byte| *byte == b'.')?;
            let digits = &name[dot + 1..];
            (!digits.is_empty() && digits.iter().all(u8::is_ascii_digit)).then_some(&name[..dot])
        }
        before_number(bound)
            .and_then(before_number)
            .is_some_and(|path| self.names(path))
    }
}

/// A listener this process binds for one table read to name ([`scan_listener_holders`]): it
/// proves the read lays out a path, a user and a pid where this build reads them. It is bound in a
/// folder of its own, made for it (mode 0700, an unpredictable name), so no name another user laid
/// down first (a link `bind(2)` would follow, a file) stands where it binds. Dropping it closes it
/// and removes its file and its folder.
#[cfg(target_vendor = "apple")]
struct TableCheck {
    dir: std::path::PathBuf,
    path: std::path::PathBuf,
    _listener: std::os::unix::net::UnixListener,
}

#[cfg(target_vendor = "apple")]
impl TableCheck {
    /// Under this process's temporary folder, else under `/tmp`: a `TMPDIR` that is relative, too
    /// deep for `sun_path`, missing or closed does not stop the scan.
    fn bind() -> io::Result<Self> {
        Self::bind_in(&[std::env::temp_dir(), std::path::PathBuf::from("/tmp")])
    }

    /// In a new folder under the first of `dirs` that is absolute and takes the folder and the
    /// listener in it.
    fn bind_in(dirs: &[std::path::PathBuf]) -> io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt as _;
        let mut last = io::Error::new(
            io::ErrorKind::InvalidInput,
            "no absolute folder to bind the table check in",
        );
        for parent in dirs.iter().filter(|d| d.is_absolute()) {
            let parent = aterm_tempfile::quiet_in(parent);
            let made = (0..4).find_map(|_| {
                let dir = parent.join(format!("aterm-pcb-{}", crate::rand::hex_token::<6>().ok()?));
                match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
                    Ok(()) => Some(Ok(dir)),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => None,
                    Err(e) => Some(Err(e)),
                }
            });
            let dir = match made {
                Some(Ok(dir)) => dir,
                Some(Err(e)) => {
                    last = e;
                    continue;
                }
                None => {
                    last = io::Error::from(io::ErrorKind::AlreadyExists);
                    continue;
                }
            };
            let path = dir.join("l.sock");
            match std::os::unix::net::UnixListener::bind(&path) {
                Ok(listener) => {
                    return Ok(Self {
                        dir,
                        path,
                        _listener: listener,
                    });
                }
                Err(e) => {
                    drop(std::fs::remove_dir(&dir));
                    last = e;
                }
            }
        }
        Err(last)
    }
}

#[cfg(target_vendor = "apple")]
impl Drop for TableCheck {
    fn drop(&mut self) {
        drop(std::fs::remove_file(&self.path));
        drop(std::fs::remove_dir(&self.dir));
    }
}

/// The kernel's unix-domain stream table ([`aterm_sysprobe::jobs::darwin::UNIX_STREAM_TABLE`]),
/// read whole: sized, then read into that size with room for streams opened between the two
/// calls, again when it outgrew that room.
#[cfg(target_vendor = "apple")]
fn unix_stream_table() -> io::Result<Vec<u8>> {
    unix_stream_table_with(unix_stream_table_once, std::time::Instant::now())
}

#[cfg(target_vendor = "apple")]
fn unix_stream_table_with(
    mut read: impl FnMut() -> io::Result<Vec<u8>>,
    started: std::time::Instant,
) -> io::Result<Vec<u8>> {
    let deadline = started + SCAN_DEADLINE;
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        match read() {
            Ok(table) => return Ok(table),
            Err(error) if error.raw_os_error() == Some(libc::ENOMEM) => {}
            Err(error) => return Err(error),
        }
    }
}

#[cfg(target_vendor = "apple")]
fn unix_stream_table_once() -> io::Result<Vec<u8>> {
    let name = aterm_sysprobe::jobs::darwin::UNIX_STREAM_TABLE;
    let mut len: libc::size_t = 0;
    // SAFETY: a size query: no buffer, and `len` a live `size_t` the call writes.
    let sized = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            std::ptr::null_mut(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if sized != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut table = vec![0u8; len + len / 4 + 4096];
    let mut got: libc::size_t = table.len();
    // SAFETY: `table` is `got` writable bytes; the call writes at most `got` of them and sets
    // `got` to the count it wrote.
    let read = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            table.as_mut_ptr().cast(),
            &raw mut got,
            std::ptr::null_mut(),
            0,
        )
    };
    if read == 0 {
        table.truncate(got);
        return Ok(table);
    }
    Err(io::Error::last_os_error())
}

/// One walk of this uid's processes except `skip`.
#[cfg(target_vendor = "apple")]
fn scan(wanted: &[u128], lookup: Lookup, skip: &[u32]) -> io::Result<Vec<HolderScan>> {
    let mut scans = vec![HolderScan::default(); wanted.len()];
    if wanted.is_empty() {
        return Ok(scans);
    }
    for pid in uid_pids()? {
        if skip.contains(&pid) {
            continue;
        }
        for (scan, read) in scans.iter_mut().zip(read_table(pid, wanted, lookup)) {
            scan.scanned += 1;
            match read {
                TableRead::Holds => scan.holders.push(pid),
                TableRead::Clear | TableRead::Exited | TableRead::Left => {}
                TableRead::Unreadable => scan.unreadable.push(pid),
            }
        }
    }
    Ok(scans)
}

/// Scan live same-UID processes except `skip` for each device in `rdevs`.
/// # Errors
/// Always `Unsupported`.
#[cfg(not(target_vendor = "apple"))]
pub fn scan_holders_of(_rdevs: &[u64], _skip: &[u32]) -> io::Result<Vec<HolderScan>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the holder scan reads libproc (macOS)",
    ))
}

/// Whether any standard descriptor (`0..=2`) of `pid` sits on a revoked vnode.
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn stdio_on_dead_vnode(pid: u32) -> Option<bool> {
    let pid = i32::try_from(pid).ok()?;
    for (fd, kind) in list_fds(pid).ok()? {
        if !(0..=2).contains(&fd) || kind != ffi::PROX_FDTYPE_VNODE {
            continue;
        }
        match vnode_of(pid, fd, Lookup::BY_RDEV) {
            Ok(_) | Err(Failed::Closed) => {}
            Err(Failed::Dead) => return Some(true),
            Err(Failed::Exited | Failed::Refused | Failed::Other) => return None,
        }
    }
    Some(false)
}

/// Whether any standard descriptor (`0..=2`) of `pid` sits on a revoked vnode.
#[cfg(not(target_vendor = "apple"))]
#[must_use]
pub fn stdio_on_dead_vnode(_pid: u32) -> Option<bool> {
    None
}

/// Whether `pid`'s descriptor `fd` is a terminal, by the rule the job probe reads a descriptor
/// with ([`aterm_sysprobe::jobs::terminal_path`]): a descriptor that is closed or no vnode (a
/// pipe, a socket; the kernel answers `EBADF` for each) is none. `None` when it does not read:
/// a revoked vnode (a terminal whose pty closed, or a file on a volume forced off), a process
/// gone or refused, a short record.
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn fd_is_terminal(pid: u32, fd: i32) -> Option<bool> {
    use aterm_sysprobe::jobs::darwin;
    let pid = i32::try_from(pid).ok()?;
    let mut buf = vec![0u8; darwin::VNODE_PATH_SIZE];
    let size = i32::try_from(buf.len()).ok()?;
    clear_errno();
    // SAFETY: `buf` has `size` writable bytes, the size `PROC_PIDFDVNODEPATHINFO` writes.
    let got = unsafe {
        ffi::proc_pidfdinfo(
            pid,
            fd,
            darwin::PROC_PIDFDVNODEPATHINFO,
            buf.as_mut_ptr().cast(),
            size,
        )
    };
    let read = if got <= 0 {
        Err(Failed::last())
    } else {
        usize::try_from(got)
            .ok()
            .and_then(|got| buf.get(..got))
            .ok_or(Failed::Other)
    };
    vnode_terminal(read)
}

/// What a `PROC_PIDFDVNODEPATHINFO` read of a descriptor says of a terminal ([`fd_is_terminal`]).
#[cfg(target_vendor = "apple")]
fn vnode_terminal(read: Result<&[u8], Failed>) -> Option<bool> {
    use aterm_sysprobe::jobs::darwin;
    match read {
        Ok(record) => darwin::vnode(record).map(|v| v.terminal()),
        Err(Failed::Closed) => Some(false),
        Err(Failed::Dead | Failed::Exited | Failed::Refused | Failed::Other) => None,
    }
}

/// See the Apple twin: `/proc/<pid>/fd/<fd>`'s link, judged by
/// [`aterm_sysprobe::jobs::linux::parse_fd_link`]; a terminal the kernel marks ` (deleted)` is
/// the revoked vnode's twin, and a link that is gone is a closed descriptor only while the
/// process is not exiting.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[must_use]
pub fn fd_is_terminal(pid: u32, fd: i32) -> Option<bool> {
    match std::fs::read_link(format!("/proc/{pid}/fd/{fd}")) {
        Ok(link) => link_terminal(link.to_str()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => closed_answer(
            crate::process::read_proc_text(std::path::Path::new(&format!("/proc/{pid}/stat")))
                .ok()
                .as_deref(),
        ),
        Err(_) => None,
    }
}

/// What a descriptor's `/proc` link (`None`: no UTF-8) says of a terminal: a terminal's name is
/// one, unless the kernel marks it ` (deleted)` (a pty whose master closed), which proves nothing.
#[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
fn link_terminal(link: Option<&str>) -> Option<bool> {
    use aterm_sysprobe::jobs::linux::{FdLink, parse_fd_link};
    match link {
        Some(t) if parse_fd_link(t) == FdLink::Tty => (!t.ends_with(" (deleted)")).then_some(true),
        _ => Some(false),
    }
}

/// A descriptor `/proc` no longer lists, by the process's `stat` line read after it: closed
/// while the process is not exiting, and unknown once it is (its exit closes every descriptor
/// before it is a zombie), or when the line does not read.
#[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
fn closed_answer(stat: Option<&str>) -> Option<bool> {
    let line = aterm_sysprobe::jobs::linux::parse_stat(stat?)?;
    (!line.exiting()).then_some(false)
}

/// See the Apple twin: no reader here.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
#[must_use]
pub const fn fd_is_terminal(_pid: u32, _fd: i32) -> Option<bool> {
    None
}

/// Whether process `pid` holds a descriptor for `rdev`.
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn process_holds_rdev(pid: u32, rdev: u64) -> Option<bool> {
    match read_table(pid, &[u128::from(rdev)], Lookup::BY_RDEV).first() {
        Some(TableRead::Holds) => Some(true),
        Some(TableRead::Clear | TableRead::Exited) => Some(false),
        Some(TableRead::Unreadable | TableRead::Left) | None => None,
    }
}

/// What reading one process's descriptor table for one device or listener found.
#[cfg(target_vendor = "apple")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TableRead {
    /// A descriptor for the device.
    Holds,
    /// No descriptor for it.
    Clear,
    /// The process has exited.
    Exited,
    /// The process runs as another effective user now, outside a scan of this user's
    /// processes ([`left_the_user`]).
    Left,
    /// Unknown: the table was refused, or a read failed otherwise.
    Unreadable,
}

/// Why one libproc read failed, by its `errno`.
#[cfg(target_vendor = "apple")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failed {
    /// `ESRCH`: no such process.
    Exited,
    /// `EBADF`: the descriptor was closed, or its number reused for another kind, since the
    /// listing.
    Closed,
    /// `ENOENT`, reading one descriptor or a working directory.
    Dead,
    /// `EPERM`: the kernel will not tell this process.
    Refused,
    /// Anything else (`EACCES`, or a failure that set no `errno`).
    Other,
}

#[cfg(target_vendor = "apple")]
impl Failed {
    const ESRCH: i32 = 3;
    const EPERM: i32 = 1;
    const EBADF: i32 = 9;
    /// A descriptor's or a working directory's read's answer for a dead vnode.
    const ENOENT: i32 = 2;

    const fn of(errno: i32) -> Self {
        match errno {
            Self::ESRCH => Self::Exited,
            Self::EBADF => Self::Closed,
            Self::ENOENT => Self::Dead,
            Self::EPERM => Self::Refused,
            _ => Self::Other,
        }
    }

    /// The failure the last libproc call on this thread reported ([`clear_errno`]).
    fn last() -> Self {
        let failed = Self::of(errno());
        #[cfg(any(test, debug_assertions))]
        let failed = hook::as_read(failed);
        failed
    }
}

/// What a vnode descriptor names.
#[cfg(target_vendor = "apple")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Vnode {
    /// `vst_dev`.
    dev: u32,
    /// `vst_ino`.
    ino: u64,
    /// `vst_rdev`.
    rdev: u32,
}

#[cfg(target_vendor = "apple")]
impl Vnode {
    /// The device a character special file stands for (`st_rdev`), as a scan's key.
    fn rdev_key(&self) -> u128 {
        u128::from(self.rdev)
    }

    /// The file itself (`st_dev` and `st_ino`), as a scan's key.
    fn file_key(&self) -> u128 {
        (u128::from(self.dev) << 64) | u128::from(self.ino)
    }
}

/// How a scan reads each vnode descriptor, and the key it matches the vnode by.
#[cfg(target_vendor = "apple")]
#[derive(Clone, Copy)]
struct Lookup {
    /// The `proc_pidfdinfo` flavor read.
    flavor: i32,
    /// The size of that flavor's answer.
    size: usize,
    /// The scan's key of the vnode read.
    key: fn(&Vnode) -> u128,
    /// Whether a descriptor refused for its vnode ([`refusal_is_the_vnodes`]) is known not to be
    /// the one sought.
    vnode_refusal_clears: bool,
}

#[cfg(target_vendor = "apple")]
impl Lookup {
    /// A device by its `st_rdev`, read with the descriptor's path (`PROC_PIDFDVNODEPATHINFO`).
    const BY_RDEV: Self = Self {
        flavor: ffi::PROC_PIDFDVNODEPATHINFO,
        size: ffi::VNODE_PATH_SIZE,
        key: Vnode::rdev_key,
        vnode_refusal_clears: false,
    };
    /// A file by its device and inode.
    const BY_FILE: Self = Self {
        flavor: ffi::PROC_PIDFDVNODEINFO,
        size: ffi::VNODE_FD_SIZE,
        key: Vnode::file_key,
        vnode_refusal_clears: true,
    };
}

/// Whether the kernel refused (`EPERM`) each descriptor of `pid` in `refused` for its vnode and
/// not for the process.
// Skip: bottoms out at the libproc FFI calls.
#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
fn refusal_is_the_vnodes(pid: i32, refused: &[i32], lookup: Lookup) -> bool {
    refusal_shown(refused, list_fds(pid), |fd| vnode_of(pid, fd, lookup))
}

/// [`refusal_is_the_vnodes`]' decision over its reads.
#[cfg(target_vendor = "apple")]
fn refusal_shown(
    refused: &[i32],
    relisted: Result<Vec<(i32, u32)>, Failed>,
    mut describe: impl FnMut(i32) -> Result<Vnode, Failed>,
) -> bool {
    let Ok(fds) = relisted else {
        return false;
    };
    let vnodes = || {
        fds.iter()
            .filter(|(_, kind)| *kind == ffi::PROX_FDTYPE_VNODE)
            .map(|(fd, _)| *fd)
    };
    for fd in vnodes().filter(|fd| refused.contains(fd)) {
        match describe(fd) {
            Err(Failed::Refused | Failed::Closed | Failed::Dead) => {}
            Ok(_) | Err(Failed::Exited | Failed::Other) => return false,
        }
    }
    vnodes()
        .filter(|fd| !refused.contains(fd))
        .any(|fd| describe(fd).is_ok())
}

/// What `pid`'s descriptor table says of each key in `wanted`, a key left unknown read
/// [`TableRead::Left`] once `pid` runs as another effective user ([`left_the_user`]).
#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
fn read_table(pid: u32, wanted: &[u128], lookup: Lookup) -> Vec<TableRead> {
    let reads = read_own_table(pid, wanted, lookup);
    if !reads.contains(&TableRead::Unreadable) {
        return reads;
    }
    let (now, me) = effective_users(pid);
    left_the_user(reads, now, me)
}

/// `pid`'s effective uid now (`None`: unread, or a zombie) and this process's.
#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
fn effective_users(pid: u32) -> (Option<u32>, u32) {
    let now = i32::try_from(pid)
        .ok()
        .and_then(crate::process::read_short_info)
        .map(|info| info.pbsi_uid);
    // SAFETY: `geteuid` is a side-effect-free getter.
    (now, unsafe { ffi::geteuid() })
}

/// Whether `pid` runs as another effective user than this process now: outside a scan of this
/// user's processes, which lists by effective uid ([`uid_pids`], [`left_the_user`]).
#[cfg(target_vendor = "apple")]
fn runs_as_another_user(pid: u32) -> bool {
    let (now, me) = effective_users(pid);
    now.is_some_and(|uid| uid != me)
}

/// See the macOS twin. Linux reads the owner of `/proc/<pid>`, which the kernel keeps the
/// process's effective uid ([`uid_pids`]).
#[cfg(target_os = "linux")]
fn runs_as_another_user(pid: u32) -> bool {
    // SAFETY: `geteuid` is a side-effect-free getter.
    let me = unsafe { libc::geteuid() };
    procfs::runs_as_another_user_in(std::path::Path::new(procfs::PROC), pid, me)
}

/// `reads` of a process whose effective user is `now` (`None`: unread), scanned as `me`: each
/// unknown key reads [`TableRead::Left`] when `now` names another user, and a key the table was
/// shown to hold stays [`TableRead::Holds`]. A scan lists only `me`'s processes ([`uid_pids`]),
/// so a process that execs a set-user-ID program (`/bin/ps`, `top`) between the listing and its
/// read is outside it, as it would be listed a moment later; the kernel refuses its table
/// (`EPERM`), which would otherwise leave the whole scan unknown.
#[cfg(target_vendor = "apple")]
fn left_the_user(reads: Vec<TableRead>, now: Option<u32>, me: u32) -> Vec<TableRead> {
    if now.is_none_or(|uid| uid == me) {
        return reads;
    }
    reads
        .into_iter()
        .map(|read| match read {
            TableRead::Unreadable => TableRead::Left,
            TableRead::Holds | TableRead::Clear | TableRead::Exited | TableRead::Left => read,
        })
        .collect()
}

/// [`read_table`]'s read of `pid`'s own table.
#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
fn read_own_table(pid: u32, wanted: &[u128], lookup: Lookup) -> Vec<TableRead> {
    let every = |read| vec![read; wanted.len()];
    let Ok(pid) = i32::try_from(pid) else {
        return every(TableRead::Unreadable);
    };
    let fds = match list_fds(pid) {
        Ok(fds) => fds,
        Err(Failed::Exited) => return every(TableRead::Exited),
        Err(Failed::Closed | Failed::Dead | Failed::Refused | Failed::Other) => {
            return every(TableRead::Unreadable);
        }
    };
    #[cfg(test)]
    hook::after_listing(pid, &fds);
    read_listed_table(
        wanted,
        lookup,
        fds,
        |fd| vnode_of(pid, fd, lookup),
        |refused| refusal_is_the_vnodes(pid, refused, lookup),
    )
}

/// Fold a listed table's vnode reads ([`refusal_is_the_vnodes`]).
#[cfg(target_vendor = "apple")]
fn read_listed_table(
    wanted: &[u128],
    lookup: Lookup,
    fds: Vec<(i32, u32)>,
    mut read: impl FnMut(i32) -> Result<Vnode, Failed>,
    vnodes_refused: impl FnOnce(&[i32]) -> bool,
) -> Vec<TableRead> {
    let every = |read| vec![read; wanted.len()];
    let mut wanted: Vec<(u128, usize)> = wanted.iter().copied().zip(0..).collect();
    wanted.sort_unstable();
    let mut held = vec![false; wanted.len()];
    let mut found = 0;
    let mut unreadable = false;
    let mut refused: Vec<i32> = Vec::new();
    let mut described = false;
    for (fd, kind) in fds {
        if kind != ffi::PROX_FDTYPE_VNODE {
            continue;
        }
        match read(fd) {
            Ok(vnode) => {
                described = true;
                let r = (lookup.key)(&vnode);
                let from = wanted.partition_point(|(w, _)| *w < r);
                for (_, at) in wanted.iter().skip(from).take_while(|(w, _)| *w == r) {
                    if let Some(slot) = held.get_mut(*at)
                        && !*slot
                    {
                        *slot = true;
                        found += 1;
                    }
                }
                if found == held.len() {
                    break;
                }
            }
            Err(Failed::Exited) => return every(TableRead::Exited),
            Err(Failed::Closed | Failed::Dead) => {}
            Err(Failed::Refused) => refused.push(fd),
            Err(Failed::Other) => unreadable = true,
        }
    }
    if !refused.is_empty()
        && found < held.len()
        && !(lookup.vnode_refusal_clears && described && vnodes_refused(&refused))
    {
        unreadable = true;
    }
    held.into_iter()
        .map(|held| match (held, unreadable) {
            (true, _) => TableRead::Holds,
            (false, true) => TableRead::Unreadable,
            (false, false) => TableRead::Clear,
        })
        .collect()
}

/// Whether process `pid` holds a descriptor for `rdev`.
#[cfg(not(target_vendor = "apple"))]
#[must_use]
pub fn process_holds_rdev(_pid: u32, _rdev: u64) -> Option<bool> {
    None
}

/// libproc constants and layout offsets verified against the macOS SDK.
#[cfg(target_vendor = "apple")]
pub const SDK_ROWS: &[(&str, i64)] = &[
    ("PROC_UID_ONLY", ffi::PROC_UID_ONLY as i64),
    ("PROC_PIDLISTFDS", ffi::PROC_PIDLISTFDS as i64),
    (
        "PROC_PIDFDVNODEPATHINFO",
        ffi::PROC_PIDFDVNODEPATHINFO as i64,
    ),
    ("PROX_FDTYPE_VNODE", ffi::PROX_FDTYPE_VNODE as i64),
    ("PROX_FDTYPE_PSHM", ffi::PROX_FDTYPE_PSHM as i64),
    ("sizeof(struct proc_fdinfo)", ffi::FDINFO_SIZE as i64),
    (
        "sizeof(struct vnode_fdinfowithpath)",
        ffi::VNODE_PATH_SIZE as i64,
    ),
    ("PROC_PIDFDVNODEINFO", ffi::PROC_PIDFDVNODEINFO as i64),
    ("sizeof(struct vnode_fdinfo)", ffi::VNODE_FD_SIZE as i64),
    (
        "offsetof(struct vnode_fdinfo, pvi.vi_stat.vst_dev)",
        ffi::VST_DEV as i64,
    ),
    (
        "offsetof(struct vnode_fdinfo, pvi.vi_stat.vst_ino)",
        ffi::VST_INO as i64,
    ),
    (
        "offsetof(struct vnode_fdinfowithpath, pvip.vip_vi.vi_stat.vst_dev)",
        ffi::VST_DEV as i64,
    ),
    (
        "offsetof(struct vnode_fdinfowithpath, pvip.vip_vi.vi_stat.vst_ino)",
        ffi::VST_INO as i64,
    ),
    (
        "offsetof(struct vnode_fdinfowithpath, pvip.vip_vi.vi_stat.vst_rdev)",
        ffi::VST_RDEV as i64,
    ),
    (
        "offsetof(struct vnode_fdinfowithpath, pvip.vip_path)",
        ffi::VNODE_PATH as i64,
    ),
    ("MAXPATHLEN", ffi::MAXPATHLEN as i64),
    ("PROC_PIDVNODEPATHINFO", ffi::PROC_PIDVNODEPATHINFO as i64),
    (
        "sizeof(struct proc_vnodepathinfo)",
        ffi::VNODEPATHINFO_SIZE as i64,
    ),
    (
        "offsetof(struct proc_vnodepathinfo, pvi_cdir.vip_path)",
        ffi::CDIR_PATH as i64,
    ),
    ("PROC_PIDPATHINFO_MAXSIZE", ffi::PIDPATH_MAX as i64),
    ("PROC_PIDREGIONPATHINFO", ffi::PROC_PIDREGIONPATHINFO as i64),
    (
        "sizeof(struct proc_regionwithpathinfo)",
        ffi::REGION_SIZE as i64,
    ),
    (
        "offsetof(struct proc_regionwithpathinfo, prp_prinfo.pri_address)",
        ffi::REGION_ADDRESS as i64,
    ),
    (
        "offsetof(struct proc_regionwithpathinfo, prp_prinfo.pri_size)",
        ffi::REGION_LEN as i64,
    ),
    (
        "offsetof(struct proc_regionwithpathinfo, prp_vip.vip_vi.vi_stat.vst_ino)",
        ffi::REGION_INODE as i64,
    ),
    (
        "offsetof(struct proc_regionwithpathinfo, prp_vip.vip_path)",
        ffi::REGION_PATH as i64,
    ),
    ("PROC_PIDFDPSHMINFO", ffi::PROC_PIDFDPSHMINFO as i64),
    ("sizeof(struct pshm_fdinfo)", ffi::PSHM_FDINFO_SIZE as i64),
    (
        "offsetof(struct pshm_fdinfo, pshminfo.pshm_stat.vst_size)",
        ffi::PSHM_VST_SIZE as i64,
    ),
];

#[cfg(target_vendor = "apple")]
pub(crate) mod ffi {
    pub const PROC_UID_ONLY: u32 = 4;
    pub const PROC_PIDLISTFDS: i32 = 1;
    pub const PROC_PIDFDVNODEPATHINFO: i32 = 2;
    /// `PROC_PIDFDVNODEINFO` flavor.
    pub const PROC_PIDFDVNODEINFO: i32 = 1;
    /// `sizeof(struct vnode_fdinfo)`.
    pub const VNODE_FD_SIZE: usize = 176;
    pub const PROX_FDTYPE_VNODE: u32 = 1;
    /// `PROX_FDTYPE_PSHM` descriptor type.
    pub const PROX_FDTYPE_PSHM: u32 = 3;
    /// `sizeof(struct proc_fdinfo)`.
    pub const FDINFO_SIZE: usize = 8;
    /// `sizeof(struct vnode_fdinfowithpath)`.
    pub const VNODE_PATH_SIZE: usize = 1200;
    /// `offsetof(struct vnode_fdinfowithpath, pvip.vip_vi.vi_stat.vst_dev)`.
    pub const VST_DEV: usize = 24;
    /// `offsetof(struct vnode_fdinfowithpath, pvip.vip_vi.vi_stat.vst_ino)`.
    pub const VST_INO: usize = 32;
    /// `offsetof(struct vnode_fdinfowithpath, pvip.vip_vi.vi_stat.vst_rdev)`.
    pub const VST_RDEV: usize = 140;
    /// `offsetof(struct vnode_fdinfowithpath, pvip.vip_path)`.
    pub const VNODE_PATH: usize = 176;
    /// `MAXPATHLEN`.
    pub const MAXPATHLEN: usize = 1024;
    /// `PROC_PIDVNODEPATHINFO` flavor.
    pub const PROC_PIDVNODEPATHINFO: i32 = 9;
    /// `sizeof(struct proc_vnodepathinfo)`.
    pub const VNODEPATHINFO_SIZE: usize = 2352;
    /// `offsetof(struct proc_vnodepathinfo, pvi_cdir.vip_path)`.
    pub const CDIR_PATH: usize = 152;
    /// `PROC_PIDPATHINFO_MAXSIZE`.
    pub const PIDPATH_MAX: usize = 4096;
    /// `PROC_PIDREGIONPATHINFO` flavor and its SDK-checked result layout.
    pub const PROC_PIDREGIONPATHINFO: i32 = 8;
    pub const REGION_SIZE: usize = 1272;
    pub const REGION_ADDRESS: usize = 80;
    pub const REGION_LEN: usize = 88;
    pub const REGION_INODE: usize = 104;
    pub const REGION_PATH: usize = 248;
    /// `PROC_PIDFDPSHMINFO` flavor.
    pub const PROC_PIDFDPSHMINFO: i32 = 5;
    /// `sizeof(struct pshm_fdinfo)`.
    pub const PSHM_FDINFO_SIZE: usize = 1192;
    /// `offsetof(struct pshm_fdinfo, pshminfo.pshm_stat.vst_size)`.
    pub const PSHM_VST_SIZE: usize = 112;

    unsafe extern "C" {
        pub fn proc_listpids(kind: u32, typeinfo: u32, buffer: *mut i32, size: i32) -> i32;
        pub fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut core::ffi::c_void,
            size: i32,
        ) -> i32;
        pub fn proc_pidfdinfo(
            pid: i32,
            fd: i32,
            flavor: i32,
            buffer: *mut core::ffi::c_void,
            size: i32,
        ) -> i32;
        pub fn proc_pidpath(pid: i32, buffer: *mut core::ffi::c_void, size: u32) -> i32;
        pub fn geteuid() -> u32;
        /// Darwin's thread-local `errno` pointer.
        pub fn __error() -> *mut i32;
    }
}

/// Clear `errno`, so that a libproc call's failure reads as its own.
#[cfg(target_vendor = "apple")]
pub(crate) fn clear_errno() {
    // SAFETY: `__error` returns this thread's live `errno` slot.
    unsafe { *ffi::__error() = 0 };
}

/// This thread's `errno`.
#[cfg(target_vendor = "apple")]
pub(crate) fn errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// List every live process PID owned by this UID ([`pids_of_uid`] of the effective uid).
/// # Errors
/// Returns an error if the process listing fails.
#[cfg(target_vendor = "apple")]
pub fn uid_pids() -> io::Result<Vec<u32>> {
    // SAFETY: `geteuid` is a side-effect-free getter.
    pids_of_uid(unsafe { ffi::geteuid() })
}

/// List every live process PID owned by `uid` (`proc_listpids(PROC_UID_ONLY, uid)`). libproc
/// answers 0 both for a uid with no process and for a failure, which alone sets `errno`.
///
/// # Errors
///
/// The listing failing, or filling its buffer at its cap of 1 Mi pids: a partial list proves no
/// absence.
#[cfg(target_vendor = "apple")]
pub fn pids_of_uid(uid: u32) -> io::Result<Vec<u32>> {
    listed_growing(|pids| {
        let bytes = i32::try_from(std::mem::size_of_val(pids))
            .map_err(|_| io::Error::other("pid list too large"))?;
        clear_errno();
        // SAFETY: `pids` has `bytes` writable bytes of `i32` slots.
        let got = unsafe { ffi::proc_listpids(ffi::PROC_UID_ONLY, uid, pids.as_mut_ptr(), bytes) };
        if got < 0 || (got == 0 && errno() != 0) {
            return Err(io::Error::last_os_error());
        }
        Ok(usize::try_from(got).unwrap_or(0) / std::mem::size_of::<i32>())
    })
}

/// The most pids one listing holds: a listing that still fills this many slots is refused.
#[cfg(any(target_vendor = "apple", test))]
const MAX_LISTED_PIDS: usize = 1 << 20;

/// The positive pids `fill` lists, into a buffer that grows fourfold from 1024 slots while the
/// listing fills it; `fill` answers how many slots it wrote. A listing that fills
/// [`MAX_LISTED_PIDS`] slots is refused, never cut short.
#[cfg(any(target_vendor = "apple", test))]
fn listed_growing(mut fill: impl FnMut(&mut [i32]) -> io::Result<usize>) -> io::Result<Vec<u32>> {
    let mut room = 1024usize;
    loop {
        let mut pids = vec![0i32; room];
        let n = fill(&mut pids)?;
        if n >= room && room < MAX_LISTED_PIDS {
            room *= 4;
            continue;
        }
        if n >= room {
            return Err(io::Error::other(format!(
                "the process listing filled all {room} slots"
            )));
        }
        return Ok(pids
            .into_iter()
            .take(n)
            .filter_map(|p| u32::try_from(p).ok())
            .filter(|p| *p > 0)
            .collect());
    }
}

/// List every live process PID owned by this UID: its effective uid, as macOS's
/// `PROC_UID_ONLY` lists ([`pids_of_uid`]).
///
/// # Errors
/// Returns an error if `/proc` cannot be read.
#[cfg(target_os = "linux")]
pub fn uid_pids() -> io::Result<Vec<u32>> {
    // SAFETY: `geteuid` is a side-effect-free getter.
    pids_of_uid(unsafe { libc::geteuid() })
}

/// List every live process PID whose effective uid is `uid`: the pids [`listed_pids`] lists
/// whose [`euid_of`] is `uid`. The kernel gives root only a non-dumpable process's files inside
/// its directory, so root's listing holds no other uid's non-dumpable process.
///
/// # Errors
/// Returns an error if `/proc` cannot be read whole.
#[cfg(target_os = "linux")]
pub fn pids_of_uid(uid: u32) -> io::Result<Vec<u32>> {
    procfs::uid_pids_in(std::path::Path::new(procfs::PROC), uid)
}

/// Every pid `/proc` lists, of every user `/proc` shows: a listing with an entry that does not
/// read is refused, never cut.
///
/// # Errors
/// `/proc` cannot be listed, or one of its entries does not read.
#[cfg(target_os = "linux")]
pub fn listed_pids() -> io::Result<Vec<u32>> {
    procfs::listed_pids_in(std::path::Path::new(procfs::PROC))
}

/// The effective uid `pid` runs as: the owner of its `/proc/<pid>`, which the kernel keeps the
/// effective uid's, dumpable or not.
///
/// # Errors
/// `NotFound` once `pid` is gone; the `stat`'s error otherwise.
#[cfg(target_os = "linux")]
pub fn euid_of(pid: u32) -> io::Result<u32> {
    procfs::euid_in(std::path::Path::new(procfs::PROC), pid)
}

/// List every live process PID owned by this user SID.
/// # Errors
/// Returns an error if the process token or WTS enumeration fails.
#[cfg(windows)]
pub fn uid_pids() -> io::Result<Vec<u32>> {
    win_pids::same_user_pids()
}

/// This process's token-user SID and the session manager's process listing ([`uid_pids`]).
#[cfg(windows)]
mod win_pids {
    use super::io;
    use crate::win::ffi;

    /// `TOKEN_INFORMATION_CLASS::TokenUser`.
    const TOKEN_USER: u32 = 1;
    const TOKEN_QUERY: u32 = 0x0008;
    /// `WTS_CURRENT_SERVER_HANDLE` (this server).
    const WTS_CURRENT_SERVER: ffi::RawHandle = 0;
    /// `WTS_ANY_SESSION` (`(DWORD)-2`).
    const WTS_ANY_SESSION: u32 = 0xFFFF_FFFE;
    /// `WTS_PROCESS_INFO_TYPE::WTSTypeProcessInfoLevel1`.
    const WTS_PROCESS_INFO_LEVEL1: u32 = 1;

    /// This process's token-user SID.
    fn own_sid() -> io::Result<(Vec<u8>, *const core::ffi::c_void)> {
        let mut token: ffi::RawHandle = 0;
        // SAFETY: `GetCurrentProcess` is a pseudo-handle and `token` is a live out-pointer.
        if unsafe { ffi::OpenProcessToken(ffi::GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut len: u32 = 0;
        // SAFETY: null buffer queries the required byte length in `len`.
        unsafe { ffi::GetTokenInformation(token, TOKEN_USER, std::ptr::null_mut(), 0, &mut len) };
        if len == 0 {
            // SAFETY: closes the token opened above.
            let _ = unsafe { ffi::CloseHandle(token) };
            return Err(io::Error::last_os_error());
        }
        let mut buf = vec![0u8; len as usize];
        // SAFETY: `buf` has `len` writable bytes; `TOKEN_USER` fills it with a `SID_AND_ATTRIBUTES`
        // whose `Sid` points inside `buf`.
        let ok = unsafe {
            ffi::GetTokenInformation(token, TOKEN_USER, buf.as_mut_ptr().cast(), len, &mut len)
        };
        // SAFETY: closes the token opened above.
        let _ = unsafe { ffi::CloseHandle(token) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let sid = buf
            .get(..std::mem::size_of::<usize>())
            .and_then(|b| <[u8; std::mem::size_of::<usize>()]>::try_from(b).ok())
            .map(usize::from_ne_bytes)
            .filter(|p| *p != 0)
            .ok_or_else(|| io::Error::other("the token user holds no SID"))?;
        Ok((buf, sid as *const core::ffi::c_void))
    }

    /// [`super::uid_pids`].
    pub(super) fn same_user_pids() -> io::Result<Vec<u32>> {
        let (_buf, own) = own_sid()?;
        let mut level = WTS_PROCESS_INFO_LEVEL1;
        let mut info: *mut ffi::WtsProcessInfoExW = std::ptr::null_mut();
        let mut count: u32 = 0;
        // SAFETY: level and out-params are live locals.
        let ok = unsafe {
            ffi::WTSEnumerateProcessesExW(
                WTS_CURRENT_SERVER,
                &mut level,
                WTS_ANY_SESSION,
                &mut info,
                &mut count,
            )
        };
        if ok == 0 || info.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut pids = Vec::new();
        // SAFETY: `info` points to `count` valid `WtsProcessInfoExW` entries.
        let entries = unsafe { std::slice::from_raw_parts(info, count as usize) };
        for e in entries {
            // SAFETY: `e.pUserSid` and `own` are valid SIDs for the duration of the call.
            let same = !e.pUserSid.is_null()
                && unsafe { ffi::IsValidSid(e.pUserSid) } != 0
                && unsafe { ffi::EqualSid(own, e.pUserSid) } != 0;
            if same && e.ProcessId != 0 {
                pids.push(e.ProcessId);
            }
        }
        // SAFETY: frees the buffer allocated by `WTSEnumerateProcessesExW`.
        let _ = unsafe { ffi::WTSFreeMemoryExW(WTS_PROCESS_INFO_LEVEL1, info.cast(), count) };
        Ok(pids)
    }
}

/// List every live process PID owned by this UID.
/// # Errors
/// Returns `Unsupported` on platforms without a reader.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", windows)))]
pub fn uid_pids() -> io::Result<Vec<u32>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the process listing reads libproc (macOS) or /proc (Linux)",
    ))
}

/// `pid`'s descriptors, each `(fd, type)`.
#[cfg(target_vendor = "apple")]
fn list_fds(pid: i32) -> Result<Vec<(i32, u32)>, Failed> {
    clear_errno();
    // SAFETY: null buffer queries the required byte size.
    let need = unsafe { ffi::proc_pidinfo(pid, ffi::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    if need <= 0 {
        return Err(Failed::last());
    }
    let slots = usize::try_from(need).map_err(|_| Failed::Other)? / ffi::FDINFO_SIZE + 32;
    let mut buf = vec![0u8; slots * ffi::FDINFO_SIZE];
    let size = i32::try_from(buf.len()).map_err(|_| Failed::Other)?;
    clear_errno();
    // SAFETY: `buf` has `size` writable bytes.
    let got =
        unsafe { ffi::proc_pidinfo(pid, ffi::PROC_PIDLISTFDS, 0, buf.as_mut_ptr().cast(), size) };
    if got == 0 && errno() == 0 {
        return Ok(Vec::new());
    }
    if got <= 0 {
        return Err(Failed::last());
    }
    let got = usize::try_from(got)
        .map_err(|_| Failed::Other)?
        .min(buf.len());
    Ok(buf
        .get(..got)
        .ok_or(Failed::Other)?
        .as_chunks::<{ ffi::FDINFO_SIZE }>()
        .0
        .iter()
        .map(|c| {
            let [a, b, c0, d, e, f, g, h] = *c;
            (
                i32::from_ne_bytes([a, b, c0, d]),
                u32::from_ne_bytes([e, f, g, h]),
            )
        })
        .collect())
}

#[cfg(target_vendor = "apple")]
fn vnode_of(pid: i32, fd: i32, lookup: Lookup) -> Result<Vnode, Failed> {
    #[cfg(test)]
    if hook::refuses(pid, fd) {
        return Err(Failed::Refused);
    }
    let mut buf = [0u8; ffi::VNODE_PATH_SIZE];
    let buf = buf.get_mut(..lookup.size).ok_or(Failed::Other)?;
    let size = i32::try_from(buf.len()).map_err(|_| Failed::Other)?;
    clear_errno();
    // SAFETY: `buf` matches the structure size required by `lookup.flavor`.
    let got = unsafe { ffi::proc_pidfdinfo(pid, fd, lookup.flavor, buf.as_mut_ptr().cast(), size) };
    if got <= 0 {
        return Err(Failed::last());
    }
    if got != size {
        return Err(Failed::Other);
    }
    let u32_at = |at: usize| {
        buf.get(at..at + 4)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map(u32::from_ne_bytes)
            .ok_or(Failed::Other)
    };
    let ino = buf
        .get(ffi::VST_INO..ffi::VST_INO + 8)
        .and_then(|b| <[u8; 8]>::try_from(b).ok())
        .map(u64::from_ne_bytes)
        .ok_or(Failed::Other)?;
    Ok(Vnode {
        dev: u32_at(ffi::VST_DEV)?,
        ino,
        rdev: u32_at(ffi::VST_RDEV)?,
    })
}

/// Size of the POSIX shared-memory object named by `fd`, or `None` if not PSHM.
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn fd_is_pshm(fd: std::os::fd::BorrowedFd<'_>) -> Option<u64> {
    use std::os::fd::AsRawFd as _;
    pshm_size(i32::try_from(std::process::id()).ok()?, fd.as_raw_fd())
}

/// Sizes of the POSIX shared-memory objects held by `pid`.
/// # Errors
/// Returns an error if `pid`'s descriptor table cannot be read.
#[cfg(target_vendor = "apple")]
pub fn pshm_sizes_of(pid: u32) -> io::Result<Vec<u64>> {
    let unreadable = || io::Error::other(format!("pid {pid}'s descriptor table is unreadable"));
    let pid = i32::try_from(pid).map_err(|_| unreadable())?;
    let fds = list_fds(pid).map_err(|_| unreadable())?;
    Ok(fds
        .into_iter()
        .filter(|(_, kind)| *kind == ffi::PROX_FDTYPE_PSHM)
        .filter_map(|(fd, _)| pshm_size(pid, fd))
        .collect())
}

/// `pid`'s descriptor `fd`'s shared-memory size.
#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
fn pshm_size(pid: i32, fd: i32) -> Option<u64> {
    let mut buf = [0u8; ffi::PSHM_FDINFO_SIZE];
    let size = i32::try_from(buf.len()).ok()?;
    // SAFETY: `buf` matches the structure size for `PROC_PIDFDPSHMINFO`.
    let got = unsafe {
        ffi::proc_pidfdinfo(
            pid,
            fd,
            ffi::PROC_PIDFDPSHMINFO,
            buf.as_mut_ptr().cast(),
            size,
        )
    };
    if got != size {
        return None;
    }
    let bytes = buf
        .get(ffi::PSHM_VST_SIZE..ffi::PSHM_VST_SIZE + 8)
        .and_then(|b| <[u8; 8]>::try_from(b).ok())?;
    u64::try_from(i64::from_ne_bytes(bytes)).ok()
}

/// Size of the POSIX shared-memory object named by `fd`, or `None`.
#[cfg(all(unix, not(target_vendor = "apple")))]
#[must_use]
pub fn fd_is_pshm(_fd: std::os::fd::BorrowedFd<'_>) -> Option<u64> {
    None
}

/// Sizes of the POSIX shared-memory objects held by `pid`.
/// # Errors
/// Returns `Unsupported` on platforms without a PSHM reader.
#[cfg(all(unix, not(target_vendor = "apple")))]
pub fn pshm_sizes_of(_pid: u32) -> io::Result<Vec<u64>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "a process's shared-memory descriptors are read through libproc (macOS)",
    ))
}

#[cfg(target_os = "linux")]
#[path = "holders_diag.rs"]
mod diag;
#[cfg(all(any(test, debug_assertions), target_vendor = "apple"))]
#[path = "holders_jail.rs"]
mod jail;
#[path = "holders_paths.rs"]
mod paths;
#[cfg(any(target_os = "linux", all(test, unix)))]
#[path = "holders_procfs.rs"]
mod procfs;
#[cfg(all(any(test, debug_assertions), target_vendor = "apple"))]
pub use jail::lab_jail_pids;

/// LAB SEAM: the variable in which aterm-lab names the mark of the jail its instance runs in, so
/// that a debug build's path scans read that jail's processes alone ([`lab_jail_pids`], macOS).
pub const JAIL_SEAM: &str = "ATERM_DEBUG_JAIL_MARK";
#[cfg(all(debug_assertions, target_vendor = "apple"))]
pub use paths::a_force_detached_volume_is_no_working_directory;
#[cfg(all(windows, any(test, debug_assertions)))]
pub use paths::scan_path_users_of_within;
pub use paths::{
    Exe, PathScan, PathUse, cwd_of, exe_of, open_files, scan_path_users, scan_path_users_of,
    write_open_files,
};

/// Test seams: run a closure between one table's listing and the reads of its descriptors (to
/// make a race certain), refuse chosen descriptor reads (`EPERM`), read a dead vnode as unknown
/// (for a negative control), and read a process's working directory as a dead vnode (a scratch
/// folder standing in for a volume force-unmounted under it).
#[cfg(all(any(test, debug_assertions), target_vendor = "apple"))]
mod hook {
    use std::cell::Cell;
    #[cfg(test)]
    use std::cell::RefCell;

    use super::Failed;

    #[cfg(test)]
    type Hook = Box<dyn FnOnce(i32, &[(i32, u32)])>;

    /// The pid whose reads [`refusing`] refuses, and each refused descriptor with the reads of it
    /// left to refuse.
    #[cfg(test)]
    type Refusing = (i32, Vec<(i32, usize)>);

    thread_local! {
        #[cfg(test)]
        static AFTER_LISTING: RefCell<Option<Hook>> = const { RefCell::new(None) };
        #[cfg(test)]
        static REFUSING: RefCell<Option<Refusing>> = const { RefCell::new(None) };
        static DEAD_AS_UNKNOWN: Cell<bool> = const { Cell::new(false) };
        #[cfg(test)]
        static CWD_DEAD: Cell<Option<i32>> = const { Cell::new(None) };
    }

    /// Run `f` with reads of `pid`'s working directory returning `ENOENT`.
    #[cfg(test)]
    pub fn reading_cwd_dead<R>(pid: i32, f: impl FnOnce() -> R) -> R {
        CWD_DEAD.with(|d| d.set(Some(pid)));
        let out = f();
        CWD_DEAD.with(|d| d.set(None));
        out
    }

    /// Whether this thread reads `pid`'s working directory as a dead vnode.
    #[cfg(test)]
    pub(super) fn cwd_dead(pid: i32) -> bool {
        CWD_DEAD.with(Cell::get) == Some(pid)
    }

    /// Run `f` with dead-vnode `ENOENT` results treated as unknown failures.
    pub fn reading_dead_as_unknown<R>(f: impl FnOnce() -> R) -> R {
        DEAD_AS_UNKNOWN.with(|d| d.set(true));
        let out = f();
        DEAD_AS_UNKNOWN.with(|d| d.set(false));
        out
    }

    /// `failed`, as this thread classifies it now.
    pub(super) fn as_read(failed: Failed) -> Failed {
        if failed == Failed::Dead && DEAD_AS_UNKNOWN.with(Cell::get) {
            Failed::Other
        } else {
            failed
        }
    }

    /// The `reads` for [`refusing`] that refuses every read, as the kernel refuses a vnode it
    /// keeps from this process.
    #[cfg(test)]
    pub const ALWAYS: usize = usize::MAX;

    /// Run `f` with this thread's first `reads` reads of each of `fds` of `pid` refused (`EPERM`)
    /// ([`ALWAYS`]: every read).
    #[cfg(test)]
    pub fn refusing<R>(pid: u32, fds: &[i32], reads: usize, f: impl FnOnce() -> R) -> R {
        let pid = i32::try_from(pid).expect("a pid");
        let fds = fds.iter().map(|fd| (*fd, reads)).collect();
        REFUSING.with(|r| *r.borrow_mut() = Some((pid, fds)));
        let out = f();
        REFUSING.with(|r| *r.borrow_mut() = None);
        out
    }

    /// Whether this thread refuses this read of `pid`'s `fd` ([`refusing`]), counting it.
    #[cfg(test)]
    pub(super) fn refuses(pid: i32, fd: i32) -> bool {
        REFUSING.with(|r| {
            let mut r = r.borrow_mut();
            let Some((refused_pid, fds)) = r.as_mut() else {
                return false;
            };
            if *refused_pid != pid {
                return false;
            }
            match fds.iter_mut().find(|(n, _)| *n == fd) {
                Some((_, 0)) | None => false,
                Some((_, ALWAYS)) => true,
                Some((_, left)) => {
                    *left -= 1;
                    true
                }
            }
        })
    }

    /// Run `f` once after the next descriptor listing on this thread.
    #[cfg(test)]
    pub fn after_next_listing(f: impl FnOnce(i32, &[(i32, u32)]) + 'static) {
        AFTER_LISTING.with(|h| *h.borrow_mut() = Some(Box::new(f)));
    }

    #[cfg(test)]
    pub(super) fn after_listing(pid: i32, fds: &[(i32, u32)]) {
        if let Some(f) = AFTER_LISTING.with(|h| h.borrow_mut().take()) {
            f(pid, fds);
        }
    }
}

#[cfg(test)]
#[path = "holders_listing_tests.rs"]
mod listing_tests;

#[cfg(all(test, target_vendor = "apple"))]
#[path = "holders_tests.rs"]
mod tests;
