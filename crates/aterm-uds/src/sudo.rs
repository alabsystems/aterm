// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The kernel facts a `sudo` askpass relay stands on, and a scrubbed
//! [`Secret`] for the password in flight (`docs/DESIGN-sudo-askpass-2026-10-15.md`).
//!
//! Claude Code's Bash tool has no controlling terminal, so `sudo` there asks
//! its askpass helper instead of a tty. aterm answers that request only when
//! the kernel shows the asking process is a child of Apple's `/usr/bin/sudo`.
//! This module reads those facts (macOS only; every other platform refuses),
//! and nothing here ever presents UI or runs a child.
//!
//! Two sides use it. The HELPER (`aterm-ctl`'s `askpass` role) attests its own
//! parent before it will dial aterm; the SERVER (`aterm-gui`'s `askpass` verb)
//! attests the connecting peer's parent before it will raise a sheet. The
//! facts an unprivileged process can read about a setuid-root `sudo` (euid 0,
//! ruid ours), measured 2026-10-15: `proc_pidpath`, `PROC_PIDT_SHORTBSDINFO`
//! (ppid/uid/ruid) and `csops` all work; `PROC_PIDTBSDINFO`, `KERN_PROCARGS2`
//! and sudo's fd table are refused. None of these proves `sudo` is the only
//! reader of the helper's stdout — that residual is stated in the design §4.

/// A password in flight, held off the agent's reach: heap-backed (so a move
/// copies a pointer, not the bytes), scrubbed with `write_volatile` on drop,
/// `Debug`-redacted, and never `Serialize`/`Clone`. Modelled on
/// `atpkg-keys`'s master-secret discipline.
pub struct Secret {
    bytes: Box<[u8]>,
    len: usize,
}

impl Secret {
    /// At most 1023 bytes (what `sudo` reads), the rest dropped. CR, LF and
    /// NUL are refused by the callers before this, never here.
    #[must_use]
    pub fn from_bytes(src: &[u8]) -> Self {
        let len = src.len().min(1023);
        let mut bytes = vec![0u8; 1023].into_boxed_slice();
        bytes[..len].copy_from_slice(&src[..len]);
        Self { bytes, len }
    }

    /// Decode directly into owned scrubbed storage. Malformed, oversized, or
    /// line-cutting values refuse without leaving an ordinary plaintext Vec.
    #[must_use]
    pub fn decode_hex(hex: &[u8]) -> Option<Self> {
        if !hex.len().is_multiple_of(2) || hex.len() > 2046 {
            return None;
        }
        let mut secret = Self::from_bytes(&[]);
        for pair in hex.as_chunks::<2>().0 {
            let byte = (hex_digit(pair[0])? << 4) | hex_digit(pair[1])?;
            if matches!(byte, 0 | b'\r' | b'\n') {
                return None;
            }
            secret.bytes[secret.len] = byte;
            secret.len += 1;
        }
        Some(secret)
    }

    /// Encode the secure field's UTF-16 directly into scrubbed UTF-8 storage.
    /// Refuse invalid surrogates, line-cutting characters, and more than sudo's
    /// 1023 bytes; a password is never silently truncated.
    #[must_use]
    pub fn from_utf16(units: &[u16]) -> Option<Self> {
        let mut secret = Self::from_bytes(&[]);
        for scalar in char::decode_utf16(units.iter().copied()) {
            let scalar = scalar.ok()?;
            if matches!(scalar, '\0' | '\r' | '\n') {
                return None;
            }
            let end = secret.len.checked_add(scalar.len_utf8())?;
            if end > secret.bytes.len() {
                return None;
            }
            scalar.encode_utf8(&mut secret.bytes[secret.len..end]);
            secret.len = end;
        }
        Some(secret)
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        for b in self.bytes.iter_mut() {
            // SAFETY: `b` is a live, aligned, writable byte of the owned box.
            unsafe { std::ptr::write_volatile(b, 0) };
        }
        std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret(<{} bytes redacted>)", self.len)
    }
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Direct storage for AppKit's secure UTF-16 field extraction. Moves copy
/// only its pointer; every allocated unit is scrubbed on all exits.
pub struct SecretUtf16 {
    units: Box<[u16]>,
}

impl SecretUtf16 {
    #[must_use]
    pub fn new(len: usize) -> Option<Self> {
        (len <= 1023).then(|| Self {
            units: vec![0; len].into_boxed_slice(),
        })
    }

