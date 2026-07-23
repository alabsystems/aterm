// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows implementations behind [`control_auth`](super), delegating the
//! platform work to `aterm_uds::{latest, process, rand}`.
//!
//! HONEST POSTURE (never overstated; disclosed by the one-line startup notice
//! `control::spawn` prints): the isolation boundary is an OWNER-verified,
//! owner-only DACL on the control directory — [`ensure_private_dir`] refuses a
//! directory the current user does NOT own (the Windows twin of the Unix SEC-3
//! owner gate) and rewrites its DACL to a PROTECTED (non-inheriting) grant to
//! the user + SYSTEM + Administrators, so an explicit `ATERM_CONTROL_SOCK` placed
//! in a world-writable location — or a pre-existing loosely-ACL'd `%LOCALAPPDATA%`
//! subtree — cannot leave the socket + token world-readable/-writable. There is
//! still NO peer-uid gate (AF_UNIX on Windows has no `SO_PEERCRED`/`getpeereid`
//! analog); the per-launch capability token remains the MANDATORY, fail-closed
//! gate on every connection, exactly as on Unix. Follow-up hardening (documented,
//! not v1): the undocumented `SIO_AF_UNIX_GETPEERPID` ioctl + token-SID compare,
//! enforced only when it succeeds.

use std::path::Path;

use aterm_uds::CtlStream;

/// Create `dir` (and parents) if absent, then FAIL CLOSED unless the current
/// user owns it, tightening its DACL to an owner-only (user + SYSTEM +
/// Administrators), non-inheriting grant. This is the Windows twin of the Unix
/// `ensure_private_dir` owner check + `0700` chmod: a directory owned by another
/// user (a planted `%LOCALAPPDATA%\aterm`, or an explicit `ATERM_CONTROL_SOCK`
/// under someone else's tree) is REFUSED, and a same-owned but loosely-ACL'd
/// directory is re-protected so the socket + token it will hold are not
/// world-accessible.
pub fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    acl::verify_owner_and_harden(dir)
}

/// Pid liveness via `OpenProcess` (see [`aterm_uds::process::pid_alive`] —
/// access-denied mirrors `EPERM`'s "alive").
pub(crate) fn pid_alive(pid: u32) -> bool {
    aterm_uds::process::pid_alive(pid)
}

/// Atomically (re)point the `latest` alias — a regular POINTER FILE on
/// Windows (contents = the relative instance sock name) — at this instance's
/// socket. Same temp-name + rename publish as the Unix symlink; best-effort.
pub fn publish_latest_link(link: &Path, sock_path: &str) {
    aterm_uds::latest::publish(link, sock_path);
}

/// Generate 32 CSPRNG bytes (`BCryptGenRandom`, system-preferred RNG) as a
/// 64-char lowercase hex string, or `None` when entropy is unavailable (the
/// caller must then refuse to start the socket — fail closed).
#[must_use]
pub fn random_token_hex() -> Option<String> {
    let mut buf = [0u8; 32];
    aterm_uds::rand::fill(&mut buf).ok()?;
    let mut hex = String::with_capacity(64);
    for b in buf {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    Some(hex)
}

/// Provision the capability token: fresh token, written through
/// `create_new(true)` after an unlink-first, and returned as hex. The token
/// rotates every launch — a leaked token from a prior run is worthless.
///
/// `CREATE_NEW` atomically refuses ANY pre-existing object at the path —
/// including a planted symlink/junction — which is the `O_EXCL|O_NOFOLLOW`
/// effect of the Unix twin: the token is only ever written into a file WE
/// just created. No mode bits: the file inherits the private dir's ACL.
///
/// Returns `None` when entropy is unavailable or the file cannot be written;
/// a `None` here MUST make the caller skip binding the socket (fail closed).
#[must_use]
pub fn provision_token(path: &Path) -> Option<String> {
    use std::io::Write;
    let token = random_token_hex()?;
    let _ = std::fs::remove_file(path);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .ok()?;
    f.write_all(token.as_bytes()).ok()?;
    f.flush().ok()?;
    Some(token)
}

/// No-op on Windows (documented): there are no POSIX mode bits to tighten;
/// the socket file inherits the private dir's ACL, and the directory ACL +
/// mandatory token are the in-force gates.
pub fn lock_socket_file(_path: &str) {}

/// The accept-time peer gate. Windows AF_UNIX has NO peer-credential
/// primitive, so this always passes — a "refuse when unverifiable" posture
/// here would refuse EVERY connection. The mandatory per-launch token (plus
/// the directory ACL) is the gate; the reduction is disclosed by the startup
/// notice, per the never-overstate-security rule.
#[allow(
    clippy::unnecessary_wraps,
    reason = "signature shared with the Unix peer-uid gate"
)]
pub fn peer_check(_stream: &CtlStream) -> Result<(), String> {
    Ok(())
}

