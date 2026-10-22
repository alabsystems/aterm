// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The paper master: generate it, read it back from a human, and derive its signing key,
//! without the master touching a disk, an argv, an environment variable, a log line or a
//! `Debug` impl.
//!
//! # Invariant
//!
//! The master secret is never written anywhere: not to `~/.aterm`, a temp file, shell history,
//! or the terminal after it has been read back. It exists on paper, and in this process from
//! the last character read until the roster signature is produced
//! (`no_file_written_by_a_run_contains_the_phrase_or_its_seed` pins the mint path).
//!
//! # Encoding
//!
//! 32 bytes as 52 characters of lowercase Crockford base32 (`0-9a-z` minus `i l o u`), shown in
//! groups of four. The reader strips spaces and hyphens, folds case, and maps `o→0`, `i→1`,
//! `l→1`. The last character's low four bits are padding the reader masks off, so the identity
//! is canonical, not the spelling. [`fingerprint`] prints a short digest of the derived public
//! key at generation and after every type-back, and `setup` refuses to arm until the retyped
//! phrase matches.
//!
//! The 32 bytes are the Ed25519 seed ([`ring::signature::Ed25519KeyPair::from_seed_unchecked`])
//! with no KDF: the phrase is the master, not a password, and the same paper always yields the
//! same identity. So it comes from a CSPRNG and is never human-chosen: a memorable phrase here
//! is a directly attackable private key.
//!
//! # Leak vectors
//!
//! 1. argv (world-readable via `ps`): no flag takes the master, and none may ever be added.
//! 2. environment (`ps -E`): no environment variable is read for the master.
//! 3. shell history: follows from (1).
//! 4. terminal scrollback: echo is off while reading, restored by a `Drop` guard ([`TtyEcho`])
//!    on every return, error or unwind. Death without unwinding (default SIGINT) leaves the
//!    terminal echo-less until the shell resets it; nothing secret is emitted.
//! 5. stdin redirection: the phrase is read from `/dev/tty`, refused if that is not a
//!    terminal, so `join < phrase.txt` fails loudly.
//! 6. swap / hibernation: not fully closable. [`MasterSeed`] and [`MasterPhrase`] `mlock(2)`
//!    their buffers, best-effort under `RLIMIT_MEMLOCK`; macOS encrypts swap by default. The
//!    residency window (last character read to signature) has no user interaction, network or
//!    await point.
//! 7. core dump: [`forbid_core_dumps`] sets `RLIMIT_CORE` to 0 before the prompt.
//! 8. logs / `Debug`: both secret types have hand-written `Debug` impls printing `<redacted>`,
//!    and no `Serialize` or `Display`; only [`fingerprint`] and the public key are printable.
//! 9. memory scrub: hand-rolled (the workspace has no `zeroize`). Both secrets are fixed
//!    arrays, never `String`/`Vec`, whose reallocation leaves unscrubbable copies in freed
//!    heap. The scrub is `write_volatile` plus a `compiler_fence`. Residuals:
//!    `ring::Ed25519KeyPair` copies the seed internally with no scrub hook, and returning a
//!    secret by value may memcpy it to a new, unlocked stack address, leaving the old bytes
//!    unscrubbed (`Drop` runs only at the final address; closing this would mean heap-pinning
//!    the secrets).
//! 10. clipboard: a pasted phrase stays in the pasteboard, which this tool cannot prevent.
//! 11. stdout redirection: the phrase is written to the [`Tty`] handle (`/dev/tty`, checked
//!     with `isatty`), never fd 1, and a run with no terminal is refused before a master is
//!     generated.
//! 12. the terminal's own persistence: terminals that write their grid to disk (aterm's
//!     seamless-update snapshots, iTerm2 session restore) are beyond this tool's reach. Run
//!     the ceremony in one that does not persist; `setup` reminds the owner to clear scrollback.

use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{Ed25519KeyPair, KeyPair};

/// Characters in the written master: 32 bytes in base32.
pub const MASTER_PHRASE_LEN: usize = 52;