    pub fn as_mut_units(&mut self) -> &mut [u16] {
        &mut self.units
    }

    #[must_use]
    pub fn as_units(&self) -> &[u16] {
        &self.units
    }
}

impl Drop for SecretUtf16 {
    fn drop(&mut self) {
        for unit in &mut self.units {
            // SAFETY: each unit is live, aligned, writable owned storage.
            unsafe { std::ptr::write_volatile(unit, 0) };
        }
        std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
    }
}

/// One bounded askpass wire line, including its newline. Its maximum holds
/// the 18-byte success prefix, 2046 hex bytes, and LF. Encoded passwords are
/// secrets too: this buffer never formats them through an ordinary String,
/// and scrubs its entire allocation on every return/error/unwind path.
pub struct WireBuffer {
    bytes: Box<[u8]>,
    len: usize,
}

impl WireBuffer {
    const CAPACITY: usize = 2065;

    fn empty() -> Self {
        Self {
            bytes: vec![0; Self::CAPACITY].into_boxed_slice(),
            len: 0,
        }
    }

    /// Copy a bounded public refusal line into the guarded wire container.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > Self::CAPACITY {
            return None;
        }
        let mut wire = Self::empty();
        wire.bytes[..bytes.len()].copy_from_slice(bytes);
        wire.len = bytes.len();
        Some(wire)
    }

    #[must_use]
    pub fn secret_reply(secret: &Secret) -> Self {
        let mut wire = Self::from_bytes(b"OK askpass secret=").unwrap();
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for &byte in secret.as_bytes() {
            wire.bytes[wire.len] = HEX[usize::from(byte >> 4)];
            wire.bytes[wire.len + 1] = HEX[usize::from(byte & 15)];
            wire.len += 2;
        }
        wire.bytes[wire.len] = b'\n';
        wire.len += 1;
        wire
    }

    /// The one plaintext line delivered to sudo's unbuffered pipe.
    #[must_use]
    pub fn password_line(secret: &Secret) -> Self {
        let mut wire = Self::from_bytes(secret.as_bytes()).unwrap();
        wire.bytes[wire.len] = b'\n';
        wire.len += 1;
        wire
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    /// Read exactly one bounded LF-terminated line directly into guarded
    /// storage. The caller supplies the I/O deadline; no BufReader or ordinary
    /// String/Vec receives the reply. Incomplete lines and bytes following LF
    /// in the same read refuse, rather than guessing a password from a partial response.
    ///
    /// # Errors
    /// I/O failure, missing LF, multiple frames, or an oversized line.
    pub fn read_line(reader: &mut impl std::io::Read) -> std::io::Result<Self> {
        use std::io::{Error, ErrorKind};
        let mut wire = Self::empty();
        loop {
            if wire.len == wire.bytes.len() {
                return Err(Error::new(ErrorKind::InvalidData, "askpass reply too long"));
            }
            let count = match reader.read(&mut wire.bytes[wire.len..]) {
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                result => result?,
            };
            if count == 0 {
                return Err(Error::new(
                    ErrorKind::UnexpectedEof,
                    "incomplete askpass reply",
                ));
            }
            let start = wire.len;
            wire.len += count;
            if let Some(end) = wire.bytes[start..wire.len].iter().position(|&b| b == b'\n') {
                if start + end + 1 != wire.len {
                    return Err(Error::new(
                        ErrorKind::InvalidData,
                        "extra askpass reply data",
                    ));
                }
                return Ok(wire);
            }
        }
    }
}

