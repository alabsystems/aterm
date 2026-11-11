// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Paper master phrase generation, terminal prompting, and Ed25519 key derivation.
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{Ed25519KeyPair, KeyPair};

/// Number of base32 characters in the written master phrase.
pub const MASTER_PHRASE_LEN: usize = 52;

const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// Overwrites `buf` with zeroes via volatile writes and a sequential-consistency compiler fence.
pub fn scrub(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        // SAFETY: `b` is a live, uniquely borrowed, aligned `u8` from a mutable slice, so a
        // one-byte volatile write is in bounds and unaliased.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

fn scrub_u32(v: &mut u32) {
    // SAFETY: `v` is a live, uniquely borrowed, aligned `u32`.
    unsafe { std::ptr::write_volatile(v, 0) };
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

fn try_lock(buf: &[u8]) {
    // SAFETY: `buf` points to `buf.len()` live bytes for the duration of the `mlock` call. `mlock`
    // neither reads nor writes the contents.
    unsafe {
        let _ = libc::mlock(buf.as_ptr().cast::<libc::c_void>(), buf.len());
    }
}

fn try_unlock(buf: &[u8]) {
    // SAFETY: `buf` points to `buf.len()` live bytes for the duration of the `munlock` call.
    // `munlock` neither reads nor writes the contents, and it is harmless on a range `try_lock`
    // never managed to lock.
    unsafe {
        let _ = libc::munlock(buf.as_ptr().cast::<libc::c_void>(), buf.len());
    }
}

/// Sets `RLIMIT_CORE` to zero so a crash cannot dump the master secret.
pub fn forbid_core_dumps() {
    let zero = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `zero` is a fully initialized `rlimit` living for the call, and `RLIMIT_CORE` is a
    // valid resource. `setrlimit` writes nothing through the pointer.
    unsafe {
        let _ = libc::setrlimit(libc::RLIMIT_CORE, &raw const zero);
    }
}

/// Raw 32-byte paper master seed, `mlock`ed on creation and volatile-scrubbed on drop.
pub struct MasterSeed {
    bytes: [u8; 32],
}

impl std::fmt::Debug for MasterSeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MasterSeed(<redacted>)")
    }
}

impl Drop for MasterSeed {
    fn drop(&mut self) {
        try_unlock(&self.bytes);
        scrub(&mut self.bytes);
    }
}

impl MasterSeed {
    fn new(bytes: [u8; 32]) -> Self {
        let seed = Self { bytes };
        try_lock(&seed.bytes);
        seed
    }

    /// Derives the deterministic Ed25519 keypair for this master seed.
    pub fn keypair(&self) -> Result<Ed25519KeyPair, String> {
        Ed25519KeyPair::from_seed_unchecked(&self.bytes)
            .map_err(|_| "master seed rejected by the Ed25519 implementation".to_string())
    }

    /// Returns the master's base64 public key (`pins::PAPER_MASTER_PUBKEYS`).
    pub fn pubkey_b64(&self) -> Result<String, String> {
        aterm_codec::base64::encode(self.keypair()?.public_key().as_ref())
            .map_err(|_| "public key too large to encode".to_string())
    }

    /// Returns the short public [`fingerprint`] for visual verification.
    pub fn fingerprint(&self) -> Result<String, String> {
        Ok(fingerprint(self.keypair()?.public_key().as_ref()))
    }

    /// Produces a detached signature over `msg` with the master key.
    pub fn sign(&self, msg: &[u8]) -> Result<Vec<u8>, String> {
        Ok(self.keypair()?.sign(msg).as_ref().to_vec())
    }
}

/// Written 52-character base32 master phrase, `mlock`ed and volatile-scrubbed on drop.
pub struct MasterPhrase {
    chars: [u8; MASTER_PHRASE_LEN],
}

impl std::fmt::Debug for MasterPhrase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MasterPhrase(<redacted>)")
    }
}

impl Drop for MasterPhrase {
    fn drop(&mut self) {
        try_unlock(&self.chars);
        scrub(&mut self.chars);
    }
}

