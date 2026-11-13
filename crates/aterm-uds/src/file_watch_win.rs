// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows: `ReadDirectoryChangesW` listeners on one I/O completion port, which the wait parks
//! on with no clock.
//!
//! A file key listens on its file's folder, or on the nearest folder above it that is there
//! (events name the entry, so a replace by rename is heard, as is the making of the missing
//! folder), and a directory key on the folder's every entry. Each key also listens on the folder
//! above the one it holds for that folder's own name made, removed or renamed, which the folder's
//! own listener does not hear. Nothing listens higher: Windows refuses to rename a folder while
//! a handle is open on anything below it, so no folder above a listened one moves while it is
//! listened on. The watched folder itself can be renamed, deleted and made again.
//!
//! That refusal is what a listener costs everyone else: while one is on a folder, no folder
//! above it can be renamed, moved or sent to the Recycle Bin (a rename) by anyone, whatever the
//! handle's access and sharing (measured on Windows 11: a handle opened with no access at all
//! and every share refuses it too; deleting the tree outright still works, and a handle on a
//! volume's root refuses nothing). So this arm listens only where no person renames the folders
//! above: inside the account's application-data folders (`%APPDATA%`, `%LOCALAPPDATA%`) and its
//! temporary folder, where aterm's own config, state and sockets live, judged by where the
//! folder really is, so one of those that is a junction into a project is not listened on. A
//! key anywhere else is refused
//! ([`FileEvents::listens_for`]) and its file is looked at on a clock by the watch, which holds
//! nothing open; so is a file its host asked only to be looked at ([`FileEvents::pins_folders`]).
//!
//! Two keys in one folder share its listener. A listener whose read fails (its folder went, its
//! volume was taken away) or overflows touches every key on it, and the watch arms each again
//! on whatever its path names now. A read cancelled as its listener is let go still completes:
//! its buffer is kept until that completion is taken from the port.
//!
//! What it does not hear: a write through another hard link of the file (no listener is on the
//! file itself), and a folder above the path replaced through a junction retargeted higher up.
//! A write behind a handle its writer keeps open is heard when that writer flushes the file to
//! disk or closes it, not before: NTFS tells a folder of a write only once it brings the file's
//! record in the folder up to date. A save, which closes or replaces the file, is heard at once.
//! A volume that is not a local disk (`GetDriveTypeW`: a network share, a `\\wsl$` mount) need
//! not report another machine's writes, so a key on one is also looked at in the watch's rounds
//! ([`FileEvents::hears_every_writer`]). So is a key whose folder's own name has no listener,
//! because the kernel refused one on the folder above (an account that may pass through that
//! folder and not list it): nothing would tell of its folder renamed away.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::{FileEvents, Waker};
use crate::win::ffi::{CancelIoEx, CloseHandle, RawHandle};

mod ffi {
    #![allow(non_snake_case, reason = "Win32 API names are camel-case by contract")]

    use crate::win::ffi::RawHandle;

