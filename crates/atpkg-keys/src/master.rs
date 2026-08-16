// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The **paper master**: generate it, read it back from a human, and mint machine keys
//! from it — without the master ever touching a disk, an argv, an environment variable,
//! a log line or a `Debug` impl.
//!
//! # The one invariant everything else rests on
//!
//! THE MASTER SECRET IS NEVER WRITTEN ANYWHERE. Not to `~/.aterm`, not to a temp file,
//! not to a shell history, not to the terminal after it has been read back. It exists on
//! paper, and inside this process for the few milliseconds between the last character
//! being read and the roster signature being produced. Everything below is in service of
//! that sentence, and a test asserts the mint path writes no bytes derived from it.
//!
//! # The encoding is the owner's choice: 52 base32 characters (ruling 2026-08-15)
//!
//! Thirty-two bytes, written as 52 characters of lowercase Crockford base32 — `0-9` and
//! `a-z` minus `i l o u`, the characters hands confuse. Hex was the first spelling; the
//! owner ruled 64 characters too long to write, and base32 is the shortest spelling that
//! stays lowercase alphanumeric. Shown in groups of four. The reader strips spaces and
//! hyphens, folds case, and maps `o→0`, `i→1`, `l→1`, so the classic misreads parse as
//! what they resemble instead of failing. The final character carries four padding bits
//! the reader masks off — the IDENTITY (checked by fingerprint) is canonical, not the
//! spelling. Hand-copied characters WILL still eventually be typed back wrong, so
//! [`fingerprint`] prints a short digest of the derived PUBLIC key at generation and
//! after every type-back, and `setup` refuses to arm until the retyped phrase matches.
//!
//! # Why the phrase is generated, never chosen
//!
//! The 32 bytes ARE the Ed25519 seed ([`ring::signature::Ed25519KeyPair::from_seed_unchecked`],
//! the in-tree idiom `atpkg::sig` already uses in its fixtures). There is no KDF and no
//! stretching, deliberately — that is what makes the phrase itself the master rather than
//! a password to something else, and what makes the derivation deterministic, so the same
//! paper always yields the same identity. The direct consequence is that the phrase must
//! come from a CSPRNG and must never be human-chosen: a memorable "phrase" here is not a
//! password guarding a key, it is a directly attackable private key.
//!
//! # Leak vectors, each named and each answered
//!
//! 1. **argv** — world-readable via `ps` on macOS. There is no `--master <hex>` flag and
//!    there must never be one. The only flags this tool takes are non-secret. A future
//!    "convenience" flag here would be a security regression, and this sentence exists
//!    because that is precisely how such a flag gets added.
//! 2. **environment** — `ps -E`. No environment variable is read for the master. Note the
//!    habit being deliberately broken: the retired design read `ATERM_UPDATE_PUBKEY` and
//!    friends from the environment.
//! 3. **shell history** — falls out of (1). Nothing secret is ever typed on a command
//!    line, so nothing secret can land in history.
//! 4. **terminal scrollback** — ECHO is off while reading, and the termios state is
//!    restored on every ORDINARY path by a `Drop` guard ([`TtyEcho`]), including errors
//!    and unwinds. The exception is process death with no unwind — Ctrl-C's default
//!    SIGINT disposition — which skips Drop and leaves the terminal echo-less until the
//!    shell resets it (nothing secret is emitted; the cost is a confused owner). A
//!    crash that left the terminal echo-less would otherwise be both a usability bug
//!    and a security one, because the next thing the owner types would be invisible.
//! 5. **stdin redirection** — `mint < phrase.txt` would put the master in a file. The
//!    phrase is read from `/dev/tty` explicitly and refused if that is not a terminal, so
//!    such an invocation fails loudly rather than silently succeeding.
//! 6. **swap / hibernation image** — THE ONE THAT CANNOT BE FULLY CLOSED. [`MasterSeed`]
//!    and [`MasterPhrase`] `mlock(2)` their buffers, but `mlock` is best-effort under
//!    `RLIMIT_MEMLOCK` and is NOT claimed as a guarantee. Modern macOS encrypts swap by
//!    default, which mitigates without eliminating. The honest mitigation is the short
//!    residency window: from the last character read to the signature produced there is no
//!    user interaction, no network and no await point.
//! 7. **core dump** — [`forbid_core_dumps`] sets `RLIMIT_CORE` to 0 before the prompt.
//! 8. **logs / journal / `Debug`** — both secret types have hand-written `Debug` impls
//!    printing `<redacted>`, no `Serialize`, and no `Display`. Only [`fingerprint`] and
//!    the derived PUBLIC key are ever printable. This copies the discipline
//!    `aterm-release`'s `ReleaseCredentials` already established.
//! 9. **memory scrub** — hand-rolled, because there is no `zeroize` in this workspace. The
//!    SHAPE matters: both secrets are fixed arrays, never `String`/`Vec`. A growing
//!    `String` reallocates, and reallocation leaves copies of the phrase in freed heap
//!    that cannot be found and cannot be scrubbed. The scrub itself is `write_volatile`
//!    plus a `compiler_fence` so dead-store elimination cannot remove it.
//!    RESIDUAL, stated rather than hidden: `ring::Ed25519KeyPair` copies the seed
//!    internally and exposes no scrubbing hook. That copy outlives our scrub until the
//!    keypair drops, and we cannot reach it. ALSO RESIDUAL: Rust moves. Returning a
//!    `MasterPhrase`/`MasterSeed` by value (through `Result`, into a caller's frame) may
//!    memcpy the secret bytes to a new stack address; the `mlock` stays on the
//!    construction address and the moved-out bytes are not scrubbed. Drop runs at the
//!    final resting address only. Mitigations: copy elision usually avoids the move,
//!    adjacent frames usually share the locked page, and macOS encrypts swap — but this
//!    is a residual of the same kind as ring's copy, not a closed vector. Closing it
//!    would mean heap-pinning the secrets (Box::pin + one lock) — noted, not done.
//! 10. **the clipboard** — if the owner PASTES the phrase instead of typing it, it sits in
//!     the pasteboard afterwards. Nothing inside this tool can prevent that; the docs say
//!     so and recommend typing.
//! 12. **the terminal emulator's own persistence** — the phrase is shown on a terminal,
//!     and some terminals (aterm's seamless-update grid snapshots, iTerm2 session
//!     restore) write their grid to DISK. The ceremony docs therefore say: run
//!     `setup`/`join` in a terminal that does not persist its contents (macOS
//!     Terminal.app with session restore off is fine; aterm is NOT, while a seamless
//!     update can snapshot the grid), and clear scrollback after. This tool cannot
//!     close a vector that lives in whatever renders it.
//! 11. **stdout redirection — the OUTPUT side of vector 5, and the one this inventory
//!     originally missed.** Reading was scrupulous (`/dev/tty`, `isatty`, refuse otherwise)
//!     while the single deliberate WRITE went to fd 1, so `setup > log`, `| tee`, `| less`
//!     or any wrapper that captures stdout put the 64 hex characters into a file forever —
//!     and a wrapper that captured and discarded them destroyed the master instead, while
//!     the tool reported success. Both are closed the same way: the phrase is written to
//!     the [`Tty`] handle, which is `/dev/tty` proved with `isatty`, and a run with no
//!     terminal is REFUSED before a master is generated. A redirect can no longer capture
//!     it and a discarded stream can no longer swallow it.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{Ed25519KeyPair, KeyPair};