impl MasterPhrase {
    /// The phrase as text. Test-only: no shipped path prints a phrase.
    #[cfg(test)]
    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.chars).unwrap_or("")
    }

    /// Decodes the phrase into its 32-byte [`MasterSeed`] ([`generate_master`], [`parse_master`]).
    pub fn seed(&self) -> MasterSeed {
        let mut bytes = b32_decode(&self.chars);
        let seed = MasterSeed::new(bytes);
        scrub(&mut bytes);
        seed
    }
}

/// Decodes 52 Crockford Base32 characters into the 32-byte seed, masking the 4 trailing pad bits.
#[must_use]
pub fn b32_decode(chars: &[u8; MASTER_PHRASE_LEN]) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    let mut acc: u32 = 0;
    let mut nbits: u32 = 0;
    let mut out = 0usize;
    for &c in chars {
        acc = (acc << 5) | u32::from(b32_val(c));
        nbits += 5;
        if nbits >= 8 {
            nbits -= 8;
            bytes[out] = ((acc >> nbits) & 0xff) as u8;
            out += 1;
        }
    }
    debug_assert_eq!(out, 32);
    scrub_u32(&mut acc);
    bytes
}

const fn b32_val(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'h' => c - b'a' + 10,
        b'j' | b'k' => c - b'j' + 18,
        b'm' | b'n' => c - b'm' + 20,
        b'p'..=b't' => c - b'p' + 22,
        b'v'..=b'z' => c - b'v' + 27,
        _ => 0,
    }
}

/// Reason a typed-back master phrase was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhraseError {
    /// Out-of-alphabet character at a 1-based significant character position.
    BadChar { position: usize },
    /// Wrong number of significant characters.
    Length { got: usize },
}

impl PhraseError {
    /// Formats the user-facing error message without echoing secret characters.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::BadChar { position } => {
                let mut s = String::from("character ");
                s.push_str(&position.to_string());
                s.push_str(
                    " of the master phrase is not a phrase character (0-9 and a-z \
                     without i, l, o, u) — check that position against the paper",
                );
                s
            }
            Self::Length { got } => {
                let mut s =
                    String::from("the master phrase must be exactly 52 characters; this one has ");
                s.push_str(&got.to_string());
                s
            }
        }
    }
}

/// Parses and normalizes a typed-back master phrase.
pub fn parse_master(input: &str) -> Result<MasterPhrase, PhraseError> {
    let mut chars = [0u8; MASTER_PHRASE_LEN];
    let mut n = 0usize;
    for c in input.bytes() {
        if c.is_ascii_whitespace() || c == b'-' {
            continue;
        }
        let c = match c.to_ascii_lowercase() {
            b'o' => b'0',
            b'i' | b'l' => b'1',
            other => other,
        };
        if !ALPHABET.contains(&c) {
            scrub(&mut chars);
            return Err(PhraseError::BadChar { position: n + 1 });
        }
        if n < MASTER_PHRASE_LEN {
            chars[n] = c;
        }
        n += 1;
    }
    if n != MASTER_PHRASE_LEN {
        scrub(&mut chars);
        return Err(PhraseError::Length { got: n });
    }
    let phrase = MasterPhrase { chars };
    try_lock(&phrase.chars);
    Ok(phrase)
}

/// Generates a fresh [`MasterPhrase`] from the system CSPRNG.
pub fn generate_master() -> Result<MasterPhrase, String> {
    let mut raw = [0u8; 32];
    SystemRandom::new()
        .fill(&mut raw)
        .map_err(|_| "the system CSPRNG refused; refusing to mint a weak master".to_string())?;
    let phrase = encode_phrase(&raw);
    scrub(&mut raw);
    Ok(phrase)
}

/// Encodes a 32-byte seed into 52 lowercase Crockford Base32 characters (`chars[51]` is `0` or `g`).
#[must_use]
pub fn b32_encode(raw: &[u8; 32]) -> [u8; MASTER_PHRASE_LEN] {
    let mut chars = [0u8; MASTER_PHRASE_LEN];
    let mut acc: u32 = 0;
    let mut nbits: u32 = 0;
    let mut out = 0usize;
    for &b in raw {
        acc = (acc << 8) | u32::from(b);
        nbits += 8;
        while nbits >= 5 {
            nbits -= 5;
            chars[out] = ALPHABET[((acc >> nbits) & 31) as usize];
            out += 1;
        }
    }
    chars[out] = ALPHABET[((acc << (5 - nbits)) & 31) as usize];
    scrub_u32(&mut acc);
    chars
}