    /// `OVERLAPPED`: the kernel's record of one pending request, which must stay where it is
    /// until that request's completion is taken.
    #[repr(C)]
    pub(super) struct Overlapped {
        pub Internal: usize,
        pub InternalHigh: usize,
        pub Offset: u32,
        pub OffsetHigh: u32,
        pub hEvent: RawHandle,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        /// Declared with the pointer-typed handles `crate::deadline`'s declaration of it has,
        /// so the two agree.
        pub(super) fn CreateFileW(
            lpFileName: *const u16,
            dwDesiredAccess: u32,
            dwShareMode: u32,
            lpSecurityAttributes: *const core::ffi::c_void,
            dwCreationDisposition: u32,
            dwFlagsAndAttributes: u32,
            hTemplateFile: *mut core::ffi::c_void,
        ) -> *mut core::ffi::c_void;
        /// With `INVALID_HANDLE_VALUE` for `FileHandle` it makes a port; with one, it
        /// queues that handle's completions to `ExistingCompletionPort` under `CompletionKey`.
        pub(super) fn CreateIoCompletionPort(
            FileHandle: RawHandle,
            ExistingCompletionPort: RawHandle,
            CompletionKey: usize,
            NumberOfConcurrentThreads: u32,
        ) -> RawHandle;
        pub(super) fn GetQueuedCompletionStatus(
            CompletionPort: RawHandle,
            lpNumberOfBytesTransferred: *mut u32,
            lpCompletionKey: *mut usize,
            lpOverlapped: *mut *mut core::ffi::c_void,
            dwMilliseconds: u32,
        ) -> i32;
        pub(super) fn PostQueuedCompletionStatus(
            CompletionPort: RawHandle,
            dwNumberOfBytesTransferred: u32,
            dwCompletionKey: usize,
            lpOverlapped: *mut core::ffi::c_void,
        ) -> i32;
        pub(super) fn ReadDirectoryChangesW(
            hDirectory: RawHandle,
            lpBuffer: *mut core::ffi::c_void,
            nBufferLength: u32,
            bWatchSubtree: i32,
            dwNotifyFilter: u32,
            lpBytesReturned: *mut u32,
            lpOverlapped: *mut core::ffi::c_void,
            lpCompletionRoutine: *mut core::ffi::c_void,
        ) -> i32;
        /// `BY_HANDLE_FILE_INFORMATION`, thirteen `DWORD`s, into `lpFileInformation`.
        pub(super) fn GetFileInformationByHandle(
            hFile: RawHandle,
            lpFileInformation: *mut u32,
        ) -> i32;
        pub(super) fn GetVolumePathNameW(
            lpszFileName: *const u16,
            lpszVolumePathName: *mut u16,
            cchBufferLength: u32,
        ) -> i32;
        pub(super) fn GetDriveTypeW(lpRootPathName: *const u16) -> u32;
        /// The handle's path with every link and junction followed, into `lpszFilePath`: its
        /// length in units without the NUL, or the length needed where the buffer is short.
        pub(super) fn GetFinalPathNameByHandleW(
            hFile: RawHandle,
            lpszFilePath: *mut u16,
            cchFilePath: u32,
            dwFlags: u32,
        ) -> u32;
    }
}

const INVALID_HANDLE_VALUE: RawHandle = -1;
const FILE_LIST_DIRECTORY: u32 = 0x0001;
/// Read, write and delete: a listener refuses nobody else's open, rename or delete of its folder.
const FILE_SHARE_ALL: u32 = 0x0001 | 0x0002 | 0x0004;
const OPEN_EXISTING: u32 = 3;
/// Opens a directory.
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0010;
/// `GetQueuedCompletionStatus`'s error when its time ran out.
const WAIT_TIMEOUT: i32 = 258;
const INFINITE: u32 = 0xFFFF_FFFF;

/// `FILE_NOTIFY_CHANGE_DIR_NAME`: a folder made, removed or renamed.
const NOTIFY_DIR_NAME: u32 = 0x0002;
/// What a folder's entries listener hears: an entry's name (`FILE_NAME`, `DIR_NAME`),
/// attributes, size, last write, creation time and security.
const NOTIFY_ENTRIES: u32 = 0x0001 | NOTIFY_DIR_NAME | 0x0004 | 0x0008 | 0x0010 | 0x0040 | 0x0100;

/// The completion key of a wake, which no listener's id reaches.
const WAKE: usize = usize::MAX;

/// One listener's buffer in `DWORD`s (16 KiB): `FILE_NOTIFY_INFORMATION` records are
/// `DWORD`-aligned, and a network redirector refuses one past 64 KiB.
const BUFFER_WORDS: usize = 4096;

/// How many more completions one wait takes once a key was touched or it was woken, so a folder
/// that never goes quiet cannot keep the watch from its commands.
const DRAINS_PER_TURN: usize = 64;

/// How long a dropped arm waits for its cancelled reads' completions before it leaks their
/// buffers instead of freeing memory the kernel may still write.
const LET_GO_WITHIN: Duration = Duration::from_secs(1);

/// An owned kernel handle.
struct Handle(RawHandle);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle is live and exclusively this value's.
        unsafe { CloseHandle(self.0) };
    }
}

/// Which folder a handle is open on: its volume's serial number and its file index there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    volume: u32,
    index: u64,
}

/// What a listener hears.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Role {
    /// Every change to an entry of the folder.
    Entries,
    /// Only a folder in it made, removed or renamed: the quiet listener above a key's own folder.
    Names,
}

/// Which of a listener's events touch a key.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Hear {
    /// Every one (a directory key on its own folder).
    Any,
    /// The ones that name this entry, folded ([`folded`]).
    Named(Vec<u16>),
}

