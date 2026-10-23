// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Kernel parent attestation and scrubbed [`Secret`](crate::sudo::Secret) for the `sudo` askpass relay.

/// A password in flight, held off the agent's reach.
pub struct Secret {
    bytes: Box<[u8]>,
    len: usize,
}

impl Secret {
    /// At most 1023 bytes (what `sudo` reads), the rest dropped.
    #[must_use]
    pub fn from_bytes(src: &[u8]) -> Self {
        let len = src.len().min(1023);
        let mut bytes = vec![0u8; 1023].into_boxed_slice();
        bytes[..len].copy_from_slice(&src[..len]);
        Self { bytes, len }
    }

    /// Decode directly into owned scrubbed storage.
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

    #[inline]
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
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

#[inline]
const fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Direct storage for AppKit's secure UTF-16 field extraction.
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

    #[inline]
    pub const fn as_mut_units(&mut self) -> &mut [u16] {
        &mut self.units
    }

    #[inline]
    #[must_use]
    pub const fn as_units(&self) -> &[u16] {
        &self.units
    }
}

impl Drop for SecretUtf16 {
    fn drop(&mut self) {
        for unit in &mut self.units {
            // SAFETY: Each unit is live, aligned, writable owned storage.
            unsafe { std::ptr::write_volatile(unit, 0) };
        }
        std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
    }
}

/// One bounded askpass wire line, including its newline.
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

    #[inline]
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    /// Read exactly one bounded LF-terminated line directly into guarded storage.
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
            // SAFETY: Each byte is live, writable owned storage.
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

/// Why a `sudo` attestation refused.
pub type AttestError = String;

#[cfg(target_vendor = "apple")]
mod imp {
    use std::path::PathBuf;

    use super::AttestError;

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
        /// `int csops(pid_t, unsigned int ops, void *useraddr, size_t usersize)`.
        fn csops(pid: i32, ops: u32, useraddr: *mut core::ffi::c_void, usersize: usize) -> i32;
    }

    const PROC_PIDT_SHORTBSDINFO: i32 = 13;
    const SHORT_INFO_SIZE: usize = 64;
    const PROC_PIDLISTFDS: i32 = 1;
    const PROX_FDTYPE_PIPE: u32 = 6;
    const PROC_PIDPATHINFO_MAXSIZE: usize = 4096;

    const CS_OPS_STATUS: u32 = 0;
    const CS_OPS_IDENTITY: u32 = 11;
    const CS_VALID: u32 = 0x0000_0001;
    const CS_PLATFORM_BINARY: u32 = 0x0400_0000;

    /// `struct proc_bsdshortinfo` (`sys/proc_info.h`): 64 bytes on both Darwin ABIs.
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
    pub(super) fn cs_platform_valid(pid: i32) -> bool {
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

    /// Best-effort: `pid`'s code-signing identity contains `needle`.
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

    /// The attested facts of a genuine `sudo` at `pid`.
    pub(super) fn attest_sudo(pid: i32) -> Result<(), AttestError> {
        // SAFETY: A plain syscall with no arguments.
        let our_uid = unsafe { getuid() };
        attest(
            short_info(pid),
            our_uid,
            || exe_path(pid),
            || cs_platform_valid(pid),
        )?;
        let _ = cs_identity_contains(pid, b"com.apple.sudo");
        Ok(())
    }

    /// [`attest_sudo`]'s rule over the facts read for one pid.
    pub(super) fn attest(
        info: Option<ShortInfo>,
        our_uid: u32,
        exe: impl FnOnce() -> Option<PathBuf>,
        platform_valid: impl FnOnce() -> bool,
    ) -> Result<(), AttestError> {
        let info = info.ok_or("parent is gone or not readable")?;
        if info.uid != 0 {
            return Err("parent is not running as root".into());
        }
        if info.ruid != our_uid {
            return Err("parent's real uid is not ours".into());
        }
        match exe() {
            Some(p) if p == std::path::Path::new("/usr/bin/sudo") => {}
            Some(_) => return Err("parent is not /usr/bin/sudo".into()),
            None => return Err("parent's executable is not readable".into()),
        }
        if !platform_valid() {
            return Err("parent is not a valid Apple platform binary".into());
        }
        Ok(())
    }

    pub(super) fn parent_pid() -> i32 {
        // SAFETY: A plain syscall with no arguments.
        unsafe { getppid() }
    }

    /// Whether this process's fd 1 is an ANONYMOUS pipe (`PROX_FDTYPE_PIPE`).
    pub(super) fn stdout_is_anon_pipe() -> Result<(), AttestError> {
        // SAFETY: A plain syscall with no arguments.
        fd_one_is_anon_pipe(unsafe { getpid() })
    }

    /// [`stdout_is_anon_pipe`]'s read of `me`'s live fd table.
    pub(super) fn fd_one_is_anon_pipe(me: i32) -> Result<(), AttestError> {
        // SAFETY: a null buffer and zero size asks proc_pidinfo for the size.
        let need = unsafe { proc_pidinfo(me, PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
        let need = usize::try_from(need).ok().filter(|&n| n > 0 && n < 1 << 20);
        let Some(need) = need else {
            return Err("own fd table is not readable".into());
        };
        let mut buf = vec![0u8; need];
        // SAFETY: `buf` is `need` writable bytes, the size passed.
        let got =
            unsafe { proc_pidinfo(me, PROC_PIDLISTFDS, 0, buf.as_mut_ptr().cast(), need as i32) };
        let got = usize::try_from(got)
            .ok()
            .filter(|&n| n > 0 && n <= buf.len())
            .ok_or("own fd table is not readable")?;
        stdout_record_is_anon_pipe(&buf[..got])
    }

    /// [`stdout_is_anon_pipe`]'s rule over the `PROC_PIDLISTFDS` table it read.
    pub(super) fn stdout_record_is_anon_pipe(table: &[u8]) -> Result<(), AttestError> {
        const FDINFO: usize = 8;
        for rec in table.as_chunks::<FDINFO>().0 {
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

/// The executable the kernel records for `pid`, or `None` (gone, unreadable, or off macOS).
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

/// Attest that `pid` is Apple's `/usr/bin/sudo` running as root with our real uid.
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

/// Whether this process's fd 1 is an anonymous pipe.
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