fn encode_phrase(raw: &[u8; 32]) -> MasterPhrase {
    let chars = b32_encode(raw);
    let phrase = MasterPhrase { chars };
    try_lock(&phrase.chars);
    phrase
}

/// Returns the 8-hex-character SHA-256 verification fingerprint (`xxxx-xxxx`) of `pubkey_raw`.
#[must_use]
pub fn fingerprint(pubkey_raw: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, pubkey_raw);
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::new();
    for (i, b) in digest.as_ref().iter().take(4).enumerate() {
        if i == 2 {
            out.push('-');
        }
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 0x0f)]));
    }
    out
}

/// Guard that disables terminal echo on creation and restores it on drop.
pub struct TtyEcho {
    fd: std::os::unix::io::RawFd,
    saved: libc::termios,
}

impl TtyEcho {
    fn disable(fd: std::os::unix::io::RawFd) -> Result<Self, String> {
        // SAFETY: `termios` is a plain-old-data struct initialized by `tcgetattr` before read.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is an open terminal descriptor and `saved` is a valid out-pointer.
        if unsafe { libc::tcgetattr(fd, &raw mut saved) } != 0 {
            return Err("could not read terminal settings from /dev/tty".to_string());
        }
        let mut raw = saved;
        raw.c_lflag &= !libc::ECHO;
        // SAFETY: `fd` is an open terminal descriptor and `raw` is an initialized `termios`.
        if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &raw const raw) } != 0 {
            return Err("could not disable terminal echo; refusing to prompt".to_string());
        }
        Ok(Self { fd, saved })
    }
}

impl Drop for TtyEcho {
    fn drop(&mut self) {
        // SAFETY: `self.fd` is still open: the caller declares this guard after the `File` that
        // owns the descriptor, so the guard drops first. `self.saved` is the initialized snapshot
        // taken in `disable`.
        unsafe {
            let _ = libc::tcsetattr(self.fd, libc::TCSAFLUSH, &raw const self.saved);
        }
    }
}

const PROMPT_CAP: usize = 256;

/// Controlling terminal (`/dev/tty`) used to read and display the master phrase.
pub struct Tty {
    file: std::fs::File,
    fd: std::os::unix::io::RawFd,
}

impl std::fmt::Debug for Tty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Tty(/dev/tty)")
    }
}

/// Byte length of a 52-character master phrase formatted in 4-character space-separated groups plus `\n`.
pub const GROUPED_PHRASE_LEN: usize = MASTER_PHRASE_LEN + MASTER_PHRASE_LEN / 4 - 1 + 1;

/// Formats a 52-character master phrase into 13 space-separated 4-character groups terminated by `\n`.
#[must_use]
pub fn format_phrase_line(chars: &[u8; MASTER_PHRASE_LEN]) -> [u8; GROUPED_PHRASE_LEN] {
    let mut buf = [0u8; GROUPED_PHRASE_LEN];
    let mut at = 0usize;
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 && i % 4 == 0 {
            buf[at] = b' ';
            at += 1;
        }
        buf[at] = c;
        at += 1;
    }
    buf[GROUPED_PHRASE_LEN - 1] = b'\n';
    buf
}

impl Tty {
    /// Opens `/dev/tty` and verifies it is a terminal.
    pub fn open() -> Result<Self, String> {
        use std::os::unix::fs::OpenOptionsExt as _;
        use std::os::unix::io::AsRawFd as _;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open("/dev/tty")
            .map_err(|e| {
                let mut s = String::from(
                    "cannot open /dev/tty (the master is read from, and written to, the \
                     terminal only — never stdin, argv, an environment variable, a file, \
                     or a redirected stdout): ",
                );
                s.push_str(&e.to_string());
                s
            })?;
        let fd = file.as_raw_fd();
        // SAFETY: `fd` is an open descriptor owned by `file`.
        if unsafe { libc::isatty(fd) } != 1 {
            return Err("/dev/tty is not a terminal; refusing to handle the master".to_string());
        }
        Ok(Self { file, fd })
    }