/// One pending read's `OVERLAPPED` and the buffer the kernel fills.
#[repr(C)]
struct Io {
    overlapped: ffi::Overlapped,
    buffer: [u32; BUFFER_WORDS],
}

/// An [`Io`] on the heap, owned through a raw pointer because the kernel writes it while a read
/// is pending: no reference to it lives across one. It is freed by hand ([`Self::free`]) once no
/// read is pending, and is leaked, never freed, where that cannot be known.
struct IoBox(*mut Io);

// SAFETY: the allocation is owned by this value alone and is only touched by the thread that
// holds the arm, or by the kernel filling a pending read.
unsafe impl Send for IoBox {}

impl IoBox {
    fn new() -> Self {
        Self(Box::into_raw(Box::new(Io {
            overlapped: ffi::Overlapped {
                Internal: 0,
                InternalHigh: 0,
                Offset: 0,
                OffsetHigh: 0,
                hEvent: 0,
            },
            buffer: [0; BUFFER_WORDS],
        })))
    }

    /// Free it. The caller knows no read is pending on it.
    fn free(self) {
        // SAFETY: the pointer came from `Box::into_raw` in `new`, is freed once (this consumes
        // the owner), and the kernel no longer writes it.
        drop(unsafe { Box::from_raw(self.0) });
    }
}

struct Listener {
    handle: Handle,
    path: PathBuf,
    role: Role,
    identity: Identity,
    io: IoBox,
    /// A read is pending, so a completion naming `io` is still to come.
    reading: bool,
    subs: Vec<(u64, Hear)>,
    every_writer: bool,
}

impl Listener {
    /// Ask for the folder's next changes.
    ///
    /// # Errors
    /// The kernel's refusal, with no read pending and no completion to come.
    fn read(&mut self) -> io::Result<()> {
        let filter = match self.role {
            Role::Entries => NOTIFY_ENTRIES,
            Role::Names => NOTIFY_DIR_NAME,
        };
        // SAFETY: a live directory handle opened for overlapped I/O; the buffer and the
        // `OVERLAPPED` are one heap allocation that outlives the read (it is freed only once
        // the read's completion was taken), and no read is pending on it now.
        let asked = unsafe {
            (*self.io.0).overlapped = ffi::Overlapped {
                Internal: 0,
                InternalHigh: 0,
                Offset: 0,
                OffsetHigh: 0,
                hEvent: 0,
            };
            ffi::ReadDirectoryChangesW(
                self.handle.0,
                (&raw mut (*self.io.0).buffer).cast(),
                (BUFFER_WORDS * 4) as u32,
                0,
                filter,
                std::ptr::null_mut(),
                (&raw mut (*self.io.0).overlapped).cast(),
                std::ptr::null_mut(),
            )
        };
        if asked == 0 {
            return Err(io::Error::last_os_error());
        }
        self.reading = true;
        Ok(())
    }

    /// Push the keys the `bytes` of records a completed read left touch.
    fn route(&self, bytes: u32, touched: &mut Vec<u64>) {
        let words = (bytes as usize).div_ceil(4).min(BUFFER_WORDS);
        // SAFETY: no read is pending (its completion was just taken), so the kernel no longer
        // writes the buffer, and this shared borrow ends before the next read is asked for.
        let buffer = unsafe { &(*self.io.0).buffer };
        notified(&buffer[..words], self.role, &self.subs, touched);
    }

    fn keys(&self) -> impl Iterator<Item = u64> + '_ {
        self.subs.iter().map(|(key, _)| *key)
    }
}

/// A file name as Windows compares it, nearly: each UTF-16 unit upper-cased where that is one
/// unit. A name that differs only by a case this misses is not heard by name.
fn folded(name: impl Iterator<Item = u16>) -> Vec<u16> {
    name.map(|unit| {
        let Some(letter) = char::from_u32(u32::from(unit)) else {
            return unit;
        };
        let mut upper = letter.to_uppercase();
        match (upper.next(), upper.next()) {
            (Some(one), None) => u16::try_from(u32::from(one)).unwrap_or(unit),
            _ => unit,
        }
    })
    .collect()
}