impl Drop for WireBuffer {
    fn drop(&mut self) {
        for byte in &mut self.bytes {
            // SAFETY: each byte is live, writable owned storage.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
    }
}

impl std::fmt::Debug for WireBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "WireBuffer(<{} bytes redacted>)", self.len)
    }
}

/// Why a `sudo` attestation refused — a short, secret-free reason that is safe
/// to surface to the agent through `sudo`'s stderr or an `ERR askpass` line.
pub type AttestError = String;

// ---------------------------------------------------------------------------
// macOS
// ---------------------------------------------------------------------------

#[cfg(target_vendor = "apple")]
mod imp {
    use std::path::PathBuf;

    use super::AttestError;

    // libproc / libsystem exports, declared here because the first-party `libc`
    // carries none of them and aterm-uds takes no `libc` dependency (the whole
    // transport stays auditable). All are `libSystem`; none takes a `$`-suffix.
    unsafe extern "C" {
        fn getppid() -> i32;
        fn getuid() -> u32;
        fn getpid() -> i32;
        fn proc_pidpath(pid: i32, buffer: *mut core::ffi::c_void, buffersize: u32) -> i32;
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut core::ffi::c_void,
            buffersize: i32,
        ) -> i32;
        /// `int csops(pid_t, unsigned int ops, void *useraddr, size_t usersize)`
        /// — xnu `bsd/sys/codesign.h`; no SDK header declares it.
        fn csops(pid: i32, ops: u32, useraddr: *mut core::ffi::c_void, usersize: usize) -> i32;
    }

    const PROC_PIDT_SHORTBSDINFO: i32 = 13;
    const SHORT_INFO_SIZE: usize = 64;
    const PROC_PIDLISTFDS: i32 = 1;
    const PROX_FDTYPE_PIPE: u32 = 6;
    const PROC_PIDPATHINFO_MAXSIZE: usize = 4096;

    // csops ops and status bits (`bsd/sys/codesign.h`).
    const CS_OPS_STATUS: u32 = 0;
    const CS_OPS_IDENTITY: u32 = 11;
    const CS_VALID: u32 = 0x0000_0001;
    const CS_PLATFORM_BINARY: u32 = 0x0400_0000;

    /// `struct proc_bsdshortinfo` (`sys/proc_info.h`): 64 bytes on both Darwin
    /// ABIs. Read as bytes at measured offsets, so this leaf crate declares no
    /// C struct.
    #[derive(Clone, Copy)]
    pub(super) struct ShortInfo {
        pub ppid: u32,
        pub uid: u32,
        pub ruid: u32,
    }

    pub(super) fn short_info(pid: i32) -> Option<ShortInfo> {
        if pid <= 0 {
            return None;
        }
        let mut buf = [0u8; SHORT_INFO_SIZE];
        // SAFETY: `buf` is SHORT_INFO_SIZE writable bytes, the size passed.
        let n = unsafe {
            proc_pidinfo(
                pid,
                PROC_PIDT_SHORTBSDINFO,
                0,
                buf.as_mut_ptr().cast(),
                SHORT_INFO_SIZE as i32,
            )
        };
        if usize::try_from(n).ok() != Some(SHORT_INFO_SIZE) {
            return None;
        }
        let u32_at = |off: usize| u32::from_ne_bytes(buf[off..off + 4].try_into().unwrap());
        // Offsets: pbsi_pid(0) pbsi_ppid(4) pbsi_pgid(8) pbsi_status(12)
        // pbsi_comm[16](16) pbsi_flags(32) pbsi_uid(36) pbsi_gid(40)
        // pbsi_ruid(44) pbsi_rgid(48) …
        if u32_at(0) != pid as u32 {
            return None;
        }
        Some(ShortInfo {
            ppid: u32_at(4),
            uid: u32_at(36),
            ruid: u32_at(44),
        })
    }

    pub(super) fn exe_path(pid: i32) -> Option<PathBuf> {
        use std::os::unix::ffi::OsStrExt as _;
        if pid <= 0 {
            return None;
        }
        let mut buf = [0u8; PROC_PIDPATHINFO_MAXSIZE];
        // SAFETY: `buf` is MAX writable bytes; libproc writes at most that many
        // and returns the length, or <= 0 on failure.
        let n = unsafe { proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
        let n = usize::try_from(n)
            .ok()
            .filter(|&n| n > 0 && n <= buf.len())?;
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..n])))
    }

    /// `pid`'s code signing is valid AND it is an Apple platform binary.
    fn cs_platform_valid(pid: i32) -> bool {
        let mut status: u32 = 0;
        // SAFETY: `status` is one writable u32, the size passed.
        let r = unsafe {
            csops(
                pid,
                CS_OPS_STATUS,
                (&raw mut status).cast(),
                size_of::<u32>(),
            )
        };
        r == 0 && (status & CS_VALID) != 0 && (status & CS_PLATFORM_BINARY) != 0
    }

    /// Best-effort: `pid`'s code-signing identity contains `needle`. The
    /// `CS_OPS_IDENTITY` buffer carries a short header before the C string, so
    /// this searches the whole buffer for the identity bytes rather than
    /// parsing a header whose layout is not in any SDK. A miss (short buffer,
    /// refusal, identity absent) is `false`, and the caller treats it as a
    /// bonus signal, never as the whole proof.
    fn cs_identity_contains(pid: i32, needle: &[u8]) -> bool {
        let mut buf = [0u8; 256];
        // SAFETY: `buf` is 256 writable bytes, the size passed. csops fills at
        // most that many and leaves the rest as the zeros we set.
        let r = unsafe { csops(pid, CS_OPS_IDENTITY, buf.as_mut_ptr().cast(), buf.len()) };
        if r != 0 {
            return false;
        }
        buf.windows(needle.len()).any(|w| w == needle)
    }

    /// The attested facts of a genuine `sudo` at `pid`: Apple's `/usr/bin/sudo`,
    /// a valid platform binary, euid 0 and real uid ours. Returns the error
    /// reason (never the bytes read) on any miss. The identity check is a bonus
    /// only, so a csops-identity quirk never refuses a real `sudo`.
    pub(super) fn attest_sudo(pid: i32) -> Result<(), AttestError> {
        let info = short_info(pid).ok_or("parent is gone or not readable")?;
        if info.uid != 0 {
            return Err("parent is not running as root".into());
        }
        // SAFETY: a plain syscall with no arguments.
        let our_uid = unsafe { getuid() };
        if info.ruid != our_uid {
            return Err("parent's real uid is not ours".into());
        }
        match exe_path(pid) {
            Some(p) if p == std::path::Path::new("/usr/bin/sudo") => {}
            Some(_) => return Err("parent is not /usr/bin/sudo".into()),
            None => return Err("parent's executable is not readable".into()),
        }
        if !cs_platform_valid(pid) {
            return Err("parent is not a valid Apple platform binary".into());
        }
        // A bonus positive signal; its absence does not refuse (the triad
        // above is the proof).
        let _ = cs_identity_contains(pid, b"com.apple.sudo");
        Ok(())
    }

    pub(super) fn parent_pid() -> i32 {
        // SAFETY: a plain syscall with no arguments.
        unsafe { getppid() }
    }

    /// Whether this process's fd 1 is an ANONYMOUS pipe (`PROX_FDTYPE_PIPE`),
    /// not a regular file or a named FIFO. `sudo` dups a `pipe2` read end onto
    /// the helper's fd 1, so a genuine helper always sees a pipe; a redirect to
    /// a file is refused. A named FIFO reads as a vnode, so it too is refused.
    /// `Err` with a reason on anything else.
    ///
    /// Reads `PROC_PIDLISTFDS` for our own pid and finds fd 1's type. If the
    /// listing cannot be read at all, that is itself a refusal — the design
    /// prefers failing closed here.
    pub(super) fn stdout_is_anon_pipe() -> Result<(), AttestError> {
        // SAFETY: a plain syscall with no arguments.
        let me = unsafe { getpid() };
        // Size the fd listing, then read it (bounded to a sane ceiling).
        // SAFETY: a null buffer and zero size asks proc_pidinfo for the size.
        let need = unsafe { proc_pidinfo(me, PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
        let need = usize::try_from(need).ok().filter(|&n| n > 0 && n < 1 << 20);
        let Some(need) = need else {
            return Err("own fd table is not readable".into());
        };
        // `struct proc_fdinfo { int32_t proc_fd; uint32_t proc_fdtype; }` = 8.
        const FDINFO: usize = 8;
        let mut buf = vec![0u8; need];
        // SAFETY: `buf` is `need` writable bytes, the size passed.
        let got =
            unsafe { proc_pidinfo(me, PROC_PIDLISTFDS, 0, buf.as_mut_ptr().cast(), need as i32) };
        let got = usize::try_from(got)
            .ok()
            .filter(|&n| n > 0 && n <= buf.len())
            .ok_or("own fd table is not readable")?;
        for rec in buf[..got].as_chunks::<FDINFO>().0 {
            let fd = i32::from_ne_bytes(rec[0..4].try_into().unwrap());
            if fd == 1 {
                let ty = u32::from_ne_bytes(rec[4..8].try_into().unwrap());
                return if ty == PROX_FDTYPE_PIPE {
                    Ok(())
                } else {
                    Err("stdout is not an anonymous pipe".into())
                };
            }
        }
        Err("stdout is not an anonymous pipe".into())
    }
}

/// This process's parent pid (`getppid`), or `0` off macOS.
#[must_use]
pub fn parent_pid() -> i32 {
    #[cfg(target_vendor = "apple")]
    {
        imp::parent_pid()
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        0
    }
}

/// The executable the kernel records for `pid`, or `None` (gone, unreadable,
/// or off macOS).
#[must_use]
pub fn exe_path(pid: i32) -> Option<std::path::PathBuf> {
    #[cfg(target_vendor = "apple")]
    {
        imp::exe_path(pid)
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let _ = pid;
        None
    }
}

/// `pid`'s parent pid from its short BSD info, or `None`.
#[must_use]
pub fn parent_of(pid: i32) -> Option<i32> {
    #[cfg(target_vendor = "apple")]
    {
        imp::short_info(pid).and_then(|i| i32::try_from(i.ppid).ok())
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let _ = pid;
        None
    }
}

/// Attest that `pid` is Apple's `/usr/bin/sudo` running as root with our real
/// uid (module docs). `Err(reason)` on any miss; off macOS always `Err`.
///
/// # Errors
/// A short, secret-free reason safe to surface to the agent.
pub fn attest_sudo(pid: i32) -> Result<(), AttestError> {
    #[cfg(target_vendor = "apple")]
    {
        imp::attest_sudo(pid)
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let _ = pid;
        Err("sudo askpass is macOS-only".into())
    }
}

/// Whether this process's fd 1 is an anonymous pipe (module docs). `Err` with a
/// reason otherwise; off macOS always `Err`.
///
/// # Errors
/// A short, secret-free reason.
pub fn stdout_is_anon_pipe() -> Result<(), AttestError> {
    #[cfg(target_vendor = "apple")]
    {
        imp::stdout_is_anon_pipe()
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        Err("sudo askpass is macOS-only".into())
    }
}

#[cfg(test)]
#[path = "sudo_tests.rs"]
mod tests;
