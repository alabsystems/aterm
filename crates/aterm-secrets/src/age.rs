// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! age v1 (C2SP age.md) with X25519 recipients: the file format every locker of a vault is.
//!
//! [`encrypt`] makes a binary age file; [`armor`] wraps it in the ASCII armor the envelope
//! carries; [`decrypt`] reads either. Only `X25519` stanzas are understood: a stanza of
//! another type is skipped, as the spec requires, and an `scrypt` stanza next to another is
//! a header failure. Decryption releases no plaintext unless every chunk authenticated.
//!
//! The errors are the C2SP CCTV vector classes, so the vectors are held to them exactly
//! (`tests/cctv.rs`, 102 vectors). The reader follows the reference implementation
//! (filippo.io/age) wherever the spec leaves a choice: the header grammar of its
//! `internal/format`, the stanza rules of its `x25519.go`, the STREAM reader of its
//! `internal/stream` and the armor reader of its `armor.go`, in the same order, so a file
//! that fails two ways fails with the class the reference reports first:
//!
//! 1. Armor ([`AgeError::Armor`]): a file that does not begin `age-encryption.org/` is read
//!    as armor, and the whole armor is checked before anything inside it is parsed.
//! 2. Header grammar ([`AgeError::Header`]), including an empty file and one over
//!    [`MAX_FILE`] or with a header over [`MAX_HEADER`].
//! 3. An `scrypt` stanza beside any other ([`AgeError::Header`]).
//! 4. The identities, each against every stanza in order: a malformed `X25519` stanza (not
//!    exactly one argument, a share that is not canonical base64 of 32 bytes, a body that is
//!    not 32 bytes) or an all-zero shared secret is [`AgeError::Header`]; no stanza opening
//!    is [`AgeError::NoMatch`]. A stanza whose type is not exactly `X25519` (`x25519`
//!    included) is skipped, which is why CCTV's `x25519_lowercase` is "no match".
//! 5. The header MAC ([`AgeError::Hmac`]).
//! 6. A payload nonce shorter than 16 bytes ([`AgeError::Header`], as CCTV classes it).
//! 7. The STREAM payload ([`AgeError::Payload`]): a chunk that does not authenticate, no
//!    final chunk, an empty final chunk after a full one, or bytes after the final chunk.
//!
//! CCTV's `payload failure` vectors also give the hash of what a streaming reader released
//! before the failure; this reader releases nothing then, so only `success` hashes apply.

use crate::{SecretBytes, b64, bech32, x25519};
use ring::{aead, hkdf, hmac};
use zeroize::Zeroize;

/// Why an age operation failed. Deliberately coarse: it never says which byte or which key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgeError {
    /// No stanza opened with the identities given.
    NoMatch,
    /// The header is malformed (grammar, base64, a bad X25519 stanza, an all-zero shared
    /// secret, a missing or short payload nonce).
    Header,
    /// The header MAC does not verify.
    Hmac,
    /// A payload chunk failed, the final chunk is missing, or bytes follow it.
    Payload,
    /// The ASCII armor is malformed.
    Armor,
    /// A recipient or identity string is malformed.
    Key,
}

impl std::fmt::Display for AgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            AgeError::NoMatch => "age: no identity matches this file",
            AgeError::Header => "age: malformed header",
            AgeError::Hmac => "age: header MAC does not verify",
            AgeError::Payload => "age: payload does not authenticate",
            AgeError::Armor => "age: malformed armor",
            AgeError::Key => "age: malformed key",
        })
    }
}

impl std::error::Error for AgeError {}

/// The largest binary age file [`decrypt`] reads, and [`encrypt`] makes: 64 MiB. A vault
/// envelope is at most 16 MiB, and CCTV's largest vector (258 chunks) is 16.1 MiB.
pub const MAX_FILE: usize = 64 << 20;

/// The largest header [`decrypt`] reads, and [`encrypt`] makes: 1 MiB (about 9,000 X25519
/// recipients).
pub const MAX_HEADER: usize = 1 << 20;

/// The largest armored file [`dearmor`] reads: [`MAX_FILE`] at 48 bytes per 64-column line
/// with CRLF ends, plus the BEGIN and END lines and the whitespace allowed around them.
pub const MAX_ARMORED: usize = MAX_FILE.div_ceil(BYTES_PER_LINE) * (COLUMNS + 2) + 4096;

/// What every binary age file starts with (the version line's fixed part).
const INTRO: &[u8] = b"age-encryption.org/";
/// The v1 version line, without its LF.
const VERSION_LINE: &[u8] = b"age-encryption.org/v1";
/// The X25519 stanza's HKDF info.
const X25519_LABEL: &[u8] = b"age-encryption.org/v1/X25519";
const ARMOR_BEGIN: &[u8] = b"-----BEGIN AGE ENCRYPTED FILE-----";
const ARMOR_END: &[u8] = b"-----END AGE ENCRYPTED FILE-----";
/// The armor's (and a stanza body's) line: 64 columns of base64, 48 bytes.
const COLUMNS: usize = 64;
const BYTES_PER_LINE: usize = 48;
/// The whitespace the armor reader allows before (and, strictly less, after) the block.
const ARMOR_WHITESPACE: usize = 1024;
/// STREAM's plaintext chunk.
const CHUNK: usize = 64 * 1024;
/// ChaCha20-Poly1305's tag.
const TAG: usize = 16;
/// The payload nonce.
const PAYLOAD_NONCE: usize = 16;
/// The file key.
const FILE_KEY: usize = 16;

/// An age X25519 identity: a machine key. Wiped on drop; `Debug` shows only its recipient.
pub struct Identity {
    secret: [u8; 32],
    /// X25519(secret, 9), computed once: every unwrap salts with it.
    public: [u8; 32],
}

impl Identity {
    /// A fresh identity from the OS CSPRNG (`aterm_uds::rand::fill`).
    pub fn generate() -> std::io::Result<Identity> {
        let mut identity = Identity {
            secret: [0u8; 32],
            public: [0u8; 32],
        };
        aterm_uds::rand::fill(&mut identity.secret)?;
        identity.public = x25519::x25519_base(&identity.secret);
        Ok(identity)
    }