/// The phrase alphabet: lowercase Crockford base32, `0-9a-z` minus the easily confused
/// `i l o u`.
const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// Overwrite `buf` with zeroes the optimizer may not delete. A zeroing loop on a buffer never
/// read again is dead-store elimination's textbook case; `write_volatile` cannot be removed
/// and the fence stops the writes sinking past the buffer's death. Do not "simplify" this to
/// `buf.fill(0)`; replace it only with `zeroize`, which the workspace does not carry.
fn scrub(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        // SAFETY: `b` is a live, uniquely borrowed, aligned `u8` from a mutable slice, so a
        // one-byte volatile write is in bounds and unaliased.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

/// [`scrub`] for the one-word bit accumulators the base32 codec threads master bits
/// through. Same contract: a volatile write the optimizer may not delete.
fn scrub_u32(v: &mut u32) {
    // SAFETY: `v` is a live, uniquely borrowed, correctly aligned `u32`.
    unsafe { std::ptr::write_volatile(v, 0) };
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

/// Ask the kernel to keep `buf` out of swap. Best-effort: `mlock` is bounded by
/// `RLIMIT_MEMLOCK` and its failure is ignored, so a machine with a tight limit can still
/// mint. It narrows leak vector 6; it does not close it.
fn try_lock(buf: &[u8]) {
    // SAFETY: `buf` is a live borrow, so its pointer and length describe mapped, readable
    // memory for the duration of the call. `mlock` neither reads nor writes the contents.
    unsafe {
        let _ = libc::mlock(buf.as_ptr().cast::<libc::c_void>(), buf.len());
    }
}

/// Undo [`try_lock`] before the memory is freed, so a long-running process does not leak
/// locked pages. Best-effort too.
fn try_unlock(buf: &[u8]) {
    // SAFETY: same live-borrow argument as `try_lock`; `munlock` on a range that was
    // never successfully locked is a harmless error return.
    unsafe {
        let _ = libc::munlock(buf.as_ptr().cast::<libc::c_void>(), buf.len());
    }
}

/// Set `RLIMIT_CORE` to zero so a crash in the mint path cannot write the master into a core
/// file (leak vector 7). Call it before the prompt. Best-effort and non-fatal.
pub fn forbid_core_dumps() {
    let zero = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `zero` is a fully initialized `rlimit` living for the call, and
    // `RLIMIT_CORE` is a valid resource. `setrlimit` writes nothing through the pointer.
    unsafe {
        let _ = libc::setrlimit(libc::RLIMIT_CORE, &raw const zero);
    }
}

/// The 32 raw bytes of the paper master, in a fixed array `mlock`ed on construction and
/// volatile-scrubbed on drop.
///
/// No `Clone`, `Serialize`, `Display` or byte accessor: a caller can only derive the keypair,
/// public key or fingerprint. A `fn bytes(&self)` would soon become a `write(path, ..)`.
pub struct MasterSeed {
    bytes: [u8; 32],
}

impl std::fmt::Debug for MasterSeed {
    /// Hand-written so a derive can never print the master (as `ReleaseCredentials` in
    /// `aterm-release/src/sign.rs`).
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
    /// Wrap 32 raw bytes, locking them out of swap immediately.
    fn new(bytes: [u8; 32]) -> Self {
        let seed = Self { bytes };
        try_lock(&seed.bytes);
        seed
    }

    /// The Ed25519 keypair this master is. Deterministic: the same phrase always yields the
    /// same identity.
    ///
    /// `from_seed_unchecked` is unchecked in that no public key is given to detect a corrupt
    /// seed; the fingerprint and the committed anchor are that check here.
    pub fn keypair(&self) -> Result<Ed25519KeyPair, String> {
        Ed25519KeyPair::from_seed_unchecked(&self.bytes)
            .map_err(|_| "master seed rejected by the Ed25519 implementation".to_string())
    }

    /// The master's base64 public key, the value in `pins::PAPER_MASTER_PUBKEYS`.
    pub fn pubkey_b64(&self) -> Result<String, String> {
        aterm_codec::base64::encode(self.keypair()?.public_key().as_ref())
            .map_err(|_| "public key too large to encode".to_string())
    }

    /// The short public [`fingerprint`] the owner eyeballs against the paper.
    pub fn fingerprint(&self) -> Result<String, String> {
        Ok(fingerprint(self.keypair()?.public_key().as_ref()))
    }

    /// Detached-sign `msg`'s exact bytes with the master. Only rosters are signed with it;
    /// nothing enforces that, but nothing else calls it.
    pub fn sign(&self, msg: &[u8]) -> Result<Vec<u8>, String> {
        Ok(self.keypair()?.sign(msg).as_ref().to_vec())
    }
}

/// The written master, the 52 base32 characters that go on paper, in a fixed array (leak
/// vector 9).
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

    /// Decode the phrase into the 32-byte seed. Infallible: [`generate_master`] and
    /// [`parse_master`] have already proved every character is in the alphabet.
    ///
    /// 52 characters carry 260 bits; the last four are padding and dropped, so two spellings
    /// can name one master while the identity stays exact.
    pub fn seed(&self) -> MasterSeed {
        let mut bytes = [0u8; 32];
        let mut acc: u32 = 0;
        let mut nbits: u32 = 0;
        let mut out = 0usize;
        for &c in &self.chars {
            acc = (acc << 5) | u32::from(b32_val(c));
            nbits += 5;
            if nbits >= 8 {
                nbits -= 8;
                bytes[out] = ((acc >> nbits) & 0xff) as u8;
                out += 1;
            }
        }
        debug_assert_eq!(out, 32);
        let seed = MasterSeed::new(bytes);
        // The `MasterSeed` owns its own copy: scrub this stack copy and the accumulator.
        scrub(&mut bytes);
        scrub_u32(&mut acc);
        seed
    }
}

/// The 5-bit value of one phrase character. Called only on alphabet bytes; the unreachable
/// fallback returns 0 rather than panicking with the master live in memory.
fn b32_val(c: u8) -> u8 {
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

/// Why a typed-back phrase was refused. No variant carries any input: a position is safe to
/// print, a character is a byte of the master.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhraseError {
    /// A character outside the alphabet, at this 1-based position among the significant
    /// (non-whitespace, non-hyphen) characters, counting as the owner counts what they wrote.
    BadChar { position: usize },
    /// The wrong number of significant characters; `got` is the count.
    Length { got: usize },
}

impl PhraseError {
    /// The message the tool prints: the position, never the character or its neighbours.
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

/// Parse a typed-back master phrase, fail-closed and forgiving.
///
/// ASCII whitespace and hyphens are stripped, case is folded, and `o→0`, `i→1`, `l→1` (the
/// alphabet excludes those letters, so the mapping is unambiguous). Then every character must
/// be in [`ALPHABET`], the first that is not reported by 1-based position only, and there must
/// be exactly [`MASTER_PHRASE_LEN`]. The character check runs first: "character 37" points at
/// the line to re-read, where "51 characters" does not.
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
            // A partial buffer holds a prefix of the master.
            scrub(&mut chars);
            return Err(PhraseError::BadChar { position: n + 1 });
        }
        if n < MASTER_PHRASE_LEN {
            // Count past the end so the length error reports the true count.
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

/// Generate a fresh master from the system CSPRNG: the only way a master is created.
pub fn generate_master() -> Result<MasterPhrase, String> {
    let mut raw = [0u8; 32];
    SystemRandom::new()
        .fill(&mut raw)
        .map_err(|_| "the system CSPRNG refused; refusing to mint a weak master".to_string())?;
    let phrase = encode_phrase(&raw);
    scrub(&mut raw);
    Ok(phrase)
}

/// Spell 32 seed bytes as the 52-character phrase [`MasterPhrase::seed`] decodes back to
/// exactly those bytes. Borrowed, never moved, so the caller's array stays the only copy
/// of the seed and the caller scrubs it.
fn encode_phrase(raw: &[u8; 32]) -> MasterPhrase {
    let mut chars = [0u8; MASTER_PHRASE_LEN];
    let mut acc: u32 = 0;
    let mut nbits: u32 = 0;
    let mut out = 0usize;
    // `for b in *raw` would move a COPY of the whole seed into `array::IntoIter`'s
    // backing storage, which nothing scrubs; iterate by reference so the only seed copy
    // is the caller's, which it scrubs.
    for &b in raw {
        acc = (acc << 8) | u32::from(b);
        nbits += 8;
        while nbits >= 5 {
            nbits -= 5;
            chars[out] = ALPHABET[((acc >> nbits) & 31) as usize];
            out += 1;
        }
    }
    // 256 bits leave one significant bit for the 52nd character; its low four bits are
    // padding, written as zero (and masked off again by `seed`).
    chars[out] = ALPHABET[((acc << (5 - nbits)) & 31) as usize];
    scrub_u32(&mut acc);
    let phrase = MasterPhrase { chars };
    try_lock(&phrase.chars);
    phrase
}

/// The short verification fingerprint: the first 8 hex characters of SHA-256 over the public
/// key, as `a3f2-9c1b`.
///
/// Over the public key, never the secret: a fingerprint of the secret is an offline check for
/// guesses, while the public key is committed in `pins.rs` anyway. 32 bits is not collision
/// resistance and need not be: it catches a hand-transcription error, which yields an
/// unrelated key, not an adversary grinding for a match.
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

/// Turns terminal echo off for the life of the value and restores the original settings on
/// drop, on success, error or unwind alike: a terminal left without echo hides the owner's
/// next keystrokes from them.
pub struct TtyEcho {
    fd: std::os::unix::io::RawFd,
    saved: libc::termios,
}

impl TtyEcho {
    /// Disable ECHO on `fd`, remembering the previous state.
    fn disable(fd: std::os::unix::io::RawFd) -> Result<Self, String> {
        // SAFETY: `saved` is only read after `tcgetattr` reports success, which is exactly
        // when the kernel has initialized it. `zeroed` is a valid bit pattern for termios
        // (it is a plain POD struct of integers and an array).
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is an open terminal descriptor owned by the caller and `saved` is a
        // live, correctly sized out-parameter.
        if unsafe { libc::tcgetattr(fd, &raw mut saved) } != 0 {
            return Err("could not read terminal settings from /dev/tty".to_string());
        }
        let mut raw = saved;
        raw.c_lflag &= !libc::ECHO;
        // SAFETY: same descriptor, and `raw` is a fully initialized copy of a valid
        // termios with one flag cleared.
        if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &raw const raw) } != 0 {
            return Err("could not disable terminal echo; refusing to prompt".to_string());
        }
        Ok(Self { fd, saved })
    }
}

