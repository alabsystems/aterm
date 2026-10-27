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
    procfs::within(procfs::SCAN_DEADLINE, move || {
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
/// after that number is reused, the unrelated live process of this user that has it. A bound path
/// names `path` as [`SocketPath::names`] says. Each scan finds a listener of its own in the
/// same table read, bound for the check, so a table this build misreads is refused, never read as
/// no holder.
///
/// # Errors
/// The process listing or the table read failing, or the check's listener not binding or not
/// found in the table as this build reads it (`Unsupported`).
#[cfg(target_vendor = "apple")]
pub fn scan_listener_holders(path: &std::path::Path, skip: &[u32]) -> io::Result<HolderScan> {
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
        &uid_pids()?,
        skip,
    )
}

/// The holders one table read's listeners `rows` name ([`scan_listener_holders`]): the pid of
/// each listener `own`'s user made, bound at a path `target` names, once, when it is `live` and
/// not in `skip`. `scanned` counts `live` but `skip`.
///
/// # Errors
/// `Unsupported` when `rows` is no table read (`None`), or a read without `own`, the check's
/// listener: a table this build misreads.
#[cfg(target_vendor = "apple")]
fn listener_holders(
    rows: Option<&[aterm_sysprobe::jobs::darwin::UnixListenerRow<'_>]>,
    own: aterm_sysprobe::jobs::darwin::UnixListenerRow<'_>,
    target: &SocketPath,
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
    for row in rows {
        let holds = row.uid == own.uid && target.names(row.path);
        if holds && live.contains(&row.pid) && !skip.contains(&row.pid) && !scan.holds(row.pid) {
            scan.holders.push(row.pid);
        }
    }
    Ok(scan)
}

/// Scan live same-UID processes except `skip` for a listening `AF_UNIX` socket bound at `path`,
/// by each socket's own kernel record, never by a process's arguments: `/proc/net/unix`'s
/// listening rows ([`aterm_sysprobe::jobs::linux::listening_unix`]) matched with each
/// process's `/proc/<pid>/fd` links (`holders_procfs.rs`). A bound path names `path` as
/// [`SocketPath::names`] says. A table only root reads is in [`HolderScan::sealed`].
///
/// # Errors
/// `/proc/net/unix` or the process listing cannot be read, or the reads take longer than 10 s
/// (`TimedOut`).
#[cfg(target_os = "linux")]
pub fn scan_listener_holders(path: &std::path::Path, skip: &[u32]) -> io::Result<HolderScan> {
    // SAFETY: `geteuid` is a side-effect-free getter.
    let me = unsafe { libc::geteuid() };
    let (path, skip) = (path.to_path_buf(), skip.to_vec());
    procfs::within(procfs::SCAN_DEADLINE, move || {
        let target = SocketPath::of(&path);
        procfs::scan_listener_holders_in(
            std::path::Path::new(procfs::PROC),
            me,
            |bound| target.names(bound),
            &skip,
        )
    })
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
    let name = aterm_sysprobe::jobs::darwin::UNIX_STREAM_TABLE;
    for _ in 0..4 {
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
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::ENOMEM) {
            return Err(err);
        }
    }
    Err(io::Error::other(
        "the unix-domain stream table outgrew each read's room",
    ))
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
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
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
fn clear_errno() {
    // SAFETY: `__error` returns this thread's live `errno` slot.
    unsafe { *ffi::__error() = 0 };
}

/// This thread's `errno`.
#[cfg(target_vendor = "apple")]
fn errno() -> i32 {
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

/// List every live process PID whose effective uid is `uid`: the pids whose `/proc/<pid>`
/// directory `uid` owns. The kernel keeps that directory the effective uid's, dumpable or not,
/// and gives root only a non-dumpable process's files inside it, so root's listing holds no
/// other uid's non-dumpable process.
///
/// # Errors
/// Returns an error if `/proc` cannot be read.
#[cfg(target_os = "linux")]
pub fn pids_of_uid(uid: u32) -> io::Result<Vec<u32>> {
    procfs::uid_pids_in(std::path::Path::new(procfs::PROC), uid)
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
    Exe, PathScan, PathUse, exe_of, open_files, scan_path_users, scan_path_users_of,
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
mod listing_tests {
    /// A listing that still fills its buffer at the cap is refused, never cut short; one that
    /// leaves a slot free is read whole, a non-positive slot dropped.
    #[test]
    fn a_listing_that_fills_its_buffer_at_the_cap_is_refused() {
        let mut rooms = Vec::new();
        let full = super::listed_growing(|pids| {
            rooms.push(pids.len());
            pids.fill(7);
            Ok(pids.len())
        });
        assert!(
            full.is_err(),
            "a listing cut at the cap: {:?}",
            full.map(|p| p.len())
        );
        assert_eq!(rooms.last(), Some(&super::MAX_LISTED_PIDS), "{rooms:?}");
        let mut asked = 0;
        let grown = super::listed_growing(|pids| {
            asked += 1;
            let n = 1500.min(pids.len());
            for (at, slot) in pids.iter_mut().take(n).enumerate() {
                *slot = i32::try_from(at).expect("small");
            }
            Ok(n)
        })
        .expect("a listing with room to spare");
        assert_eq!(asked, 2, "1024 slots filled, then 4096 with room");
        assert_eq!(grown.len(), 1499, "pid 0 is no process");
        assert!(super::listed_growing(|_| Err(std::io::Error::other("refused"))).is_err());
    }

    /// The effective uid, as the kernel names this process's owner.
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    fn euid() -> u32 {
        // SAFETY: `geteuid` is a side-effect-free getter.
        unsafe { libc::geteuid() }
    }

    /// A uid's listing holds that uid's processes and no other's: this process is in its own
    /// uid's list and not in root's, where launchd or init (pid 1) is.
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    #[test]
    fn pids_of_uid_lists_the_uid_it_is_given() {
        let me = std::process::id();
        let mine = super::pids_of_uid(euid()).expect("this uid's processes");
        assert!(mine.contains(&me), "this process in its own uid's list");
        assert!(
            super::uid_pids()
                .expect("the own-uid listing")
                .contains(&me),
            "uid_pids is this uid's list"
        );
        if euid() != 0 {
            let root = super::pids_of_uid(0).expect("root's processes");
            assert!(root.contains(&1), "pid 1 is root's: {} pids", root.len());
            assert!(!root.contains(&me), "this process is not root's");
            assert!(!mine.contains(&1), "pid 1 is not this uid's");
        }
    }

    /// A pty's terminal end, opened without making it this process's controlling terminal, and
    /// the master that keeps it open. XNU's lost allocation race (`EREDRIVEOPEN`) is opened
    /// again, within the one-minute hang detector.
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    fn a_terminal() -> (std::os::fd::OwnedFd, std::fs::File) {
        use std::os::fd::FromRawFd as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        const EREDRIVEOPEN: i32 = -6;
        let started = std::time::Instant::now();
        let raw = loop {
            // SAFETY: `posix_openpt` returns a fresh descriptor this test owns.
            let raw = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
            if raw >= 0 {
                break raw;
            }
            let err = std::io::Error::last_os_error();
            assert_eq!(
                err.raw_os_error(),
                Some(EREDRIVEOPEN),
                "posix_openpt: {err}"
            );
            assert!(
                started.elapsed() < std::time::Duration::from_secs(60),
                "posix_openpt lost every race"
            );
        };
        // SAFETY: wraps the fresh descriptor this test owns.
        let master = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        // SAFETY: `grantpt` and `unlockpt` read the master this test owns.
        unsafe {
            assert_eq!(libc::grantpt(raw), 0, "grantpt");
            assert_eq!(libc::unlockpt(raw), 0, "unlockpt");
        }
        // The name is written into this buffer, never `ptsname`'s process-wide one, which a
        // test on another thread could overwrite.
        let mut buf = [0 as libc::c_char; 128];
        #[cfg(target_vendor = "apple")]
        // SAFETY: `TIOCPTYGNAME` writes a NUL-terminated name of at most 128 bytes into `buf`,
        // which has exactly that many.
        let rc = unsafe { libc::ioctl(raw, libc::TIOCPTYGNAME as libc::c_ulong, buf.as_mut_ptr()) };
        #[cfg(not(target_vendor = "apple"))]
        // SAFETY: `ptsname_r` writes a NUL-terminated name of at most `buf.len()` bytes.
        let rc = unsafe { libc::ptsname_r(raw, buf.as_mut_ptr(), buf.len()) };
        assert_eq!(
            rc,
            0,
            "the terminal end's name: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: `rc == 0`: `buf` holds a NUL-terminated name.
        let name = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        let terminal = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open(&name)
            .unwrap_or_else(|e| panic!("open {name}: {e}"));
        (master, terminal)
    }

    /// A child's standard input is a terminal when it is a pty's terminal end, and none when it
    /// is `/dev/null` or a pipe; a descriptor the child does not hold is none. Each child shows
    /// it runs by a line before it is read.
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    #[test]
    fn a_child_s_standard_input_is_a_terminal_only_on_a_pty() {
        use crate::spawnfd::{Child, Stdio, command};
        /// The children, each ended and reaped however the test ends.
        struct Ended(Vec<Child>);
        impl Drop for Ended {
            fn drop(&mut self) {
                for child in &mut self.0 {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        let (_master, terminal) = a_terminal();
        let mut children = Ended(Vec::new());
        for stdin in [Stdio::from(terminal), Stdio::null(), Stdio::piped()] {
            let child = command("/bin/sh")
                .args(["-c", "echo ran; exec /bin/sleep 60"])
                .stdin(stdin)
                .stdout(Stdio::piped())
                .spawn()
                .expect("spawn the shell");
            children.0.push(child);
            let child = children.0.last_mut().expect("the child just kept");
            crate::deadline::prints(child, "ran", crate::deadline::ECHO_HANG);
        }
        let reads: Vec<Option<bool>> = children
            .0
            .iter()
            .map(|child| super::fd_is_terminal(child.id(), 0))
            .collect();
        let unheld = super::fd_is_terminal(children.0[0].id(), 37);
        drop(children);
        assert_eq!(
            reads,
            [Some(true), Some(false), Some(false)],
            "a pty, /dev/null, a pipe"
        );
        assert_eq!(unheld, Some(false), "a descriptor the child does not hold");
    }

    /// A descriptor's vnode read answers whether it is a terminal only from a whole record or a
    /// closed descriptor: a revoked vnode, a process gone or refused, another failure and a short
    /// record prove nothing.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_vnode_read_answers_for_a_terminal_only_when_it_reads() {
        use super::Failed;
        use aterm_sysprobe::jobs::darwin;
        let record = |path: &[u8]| {
            let mut b = vec![0u8; darwin::VNODE_PATH_SIZE];
            let mode = (darwin::S_IFCHR | 0o620).to_ne_bytes();
            b[darwin::VNODE_MODE_AT..darwin::VNODE_MODE_AT + mode.len()].copy_from_slice(&mode);
            b[darwin::VNODE_PATH_AT..darwin::VNODE_PATH_AT + path.len()].copy_from_slice(path);
            b
        };
        let (pty, null) = (record(b"/dev/ttys003"), record(b"/dev/null"));
        assert_eq!(super::vnode_terminal(Ok(&pty)), Some(true), "a pty");
        assert_eq!(super::vnode_terminal(Ok(&null)), Some(false), "/dev/null");
        assert_eq!(
            super::vnode_terminal(Ok(&pty[..darwin::VNODE_PATH_SIZE - 1])),
            None,
            "a short record"
        );
        assert_eq!(
            super::vnode_terminal(Err(Failed::Closed)),
            Some(false),
            "a closed descriptor, a pipe or a socket"
        );
        for failed in [Failed::Dead, Failed::Exited, Failed::Refused, Failed::Other] {
            assert_eq!(super::vnode_terminal(Err(failed)), None, "{failed:?}");
        }
    }

    /// A Linux `fd/<n>` link names a terminal by the job probe's rule; a terminal the kernel marks
    /// ` (deleted)` (a pty whose master closed) proves nothing, as a revoked vnode on macOS; a
    /// file is none, deleted or not.
    #[cfg(unix)]
    #[test]
    fn a_descriptor_link_is_a_terminal_by_its_name() {
        for (link, terminal) in [
            ("/dev/pts/3", Some(true)),
            ("/dev/pts/3 (deleted)", None),
            ("/dev/null", Some(false)),
            ("pipe:[4711]", Some(false)),
            ("/tmp/in (deleted)", Some(false)),
        ] {
            assert_eq!(super::link_terminal(Some(link)), terminal, "{link}");
        }
        assert_eq!(
            super::link_terminal(None),
            Some(false),
            "a link that is no UTF-8 names no terminal"
        );
    }

    /// A descriptor `/proc` no longer lists is closed only while its process, read after it, is
    /// not exiting: an exit closes every descriptor before the process is a zombie.
    #[cfg(unix)]
    #[test]
    fn a_missing_descriptor_is_closed_only_while_its_process_is_not_exiting() {
        let stat = |flags: u32| {
            format!("4711 (aterm) S 1 4711 4711 0 -1 {flags} 0 0 0 0 1 2 0 0 20 0 1 0 900 0 0")
        };
        assert_eq!(
            super::closed_answer(Some(&stat(0x0040_0000))),
            Some(false),
            "a live process closed it"
        );
        assert_eq!(
            super::closed_answer(Some(&stat(0x0040_0004))),
            None,
            "an exiting process closes them all"
        );
        assert_eq!(super::closed_answer(Some("4711 (cut")), None, "a torn line");
        assert_eq!(super::closed_answer(None), None, "the process is gone");
    }

    /// A uid with no live process lists none: an answer, never an error.
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    #[test]
    fn a_uid_with_no_process_lists_none() {
        let unused = 2_000_000_011;
        assert_eq!(
            super::pids_of_uid(unused).map_err(|e| e.to_string()),
            Ok(Vec::new())
        );
    }

    /// A listener is found by the path it is bound at, read from the socket's own record, and by
    /// a spelling whose folder resolves to that one; a listener bound through a link is found by
    /// that spelling alone. A listener bound elsewhere, or closed in every process holding it, is
    /// not found.
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    #[test]
    fn this_process_holds_the_listener_it_bound() {
        use std::os::unix::net::{UnixListener, UnixStream};
        let me = std::process::id();
        let dir = aterm_tempfile::Builder::new()
            .tempdir_in("/tmp")
            .expect("a scratch folder short enough for sun_path");
        let base = dir
            .path()
            .canonicalize()
            .expect("the scratch folder's own spelling");
        let (real, link) = (base.join("real"), base.join("link"));
        std::fs::create_dir(&real).expect("the real folder");
        std::os::unix::fs::symlink(&real, &link).expect("a link to it");
        let (a, b, c) = (
            real.join("a.sock"),
            real.join("b.sock"),
            link.join("c.sock"),
        );
        let l1 = UnixListener::bind(&a).expect("bind a");
        let l2 = UnixListener::bind(&b).expect("bind b");
        let l3 = UnixListener::bind(&c).expect("bind c through the link");
        let client = UnixStream::connect(&a).expect("connect a");
        let (accepted, _) = l1.accept().expect("accept");
        let scan = |path: &std::path::Path| {
            super::scan_listener_holders(path, &[]).expect("the listener scan")
        };
        let found = scan(&a);
        assert!(found.holds(me), "{found:?}");
        assert!(found.scanned > 0);
        assert!(
            scan(&link.join("a.sock")).holds(me),
            "the same socket through a link to its folder"
        );
        assert!(scan(&b).holds(me), "the other listener");
        assert!(
            scan(&c).holds(me),
            "a listener bound through a link, by that spelling"
        );
        assert!(
            !scan(&real.join("c.sock")).holds(me),
            "a listener bound through a link is not resolved to its folder's own spelling"
        );
        assert!(
            !super::scan_listener_holders(&a, &[me])
                .expect("the listener scan")
                .holds(me),
            "skip is honoured"
        );
        drop((client, accepted, l1));
        std::fs::remove_file(&a).expect("unlink a");
        // A child another test is spawning holds a copy of the listener until its exec closes
        // it, and the socket, its binder named, lives until every copy is closed: the wait is a
        // hang detector.
        let started = std::time::Instant::now();
        let gone = loop {
            let gone = scan(&a);
            if !gone.holds(me) || started.elapsed() >= std::time::Duration::from_secs(60) {
                break gone;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(
            !gone.holds(me),
            "a closed listener is held by nobody, with another still listening here: {gone:?}"
        );
        assert!(scan(&b).holds(me), "the other listener still");
        drop((l2, l3));
    }
}

#[cfg(all(test, target_vendor = "apple"))]
mod tests {
    use std::os::unix::fs::MetadataExt as _;
    use std::time::{Duration, Instant};

    use super::{ffi, hook};
    use crate::exitwatch::ExitWatch;

    const HANG: Duration = Duration::from_secs(60);

    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
        fn posix_openpt(flags: i32) -> i32;
        fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    }

    const LOW: i32 = 210;
    const HIGH: i32 = 240;

    fn dup_at(fd: &std::os::fd::OwnedFd, at: i32) -> std::os::fd::OwnedFd {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        const F_DUPFD_CLOEXEC: i32 = 67;
        // SAFETY: `F_DUPFD_CLOEXEC` duplicates a valid descriptor we own.
        let high = unsafe { fcntl(fd.as_raw_fd(), F_DUPFD_CLOEXEC, at) };
        assert!(
            high >= at,
            "F_DUPFD_CLOEXEC at {at}: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: wraps the newly duplicated descriptor.
        unsafe { std::os::fd::OwnedFd::from_raw_fd(high) }
    }

    const EREDRIVEOPEN: i32 = -6;

    /// A fresh PTY master (a device no other process holds) at the lowest free number at or
    /// above `at`, and its rdev.
    pub(super) fn a_master_at(at: i32) -> (std::os::fd::OwnedFd, u64) {
        use std::os::fd::FromRawFd as _;
        const O_RDWR: i32 = 2;
        const O_NOCTTY: i32 = 0x20000;
        const O_CLOEXEC: i32 = 0x0100_0000;
        let started = Instant::now();
        let raw = loop {
            // SAFETY: `posix_openpt` returns a fresh descriptor owned by this test.
            let raw = unsafe { posix_openpt(O_RDWR | O_NOCTTY | O_CLOEXEC) };
            if raw >= 0 {
                break raw;
            }
            let err = std::io::Error::last_os_error();
            assert_eq!(
                err.raw_os_error(),
                Some(EREDRIVEOPEN),
                "posix_openpt: {err}"
            );
            assert!(
                started.elapsed() < HANG,
                "posix_openpt lost every race for {HANG:?}"
            );
        };
        // SAFETY: wraps the fresh descriptor owned by this test.
        let low = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        let fd = dup_at(&low, at);
        drop(low);
        let rdev = std::fs::File::from(fd.try_clone().expect("dup"))
            .metadata()
            .expect("fstat")
            .rdev();
        (fd, rdev)
    }

    #[test]
    fn a_descriptor_closed_between_the_listing_and_its_read_is_not_held() {
        use std::os::fd::AsRawFd as _;
        let me = std::process::id();
        let (fd, rdev) = a_master_at(LOW);
        let n = fd.as_raw_fd();
        assert_eq!(super::process_holds_rdev(me, rdev), Some(true), "held");
        let named = std::rc::Rc::new(std::cell::Cell::new(false));
        let seen = std::rc::Rc::clone(&named);
        hook::after_next_listing(move |_, fds| {
            seen.set(fds.contains(&(n, ffi::PROX_FDTYPE_VNODE)));
            drop(fd);
        });
        assert_eq!(
            super::process_holds_rdev(me, rdev),
            Some(false),
            "closed after the listing: not held, and not unknown"
        );
        assert!(named.get(), "the listing named the descriptor it closed");

        let (low, rdev) = a_master_at(LOW);
        let kept = dup_at(&low, HIGH);
        let (n, k) = (low.as_raw_fd(), kept.as_raw_fd());
        let order = std::rc::Rc::new(std::cell::Cell::new(None));
        let seen = std::rc::Rc::clone(&order);
        hook::after_next_listing(move |_, fds| {
            let at = |fd| fds.iter().position(|e| *e == (fd, ffi::PROX_FDTYPE_VNODE));
            seen.set(Some((at(n), at(k))));
            drop(low);
        });
        assert_eq!(
            super::process_holds_rdev(me, rdev),
            Some(true),
            "the closed descriptor's EBADF ends only its own read"
        );
        match order.get() {
            Some((Some(closed), Some(open))) => assert!(
                closed < open,
                "the scan meets the closed descriptor before the open one"
            ),
            other => panic!("the listing named both descriptors: {other:?}"),
        }
        drop(kept);

        use super::Failed;
        assert_eq!(Failed::of(3), Failed::Exited, "ESRCH");
        assert_eq!(Failed::of(9), Failed::Closed, "EBADF");
        assert_eq!(Failed::of(2), Failed::Dead, "ENOENT");
        assert_eq!(Failed::of(1), Failed::Refused, "EPERM");
        for other in [0, 12, 13, 22] {
            assert_eq!(Failed::of(other), Failed::Other, "errno {other}");
        }

        let vnode = super::Vnode {
            dev: 7,
            ino: 9,
            rdev: 11,
        };
        let unasked = |_: &[i32]| -> bool { panic!("a refusal confirmed where none is asked") };
        for lookup in [super::Lookup::BY_FILE, super::Lookup::BY_RDEV] {
            let wanted = (lookup.key)(&vnode);
            let fds = || vec![(3, ffi::PROX_FDTYPE_VNODE)];
            assert_eq!(
                super::read_listed_table(
                    &[wanted],
                    lookup,
                    fds(),
                    |_| Err(Failed::Refused),
                    unasked
                ),
                [super::TableRead::Unreadable],
                "EPERM is unknown for every unconfirmed key"
            );
            assert_eq!(
                super::read_listed_table(&[wanted], lookup, fds(), |_| Ok(vnode), unasked),
                [super::TableRead::Holds],
                "a positive match remains a positive match"
            );
            assert_eq!(
                super::read_listed_table(
                    &[wanted, wanted + 1],
                    lookup,
                    vec![(3, ffi::PROX_FDTYPE_VNODE), (4, ffi::PROX_FDTYPE_VNODE)],
                    |fd| if fd == 3 {
                        Err(Failed::Refused)
                    } else {
                        Ok(vnode)
                    },
                    |_| false,
                ),
                [super::TableRead::Holds, super::TableRead::Unreadable],
                "a refusal obscures only keys not positively found"
            );
        }
    }

    #[test]
    fn a_refusal_shown_to_be_the_vnodes_is_not_the_file() {
        use super::{Failed, Lookup, TableRead};
        const OTHER: super::Vnode = super::Vnode {
            dev: 7,
            ino: 9,
            rdev: 11,
        };
        const SOUGHT: super::Vnode = super::Vnode {
            ino: 10,
            rdev: 12,
            ..OTHER
        };
        let fds = || vec![(3, ffi::PROX_FDTYPE_VNODE), (4, ffi::PROX_FDTYPE_VNODE)];
        let read: fn(i32) -> Result<super::Vnode, Failed> = |fd| {
            if fd == 3 {
                Err(Failed::Refused)
            } else {
                Ok(OTHER)
            }
        };
        let fold = |lookup: Lookup, read: fn(i32) -> Result<super::Vnode, Failed>, shown| {
            let asked = std::cell::Cell::new(None);
            let answer = super::read_listed_table(
                &[(lookup.key)(&SOUGHT)],
                lookup,
                fds(),
                read,
                |refused: &[i32]| {
                    asked.set(Some(refused.to_vec()));
                    shown
                },
            );
            (answer, asked.take())
        };
        assert_eq!(
            fold(Lookup::BY_FILE, read, true),
            (vec![TableRead::Clear], Some(vec![3])),
            "the vnode's refusal: not the file"
        );
        assert_eq!(
            fold(Lookup::BY_FILE, read, false),
            (vec![TableRead::Unreadable], Some(vec![3])),
            "a refusal not shown to be the vnode's"
        );
        assert_eq!(
            fold(Lookup::BY_FILE, |_| Err(Failed::Refused), true),
            (vec![TableRead::Unreadable], None),
            "no descriptor described: the process may have refused them all"
        );
        assert_eq!(
            fold(Lookup::BY_RDEV, read, true),
            (vec![TableRead::Unreadable], None),
            "a device's scan keeps a refusal unknown"
        );
        let held: fn(i32) -> Result<super::Vnode, Failed> = |fd| {
            if fd == 3 {
                Err(Failed::Refused)
            } else {
                Ok(SOUGHT)
            }
        };
        assert_eq!(
            fold(Lookup::BY_FILE, held, false),
            (vec![TableRead::Holds], None),
            "found elsewhere: nothing to confirm"
        );
    }

    #[test]
    fn a_refusal_is_the_vnodes_only_when_every_read_says_so() {
        use super::{Failed, Vnode, refusal_shown};
        const V: u32 = ffi::PROX_FDTYPE_VNODE;
        const PIPE: u32 = 6;
        const FILE: Vnode = Vnode {
            dev: 1,
            ino: 2,
            rdev: 0,
        };
        let listed = || Ok(vec![(3, V), (4, V), (5, PIPE)]);
        let reads = |three: Result<Vnode, Failed>, four: Result<Vnode, Failed>| {
            move |fd: i32| match fd {
                3 => three,
                4 => four,
                other => panic!("descriptor {other} is no vnode the table lists"),
            }
        };
        for (refused, three, four, why) in [
            (
                &[3][..],
                Err(Failed::Refused),
                Ok(FILE),
                "refused again, another described",
            ),
            (&[3], Err(Failed::Closed), Ok(FILE), "closed since"),
            (&[3], Err(Failed::Dead), Ok(FILE), "dead since"),
            (&[9], Ok(FILE), Err(Failed::Refused), "no longer listed"),
        ] {
            assert!(
                refusal_shown(refused, listed(), reads(three, four)),
                "{why}"
            );
        }
        for failed in [Failed::Refused, Failed::Exited, Failed::Other] {
            assert!(
                !refusal_shown(&[3], Err(failed), reads(Ok(FILE), Ok(FILE))),
                "the listing {failed:?}"
            );
        }
        for (refused, three, four, why) in [
            (&[3][..], Ok(FILE), Ok(FILE), "described now"),
            (&[3], Err(Failed::Exited), Ok(FILE), "exited"),
            (&[3], Err(Failed::Other), Ok(FILE), "failed otherwise"),
            (
                &[3],
                Err(Failed::Refused),
                Err(Failed::Refused),
                "no other described",
            ),
            (
                &[3],
                Err(Failed::Refused),
                Err(Failed::Closed),
                "the other closed",
            ),
            (
                &[3, 4],
                Err(Failed::Refused),
                Err(Failed::Refused),
                "every vnode refused",
            ),
        ] {
            assert!(
                !refusal_shown(refused, listed(), reads(three, four)),
                "{why}"
            );
        }
        assert!(
            !refusal_shown(
                &[3],
                Ok(vec![(3, V), (5, PIPE)]),
                reads(Err(Failed::Refused), Ok(FILE))
            ),
            "only a pipe beside it"
        );
    }

    fn file_key(path: &std::path::Path) -> u128 {
        let meta = std::fs::metadata(path).expect("stat");
        super::Vnode {
            dev: u32::try_from(meta.dev() & u64::from(u32::MAX)).expect("a Darwin dev_t"),
            ino: meta.ino(),
            rdev: 0,
        }
        .file_key()
    }

    #[test]
    fn a_table_left_unknown_is_outside_the_scan_once_its_process_runs_as_another_user() {
        use super::TableRead::{Clear, Exited, Holds, Left, Unreadable};
        let read = vec![Holds, Unreadable, Clear, Exited];
        assert_eq!(
            super::left_the_user(read.clone(), Some(0), 501),
            [Holds, Left, Clear, Exited],
            "another user's now: unknown keys leave, a key it held stays held"
        );
        assert_eq!(
            super::left_the_user(read.clone(), Some(501), 501),
            read,
            "still this user's: unknown stays unknown"
        );
        assert_eq!(
            super::left_the_user(read.clone(), None, 501),
            read,
            "its user unread: unknown stays unknown"
        );
    }

    /// A child killed and reaped when it drops, so a failed assertion or a hang detector leaves
    /// nothing running (`top -l 0` samples forever).
    struct KilledOnDrop(crate::spawnfd::Child);

    impl Drop for KilledOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// A process of this user's that becomes a set-user-ID program after a scan lists it
    /// (`/bin/ps` run from a shell, measured leaving every scan unknown under a `ps` loop) is
    /// outside the scan, as it is once listed so; `top` is one that stays to be read.
    /// Negative control: without [`super::left_the_user`] its read is `Unreadable`.
    #[test]
    fn a_process_that_became_a_set_user_id_program_reads_left_not_unknown() {
        use super::{Lookup, TableRead};
        // SAFETY: `geteuid` is a side-effect-free getter.
        let me = unsafe { ffi::geteuid() };
        assert_ne!(me, 0, "a scan as root reads every process");
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let sought = tmp.path().join("sought");
        std::fs::write(&sought, b"sought").expect("write");
        let top = KilledOnDrop(
            crate::spawnfd::command("/usr/bin/top")
                .args(["-l", "0", "-s", "60"])
                .stdin(crate::spawnfd::Stdio::null())
                .stdout(crate::spawnfd::Stdio::null())
                .stderr(crate::spawnfd::Stdio::null())
                .spawn()
                .expect("spawn top"),
        );
        let pid = top.0.id();
        let started = Instant::now();
        while crate::process::read_short_info(i32::try_from(pid).expect("a pid"))
            .is_none_or(|info| info.pbsi_uid == me)
        {
            assert!(started.elapsed() < HANG, "top never ran as its owner");
            std::thread::yield_now();
        }
        let key = file_key(&sought);
        let read = super::read_table(pid, &[key], Lookup::BY_FILE);
        let asked = super::process_holds_rdev(pid, u64::from(u32::MAX - 1));
        drop(top);
        assert_eq!(read, [TableRead::Left], "outside this user's scan");
        assert_eq!(asked, None, "asked of that process alone, unknown");
    }

    #[test]
    fn a_refusal_is_cleared_only_once_libproc_shows_it_is_the_vnodes() {
        use super::{Lookup, TableRead};
        use std::io::ErrorKind;
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let (held, sought) = (tmp.path().join("held"), tmp.path().join("sought"));
        std::fs::write(&held, b"held").expect("write");
        std::fs::write(&sought, b"sought").expect("write");
        let mut child = crate::spawnfd::command("/bin/sleep")
            .arg("600")
            .stdin(crate::spawnfd::Stdio::from(
                std::fs::File::open(&held).expect("open"),
            ))
            .stdout(crate::spawnfd::Stdio::null())
            .stderr(crate::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn");
        let pid = child.id();
        let started = Instant::now();
        while super::read_table(pid, &[file_key(&held)], Lookup::BY_FILE) != [TableRead::Holds] {
            assert!(started.elapsed() < HANG, "the child never showed the file");
            std::thread::yield_now();
        }
        let key = file_key(&sought);
        let table = |fds: &[i32], reads: usize, key: u128, lookup: Lookup| {
            hook::refusing(pid, fds, reads, || super::read_table(pid, &[key], lookup))
        };
        let file = Lookup::BY_FILE;
        assert_eq!(
            table(&[0], hook::ALWAYS, key, file),
            [TableRead::Clear],
            "the vnode's"
        );
        assert_eq!(
            table(&[], hook::ALWAYS, key, file),
            [TableRead::Clear],
            "none refused"
        );
        assert_eq!(
            table(&[0], 1, key, file),
            [TableRead::Unreadable],
            "it did not last"
        );
        assert_eq!(
            table(&[0, 1, 2], hook::ALWAYS, key, file),
            [TableRead::Unreadable],
            "nothing described"
        );
        let unheld = u128::from(u32::MAX - 1);
        assert_eq!(
            table(&[0], hook::ALWAYS, unheld, Lookup::BY_RDEV),
            [TableRead::Unreadable],
            "a device's scan"
        );
        let scan = |reads| {
            hook::refusing(pid, &[0], reads, || {
                super::scan_file_holders(&sought, &[]).expect("scan")
            })
        };
        let cleared = scan(hook::ALWAYS);
        assert!(
            !cleared.unreadable.contains(&pid) && !cleared.holds(pid),
            "{cleared:?}"
        );
        let unknown = scan(1);
        assert!(unknown.unreadable.contains(&pid), "{unknown:?}");
        let files = |fds: &[i32], reads| {
            hook::refusing(pid, fds, reads, || super::open_files(pid)).map_err(|e| e.kind())
        };
        let named = |p: &std::path::PathBuf| p.file_name() == Some(std::ffi::OsStr::new("held"));
        let shown = files(&[0], hook::ALWAYS).expect("its open files");
        assert!(
            !shown.iter().any(named) && shown.iter().any(|p| p.as_os_str() == "/dev/null"),
            "{shown:?}"
        );
        assert!(
            files(&[], hook::ALWAYS)
                .expect("unrefused")
                .iter()
                .any(named)
        );
        assert_eq!(
            files(&[0], 1),
            Err(ErrorKind::PermissionDenied),
            "it did not last"
        );
        assert_eq!(
            files(&[0, 1, 2], hook::ALWAYS),
            Err(ErrorKind::PermissionDenied),
            "nothing described"
        );
        assert!(
            !super::refusal_is_the_vnodes(1, &[3], Lookup::BY_FILE),
            "a table refused to this uid"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    fn a_holder_of_dev_null() -> (crate::spawnfd::Child, u64) {
        let rdev = std::fs::metadata("/dev/null").expect("stat").rdev();
        let child = crate::spawnfd::command("/bin/sleep")
            .arg("600")
            .stdin(crate::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn");
        let started = Instant::now();
        while super::process_holds_rdev(child.id(), rdev) != Some(true) {
            assert!(
                started.elapsed() < HANG,
                "the child never showed the device"
            );
            std::thread::yield_now();
        }
        (child, rdev)
    }

    /// SIGKILL `pid` and return once the kernel reports its exit: it is a
    /// zombie then, until its parent (this test) reaps it.
    pub(super) fn kill_unreaped(pid: u32) {
        let watch = ExitWatch::watch(pid).expect("watch the child");
        // SAFETY: sends `SIGKILL` to the child process spawned by this test.
        unsafe { kill(pid as i32, 9) };
        let started = Instant::now();
        while watch.exit_status().is_none() {
            assert!(started.elapsed() < HANG, "the child never exited");
            std::thread::yield_now();
        }
    }

    #[test]
    fn an_exited_process_still_listed_holds_nothing() {
        let (mut child, rdev) = a_holder_of_dev_null();
        let pid = child.id();
        kill_unreaped(pid);
        assert!(
            super::uid_pids().expect("listing").contains(&pid),
            "the zombie is still listed: the scan meets it"
        );
        assert_eq!(super::process_holds_rdev(pid, rdev), Some(false));
        let scan = super::scan_rdev_holders(rdev, &[]).expect("scan");
        assert!(!scan.holds(pid), "{scan:?}");
        assert!(!scan.unreadable.contains(&pid), "{scan:?}");
        let _ = child.wait();
        assert_eq!(
            super::process_holds_rdev(1, rdev),
            None,
            "a refused table is unknown"
        );
    }

    #[test]
    fn a_process_that_exits_after_its_listing_holds_nothing() {
        let (mut child, rdev) = a_holder_of_dev_null();
        let pid = child.id();
        let listed = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let seen = std::rc::Rc::clone(&listed);
        hook::after_next_listing(move |at, fds| {
            assert_eq!(at, pid as i32);
            seen.set(
                fds.iter()
                    .filter(|(_, kind)| *kind == ffi::PROX_FDTYPE_VNODE)
                    .count(),
            );
            kill_unreaped(pid);
        });
        assert_eq!(super::process_holds_rdev(pid, rdev), Some(false));
        assert!(
            listed.get() > 0,
            "the listing named its vnodes, read after its exit"
        );
        let _ = child.wait();
    }

    #[test]
    fn the_scan_finds_a_child_holding_a_character_device() {
        let rdev = std::fs::metadata("/dev/null").expect("stat").rdev();
        assert_ne!(rdev, 0, "/dev/null is a character device");
        let mut child = crate::spawnfd::command("/bin/sleep")
            .arg("600")
            .stdin(crate::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn");
        let started = std::time::Instant::now();
        loop {
            if super::process_holds_rdev(child.id(), rdev) == Some(true) {
                break;
            }
            assert!(
                started.elapsed().as_secs() < 60,
                "the child never showed the device"
            );
            std::thread::yield_now();
        }
        let scan = super::scan_rdev_holders(rdev, &[]).expect("scan");
        assert!(scan.holds(child.id()), "{scan:?}");
        assert!(scan.scanned > 0);
        let skipped = super::scan_rdev_holders(rdev, &[child.id()]).expect("scan");
        assert!(!skipped.holds(child.id()), "skip is honoured");
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(
            super::process_holds_rdev(child.id(), rdev),
            Some(false),
            "gone: holds nothing"
        );
    }

    #[test]
    fn one_walk_answers_for_every_device_asked() {
        let me = std::process::id();
        let (mut child, null) = a_holder_of_dev_null();
        let (master, pty) = a_master_at(220);
        let nobody = 0xfeed_0002;
        let asked = [null, pty, nobody, pty];
        let scans = super::scan_holders_of(&asked, &[]).expect("scan");
        assert_eq!(scans.len(), asked.len());
        assert!(scans[0].holds(child.id()), "{:?}", scans[0]);
        assert!(scans[1].holds(me) && scans[3].holds(me), "{scans:?}");
        assert!(
            !scans[1].holds(child.id()),
            "the child holds no PTY of ours"
        );
        assert!(scans[2].holders.is_empty(), "{:?}", scans[2]);
        for (rdev, scan) in asked.iter().zip(&scans) {
            let alone = super::scan_rdev_holders(*rdev, &[]).expect("scan");
            let pids = if *rdev == null {
                vec![child.id()]
            } else {
                vec![me, child.id()]
            };
            for pid in pids {
                assert_eq!(
                    scan.holds(pid),
                    alone.holds(pid),
                    "rdev {rdev:#x}, pid {pid}"
                );
            }
        }
        let skipped = super::scan_holders_of(&asked, &[me, child.id()]).expect("scan");
        assert!(
            skipped.iter().all(|s| !s.holds(me) && !s.holds(child.id())),
            "{skipped:?}"
        );
        assert!(
            super::scan_holders_of(&[], &[])
                .expect("nothing")
                .is_empty()
        );
        drop(master);
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn an_unheld_rdev_has_no_holders() {
        let scan = super::scan_rdev_holders(0xfeed_0001, &[]).expect("scan");
        assert!(scan.holders.is_empty(), "{scan:?}");
    }

    #[test]
    fn a_shared_memory_descriptor_is_named_with_its_size_and_nothing_else_is() {
        use std::os::fd::AsFd as _;
        assert_eq!(ffi::PSHM_FDINFO_SIZE, 1192);
        assert_eq!(ffi::PSHM_VST_SIZE, 112);
        for len in [crate::shm::PAGE, 5 * crate::shm::PAGE] {
            // SAFETY: this private custody is queried only for metadata.
            let (_region, custody) = unsafe { crate::shm::Region::create(len) }.expect("a region");
            assert_eq!(super::fd_is_pshm(custody.as_fd()), Some(len as u64));
            // SAFETY: this private writable descriptor is queried only for metadata.
            let (_twin, writable) =
                unsafe { crate::shm::testing::create_with_writable_custody(len) }.expect("a twin");
            assert_eq!(super::fd_is_pshm(writable.as_fd()), Some(len as u64));
        }
        let file = std::fs::File::open("/dev/null").expect("/dev/null");
        assert_eq!(super::fd_is_pshm(file.as_fd()), None, "a vnode");
        let (a, _b) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
        assert_eq!(super::fd_is_pshm(a.as_fd()), None, "a socket");
        let (r, _w) = std::io::pipe().expect("a pipe");
        assert_eq!(super::fd_is_pshm(r.as_fd()), None, "a pipe");
    }

    #[test]
    fn a_process_s_shared_memory_descriptors_are_listed_with_their_sizes() {
        let len = 13 * crate::shm::PAGE;
        let mine = || {
            super::pshm_sizes_of(std::process::id())
                .expect("our own table")
                .into_iter()
                .filter(|s| *s == len as u64)
                .count()
        };
        assert_eq!(mine(), 0);
        // SAFETY: the fixture only inspects the descriptor table and drops these owners.
        let (region, custody) = unsafe { crate::shm::Region::create(len) }.expect("a region");
        assert_eq!(mine(), 1, "the custody descriptor");
        drop(region);
        assert_eq!(mine(), 1, "the mapping was no descriptor");
        drop(custody);
        assert_eq!(mine(), 0, "closed");
    }

    /// One table read's rows name the holders: a live listener of this user's at the target,
    /// once; not another user's, a dead binder's, a skipped one's or one bound elsewhere. A read
    /// that is no table, or one without the check's own listener, is refused.
    #[test]
    fn a_table_read_names_its_live_binders_once_it_holds_its_own_listener() {
        use aterm_sysprobe::jobs::darwin::UnixListenerRow as Row;
        let row = |pid, uid, path: &'static [u8]| Row { pid, uid, path };
        let own = row(7, 501, b"/t/aterm-pcb-7-0.sock");
        let bus = b"/nowhere-folder/bus.sock";
        let target = super::SocketPath::of(std::path::Path::new("/nowhere-folder/bus.sock"));
        let rows = [
            own,
            row(41, 501, bus),
            row(41, 501, bus),
            row(42, 0, bus),
            row(43, 501, bus),
            row(44, 501, bus),
            row(45, 501, b"/nowhere-folder/other.sock"),
        ];
        let live = [7, 41, 42, 44, 45, 46];
        let scan = super::listener_holders(Some(&rows), own, &target, &live, &[44])
            .expect("a read holding its own listener");
        assert_eq!(scan.holders, [41], "{scan:?}");
        assert_eq!(scan.scanned, 5, "the live processes but the skipped one");
        assert!(
            scan.unreadable.is_empty() && scan.sealed.is_empty(),
            "{scan:?}"
        );
        for refused in [None, Some(&rows[1..])] {
            let e = super::listener_holders(refused, own, &target, &live, &[])
                .expect_err("a misread table");
            assert_eq!(e.kind(), std::io::ErrorKind::Unsupported, "{e}");
        }
    }

    /// The check binds in a folder of its own, made owner-only, under the first absolute folder
    /// that takes it: a relative one, a missing one and one too deep for `sun_path` pass it on and
    /// keep nothing. With none that takes it the last refusal is the answer.
    #[test]
    fn the_table_check_binds_in_a_folder_of_its_own_under_the_first_that_takes_it() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = aterm_tempfile::Builder::new()
            .tempdir_in("/tmp")
            .expect("a scratch folder short enough for sun_path");
        let base = dir.path().to_path_buf();
        let deep = base.join("d".repeat(120));
        std::fs::create_dir(&deep).expect("a folder too deep for sun_path");
        let missing = base.join("missing");
        let relative = std::path::PathBuf::from(".");
        let check = super::TableCheck::bind_in(&[
            relative.clone(),
            missing.clone(),
            deep.clone(),
            base.clone(),
        ])
        .expect("bound in the folder that takes it");
        assert_eq!(check.path.parent(), Some(check.dir.as_path()));
        assert_eq!(
            check.dir.parent(),
            Some(base.as_path()),
            "{}",
            check.path.display()
        );
        let mode = std::fs::symlink_metadata(&check.dir)
            .expect("its folder")
            .permissions()
            .mode();
        assert_eq!(mode & 0o7777, 0o700, "{mode:o}");
        assert_eq!(
            std::fs::read_dir(&deep).expect("the deep folder").count(),
            0,
            "the folder whose bind failed is gone"
        );
        drop(check);
        assert!(super::TableCheck::bind_in(&[relative, missing, deep]).is_err());
    }

    /// The check's listener is this process's at its path while it lives, and its file and folder
    /// are gone once it is dropped.
    #[test]
    fn the_table_check_listener_leaves_no_file() {
        let check = super::TableCheck::bind().expect("a check listener");
        let (path, folder) = (check.path.clone(), check.dir.clone());
        assert!(
            std::fs::symlink_metadata(&path).is_ok(),
            "{}",
            path.display()
        );
        let table = super::unix_stream_table().expect("the table");
        let rows = aterm_sysprobe::jobs::darwin::unix_listeners(&table).expect("read as laid out");
        // SAFETY: `geteuid` is a side-effect-free getter.
        let uid = unsafe { ffi::geteuid() };
        assert!(
            rows.iter().any(|row| row.pid == std::process::id()
                && row.uid == uid
                && row.path == path.as_os_str().as_encoded_bytes()),
            "the table names the check's listener"
        );
        drop(check);
        assert!(
            std::fs::symlink_metadata(&path).is_err()
                && std::fs::symlink_metadata(&folder).is_err(),
            "{}",
            path.display()
        );
    }
}
