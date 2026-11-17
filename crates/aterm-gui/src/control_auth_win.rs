// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows implementations behind [`control_auth`](super).

use std::path::{Path, PathBuf};

use aterm_uds::CtlStream;

/// No hold here: a Windows file lock is mandatory, so one on the token would refuse a client's
/// read of it, and [`aterm_uds::ownerlock::probe`] has no wiring on this platform either.
#[derive(Debug)]
pub(crate) struct SocketOwner;

impl SocketOwner {
    /// The Unix twin takes the token file's lock; here there is nothing to take.
    pub(crate) const fn hold(_token_path: &Path) -> Option<Self> {
        Some(Self)
    }
}

/// Create `dir` (and parents) if absent.
pub(crate) fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    if aterm_uds::elevation::is_elevated()
        && let Some(root) = aterm_uds::control_socket_dir()
        && dir.starts_with(&root)
    {
        return acl::ensure_elevated_dir(dir, dir == root);
    }
    std::fs::create_dir_all(dir)?;
    acl::verify_owner_and_harden(dir)
}

/// [`ensure_private_dir`] for a directory that will hold a control socket and its token.
pub(crate) fn ensure_control_dir(dir: &Path) -> std::io::Result<()> {
    if aterm_uds::elevation::is_elevated() {
        return acl::ensure_elevated_dir(dir, true);
    }
    ensure_private_dir(dir)
}

/// Pid liveness via [`aterm_uds::process::pid_alive`] (access-denied counts as alive).
pub(crate) fn pid_alive(pid: u32) -> bool {
    aterm_uds::process::pid_alive(pid)
}

/// `%LOCALAPPDATA%\aterm` (else `%USERPROFILE%\AppData\Local\aterm`).
pub(crate) fn legacy_socket_dirs() -> Vec<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| aterm_types::dirs::home_dir().map(|h| h.join("AppData").join("Local")));
    base.map(|b| b.join("aterm")).into_iter().collect()
}

/// Write a fresh per-launch capability token to `path` and return its hex.
#[must_use]
pub(crate) fn provision_token(path: &Path) -> Option<String> {
    use std::io::Write;
    let token = super::random_token_hex()?;
    let _ = std::fs::remove_file(path);
    let mut f = if aterm_uds::elevation::is_elevated() {
        acl::create_elevated_file(path).ok()?
    } else {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .ok()?
    };
    f.write_all(token.as_bytes()).ok()?;
    f.flush().ok()?;
    Some(token)
}

/// No-op: no mode bits; the socket file inherits the private directory's ACL.
pub(crate) fn lock_socket_file(_path: &str) {}

/// The accept-time peer gate.
pub(crate) fn peer_check(stream: &CtlStream) -> Result<(), String> {
    let Ok(pid) = aterm_uds::fdpass::peer_pid(stream) else {
        return Ok(());
    };
    match acl::peer_user(pid) {
        acl::PeerUser::Same | acl::PeerUser::Unreadable => Ok(()),
        acl::PeerUser::Other => Err(format!("connect (peer pid {pid} runs as another user)")),
        acl::PeerUser::Denied => Err(format!(
            "connect (peer pid {pid} belongs to a principal this user may not query)"
        )),
    }
}

/// The control folder's two postures over [`aterm_uds::acl`], which builds and writes their
/// descriptors: the shared one (the token owner, SYSTEM and Administrators, each passed on) and
/// the elevated one ([`ELEVATED_DIR_SDDL`]); and the peer's user.
mod acl {
    #![allow(non_snake_case, reason = "Win32 API names are camel-case by contract")]

    use std::ffi::c_void;
    use std::fs::File;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::ptr::null_mut;
    use std::sync::{Mutex, PoisonError};

    pub(super) use aterm_uds::acl::Descriptor;
    use aterm_uds::acl::{Grantee, Inherit, Parts, Sid, WellKnown};