impl Drop for TtyEcho {
    fn drop(&mut self) {
        // SAFETY: `self.fd` is still open: the caller declares this guard after the `File`
        // that owns the descriptor, so the guard drops first. `self.saved` is the initialized
        // snapshot taken in `disable`.
        unsafe {
            let _ = libc::tcsetattr(self.fd, libc::TCSAFLUSH, &raw const self.saved);
        }
    }
}

/// The longest line the phrase prompt reads: 52 characters plus room for separators and the
/// newline. A longer paste truncates into a clean length error.
const PROMPT_CAP: usize = 256;

/// The controlling terminal, opened explicitly: the only place a master is read from or
/// written to. `/dev/tty` is resolved by the kernel, not fd 0 or 1, so no shell redirection
/// can point it at a file (leak vectors 5 and 11), and a process without one cannot open it.
pub struct Tty {
    file: std::fs::File,
    fd: std::os::unix::io::RawFd,
}

impl std::fmt::Debug for Tty {
    /// Content-free, like every other `Debug` in this module.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Tty(/dev/tty)")
    }
}

impl Tty {
    /// Open the controlling terminal, or refuse. A caller must know it can deliver a master
    /// before generating one: a master with nowhere to go never existed for anyone.
    pub fn open() -> Result<Self, String> {
        use std::os::unix::io::AsRawFd as _;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
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
        // SAFETY: `fd` is an open descriptor for the `file` handle owned by this value.
        if unsafe { libc::isatty(fd) } != 1 {
            return Err("/dev/tty is not a terminal; refusing to handle the master".to_string());
        }
        Ok(Self { file, fd })
    }