    /// The identity with these 32 secret bytes.
    pub fn from_secret_bytes(secret: [u8; 32]) -> Identity {
        Identity {
            public: x25519::x25519_base(&secret),
            secret,
        }
    }

    /// Parse `AGE-SECRET-KEY-1…` (all-uppercase, HRP exactly `AGE-SECRET-KEY-`, 32 bytes).
    pub fn parse(text: &str) -> Result<Identity, AgeError> {
        let mut decoded = bech32::decode(text).ok_or(AgeError::Key)?;
        let result =
            if decoded.uppercase && decoded.hrp == "age-secret-key-" && decoded.data.len() == 32 {
                let mut identity = Identity {
                    secret: [0u8; 32],
                    public: [0u8; 32],
                };
                identity.secret.copy_from_slice(&decoded.data);
                identity.public = x25519::x25519_base(&identity.secret);
                Ok(identity)
            } else {
                Err(AgeError::Key)
            };
        decoded.data.zeroize();
        result
    }

    /// The `AGE-SECRET-KEY-1…` spelling, uppercase, as a secret.
    pub fn to_bech32(&self) -> SecretBytes {
        let mut text = bech32::encode("age-secret-key-", &self.secret);
        text.make_ascii_uppercase();
        SecretBytes::from(text)
    }

    /// The recipient (public key) of this identity.
    pub fn recipient(&self) -> Recipient {
        Recipient(self.public)
    }

    /// The 32 secret bytes (for the X25519 unwrap).
    pub fn secret_bytes(&self) -> &[u8; 32] {
        &self.secret
    }
}

impl Drop for Identity {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.secret);
    }
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Identity({})", self.recipient().to_bech32())
    }
}

/// An age X25519 recipient: a machine's public key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Recipient(pub [u8; 32]);

impl Recipient {
    /// Parse `age1…` (all-lowercase, HRP exactly `age`, 32 bytes, not a low-order point:
    /// [`encrypt`] could never wrap a file key to one, so a vault never names one).
    pub fn parse(text: &str) -> Result<Recipient, AgeError> {
        let decoded = bech32::decode(text).ok_or(AgeError::Key)?;
        if decoded.uppercase || decoded.hrp != "age" {
            return Err(AgeError::Key);
        }
        let key: [u8; 32] = decoded.data.try_into().map_err(|_| AgeError::Key)?;
        if low_order(&key) {
            return Err(AgeError::Key);
        }
        Ok(Recipient(key))
    }

    /// The `age1…` spelling.
    pub fn to_bech32(&self) -> String {
        bech32::encode("age", &self.0)
    }
}

/// True when `point` is a low-order u-coordinate, one whose X25519 with every scalar is all
/// zero: a point of order dividing 8 on the curve or 4 on its twist, in any of its spellings
/// (the top bit set, or u ≥ p). One scalar decides for all: a clamped scalar is 8 times a
/// number below both large prime orders, so it sends every other point to a point of large
/// odd order, never to the identity or the point of order 2 (the two the ladder spells as
/// zero). The point is public, so the probe need not be constant time.
fn low_order(point: &[u8; 32]) -> bool {
    x25519::x25519(&[0x55; 32], point) == [0u8; 32]
}

/// Encrypt `plaintext` to every recipient (at least one): a fresh 16-byte file key, one
/// X25519 stanza per recipient with a fresh ephemeral key each, the header MAC, a fresh
/// 16-byte payload nonce and the STREAM payload. Binary age file.
///
/// Refused (`InvalidInput`): no recipients, a recipient that is a low-order point (the
/// shared secret would be all zero, which every reader refuses), and a file or header over
/// [`MAX_FILE`] or [`MAX_HEADER`], which [`decrypt`] would refuse.
pub fn encrypt(recipients: &[Recipient], plaintext: &[u8]) -> std::io::Result<Vec<u8>> {
    if recipients.is_empty() {
        return Err(invalid_input("age: no recipients"));
    }
    if plaintext.len() > MAX_FILE {
        return Err(invalid_input("age: plaintext too large"));
    }
    let mut file_key = [0u8; FILE_KEY];
    aterm_uds::rand::fill(&mut file_key)?;
    let result = encrypt_with_file_key(recipients, plaintext, &file_key);
    file_key.zeroize();
    result
}

fn invalid_input(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message.to_string())
}

fn encrypt_with_file_key(
    recipients: &[Recipient],
    plaintext: &[u8],
    file_key: &[u8; FILE_KEY],
) -> std::io::Result<Vec<u8>> {
    let mut header = Vec::with_capacity(VERSION_LINE.len() + 1 + recipients.len() * 100 + 48);
    header.extend_from_slice(VERSION_LINE);
    header.push(b'\n');
    for recipient in recipients {
        let (share, body) = wrap(recipient, file_key)?;
        header.extend_from_slice(b"-> X25519 ");
        header.extend_from_slice(b64::encode_nopad(&share).as_bytes());
        header.push(b'\n');
        push_wrapped(&mut header, &b64::encode_nopad(&body));
    }
    header.extend_from_slice(b"---");
    let mac = hmac::sign(&header_key(file_key), &header);
    header.push(b' ');
    header.extend_from_slice(b64::encode_nopad(mac.as_ref()).as_bytes());
    header.push(b'\n');
    if header.len() > MAX_HEADER {
        return Err(invalid_input("age: too many recipients"));
    }

    let chunks = plaintext.len().div_ceil(CHUNK).max(1);
    let total = header.len() + PAYLOAD_NONCE + plaintext.len() + chunks * TAG;
    if total > MAX_FILE {
        return Err(invalid_input("age: plaintext too large"));
    }
    let mut nonce = [0u8; PAYLOAD_NONCE];
    aterm_uds::rand::fill(&mut nonce)?;
    let key = payload_key(file_key, &nonce);

    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&header);
    out.extend_from_slice(&nonce);
    for index in 0..chunks {
        let start = index * CHUNK;
        let end = plaintext.len().min(start + CHUNK);
        let at = out.len();
        // The chunk is sealed in place, so `out` never holds it in the clear past this call.
        out.extend_from_slice(&plaintext[start..end]);
        let tag = key
            .seal_in_place_separate_tag(
                stream_nonce(index as u64, index + 1 == chunks),
                aead::Aad::empty(),
                &mut out[at..],
            )
            .map_err(|_| std::io::Error::other("age: payload seal failed"))?;
        out.extend_from_slice(tag.as_ref());
    }
    debug_assert_eq!(out.len(), total);
    Ok(out)
}