/// Exactly how many characters the written master is: 32 bytes in base32, spelled the
/// way it goes on paper.
pub const MASTER_PHRASE_LEN: usize = 52;

/// The 32 phrase characters: Crockford base32, lowercase — `0-9` and `a-z` minus
/// `i l o u`. Chosen for hands, not machines: nothing in it reads as anything else in it.
const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// Overwrite `buf` with zeroes in a way the optimizer may not delete.
///
/// A plain loop writing zeroes into a buffer that is never read again is textbook
/// dead-store elimination — the compiler is entitled to remove it, and at `-O2` it will.
/// `write_volatile` is not removable, and the fence stops the writes being sunk past the
/// point the buffer dies. This is the hand-rolled stand-in for `zeroize`, which this
/// workspace does not carry; if that ever changes, replace this and delete the comment,
/// but do NOT "simplify" it back into `buf.fill(0)`.
fn scrub(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        // SAFETY: `b` is a live, uniquely borrowed, correctly aligned `u8` obtained from
        // a mutable slice, so a one-byte volatile write to it is always in bounds and
        // never aliased.
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

/// Ask the kernel to keep `buf` out of swap. Best-effort by design and by necessity:
/// `mlock` is bounded by `RLIMIT_MEMLOCK` and can simply fail, so the return value is
/// deliberately ignored — a machine with a tight limit must still be able to mint a key.
/// This REDUCES the swap exposure in vector 6; it does not close it, and no caller should
/// read it as a guarantee.
fn try_lock(buf: &[u8]) {
    // SAFETY: `buf` is a live borrow, so its pointer and length describe mapped, readable
    // memory for the duration of the call. `mlock` neither reads nor writes the contents.
    unsafe {
        let _ = libc::mlock(buf.as_ptr().cast::<libc::c_void>(), buf.len());
    }
}

/// Undo [`try_lock`] before the memory is freed, so a long-running process does not leak
/// locked pages. Also best-effort, for the same reason.
fn try_unlock(buf: &[u8]) {
    // SAFETY: same live-borrow argument as `try_lock`; `munlock` on a range that was
    // never successfully locked is a harmless error return.
    unsafe {
        let _ = libc::munlock(buf.as_ptr().cast::<libc::c_void>(), buf.len());
    }
}

/// Set `RLIMIT_CORE` to zero, so a crash anywhere in the mint path cannot write the
/// master into a core file (leak vector 7). Called before the prompt, never after.
///
/// Best-effort and non-fatal: a hardened environment that already forbids raising it will
/// simply keep the stricter value, and there is no state where refusing to mint because
/// the limit could not be lowered would be the better outcome.
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

/// The 32 raw bytes of the paper master, held in a fixed array that is `mlock`ed on
/// construction and volatile-scrubbed on drop.
///
/// There is no `Clone`, no `Serialize`, no `Display`, and no accessor returning the
/// bytes: everything a caller can do with a master is derive its keypair, its public key,
/// or its fingerprint. That is the whole API surface on purpose — a `fn bytes(&self)`
/// here would immediately become a `write(path, seed.bytes())` somewhere else.
pub struct MasterSeed {
    bytes: [u8; 32],
}

impl std::fmt::Debug for MasterSeed {
    /// Hand-written so a derive can never print the master. The `ReleaseCredentials`
    /// treatment in `aterm-release/src/sign.rs`, copied deliberately rather than
    /// re-invented.
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

    /// The Ed25519 keypair this master IS. Deterministic: the same paper phrase always
    /// yields the same identity, which is what makes the phrase the master rather than a
    /// password to something stored elsewhere.
    ///
    /// `from_seed_unchecked` is the right call here and its name deserves a word: the
    /// "unchecked" part is that it does not verify the seed came from a CSPRNG. Ours did
    /// — [`generate_master`] is the only way one is created — and a seed read back from
    /// paper is by construction one that was generated that way.
    pub fn keypair(&self) -> Result<Ed25519KeyPair, String> {
        Ed25519KeyPair::from_seed_unchecked(&self.bytes)
            .map_err(|_| "master seed rejected by the Ed25519 implementation".to_string())
    }

    /// The master's base64 PUBLIC key — the value that goes into
    /// `pins::PAPER_MASTER_PUBKEYS`. Public identity only; safe to print, commit and log.
    pub fn pubkey_b64(&self) -> Result<String, String> {
        Ok(STANDARD.encode(self.keypair()?.public_key().as_ref()))
    }

    /// The short public [`fingerprint`] the owner eyeballs against the paper.
    pub fn fingerprint(&self) -> Result<String, String> {
        Ok(fingerprint(self.keypair()?.public_key().as_ref()))
    }

    /// Detached-sign `msg`'s exact bytes with the master. The ONLY thing a master signs is
    /// a roster; nothing here enforces that, but nothing else in the tree calls it either.
    pub fn sign(&self, msg: &[u8]) -> Result<Vec<u8>, String> {
        Ok(self.keypair()?.sign(msg).as_ref().to_vec())
    }
}

/// The written master, as the 52 base32 characters that go on paper.
///
/// Fixed `[u8; 52]`, never a `String`: see the module doc's leak vector 9 — a growing
/// `String` reallocates and strews unreachable copies of the phrase through freed heap.
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
    /// The phrase as text, for the ONE moment it is legitimately printed: generation.
    /// Always ASCII by construction, so the conversion cannot fail.
    ///
    /// This is the single most dangerous method here. It exists because the owner has to
    /// be able to read the phrase in order to write it down, and for no other reason.
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.chars).unwrap_or("")
    }

    /// Decode the phrase into the 32-byte seed. Infallible: the phrase can only have been
    /// built by [`generate_master`] or [`parse_master`], both of which have already
    /// proved every character is in the alphabet.
    ///
    /// 52 characters carry 260 bits; the final four are padding and are dropped here.
    /// That makes the last character's LOW bits non-canonical spelling — two spellings
    /// can name one master — while the identity itself stays exact, which is the property
    /// the fingerprint checks and the only one that matters.
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
        // `bytes` is a copy the `MasterSeed` now owns; scrub this stack copy too, and the
        // accumulator the master's bits streamed through. Without this the seed would
        // live in places only one of which gets cleaned.
        scrub(&mut bytes);
        scrub_u32(&mut acc);
        seed
    }
}