    /// Write the phrase to the terminal: the one deliberate emission of a secret in this tree.
    ///
    /// It goes to the terminal (leak vector 11), is assembled in an `mlock`ed, volatile-scrubbed
    /// fixed array (leak vector 9), and is grouped in fours, which the parser strips. The flush is
    /// checked and its failure returned: `setup` arms a trust anchor on the strength of it.
    pub fn write_phrase(&mut self, phrase: &MasterPhrase) -> Result<(), String> {
        use std::io::Write as _;
        // 52 characters + 12 group spaces + newline.
        const GROUPED: usize = MASTER_PHRASE_LEN + MASTER_PHRASE_LEN / 4 - 1 + 1;
        let mut buf = [0u8; GROUPED];
        try_lock(&buf);
        let mut at = 0usize;
        for (i, &c) in phrase.chars.iter().enumerate() {
            if i > 0 && i % 4 == 0 {
                buf[at] = b' ';
                at += 1;
            }
            buf[at] = c;
            at += 1;
        }
        buf[GROUPED - 1] = b'\n';
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

    /// Write one non-secret line to the terminal: the heading and fingerprint that frame the
    /// phrase. They share its surface so "write the next line down" cannot point at a line sent
    /// elsewhere.
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

/// Prompt for the master phrase on `/dev/tty` with echo off and parse it: one attempt.
///
/// The outer error is the terminal failing, the inner the typed text failing to parse: a
/// retrying caller (`setup`'s transcription gate) aborts on the first and retries the second.
/// EOF presents as an empty attempt (`Length { got: 0 }`), which a retrying caller must also
/// treat as an abort. `/dev/tty` is opened explicitly, never fd 0, so `join < phrase.txt`
/// fails (leak vector 5); with no controlling terminal this refuses.
pub fn prompt_master_attempt(prompt: &str) -> Result<Result<MasterPhrase, PhraseError>, String> {
    use std::io::{Read as _, Write as _};

    let handle = Tty::open()?;
    let fd = handle.fd;
    let mut tty = handle.file;
    // Echo goes off before the prompt is written, so no early keystroke is echoed.
    let _echo = TtyEcho::disable(fd)?;
    let _ = tty.write_all(prompt.as_bytes());
    let _ = tty.flush();

    let mut buf = [0u8; PROMPT_CAP];
    try_lock(&buf);
    let mut used = 0usize;
    loop {
        if used == buf.len() {
            break;
        }
        match tty.read(&mut buf[used..]) {
            Ok(0) => break,
            Ok(n) => {
                used += n;
                if buf[..used].contains(&b'\n') {
                    break;
                }
            }
            Err(e) => {
                scrub(&mut buf);
                try_unlock(&buf);
                let mut s = String::from("reading the master from /dev/tty: ");
                s.push_str(&e.to_string());
                return Err(s);
            }
        }
    }
    // Echo was off, so the newline the owner typed was never printed; emit one so the
    // next output does not land on the prompt line.
    let _ = tty.write_all(b"\n");

    // Not `from_utf8_lossy`: it would allocate an unscrubbable copy on a non-ASCII byte.
    // `parse_master` names the position of a non-ASCII character; invalid UTF-8 parses as
    // empty.
    let text = std::str::from_utf8(&buf[..used]).unwrap_or("");
    let parsed = parse_master(text);
    scrub(&mut buf);
    try_unlock(&buf);
    Ok(parsed)
}

/// [`prompt_master_attempt`] flattened: a typo is a final error, message and all. The
/// right shape for `join` and `master-check`, where the command is cheap to re-run.
pub fn prompt_for_master(prompt: &str) -> Result<MasterPhrase, String> {
    match prompt_master_attempt(prompt)? {
        Ok(phrase) => Ok(phrase),
        Err(e) => Err(e.message()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A synthetic phrase: the alphabet in order, then filler, ending on the canonical '0'.
    const SYNTHETIC: &str = "0123456789abcdefghjkmnpqrstvwxyz0123456789abcdefghj0";

    /// The refusal for an input that must not parse. [`MasterPhrase`] has no `PartialEq` on
    /// purpose: equality on secret material invites comparison and timing side channels.
    fn refused(input: &str) -> PhraseError {
        // `expect_err` needs `Debug` on the Ok type, which prints `<redacted>`, so a failure
        // cannot spill phrase bytes into the panic message.
        parse_master(input).expect_err("this input must be refused")
    }

    #[test]
    fn exactly_fifty_two_characters_is_accepted_and_case_folds() {
        assert_eq!(SYNTHETIC.len(), MASTER_PHRASE_LEN, "fixture length");
        let phrase = parse_master(SYNTHETIC).expect("52 characters must parse");
        assert_eq!(phrase.as_str(), SYNTHETIC);
        let upper = parse_master(&SYNTHETIC.to_uppercase()).expect("case is folded");
        assert_eq!(
            phrase.seed().pubkey_b64().unwrap(),
            upper.seed().pubkey_b64().unwrap()
        );
    }

    #[test]
    fn o_i_and_l_are_read_as_the_characters_they_resemble() {
        let mut misread = String::from(SYNTHETIC);
        misread = misread.replacen('0', "o", 1);
        misread = misread.replacen('1', "i", 2);
        misread = misread.replacen('1', "l", 1);
        assert_ne!(misread, SYNTHETIC, "the fixture must actually differ");
        assert_eq!(
            parse_master(&misread).unwrap().seed().pubkey_b64().unwrap(),
            parse_master(SYNTHETIC)
                .unwrap()
                .seed()
                .pubkey_b64()
                .unwrap(),
            "a misread spelling must name the same master"
        );
    }

    #[test]
    fn fifty_one_and_fifty_three_characters_are_refused_with_the_count() {
        let short = &SYNTHETIC[..51];
        assert_eq!(
            refused(short),
            PhraseError::Length { got: 51 },
            "51 characters must not be accepted"
        );
        assert!(refused(short).message().contains("51"));

        let mut long = String::from(SYNTHETIC);
        long.push('a');
        assert_eq!(refused(&long), PhraseError::Length { got: 53 });
        assert!(refused(&long).message().contains("53"));

        // Far too long: the count is still true, and nothing was written past the buffer.
        let flood = SYNTHETIC.repeat(4);
        assert_eq!(refused(&flood), PhraseError::Length { got: 208 });
        assert_eq!(refused(""), PhraseError::Length { got: 0 });
    }

    #[test]
    fn a_bad_character_is_refused_by_position_and_never_echoed() {
        let mut bad = String::from(SYNTHETIC);
        // `u` is outside the alphabet and is not mapped to anything.
        bad.replace_range(36..37, "u");
        assert_eq!(
            refused(&bad),
            PhraseError::BadChar { position: 37 },
            "the 37th character (1-based) is the bad one"
        );
        let msg = refused(&bad).message();
        assert!(msg.contains("character 37"), "{msg}");
        // Nor does it carry the neighbouring characters.
        assert!(!msg.contains(&bad[30..40]), "{msg}");

        // First and last positions, so the 1-based arithmetic is pinned at both ends.
        let mut first = String::from(SYNTHETIC);
        first.replace_range(0..1, "!");
        assert_eq!(refused(&first), PhraseError::BadChar { position: 1 });
        let mut last = String::from(SYNTHETIC);
        last.replace_range(51..52, "u");
        assert_eq!(refused(&last), PhraseError::BadChar { position: 52 });
    }

    #[test]
    fn a_bad_character_is_reported_even_when_the_length_is_also_wrong() {
        let mut bad = String::from(&SYNTHETIC[..40]);
        bad.replace_range(9..10, "u");
        assert_eq!(refused(&bad), PhraseError::BadChar { position: 10 });
    }

    #[test]
    fn separators_are_stripped_and_positions_count_significant_characters() {
        let grouped = "0123 4567 89ab cdef ghjk mnpq rstv-wxyz\n\
                       0123 4567 89ab cdef ghj0";
        assert_eq!(
            parse_master(grouped).unwrap().as_str(),
            SYNTHETIC,
            "grouping must not change the phrase"
        );
        // The 5th significant character is bad; it sits at raw offset 5 because of the
        // space, and the report must say 5, not 6.
        let mut bad = String::from(grouped);
        bad.replace_range(5..6, "u");
        assert_eq!(refused(&bad), PhraseError::BadChar { position: 5 });
    }

    /// Two generations differ, so the CSPRNG is consulted.
    #[test]
    fn generate_produces_a_fresh_fifty_two_character_phrase() {
        let a = generate_master().unwrap();
        let b = generate_master().unwrap();
        assert_eq!(a.as_str().len(), MASTER_PHRASE_LEN);
        assert!(a.as_str().bytes().all(|c| ALPHABET.contains(&c)));
        assert!(
            a.as_str().bytes().all(|c| !c.is_ascii_uppercase()),
            "generated phrases are lowercase so the paper has one canonical spelling"
        );
        assert_ne!(a.as_str(), b.as_str(), "two masters must not be the same");
        // A generated phrase round-trips through the reader.
        assert_eq!(parse_master(a.as_str()).unwrap().as_str(), a.as_str());
    }

    /// THE PAPER IS THE SEED. A phrase decodes to exactly the 32 bytes it was spelled
    /// from, its last character carries zero padding bits (one canonical spelling), and
    /// the derivation is a cross-version contract: paper written under an earlier build
    /// names the same master in this one. The pinned key was derived outside this crate
    /// (a Crockford decode, then OpenSSL's Ed25519).
    #[test]
    fn a_phrase_decodes_to_exactly_the_seed_it_spells() {
        for raw in [
            [0x00; 32],
            [0xFF; 32],
            [0x01; 32],
            *b"0123456789abcdefghijklmnopqrstuv",
        ] {
            let phrase = encode_phrase(&raw);
            assert!(
                matches!(phrase.chars[51], b'0' | b'g'),
                "padding bits are zero"
            );
            let spelled = Ed25519KeyPair::from_seed_unchecked(&raw).unwrap();
            assert_eq!(
                phrase.seed().pubkey_b64().unwrap(),
                aterm_codec::base64::encode(spelled.public_key().as_ref()).unwrap()
            );
        }
        assert_eq!(
            parse_master(SYNTHETIC)
                .unwrap()
                .seed()
                .pubkey_b64()
                .unwrap(),
            "+K4Suo6CHtd/Se3P/nIfZ/FpgUX0kUMJafiLDrxMius="
        );
    }

    /// Spellings that differ only in the last character's low four bits name the same
    /// master; its top bit is significant.
    #[test]
    fn the_padding_bits_of_the_last_character_are_masked() {
        let base = parse_master(SYNTHETIC)
            .unwrap()
            .seed()
            .pubkey_b64()
            .unwrap();
        // '7' = 0b00111: same top bit as '0', different padding bits — same master.
        let mut pad = String::from(SYNTHETIC);
        pad.replace_range(51..52, "7");
        assert_eq!(
            parse_master(&pad).unwrap().seed().pubkey_b64().unwrap(),
            base
        );
        // 'g' = 0b10000: different top bit — different master.
        let mut sig = String::from(SYNTHETIC);
        sig.replace_range(51..52, "g");
        assert_ne!(
            parse_master(&sig).unwrap().seed().pubkey_b64().unwrap(),
            base
        );
    }

    #[test]
    fn the_fingerprint_matches_on_a_correct_transcription_and_differs_otherwise() {
        let phrase = parse_master(SYNTHETIC).unwrap();
        let fp = phrase.seed().fingerprint().unwrap();
        assert_eq!(fp.len(), 9, "8 hex characters and one hyphen: {fp}");
        assert_eq!(&fp[4..5], "-", "{fp}");

        // A correct type-back reproduces it — this is the eyeball check.
        let typed_back =
            parse_master("0123 4567 89ab cdef ghjk mnpq rstv wxyz 0123 4567 89ab cdef ghj0")
                .unwrap();
        assert_eq!(typed_back.seed().fingerprint().unwrap(), fp);

        // A one-character mistranscription changes it.
        let mut slip = String::from(SYNTHETIC);
        slip.replace_range(17..18, "7");
        assert_ne!(&slip, SYNTHETIC, "the fixture must actually differ");
        assert_ne!(
            parse_master(&slip).unwrap().seed().fingerprint().unwrap(),
            fp,
            "a single wrong character must change the fingerprint"
        );
    }

    /// Pins the input: the fingerprint is taken over the public key, never the secret.
    #[test]
    fn the_fingerprint_is_taken_over_the_public_key() {
        let seed = parse_master(SYNTHETIC).unwrap().seed();
        let kp = seed.keypair().unwrap();
        assert_eq!(
            seed.fingerprint().unwrap(),
            fingerprint(kp.public_key().as_ref())
        );
        // Control: other 32 bytes fingerprint differently.
        let raw_seed_fp = fingerprint(&[0x01u8; 32]);
        assert_ne!(seed.fingerprint().unwrap(), raw_seed_fp);
    }

    #[test]
    fn debug_never_prints_secret_material() {
        let phrase = parse_master(SYNTHETIC).unwrap();
        let seed = phrase.seed();
        let p = format!("{phrase:?}");
        let s = format!("{seed:?}");
        assert_eq!(p, "MasterPhrase(<redacted>)");
        assert_eq!(s, "MasterSeed(<redacted>)");
        assert!(!p.contains("0123"), "{p}");
        assert!(!s.contains("0123"), "{s}");
    }

    /// Tests the primitive, the only testable part of vector 9: whether the optimizer honours
    /// `write_volatile` is the compiler's contract.
    #[test]
    fn scrub_zeroes_the_buffer() {
        let mut buf = *b"0123456789abcdef";
        assert!(buf.iter().any(|b| *b != 0), "the fixture must start dirty");
        scrub(&mut buf);
        assert!(buf.iter().all(|b| *b == 0));
    }
}