/// One X25519 stanza: (ephemeral share, body) for `recipient`.
fn wrap(
    recipient: &Recipient,
    file_key: &[u8; FILE_KEY],
) -> std::io::Result<([u8; 32], [u8; FILE_KEY + TAG])> {
    let mut ephemeral = [0u8; 32];
    aterm_uds::rand::fill(&mut ephemeral)?;
    let share = x25519::x25519_base(&ephemeral);
    let mut shared = x25519::x25519(&ephemeral, &recipient.0);
    ephemeral.zeroize();
    if aterm_digest::ct_eq(&shared, &[0u8; 32]) {
        return Err(invalid_input("age: recipient is a low-order point"));
    }
    let key = wrap_key(&shared, &share, &recipient.0);
    shared.zeroize();
    let mut body = [0u8; FILE_KEY + TAG];
    body[..FILE_KEY].copy_from_slice(file_key);
    match key.seal_in_place_separate_tag(zero_nonce(), aead::Aad::empty(), &mut body[..FILE_KEY]) {
        Ok(tag) => {
            body[FILE_KEY..].copy_from_slice(tag.as_ref());
            Ok((share, body))
        }
        Err(_) => {
            body.zeroize();
            Err(std::io::Error::other("age: stanza seal failed"))
        }
    }
}

/// A stanza body wrapped at 64 columns; a body whose encoding fills its last line (or is
/// empty) ends with an empty line, so the stanza always ends with a short one.
fn push_wrapped(header: &mut Vec<u8>, encoded: &str) {
    let bytes = encoded.as_bytes();
    for line in bytes.chunks(COLUMNS) {
        header.extend_from_slice(line);
        header.push(b'\n');
    }
    if bytes.len().is_multiple_of(COLUMNS) {
        header.push(b'\n');
    }
}

/// HKDF-SHA-256(ikm, salt, info) as a ChaCha20-Poly1305 key.
fn hkdf_aead_key(ikm: &[u8], salt: &[u8], info: &[u8]) -> aead::LessSafeKey {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(ikm);
    let info = [info];
    let okm = prk
        .expand(&info, &aead::CHACHA20_POLY1305)
        .expect("32 bytes is within HKDF-SHA-256's output bound");
    aead::LessSafeKey::new(aead::UnboundKey::from(okm))
}

/// The X25519 stanza's wrap key: HKDF(shared, share || recipient, label).
fn wrap_key(shared: &[u8; 32], share: &[u8; 32], recipient: &[u8; 32]) -> aead::LessSafeKey {
    let mut salt = [0u8; 64];
    salt[..32].copy_from_slice(share);
    salt[32..].copy_from_slice(recipient);
    hkdf_aead_key(shared, &salt, X25519_LABEL)
}

/// The header MAC key: HKDF(file key, empty salt, "header").
fn header_key(file_key: &[u8; FILE_KEY]) -> hmac::Key {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]).extract(file_key);
    let info = [b"header".as_slice()];
    let okm = prk
        .expand(&info, hmac::HMAC_SHA256)
        .expect("32 bytes is within HKDF-SHA-256's output bound");
    hmac::Key::from(okm)
}

/// The payload key: HKDF(file key, nonce, "payload").
fn payload_key(file_key: &[u8; FILE_KEY], nonce: &[u8]) -> aead::LessSafeKey {
    hkdf_aead_key(file_key, nonce, b"payload")
}

fn zero_nonce() -> aead::Nonce {
    aead::Nonce::assume_unique_for_key([0u8; 12])
}

/// STREAM's chunk nonce: an 11-byte big-endian counter, then 1 on the final chunk.
fn stream_nonce(counter: u64, last: bool) -> aead::Nonce {
    let mut nonce = [0u8; 12];
    nonce[3..11].copy_from_slice(&counter.to_be_bytes());
    nonce[11] = u8::from(last);
    aead::Nonce::assume_unique_for_key(nonce)
}

/// Decrypt an age file, binary or armored, with the first identity that opens a stanza.
/// All-or-nothing: no plaintext unless the whole payload authenticated. A file that does
/// not begin with age's version line (`age-encryption.org/`) is read as armor (whitespace
/// around an armor is allowed as [`dearmor`] says), so anything before a binary file's
/// version line is an armor failure; an empty file is a header failure. The order of the
/// checks, and so which class a file that fails twice gets, is the module's.
pub fn decrypt(identities: &[Identity], file: &[u8]) -> Result<SecretBytes, AgeError> {
    if file.is_empty() || file.starts_with(INTRO) {
        decrypt_binary(identities, file)
    } else {
        let binary = dearmor(file)?;
        decrypt_binary(identities, &binary)
    }
}

/// A parsed recipient stanza.
struct Stanza<'a> {
    kind: &'a str,
    args: Vec<&'a str>,
    body: Vec<u8>,
}

/// A parsed header.
struct Header<'a> {
    stanzas: Vec<Stanza<'a>>,
    mac: [u8; 32],
    /// The bytes the MAC covers: through the `---` of the MAC line.
    mac_covers: usize,
    /// The header's length, its final LF included: where the payload starts.
    len: usize,
}

/// LF-terminated lines of a byte slice.
struct Lines<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Lines<'a> {
    fn rest(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }

    /// The next line without its LF; `None` when no LF ends it.
    fn next_terminated(&mut self) -> Option<&'a [u8]> {
        let rest = self.rest();
        let end = rest.iter().position(|&c| c == b'\n')?;
        self.pos += end + 1;
        Some(&rest[..end])
    }

    /// The next line without its LF and one CR before it; the last line may lack its LF.
    /// `None` at the end of the data (the armor reader's `getLine`).
    fn next_armor_line(&mut self) -> Option<&'a [u8]> {
        let rest = self.rest();
        if rest.is_empty() {
            return None;
        }
        let line = match rest.iter().position(|&c| c == b'\n') {
            Some(end) => {
                self.pos += end + 1;
                &rest[..end]
            }
            None => {
                self.pos = self.data.len();
                rest
            }
        };
        Some(line.strip_suffix(b"\r").unwrap_or(line))
    }
}