/// The 5-bit value of one phrase character. Only ever called on bytes already proved to
/// be in [`ALPHABET`], so the fallback is unreachable and returns 0 rather than panicking
/// — a panic here would abort mid-mint with the master live in memory.
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

/// Why a typed-back master phrase was refused. Neither variant carries any part of the
/// input — a position is safe to print, a character is not, because printing it puts a
/// byte of the master into the terminal scrollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhraseError {
    /// A character outside the alphabet, at this **1-based position** among the
    /// significant (non-whitespace, non-hyphen) characters — i.e. counting the way the
    /// owner counts the characters they wrote down, not the separators they typed.
    BadChar { position: usize },
    /// The right characters, the wrong number of them. `got` is the count of significant
    /// characters, so "51" and "53" are both immediately diagnosable.
    Length { got: usize },
}

impl PhraseError {
    /// The exact message the tool prints. Deliberately names the position and NEVER the
    /// character or its neighbours.
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

/// Parse a typed-back master phrase, fail-closed, forgiving, and diagnostic-first.
///
/// Forgiveness first, because the phrase is copied by hand twice: ASCII whitespace and
/// hyphens are stripped (the owner grouped the characters), case is folded, and the
/// misread trio maps to what it resembles — `o→0`, `i→1`, `l→1`. The alphabet excludes
/// those letters precisely so this mapping is unambiguous. After that the rules are
/// exact:
///
/// * every remaining character must be in [`ALPHABET`] — the FIRST that is not names its
///   1-based position, and nothing about its value;
/// * there must be exactly [`MASTER_PHRASE_LEN`] of them.
///
/// The character check runs BEFORE the length check on purpose. A phrase with one wrong
/// character AND one missing one is far more useful diagnosed as "character 37 is not a
/// phrase character" than as "51 characters", because the former points at the line to
/// re-read.
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
            // Scrub before returning: a partially filled buffer holds a prefix of the
            // master, and an error path is exactly where that is easiest to forget.
            scrub(&mut chars);
            return Err(PhraseError::BadChar { position: n + 1 });
        }
        if n < MASTER_PHRASE_LEN {
            // Count past the end so the length error reports the true count, but never
            // write past the array.
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

/// Generate a fresh master from the system CSPRNG. The ONLY way a master is created —
/// there is deliberately no "choose your own phrase" path (see the module doc).
pub fn generate_master() -> Result<MasterPhrase, String> {
    let mut raw = [0u8; 32];
    SystemRandom::new()
        .fill(&mut raw)
        .map_err(|_| "the system CSPRNG refused; refusing to mint a weak master".to_string())?;
    let mut chars = [0u8; MASTER_PHRASE_LEN];
    let mut acc: u32 = 0;
    let mut nbits: u32 = 0;
    let mut out = 0usize;
    // `for b in raw` would move a COPY of the whole seed into `array::IntoIter`'s
    // backing storage, which nothing scrubs (found by external audit 2026-08-15);
    // iterate by reference so the only seed copy is `raw`, which is scrubbed below.
    for &b in &raw {
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
    scrub(&mut raw);
    scrub_u32(&mut acc);
    let phrase = MasterPhrase { chars };
    try_lock(&phrase.chars);
    Ok(phrase)
}

/// The short verification fingerprint: the first 8 hex characters of SHA-256 over the
/// **public** key, hyphenated as `a3f2-9c1b`.
///
/// # Why the public key and never the secret
///
/// A fingerprint of secret material is a slow oracle against it — publish one and you
/// have handed an attacker a cheap check for candidate guesses. Fingerprinting the public
/// key gives the owner exactly the property they asked for (does what I typed back match
/// what I wrote down?) and gives an attacker nothing they could not compute anyway from a
/// public key that is committed in `pins.rs`.
///
/// Eight hex characters is 32 bits. That is not collision resistance and is not meant to
/// be: this defends against a HAND TRANSCRIPTION ERROR, an accidental adversary that
/// produces a uniformly random unrelated key, not against someone grinding for a match.
/// Short enough to compare at a glance is the property that matters, because a
/// fingerprint nobody actually reads defends nothing.
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

/// The termios ECHO guard: turns terminal echo OFF for the life of the value and restores
/// the ORIGINAL settings on drop — success, error, or unwind alike.
///
/// The `Drop` placement is the point. Restoring only on the happy path leaves a terminal
/// with no echo after any failure, which is a usability bug and a security one: the next
/// thing the owner types is invisible to them, so they cannot see that they are typing
/// into the wrong place.
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
        // SAFETY: `self.fd` is still open (this value borrows the caller's live `File`)
        // and `self.saved` is the initialized snapshot taken in `disable`.
        unsafe {
            let _ = libc::tcsetattr(self.fd, libc::TCSAFLUSH, &raw const self.saved);
        }
    }
}