/// Hand-rolled advapi32/kernel32 FFI for the control-directory owner check + DACL
/// hardening — the Windows analog of the Unix SEC-3 owner gate + `0700` chmod.
/// Kept in one auditable module; the import libraries ship with every Windows SDK,
/// so `#[link]` is clean on stable with zero new dependencies.
mod acl {
    #![allow(non_snake_case, reason = "Win32 API names are camel-case by contract")]

    use std::ffi::c_void;
    use std::io;
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr::null_mut;

    /// Win32 `HANDLE`.
    type Handle = isize;

    const SE_FILE_OBJECT: u32 = 1;
    const OWNER_SECURITY_INFORMATION: u32 = 0x0000_0001;
    const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
    const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;
    const TOKEN_QUERY: u32 = 0x0008;
    /// `TOKEN_INFORMATION_CLASS::TokenOwner` — the SID assigned as OWNER to
    /// objects this process creates. For a standard user this is the user SID; for
    /// an ELEVATED admin it is the Administrators group SID (the default owner of
    /// objects an elevated token creates), so comparing an object's owner against
    /// it correctly accepts directories we could have created in BOTH cases (and
    /// rejects one owned by a different principal) — where `TokenUser` would wrongly
    /// refuse every elevated launch.
    const TOKEN_OWNER_CLASS: u32 = 4;
    const SDDL_REVISION_1: u32 = 1;
    const ERROR_SUCCESS: u32 = 0;

    #[link(name = "advapi32")]
    unsafe extern "system" {
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
        fn GetCurrentProcess() -> Handle;
        fn CloseHandle(hObject: Handle) -> i32;
        fn LocalFree(hMem: *mut c_void) -> *mut c_void;
    }

    /// NUL-terminated wide encoding of a path for the `*W` APIs.
    fn to_wide(p: &Path) -> Vec<u16> {
        p.as_os_str().encode_wide().chain(once(0)).collect()
    }

    /// Copy a LocalAlloc'd wide C string into an owned `String`.
    /// SAFETY: `p` is non-null and points at a NUL-terminated UTF-16 string.
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

    /// The current process token's `TokenOwner` blob (the returned buffer OWNS the
    /// SID the pointer at its head references). `None` on any query failure.
    fn current_token_owner_info() -> Option<Vec<u8>> {
        let mut token: Handle = 0;
        // SAFETY: GetCurrentProcess returns a pseudo-handle; we request TOKEN_QUERY.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return None;
        }
        let mut len: u32 = 0;
        // First call sizes the buffer (expected to "fail" with the needed length).
        // SAFETY: passing a null buffer with length 0 to learn ReturnLength.
        unsafe { GetTokenInformation(token, TOKEN_OWNER_CLASS, null_mut(), 0, &mut len) };
        if len == 0 {
            // SAFETY: `token` is a live handle from OpenProcessToken.
            unsafe { CloseHandle(token) };
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        // SAFETY: `buf` has `len` bytes; TokenOwner writes a TOKEN_OWNER + trailing SID.
        let ok = unsafe {
            GetTokenInformation(
                token,
                TOKEN_OWNER_CLASS,
                buf.as_mut_ptr().cast::<c_void>(),
                len,
                &mut len,
            )
        };
        // SAFETY: `token` is still the live handle from OpenProcessToken.
        unsafe { CloseHandle(token) };
        (ok != 0).then_some(buf)
    }