    type Handle = isize;

    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_USER_CLASS: u32 = 1;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const ERROR_ACCESS_DENIED: u32 = 5;
    const ERROR_ALREADY_EXISTS: i32 = 183;
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0010;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    const FILE_LIST_DIRECTORY: u32 = 0x0001;
    const READ_CONTROL: u32 = 0x0002_0000;
    const WRITE_DAC: u32 = 0x0004_0000;
    const WRITE_OWNER: u32 = 0x0008_0000;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_READ: u32 = 0x0001;
    const FILE_SHARE_WRITE: u32 = 0x0002;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    /// The elevated control directory's descriptor.
    pub(super) const ELEVATED_DIR_SDDL: &str =
        "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;RC;;;OW)S:(ML;OICI;NRNW;;;HI)";
    /// [`ELEVATED_DIR_SDDL`] for one file (the token): the same entries, nothing to inherit.
    pub(super) const ELEVATED_FILE_SDDL: &str =
        "O:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;RC;;;OW)S:(ML;;NRNW;;;HI)";

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn OpenProcessToken(
            ProcessHandle: Handle,
            DesiredAccess: u32,
            TokenHandle: *mut Handle,
        ) -> i32;
        fn GetTokenInformation(
            TokenHandle: Handle,
            TokenInformationClass: u32,
            TokenInformation: *mut c_void,
            TokenInformationLength: u32,
            ReturnLength: *mut u32,
        ) -> i32;
        fn EqualSid(pSid1: *mut c_void, pSid2: *mut c_void) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> Handle;
        fn OpenProcess(dwDesiredAccess: u32, bInheritHandle: i32, dwProcessId: u32) -> Handle;
        fn GetLastError() -> u32;
        fn CloseHandle(hObject: Handle) -> i32;
    }

    /// What [`peer_user`] found out about a peer process.
    pub(super) enum PeerUser {
        /// Its token runs as this process's user.
        Same,
        /// Its token runs as another user.
        Other,
        /// This user may not even query it: another principal's process.
        Denied,
        /// It could not be read.
        Unreadable,
    }

    /// Whether process `pid` runs as the same user as this one.
    pub(super) fn peer_user(pid: u32) -> PeerUser {
        // SAFETY: GetCurrentProcess returns a pseudo-handle, always valid.
        let Some(ours) = token_info(unsafe { GetCurrentProcess() }, TOKEN_USER_CLASS) else {
            return PeerUser::Unreadable;
        };
        // SAFETY: plain query-access open of a process by id; checked below.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process == 0 {
            // SAFETY: reads this thread's last-error value, set by OpenProcess.
            return if unsafe { GetLastError() } == ERROR_ACCESS_DENIED {
                PeerUser::Denied
            } else {
                PeerUser::Unreadable
            };
        }
        let theirs = token_info(process, TOKEN_USER_CLASS);
        // SAFETY: `process` is the live handle OpenProcess returned.
        unsafe { CloseHandle(process) };
        let Some(theirs) = theirs else {
            return PeerUser::Unreadable;
        };
        let (a, b) = (sid_ptr(&ours), sid_ptr(&theirs));
        if a.is_null() || b.is_null() {
            return PeerUser::Unreadable;
        }
        // SAFETY: both point at SIDs inside the live `ours`/`theirs` buffers.
        if unsafe { EqualSid(a, b) } != 0 {
            PeerUser::Same
        } else {
            PeerUser::Other
        }
    }

    fn token_info(process: Handle, class: u32) -> Option<Vec<u8>> {
        let mut token: Handle = 0;
        // SAFETY: `process` is a live (or pseudo) process handle; TOKEN_QUERY only.
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
            return None;
        }
        let mut len: u32 = 0;
        // SAFETY: passing a null buffer with length 0 to learn ReturnLength.
        unsafe { GetTokenInformation(token, class, null_mut(), 0, &mut len) };
        if len == 0 {
            // SAFETY: `token` is a live handle from OpenProcessToken.
            unsafe { CloseHandle(token) };
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        // SAFETY: `buf` has `len` bytes; TokenOwner/TokenUser write their struct with the SID
        // pointer at its head, plus the trailing SID.
        let ok = unsafe {
            GetTokenInformation(
                token,
                class,
                buf.as_mut_ptr().cast::<c_void>(),
                len,
                &mut len,
            )
        };
        // SAFETY: `token` is still the live handle from OpenProcessToken.
        unsafe { CloseHandle(token) };
        (ok != 0).then_some(buf)
    }

    fn sid_ptr(buf: &[u8]) -> *mut c_void {
        if buf.len() < std::mem::size_of::<*mut c_void>() {
            return null_mut();
        }
        // SAFETY: `buf` is at least pointer-sized; TOKEN_USER begins with the PSID.
        unsafe { *(buf.as_ptr().cast::<*mut c_void>()) }
    }

    /// Refuse `dir` unless the current user owns it.
    pub(super) fn verify_owner_and_harden(dir: &Path) -> io::Result<()> {
        let token_owner = Sid::token_owner()
            .map_err(|e| io::Error::other(format!("cannot query process token owner SID: {e}")))?;
        verify_owner_and_harden_with(dir, &token_owner, &owner_of_path)
    }

    /// The owner part of the descriptor of the file or directory at `dir`, read by its name.
    pub(super) fn owner_of_path(dir: &Path) -> io::Result<Descriptor> {
        Descriptor::of_path(dir, Parts::OWNER)
    }

    /// [`verify_owner_and_harden`] with its two facts injected.
    pub(super) fn verify_owner_and_harden_with(
        dir: &Path,
        token_owner: &Sid,
        read_owner: &dyn Fn(&Path) -> io::Result<Descriptor>,
    ) -> io::Result<()> {
        let owner = read_owner(dir)?.owner()?;
        owner_verdict(owner.as_ref(), token_owner)?;
        harden_dacl(dir, token_owner)
    }

    /// The owner check itself.
    pub(super) fn owner_verdict(dir_owner: Option<&Sid>, token_owner: &Sid) -> io::Result<()> {
        if dir_owner == Some(token_owner) {
            return Ok(());
        }
        let by_admins = dir_owner.is_some_and(|owner| owner.is(WellKnown::Administrators));
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            if by_admins {
                "control directory is owned by Administrators (an elevated aterm made \
                 it); remove it from an elevated shell"
            } else {
                "control directory is not owned by the current user"
            },
        ))
    }

    /// The shared posture's protected list, by `dir`'s name: the token owner, SYSTEM and
    /// Administrators, full access, each passed on to what is inside.
    fn harden_dacl(dir: &Path, owner: &Sid) -> io::Result<()> {
        let dacl = aterm_uds::acl::protected_dacl(
            &[
                Grantee::Sid(owner),
                Grantee::WellKnown(WellKnown::LocalSystem),
                Grantee::WellKnown(WellKnown::Administrators),
            ],
            Inherit::ToChildren,
        );
        Descriptor::from_sddl(&dacl)?.apply_to_path(dir, Parts::DACL | Parts::PROTECTED_DACL)
    }

    static PINNED: Mutex<Vec<(PathBuf, File)>> = Mutex::new(Vec::new());

    /// Give `dir` the elevated posture ([`ELEVATED_DIR_SDDL`]).
    pub(super) fn ensure_elevated_dir(dir: &Path, pin: bool) -> io::Result<()> {
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let sd = Descriptor::from_sddl(ELEVATED_DIR_SDDL)?;
        create_dir_with(dir, &sd)?;
        let handle = open_dir_nofollow(dir)?;
        verify_elevated_owner(&handle, dir)?;
        sd.apply(&handle, Parts::DACL | Parts::PROTECTED_DACL | Parts::LABEL)?;
        if pin {
            let mut pinned = PINNED.lock().unwrap_or_else(PoisonError::into_inner);
            if !pinned.iter().any(|(path, _)| path == dir) {
                pinned.push((dir.to_path_buf(), handle));
            }
        }
        Ok(())
    }

    /// Create `path` for writing (`CREATE_NEW`: never through a planted link).
    pub(super) fn create_elevated_file(path: &Path) -> io::Result<File> {
        use std::os::windows::fs::OpenOptionsExt as _;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .access_mode(GENERIC_WRITE | WRITE_DAC | WRITE_OWNER)
            .share_mode(0)
            .open(path)?;
        let applied = Descriptor::from_sddl(ELEVATED_FILE_SDDL).and_then(|sd| {
            sd.apply(
                &file,
                Parts::OWNER | Parts::DACL | Parts::PROTECTED_DACL | Parts::LABEL,
            )
        });
        match applied {
            Ok(()) => Ok(file),
            Err(e) => {
                drop(file);
                let _ = std::fs::remove_file(path);
                Err(e)
            }
        }
    }

    /// `dir` made with `sd` from its creation; one already there is left as it is.
    fn create_dir_with(dir: &Path, sd: &Descriptor) -> io::Result<()> {
        match aterm_uds::acl::create_dir(dir, sd) {
            Err(e) if e.raw_os_error() != Some(ERROR_ALREADY_EXISTS) => Err(e),
            _ => Ok(()),
        }
    }

    /// `dir` itself, never a junction's or symlink's target, opened to read and rewrite its
    /// security.
    pub(super) fn open_dir_nofollow(dir: &Path) -> io::Result<File> {
        use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
        let file = std::fs::OpenOptions::new()
            .access_mode(FILE_LIST_DIRECTORY | READ_CONTROL | WRITE_DAC | WRITE_OWNER | SYNCHRONIZE)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(dir)?;
        let attributes = file.metadata()?.file_attributes();
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || attributes & FILE_ATTRIBUTE_DIRECTORY == 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is a link or not a directory", dir.display()),
            ));
        }
        Ok(file)
    }

    fn verify_elevated_owner(handle: &File, dir: &Path) -> io::Result<()> {
        let owner = Descriptor::of(handle, Parts::OWNER)?.owner()?;
        elevated_owner_verdict(owner.as_ref(), dir)
    }

    /// The elevated owner check itself.
    pub(super) fn elevated_owner_verdict(owner: Option<&Sid>, dir: &Path) -> io::Result<()> {
        let elevated_owner = owner.is_some_and(|owner| {
            owner.is(WellKnown::Administrators) || owner.is(WellKnown::LocalSystem)
        });
        if elevated_owner {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} is not owned by Administrators or SYSTEM, so an unelevated process may \
                 control it; an elevated aterm keeps no control socket there",
                dir.display()
            ),
        ))
    }
}

#[cfg(test)]
#[path = "control_auth_win_tests.rs"]
mod tests;