/// The longest line the phrase prompt will read: 52 characters plus generous room for
/// the separators the owner groups them with, plus the newline. Anything longer is a
/// paste of the wrong thing, and truncating it produces a clean length error rather than
/// an unbounded read from a terminal.
const PROMPT_CAP: usize = 256;

/// THE TERMINAL, opened explicitly — the only place a master is ever read from and the
/// only place one is ever written to.
///
/// # Why an owned handle rather than "print it and hope"
///
/// The reading side always did this: `/dev/tty` is opened by name and `isatty`-checked, so
/// `join < phrase.txt` fails loudly instead of quietly taking the master out of a file
/// (leak vector 5). The WRITING side did not, and that asymmetry was a hole in both
/// directions (leak vector 11): a redirect captured the phrase into a file that outlives
/// the terminal, and a discarded stream swallowed it while the tool reported success —
/// which is worse, because the anchor it had just armed then named a master nobody held.
///
/// `/dev/tty` is the process's CONTROLLING TERMINAL, resolved by the kernel. It is not fd
/// 1, so no shell redirection can point it at a file, and a process with no controlling
/// terminal cannot open it at all. That is the property both directions want, so both
/// directions now go through this one handle.
pub struct Tty {
    file: std::fs::File,
    fd: std::os::unix::io::RawFd,
}

