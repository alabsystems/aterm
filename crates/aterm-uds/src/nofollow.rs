// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PATHS READ WITHOUT FOLLOWING A LINK ANYWHERE ON THEM: macOS's `O_NOFOLLOW_ANY` and
//! `AT_SYMLINK_NOFOLLOW_ANY`, so a folder of the path swapped for a link answers `ELOOP` and the
//! kernel never walks into where the link points. What is at a path ([`kind_of`]), an event-only
//! descriptor a watch holds on a folder or a regular file alone ([`open_evtonly`]), a reader that
//! opens a regular file alone ([`open_reader`]), neither open ever blocking on a FIFO, and the
//! kqueue that hears a watched vnode change ([`Watcher`]). The opener is aterm-lab's scratch
//! opener's (`sys::scratch_file`), lifted. Off macOS every call answers `Unsupported`, and no
//! path is touched.

use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::path::Path;
use std::time::Duration;

/// What is at a path, read without following a link on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Nothing (`ENOENT`, or a component that is not a folder).
    Missing,
    /// A folder.
    Dir,
    /// A regular file.
    File,
    /// A link, on the path or at its end (`ELOOP`).
    Link,
    /// Anything else: a FIFO, a socket, a device.
    Other,
}

/// What one descriptor names: its file's device and inode, and its size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    /// The device.
    pub dev: u64,
    /// The inode.
    pub ino: u64,
    /// The size in bytes.
    pub size: u64,
}

/// Whether `error` is a link met on the way (`ELOOP`).
#[must_use]
pub fn is_link(error: &io::Error) -> bool {
    error.raw_os_error() == Some(ELOOP)
}

/// Whether `error` says nothing is there (`ENOENT`, `ENOTDIR`).
#[must_use]
pub fn is_missing(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(e) if e == ENOENT || e == ENOTDIR)
}

/// Whether `error` is [`open_reader`]'s refusal of a path that is not a regular file, or
/// [`open_evtonly`]'s of one that is not what it was asked for (`EFTYPE`).
#[must_use]
pub fn not_regular(error: &io::Error) -> bool {
    error.raw_os_error() == Some(EFTYPE)
}

#[cfg(target_os = "macos")]
const ELOOP: i32 = libc::ELOOP;
#[cfg(target_os = "macos")]
const ENOENT: i32 = libc::ENOENT;
#[cfg(target_os = "macos")]
const ENOTDIR: i32 = libc::ENOTDIR;
#[cfg(target_os = "macos")]
const EFTYPE: i32 = libc::EFTYPE;
#[cfg(not(target_os = "macos"))]
const ELOOP: i32 = -1;
#[cfg(not(target_os = "macos"))]
const ENOENT: i32 = -2;
#[cfg(not(target_os = "macos"))]
const ENOTDIR: i32 = -3;
#[cfg(not(target_os = "macos"))]
const EFTYPE: i32 = -4;

/// The vnode changes a [`Watcher`] reports: a write or an append, a link added, an attribute,
/// and the vnode's going ([`NOTE_GONE`]).
#[cfg(target_os = "macos")]
const NOTES: u32 = libc::NOTE_WRITE
    | libc::NOTE_EXTEND
    | libc::NOTE_ATTRIB
    | libc::NOTE_LINK
    | libc::NOTE_DELETE
    | libc::NOTE_RENAME
    | libc::NOTE_REVOKE;

/// The vnode is gone from its name: deleted, renamed, or its volume revoked.
#[cfg(target_os = "macos")]
pub const NOTE_GONE: u32 = libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_REVOKE;
/// The vnode is gone from its name (no watch fires off macOS).
#[cfg(not(target_os = "macos"))]
pub const NOTE_GONE: u32 = 0;

/// `open(2)`'s flag refusing a link anywhere on the path (`sys/fcntl.h`).
#[cfg(target_os = "macos")]
const O_NOFOLLOW_ANY: libc::c_int = 0x2000_0000;

/// `open(2)`'s flag for a descriptor that only hears events: it holds no volume busy.
#[cfg(target_os = "macos")]
const O_EVTONLY: libc::c_int = 0x0000_8000;