    /// Writes the formatted master phrase to `/dev/tty`.
    pub fn write_phrase(&mut self, phrase: &MasterPhrase) -> Result<(), String> {
        use std::io::Write as _;
        let mut buf = format_phrase_line(&phrase.chars);
        try_lock(&buf);
        let wrote = self.file.write_all(&buf).and_then(|()| self.file.flush());
        scrub(&mut buf);
        try_unlock(&buf);
        wrote.map_err(|e| {
            let mut s = String::from(
                "the master phrase could NOT be written to the terminal, so it has not \
                 reached anyone and is now unrecoverable: ",
            );
            s.push_str(&e.to_string());
            s
        })
    }

    /// Writes one non-secret line with a trailing newline to `/dev/tty`.
    pub fn write_line(&mut self, line: &str) -> Result<(), String> {
        use std::io::Write as _;
        self.file
            .write_all(line.as_bytes())
            .and_then(|()| self.file.write_all(b"\n"))
            .and_then(|()| self.file.flush())
            .map_err(|e| {
                let mut s = String::from("writing to the terminal: ");
                s.push_str(&e.to_string());
                s
            })
    }
}

/// Prompts for the master phrase on `/dev/tty` with echo disabled and parses one attempt.
pub fn prompt_master_attempt(prompt: &str) -> Result<Result<MasterPhrase, PhraseError>, String> {
    use std::io::{Read as _, Write as _};

    let handle = Tty::open()?;
    let fd = handle.fd;
    let mut tty = handle.file;
    let _echo = TtyEcho::disable(fd)?;
    let _ = tty.write_all(prompt.as_bytes());
    let _ = tty.flush();

    let mut buf = [0u8; PROMPT_CAP];
    let mut chars = [0u8; MASTER_PHRASE_LEN];
    try_lock(&buf);
    try_lock(&chars);
    let mut n = 0usize;
    let mut first_bad: Option<usize> = None;
    loop {
        match tty.read(&mut buf) {
            Ok(0) => break,
            Ok(read_len) => {
                let newline_at = buf[..read_len].iter().position(|&b| b == b'\n');
                let slice = &buf[..newline_at.unwrap_or(read_len)];
                for &raw in slice {
                    if raw.is_ascii_whitespace() || raw == b'-' {
                        continue;
                    }
                    let folded = match raw.to_ascii_lowercase() {
                        b'o' => b'0',
                        b'i' | b'l' => b'1',
                        other => other,
                    };
                    if !ALPHABET.contains(&folded) {
                        if first_bad.is_none() {
                            first_bad = Some(n.saturating_add(1));
                        }
                    } else if first_bad.is_none() && n < MASTER_PHRASE_LEN {
                        chars[n] = folded;
                    }
                    n = n.saturating_add(1);
                }
                if newline_at.is_some() {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => {
                scrub(&mut buf);
                scrub(&mut chars);
                try_unlock(&buf);
                try_unlock(&chars);
                let mut s = String::from("reading the master from /dev/tty: ");
                s.push_str(&e.to_string());
                return Err(s);
            }
        }
    }
    let _ = tty.write_all(b"\n");
    scrub(&mut buf);
    try_unlock(&buf);

    if let Some(position) = first_bad {
        scrub(&mut chars);
        try_unlock(&chars);
        return Ok(Err(PhraseError::BadChar { position }));
    }
    if n != MASTER_PHRASE_LEN {
        scrub(&mut chars);
        try_unlock(&chars);
        return Ok(Err(PhraseError::Length { got: n }));
    }
    try_unlock(&chars);
    let phrase = MasterPhrase { chars };
    try_lock(&phrase.chars);
    Ok(Ok(phrase))
}

/// Prompts for the master phrase via [`prompt_master_attempt`], flattening parse errors.
pub fn prompt_for_master(prompt: &str) -> Result<MasterPhrase, String> {
    match prompt_master_attempt(prompt)? {
        Ok(phrase) => Ok(phrase),
        Err(e) => Err(e.message()),
    }
}

#[cfg(test)]
#[path = "master_tests.rs"]
mod tests;