impl std::fmt::Debug for Tty {
    /// Hand-written and content-free. This value is the sink a master is written to; a
    /// derived impl would print a descriptor number, which is noise, and it sets the wrong
    /// precedent in a module where every other `Debug` is deliberately mute.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Tty(/dev/tty)")
    }
}

impl Tty {
    /// Open the controlling terminal, or refuse.
    ///
    /// Refusing is the point. A caller that is about to generate a master must know FIRST
    /// that it has somewhere to deliver it, because a master generated with nowhere to go
    /// is a master that never existed for anyone — and if the anchor has been armed by
    /// then, that is unrecoverable.
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

    /// WRITE THE PHRASE TO THE TERMINAL — the one deliberate emission of a secret in this
    /// tree, and the only one there will ever be.
    ///
    /// Two properties beyond "it prints":
    ///
    /// * It goes to the TERMINAL, so a redirect cannot capture it and a discarded stream
    ///   cannot swallow it (leak vector 11).
    /// * It is assembled in a fixed array that is `mlock`ed and volatile-scrubbed,
    ///   never a `String` — the CLI's old `push_str` + `push('\n')` grew the allocation
    ///   and freed the original with the phrase still in it, which is exactly the
    ///   reallocation leak vector 9 exists to forbid.
    ///
    /// The phrase is written in GROUPS OF FOUR — thirteen blocks the hand can copy
    /// without losing its place. The parser strips the spaces, so the grouping costs the
    /// owner nothing to type back.
    ///
    /// The flush is checked and its failure is returned, because "the phrase was printed"
    /// is a claim the caller acts on: `setup` arms a trust anchor on the strength of it.
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