/// An argument: one or more VCHAR (0x21..=0x7E).
fn is_argument(arg: &str) -> bool {
    !arg.is_empty() && arg.bytes().all(|c| (0x21..=0x7e).contains(&c))
}

/// The header grammar (age.md's ABNF, as the reference's `format.Parse` reads it).
fn parse_header(file: &[u8]) -> Result<Header<'_>, AgeError> {
    let mut lines = Lines {
        data: &file[..file.len().min(MAX_HEADER)],
        pos: 0,
    };
    if lines.next_terminated() != Some(VERSION_LINE) {
        return Err(AgeError::Header);
    }
    let mut stanzas = Vec::new();
    loop {
        if lines.rest().starts_with(b"---") {
            let mac_covers = lines.pos + 3;
            let line = lines.next_terminated().ok_or(AgeError::Header)?;
            let encoded = line.strip_prefix(b"--- ").ok_or(AgeError::Header)?;
            let encoded = std::str::from_utf8(encoded).map_err(|_| AgeError::Header)?;
            let mac: [u8; 32] = b64::decode_nopad(encoded)
                .and_then(|mac| mac.try_into().ok())
                .ok_or(AgeError::Header)?;
            if stanzas.is_empty() {
                return Err(AgeError::Header);
            }
            return Ok(Header {
                stanzas,
                mac,
                mac_covers,
                len: lines.pos,
            });
        }
        let line = lines.next_terminated().ok_or(AgeError::Header)?;
        let line = std::str::from_utf8(line).map_err(|_| AgeError::Header)?;
        let mut parts = line.split(' ');
        if parts.next() != Some("->") {
            return Err(AgeError::Header);
        }
        let args: Vec<&str> = parts.collect();
        if args.is_empty() || !args.iter().all(|arg| is_argument(arg)) {
            return Err(AgeError::Header);
        }
        let mut body = Vec::new();
        loop {
            let line = lines.next_terminated().ok_or(AgeError::Header)?;
            let line = std::str::from_utf8(line).map_err(|_| AgeError::Header)?;
            let bytes = b64::decode_nopad(line).ok_or(AgeError::Header)?;
            if bytes.len() > BYTES_PER_LINE {
                return Err(AgeError::Header);
            }
            body.extend_from_slice(&bytes);
            if bytes.len() < BYTES_PER_LINE {
                break;
            }
        }
        stanzas.push(Stanza {
            kind: args[0],
            args: args[1..].to_vec(),
            body,
        });
    }
}

fn decrypt_binary(identities: &[Identity], file: &[u8]) -> Result<SecretBytes, AgeError> {
    if file.len() > MAX_FILE {
        return Err(AgeError::Header);
    }
    let header = parse_header(file)?;
    if header.stanzas.len() > 1 && header.stanzas.iter().any(|s| s.kind == "scrypt") {
        return Err(AgeError::Header);
    }
    let mut file_key = unwrap_file_key(identities, &header.stanzas)?;
    let result = open_after_header(&file_key, file, &header);
    file_key.zeroize();
    result
}

fn open_after_header(
    file_key: &[u8; FILE_KEY],
    file: &[u8],
    header: &Header<'_>,
) -> Result<SecretBytes, AgeError> {
    hmac::verify(
        &header_key(file_key),
        &file[..header.mac_covers],
        &header.mac,
    )
    .map_err(|_| AgeError::Hmac)?;
    let payload = &file[header.len..];
    if payload.len() < PAYLOAD_NONCE {
        return Err(AgeError::Header);
    }
    let (nonce, chunks) = payload.split_at(PAYLOAD_NONCE);
    open_stream(&payload_key(file_key, nonce), chunks)
}

