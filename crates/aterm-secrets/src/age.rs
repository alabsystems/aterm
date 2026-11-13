// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! age v1 (C2SP age.md) encryption and decryption with X25519 recipients ([`encrypt`],
//! [`armor`], [`decrypt`], [`AgeError::Armor`], [`AgeError::Header`], [`MAX_FILE`],
//! [`MAX_HEADER`], [`AgeError::NoMatch`], [`AgeError::Hmac`], [`AgeError::Payload`]).

use crate::{SecretBytes, b64, bech32, x25519};
use ring::{aead, hkdf, hmac};
use zeroize::{Zeroize, Zeroizing};

/// Why an age operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgeError {
    /// No stanza opened with the identities given.
    NoMatch,
    /// The header is malformed.
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

/// The largest binary age file [`decrypt`] reads, and [`encrypt`] makes: 64 MiB.
pub const MAX_FILE: usize = 64 << 20;

/// The largest header [`decrypt`] reads, and [`encrypt`] makes: 1 MiB.
pub const MAX_HEADER: usize = 1 << 20;

/// Largest armored file [`dearmor`] reads ([`MAX_FILE`]).
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

/// Parsing still validates every spelling and retains the supplied secret in
/// its own identity. Only the deterministic public derivation is shared; the
/// bounded cache keeps a digest and public point, never a private key or grant.
fn parsed_public(secret: &[u8; 32]) -> [u8; 32] {
    type Entry = (Zeroizing<[u8; 32]>, [u8; 32]);
    static PUBLIC: std::sync::Mutex<std::collections::VecDeque<Entry>> =
        std::sync::Mutex::new(std::collections::VecDeque::new());
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm/age/identity-public/1\0");
    hash.update(secret);
    let digest = Zeroizing::new(hash.finalize());
    if let Ok(cache) = PUBLIC.lock()
        && let Some((_, public)) = cache.iter().find(|(key, _)| **key == *digest)
    {
        return *public;
    }
    let public = x25519::x25519_base(secret);
    if let Ok(mut cache) = PUBLIC.lock() {
        if cache.len() >= 16 {
            cache.pop_front();
        }
        cache.push_back((digest, public));
    }
    public
}

/// An age X25519 identity: a machine key.
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
                identity.public = parsed_public(&identity.secret);
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
    pub const fn recipient(&self) -> Recipient {
        Recipient(self.public)
    }

    /// The 32 secret bytes (for the X25519 unwrap).
    pub const fn secret_bytes(&self) -> &[u8; 32] {
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
    /// Parse an `age1…` X25519 recipient ([`encrypt`]).
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

/// True when `point` is a low-order u-coordinate, one whose X25519 with every scalar is all zero.
/// Those are [`LOW_ORDER`]'s seven with bit 255 either way, which X25519 ignores, so a parse
/// compares them and never runs a ladder for this.
fn low_order(point: &[u8; 32]) -> bool {
    let mut u = *point;
    u[31] &= 0x7f;
    LOW_ORDER.contains(&u)
}

/// Every u-coordinate below 2^255 of a point of order dividing 8 on the curve or its twist,
/// little-endian (libsodium's `has_small_order` list): 0 (order 2), 1 and p - 1 (order 4, one
/// on the curve and one on its twist), the two u-coordinates of points of order 8, and p and
/// p + 1 (0 and 1 unreduced). A clamped scalar is a multiple of 8 and of neither prime order,
/// so its X25519 is all zero for these alone.
const LOW_ORDER: [[u8; 32]; 7] = [
    small(0),
    small(1),
    [
        0xe0, 0xeb, 0x7a, 0x7c, 0x3b, 0x41, 0xb8, 0xae, 0x16, 0x56, 0xe3, 0xfa, 0xf1, 0x9f, 0xc4,
        0x6a, 0xda, 0x09, 0x8d, 0xeb, 0x9c, 0x32, 0xb1, 0xfd, 0x86, 0x62, 0x05, 0x16, 0x5f, 0x49,
        0xb8, 0x00,
    ],
    [
        0x5f, 0x9c, 0x95, 0xbc, 0xa3, 0x50, 0x8c, 0x24, 0xb1, 0xd0, 0xb1, 0x55, 0x9c, 0x83, 0xef,
        0x5b, 0x04, 0x44, 0x5c, 0xc4, 0x58, 0x1c, 0x8e, 0x86, 0xd8, 0x22, 0x4e, 0xdd, 0xd0, 0x9f,
        0x11, 0x57,
    ],
    near_2_255(0xec),
    near_2_255(0xed),
    near_2_255(0xee),
];

/// The u-coordinate `low`.
const fn small(low: u8) -> [u8; 32] {
    let mut u = [0u8; 32];
    u[0] = low;
    u
}

/// 2^255 - 256 + `low`: p - 1, p and p + 1 for 0xec, 0xed and 0xee.
const fn near_2_255(low: u8) -> [u8; 32] {
    let mut u = [0xffu8; 32];
    u[0] = low;
    u[31] = 0x7f;
    u
}

/// Encrypt `plaintext` to every recipient ([`MAX_FILE`], [`MAX_HEADER`], [`decrypt`]).
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

/// A stanza body wrapped at 64 columns.
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

/// Decrypt a binary or armored age file ([`dearmor`]).
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
    let header = binary_header(file)?;
    let mut file_key = unwrap_file_key(identities, &header.stanzas)?;
    let result = open_after_header(&file_key, file, &header);
    file_key.zeroize();
    result
}

fn binary_header(file: &[u8]) -> Result<Header<'_>, AgeError> {
    if file.len() > MAX_FILE {
        return Err(AgeError::Header);
    }
    let header = parse_header(file)?;
    if header.stanzas.len() > 1 && header.stanzas.iter().any(|s| s.kind == "scrypt") {
        return Err(AgeError::Header);
    }
    Ok(header)
}

/// Only Commons history handoff uses an original file key. Authenticate the
/// complete binary file before allowing that key to be wrapped to a new member.
#[cfg(unix)]
pub(crate) fn authenticated_file_key(
    identity: &Identity,
    file: &[u8],
) -> Result<Zeroizing<[u8; FILE_KEY]>, AgeError> {
    let header = binary_header(file)?;
    let key = Zeroizing::new(unwrap_file_key(
        std::slice::from_ref(identity),
        &header.stanzas,
    )?);
    open_after_header(&key, file, &header)?;
    Ok(key)
}

/// Opening with a handoff key still authenticates the original header and every
/// payload chunk. This is crate-private, not another public decryption mode.
#[cfg(unix)]
pub(crate) fn decrypt_file_key(key: &[u8; FILE_KEY], file: &[u8]) -> Result<SecretBytes, AgeError> {
    open_after_header(key, file, &binary_header(file)?)
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

/// Every identity against every stanza.
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

/// The STREAM payload after its nonce, all or nothing.
fn open_stream(key: &aead::LessSafeKey, mut rest: &[u8]) -> Result<SecretBytes, AgeError> {
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

/// Wrap a binary age file in RFC 7468 strict ASCII armor.
pub fn armor(binary: &[u8]) -> String {
    let lines = binary.len().div_ceil(BYTES_PER_LINE);
    let mut text =
        String::with_capacity(ARMOR_BEGIN.len() + ARMOR_END.len() + 2 + lines * (COLUMNS + 1));
    text.push_str("-----BEGIN AGE ENCRYPTED FILE-----\n");
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
const fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// Decode the binary age file inside an ASCII armor ([`MAX_ARMORED`]).
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
#[path = "age_tests.rs"]
mod tests;