    /// Write one non-secret line to the terminal — the warning that frames the phrase, and
    /// the fingerprint beside it.
    ///
    /// These go to the terminal too, and not because they are secret. A warning that says
    /// WRITE THE NEXT LINE DOWN is only true if it lands on the same surface as the line it
    /// is about; split across two streams, one of them redirected, it is an instruction
    /// pointing at nothing.
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

/// Prompt for the master phrase on `/dev/tty` with echo off, and parse it — one attempt.
///
/// The DOUBLE `Result` is the point of this variant: the outer error is the TERMINAL
/// failing (no tty, read error), the inner is the TYPED TEXT failing to parse. A caller
/// that retries — `setup`'s transcription gate — needs the distinction, because a dead
/// terminal must abort while a typo must not. An empty attempt (`Length` with `got: 0`)
/// is how EOF presents, so a retrying caller treats that as abort too, or a closed tty
/// would loop forever.
///
/// `/dev/tty` is opened EXPLICITLY rather than reading fd 0: that is what makes
/// `join < phrase.txt` fail loudly instead of quietly succeeding and leaving the
/// master in a file (leak vector 5). If the process has no controlling terminal, this
/// refuses — there is no non-interactive path to a master by design.
pub fn prompt_master_attempt(prompt: &str) -> Result<Result<MasterPhrase, PhraseError>, String> {
    use std::io::{Read as _, Write as _};

    let handle = Tty::open()?;
    let fd = handle.fd;
    let mut tty = handle.file;
    // Echo goes off BEFORE the prompt is written, so there is no window in which a fast
    // typist's first characters are echoed.
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

    // `from_utf8_lossy` would ALLOCATE A COPY of the phrase on any non-ASCII byte, and
    // that copy is a `Cow` we could not reliably scrub. Parse the bytes we have: the
    // phrase is ASCII by definition, so a non-ASCII byte is simply not a phrase character
    // and `parse_master` will name its position — which is the right diagnosis anyway.
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

    // An obviously synthetic phrase: the alphabet in order, then filler, ending on the
    // canonical '0' tail. It never appears outside this test module.
    const SYNTHETIC: &str = "0123456789abcdefghjkmnpqrstvwxyz0123456789abcdefghj0";

    /// The refusal for an input that must not parse. A helper rather than `assert_eq!`
    /// over the `Result`, because [`MasterPhrase`] deliberately has no `PartialEq`: an
    /// equality operator on secret material is both an invitation to compare it and a
    /// timing side channel waiting to be written.
    fn refused(input: &str) -> PhraseError {
        // `expect_err` (not `.err().expect()`) — it needs `Debug` on the Ok type, which
        // `MasterPhrase` has and which prints `<redacted>`, so a failure here still
        // cannot spill phrase bytes into the panic message.
        parse_master(input).expect_err("this input must be refused")
    }

    /// EXACTLY 52 alphabet characters is accepted, and case folds.
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

    /// THE MISREAD TRIO IS FORGIVEN: `o` reads as `0`, `i` and `l` read as `1` — the
    /// alphabet excludes those letters precisely so the mapping is unambiguous.
    #[test]
    fn o_i_and_l_are_read_as_the_characters_they_resemble() {
        let mut misread = String::from(SYNTHETIC);
        misread = misread.replacen('0', "o", 1);
        misread = misread.replacen('1', "i", 2);
        misread = misread.replacen('1', "l", 1);
        assert_ne!(misread, SYNTHETIC, "the fixture must actually differ");
        assert_eq!(
            parse_master(&misread).unwrap().seed().pubkey_b64().unwrap(),
            parse_master(SYNTHETIC).unwrap().seed().pubkey_b64().unwrap(),
            "a misread spelling must name the same master"
        );
    }

    /// 51 and 53 are both refused, and the message reports the true count so the owner
    /// knows which direction they are out.
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

    /// A BAD CHARACTER names its 1-based position — and the message never contains the
    /// character itself, which would put a byte of the master in the scrollback.
    #[test]
    fn a_bad_character_is_refused_by_position_and_never_echoed() {
        let mut bad = String::from(SYNTHETIC);
        // `u` is deliberately outside the alphabet and is NOT mapped to anything.
        bad.replace_range(36..37, "u");
        assert_eq!(
            refused(&bad),
            PhraseError::BadChar { position: 37 },
            "the 37th character (1-based) is the bad one"
        );
        let msg = refused(&bad).message();
        assert!(msg.contains("character 37"), "{msg}");
        // The neighbouring GOOD characters must not leak either — the message may only
        // discuss the position.
        assert!(!msg.contains(&bad[30..40]), "{msg}");

        // First and last positions, so the 1-based arithmetic is pinned at both ends.
        let mut first = String::from(SYNTHETIC);
        first.replace_range(0..1, "!");
        assert_eq!(refused(&first), PhraseError::BadChar { position: 1 });
        let mut last = String::from(SYNTHETIC);
        last.replace_range(51..52, "u");
        assert_eq!(refused(&last), PhraseError::BadChar { position: 52 });
    }

    /// The character check runs BEFORE the length check: a phrase that is both wrong and
    /// short is diagnosed by the character, which is what points at the line to re-read.
    #[test]
    fn a_bad_character_is_reported_even_when_the_length_is_also_wrong() {
        let mut bad = String::from(&SYNTHETIC[..40]);
        bad.replace_range(9..10, "u");
        assert_eq!(refused(&bad), PhraseError::BadChar { position: 10 });
    }

    /// Whitespace and hyphens are stripped, because the owner writes the phrase in
    /// groups — and the position it reports counts the SIGNIFICANT characters, the way
    /// the owner counts the ones they wrote, not the separators they typed between them.
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

    /// A generated master is 52 lowercase alphabet characters, and two generations
    /// differ — the CSPRNG is actually being consulted, not a constant returned.
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
        // ...and a generated phrase round-trips through the reader the owner will use.
        assert_eq!(parse_master(a.as_str()).unwrap().as_str(), a.as_str());
    }