/// Every identity against every stanza, in that order (the reference's `decryptHdr` and
/// `multiUnwrap`): the first stanza that opens gives the file key.
fn unwrap_file_key(
    identities: &[Identity],
    stanzas: &[Stanza<'_>],
) -> Result<[u8; FILE_KEY], AgeError> {
    for identity in identities {
        let ours = identity.recipient();
        for stanza in stanzas {
            if stanza.kind != "X25519" {
                continue;
            }
            if let Some(file_key) = unwrap_x25519(identity, &ours, stanza)? {
                return Ok(file_key);
            }
        }
    }
    Err(AgeError::NoMatch)
}

/// One X25519 stanza with one identity: `Ok(None)` when it is not addressed to it.
fn unwrap_x25519(
    identity: &Identity,
    ours: &Recipient,
    stanza: &Stanza<'_>,
) -> Result<Option<[u8; FILE_KEY]>, AgeError> {
    let [share] = stanza.args.as_slice() else {
        return Err(AgeError::Header);
    };
    let share: [u8; 32] = b64::decode_nopad(share)
        .and_then(|share| share.try_into().ok())
        .ok_or(AgeError::Header)?;
    // Checked before any decryption, against partitioning oracles (age.md).
    if stanza.body.len() != FILE_KEY + TAG {
        return Err(AgeError::Header);
    }
    let mut shared = x25519::x25519(identity.secret_bytes(), &share);
    if aterm_digest::ct_eq(&shared, &[0u8; 32]) {
        return Err(AgeError::Header);
    }
    let key = wrap_key(&shared, &share, &ours.0);
    shared.zeroize();
    let mut body = [0u8; FILE_KEY + TAG];
    body.copy_from_slice(&stanza.body);
    let opened = match key.open_in_place(zero_nonce(), aead::Aad::empty(), &mut body) {
        Ok(plain) => {
            let mut file_key = [0u8; FILE_KEY];
            file_key.copy_from_slice(plain);
            Some(file_key)
        }
        Err(_) => None,
    };
    body.zeroize();
    Ok(opened)
}

/// The STREAM payload after its nonce, all or nothing. A chunk's place decides its flag: a
/// full chunk with more bytes after it is not final, the chunk that ends the input is. A
/// reader that tried both flags (the reference does) refuses the same files with the same
/// class: a final chunk with bytes after it is a trailing-data failure there and an
/// authentication failure here.
fn open_stream(key: &aead::LessSafeKey, mut rest: &[u8]) -> Result<SecretBytes, AgeError> {
    // Plaintext is never longer than its ciphertext, so this never reallocates and leaves
    // no unwiped copy behind.
    let mut out: Vec<u8> = Vec::with_capacity(rest.len());
    let mut counter: u64 = 0;
    let result = loop {
        if rest.is_empty() {
            // The input ended without a final chunk.
            break Err(AgeError::Payload);
        }
        let last = rest.len() <= CHUNK + TAG;
        if last && counter > 0 && rest.len() == TAG {
            // An empty final chunk is allowed only as the whole payload.
            break Err(AgeError::Payload);
        }
        let take = if last { rest.len() } else { CHUNK + TAG };
        let at = out.len();
        out.extend_from_slice(&rest[..take]);
        match key.open_in_place(
            stream_nonce(counter, last),
            aead::Aad::empty(),
            &mut out[at..],
        ) {
            Ok(plain) => {
                let len = plain.len();
                out.truncate(at + len);
            }
            Err(_) => break Err(AgeError::Payload),
        }
        rest = &rest[take..];
        counter += 1;
        if last {
            break Ok(());
        }
    };
    match result {
        Ok(()) => Ok(SecretBytes::new(out)),
        Err(error) => {
            out.zeroize();
            Err(error)
        }
    }
}

/// The ASCII armor of a binary age file (RFC 7468 strict: padded base64, 64 columns,
/// `-----BEGIN AGE ENCRYPTED FILE-----` / `-----END AGE ENCRYPTED FILE-----`, LF, a final LF).
pub fn armor(binary: &[u8]) -> String {
    let lines = binary.len().div_ceil(BYTES_PER_LINE);
    let mut text =
        String::with_capacity(ARMOR_BEGIN.len() + ARMOR_END.len() + 2 + lines * (COLUMNS + 1));
    text.push_str("-----BEGIN AGE ENCRYPTED FILE-----\n");
    // Whole lines per block, so only the last block can carry padding.
    for block in binary.chunks(BYTES_PER_LINE * 1024) {
        let encoded = b64::encode_padded(block);
        let mut at = 0;
        while at < encoded.len() {
            let end = encoded.len().min(at + COLUMNS);
            text.push_str(&encoded[at..end]);
            text.push('\n');
            at = end;
        }
    }
    text.push_str("-----END AGE ENCRYPTED FILE-----\n");
    text
}

/// ASCII whitespace as the reference's `bytes.TrimSpace` sees it.
fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// The binary age file inside an armor, by the reference reader's rules (age.md and
/// `armor.go`): at most 1024 bytes of surrounding whitespace, CR stripped from line ends, no
/// empty line, no line over 64 columns, every line but the last exactly 64, each line
/// canonical padded base64 on its own.
///
/// Precisely: whitespace-only lines before the BEGIN line (at most 1024 bytes of them,
/// counting each line's LF); the BEGIN line exactly; lines of padded base64, each at most
/// 64 columns and decoding on its own, every one but the last decoding to exactly 48 bytes;
/// the END line exactly; then fewer than 1024 bytes, all whitespace. Each line may end in
/// CRLF; the END line may end the input without one. No armor over [`MAX_ARMORED`] bytes.
pub fn dearmor(text: &[u8]) -> Result<Vec<u8>, AgeError> {
    if text.len() > MAX_ARMORED {
        return Err(AgeError::Armor);
    }
    let mut lines = Lines { data: text, pos: 0 };
    let mut leading = 0usize;
    loop {
        let line = lines.next_armor_line().ok_or(AgeError::Armor)?;
        if line.iter().all(|&c| is_space(c)) {
            leading += line.len() + 1;
            if leading > ARMOR_WHITESPACE {
                return Err(AgeError::Armor);
            }
            continue;
        }
        if line != ARMOR_BEGIN {
            return Err(AgeError::Armor);
        }
        break;
    }
    let mut binary = Vec::with_capacity(text.len() / 4 * 3);
    loop {
        let line = lines.next_armor_line().ok_or(AgeError::Armor)?;
        if line == ARMOR_END {
            break;
        }
        if line.is_empty() || line.len() > COLUMNS || line.contains(&b'\r') {
            return Err(AgeError::Armor);
        }
        let line = std::str::from_utf8(line).map_err(|_| AgeError::Armor)?;
        let bytes = b64::decode_padded(line).ok_or(AgeError::Armor)?;
        binary.extend_from_slice(&bytes);
        if bytes.len() < BYTES_PER_LINE {
            if lines.next_armor_line() != Some(ARMOR_END) {
                return Err(AgeError::Armor);
            }
            break;
        }
    }
    let trailing = lines.rest();
    if trailing.len() >= ARMOR_WHITESPACE || !trailing.iter().all(|&c| is_space(c)) {
        return Err(AgeError::Armor);
    }
    Ok(binary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(seed: u8) -> Identity {
        // A synthetic key from a fixed seed, derived at run time.
        let mut secret = [0u8; 32];
        for (i, byte) in secret.iter_mut().enumerate() {
            *byte = seed.wrapping_mul(31).wrapping_add(i as u8 * 7);
        }
        Identity::from_secret_bytes(secret)
    }

    /// Deterministic filler, so a test's plaintext is not all one byte.
    fn plaintext(len: usize) -> Vec<u8> {
        let mut state: u32 = 0x9e37_79b9;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect()
    }

    #[test]
    fn round_trips_every_size_to_one_two_and_five_recipients() {
        let identities: Vec<Identity> = (1..=5).map(identity).collect();
        let recipients: Vec<Recipient> = identities.iter().map(Identity::recipient).collect();
        for count in [1usize, 2, 5] {
            for len in [0usize, 1, 65535, 65536, 65537, 200_000] {
                let data = plaintext(len);
                let file = encrypt(&recipients[..count], &data).expect("encrypt");
                for identity in &identities[..count] {
                    let opened = decrypt(std::slice::from_ref(identity), &file).expect("decrypt");
                    assert_eq!(
                        opened.as_slice(),
                        data.as_slice(),
                        "{count} recipients, {len} bytes"
                    );
                }
                if count < 5 {
                    assert_eq!(
                        decrypt(&identities[count..], &file).err(),
                        Some(AgeError::NoMatch),
                        "{count} recipients, {len} bytes"
                    );
                }
            }
        }
    }

    #[test]
    fn chunking_matches_the_spec() {
        let recipient = identity(1).recipient();
        for (len, chunks) in [
            (0usize, 1usize),
            (1, 1),
            (65536, 1),
            (65537, 2),
            (200_000, 4),
        ] {
            let file = encrypt(&[recipient], &plaintext(len)).expect("encrypt");
            let header_len = parse_header(&file).expect("header").len;
            assert_eq!(
                file.len(),
                header_len + 16 + len + 16 * chunks,
                "{len} bytes"
            );
        }
    }

    #[test]
    fn header_is_well_formed() {
        let recipients = [identity(1).recipient(), identity(2).recipient()];
        let file = encrypt(&recipients, b"x").expect("encrypt");
        let header = parse_header(&file).expect("header");
        assert_eq!(header.stanzas.len(), 2);
        for stanza in &header.stanzas {
            assert_eq!(stanza.kind, "X25519");
            assert_eq!(stanza.args.len(), 1);
            assert_eq!(stanza.args[0].len(), 43);
            assert_eq!(stanza.body.len(), 32);
        }
        let text = std::str::from_utf8(&file[..header.len]).expect("ascii header");
        assert!(
            text.starts_with("age-encryption.org/v1\n-> X25519 "),
            "{text}"
        );
        assert!(text.lines().all(|line| line.len() <= 64), "{text}");
        // Fresh ephemeral shares and file keys: two encryptions share nothing.
        let again = encrypt(&recipients, b"x").expect("encrypt");
        let again = parse_header(&again).expect("header");
        assert_ne!(header.stanzas[0].args, again.stanzas[0].args);
        assert_ne!(header.mac, again.mac);
    }

    #[test]
    fn wrong_identity_is_no_match_and_no_identity_too() {
        let file = encrypt(&[identity(1).recipient()], b"secret").expect("encrypt");
        assert_eq!(
            decrypt(&[identity(2)], &file).err(),
            Some(AgeError::NoMatch)
        );
        assert_eq!(decrypt(&[], &file).err(), Some(AgeError::NoMatch));
        // The first identity that opens a stanza wins; others are tried in order.
        let opened = decrypt(&[identity(2), identity(1)], &file).expect("second identity");
        assert_eq!(opened.as_slice(), b"secret");
    }

    #[test]
    fn armor_round_trips() {
        let recipient = identity(3).recipient();
        for len in [0usize, 1, 2, 3, 47, 48, 49, 1000, 70_000] {
            let file = encrypt(&[recipient], &plaintext(len)).expect("encrypt");
            let armored = armor(&file);
            assert!(armored.starts_with("-----BEGIN AGE ENCRYPTED FILE-----\n"));
            assert!(armored.ends_with("\n-----END AGE ENCRYPTED FILE-----\n"));
            assert!(
                armored
                    .lines()
                    .all(|line| line.len() <= 64 || line.starts_with("-----"))
            );
            assert_eq!(dearmor(armored.as_bytes()).expect("dearmor"), file, "{len}");
            let opened = decrypt(&[identity(3)], armored.as_bytes()).expect("decrypt armor");
            assert_eq!(opened.as_slice(), plaintext(len).as_slice());
            // CRLF line ends and surrounding whitespace are accepted.
            let crlf = format!("\n \t\r\n{}\n\n", armored.replace('\n', "\r\n"));
            assert_eq!(dearmor(crlf.as_bytes()).expect("crlf"), file, "{len}");
        }
        for len in [0usize, 1, 2, 3, 48, 96, 100] {
            let bytes = plaintext(len);
            assert_eq!(
                dearmor(armor(&bytes).as_bytes()).expect("dearmor"),
                bytes,
                "{len}"
            );
        }
        assert_eq!(
            armor(b""),
            "-----BEGIN AGE ENCRYPTED FILE-----\n-----END AGE ENCRYPTED FILE-----\n"
        );
    }

    #[test]
    fn armor_whitespace_bounds() {
        let armored = armor(b"age");
        let ok = format!("{}\n{}", " ".repeat(1023), armored);
        assert!(
            dearmor(ok.as_bytes()).is_ok(),
            "1023 spaces and an LF are 1024 bytes"
        );
        let too_much = format!("{}\n{}", " ".repeat(1024), armored);
        assert_eq!(dearmor(too_much.as_bytes()).err(), Some(AgeError::Armor));
        let same_line = format!(" {armored}");
        assert_eq!(dearmor(same_line.as_bytes()).err(), Some(AgeError::Armor));
        let trailing_ok = format!("{}{}", armored, " ".repeat(1023));
        assert!(dearmor(trailing_ok.as_bytes()).is_ok());
        let trailing_too_much = format!("{}{}", armored, " ".repeat(1024));
        assert_eq!(
            dearmor(trailing_too_much.as_bytes()).err(),
            Some(AgeError::Armor)
        );
        assert_eq!(dearmor(b"").err(), Some(AgeError::Armor));
        assert_eq!(
            dearmor(b"-----BEGIN AGE ENCRYPTED FILE-----\n").err(),
            Some(AgeError::Armor),
            "no END line"
        );
    }

    #[test]
    fn identity_and_recipient_spellings() {
        let id = identity(9);
        let text = id.to_bech32();
        let text = text.as_str().expect("ascii");
        assert!(text.starts_with("AGE-SECRET-KEY-1"));
        assert_eq!(text, text.to_ascii_uppercase());
        let parsed = Identity::parse(text).expect("parse");
        assert_eq!(parsed.secret_bytes(), id.secret_bytes());
        // Lowercase (the checksum is still valid) is not an identity.
        assert_eq!(
            Identity::parse(&text.to_ascii_lowercase()).err(),
            Some(AgeError::Key)
        );
        let recipient = id.recipient();
        let spelled = recipient.to_bech32();
        assert!(spelled.starts_with("age1"));
        assert_eq!(spelled.len(), 62);
        assert_eq!(Recipient::parse(&spelled), Ok(recipient));
        assert_eq!(
            Recipient::parse(&spelled.to_ascii_uppercase()),
            Err(AgeError::Key)
        );
        // Wrong HRPs: an identity as a recipient, a recipient as an identity.
        assert_eq!(Recipient::parse(text).err(), Some(AgeError::Key));
        assert_eq!(
            Identity::parse(&spelled.to_ascii_uppercase()).err(),
            Some(AgeError::Key)
        );
        // Wrong lengths.
        assert_eq!(
            Recipient::parse(&bech32::encode("age", &[1u8; 31])),
            Err(AgeError::Key)
        );
        assert_eq!(
            Recipient::parse(&bech32::encode("age", &[1u8; 33])),
            Err(AgeError::Key)
        );
        let mut short = bech32::encode("age-secret-key-", &[1u8; 31]);
        short.make_ascii_uppercase();
        assert_eq!(Identity::parse(&short).err(), Some(AgeError::Key));
        assert_eq!(Recipient::parse("").err(), Some(AgeError::Key));
        let shown = format!("{id:?}");
        assert!(!shown.contains(&text[16..]), "Debug shows no secret");
        assert_eq!(shown, format!("Identity({spelled})"));
    }

    /// age.md's example identity (32 bytes of 0x42), spelled at run time so no key-shaped
    /// literal sits in the source.
    #[test]
    fn spec_example_identity_spelling() {
        let identity_text = format!("{}1{}GFPQ4EGAEX", "AGE-SECRET-KEY-", "GFPYYSJZ".repeat(6));
        let parsed = Identity::parse(&identity_text).expect("age.md's example identity");
        assert_eq!(parsed.secret_bytes(), &[0x42u8; 32]);
        let built = Identity::from_secret_bytes([0x42u8; 32]);
        assert_eq!(built.to_bech32().as_str(), Some(identity_text.as_str()));
        assert_eq!(built.recipient(), parsed.recipient());
    }

    #[test]
    fn generated_identities_differ_and_open_their_files() {
        let a = Identity::generate().expect("generate");
        let b = Identity::generate().expect("generate");
        assert_ne!(a.secret_bytes(), b.secret_bytes());
        let file = encrypt(&[a.recipient()], b"hello").expect("encrypt");
        assert_eq!(decrypt(&[a], &file).expect("decrypt").as_slice(), b"hello");
        assert_eq!(decrypt(&[b], &file).err(), Some(AgeError::NoMatch));
    }

    #[test]
    fn encrypt_refuses_no_recipient_and_a_low_order_one() {
        assert!(encrypt(&[], b"x").is_err());
        assert!(encrypt(&[Recipient([0u8; 32])], b"x").is_err());
        let mut one = [0u8; 32];
        one[0] = 1;
        assert!(encrypt(&[identity(1).recipient(), Recipient(one)], b"x").is_err());
    }

    /// The low-order u-coordinates, made at run time: 0 (order 2), 1 and p - 1 (order 4,
    /// one on the curve and one on its twist), the two points of order 8, and p and p + 1
    /// (0 and 1 again, unreduced); each also with the top bit set, which X25519 ignores.
    fn low_order_points() -> Vec<[u8; 32]> {
        let unreduced = |low: u8| {
            // p = 2^255 - 19 is ed ff … ff 7f little-endian.
            let mut u = [0xffu8; 32];
            u[0] = low;
            u[31] = 0x7f;
            u
        };
        let order_eight = |hex: &str| {
            let mut u = [0u8; 32];
            assert!(aterm_codec::hex::decode_into(hex.as_bytes(), &mut u), "hex");
            u
        };
        let mut one = [0u8; 32];
        one[0] = 1;
        let mut points = vec![
            [0u8; 32],
            one,
            unreduced(0xec),
            order_eight("e0eb7a7c3b41b8ae1656e3faf19fc46ada098deb9c32b1fd866205165f49b800"),
            order_eight("5f9c95bca3508c24b1d0b1559c83ef5b04445cc4581c8e86d8224eddd09f1157"),
            unreduced(0xed),
            unreduced(0xee),
        ];
        let top_bit: Vec<[u8; 32]> = points
            .iter()
            .map(|u| {
                let mut u = *u;
                u[31] |= 0x80;
                u
            })
            .collect();
        points.extend(top_bit);
        points
    }

    #[test]
    fn a_low_order_recipient_is_refused_by_parse_and_by_encrypt() {
        let points = low_order_points();
        assert_eq!(points.len(), 14);
        for point in &points {
            for scalar in [[7u8; 32], [0xa5; 32]] {
                assert_eq!(x25519::x25519(&scalar, point), [0u8; 32], "{point:02x?}");
            }
            let spelled = bech32::encode("age", point);
            assert_eq!(Recipient::parse(&spelled), Err(AgeError::Key), "{spelled}");
            assert!(encrypt(&[Recipient(*point)], b"x").is_err());
            assert!(encrypt(&[identity(1).recipient(), Recipient(*point)], b"x").is_err());
        }
        // Real keys, and small coordinates that are not of low order, still parse.
        for seed in 1..=8 {
            let recipient = identity(seed).recipient();
            assert_eq!(Recipient::parse(&recipient.to_bech32()), Ok(recipient));
        }
        for small in [2u8, 3, 9] {
            let mut u = [0u8; 32];
            u[0] = small;
            assert_eq!(
                Recipient::parse(&bech32::encode("age", &u)),
                Ok(Recipient(u))
            );
        }
    }

    #[test]
    fn a_stanza_whose_share_is_low_order_is_a_header_failure() {
        let file = encrypt(&[identity(1).recipient()], b"secret").expect("encrypt");
        let share = parse_header(&file).expect("header").stanzas[0].args[0].to_string();
        let at = file
            .windows(share.len())
            .position(|window| window == share.as_bytes())
            .expect("the share is in the header");
        for point in low_order_points() {
            let forged = b64::encode_nopad(&point);
            assert_eq!(forged.len(), share.len());
            let mut file = file.clone();
            file[at..at + share.len()].copy_from_slice(forged.as_bytes());
            // The all-zero shared secret is refused before the header MAC is looked at, for
            // the recipient and for anyone else alike.
            for id in [identity(1), identity(2)] {
                assert_eq!(
                    decrypt(std::slice::from_ref(&id), &file).err(),
                    Some(AgeError::Header),
                    "{point:02x?}"
                );
            }
        }
        assert_eq!(
            decrypt(&[identity(1)], &file)
                .expect("the original opens")
                .as_slice(),
            b"secret"
        );
    }

    /// A real file with one more stanza spliced in first, to reach the rules no CCTV
    /// vector applicable here covers.
    fn with_stanza_first(file: &[u8], extra_stanza: &str) -> Vec<u8> {
        let start = VERSION_LINE.len() + 1;
        let mut out = file[..start].to_vec();
        out.extend_from_slice(extra_stanza.as_bytes());
        out.extend_from_slice(&file[start..]);
        out
    }

    #[test]
    fn scrypt_beside_another_stanza_is_a_header_failure() {
        let file = encrypt(&[identity(1).recipient()], b"x").expect("encrypt");
        let mixed = with_stanza_first(&file, "-> scrypt c2FsdHNhbHRzYWx0c2FsdA 10\nAAAA\n");
        assert_eq!(
            decrypt(&[identity(1)], &mixed).err(),
            Some(AgeError::Header)
        );
        // An unknown stanza is skipped (the MAC then fails, as the header changed).
        let grease = with_stanza_first(&file, "-> grease x y\n\n");
        assert_eq!(decrypt(&[identity(1)], &grease).err(), Some(AgeError::Hmac));
    }

    #[test]
    fn a_header_without_stanzas_or_past_the_bound_is_refused() {
        let no_stanza = format!(
            "age-encryption.org/v1\n--- {}\n{}",
            "A".repeat(43),
            "B".repeat(40)
        );
        assert_eq!(
            decrypt(&[identity(1)], no_stanza.as_bytes()).err(),
            Some(AgeError::Header)
        );
        // A stanza body of full lines that never ends runs past the header bound.
        let mut huge = b"age-encryption.org/v1\n-> grease\n".to_vec();
        while huge.len() <= MAX_HEADER {
            huge.extend_from_slice(&[b'A'; 64]);
            huge.push(b'\n');
        }
        huge.extend_from_slice(b"\n--- AAAA\n");
        assert_eq!(decrypt(&[identity(1)], &huge).err(), Some(AgeError::Header));
    }

    /// Malformed input never panics: truncations and bit flips of a valid file (binary
    /// and armored) give an error, never a plaintext and never a panic.
    #[test]
    fn truncations_and_bit_flips_only_fail() {
        let data = plaintext(70_000);
        let file =
            encrypt(&[identity(4).recipient(), identity(5).recipient()], &data).expect("encrypt");
        let ids = [identity(4)];
        let header_len = parse_header(&file).expect("header").len;
        // Every cut inside the header, the nonce and the first chunk's start; a stride
        // through the payload; every cut near the end.
        let mut cuts: Vec<usize> = (0..header_len + 64).collect();
        cuts.extend((header_len + 64..file.len()).step_by(997));
        cuts.extend(file.len() - 40..file.len());
        for cut in cuts {
            assert!(decrypt(&ids, &file[..cut]).is_err(), "cut at {cut}");
        }
        // The armor of a smaller file (each decrypt that reaches a stanza costs an X25519,
        // milliseconds unoptimized).
        let small = plaintext(1000);
        let small_file =
            encrypt(&[identity(4).recipient(), identity(5).recipient()], &small).expect("encrypt");
        let armored = armor(&small_file).into_bytes();
        for cut in (0..armored.len() - 1).step_by(7) {
            assert!(
                decrypt(&ids, &armored[..cut]).is_err(),
                "armor cut at {cut}"
            );
        }
        // One bit of every header and nonce byte (the bit cycling with the offset), then a
        // stride through the chunks and their tags.
        let mut flips: Vec<usize> = (0..header_len + 16).map(|at| at * 8 + at % 8).collect();
        flips.extend(((header_len + 16) * 8..file.len() * 8).step_by(4099));
        flips.push(file.len() * 8 - 1);
        for bit in flips {
            let mut flipped = file.clone();
            flipped[bit / 8] ^= 1 << (bit % 8);
            assert!(decrypt(&ids, &flipped).is_err(), "bit {bit}");
        }
        for bit in (0..armored.len() * 8).step_by(37) {
            let mut flipped = armored.clone();
            flipped[bit / 8] ^= 1 << (bit % 8);
            if let Ok(opened) = decrypt(&ids, &flipped) {
                // Only a flip of armor whitespace that stays whitespace can survive.
                assert_eq!(opened.as_slice(), small.as_slice(), "armor bit {bit}");
            }
        }
        // Arbitrary bytes before and after are refused too.
        let mut prefixed = b"x".to_vec();
        prefixed.extend_from_slice(&file);
        assert_eq!(decrypt(&ids, &prefixed).err(), Some(AgeError::Armor));
        let mut suffixed = file.clone();
        suffixed.push(0);
        assert_eq!(decrypt(&ids, &suffixed).err(), Some(AgeError::Payload));
        // And the untouched file still opens.
        assert_eq!(
            decrypt(&ids, &file).expect("decrypt").as_slice(),
            data.as_slice()
        );
    }

    #[test]
    fn errors_name_no_value() {
        for error in [
            AgeError::NoMatch,
            AgeError::Header,
            AgeError::Hmac,
            AgeError::Payload,
            AgeError::Armor,
            AgeError::Key,
        ] {
            assert!(error.to_string().starts_with("age: "));
        }
    }
}
