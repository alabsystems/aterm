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

mod acl {
    #![allow(non_snake_case, reason = "Win32 API names are camel-case by contract")]

    use std::ffi::c_void;
    use std::fs::File;
    use std::io;
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle as _;
    use std::path::{Path, PathBuf};
    use std::ptr::null_mut;
    use std::sync::{Mutex, PoisonError};

    type Handle = isize;

    const SE_FILE_OBJECT: u32 = 1;
    const OWNER_SECURITY_INFORMATION: u32 = 0x0000_0001;
    const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
    const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;
    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_OWNER_CLASS: u32 = 4;
    const TOKEN_USER_CLASS: u32 = 1;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const ERROR_ACCESS_DENIED: u32 = 5;
    const ERROR_ALREADY_EXISTS: u32 = 183;
    const SDDL_REVISION_1: u32 = 1;
    const ERROR_SUCCESS: u32 = 0;
    const LABEL_SECURITY_INFORMATION: u32 = 0x0000_0010;
    const WIN_LOCAL_SYSTEM_SID: u32 = 22;
    const WIN_BUILTIN_ADMINISTRATORS_SID: u32 = 26;
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

    /// Win32 `SECURITY_ATTRIBUTES`.
    #[repr(C)]
    struct SecurityAttributes {
        nLength: u32,
        lpSecurityDescriptor: *mut c_void,
        bInheritHandle: i32,
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn GetSecurityInfo(
            handle: Handle,
            ObjectType: u32,
            SecurityInfo: u32,
            ppsidOwner: *mut *mut c_void,
            ppsidGroup: *mut *mut c_void,
            ppDacl: *mut *mut c_void,
            ppSacl: *mut *mut c_void,
            ppSecurityDescriptor: *mut *mut c_void,
        ) -> u32;
        fn SetSecurityInfo(
            handle: Handle,
            ObjectType: u32,
            SecurityInfo: u32,
            psidOwner: *mut c_void,
            psidGroup: *mut c_void,
            pDacl: *mut c_void,
            pSacl: *mut c_void,
        ) -> u32;
        fn GetSecurityDescriptorOwner(
            pSecurityDescriptor: *mut c_void,
            pOwner: *mut *mut c_void,
            lpbOwnerDefaulted: *mut i32,
        ) -> i32;
        fn GetSecurityDescriptorSacl(
            pSecurityDescriptor: *mut c_void,
            lpbSaclPresent: *mut i32,
            pSacl: *mut *mut c_void,
            lpbSaclDefaulted: *mut i32,
        ) -> i32;
        fn IsWellKnownSid(pSid: *mut c_void, WellKnownSidType: u32) -> i32;
        fn GetNamedSecurityInfoW(
            pObjectName: *const u16,
            ObjectType: u32,
            SecurityInfo: u32,
            ppsidOwner: *mut *mut c_void,
            ppsidGroup: *mut *mut c_void,
            ppDacl: *mut *mut c_void,
            ppSacl: *mut *mut c_void,
            ppSecurityDescriptor: *mut *mut c_void,
        ) -> u32;
        fn SetNamedSecurityInfoW(
            pObjectName: *mut u16,
            ObjectType: u32,
            SecurityInfo: u32,
            psidOwner: *mut c_void,
            psidGroup: *mut c_void,
            pDacl: *mut c_void,
            pSacl: *mut c_void,
        ) -> u32;
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            StringSecurityDescriptor: *const u16,
            StringSDRevision: u32,
            SecurityDescriptor: *mut *mut c_void,
            SecurityDescriptorSize: *mut u32,
        ) -> i32;
        fn GetSecurityDescriptorDacl(
            pSecurityDescriptor: *mut c_void,
            lpbDaclPresent: *mut i32,
            pDacl: *mut *mut c_void,
            lpbDaclDefaulted: *mut i32,
        ) -> i32;
        fn ConvertSidToStringSidW(Sid: *mut c_void, StringSid: *mut *mut u16) -> i32;
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
        fn CreateDirectoryW(
            lpPathName: *const u16,
            lpSecurityAttributes: *mut SecurityAttributes,
        ) -> i32;
        fn GetCurrentProcess() -> Handle;
        fn OpenProcess(dwDesiredAccess: u32, bInheritHandle: i32, dwProcessId: u32) -> Handle;
        fn GetLastError() -> u32;
        fn CloseHandle(hObject: Handle) -> i32;
        fn LocalFree(hMem: *mut c_void) -> *mut c_void;
    }

    fn to_wide(p: &Path) -> Vec<u16> {
        p.as_os_str().encode_wide().chain(once(0)).collect()
    }

    /// Copy a LocalAlloc'd wide C string into an owned `String`.
    /// # Safety
    /// `p` is non-null and points at a NUL-terminated UTF-16 string.
    unsafe fn wide_ptr_to_string(p: *const u16) -> String {
        let mut len = 0isize;
        // SAFETY: caller guarantees a NUL terminator within the allocation.
        while unsafe { *p.offset(len) } != 0 {
            len += 1;
        }
        // SAFETY: `len` u16s precede the NUL, all within the same allocation.
        let slice = unsafe { std::slice::from_raw_parts(p, len as usize) };
        String::from_utf16_lossy(slice)
    }

    fn current_token_owner_info() -> Option<Vec<u8>> {
        // SAFETY: GetCurrentProcess returns a pseudo-handle, always valid.
        token_info(unsafe { GetCurrentProcess() }, TOKEN_OWNER_CLASS)
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
        let owner_buf = current_token_owner_info()
            .ok_or_else(|| io::Error::other("cannot query process token owner SID"))?;
        let self_owner_sid = sid_ptr(&owner_buf);
        if self_owner_sid.is_null() {
            return Err(io::Error::other("process token has no owner SID"));
        }
        verify_owner_and_harden_with(dir, self_owner_sid, &Descriptor::owner_of_path)
    }

    /// [`verify_owner_and_harden`] with its two facts injected.
    pub(super) fn verify_owner_and_harden_with(
        dir: &Path,
        token_owner: *mut c_void,
        read_owner: &dyn Fn(&[u16]) -> io::Result<Descriptor>,
    ) -> io::Result<()> {
        let mut wide = to_wide(dir);
        let owner = read_owner(&wide)?;
        owner_verdict(owner.owner()?, token_owner)?;
        harden_dacl(&mut wide, token_owner)
    }

    /// The owner check itself.
    pub(super) fn owner_verdict(
        dir_owner: *mut c_void,
        token_owner: *mut c_void,
    ) -> io::Result<()> {
        // SAFETY: both are null or valid SIDs inside descriptors or token blobs the caller keeps
        // alive for this call.
        let owned_by_us = !dir_owner.is_null()
            && !token_owner.is_null()
            && unsafe { EqualSid(dir_owner, token_owner) } != 0;
        if owned_by_us {
            return Ok(());
        }
        // SAFETY: as above.
        let by_admins = !dir_owner.is_null()
            && unsafe { IsWellKnownSid(dir_owner, WIN_BUILTIN_ADMINISTRATORS_SID) } != 0;
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

    fn harden_dacl(wide: &mut [u16], owner_sid: *mut c_void) -> io::Result<()> {
        let mut sid_str: *mut u16 = null_mut();
        // SAFETY: `owner_sid` is a valid SID from our own token blob.
        if unsafe { ConvertSidToStringSidW(owner_sid, &mut sid_str) } == 0 || sid_str.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: on success `sid_str` is a NUL-terminated LocalAlloc'd wide string.
        let owner_sid_str = unsafe { wide_ptr_to_string(sid_str) };
        // SAFETY: `sid_str` was LocalAlloc'd by ConvertSidToStringSidW.
        unsafe { LocalFree(sid_str.cast::<c_void>()) };

        let sddl = format!("D:P(A;OICI;FA;;;{owner_sid_str})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)");
        let sddl_w: Vec<u16> = sddl.encode_utf16().chain(once(0)).collect();

        let mut psd: *mut c_void = null_mut();
        // SAFETY: `sddl_w` is a NUL-terminated SDDL string; we own `psd` on success.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl_w.as_ptr(),
                SDDL_REVISION_1,
                &mut psd,
                null_mut(),
            )
        } == 0
            || psd.is_null()
        {
            return Err(io::Error::last_os_error());
        }

        let mut present: i32 = 0;
        let mut dacl: *mut c_void = null_mut();
        let mut defaulted: i32 = 0;
        // SAFETY: `psd` is the descriptor we just built; out-params are owned locals.
        let got =
            unsafe { GetSecurityDescriptorDacl(psd, &mut present, &mut dacl, &mut defaulted) };
        if got == 0 {
            let e = io::Error::last_os_error();
            // SAFETY: `psd` was LocalAlloc'd by the convert call above.
            unsafe { LocalFree(psd) };
            return Err(e);
        }
        // SAFETY: `wide` is a NUL-terminated path; `dacl` points into `psd`, live until the
        // LocalFree below; we set only the (protected) DACL.
        let rc = unsafe {
            SetNamedSecurityInfoW(
                wide.as_mut_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                dacl,
                null_mut(),
            )
        };
        // SAFETY: `psd` was LocalAlloc'd by the convert call above.
        unsafe { LocalFree(psd) };
        if rc != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(rc as i32));
        }
        Ok(())
    }

    /// A security descriptor.
    pub(super) struct Descriptor(*mut c_void);

    impl Descriptor {
        /// `sddl` as a security descriptor.
        pub(super) fn from_sddl(sddl: &str) -> io::Result<Self> {
            let wide: Vec<u16> = sddl.encode_utf16().chain(once(0)).collect();
            let mut psd: *mut c_void = null_mut();
            // SAFETY: `wide` is a NUL-terminated SDDL string; we own `psd` on success.
            let ok = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    wide.as_ptr(),
                    SDDL_REVISION_1,
                    &mut psd,
                    null_mut(),
                )
            };
            if ok == 0 || psd.is_null() {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(psd))
        }

        /// The owner part of the descriptor of the file or directory at `wide`.
        pub(super) fn owner_of_path(wide: &[u16]) -> io::Result<Self> {
            let mut owner: *mut c_void = null_mut();
            let mut psd: *mut c_void = null_mut();
            // SAFETY: `wide` is a NUL-terminated path; out-pointers we don't request
            // (group/DACL/SACL) are null, matching the single OWNER info bit. On success the
            // descriptor is LocalAlloc'd and owned by the result.
            let rc = unsafe {
                GetNamedSecurityInfoW(
                    wide.as_ptr(),
                    SE_FILE_OBJECT,
                    OWNER_SECURITY_INFORMATION,
                    &mut owner,
                    null_mut(),
                    null_mut(),
                    null_mut(),
                    &mut psd,
                )
            };
            if rc != ERROR_SUCCESS {
                return Err(io::Error::from_raw_os_error(rc as i32));
            }
            if psd.is_null() {
                return Err(io::Error::other("no security descriptor"));
            }
            Ok(Self(psd))
        }

        /// The descriptor itself, live while `self` is.
        pub(super) fn as_ptr(&self) -> *mut c_void {
            self.0
        }

        /// Its owner SID, pointing into it.
        pub(super) fn owner(&self) -> io::Result<*mut c_void> {
            let mut owner: *mut c_void = null_mut();
            let mut defaulted: i32 = 0;
            // SAFETY: `self.0` is a live descriptor; out-params are owned locals.
            if unsafe { GetSecurityDescriptorOwner(self.0, &mut owner, &mut defaulted) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(owner)
        }

        fn dacl(&self) -> io::Result<*mut c_void> {
            let (mut present, mut defaulted): (i32, i32) = (0, 0);
            let mut dacl: *mut c_void = null_mut();
            // SAFETY: `self.0` is a live descriptor; out-params are owned locals.
            let ok = unsafe {
                GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted)
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(dacl)
        }

        fn sacl(&self) -> io::Result<*mut c_void> {
            let (mut present, mut defaulted): (i32, i32) = (0, 0);
            let mut sacl: *mut c_void = null_mut();
            // SAFETY: `self.0` is a live descriptor; out-params are owned locals.
            let ok = unsafe {
                GetSecurityDescriptorSacl(self.0, &mut present, &mut sacl, &mut defaulted)
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(sacl)
        }
    }

    impl Drop for Descriptor {
        fn drop(&mut self) {
            // SAFETY: the convert call or GetNamedSecurityInfoW LocalAlloc'd the descriptor; freed
            // once.
            unsafe { LocalFree(self.0) };
        }
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
        apply(
            &handle,
            &sd,
            DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION
                | LABEL_SECURITY_INFORMATION,
        )?;
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
            apply(
                &file,
                &sd,
                OWNER_SECURITY_INFORMATION
                    | DACL_SECURITY_INFORMATION
                    | PROTECTED_DACL_SECURITY_INFORMATION
                    | LABEL_SECURITY_INFORMATION,
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

    fn create_dir_with(dir: &Path, sd: &Descriptor) -> io::Result<()> {
        let wide = to_wide(dir);
        let mut attributes = SecurityAttributes {
            nLength: std::mem::size_of::<SecurityAttributes>() as u32,
            lpSecurityDescriptor: sd.as_ptr(),
            bInheritHandle: 0,
        };
        // SAFETY: `wide` is a NUL-terminated path; `attributes` and the descriptor it names outlive
        // the call.
        if unsafe { CreateDirectoryW(wide.as_ptr(), &mut attributes) } != 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        if e.raw_os_error() == Some(ERROR_ALREADY_EXISTS as i32) {
            Ok(())
        } else {
            Err(e)
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
        let mut owner: *mut c_void = null_mut();
        let mut psd: *mut c_void = null_mut();
        // SAFETY: `handle` is a live handle opened with READ_CONTROL; only the owner is asked for,
        // the other out-pointers are null.
        let rc = unsafe {
            GetSecurityInfo(
                handle.as_raw_handle() as Handle,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &mut owner,
                null_mut(),
                null_mut(),
                null_mut(),
                &mut psd,
            )
        };
        if rc != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(rc as i32));
        }
        let _owned = (!psd.is_null()).then(|| Descriptor(psd));
        elevated_owner_verdict(owner, dir)
    }

    /// The elevated owner check itself.
    pub(super) fn elevated_owner_verdict(owner: *mut c_void, dir: &Path) -> io::Result<()> {
        // SAFETY: `owner` is null or a valid SID inside a descriptor the caller keeps alive for
        // this call.
        let elevated_owner = !owner.is_null()
            && unsafe {
                IsWellKnownSid(owner, WIN_BUILTIN_ADMINISTRATORS_SID) != 0
                    || IsWellKnownSid(owner, WIN_LOCAL_SYSTEM_SID) != 0
            };
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

    fn apply(handle: &File, sd: &Descriptor, info: u32) -> io::Result<()> {
        let owner = if info & OWNER_SECURITY_INFORMATION != 0 {
            sd.owner()?
        } else {
            null_mut()
        };
        let (dacl, sacl) = (sd.dacl()?, sd.sacl()?);
        // SAFETY: `handle` is live with WRITE_DAC | WRITE_OWNER; the SID and the ACLs point into
        // `sd`, which outlives the call.
        let rc = unsafe {
            SetSecurityInfo(
                handle.as_raw_handle() as Handle,
                SE_FILE_OBJECT,
                info,
                owner,
                null_mut(),
                dacl,
                sacl,
            )
        };
        if rc != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(rc as i32));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "control_auth_win_tests.rs"]
mod tests;