/// `fstatat(2)`'s flag refusing a link anywhere on the path (`sys/fcntl.h`).
#[cfg(target_os = "macos")]
const AT_SYMLINK_NOFOLLOW_ANY: libc::c_int = 0x0800;

/// The most events one [`Watcher::wait`] returns.
#[cfg(target_os = "macos")]
const BATCH: usize = 64;

/// The bell's ident ([`Watcher::ring`]), a user event's, which shares no space with the watched
/// descriptors' idents.
#[cfg(target_os = "macos")]
const BELL: libc::uintptr_t = usize::MAX;

#[cfg(target_os = "macos")]
fn cpath(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt as _;
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

#[cfg(target_os = "macos")]
fn fstat_of(fd: BorrowedFd<'_>) -> io::Result<libc::stat> {
    use std::os::fd::AsRawFd as _;
    // SAFETY: `stat` is plain data; all-zero is a valid value for `fstat` to fill.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is a live descriptor and `st` a valid `stat` for the call to write.
    if unsafe { libc::fstat(fd.as_raw_fd(), &raw mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(st)
}

#[cfg(target_os = "macos")]
const fn kind_of_mode(mode: libc::mode_t) -> Kind {
    match mode & libc::S_IFMT {
        libc::S_IFDIR => Kind::Dir,
        libc::S_IFREG => Kind::File,
        libc::S_IFLNK => Kind::Link,
        _ => Kind::Other,
    }
}

#[cfg(target_os = "macos")]
fn open_with(path: &Path, flags: libc::c_int) -> io::Result<OwnedFd> {
    use std::os::fd::FromRawFd as _;
    let c = cpath(path)?;
    // SAFETY: `c` is a valid NUL-terminated path; the descriptor is owned below.
    let fd = unsafe { libc::open(c.as_ptr(), flags | O_NOFOLLOW_ANY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a fresh descriptor this process exclusively owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// WHAT IS AT `path`, by `fstatat(AT_SYMLINK_NOFOLLOW_ANY)`: a link on the path or at its end is
/// [`Kind::Link`], and nothing past it is read.
/// # Errors
/// A refusal other than nothing there or a link (`EACCES`, `EPERM`), or `Unsupported` off macOS.
#[cfg(target_os = "macos")]
#[cfg_attr(trust_verify, trust::skip)]
pub fn kind_of(path: &Path) -> io::Result<Kind> {
    let c = cpath(path)?;
    // SAFETY: `stat` is plain data; all-zero is a valid value for `fstatat` to fill.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `st` a valid `stat` for the call to write.
    let rc = unsafe {
        libc::fstatat(
            libc::AT_FDCWD,
            c.as_ptr(),
            &raw mut st,
            AT_SYMLINK_NOFOLLOW_ANY,
        )
    };
    if rc == 0 {
        return Ok(kind_of_mode(st.st_mode));
    }
    let error = io::Error::last_os_error();
    if is_missing(&error) {
        Ok(Kind::Missing)
    } else if is_link(&error) {
        Ok(Kind::Link)
    } else {
        Err(error)
    }
}

/// See the macOS twin: nothing is read here.
/// # Errors
/// Always `Unsupported`.
#[cfg(not(target_os = "macos"))]
pub fn kind_of(_path: &Path) -> io::Result<Kind> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// AN EVENT-ONLY DESCRIPTOR FOR THE `want` AT `path` (`O_EVTONLY | O_NONBLOCK | O_NOFOLLOW_ANY |
/// O_CLOEXEC`, then `fstat`), for a [`Watcher`]: it reads nothing and keeps no volume from
/// unmounting. A FIFO's open returns at once (an event-only open of one with no writer would wait
/// for a writer), and anything but `want` (a folder or a regular file) is refused.
/// # Errors
/// `open(2)`'s: `ELOOP` for a link anywhere on the path ([`is_link`]), `ENOENT` for nothing there;
/// `EFTYPE` for anything at `path` but `want` ([`not_regular`]); `Unsupported` off macOS.
#[cfg(target_os = "macos")]
#[cfg_attr(trust_verify, trust::skip)]
pub fn open_evtonly(path: &Path, want: Kind) -> io::Result<OwnedFd> {
    use std::os::fd::AsFd as _;
    let fd = open_with(path, O_EVTONLY | libc::O_NONBLOCK)?;
    let st = fstat_of(fd.as_fd())?;
    if !matches!(want, Kind::Dir | Kind::File) || kind_of_mode(st.st_mode) != want {
        return Err(io::Error::from_raw_os_error(libc::EFTYPE));
    }
    Ok(fd)
}

/// See the macOS twin: nothing is opened here.
/// # Errors
/// Always `Unsupported`.
#[cfg(not(target_os = "macos"))]
pub fn open_evtonly(_path: &Path, _want: Kind) -> io::Result<OwnedFd> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// A READER OF THE REGULAR FILE AT `path` (`O_RDONLY | O_NONBLOCK | O_NOFOLLOW_ANY | O_CLOEXEC`,
/// then `fstat`): a FIFO's open returns at once, and anything but a regular file is refused.
/// # Errors
/// `open(2)`'s, as [`open_evtonly`]'s; `EFTYPE` for a path that is not a regular file
/// ([`not_regular`]); `Unsupported` off macOS.
#[cfg(target_os = "macos")]
#[cfg_attr(trust_verify, trust::skip)]
pub fn open_reader(path: &Path) -> io::Result<OwnedFd> {
    use std::os::fd::AsFd as _;
    let fd = open_with(path, libc::O_RDONLY | libc::O_NONBLOCK)?;
    let st = fstat_of(fd.as_fd())?;
    if kind_of_mode(st.st_mode) != Kind::File {
        return Err(io::Error::from_raw_os_error(libc::EFTYPE));
    }
    Ok(fd)
}

/// See the macOS twin: nothing is opened here.
/// # Errors
/// Always `Unsupported`.
#[cfg(not(target_os = "macos"))]
pub fn open_reader(_path: &Path) -> io::Result<OwnedFd> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// What `fd` names now ([`Identity`]).
/// # Errors
/// `fstat(2)`'s; `Unsupported` off macOS.
#[cfg(target_os = "macos")]
#[cfg_attr(trust_verify, trust::skip)]
pub fn identity(fd: BorrowedFd<'_>) -> io::Result<Identity> {
    let st = fstat_of(fd)?;
    Ok(Identity {
        dev: u64::from(st.st_dev.cast_unsigned()),
        ino: st.st_ino,
        size: u64::try_from(st.st_size).unwrap_or(0),
    })
}

/// See the macOS twin.
/// # Errors
/// Always `Unsupported`.
#[cfg(not(target_os = "macos"))]
pub fn identity(_fd: BorrowedFd<'_>) -> io::Result<Identity> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// One watch that fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fired {
    /// The id it was added with ([`Watcher::add`]).
    pub id: u64,
    /// Its vnode left its name ([`NOTE_GONE`]): the descriptor names nothing the path does.
    pub gone: bool,
}

/// THE KQUEUE THAT HEARS WATCHED VNODES CHANGE, close-on-exec; a watch ends when its descriptor
/// closes.
#[derive(Debug)]
pub struct Watcher {
    #[cfg(target_os = "macos")]
    kq: OwnedFd,
}

impl Watcher {
    /// A new kqueue, its bell ([`Self::ring`]) registered.
    /// # Errors
    /// `kqueue(2)`'s or `kevent(2)`'s; `Unsupported` off macOS.
    #[cfg(target_os = "macos")]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn new() -> io::Result<Self> {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        // SAFETY: `kqueue()` takes no arguments and returns a new descriptor or -1.
        let raw = unsafe { libc::kqueue() };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a fresh descriptor this process exclusively owns.
        let kq = unsafe { OwnedFd::from_raw_fd(raw) };
        // SAFETY: F_SETFD takes an int and touches only this descriptor's flags.
        if unsafe { libc::fcntl(kq.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let watcher = Self { kq };
        watcher.change(BELL, libc::EVFILT_USER, libc::EV_ADD | libc::EV_CLEAR, 0, 0)?;
        Ok(watcher)
    }

    /// See the macOS twin: no kqueue here.
    /// # Errors
    /// Always `Unsupported`.
    #[cfg(not(target_os = "macos"))]
    pub fn new() -> io::Result<Self> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    #[cfg(target_os = "macos")]
    #[cfg_attr(trust_verify, trust::skip)]
    fn change(
        &self,
        ident: libc::uintptr_t,
        filter: i16,
        flags: u16,
        fflags: u32,
        udata: u64,
    ) -> io::Result<()> {
        use std::os::fd::AsRawFd as _;
        let change = libc::kevent {
            ident,
            filter,
            flags,
            fflags,
            data: 0,
            udata: std::ptr::without_provenance_mut(usize::try_from(udata).unwrap_or(usize::MAX)),
        };
        // SAFETY: one initialized change entry, live for the call; no event list requested.
        let rc = unsafe {
            libc::kevent(
                self.kq.as_raw_fd(),
                &raw const change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// WATCH `fd` (an [`open_evtonly`] descriptor) under `id`: its writes, appends, links,
    /// attributes and its going, each reported once until the next [`Self::wait`] (`EV_CLEAR`).
    /// # Errors
    /// `kevent(2)`'s; `Unsupported` off macOS.
    #[cfg(target_os = "macos")]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn add(&self, fd: BorrowedFd<'_>, id: u64) -> io::Result<()> {
        use std::os::fd::AsRawFd as _;
        let ident = libc::uintptr_t::try_from(fd.as_raw_fd())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        self.change(
            ident,
            libc::EVFILT_VNODE,
            libc::EV_ADD | libc::EV_ENABLE | libc::EV_CLEAR,
            NOTES,
            id,
        )
    }

    /// See the macOS twin.
    /// # Errors
    /// Always `Unsupported`.
    #[cfg(not(target_os = "macos"))]
    pub fn add(&self, _fd: BorrowedFd<'_>, _id: u64) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    /// THE WATCHES THAT FIRED within `timeout`, or since the last wait: empty at the timeout or
    /// at the bell.
    /// # Errors
    /// `kevent(2)`'s, but for an interrupted wait, which is waited again; `Unsupported` off
    /// macOS.
    #[cfg(target_os = "macos")]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn wait(&self, timeout: Duration) -> io::Result<Vec<Fired>> {
        use std::os::fd::AsRawFd as _;
        let deadline = std::time::Instant::now() + timeout;
        // SAFETY: `kevent` is plain data; all-zero is a valid value for the call to fill.
        let mut events: [libc::kevent; BATCH] = unsafe { std::mem::zeroed() };
        let got = loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let ts = libc::timespec {
                tv_sec: libc::time_t::try_from(left.as_secs()).unwrap_or(libc::time_t::MAX),
                tv_nsec: libc::c_long::from(left.subsec_nanos()),
            };
            // SAFETY: the timeout and the BATCH output slots live for this call.
            let rc = unsafe {
                libc::kevent(
                    self.kq.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    events.as_mut_ptr(),
                    libc::c_int::try_from(BATCH).unwrap_or(1),
                    &raw const ts,
                )
            };
            if rc >= 0 {
                break usize::try_from(rc).unwrap_or(0);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        };
        Ok(events[..got.min(BATCH)]
            .iter()
            .filter(|event| event.filter == libc::EVFILT_VNODE && event.flags & libc::EV_ERROR == 0)
            .map(|event| Fired {
                id: event.udata.addr() as u64,
                gone: event.fflags & NOTE_GONE != 0,
            })
            .collect())
    }

    /// See the macOS twin.
    /// # Errors
    /// Always `Unsupported`.
    #[cfg(not(target_os = "macos"))]
    pub fn wait(&self, _timeout: Duration) -> io::Result<Vec<Fired>> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    /// Ring the bell: a [`Self::wait`] on another thread returns now.
    /// # Errors
    /// `kevent(2)`'s; `Unsupported` off macOS.
    #[cfg(target_os = "macos")]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn ring(&self) -> io::Result<()> {
        self.change(BELL, libc::EVFILT_USER, 0, libc::NOTE_TRIGGER, 0)
    }

    /// See the macOS twin.
    /// # Errors
    /// Always `Unsupported`.
    #[cfg(not(target_os = "macos"))]
    pub fn ring(&self) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "nofollow_tests.rs"]
mod tests;