    /// The final character's LOW FOUR BITS are padding: spellings that differ only there
    /// name the same master (the fingerprint agrees), while its top bit is significant.
    #[test]
    fn the_padding_bits_of_the_last_character_are_masked() {
        let base = parse_master(SYNTHETIC).unwrap().seed().pubkey_b64().unwrap();
        // '7' = 0b00111: same top bit as '0', different padding bits — same master.
        let mut pad = String::from(SYNTHETIC);
        pad.replace_range(51..52, "7");
        assert_eq!(parse_master(&pad).unwrap().seed().pubkey_b64().unwrap(), base);
        // 'g' = 0b10000: different top bit — different master.
        let mut sig = String::from(SYNTHETIC);
        sig.replace_range(51..52, "g");
        assert_ne!(parse_master(&sig).unwrap().seed().pubkey_b64().unwrap(), base);
    }

    /// THE FINGERPRINT'S JOB: same key ⇒ same fingerprint, different key ⇒ different, and
    /// the shape is the short hyphenated form a human can compare at a glance.
    #[test]
    fn the_fingerprint_matches_on_a_correct_transcription_and_differs_otherwise() {
        let phrase = parse_master(SYNTHETIC).unwrap();
        let fp = phrase.seed().fingerprint().unwrap();
        assert_eq!(fp.len(), 9, "8 hex characters and one hyphen: {fp}");
        assert_eq!(&fp[4..5], "-", "{fp}");

        // A correct type-back reproduces it — this is the eyeball check.
        let typed_back = parse_master(
            "0123 4567 89ab cdef ghjk mnpq rstv wxyz 0123 4567 89ab cdef ghj0",
        )
        .unwrap();
        assert_eq!(typed_back.seed().fingerprint().unwrap(), fp);

        // A ONE-CHARACTER mistranscription changes it. This is the whole reason the
        // fingerprint exists, so it is asserted rather than assumed.
        let mut slip = String::from(SYNTHETIC);
        slip.replace_range(17..18, "7");
        assert_ne!(&slip, SYNTHETIC, "the fixture must actually differ");
        assert_ne!(
            parse_master(&slip).unwrap().seed().fingerprint().unwrap(),
            fp,
            "a single wrong character must change the fingerprint"
        );
    }

