// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Paper master phrase generation, its vault store, and Ed25519 key derivation. The vault is the
//! phrase's one store ([`aterm_secrets::paper_master`]): [`from_vault`] reads it, and
//! [`MasterPhrase::store_in_vault`] writes a new one; no path prints it or reads it from a
//! terminal, argv, the environment or stdin.
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{Ed25519KeyPair, KeyPair};

/// Number of base32 characters in the master phrase.
pub const MASTER_PHRASE_LEN: usize = aterm_secrets::paper_master::LEN;

const ALPHABET: &[u8; 32] = aterm_secrets::paper_master::ALPHABET;

/// Reason a master phrase was refused; it holds no symbol of the phrase.
pub use aterm_secrets::paper_master::PhraseError;

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

/// The 52-character base32 master phrase, `mlock`ed and volatile-scrubbed on drop.
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

    /// Writes this new phrase into the vault ([`aterm_secrets::paper_master::store`]), the one
    /// place it is kept.
    ///
    /// # Errors
    /// The vault refused it: this Mac cannot publish as the vault's admin, or the vault already
    /// holds a phrase.
    pub fn store_in_vault(&self, paths: &aterm_secrets::client::Paths) -> Result<(), String> {
        aterm_secrets::paper_master::store(paths, &self.chars).map_err(|e| {
            let mut s = String::from("the vault did not take the new master phrase: ");
            s.push_str(&e.to_string());
            s
        })
    }
}

/// The master phrase from the vault ([`aterm_secrets::paper_master::read`], which holds the
/// value to the grammar).
///
/// # Errors
/// This Mac holds no usable phrase: the line names the `aterm secrets need paper-master`
/// request it filed.
pub fn from_vault(paths: &aterm_secrets::client::Paths) -> Result<MasterPhrase, String> {
    let value = aterm_secrets::paper_master::read(paths)?;
    from_symbols(value.as_slice()).map_err(|e| e.message())
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

/// Parses and normalizes a master phrase ([`aterm_secrets::paper_master::symbols_into`]).
pub fn parse_master(input: &str) -> Result<MasterPhrase, PhraseError> {
    from_symbols(input.as_bytes())
}

/// The one constructor of a [`MasterPhrase`] from bytes: its symbols, in locked memory.
fn from_symbols(input: &[u8]) -> Result<MasterPhrase, PhraseError> {
    let mut phrase = MasterPhrase {
        chars: [0u8; MASTER_PHRASE_LEN],
    };
    try_lock(&phrase.chars);
    aterm_secrets::paper_master::symbols_into(input, &mut phrase.chars)?;
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

#[cfg(test)]
#[path = "master_tests.rs"]
mod tests;