    /// The `PSID` at the head of a `TokenOwner` blob (`TOKEN_OWNER.Owner`, the
    /// first pointer-sized field). Points INTO `buf`, valid while `buf` lives.
    fn sid_ptr(buf: &[u8]) -> *mut c_void {
        if buf.len() < std::mem::size_of::<*mut c_void>() {
            return null_mut();
        }
        // SAFETY: `buf` is at least pointer-sized; TOKEN_USER begins with the PSID.
        unsafe { *(buf.as_ptr().cast::<*mut c_void>()) }
    }

    /// Verify `dir` is owned by the current user (else refuse — fail closed) and
    /// rewrite its DACL to a protected owner-only grant. Runs on every
    /// `ensure_private_dir`; idempotent on a directory we already hardened.
    pub(super) fn verify_owner_and_harden(dir: &Path) -> io::Result<()> {
        let owner_buf = current_token_owner_info()
            .ok_or_else(|| io::Error::other("cannot query process token owner SID"))?;
        let self_owner_sid = sid_ptr(&owner_buf);
        if self_owner_sid.is_null() {
            return Err(io::Error::other("process token has no owner SID"));
        }

        let mut wide = to_wide(dir);

        // Owner check: the object's owner SID must equal our token owner SID.
        let mut owner: *mut c_void = null_mut();
        let mut psd: *mut c_void = null_mut();
        // SAFETY: `wide` is a NUL-terminated path; out-pointers we don't request
        // (group/DACL/SACL) are null, matching the single OWNER info bit.
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
        // SAFETY: on success `owner` points into the returned security descriptor.
        let owned_by_us = !owner.is_null() && unsafe { EqualSid(owner, self_owner_sid) } != 0;
        if !psd.is_null() {
            // SAFETY: GetNamedSecurityInfoW LocalAlloc'd the descriptor.
            unsafe { LocalFree(psd) };
        }
        if !owned_by_us {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "control directory is not owned by the current user",
            ));
        }

        harden_dacl(&mut wide, self_owner_sid)
    }

    /// Replace `dir`'s DACL with a PROTECTED (non-inheriting) grant of full access
    /// to the owning principal + LocalSystem + Builtin Administrators — the
    /// owner-only posture (analogous to Unix `0700`), so nothing else can read the
    /// token or connect to the socket even if the directory inherited a loose ACL.
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

        // `D:P(...)` — a protected DACL (no parent inheritance); each `(A;OICI;FA;;;SID)`
        // allows FILE_ALL_ACCESS with object+container inheritance. SY = LocalSystem,
        // BA = Builtin Administrators.
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
        // SAFETY: `wide` is a NUL-terminated path; `dacl` points into `psd`, live
        // until the LocalFree below; we set only the (protected) DACL.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A freshly-created, current-user-owned directory passes the owner check and
    /// is hardened without error, and the owner retains full access afterward
    /// (guards against the FFI ever bricking the socket for a legitimate launch).
    #[test]
    fn ensure_private_dir_accepts_and_hardens_owned_dir() {
        let dir = std::env::temp_dir().join(format!("aterm-win-acl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).expect("current-user-owned dir must verify + harden");
        assert!(dir.is_dir());
        // Idempotent: the dir now carries a protected DACL; a second pass still
        // succeeds and we can still create files inside (owner keeps full access).
        ensure_private_dir(&dir).expect("second pass over a hardened dir is idempotent");
        std::fs::write(dir.join("probe"), b"x").expect("owner can still write inside");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
