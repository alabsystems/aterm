// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Paper master phrase generation, terminal prompting, and Ed25519 key derivation.
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{Ed25519KeyPair, KeyPair};

/// Number of base32 characters in the written master phrase.
pub const MASTER_PHRASE_LEN: usize = 52;

/// The phrase alphabet: lowercase Crockford base32, `0-9a-z` minus the easily confused
/// `i l o u`.
const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// Overwrite `buf` with zeroes the optimizer may not delete. A zeroing loop on a buffer never
/// read again is dead-store elimination's textbook case; `write_volatile` cannot be removed
/// and the fence stops the writes sinking past the buffer's death.
fn scrub(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        // SAFETY: `b` is a live, uniquely borrowed, aligned `u8`.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

/// [`scrub`] for the one-word bit accumulators the base32 codec threads master bits
/// through.
fn scrub_u32(v: &mut u32) {
    // SAFETY: `v` is a live, uniquely borrowed, aligned `u32`.
    unsafe { std::ptr::write_volatile(v, 0) };
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

/// Ask the kernel to keep `buf` out of swap. Best-effort: `mlock` is bounded by
/// `RLIMIT_MEMLOCK` and its failure is ignored, so a machine with a tight limit can still
/// mint.
fn try_lock(buf: &[u8]) {
    // SAFETY: `buf` points to `buf.len()` live bytes for the duration of the `mlock` call.
    unsafe {
        let _ = libc::mlock(buf.as_ptr().cast::<libc::c_void>(), buf.len());
    }
}

/// Undo [`try_lock`] before the memory is freed, so a long-running process does not leak
/// locked pages.
fn try_unlock(buf: &[u8]) {
    // SAFETY: `buf` points to `buf.len()` live bytes for the duration of the `munlock` call.
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
    // SAFETY: `zero` is an initialized `rlimit` valid for `RLIMIT_CORE`.
    unsafe {
        let _ = libc::setrlimit(libc::RLIMIT_CORE, &raw const zero);
    }
}

/// Raw 32-byte paper master seed, `mlock`ed on creation and volatile-scrubbed on drop.
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
        scrub(&mut bytes);
        scrub_u32(&mut acc);
        seed
    }
}

/// The 5-bit value of one phrase character. Called only on alphabet bytes; the unreachable
/// fallback returns 0 rather than panicking with the master live in memory.
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

/// Spell 32 seed bytes as the 52-character phrase [`MasterPhrase::seed`] decodes back to
/// exactly those bytes.
fn encode_phrase(raw: &[u8; 32]) -> MasterPhrase {
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
    /// Disable ECHO on `fd`, remembering the previous state.
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
        // SAFETY: `self.fd` outlives this guard and `self.saved` was initialized in `disable`.
        unsafe {
            let _ = libc::tcsetattr(self.fd, libc::TCSAFLUSH, &raw const self.saved);
        }
    }
}

/// The longest line the phrase prompt reads: 52 characters plus room for separators and the
/// newline.
const PROMPT_CAP: usize = 256;

/// Controlling terminal (`/dev/tty`) used to read and display the master phrase.
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
    /// Opens `/dev/tty` and verifies it is a terminal.
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
        // SAFETY: `fd` is an open descriptor owned by `file`.
        if unsafe { libc::isatty(fd) } != 1 {
            return Err("/dev/tty is not a terminal; refusing to handle the master".to_string());
        }
        Ok(Self { file, fd })
    }

    /// Writes the formatted master phrase to `/dev/tty`.
    pub fn write_phrase(&mut self, phrase: &MasterPhrase) -> Result<(), String> {
        use std::io::Write as _;
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
    let _ = tty.write_all(b"\n");

    let text = std::str::from_utf8(&buf[..used]).unwrap_or("");
    let parsed = parse_master(text);
    scrub(&mut buf);
    try_unlock(&buf);
    Ok(parsed)
}

/// Prompts for the master phrase via [`prompt_master_attempt`], flattening parse errors.
pub fn prompt_for_master(prompt: &str) -> Result<MasterPhrase, String> {
    match prompt_master_attempt(prompt)? {
        Ok(phrase) => Ok(phrase),
        Err(e) => Err(e.message()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYNTHETIC: &str = "0123456789abcdefghjkmnpqrstvwxyz0123456789abcdefghj0";

    /// The refusal for an input that must not parse. [`MasterPhrase`] has no `PartialEq` on
    /// purpose: equality on secret material invites comparison and timing side channels.
    fn refused(input: &str) -> PhraseError {
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

        let flood = SYNTHETIC.repeat(4);
        assert_eq!(refused(&flood), PhraseError::Length { got: 208 });
        assert_eq!(refused(""), PhraseError::Length { got: 0 });
    }

    #[test]
    fn a_bad_character_is_refused_by_position_and_never_echoed() {
        let mut bad = String::from(SYNTHETIC);
        bad.replace_range(36..37, "u");
        assert_eq!(
            refused(&bad),
            PhraseError::BadChar { position: 37 },
            "the 37th character (1-based) is the bad one"
        );
        let msg = refused(&bad).message();
        assert!(msg.contains("character 37"), "{msg}");
        assert!(!msg.contains(&bad[30..40]), "{msg}");

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
        assert_eq!(parse_master(a.as_str()).unwrap().as_str(), a.as_str());
    }

    /// THE PAPER IS THE SEED. A phrase decodes to exactly the 32 bytes it was spelled
    /// from, its last character carries zero padding bits (one canonical spelling), and
    /// the derivation is a cross-version contract: paper written under an earlier build
    /// names the same master in this one.
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
        let mut pad = String::from(SYNTHETIC);
        pad.replace_range(51..52, "7");
        assert_eq!(
            parse_master(&pad).unwrap().seed().pubkey_b64().unwrap(),
            base
        );
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

        let typed_back =
            parse_master("0123 4567 89ab cdef ghjk mnpq rstv wxyz 0123 4567 89ab cdef ghj0")
                .unwrap();
        assert_eq!(typed_back.seed().fingerprint().unwrap(), fp);

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