    /// The fingerprint is over the PUBLIC key. Fingerprinting the secret would be a slow
    /// oracle against it, so this pins which input is used: `fingerprint(pubkey)` must
    /// equal what `MasterSeed::fingerprint` returns.
    #[test]
    fn the_fingerprint_is_taken_over_the_public_key() {
        let seed = parse_master(SYNTHETIC).unwrap().seed();
        let kp = seed.keypair().unwrap();
        assert_eq!(
            seed.fingerprint().unwrap(),
            fingerprint(kp.public_key().as_ref())
        );
        // And the SEED's own bytes produce a different value, proving the public key is
        // what was hashed rather than the secret happening to agree.
        let raw_seed_fp = fingerprint(&[0x01u8; 32]);
        assert_ne!(seed.fingerprint().unwrap(), raw_seed_fp);
    }

    /// The master is deterministic: the paper phrase alone reproduces the identity, with
    /// no stored state. That is what makes the paper the master.
    #[test]
    fn the_same_phrase_always_derives_the_same_identity() {
        let a = parse_master(SYNTHETIC)
            .unwrap()
            .seed()
            .pubkey_b64()
            .unwrap();
        let b = parse_master(SYNTHETIC)
            .unwrap()
            .seed()
            .pubkey_b64()
            .unwrap();
        assert_eq!(a, b);
        assert_eq!(STANDARD.decode(&a).unwrap().len(), 32);
        // A different phrase is a different master.
        let other = parse_master(&SYNTHETIC.replace('3', "x")).unwrap();
        assert_ne!(other.seed().pubkey_b64().unwrap(), a);
    }

    /// SECRETS ARE NOT PRINTABLE. A stray `{:?}` on either secret type prints `<redacted>`
    /// and nothing else — the `sign.rs` discipline, enforced here by test rather than by
    /// hoping nobody adds a derive.
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

    /// The scrub is not a no-op: a buffer holding the phrase is all zeroes afterwards.
    /// (This tests the primitive, which is the only part of vector 9 that is testable —
    /// whether the optimizer honours `write_volatile` is the compiler's contract.)
    #[test]
    fn scrub_zeroes_the_buffer() {
        let mut buf = *b"0123456789abcdef";
        assert!(buf.iter().any(|b| *b != 0), "the fixture must start dirty");
        scrub(&mut buf);
        assert!(buf.iter().all(|b| *b == 0));
    }
}