/// Push the keys of `subs` that a buffer of `FILE_NOTIFY_INFORMATION` records touches: each
/// record's entry name against each key's ([`Hear`]). An entries listener also lets a name with
/// a `~` touch every named key, since the kernel may name an entry by its short (8.3) alias,
/// which no key knows; the look that follows finds whether its file changed.
fn notified(words: &[u32], role: Role, subs: &[(u64, Hear)], touched: &mut Vec<u64>) {
    let mut at = 0usize;
    while let (Some(&next), Some(&length)) = (words.get(at), words.get(at + 2)) {
        let units = length as usize / 2;
        let name = folded((0..units).filter_map(|unit| {
            words
                .get(at + 3 + unit / 2)
                .map(|word| (word >> (16 * (unit % 2))) as u16)
        }));
        let alias = role == Role::Entries && name.contains(&u16::from(b'~'));
        for (key, hear) in subs {
            let hears = match hear {
                Hear::Any => true,
                Hear::Named(entry) => alias || *entry == name,
            };
            if hears {
                touched.push(*key);
            }
        }
        if next == 0 || next % 4 != 0 {
            break;
        }
        at += next as usize / 4;
    }
}

/// `path` as a NUL-terminated wide string.
fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    wide.push(0);
    Ok(wide)
}

/// Open the folder `path` names for its changes, refusing nobody else's use of it.
///
/// # Errors
/// [`io::ErrorKind::NotFound`] where nothing is there, [`io::ErrorKind::NotADirectory`] where a
/// file is, and the kernel's refusal otherwise.
fn open_directory(path: &Path) -> io::Result<(Handle, Identity)> {
    let name = wide(path)?;
    // SAFETY: a NUL-terminated path; the call returns a fresh handle or `INVALID_HANDLE_VALUE`.
    let raw = unsafe {
        ffi::CreateFileW(
            name.as_ptr(),
            FILE_LIST_DIRECTORY,
            FILE_SHARE_ALL,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
            std::ptr::null_mut(),
        )
    } as RawHandle;
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let handle = Handle(raw);
    let mut facts = [0u32; 13];
    // SAFETY: a live handle and the thirteen `DWORD`s of a `BY_HANDLE_FILE_INFORMATION`.
    if unsafe { ffi::GetFileInformationByHandle(handle.0, facts.as_mut_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if facts[0] & FILE_ATTRIBUTE_DIRECTORY == 0 {
        return Err(io::Error::from(io::ErrorKind::NotADirectory));
    }
    Ok((
        handle,
        Identity {
            volume: facts[7],
            index: (u64::from(facts[11]) << 32) | u64::from(facts[12]),
        },
    ))
}

/// Where the folder `handle` is open on really is: its path with every link and junction
/// followed, in the form [`std::fs::canonicalize`] gives.
fn final_path(handle: &Handle) -> io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt as _;
    let mut units = vec![0u16; 512];
    loop {
        // SAFETY: a live handle and a buffer of the length passed.
        let length = unsafe {
            ffi::GetFinalPathNameByHandleW(handle.0, units.as_mut_ptr(), units.len() as u32, 0)
        } as usize;
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        if length < units.len() {
            return Ok(PathBuf::from(std::ffi::OsString::from_wide(
                &units[..length],
            )));
        }
        units.resize(length + 1, 0);
    }
}

/// The account's application-data folders (`%APPDATA%`, `%LOCALAPPDATA%`) and its temporary
/// folder (where the control sockets are, usually inside `%LOCALAPPDATA%`) as they really are:
/// the only places this arm listens. One that is not set or not there is left out.
fn application_data() -> Vec<PathBuf> {
    ["APPDATA", "LOCALAPPDATA"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .chain([std::env::temp_dir()])
        .filter_map(|folder| std::fs::canonicalize(folder).ok())
        .collect()
}

/// Whether the volume `path` is on reports every writer's change to this machine: a disk of
/// its own does (fixed, removable, optical, RAM), a network one (a share, a `\\wsl$` mount)
/// need not, and one that cannot be asked is taken as not.
fn volume_reports_every_writer(path: &Path) -> bool {
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_CDROM: u32 = 5;
    const DRIVE_RAMDISK: u32 = 6;
    let Ok(name) = wide(path) else {
        return false;
    };
    let mut root = [0u16; 512];
    // SAFETY: a NUL-terminated path and a buffer of the length passed.
    if unsafe { ffi::GetVolumePathNameW(name.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0
    {
        return false;
    }
    // SAFETY: `GetVolumePathNameW` NUL-terminated the root it wrote.
    let kind = unsafe { ffi::GetDriveTypeW(root.as_ptr()) };
    matches!(
        kind,
        DRIVE_REMOVABLE | DRIVE_FIXED | DRIVE_CDROM | DRIVE_RAMDISK
    )
}

/// `GetQueuedCompletionStatus`'s timeout for a rest of `left`: none waits for a completion, one
/// already due polls without sleeping, and a rest under a millisecond is one, never a spin.
fn millis(left: Option<Duration>) -> u32 {
    left.map_or(INFINITE, |left| {
        let whole = left.as_millis();
        let up = whole + u128::from(left.subsec_nanos() % 1_000_000 != 0);
        u32::try_from(up).map_or(INFINITE - 1, |millis| millis.min(INFINITE - 1))
    })
}

/// The completion port every listener and the waker queue to.
struct Port(RawHandle);

impl Drop for Port {
    fn drop(&mut self) {
        // SAFETY: the port is live and this is its last owner.
        unsafe { CloseHandle(self.0) };
    }
}

/// What one look at the port took.
enum Took {
    TimedOut,
    Wake,
    /// The listener `id`'s read ended, with `bytes` of records, or `failed`.
    Read {
        id: usize,
        bytes: u32,
        failed: bool,
    },
}

pub(super) struct WinEvents {
    port: Arc<Port>,
    /// A wake is queued and not yet taken: more wakes add nothing.
    woken: Arc<AtomicBool>,
    next: usize,
    listeners: BTreeMap<usize, Listener>,
    by_path: BTreeMap<(PathBuf, Role), usize>,
    /// Each key's listeners and what it hears on each.
    keys: BTreeMap<u64, Vec<(usize, Hear)>>,
    /// The keys last armed where not every change is heard: on a volume that need not report
    /// every writer, or on a folder whose own name has no listener.
    elsewhere: BTreeSet<u64>,
    /// Let-go listeners' buffers, each until its cancelled read's completion is taken.
    retired: BTreeMap<usize, IoBox>,
    /// Keys of a listener found on a folder its path no longer names: touched at the next wait.
    stale: Vec<u64>,
    /// The folders this arm listens inside, as they really are ([`application_data`]): a
    /// listener anywhere else would keep a person's folders from being renamed.
    own: Vec<PathBuf>,
    /// The kernel refuses every listener on a folder above a key's own, as it does for an
    /// account that may not list that folder.
    #[cfg(test)]
    pub(super) deaf_above: bool,
}

impl WinEvents {
    pub(super) fn new() -> io::Result<Self> {
        Self::inside(application_data())
    }

    /// An arm that listens only inside `own`, folders named as they really are
    /// ([`std::fs::canonicalize`]).
    pub(super) fn inside(own: Vec<PathBuf>) -> io::Result<Self> {
        // SAFETY: no file handle and no existing port asks for a new port.
        let port = unsafe { ffi::CreateIoCompletionPort(INVALID_HANDLE_VALUE, 0, 0, 1) };
        if port == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            port: Arc::new(Port(port)),
            woken: Arc::new(AtomicBool::new(false)),
            next: 0,
            listeners: BTreeMap::new(),
            by_path: BTreeMap::new(),
            keys: BTreeMap::new(),
            elsewhere: BTreeSet::new(),
            retired: BTreeMap::new(),
            stale: Vec::new(),
            own,
            #[cfg(test)]
            deaf_above: false,
        })
    }

    /// Whether the folder really at `real` is one this arm listens on.
    fn owns(&self, real: &Path) -> bool {
        self.own.iter().any(|own| real.starts_with(own))
    }

    /// The listener of `role` on the folder `path` names now: the one already there while the
    /// path still names its folder, else a new one (the old one, on a folder renamed away or
    /// replaced, is let go and its keys touched).
    ///
    /// An entries listener is refused on a folder outside this arm's own ([`Self::owns`]),
    /// which it would pin the folders above. A names listener is on the folder above an entries
    /// listener, which pins that folder and all above it already.
    ///
    /// # Errors
    /// [`open_directory`]'s, [`io::ErrorKind::Unsupported`] for a folder this arm does not
    /// listen on, and the kernel's where it refuses the port or the first read.
    fn listen(&mut self, path: &Path, role: Role) -> io::Result<usize> {
        #[cfg(test)]
        if role == Role::Names && self.deaf_above {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied));
        }
        let (handle, identity) = open_directory(path)?;
        if role == Role::Entries && !self.owns(&final_path(&handle)?) {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        let slot = (path.to_path_buf(), role);
        if let Some(&id) = self.by_path.get(&slot) {
            if self
                .listeners
                .get(&id)
                .is_some_and(|listener| listener.identity == identity)
            {
                return Ok(id);
            }
            self.let_go(id, true);
        }
        let id = self.next;
        self.next += 1;
        // SAFETY: a live directory handle and this arm's live port.
        if unsafe { ffi::CreateIoCompletionPort(handle.0, self.port.0, id, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut listener = Listener {
            handle,
            path: slot.0.clone(),
            role,
            identity,
            io: IoBox::new(),
            reading: false,
            subs: Vec::new(),
            every_writer: volume_reports_every_writer(path),
        };
        if let Err(error) = listener.read() {
            listener.io.free();
            return Err(error);
        }
        self.listeners.insert(id, listener);
        self.by_path.insert(slot, id);
        Ok(id)
    }

    /// Stop the listener `id`, touching its keys at the next wait where `touch`. Its buffer is
    /// kept until its pending read's completion is taken.
    fn let_go(&mut self, id: usize, touch: bool) {
        let Some(listener) = self.listeners.remove(&id) else {
            return;
        };
        if touch {
            self.stale.extend(listener.keys());
        }
        let Listener {
            handle,
            path,
            role,
            io,
            reading,
            ..
        } = listener;
        let slot = (path, role);
        if self.by_path.get(&slot) == Some(&id) {
            self.by_path.remove(&slot);
        }
        if reading {
            // SAFETY: a live handle; a null `OVERLAPPED` cancels its one pending read, whose
            // completion (cancelled, or ended by the close below) is still queued to the port.
            unsafe { CancelIoEx(handle.0, std::ptr::null_mut()) };
            self.retired.insert(id, io);
        } else {
            io.free();
        }
        drop(handle);
    }

    /// `key` hears `new` in place of what it heard; a listener no key hears any more is let go.
    fn replace(&mut self, key: u64, new: Vec<(usize, Hear)>) {
        for (id, hear) in &new {
            if let Some(listener) = self.listeners.get_mut(id) {
                listener.subs.push((key, hear.clone()));
            }
        }
        let old = if new.is_empty() {
            self.keys.remove(&key)
        } else {
            self.keys.insert(key, new)
        };
        for (id, hear) in old.unwrap_or_default() {
            if let Some(listener) = self.listeners.get_mut(&id)
                && let Some(at) = listener
                    .subs
                    .iter()
                    .position(|(held, heard)| *held == key && *heard == hear)
            {
                listener.subs.swap_remove(at);
            }
        }
        let idle: Vec<usize> = self
            .listeners
            .iter()
            .filter(|(_, listener)| listener.subs.is_empty())
            .map(|(id, _)| *id)
            .collect();
        for id in idle {
            self.let_go(id, false);
        }
    }

    /// Whether `folder` still names the folder the listener `id` is on.
    fn still(&self, id: usize, folder: &Path) -> bool {
        open_directory(folder).is_ok_and(|(_, identity)| {
            self.listeners
                .get(&id)
                .is_some_and(|listener| listener.identity == identity)
        })
    }

    /// Hear `folder`'s own name made, removed or renamed, from the folder above it: whether it
    /// is heard. A root has no folder above it and no name to lose, so it is. Where the kernel
    /// refuses the listener above (an account that may pass through that folder and not list
    /// it), nothing tells of `folder` renamed away with its listener, and the key is looked at
    /// in the watch's rounds instead ([`FileEvents::hears_every_writer`]).
    fn above(&mut self, folder: &Path, subs: &mut Vec<(usize, Hear)>) -> bool {
        let (Some(parent), Some(name)) = (folder.parent(), folder.file_name()) else {
            return true;
        };
        match self.listen(parent, Role::Names) {
            Ok(id) => {
                subs.push((id, Hear::Named(folded(name.encode_wide()))));
                true
            }
            Err(_) => false,
        }
    }

    /// The listeners a file key on `path` hears, and whether every change to it is heard: its
    /// folder's volume reports every writer and its folder's own name has a listener.
    fn file_listeners(&mut self, path: &Path) -> io::Result<(Vec<(usize, Hear)>, bool)> {
        'again: for _ in 0..=super::ARM_AGAIN {
            for (directory, below) in super::folders_up(path) {
                let id = match self.listen(directory, Role::Entries) {
                    Ok(id) => id,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error),
                };
                let name = below
                    .file_name()
                    .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
                // A folder made between its failed listen and the listen above it, which
                // hears nothing inside it: arm again, lower.
                if below != path && below.exists() {
                    continue 'again;
                }
                let mut subs = vec![(id, Hear::Named(folded(name.encode_wide())))];
                let named = self.above(directory, &mut subs);
                // Renamed away before the listener above it took: arm again.
                if !self.still(id, directory) {
                    continue 'again;
                }
                let every_writer = named
                    && self
                        .listeners
                        .get(&id)
                        .is_some_and(|listener| listener.every_writer);
                return Ok((subs, every_writer));
            }
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        Err(io::Error::from(io::ErrorKind::WouldBlock))
    }

    /// The listeners a directory key on the existing folder `path` hears, and whether every
    /// change to it is heard, as [`Self::file_listeners`] says.
    fn directory_listeners(&mut self, path: &Path) -> io::Result<(Vec<(usize, Hear)>, bool)> {
        for _ in 0..=super::ARM_AGAIN {
            let id = self.listen(path, Role::Entries)?;
            let mut subs = vec![(id, Hear::Any)];
            let named = self.above(path, &mut subs);
            if !self.still(id, path) {
                continue;
            }
            let every_writer = named
                && self
                    .listeners
                    .get(&id)
                    .is_some_and(|listener| listener.every_writer);
            return Ok((subs, every_writer));
        }
        Err(io::Error::from(io::ErrorKind::WouldBlock))
    }

    /// `key` hears what `armed` found, or nothing where it failed.
    fn take(&mut self, key: u64, armed: io::Result<(Vec<(usize, Hear)>, bool)>) -> io::Result<()> {
        match armed {
            Ok((subs, every_writer)) => {
                self.replace(key, subs);
                if every_writer {
                    self.elsewhere.remove(&key);
                } else {
                    self.elsewhere.insert(key);
                }
                Ok(())
            }
            Err(error) => {
                self.replace(key, Vec::new());
                self.elsewhere.remove(&key);
                Err(error)
            }
        }
    }

    /// Take one completion from the port, resting at most `left`.
    fn dequeue(&self, left: Option<Duration>) -> io::Result<Took> {
        let mut bytes = 0u32;
        let mut id = 0usize;
        let mut overlapped: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: this arm's live port and three live out-slots.
        let taken = unsafe {
            ffi::GetQueuedCompletionStatus(
                self.port.0,
                &raw mut bytes,
                &raw mut id,
                &raw mut overlapped,
                millis(left),
            )
        };
        if !overlapped.is_null() {
            return Ok(Took::Read {
                id,
                bytes,
                failed: taken == 0,
            });
        }
        if taken != 0 {
            return Ok(Took::Wake);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(WAIT_TIMEOUT) {
            Ok(Took::TimedOut)
        } else {
            Err(error)
        }
    }

    /// The listener `id`'s read ended: push the keys its records touch and ask for more. One
    /// that failed (its folder went) or lost events touches every key on it; a failed one is let
    /// go, and the watch arms its keys again where their paths lead now.
    fn completed(&mut self, id: usize, bytes: u32, failed: bool, touched: &mut Vec<u64>) {
        if let Some(io) = self.retired.remove(&id) {
            io.free();
            return;
        }
        let Some(listener) = self.listeners.get_mut(&id) else {
            return;
        };
        listener.reading = false;
        if failed {
            touched.extend(listener.keys());
            self.let_go(id, false);
            return;
        }
        let role = listener.role;
        let from = touched.len();
        if bytes == 0 {
            // More changes than the buffer holds: any entry may have changed.
            touched.extend(listener.keys());
        } else {
            listener.route(bytes, touched);
        }
        if listener.read().is_err() {
            touched.extend(listener.keys());
            self.let_go(id, false);
        }
        if role == Role::Names {
            // The folder a key listens on was removed, renamed or made again: let the folder go
            // at once, not at the key's next arm. A handle held on a deleted folder keeps its
            // name taken (where the delete is not a POSIX one), so making it again is refused.
            for key in touched[from..].iter().copied() {
                self.let_go_entries(key);
            }
        }
    }

    /// Let go the entries listeners `key` hears, whose folder moved; every key on them is
    /// touched, and the watch arms each again.
    fn let_go_entries(&mut self, key: u64) {
        let ids: Vec<usize> = self
            .keys
            .get(&key)
            .into_iter()
            .flatten()
            .map(|(id, _)| *id)
            .filter(|id| {
                self.listeners
                    .get(id)
                    .is_some_and(|listener| listener.role == Role::Entries)
            })
            .collect();
        for id in ids {
            self.let_go(id, true);
        }
    }

    /// How many listeners are on, and how many let-go buffers still wait for their completion.
    #[cfg(test)]
    pub(super) fn held(&self) -> (usize, usize) {
        (self.listeners.len(), self.retired.len())
    }
}

impl FileEvents for WinEvents {
    fn arm(&mut self, key: u64, path: &Path) -> io::Result<()> {
        let armed = self.file_listeners(path);
        self.take(key, armed)
    }

    fn arm_directory(&mut self, key: u64, path: &Path) -> io::Result<()> {
        let armed = self.directory_listeners(path);
        self.take(key, armed)
    }

    fn disarm(&mut self, key: u64) {
        self.replace(key, Vec::new());
        self.elsewhere.remove(&key);
    }

    fn hears_every_writer(&self, key: u64) -> bool {
        !self.elsewhere.contains(&key)
    }

    fn pins_folders(&self) -> bool {
        true
    }

    /// Whether `path`, or the nearest folder above it that is there, really is inside this
    /// arm's own folders.
    fn listens_for(&self, path: &Path) -> bool {
        if std::fs::canonicalize(path).is_ok_and(|real| self.owns(&real)) {
            return true;
        }
        super::folders_up(path)
            .find_map(|(folder, _)| std::fs::canonicalize(folder).ok())
            .is_some_and(|real| self.owns(&real))
    }

    /// Events that touch no key end no wait (a neighbour's write in a watched folder): the wait
    /// goes on for what is left of `timeout`. Once a key is touched or the wait is woken it
    /// takes what else is already queued, up to [`DRAINS_PER_TURN`], and returns.
    fn wait(&mut self, timeout: Option<Duration>, touched: &mut Vec<u64>) -> io::Result<()> {
        let before = touched.len();
        touched.append(&mut self.stale);
        let deadline = timeout.and_then(|timeout| Instant::now().checked_add(timeout));
        let mut woken = false;
        let mut drains = 0usize;
        loop {
            let left = if woken || touched.len() > before {
                if drains == DRAINS_PER_TURN {
                    break;
                }
                drains += 1;
                Some(Duration::ZERO)
            } else {
                match (timeout, deadline) {
                    (Some(_), Some(deadline)) => {
                        Some(deadline.saturating_duration_since(Instant::now()))
                    }
                    (timeout, _) => timeout,
                }
            };
            match self.dequeue(left)? {
                Took::TimedOut => break,
                Took::Wake => {
                    self.woken.store(false, Ordering::Release);
                    woken = true;
                }
                Took::Read { id, bytes, failed } => self.completed(id, bytes, failed, touched),
            }
        }
        touched[before..].sort_unstable();
        Ok(())
    }

    fn waker(&self) -> Waker {
        let port = Arc::clone(&self.port);
        let woken = Arc::clone(&self.woken);
        Arc::new(move || {
            if !woken.swap(true, Ordering::AcqRel) {
                // SAFETY: the port lives as long as this waker holds it; a wake carries no
                // `OVERLAPPED`.
                unsafe { ffi::PostQueuedCompletionStatus(port.0, 0, WAKE, std::ptr::null_mut()) };
            }
        })
    }
}

impl Drop for WinEvents {
    fn drop(&mut self) {
        let ids: Vec<usize> = self.listeners.keys().copied().collect();
        for id in ids {
            self.let_go(id, false);
        }
        // Each cancelled read still completes, and until it does the kernel may write its
        // buffer: free each as its completion comes, and leak what does not come in time.
        let deadline = Instant::now().checked_add(LET_GO_WITHIN);
        while !self.retired.is_empty() {
            let left = deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
            match self.dequeue(left.or(Some(Duration::ZERO))) {
                Ok(Took::Read { id, .. }) => {
                    if let Some(io) = self.retired.remove(&id) {
                        io.free();
                    }
                }
                Ok(Took::Wake) => {}
                Ok(Took::TimedOut) | Err(_) => break,
            }
        }
    }
}

#[cfg(test)]
#[path = "file_watch_win_tests.rs"]
mod tests;
