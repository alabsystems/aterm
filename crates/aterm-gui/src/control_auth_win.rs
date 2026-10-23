// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows implementations behind [`control_auth`](super).

use std::path::{Path, PathBuf};

use aterm_uds::CtlStream;

/// Create `dir` (and parents) if absent, refuse it unless the current user owns it, and tighten its DACL to a non-inheriting user + SYSTEM + Administrators grant: the Windows twin of the Unix owner check and `0700`.
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

/// `%LOCALAPPDATA%\aterm` (else `%USERPROFILE%\AppData\Local\aterm`), where older builds bound the control socket, so the launch sweep can retire the `.sock` files left there; `sweep_stale_instances` only walks the current directory.
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

/// The accept-time peer gate: the peer pid afunix recorded, and the user its token runs as.
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

/// Hand-rolled advapi32/kernel32 FFI for the control-directory owner check + DACL
/// hardening — the Windows analog of the Unix SEC-3 owner gate + `0700` chmod —
/// and the elevated posture.
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

    /// Win32 `HANDLE`.
    type Handle = isize;

    const SE_FILE_OBJECT: u32 = 1;
    const OWNER_SECURITY_INFORMATION: u32 = 0x0000_0001;
    const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
    const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;
    const TOKEN_QUERY: u32 = 0x0008;
    /// `TOKEN_INFORMATION_CLASS::TokenOwner`: the default owner of objects this process
    /// creates, the user SID for a standard user and the Administrators SID when elevated.
    const TOKEN_OWNER_CLASS: u32 = 4;
    /// `TOKEN_INFORMATION_CLASS::TokenUser` — the user the token runs as.
    const TOKEN_USER_CLASS: u32 = 1;
    /// Enough to open another process's token for a query; granted on a
    /// same-user process even when it is elevated.
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const ERROR_ACCESS_DENIED: u32 = 5;
    const ERROR_ALREADY_EXISTS: u32 = 183;
    const SDDL_REVISION_1: u32 = 1;
    const ERROR_SUCCESS: u32 = 0;
    /// `SetSecurityInfo`'s mandatory-label part (the SACL's `ML` entries).
    const LABEL_SECURITY_INFORMATION: u32 = 0x0000_0010;
    /// `WELL_KNOWN_SID_TYPE`s an elevated control directory's owner may be.
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
    /// Opens a directory at all.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    /// Opens a junction or symlink itself, never its target.
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    /// The elevated control directory's descriptor (`O:BA`: owned by Administrators; `D:P`: a protected DACL, nothing inherited from the user's temp folder): full access for SYSTEM and Administrators, inherited by everything created inside (`OICI`); OWNER RIGHTS (`OW`) held to `READ_CONTROL`, so an owner other than Administrators gets no implicit `WRITE_DAC`; and a High mandatory label (`S:(ML;…;NRNW;;;HI)`) under which no Medium-integrity process may read or write, whatever the DACL says.
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

    /// NUL-terminated wide encoding of a path for the `*W` APIs.
    fn to_wide(p: &Path) -> Vec<u16> {
        p.as_os_str().encode_wide().chain(once(0)).collect()
    }

    /// Copy a LocalAlloc'd wide C string into an owned `String`.
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

    /// The current process token's `TokenOwner` blob (the returned buffer owns the
    /// SID the pointer at its head references).
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
        /// It could not be read (it exited, or its token is closed to this user — an elevated same-user process seen from an unelevated one).
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

    /// The `class` blob of `process`'s token (the buffer owns any SID the
    /// pointer at its head references).
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
        // SAFETY: `buf` has `len` bytes; TokenOwner/TokenUser write their struct with the SID pointer at its head, plus the trailing SID.
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

    /// The `PSID` at the head of a `TokenOwner`/`TokenUser` blob
    /// (`TOKEN_OWNER.Owner` / `TOKEN_USER.User.Sid`, the first pointer-sized
    /// field).
    fn sid_ptr(buf: &[u8]) -> *mut c_void {
        if buf.len() < std::mem::size_of::<*mut c_void>() {
            return null_mut();
        }
        // SAFETY: `buf` is at least pointer-sized; TOKEN_USER begins with the PSID.
        unsafe { *(buf.as_ptr().cast::<*mut c_void>()) }
    }

    /// Refuse `dir` unless the current user owns it, then rewrite its DACL to a protected owner-only grant.
    pub(super) fn verify_owner_and_harden(dir: &Path) -> io::Result<()> {
        let owner_buf = current_token_owner_info()
            .ok_or_else(|| io::Error::other("cannot query process token owner SID"))?;
        let self_owner_sid = sid_ptr(&owner_buf);
        if self_owner_sid.is_null() {
            return Err(io::Error::other("process token has no owner SID"));
        }
        verify_owner_and_harden_with(dir, self_owner_sid, &Descriptor::owner_of_path)
    }

    /// [`verify_owner_and_harden`] with its two facts injected: `token_owner`, the SID this process's objects are owned by (`TokenOwner`), and `read_owner`, which reads the directory's owner from its NUL-terminated path ([`Descriptor::owner_of_path`]).
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

    /// The owner check itself: a directory owned by `dir_owner` passes only when that SID is `token_owner`.
    pub(super) fn owner_verdict(
        dir_owner: *mut c_void,
        token_owner: *mut c_void,
    ) -> io::Result<()> {
        // SAFETY: both are null or valid SIDs inside descriptors or token blobs the caller keeps alive for this call.
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

    /// Replace `dir`'s DACL with a protected (non-inheriting) full-access grant to the owning
    /// principal, LocalSystem and Builtin Administrators: the Unix `0700` analogue, whatever
    /// ACL the directory inherited.
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
        // SAFETY: `wide` is a NUL-terminated path; `dacl` points into `psd`, live until the LocalFree below; we set only the (protected) DACL.
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

    /// A security descriptor, built from SDDL or read from a path, freed on drop (both calls `LocalAlloc` it).
    pub(super) struct Descriptor(*mut c_void);

    impl Descriptor {
        /// `sddl` as a security descriptor (`ConvertStringSecurityDescriptorToSecurityDescriptorW`).
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

        /// The owner part of the descriptor of the file or directory at `wide` (a NUL-terminated path), read by name (`GetNamedSecurityInfoW`).
        pub(super) fn owner_of_path(wide: &[u16]) -> io::Result<Self> {
            let mut owner: *mut c_void = null_mut();
            let mut psd: *mut c_void = null_mut();
            // SAFETY: `wide` is a NUL-terminated path; out-pointers we don't request (group/DACL/SACL) are null, matching the single OWNER info bit.
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

        /// Its DACL, pointing into it.
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

        /// Its SACL (here: the mandatory label), pointing into it.
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
            // SAFETY: the convert call or GetNamedSecurityInfoW LocalAlloc'd the descriptor; freed once.
            unsafe { LocalFree(self.0) };
        }
    }

    /// The elevated control directories this process holds open (the module
    /// docs), without `FILE_SHARE_DELETE`: neither they nor any directory above
    /// them can be renamed or removed while it runs.
    static PINNED: Mutex<Vec<(PathBuf, File)>> = Mutex::new(Vec::new());

    /// Give `dir` the elevated posture ([`ELEVATED_DIR_SDDL`]), creating it with that descriptor when absent (its parents are created as ordinary directories).
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

    /// Create `path` for writing (`CREATE_NEW`: never through a planted link), shared with no one while it is open, and give it [`ELEVATED_FILE_SDDL`] before a byte is written, so no other handle can exist on it before that descriptor is in force.
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

    /// `CreateDirectoryW` with `sd`. A directory already there is not an error:
    /// the caller verifies what it is.
    fn create_dir_with(dir: &Path, sd: &Descriptor) -> io::Result<()> {
        let wide = to_wide(dir);
        let mut attributes = SecurityAttributes {
            nLength: std::mem::size_of::<SecurityAttributes>() as u32,
            lpSecurityDescriptor: sd.as_ptr(),
            bInheritHandle: 0,
        };
        // SAFETY: `wide` is a NUL-terminated path; `attributes` and the descriptor it names outlive the call.
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

    /// `dir` itself, never a junction's or symlink's target, opened to read and rewrite its security and shared for read and write but not delete.
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

    /// Refuse `handle`'s directory unless Administrators or SYSTEM owns it.
    fn verify_elevated_owner(handle: &File, dir: &Path) -> io::Result<()> {
        let mut owner: *mut c_void = null_mut();
        let mut psd: *mut c_void = null_mut();
        // SAFETY: `handle` is a live handle opened with READ_CONTROL; only the owner is asked for, the other out-pointers are null.
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

    /// The elevated owner check itself: `owner` (the directory's) must be Administrators or SYSTEM, which only an elevated process or SYSTEM can have made it.
    pub(super) fn elevated_owner_verdict(owner: *mut c_void, dir: &Path) -> io::Result<()> {
        // SAFETY: `owner` is null or a valid SID inside a descriptor the caller keeps alive for this call.
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

    /// Write the parts of `sd` that `info` names onto the object `handle` is
    /// open on (`SetSecurityInfo`).
    fn apply(handle: &File, sd: &Descriptor, info: u32) -> io::Result<()> {
        let owner = if info & OWNER_SECURITY_INFORMATION != 0 {
            sd.owner()?
        } else {
            null_mut()
        };
        let (dacl, sacl) = (sd.dacl()?, sd.sacl()?);
        // SAFETY: `handle` is live with WRITE_DAC | WRITE_OWNER; the SID and the ACLs point into `sd`, which outlives the call.
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
mod tests {
    use super::*;

    /// An owned directory verifies and hardens, and the owner keeps full access.
    #[test]
    fn ensure_private_dir_accepts_and_hardens_owned_dir() {
        let dir = std::env::temp_dir().join(format!("aterm-win-acl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).expect("current-user-owned dir must verify + harden");
        assert!(dir.is_dir());
        ensure_private_dir(&dir).expect("second pass over a hardened dir is idempotent");
        std::fs::write(dir.join("probe"), b"x").expect("owner can still write inside");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The elevation law on the descriptors themselves (`AccessCheck`): an
    /// elevated token of this user gets everything on the elevated directory and
    /// token file; the same user's UNELEVATED token — Administrators deny-only,
    /// Medium integrity, built here whatever this process is — gets nothing, not
    /// even the right to read the descriptor.
    #[test]
    fn the_elevated_descriptors_admit_an_elevated_token_and_refuse_the_unelevated_twin() {
        let elevated = aterm_uds::elevation::is_elevated();
        let twin = tokens::elevated_twin();
        if twin.is_none() {
            eprintln!("this account has no elevated token; the elevated side is not checked");
        }
        let me = tokens::own_user_sid();
        assert_eq!(
            tokens::granted(
                &format!("O:{me}D:P(A;OICI;FA;;;{me})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"),
                &tokens::unelevated_twin()
            ),
            tokens::FILE_ALL_ACCESS,
            "control: the shared directory's posture admits the same user"
        );
        for sddl in [acl::ELEVATED_DIR_SDDL, acl::ELEVATED_FILE_SDDL] {
            assert_eq!(
                tokens::granted(sddl, &tokens::unelevated_twin()),
                0,
                "{sddl}: the same user, unelevated"
            );
            assert_eq!(
                tokens::granted(sddl, &tokens::own()),
                if elevated { tokens::FILE_ALL_ACCESS } else { 0 },
                "{sddl}: this process (elevated: {elevated})"
            );
            if let Some(twin) = &twin {
                assert_eq!(
                    tokens::granted(sddl, twin),
                    tokens::FILE_ALL_ACCESS,
                    "{sddl}: the same user, elevated"
                );
            }
        }
    }

    /// Each layer holds alone. The High label refuses a Medium token reads and
    /// writes even where the DACL grants Everyone everything; and the OWNER
    /// RIGHTS entry leaves an owner that is the user (what an elevated token
    /// under the "object creator" owner policy makes) only `READ_CONTROL`,
    /// where without it the owner's implicit `WRITE_DAC` would let the
    /// unelevated user rewrite the DACL.
    #[test]
    fn the_label_and_the_owner_rights_entry_each_refuse_the_unelevated_twin() {
        let twin = tokens::unelevated_twin();
        let data = tokens::FILE_READ_DATA | tokens::FILE_WRITE_DATA;
        assert_eq!(
            tokens::granted("O:BAD:(A;;FA;;;WD)", &twin),
            tokens::FILE_ALL_ACCESS,
            "control: the DACL alone admits the twin"
        );
        assert_eq!(
            tokens::granted("O:BAD:(A;;FA;;;WD)S:(ML;;NRNW;;;HI)", &twin) & data,
            0,
            "the High label refuses a Medium reader and writer"
        );
        let me = tokens::own_user_sid();
        let (sy, ba) = ("(A;;FA;;;SY)", "(A;;FA;;;BA)");
        assert_eq!(
            tokens::granted(&format!("O:{me}D:P{sy}{ba}"), &twin),
            tokens::READ_CONTROL | tokens::WRITE_DAC,
            "control: an owner's implicit rights"
        );
        assert_eq!(
            tokens::granted(&format!("O:{me}D:P{sy}{ba}(A;;RC;;;OW)"), &twin),
            tokens::READ_CONTROL,
            "OWNER RIGHTS holds the owner to READ_CONTROL"
        );
    }

    /// The kernel gate under the token: afunix checks the socket FILE's DACL at
    /// `connect`.
    #[test]
    fn a_socket_under_the_elevated_entries_refuses_an_unelevated_connect() {
        let dir = std::env::temp_dir().join(format!("aterm-elev-sock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let entries = acl::ELEVATED_DIR_SDDL
            .split_once("D:P")
            .and_then(|(_, rest)| rest.split_once("S:"))
            .map(|(dacl, _)| dacl.replace(";OICI;", ";OICIIO;"))
            .expect("the DACL part");
        let me = tokens::own_user_sid();
        tokens::set_dacl(&dir, &format!("D:P(A;;FA;;;{me}){entries}"));
        let sock = dir.join("t.sock");
        let listener = aterm_uds::CtlListener::bind(&sock).expect("bind");
        let dialed = aterm_uds::CtlStream::connect(&sock);
        if aterm_uds::elevation::is_elevated() {
            dialed.expect("Administrators connect");
        } else {
            let e = dialed.expect_err("an unelevated process must not connect");
            assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied, "{e}");
        }
        drop(listener);
        let _ = std::fs::remove_file(&sock);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fail closed: an unelevated process cannot make the elevated directory or
    /// token file at all (it cannot name Administrators the owner, nor label
    /// High), and nothing is left behind; and a directory it made where an
    /// elevated instance looks is refused untouched, never adopted.
    #[test]
    fn an_elevated_dir_is_never_made_or_adopted_without_elevation() {
        let elevated = aterm_uds::elevation::is_elevated();
        let base = std::env::temp_dir().join(format!("aterm-elev-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir(&base).unwrap();

        let fresh = base.join("fresh");
        let made = acl::ensure_elevated_dir(&fresh, false);
        assert_eq!(made.is_ok(), elevated, "{made:?}");
        assert_eq!(fresh.exists(), elevated, "nothing is left behind");

        let planted = base.join("planted");
        std::fs::create_dir(&planted).unwrap();
        let adopted = acl::ensure_elevated_dir(&planted, false);
        assert_eq!(adopted.is_ok(), elevated, "{adopted:?}");
        if !elevated {
            assert_eq!(
                adopted.unwrap_err().kind(),
                std::io::ErrorKind::PermissionDenied
            );
            std::fs::write(planted.join("probe"), b"x").expect("the refusal touched nothing");
        }

        let token = base.join("t.token");
        let created = acl::create_elevated_file(&token);
        assert_eq!(created.is_ok(), elevated, "{:?}", created.as_ref().err());
        drop(created);
        assert_eq!(token.exists(), elevated, "nothing is left behind");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A fresh scratch directory under the temp folder, named for this process.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).expect("scratch dir");
        dir
    }

    /// The owner check across an elevation (`docs/WINDOWS_PARAGON_DESIGN.md` §5.7
    /// and §8), on the real security API and without one: descriptors built from
    /// SDDL stand in for the folder's owner through the injected owner read
    /// (`acl::verify_owner_and_harden_with`).
    #[test]
    fn the_owner_check_refuses_a_folder_across_elevations() {
        let me = tokens::own_user_sid();
        let me_sid = tokens::Sid::parse(&me);
        if !aterm_uds::elevation::is_elevated() {
            assert_eq!(
                tokens::token_owner_sid(&tokens::own()),
                me,
                "an unelevated token's objects are the user's"
            );
        }
        let elevated_owner = tokens::elevated_twin()
            .map(|twin| tokens::token_owner_sid(&twin))
            .unwrap_or_else(|| {
                eprintln!("this account has no elevated token; its owner is taken as BA");
                "S-1-5-32-544".to_owned()
            });
        let elevated_sid = tokens::Sid::parse(&elevated_owner);
        assert_ne!(
            elevated_owner, me,
            "an elevated token owns what it makes as Administrators, not as the user"
        );

        let dir = scratch("aterm-owner-x");
        tokens::set_dacl(
            &dir,
            &format!("D:P(A;OICI;FA;;;{me})(A;OICI;0x1200a9;;;WD)"),
        );
        let before = tokens::sddl_of(&dir);
        let owned_by = |sddl_owner: &str| {
            let sddl = format!("O:{sddl_owner}");
            move |_: &[u16]| acl::Descriptor::from_sddl(&sddl)
        };
        let refused = |token_owner: &tokens::Sid, dir_owner: &str| {
            let read = owned_by(dir_owner);
            let Err(err) = acl::verify_owner_and_harden_with(&dir, token_owner.ptr(), &read) else {
                panic!("a folder owned by {dir_owner} passed");
            };
            assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
            assert_eq!(tokens::sddl_of(&dir), before, "the refusal wrote nothing");
            err.to_string()
        };
        let unelevated_meets_elevated = refused(&me_sid, "BA");
        assert!(
            unelevated_meets_elevated.contains("Administrators"),
            "{unelevated_meets_elevated}"
        );
        let elevated_meets_unelevated = refused(&elevated_sid, &me);
        assert!(
            elevated_meets_unelevated.contains("not owned by the current user"),
            "{elevated_meets_unelevated}"
        );
        for stranger in ["SY", "WD", "S-1-5-21-1-2-3-1001"] {
            refused(&me_sid, stranger);
        }
        let ba = acl::Descriptor::from_sddl("O:BA").expect("O:BA");
        acl::owner_verdict(ba.owner().expect("owner"), elevated_sid.ptr())
            .expect("an elevated aterm keeps a folder it made");
        acl::verify_owner_and_harden_with(&dir, me_sid.ptr(), &acl::Descriptor::owner_of_path)
            .expect("the user's own folder");
        assert_ne!(
            tokens::sddl_of(&dir),
            before,
            "the pass hardened the folder"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The protected DACL, read back from a real directory. A child of a folder
    /// whose inheritable entry admits Everyone inherits that entry (control:
    /// `AccessCheck` lets a stranger list and write it); after
    /// `ensure_private_dir` its DACL is protected and holds exactly the user,
    /// SYSTEM and Administrators, so the stranger may neither list it, write it
    /// nor rewrite its DACL, and the user keeps everything.
    #[test]
    fn the_hardened_dacl_is_protected_and_shuts_everyone_else_out() {
        let me = tokens::own_user_sid();
        let base = scratch("aterm-dacl");
        tokens::set_dacl(&base, &format!("D:P(A;OICI;FA;;;{me})(A;OICI;FA;;;WD)"));
        let child = base.join("child");
        std::fs::create_dir(&child).expect("child");
        let data = tokens::FILE_READ_DATA | tokens::FILE_WRITE_DATA;
        let inherited = tokens::sddl_of(&child);
        assert!(inherited.contains(";;;WD)"), "{inherited}");
        assert_eq!(
            tokens::granted(&inherited, &tokens::stranger()) & data,
            data,
            "control: the inherited entry admits a stranger"
        );

        ensure_private_dir(&child).expect("the user's own folder");
        let hardened = tokens::sddl_of(&child);
        let (flags, mut aces) = tokens::dacl_parts(&hardened);
        assert!(flags.contains('P'), "protected: {hardened}");
        aces.sort();
        let mut want = vec![
            format!("(A;OICI;FA;;;{me})"),
            "(A;OICI;FA;;;BA)".to_owned(),
            "(A;OICI;FA;;;SY)".to_owned(),
        ];
        want.sort();
        assert_eq!(aces, want, "{hardened}");
        let stranger = tokens::granted(&hardened, &tokens::stranger());
        assert_eq!(stranger & (data | tokens::WRITE_DAC), 0, "{stranger:#x}");
        assert_eq!(
            tokens::granted(&hardened, &tokens::unelevated_twin()),
            tokens::FILE_ALL_ACCESS
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The elevated owner check (`acl::elevated_owner_verdict`) over
    /// descriptors built from SDDL: only Administrators and SYSTEM, the owners
    /// no unelevated process can give a directory, pass.
    #[test]
    fn the_elevated_owner_check_takes_only_administrators_or_system() {
        let me = tokens::own_user_sid();
        let dir = std::path::Path::new(r"C:\scratch\aterm-elevated");
        for (owner, passes) in [
            ("BA", true),
            ("SY", true),
            (me.as_str(), false),
            ("WD", false),
            ("AU", false),
            ("S-1-5-21-1-2-3-1001", false),
        ] {
            let sd = acl::Descriptor::from_sddl(&format!("O:{owner}")).expect("SDDL");
            let verdict = acl::elevated_owner_verdict(sd.owner().expect("owner"), dir);
            assert_eq!(verdict.is_ok(), passes, "{owner}: {verdict:?}");
            if let Err(e) = verdict {
                assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
            }
        }
    }

    /// The pin: a directory held open the way an elevated control directory is
    /// cannot be renamed, nor can any directory above it, so the directory
    /// whose owner was verified is the one its path names for as long as the
    /// handle lives; once it is closed the rename goes through.
    #[test]
    fn a_held_directory_cannot_be_renamed_nor_can_its_parents() {
        let base = std::env::temp_dir().join(format!("aterm-elev-pin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let held = base.join("a").join("b");
        std::fs::create_dir_all(&held).unwrap();
        let handle = acl::open_dir_nofollow(&held).expect("open the directory itself");
        assert!(std::fs::rename(&held, base.join("a").join("moved")).is_err());
        assert!(std::fs::rename(base.join("a"), base.join("moved")).is_err());
        drop(handle);
        std::fs::rename(base.join("a"), base.join("moved")).expect("free once closed");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Tokens to ask `AccessCheck` with: this process's own, its unelevated
    /// twin, and its elevated twin where the account has one.
    mod tokens {
        #![allow(non_snake_case, reason = "Win32 API names are camel-case by contract")]

        use std::ffi::c_void;
        use std::iter::once;
        use std::os::windows::ffi::OsStrExt as _;
        use std::path::Path;
        use std::ptr::{null, null_mut};

        pub(super) const FILE_ALL_ACCESS: u32 = 0x001F_01FF;
        pub(super) const FILE_READ_DATA: u32 = 0x0001;
        pub(super) const FILE_WRITE_DATA: u32 = 0x0002;
        pub(super) const READ_CONTROL: u32 = 0x0002_0000;
        pub(super) const WRITE_DAC: u32 = 0x0004_0000;
        const MAXIMUM_ALLOWED: u32 = 0x0200_0000;
        const TOKEN_ASSIGN_PRIMARY: u32 = 0x0001;
        const TOKEN_DUPLICATE: u32 = 0x0002;
        const TOKEN_QUERY: u32 = 0x0008;
        const TOKEN_ADJUST_DEFAULT: u32 = 0x0080;
        const SECURITY_IDENTIFICATION: u32 = 1;
        const TOKEN_USER_CLASS: u32 = 1;
        const TOKEN_OWNER_CLASS: u32 = 4;
        const OWNER_SECURITY_INFORMATION: u32 = 0x0000_0001;
        const TOKEN_LINKED_TOKEN_CLASS: u32 = 19;
        const TOKEN_ELEVATION_CLASS: u32 = 20;
        const TOKEN_INTEGRITY_LEVEL_CLASS: u32 = 25;
        const SE_GROUP_INTEGRITY: u32 = 0x0000_0020;
        const SE_FILE_OBJECT: u32 = 1;
        const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
        const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;

        #[repr(C)]
        struct SidAndAttributes {
            Sid: *mut c_void,
            Attributes: u32,
        }

        #[repr(C)]
        struct GenericMapping {
            GenericRead: u32,
            GenericWrite: u32,
            GenericExecute: u32,
            GenericAll: u32,
        }

        #[link(name = "advapi32")]
        unsafe extern "system" {
            fn OpenProcessToken(
                ProcessHandle: isize,
                DesiredAccess: u32,
                TokenHandle: *mut isize,
            ) -> i32;
            fn GetTokenInformation(
                TokenHandle: isize,
                TokenInformationClass: u32,
                TokenInformation: *mut c_void,
                TokenInformationLength: u32,
                ReturnLength: *mut u32,
            ) -> i32;
            fn SetTokenInformation(
                TokenHandle: isize,
                TokenInformationClass: u32,
                TokenInformation: *const c_void,
                TokenInformationLength: u32,
            ) -> i32;
            fn DuplicateToken(
                ExistingTokenHandle: isize,
                ImpersonationLevel: u32,
                DuplicateTokenHandle: *mut isize,
            ) -> i32;
            fn CreateRestrictedToken(
                ExistingTokenHandle: isize,
                Flags: u32,
                DisableSidCount: u32,
                SidsToDisable: *const SidAndAttributes,
                DeletePrivilegeCount: u32,
                PrivilegesToDelete: *const c_void,
                RestrictedSidCount: u32,
                SidsToRestrict: *const SidAndAttributes,
                NewTokenHandle: *mut isize,
            ) -> i32;
            fn AccessCheck(
                pSecurityDescriptor: *mut c_void,
                ClientToken: isize,
                DesiredAccess: u32,
                GenericMapping: *const GenericMapping,
                PrivilegeSet: *mut c_void,
                PrivilegeSetLength: *mut u32,
                GrantedAccess: *mut u32,
                AccessStatus: *mut i32,
            ) -> i32;
            fn ConvertStringSidToSidW(StringSid: *const u16, Sid: *mut *mut c_void) -> i32;
            fn ConvertSidToStringSidW(Sid: *mut c_void, StringSid: *mut *mut u16) -> i32;
            fn GetLengthSid(pSid: *mut c_void) -> u32;
            fn SetNamedSecurityInfoW(
                pObjectName: *mut u16,
                ObjectType: u32,
                SecurityInfo: u32,
                psidOwner: *mut c_void,
                psidGroup: *mut c_void,
                pDacl: *mut c_void,
                pSacl: *mut c_void,
            ) -> u32;
            fn GetSecurityDescriptorDacl(
                pSecurityDescriptor: *mut c_void,
                lpbDaclPresent: *mut i32,
                pDacl: *mut *mut c_void,
                lpbDaclDefaulted: *mut i32,
            ) -> i32;
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
            fn ConvertSecurityDescriptorToStringSecurityDescriptorW(
                SecurityDescriptor: *mut c_void,
                RequestedStringSDRevision: u32,
                SecurityInformation: u32,
                StringSecurityDescriptor: *mut *mut u16,
                StringSecurityDescriptorLen: *mut u32,
            ) -> i32;
        }

        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetCurrentProcess() -> isize;
            fn CloseHandle(hObject: isize) -> i32;
            fn LocalFree(hMem: *mut c_void) -> *mut c_void;
        }

        /// A token handle, closed on drop.
        pub(super) struct Token(isize);

        impl Drop for Token {
            fn drop(&mut self) {
                // SAFETY: a live token handle this module opened; closed once.
                unsafe { CloseHandle(self.0) };
            }
        }

        /// A SID parsed from its string form, freed on drop.
        pub(super) struct Sid(*mut c_void);

        impl Sid {
            pub(super) fn parse(text: &str) -> Self {
                let wide: Vec<u16> = text.encode_utf16().chain(once(0)).collect();
                let mut sid: *mut c_void = null_mut();
                // SAFETY: `wide` is NUL-terminated; we own `sid` on success.
                let ok = unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut sid) };
                assert!(ok != 0 && !sid.is_null(), "SID {text}");
                Self(sid)
            }

            /// The SID itself, live while `self` is.
            pub(super) fn ptr(&self) -> *mut c_void {
                self.0
            }
        }

        impl Drop for Sid {
            fn drop(&mut self) {
                // SAFETY: ConvertStringSidToSidW LocalAlloc'd it; freed once.
                unsafe { LocalFree(self.0) };
            }
        }

        fn process_token(access: u32) -> Token {
            let mut handle: isize = 0;
            // SAFETY: the pseudo-handle is always valid; the out pointer is a local.
            let ok = unsafe { OpenProcessToken(GetCurrentProcess(), access, &mut handle) };
            assert!(
                ok != 0,
                "OpenProcessToken: {}",
                std::io::Error::last_os_error()
            );
            Token(handle)
        }

        /// An identification-level impersonation copy of `token`, which is what
        /// `AccessCheck` takes.
        fn for_access_check(token: &Token) -> Token {
            let mut handle: isize = 0;
            // SAFETY: `token` is live with TOKEN_DUPLICATE; the out pointer is a local.
            let ok = unsafe { DuplicateToken(token.0, SECURITY_IDENTIFICATION, &mut handle) };
            assert!(
                ok != 0,
                "DuplicateToken: {}",
                std::io::Error::last_os_error()
            );
            Token(handle)
        }

        /// One `DWORD` of `token`'s information `class`.
        fn token_u32(token: &Token, class: u32) -> Option<u32> {
            let (mut value, mut len): (u32, u32) = (0, 0);
            // SAFETY: the buffer is one live `u32` and `len` a local.
            let ok = unsafe {
                GetTokenInformation(token.0, class, (&raw mut value).cast(), 4, &mut len)
            };
            (ok != 0).then_some(value)
        }

        /// This process's token.
        pub(super) fn own() -> Token {
            for_access_check(&process_token(TOKEN_DUPLICATE | TOKEN_QUERY))
        }

        /// This user's token as an UNELEVATED process of theirs holds it,
        /// whatever this process is: Administrators and "local account and
        /// member of Administrators" deny-only, Medium integrity.
        pub(super) fn unelevated_twin() -> Token {
            restricted(&["S-1-5-32-544", "S-1-5-114"])
        }

        /// A token of SOMEONE ELSE on this machine: the unelevated twin with the
        /// user's own SID deny-only too, so only the groups every local user
        /// holds (Everyone, Users, Authenticated Users, INTERACTIVE…) remain.
        pub(super) fn stranger() -> Token {
            let me = own_user_sid();
            restricted(&[me.as_str(), "S-1-5-32-544", "S-1-5-114"])
        }

        /// This process's token with each SID in `deny_only` made deny-only, at
        /// Medium integrity.
        fn restricted(deny_only: &[&str]) -> Token {
            let base = process_token(
                TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT,
            );
            let sids: Vec<Sid> = deny_only.iter().map(|text| Sid::parse(text)).collect();
            let disable: Vec<SidAndAttributes> = sids
                .iter()
                .map(|sid| SidAndAttributes {
                    Sid: sid.0,
                    Attributes: 0,
                })
                .collect();
            let mut handle: isize = 0;
            // SAFETY: `base` is live; `disable` holds live SIDs (owned by `sids`); no privileges or restricting SIDs; the out pointer is a local.
            let ok = unsafe {
                CreateRestrictedToken(
                    base.0,
                    0,
                    u32::try_from(disable.len()).expect("a few SIDs"),
                    disable.as_ptr(),
                    0,
                    null(),
                    0,
                    null(),
                    &mut handle,
                )
            };
            assert!(
                ok != 0,
                "CreateRestrictedToken: {}",
                std::io::Error::last_os_error()
            );
            let restricted = Token(handle);
            let medium = Sid::parse("S-1-16-8192");
            let label = SidAndAttributes {
                Sid: medium.0,
                Attributes: SE_GROUP_INTEGRITY,
            };
            // SAFETY: `label` is a live TOKEN_MANDATORY_LABEL whose SID lives in `medium`; lowering a token's integrity needs TOKEN_ADJUST_DEFAULT only.
            let ok = unsafe {
                SetTokenInformation(
                    restricted.0,
                    TOKEN_INTEGRITY_LEVEL_CLASS,
                    (&raw const label).cast(),
                    std::mem::size_of::<SidAndAttributes>() as u32 + GetLengthSid(medium.0),
                )
            };
            assert!(
                ok != 0,
                "SetTokenInformation: {}",
                std::io::Error::last_os_error()
            );
            for_access_check(&restricted)
        }

        /// This user's ELEVATED token: this process's own when it runs elevated,
        /// else its linked token where the account has one (an administrator
        /// under UAC; identification level, all `AccessCheck` needs).
        pub(super) fn elevated_twin() -> Option<Token> {
            if aterm_uds::elevation::is_elevated() {
                return Some(own());
            }
            let base = process_token(TOKEN_QUERY);
            let (mut linked, mut len): (isize, u32) = (0, 0);
            // SAFETY: `base` is live with TOKEN_QUERY; TOKEN_LINKED_TOKEN is one handle, written into the live local.
            let ok = unsafe {
                GetTokenInformation(
                    base.0,
                    TOKEN_LINKED_TOKEN_CLASS,
                    (&raw mut linked).cast(),
                    std::mem::size_of::<isize>() as u32,
                    &mut len,
                )
            };
            if ok == 0 || linked == 0 {
                return None;
            }
            let linked = Token(linked);
            (token_u32(&linked, TOKEN_ELEVATION_CLASS)? != 0).then_some(linked)
        }

        /// What `token` is granted (`MAXIMUM_ALLOWED`; 0 when nothing) on an
        /// object with descriptor `sddl`, with a group added, which
        /// `AccessCheck` requires and which grants nothing.
        pub(super) fn granted(sddl: &str, token: &Token) -> u32 {
            let at = sddl.find("D:").expect("a DACL");
            let sddl = format!("{}G:SY{}", &sddl[..at], &sddl[at..]);
            let sd = super::super::acl::Descriptor::from_sddl(&sddl).expect("the SDDL parses");
            let mapping = GenericMapping {
                GenericRead: 0x0012_0089,
                GenericWrite: 0x0012_0116,
                GenericExecute: 0x0012_00A0,
                GenericAll: FILE_ALL_ACCESS,
            };
            let mut privileges = [0u32; 64];
            let mut privileges_len = std::mem::size_of_val(&privileges) as u32;
            let (mut granted, mut status): (u32, i32) = (0, 0);
            // SAFETY: `sd` is a live descriptor and `token` a live impersonation token; every out pointer is a live local of the size given.
            let ok = unsafe {
                AccessCheck(
                    sd.as_ptr(),
                    token.0,
                    MAXIMUM_ALLOWED,
                    &mapping,
                    privileges.as_mut_ptr().cast(),
                    &mut privileges_len,
                    &mut granted,
                    &mut status,
                )
            };
            assert!(
                ok != 0,
                "AccessCheck {sddl}: {}",
                std::io::Error::last_os_error()
            );
            if status != 0 { granted } else { 0 }
        }

        /// This process's user SID, as text.
        pub(super) fn own_user_sid() -> String {
            token_sid(&process_token(TOKEN_QUERY), TOKEN_USER_CLASS)
        }

        /// The SID `token`'s objects are owned by (`TokenOwner`), as text: the
        /// user for an unelevated token, Administrators for an elevated one under
        /// Windows' default owner policy.
        pub(super) fn token_owner_sid(token: &Token) -> String {
            token_sid(token, TOKEN_OWNER_CLASS)
        }

        /// The SID at the head of `token`'s `class` blob (`TOKEN_USER`,
        /// `TOKEN_OWNER`), as text.
        fn token_sid(token: &Token, class: u32) -> String {
            let mut len: u32 = 0;
            // SAFETY: a null buffer of length 0 asks for the length.
            unsafe { GetTokenInformation(token.0, class, null_mut(), 0, &mut len) };
            let mut buf = vec![0usize; (len as usize).div_ceil(std::mem::size_of::<usize>())];
            // SAFETY: `buf` holds at least `len` bytes, pointer-aligned.
            let ok = unsafe {
                GetTokenInformation(token.0, class, buf.as_mut_ptr().cast(), len, &mut len)
            };
            assert!(
                ok != 0,
                "token class {class}: {}",
                std::io::Error::last_os_error()
            );
            sid_text(buf[0] as *mut c_void)
        }

        /// `sid` as text (`S-1-5-…`).
        fn sid_text(sid: *mut c_void) -> String {
            let mut text: *mut u16 = null_mut();
            // SAFETY: `sid` is a live SID; we own `text` on success.
            let ok = unsafe { ConvertSidToStringSidW(sid, &mut text) };
            assert!(ok != 0 && !text.is_null(), "ConvertSidToStringSidW");
            take_local_wide(text)
        }

        /// A LocalAlloc'd NUL-terminated wide string as a `String`, freed.
        fn take_local_wide(text: *mut u16) -> String {
            let mut n = 0;
            // SAFETY: `text` is NUL-terminated.
            while unsafe { *text.add(n) } != 0 {
                n += 1;
            }
            // SAFETY: `n` u16s precede the NUL in the same allocation.
            let out = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, n) });
            // SAFETY: the caller's API LocalAlloc'd it; freed once.
            unsafe { LocalFree(text.cast()) };
            out
        }

        /// The owner and DACL of the file or directory at `path`, as SDDL, read
        /// back through the same API that wrote them.
        pub(super) fn sddl_of(path: &Path) -> String {
            let wide: Vec<u16> = path.as_os_str().encode_wide().chain(once(0)).collect();
            let mut psd: *mut c_void = null_mut();
            let info = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
            // SAFETY: `wide` is a NUL-terminated path; only the descriptor is asked for, and we own it on success.
            let rc = unsafe {
                GetNamedSecurityInfoW(
                    wide.as_ptr(),
                    SE_FILE_OBJECT,
                    info,
                    null_mut(),
                    null_mut(),
                    null_mut(),
                    null_mut(),
                    &mut psd,
                )
            };
            assert_eq!(rc, 0, "GetNamedSecurityInfoW {}", path.display());
            let mut text: *mut u16 = null_mut();
            // SAFETY: `psd` is the live descriptor just read; we own `text` on success.
            let ok = unsafe {
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    psd,
                    1,
                    info,
                    &mut text,
                    null_mut(),
                )
            };
            // SAFETY: GetNamedSecurityInfoW LocalAlloc'd it; freed once.
            unsafe { LocalFree(psd) };
            assert!(ok != 0 && !text.is_null(), "to SDDL");
            take_local_wide(text)
        }

        /// The DACL part of `sddl`: its control flags (`P` for protected, `AI`…)
        /// and its entries, each `(…)`.
        pub(super) fn dacl_parts(sddl: &str) -> (String, Vec<String>) {
            let dacl = &sddl[sddl.find("D:").expect("a DACL") + 2..];
            let dacl = dacl.split("S:").next().unwrap_or(dacl);
            let open = dacl.find('(').unwrap_or(dacl.len());
            let aces = dacl[open..]
                .split_inclusive(')')
                .map(str::to_owned)
                .collect();
            (dacl[..open].to_owned(), aces)
        }

        /// Replace `dir`'s DACL with `sddl`'s, protected.
        pub(super) fn set_dacl(dir: &Path, sddl: &str) {
            let sd = super::super::acl::Descriptor::from_sddl(sddl).expect("the SDDL parses");
            let (mut present, mut defaulted): (i32, i32) = (0, 0);
            let mut dacl: *mut c_void = null_mut();
            // SAFETY: `sd` is a live descriptor; out-params are locals.
            let ok = unsafe {
                GetSecurityDescriptorDacl(sd.as_ptr(), &mut present, &mut dacl, &mut defaulted)
            };
            assert!(ok != 0, "GetSecurityDescriptorDacl");
            let mut wide: Vec<u16> = dir.as_os_str().encode_wide().chain(once(0)).collect();
            // SAFETY: `wide` is a NUL-terminated path; `dacl` points into `sd`.
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
            assert_eq!(rc, 0, "SetNamedSecurityInfoW {}", dir.display());
        }
    }
}
