// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! OpenPGP detached-signature verification, pure (no I/O, no clock): a vendor digest
//! document is accepted only under a v4 RSA signature by a key compiled into this binary
//! (`docs/DESIGN-atpkg-vendor-direct-updates-2026-09-22.md` §1.2).
//!
//! The accepted subset is what a release signer emits; everything outside it is refused,
//! never interpreted:
//!
//! - input: binary packets, or exactly one armored `PGP SIGNATURE` block (armor headers
//!   ignored; the CRC24 line is never consulted — the RSA signature is the integrity check);
//! - packets: old or new headers; old length-type 3, partial lengths, leftover bytes and
//!   every tag but signature (2) and marker (10, skipped) are refused; at most eight
//!   signatures, accepted iff one verifies;
//! - signature: v4, type 0x00, RSA (1), SHA-512 (10) or SHA-256 (8); the unhashed area is
//!   skipped by length and never read;
//! - hashed subpackets: unknown critical types and a second creation time (2) or issuer
//!   fingerprint (33) are refused; exactly one creation time, within
//!   `[key.created, now + 7 d]`; every expiration (3) is honoured; the issuer fingerprint
//!   must be `0x04 ‖ key.fingerprint`;
//! - crypto: the signed message is `doc ‖ body[0..6+hashed_len] ‖ 0x04 0xFF ‖
//!   be32(6+hashed_len)`; the digest's left 16 bits are a cheap reject only; the MPI must
//!   fit its bit count and the modulus, and is left-padded to the modulus length because
//!   ring verifies only a signature exactly as long as the modulus. Leading zero bytes and
//!   an overstated bit count are accepted: v4 allows them (RFC 9580 §3.2 demands the
//!   canonical form of v6 only) and a signer writing fixed-length PKCS#1 output emits them;
//! - a `.sig` is not canonical: armor, packet framing, markers, the unhashed area and MPI
//!   padding all vary under one verifying signature, so nothing may be keyed on its bytes.

use std::borrow::Cow;

use ring::digest;
use ring::signature::{self, RsaParameters, RsaPublicKeyComponents};

/// The largest signature input accepted, armored or binary — the `.sig` fetch cap (§1.1).
pub const MAX_SIGNATURE_LEN: usize = 16 * 1024;
/// At most this many signature packets; more is refused even when one of them verifies.
const MAX_SIGNATURES: usize = 8;
/// How far past `now` a creation time may lie (signer clock skew).
const FUTURE_SKEW_SECS: u64 = 7 * 24 * 60 * 60;

const TAG_SIGNATURE: u8 = 2;
const TAG_MARKER: u8 = 10;
const SIG_VERSION: u8 = 4;
const SIG_TYPE_BINARY: u8 = 0x00;
const PKALG_RSA: u8 = 1;
const HASH_SHA256: u8 = 8;
const HASH_SHA512: u8 = 10;
const SUBPACKET_CREATION_TIME: u8 = 2;
const SUBPACKET_EXPIRATION: u8 = 3;
const SUBPACKET_ISSUER_FINGERPRINT: u8 = 33;
/// The version octet a v4 key's issuer-fingerprint subpacket carries before the fingerprint.
const FINGERPRINT_V4: u8 = 4;

const ARMOR_BEGIN: &[u8] = b"-----BEGIN PGP SIGNATURE-----";
const ARMOR_END: &[u8] = b"-----END PGP SIGNATURE-----";

/// An RSA signing key compiled into the binary: the only key a document can verify under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedRsaKey {
    /// The modulus, big-endian, without a leading zero byte.
    pub n: &'static [u8],
    /// The public exponent, big-endian, without a leading zero byte.
    pub e: &'static [u8],
    /// The key packet's creation time (Unix seconds); no signature may predate it.
    pub created: u32,
    /// The v4 fingerprint: SHA-1 over `0x99 ‖ be16(len) ‖ public-key packet body`.
    pub fingerprint: [u8; 20],
}

/// Anthropic's Claude Code release signing key: v4 RSA-4096, created 2026-03-30,
/// fingerprint `31DDDE24DDFAB679F42D7BD2BAA929FF1A7ECACE`. A test re-derives every field
/// from the committed `vendor_direct/fixtures/anthropic-claude-code.asc`.
pub const ANTHROPIC_CLAUDE_CODE_RELEASE_KEY: PinnedRsaKey = PinnedRsaKey {
    n: &ANTHROPIC_CLAUDE_CODE_RELEASE_N,
    e: &[0x01, 0x00, 0x01],
    created: 1_774_907_248,
    fingerprint: [
        0x31, 0xdd, 0xde, 0x24, 0xdd, 0xfa, 0xb6, 0x79, 0xf4, 0x2d, 0x7b, 0xd2, 0xba, 0xa9, 0x29,
        0xff, 0x1a, 0x7e, 0xca, 0xce,
    ],
};

const ANTHROPIC_CLAUDE_CODE_RELEASE_N: [u8; 512] = [
    0xa7, 0x6f, 0x2b, 0x49, 0x5e, 0x48, 0xf0, 0x79, 0x8a, 0xf0, 0x22, 0xbd, 0x1a, 0x2c, 0x41, 0x51,
    0x94, 0x7f, 0x82, 0xd2, 0x71, 0x78, 0xe0, 0xac, 0x45, 0x73, 0xb8, 0x10, 0x98, 0x9e, 0x8f, 0x8d,
    0x23, 0xcc, 0x19, 0x90, 0x03, 0x77, 0x16, 0x5c, 0x6a, 0x2c, 0x54, 0xef, 0x36, 0x65, 0xf1, 0x6c,
    0x44, 0xa5, 0x53, 0xb4, 0xc0, 0xb9, 0x88, 0x1f, 0xc7, 0x1f, 0xdb, 0x31, 0x8e, 0x77, 0x2b, 0x99,
    0x83, 0x56, 0x1a, 0xba, 0x09, 0x35, 0xbc, 0x76, 0xf6, 0x6f, 0xc5, 0x81, 0x84, 0x01, 0x82, 0x40,
    0x97, 0x4f, 0x80, 0x1f, 0x16, 0x5e, 0xab, 0x9e, 0x75, 0xea, 0x44, 0x62, 0xd9, 0x39, 0xea, 0xe3,
    0xff, 0x80, 0x2a, 0x13, 0x8c, 0x90, 0xd4, 0x66, 0x5b, 0x84, 0x18, 0x11, 0xe7, 0x96, 0x7b, 0xae,
    0x8d, 0x22, 0x78, 0xa2, 0xbc, 0x44, 0x76, 0x14, 0xb2, 0x90, 0x4a, 0x00, 0x51, 0xf2, 0x59, 0x18,
    0x42, 0x0b, 0x3e, 0x93, 0x1d, 0x6e, 0x98, 0x45, 0xb7, 0xf4, 0xa7, 0xf8, 0x46, 0xff, 0x4f, 0x13,
    0x39, 0x8e, 0x92, 0x8f, 0x77, 0x09, 0xef, 0xf0, 0x04, 0xa1, 0x34, 0x5f, 0xf5, 0x99, 0x95, 0x35,
    0x7d, 0x64, 0xd0, 0xab, 0xdb, 0x51, 0x28, 0x95, 0x1a, 0x91, 0x98, 0xf4, 0xe2, 0x07, 0x5a, 0x0c,
    0x51, 0x25, 0x41, 0x76, 0xa4, 0x0a, 0xd4, 0xe2, 0x8f, 0xe1, 0x93, 0xeb, 0x2a, 0x18, 0x08, 0x04,
    0x52, 0x1f, 0x01, 0x8e, 0xf8, 0xf0, 0xbf, 0x3e, 0xbe, 0xaa, 0xe2, 0xd7, 0x3e, 0x0f, 0xb2, 0xd1,
    0x8f, 0x14, 0x81, 0xbd, 0x45, 0x83, 0xa0, 0x54, 0xa6, 0x96, 0xa6, 0xff, 0x3c, 0x9d, 0x61, 0x8f,
    0xd0, 0xff, 0xec, 0x8d, 0x8e, 0x7b, 0x85, 0xd7, 0x9e, 0x20, 0x0f, 0x58, 0xc4, 0x79, 0xe8, 0xab,
    0xf1, 0x1e, 0x5c, 0xce, 0xce, 0xb3, 0x50, 0x74, 0x9b, 0xa3, 0x6c, 0x97, 0x52, 0x90, 0x2c, 0x21,
    0xd5, 0x98, 0xa3, 0x91, 0x27, 0x77, 0xcb, 0xc6, 0xb2, 0x5b, 0x95, 0x98, 0x51, 0x31, 0x9d, 0x45,
    0xa6, 0xfd, 0xf4, 0xd5, 0x30, 0x6a, 0x0c, 0xe5, 0x69, 0xb7, 0xbf, 0x55, 0x51, 0x7a, 0x93, 0x32,
    0x74, 0x49, 0xc9, 0xf9, 0xab, 0x73, 0xe2, 0x84, 0x1d, 0x98, 0xf4, 0x1c, 0x28, 0xd0, 0x22, 0xa7,
    0x91, 0x89, 0x3b, 0x6b, 0xb2, 0x93, 0xef, 0xb1, 0x16, 0xbf, 0x42, 0xa4, 0xab, 0xe5, 0x47, 0x98,
    0xeb, 0x6a, 0x5f, 0x76, 0xb6, 0x86, 0x15, 0xfe, 0x6f, 0xa6, 0xba, 0xbd, 0xef, 0x6b, 0xcc, 0x9d,
    0x6a, 0x65, 0x81, 0xa3, 0xfa, 0x86, 0xef, 0xfc, 0x2d, 0xd2, 0xc7, 0x52, 0xf3, 0x52, 0x30, 0x1f,
    0xa9, 0x5c, 0xbb, 0x44, 0x90, 0xdf, 0xda, 0xe0, 0x28, 0x3f, 0x9b, 0x76, 0x83, 0xcb, 0xbf, 0x4e,
    0x44, 0xa9, 0x53, 0x18, 0x4c, 0x54, 0x48, 0xd9, 0x06, 0x44, 0xb1, 0x61, 0x98, 0x4c, 0xcb, 0x34,
    0x0c, 0xea, 0xdf, 0x08, 0x82, 0x45, 0x1f, 0x9d, 0x2a, 0xc8, 0x3d, 0x31, 0xe2, 0xc5, 0xb0, 0xd6,
    0x01, 0x16, 0x78, 0x6d, 0xe3, 0x0e, 0x06, 0x3c, 0x72, 0x3d, 0x92, 0xb5, 0x9c, 0xfe, 0xce, 0x3a,
    0xb3, 0x51, 0xdd, 0x39, 0x0a, 0x26, 0xdc, 0x0b, 0x31, 0x00, 0x1a, 0x6d, 0x44, 0x42, 0x0e, 0x72,
    0xf4, 0x3e, 0x34, 0xb4, 0x8b, 0x00, 0x53, 0x53, 0x88, 0xed, 0xba, 0x60, 0x43, 0xd5, 0x74, 0x94,
    0x2f, 0x25, 0xae, 0xa1, 0x3b, 0xcc, 0x96, 0x3a, 0xe9, 0x42, 0xbd, 0x67, 0x57, 0x5a, 0x03, 0xc8,
    0xeb, 0x74, 0x38, 0xba, 0x9a, 0x28, 0x0d, 0x8c, 0x71, 0x6b, 0x5d, 0xf4, 0xf7, 0x57, 0x84, 0x2f,
    0x32, 0xd8, 0x42, 0x20, 0xed, 0x39, 0xab, 0x4e, 0x52, 0x4b, 0x7c, 0x76, 0x1b, 0x76, 0x69, 0x22,
    0x09, 0x88, 0xf8, 0x27, 0x8f, 0x50, 0xf0, 0x75, 0xa4, 0x37, 0xfe, 0x96, 0x13, 0xe4, 0x53, 0x53,
];

/// What a verified signature attests beyond "this key signed these bytes".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifiedSignature {
    /// The signature's hashed creation time (Unix seconds).
    pub created: u32,
}

/// Why a signature was refused. Every variant is a refusal; none is ever downgraded to
/// acceptance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgpError {
    /// The signature input is longer than [`MAX_SIGNATURE_LEN`].
    TooLarge,
    /// Not exactly one `PGP SIGNATURE` armor block with a strict-base64 body.
    Armor,
    /// A packet, or a field inside one, runs past the end of its input.
    Truncated,
    /// Bytes left over after the last packet, or after a signature's MPI.
    TrailingBytes,
    /// An old-format packet header with length type 3 (indeterminate length).
    IndeterminateLength,
    /// A new-format packet header with a partial body length.
    PartialLength,
    /// A packet that is neither a signature (2) nor a marker (10).
    UnexpectedPacket(u8),
    /// No signature packet at all.
    NoSignature,
    /// More than eight signature packets.
    TooManySignatures,
    /// A signature packet version other than 4.
    UnsupportedVersion(u8),
    /// A signature type other than 0x00 (binary document).
    UnsupportedSignatureType(u8),
    /// A public-key algorithm other than RSA (1).
    UnsupportedPublicKeyAlgorithm(u8),
    /// A hash algorithm other than SHA-512 (10) or SHA-256 (8).
    UnsupportedHash(u8),
    /// A hashed subpacket whose length is zero, runs past its area, or does not fit its type.
    BadSubpacket,
    /// A hashed subpacket marked critical whose type this verifier does not implement.
    UnknownCriticalSubpacket(u8),
    /// A second hashed creation time (2) or issuer fingerprint (33).
    DuplicateSubpacket(u8),
    /// No hashed issuer fingerprint (33).
    MissingIssuer,
    /// The hashed issuer fingerprint is not `0x04 ‖` the pinned key's fingerprint.
    IssuerMismatch,
    /// No hashed creation time (2).
    MissingCreationTime,
    /// Created before the pinned key was.
    CreatedBeforeKey,
    /// Created more than seven days after `now`.
    CreatedInFuture,
    /// A hashed expiration (3) has lapsed.
    Expired,
    /// The MPI is empty, carries more bits than its bit count, or outgrows the modulus.
    BadMpi,
    /// The digest's left 16 bits disagree with the packet's (the cheap reject).
    DigestPrefixMismatch,
    /// The RSA signature does not verify over the document under the pinned key.
    BadSignature,
}

impl PgpError {
    /// How far a refused signature got. With several signatures the furthest refusal is
    /// reported, so one naming another issuer (refused by stage 4) never masks the time or
    /// crypto refusal of one naming the pinned key. The report is unauthenticated: an added
    /// packet that merely claims the pinned issuer outranks a genuine one's time refusal.
    fn stage(self) -> u8 {
        match self {
            Self::TooLarge
            | Self::Armor
            | Self::IndeterminateLength
            | Self::PartialLength
            | Self::UnexpectedPacket(_)
            | Self::NoSignature
            | Self::TooManySignatures => 0,
            Self::UnsupportedVersion(_)
            | Self::UnsupportedSignatureType(_)
            | Self::UnsupportedPublicKeyAlgorithm(_)
            | Self::UnsupportedHash(_) => 1,
            Self::Truncated | Self::TrailingBytes => 2,
            Self::BadSubpacket
            | Self::UnknownCriticalSubpacket(_)
            | Self::DuplicateSubpacket(_) => 3,
            Self::MissingIssuer | Self::IssuerMismatch => 4,
            Self::MissingCreationTime
            | Self::CreatedBeforeKey
            | Self::CreatedInFuture
            | Self::Expired => 5,
            Self::BadMpi | Self::DigestPrefixMismatch | Self::BadSignature => 6,
        }
    }
}

impl std::fmt::Display for PgpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (text, value) = match *self {
            Self::TooLarge => ("signature input is larger than 16 KiB", None),
            Self::Armor => (
                "not exactly one PGP SIGNATURE armor block of strict base64",
                None,
            ),
            Self::Truncated => ("signature packet is truncated", None),
            Self::TrailingBytes => ("unexpected bytes after the signature", None),
            Self::IndeterminateLength => ("packet has an indeterminate length", None),
            Self::PartialLength => ("packet has a partial body length", None),
            Self::UnexpectedPacket(t) => ("unexpected packet tag ", Some(t)),
            Self::NoSignature => ("no signature packet", None),
            Self::TooManySignatures => ("more than 8 signature packets", None),
            Self::UnsupportedVersion(v) => ("unsupported signature version ", Some(v)),
            Self::UnsupportedSignatureType(t) => ("unsupported signature type ", Some(t)),
            Self::UnsupportedPublicKeyAlgorithm(a) => {
                ("unsupported public-key algorithm ", Some(a))
            }
            Self::UnsupportedHash(h) => ("unsupported hash algorithm ", Some(h)),
            Self::BadSubpacket => ("malformed hashed subpacket", None),
            Self::UnknownCriticalSubpacket(t) => ("unknown critical subpacket ", Some(t)),
            Self::DuplicateSubpacket(t) => ("duplicate subpacket ", Some(t)),
            Self::MissingIssuer => ("no hashed issuer fingerprint", None),
            Self::IssuerMismatch => ("signed by a key other than the pinned one", None),
            Self::MissingCreationTime => ("no hashed creation time", None),
            Self::CreatedBeforeKey => ("signature predates its key", None),
            Self::CreatedInFuture => ("signature is dated more than 7 days ahead", None),
            Self::Expired => ("signature has expired", None),
            Self::BadMpi => ("malformed signature MPI", None),
            Self::DigestPrefixMismatch => {
                ("digest does not match the signature's check bits", None)
            }
            Self::BadSignature => ("RSA signature does not verify", None),
        };
        f.write_str("openpgp: ")?;
        f.write_str(text)?;
        match value {
            Some(v) => std::fmt::Display::fmt(&v, f),
            None => Ok(()),
        }
    }
}

impl std::error::Error for PgpError {}

/// Verify `signature` (ASCII-armored or binary) over the exact bytes of `document` under
/// `key`, at wall-clock `now_unix`.
///
/// # Errors
///
/// A framing refusal (armor, packet headers, packet count), or — when no signature packet
/// verifies — the refusal of the one that got furthest.
pub fn verify_detached(
    document: &[u8],
    signature: &[u8],
    key: &PinnedRsaKey,
    now_unix: u64,
) -> Result<VerifiedSignature, PgpError> {
    if signature.len() > MAX_SIGNATURE_LEN {
        return Err(PgpError::TooLarge);
    }
    // A packet header always has its high bit set; armor is ASCII and never does.
    let packets = match signature.first() {
        Some(b) if b & 0x80 != 0 => Cow::Borrowed(signature),
        _ => Cow::Owned(dearmor(signature)?),
    };
    let mut refusal: Option<PgpError> = None;
    for body in signature_packets(&packets)? {
        match verify_one(document, body, key, now_unix) {
            Ok(created) => return Ok(VerifiedSignature { created }),
            Err(e) => {
                if refusal.is_none_or(|r| e.stage() > r.stage()) {
                    refusal = Some(e);
                }
            }
        }
    }
    Err(refusal.unwrap_or(PgpError::NoSignature))
}

/// A bounds-checked cursor: every read either yields its bytes or `None`, never panics.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, tail) = self.0.split_at_checked(n)?;
        self.0 = tail;
        Some(head)
    }

    fn u8(&mut self) -> Option<u8> {
        let (&b, tail) = self.0.split_first()?;
        self.0 = tail;
        Some(b)
    }

    fn be16(&mut self) -> Option<u16> {
        let (b, tail) = self.0.split_first_chunk::<2>()?;
        self.0 = tail;
        Some(u16::from_be_bytes(*b))
    }

    fn be32(&mut self) -> Option<u32> {
        let (b, tail) = self.0.split_first_chunk::<4>()?;
        self.0 = tail;
        Some(u32::from_be_bytes(*b))
    }
}

/// The binary packets inside exactly one armored `PGP SIGNATURE` block. Only blank lines
/// may surround the block; header lines (`Key: value`) directly after BEGIN are skipped;
/// the CRC24 line (the last non-blank body line, starting with `=`) is dropped unread.
fn dearmor(text: &[u8]) -> Result<Vec<u8>, PgpError> {
    let mut lines = text.split(|&b| b == b'\n').map(<[u8]>::trim_ascii_end);
    loop {
        match lines.next() {
            Some([]) => {}
            Some(line) if line == ARMOR_BEGIN => break,
            _ => return Err(PgpError::Armor),
        }
    }
    let mut body: Vec<&[u8]> = Vec::new();
    let mut in_headers = true;
    loop {
        let line = lines.next().ok_or(PgpError::Armor)?;
        if line == ARMOR_END {
            break;
        }
        if line.starts_with(b"-----") {
            return Err(PgpError::Armor);
        }
        if in_headers && line.contains(&b':') {
            continue;
        }
        in_headers = false;
        if !line.is_empty() {
            body.push(line);
        }
    }
    if lines.any(|line| !line.is_empty()) {
        return Err(PgpError::Armor);
    }
    if body.last().is_some_and(|line| line.starts_with(b"=")) {
        body.pop();
    }
    aterm_codec::base64::decode_strict(&body.concat()).map_err(|_| PgpError::Armor)
}

/// The bodies of the signature packets in `bin`; marker packets are skipped and anything
/// else refuses the whole input.
fn signature_packets(bin: &[u8]) -> Result<Vec<&[u8]>, PgpError> {
    let mut r = Reader(bin);
    let mut bodies = Vec::new();
    while let Some(header) = r.u8() {
        if header & 0x80 == 0 {
            return Err(PgpError::TrailingBytes);
        }
        let (tag, len) = if header & 0x40 != 0 {
            let first = r.u8().ok_or(PgpError::Truncated)?;
            let len = match first {
                0..=191 => usize::from(first),
                192..=223 => {
                    let second = r.u8().ok_or(PgpError::Truncated)?;
                    ((usize::from(first) - 192) << 8) + usize::from(second) + 192
                }
                255 => be32_len(r.be32())?,
                _ => return Err(PgpError::PartialLength),
            };
            (header & 0x3F, len)
        } else {
            let len = match header & 0x03 {
                0 => usize::from(r.u8().ok_or(PgpError::Truncated)?),
                1 => usize::from(r.be16().ok_or(PgpError::Truncated)?),
                2 => be32_len(r.be32())?,
                _ => return Err(PgpError::IndeterminateLength),
            };
            ((header >> 2) & 0x0F, len)
        };
        let body = r.take(len).ok_or(PgpError::Truncated)?;
        match tag {
            TAG_SIGNATURE if bodies.len() == MAX_SIGNATURES => {
                return Err(PgpError::TooManySignatures);
            }
            TAG_SIGNATURE => bodies.push(body),
            TAG_MARKER => {}
            other => return Err(PgpError::UnexpectedPacket(other)),
        }
    }
    if bodies.is_empty() {
        return Err(PgpError::NoSignature);
    }
    Ok(bodies)
}

fn be32_len(len: Option<u32>) -> Result<usize, PgpError> {
    usize::try_from(len.ok_or(PgpError::Truncated)?).map_err(|_| PgpError::Truncated)
}

/// The digest and ring verification parameters for an accepted hash algorithm.
#[derive(Clone, Copy)]
struct Hash {
    digest: &'static digest::Algorithm,
    rsa: &'static RsaParameters,
}

fn hash_for(id: u8) -> Result<Hash, PgpError> {
    match id {
        HASH_SHA512 => Ok(Hash {
            digest: &digest::SHA512,
            rsa: &signature::RSA_PKCS1_2048_8192_SHA512,
        }),
        HASH_SHA256 => Ok(Hash {
            digest: &digest::SHA256,
            rsa: &signature::RSA_PKCS1_2048_8192_SHA256,
        }),
        other => Err(PgpError::UnsupportedHash(other)),
    }
}

/// A v4 RSA signature packet body, split into its fields.
struct SignaturePacket<'a> {
    hash: Hash,
    /// `body[0..6 + hashed_len]`: version through the hashed subpackets — the hashed part.
    hashed_prefix: &'a [u8],
    hashed_area: &'a [u8],
    left16: &'a [u8],
    mpi_bits: u16,
    mpi: &'a [u8],
}

impl<'a> SignaturePacket<'a> {
    fn parse(body: &'a [u8]) -> Result<Self, PgpError> {
        let mut r = Reader(body);
        let version = r.u8().ok_or(PgpError::Truncated)?;
        if version != SIG_VERSION {
            return Err(PgpError::UnsupportedVersion(version));
        }
        let sig_type = r.u8().ok_or(PgpError::Truncated)?;
        if sig_type != SIG_TYPE_BINARY {
            return Err(PgpError::UnsupportedSignatureType(sig_type));
        }
        let pkalg = r.u8().ok_or(PgpError::Truncated)?;
        if pkalg != PKALG_RSA {
            return Err(PgpError::UnsupportedPublicKeyAlgorithm(pkalg));
        }
        let hash = hash_for(r.u8().ok_or(PgpError::Truncated)?)?;
        let hashed_len = usize::from(r.be16().ok_or(PgpError::Truncated)?);
        let hashed_area = r.take(hashed_len).ok_or(PgpError::Truncated)?;
        let hashed_prefix = body.get(..6 + hashed_len).ok_or(PgpError::Truncated)?;
        let unhashed_len = usize::from(r.be16().ok_or(PgpError::Truncated)?);
        r.take(unhashed_len).ok_or(PgpError::Truncated)?;
        let left16 = r.take(2).ok_or(PgpError::Truncated)?;
        let mpi_bits = r.be16().ok_or(PgpError::Truncated)?;
        let mpi = r
            .take(usize::from(mpi_bits).div_ceil(8))
            .ok_or(PgpError::Truncated)?;
        if !r.is_empty() {
            return Err(PgpError::TrailingBytes);
        }
        Ok(Self {
            hash,
            hashed_prefix,
            hashed_area,
            left16,
            mpi_bits,
            mpi,
        })
    }
}

/// The hashed subpackets this verifier acts on.
struct Hashed<'a> {
    created: Option<u32>,
    /// The shortest non-zero expiration; zero means "never expires".
    expires_after: Option<u32>,
    issuer: Option<&'a [u8]>,
}

fn hashed_subpackets(area: &[u8]) -> Result<Hashed<'_>, PgpError> {
    let mut r = Reader(area);
    let mut out = Hashed {
        created: None,
        expires_after: None,
        issuer: None,
    };
    while !r.is_empty() {
        let first = r.u8().ok_or(PgpError::BadSubpacket)?;
        let len = match first {
            0..=191 => usize::from(first),
            192..=254 => {
                let second = r.u8().ok_or(PgpError::BadSubpacket)?;
                ((usize::from(first) - 192) << 8) + usize::from(second) + 192
            }
            255 => usize::try_from(r.be32().ok_or(PgpError::BadSubpacket)?)
                .map_err(|_| PgpError::BadSubpacket)?,
        };
        let subpacket = r.take(len).ok_or(PgpError::BadSubpacket)?;
        let (&kind, data) = subpacket.split_first().ok_or(PgpError::BadSubpacket)?;
        match kind & 0x7F {
            SUBPACKET_CREATION_TIME => {
                if out.created.is_some() {
                    return Err(PgpError::DuplicateSubpacket(SUBPACKET_CREATION_TIME));
                }
                out.created = Some(subpacket_u32(data)?);
            }
            SUBPACKET_EXPIRATION => {
                let secs = subpacket_u32(data)?;
                if secs != 0 {
                    out.expires_after = Some(out.expires_after.map_or(secs, |e| e.min(secs)));
                }
            }
            SUBPACKET_ISSUER_FINGERPRINT => {
                if out.issuer.is_some() {
                    return Err(PgpError::DuplicateSubpacket(SUBPACKET_ISSUER_FINGERPRINT));
                }
                out.issuer = Some(data);
            }
            other if kind & 0x80 != 0 => return Err(PgpError::UnknownCriticalSubpacket(other)),
            _ => {}
        }
    }
    Ok(out)
}

fn subpacket_u32(data: &[u8]) -> Result<u32, PgpError> {
    <[u8; 4]>::try_from(data)
        .map(u32::from_be_bytes)
        .map_err(|_| PgpError::BadSubpacket)
}

/// The MPI left-padded to `modulus_len` bytes. It must be non-empty, carry no bit above
/// its declared count, and be no longer than the modulus; leading zero bytes and an
/// overstated count are v4-legal (see the module rules) and accepted.
fn left_padded(bits: u16, mpi: &[u8], modulus_len: usize) -> Result<Vec<u8>, PgpError> {
    let (&top, _) = mpi.split_first().ok_or(PgpError::BadMpi)?;
    let pad = modulus_len.checked_sub(mpi.len()).ok_or(PgpError::BadMpi)?;
    let len_bits = u32::try_from(mpi.len())
        .ok()
        .and_then(|len| len.checked_mul(8))
        .ok_or(PgpError::BadMpi)?;
    if len_bits - top.leading_zeros() > u32::from(bits) {
        return Err(PgpError::BadMpi);
    }
    let mut out = vec![0; pad];
    out.extend_from_slice(mpi);
    Ok(out)
}

/// One signature packet against `key`: the checks run cheapest first, the RSA last.
fn verify_one(
    document: &[u8],
    body: &[u8],
    key: &PinnedRsaKey,
    now_unix: u64,
) -> Result<u32, PgpError> {
    let sig = SignaturePacket::parse(body)?;
    let hashed = hashed_subpackets(sig.hashed_area)?;

    let issuer = hashed.issuer.ok_or(PgpError::MissingIssuer)?;
    if issuer.split_first() != Some((&FINGERPRINT_V4, key.fingerprint.as_slice())) {
        return Err(PgpError::IssuerMismatch);
    }

    let created = hashed.created.ok_or(PgpError::MissingCreationTime)?;
    if created < key.created {
        return Err(PgpError::CreatedBeforeKey);
    }
    if u64::from(created) > now_unix.saturating_add(FUTURE_SKEW_SECS) {
        return Err(PgpError::CreatedInFuture);
    }
    if hashed
        .expires_after
        .is_some_and(|secs| u64::from(created) + u64::from(secs) <= now_unix)
    {
        return Err(PgpError::Expired);
    }

    let padded = left_padded(sig.mpi_bits, sig.mpi, key.n.len())?;
    let hashed_count = u32::try_from(sig.hashed_prefix.len()).map_err(|_| PgpError::Truncated)?;
    let mut message = Vec::with_capacity(document.len() + sig.hashed_prefix.len() + 6);
    message.extend_from_slice(document);
    message.extend_from_slice(sig.hashed_prefix);
    message.extend_from_slice(&[SIG_VERSION, 0xFF]);
    message.extend_from_slice(&hashed_count.to_be_bytes());

    if digest::digest(sig.hash.digest, &message).as_ref().get(..2) != Some(sig.left16) {
        return Err(PgpError::DigestPrefixMismatch);
    }
    RsaPublicKeyComponents { n: key.n, e: key.e }
        .verify(sig.hash.rsa, &message, &padded)
        .map_err(|_| PgpError::BadSignature)?;
    Ok(created)
}

/// The committed test RSA-4096 key and a minimal v4 signer over it: what other modules'
/// tests sign synthetic vendor documents with. It authorizes nothing outside a test.
#[cfg(test)]
pub(crate) mod testkit {
    use std::sync::LazyLock;

    use ring::rand::SystemRandom;
    use ring::signature::RsaKeyPair;

    use super::*;

    /// A throwaway RSA-4096 key (`openssl genpkey`, PKCS#8 DER) that signs the generated
    /// vectors; it authorizes nothing.
    pub(crate) const TEST_KEY_PKCS8: &[u8] =
        include_bytes!("vendor_direct/fixtures/openpgp-test-rsa4096.pk8.der");

    pub(crate) const TEST_KEY_CREATED: u32 = 1_700_000_000;

    pub(crate) struct TestKey {
        pub(crate) pair: RsaKeyPair,
        pub(crate) pinned: PinnedRsaKey,
    }

    pub(crate) static TEST_KEY: LazyLock<TestKey> = LazyLock::new(|| {
        let pair = RsaKeyPair::from_pkcs8(TEST_KEY_PKCS8).expect("the test key parses");
        let public = RsaPublicKeyComponents::<Vec<u8>>::from(pair.public());
        let n: &'static [u8] = public.n.leak();
        let e: &'static [u8] = public.e.leak();
        let fingerprint = v4_fingerprint(&key_packet_body(TEST_KEY_CREATED, n, e));
        TestKey {
            pair,
            pinned: PinnedRsaKey {
                n,
                e,
                created: TEST_KEY_CREATED,
                fingerprint,
            },
        }
    });

    /// An OpenPGP MPI: a bit count, then the value's big-endian bytes without leading zeros.
    pub(crate) fn mpi(value: &[u8]) -> Vec<u8> {
        let start = value.iter().position(|&b| b != 0).unwrap_or(value.len());
        let digits = &value[start..];
        let bits = digits
            .first()
            .map_or(0, |top| 8 * digits.len() - top.leading_zeros() as usize);
        let mut out = u16::try_from(bits).unwrap().to_be_bytes().to_vec();
        out.extend_from_slice(digits);
        out
    }

    pub(crate) fn key_packet_body(created: u32, n: &[u8], e: &[u8]) -> Vec<u8> {
        let mut body = vec![4];
        body.extend_from_slice(&created.to_be_bytes());
        body.push(PKALG_RSA);
        body.extend(mpi(n));
        body.extend(mpi(e));
        body
    }

    pub(crate) fn v4_fingerprint(key_body: &[u8]) -> [u8; 20] {
        let mut ctx = digest::Context::new(&digest::SHA1_FOR_LEGACY_USE_ONLY);
        ctx.update(&[0x99]);
        ctx.update(&u16::try_from(key_body.len()).unwrap().to_be_bytes());
        ctx.update(key_body);
        ctx.finish().as_ref().try_into().unwrap()
    }

    /// A subpacket with a one-octet length.
    pub(crate) fn subpacket(kind: u8, data: &[u8]) -> Vec<u8> {
        let len = u8::try_from(data.len() + 1).unwrap();
        assert!(len < 192);
        [&[len, kind][..], data].concat()
    }

    pub(crate) fn issuer(fingerprint: &[u8; 20]) -> Vec<u8> {
        subpacket(
            SUBPACKET_ISSUER_FINGERPRINT,
            &[&[4][..], fingerprint].concat(),
        )
    }

    pub(crate) fn created_at(t: u32) -> Vec<u8> {
        subpacket(SUBPACKET_CREATION_TIME, &t.to_be_bytes())
    }

    /// The test key as a pinned key.
    #[must_use]
    pub(crate) fn test_key() -> PinnedRsaKey {
        TEST_KEY.pinned
    }

    /// A binary v4 detached signature over `doc` by the test key: SHA-512, the issuer
    /// fingerprint and `created` hashed, nothing unhashed.
    #[must_use]
    pub(crate) fn sign_detached(doc: &[u8], created: u32) -> Vec<u8> {
        let hashed = [issuer(&TEST_KEY.pinned.fingerprint), created_at(created)].concat();
        let mut body = vec![SIG_VERSION, SIG_TYPE_BINARY, PKALG_RSA, HASH_SHA512];
        body.extend_from_slice(&u16::try_from(hashed.len()).unwrap().to_be_bytes());
        body.extend_from_slice(&hashed);
        let count = u32::try_from(body.len()).unwrap();
        let message = [doc, &body, &[SIG_VERSION, 0xFF], &count.to_be_bytes()].concat();
        let mut raw = vec![0; TEST_KEY.pair.public().modulus_len()];
        TEST_KEY
            .pair
            .sign(
                &ring::signature::RSA_PKCS1_SHA512,
                &SystemRandom::new(),
                &message,
                &mut raw,
            )
            .unwrap();
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(&digest::digest(&digest::SHA512, &message).as_ref()[..2]);
        body.extend(mpi(&raw));
        let len = u32::try_from(body.len()).unwrap();
        let mut packet = vec![0xC0 | TAG_SIGNATURE, 0xFF];
        packet.extend_from_slice(&len.to_be_bytes());
        packet.extend(body);
        packet
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use ring::rand::SystemRandom;

    use super::testkit::*;
    use super::*;

    const ANTHROPIC_ASC: &str = include_str!("vendor_direct/fixtures/anthropic-claude-code.asc");
    const MANIFEST_280: &[u8] =
        include_bytes!("vendor_direct/fixtures/claude-2.1.280-manifest.json");
    const SIG_280: &[u8] =
        include_bytes!("vendor_direct/fixtures/claude-2.1.280-manifest.json.sig");
    const MANIFEST_278: &[u8] =
        include_bytes!("vendor_direct/fixtures/claude-2.1.278-manifest.json");
    const SIG_278: &[u8] =
        include_bytes!("vendor_direct/fixtures/claude-2.1.278-manifest.json.sig");

    /// 2026-09-22T00:00:00Z: a fixed clock after both real signatures.
    const NOW: u64 = 1_790_035_200;
    const SIG_280_CREATED: u32 = 1_790_024_226;
    const SIG_278_CREATED: u32 = 1_789_781_019;
    const TEST_SIG_CREATED: u32 = 1_780_000_000;
    const DOC: &[u8] = b"{\"version\":\"9.9.9\"}\n";

    fn test_fingerprint() -> [u8; 20] {
        TEST_KEY.pinned.fingerprint
    }

    fn verify_test(doc: &[u8], sig: &[u8]) -> Result<VerifiedSignature, PgpError> {
        verify_detached(doc, sig, &TEST_KEY.pinned, NOW)
    }

    fn verify_anthropic(doc: &[u8], sig: &[u8]) -> Result<VerifiedSignature, PgpError> {
        verify_detached(doc, sig, &ANTHROPIC_CLAUDE_CODE_RELEASE_KEY, NOW)
    }

    const TEST_OK: Result<VerifiedSignature, PgpError> = Ok(VerifiedSignature {
        created: TEST_SIG_CREATED,
    });

    fn expires_after(secs: u32) -> Vec<u8> {
        subpacket(SUBPACKET_EXPIRATION, &secs.to_be_bytes())
    }

    /// The hashed area a well-formed test signature carries.
    fn good_hashed() -> Vec<u8> {
        [issuer(&test_fingerprint()), created_at(TEST_SIG_CREATED)].concat()
    }

    /// Everything a signer chooses; `trailer_delta` skews the hashed-length trailer the
    /// signer hashes, to prove the verifier's trailer is exact.
    struct Spec {
        version: u8,
        sig_type: u8,
        pkalg: u8,
        hash: u8,
        hashed: Vec<u8>,
        unhashed: Vec<u8>,
        trailer_delta: i64,
    }

    impl Spec {
        fn good() -> Self {
            Self {
                version: 4,
                sig_type: SIG_TYPE_BINARY,
                pkalg: PKALG_RSA,
                hash: HASH_SHA512,
                hashed: good_hashed(),
                unhashed: subpacket(16, &test_fingerprint()[12..]),
                trailer_delta: 0,
            }
        }

        fn hashed(hashed: Vec<u8>) -> Self {
            Self {
                hashed,
                ..Self::good()
            }
        }
    }

    fn algorithms(
        hash: u8,
    ) -> (
        &'static digest::Algorithm,
        &'static dyn signature::RsaEncoding,
    ) {
        if hash == HASH_SHA256 {
            (&digest::SHA256, &signature::RSA_PKCS1_SHA256)
        } else {
            (&digest::SHA512, &signature::RSA_PKCS1_SHA512)
        }
    }

    /// The v4 signature packet body `spec` describes, signed over `doc` by the test key.
    fn sign_body(doc: &[u8], spec: &Spec) -> Vec<u8> {
        let mut prefix = vec![spec.version, spec.sig_type, spec.pkalg, spec.hash];
        prefix.extend_from_slice(&u16::try_from(spec.hashed.len()).unwrap().to_be_bytes());
        prefix.extend_from_slice(&spec.hashed);
        let count = i64::try_from(prefix.len()).unwrap() + spec.trailer_delta;
        let message = [
            doc,
            &prefix,
            &[4, 0xFF],
            &u32::try_from(count).unwrap().to_be_bytes(),
        ]
        .concat();
        let (alg, encoding) = algorithms(spec.hash);
        let mut raw = vec![0; TEST_KEY.pair.public().modulus_len()];
        TEST_KEY
            .pair
            .sign(encoding, &SystemRandom::new(), &message, &mut raw)
            .unwrap();
        let mut body = prefix;
        body.extend_from_slice(&u16::try_from(spec.unhashed.len()).unwrap().to_be_bytes());
        body.extend_from_slice(&spec.unhashed);
        body.extend_from_slice(&digest::digest(alg, &message).as_ref()[..2]);
        body.extend(mpi(&raw));
        body
    }

    /// A new-format packet.
    fn packet(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![0xC0 | tag];
        let n = body.len();
        if n < 192 {
            out.push(u8::try_from(n).unwrap());
        } else if n < 8384 {
            let m = n - 192;
            out.push(u8::try_from((m >> 8) + 192).unwrap());
            out.push(u8::try_from(m & 0xFF).unwrap());
        } else {
            out.push(0xFF);
            out.extend(u32::try_from(n).unwrap().to_be_bytes());
        }
        out.extend_from_slice(body);
        out
    }

    /// An old-format packet with a two-octet length, as GnuPG writes one.
    fn old_packet(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![0x80 | (tag << 2) | 1];
        out.extend(u16::try_from(body.len()).unwrap().to_be_bytes());
        out.extend_from_slice(body);
        out
    }

    fn signed(doc: &[u8], spec: &Spec) -> Vec<u8> {
        packet(TAG_SIGNATURE, &sign_body(doc, spec))
    }

    fn refusal(spec: &Spec) -> PgpError {
        verify_test(DOC, &signed(DOC, spec)).unwrap_err()
    }

    /// `(end of the hashed area, start of left-16)` inside a v4 signature body.
    fn layout(body: &[u8]) -> (usize, usize) {
        let hashed_end = 6 + usize::from(u16::from_be_bytes([body[4], body[5]]));
        let unhashed_len =
            usize::from(u16::from_be_bytes([body[hashed_end], body[hashed_end + 1]]));
        (hashed_end, hashed_end + 2 + unhashed_len)
    }

    /// What the verifier hashes for `body` over `doc`.
    fn verifier_message(body: &[u8], doc: &[u8]) -> Vec<u8> {
        let (hashed_end, _) = layout(body);
        [
            doc,
            &body[..hashed_end],
            &[4, 0xFF],
            &u32::try_from(hashed_end).unwrap().to_be_bytes(),
        ]
        .concat()
    }

    /// `body` with its left-16 recomputed for `doc`, so only the RSA check can refuse it.
    fn with_left16_for(body: &[u8], doc: &[u8]) -> Vec<u8> {
        let (_, left16) = layout(body);
        let (alg, _) = algorithms(body[3]);
        let digest = digest::digest(alg, &verifier_message(body, doc));
        let mut out = body.to_vec();
        out[left16..left16 + 2].copy_from_slice(&digest.as_ref()[..2]);
        out
    }

    fn with_unhashed(body: &[u8], unhashed: &[u8]) -> Vec<u8> {
        let (hashed_end, left16) = layout(body);
        [
            &body[..hashed_end],
            &u16::try_from(unhashed.len()).unwrap().to_be_bytes(),
            unhashed,
            &body[left16..],
        ]
        .concat()
    }

    /// `body` with its MPI (bit count included) replaced by `field`.
    fn with_mpi_field(body: &[u8], field: &[u8]) -> Vec<u8> {
        let (_, left16) = layout(body);
        [&body[..left16 + 2], field].concat()
    }

    /// The real 2.1.280 signature as one binary packet — a signature by a key other than
    /// the test key.
    fn foreign_packet() -> Vec<u8> {
        dearmor(SIG_280).unwrap()
    }

    fn sig_280_text() -> &'static str {
        std::str::from_utf8(SIG_280).unwrap()
    }

    #[test]
    fn real_2_1_280_manifest_verifies_under_the_compiled_key() {
        assert_eq!(
            verify_anthropic(MANIFEST_280, SIG_280),
            Ok(VerifiedSignature {
                created: SIG_280_CREATED
            })
        );
    }

    #[test]
    fn real_2_1_278_manifest_verifies_under_the_compiled_key() {
        assert_eq!(
            verify_anthropic(MANIFEST_278, SIG_278),
            Ok(VerifiedSignature {
                created: SIG_278_CREATED
            })
        );
    }

    #[test]
    fn a_real_signature_binds_only_its_own_release() {
        assert_eq!(
            verify_anthropic(MANIFEST_278, SIG_280),
            Err(PgpError::DigestPrefixMismatch)
        );
        let mut flipped = MANIFEST_280.to_vec();
        flipped[30] ^= 0x01;
        assert_eq!(
            verify_anthropic(&flipped, SIG_280),
            Err(PgpError::DigestPrefixMismatch)
        );
        let binary = foreign_packet();
        assert_eq!(
            verify_anthropic(
                &flipped,
                &[&binary[..3], &with_left16_for(&binary[3..], &flipped)].concat()
            ),
            Err(PgpError::BadSignature)
        );
    }

    #[test]
    fn a_real_signature_verifies_in_binary_form() {
        let binary = foreign_packet();
        assert_eq!(
            binary[0], 0x89,
            "GnuPG writes an old-format two-octet header"
        );
        assert_eq!(
            verify_anthropic(MANIFEST_280, &binary),
            Ok(VerifiedSignature {
                created: SIG_280_CREATED
            })
        );
    }

    #[test]
    fn compiled_key_is_the_committed_asc() {
        let block = ANTHROPIC_ASC.replace("PUBLIC KEY BLOCK", "SIGNATURE");
        let binary = dearmor(block.as_bytes()).unwrap();
        assert_eq!(
            binary[0], 0x99,
            "an old-format public-key packet, two-octet length"
        );
        let len = usize::from(u16::from_be_bytes([binary[1], binary[2]]));
        let body = &binary[3..3 + len];
        let mut r = Reader(body);
        assert_eq!(r.u8(), Some(4));
        let created = r.be32().unwrap();
        assert_eq!(r.u8(), Some(PKALG_RSA));
        let n_bits = r.be16().unwrap();
        let n = r.take(usize::from(n_bits).div_ceil(8)).unwrap();
        let e_bits = r.be16().unwrap();
        let e = r.take(usize::from(e_bits).div_ceil(8)).unwrap();
        assert!(r.is_empty());

        let key = ANTHROPIC_CLAUDE_CODE_RELEASE_KEY;
        assert_eq!(n_bits, 4096);
        assert_eq!(n, key.n);
        assert_eq!(e, key.e);
        assert_eq!(created, key.created);
        assert_eq!(v4_fingerprint(body), key.fingerprint);
        let hex: String = key.fingerprint.iter().map(|b| format!("{b:02X}")).collect();
        assert_eq!(hex, "31DDDE24DDFAB679F42D7BD2BAA929FF1A7ECACE");
        assert_eq!(
            key_packet_body(created, n, e),
            body,
            "the test-key encoder agrees"
        );
    }

    #[test]
    fn armor_crc_is_never_consulted() {
        let text = sig_280_text();
        let crc = text.lines().find(|l| l.starts_with('=')).unwrap();
        assert_ne!(crc, "=AAAA");
        let expected = Ok(VerifiedSignature {
            created: SIG_280_CREATED,
        });
        let wrong = text.replace(crc, "=AAAA");
        assert_eq!(verify_anthropic(MANIFEST_280, wrong.as_bytes()), expected);
        let absent = text.replace(&format!("{crc}\n"), "");
        assert_eq!(verify_anthropic(MANIFEST_280, absent.as_bytes()), expected);
    }

    #[test]
    fn armor_headers_crlf_and_surrounding_blank_lines_are_tolerated() {
        let text = sig_280_text();
        let expected = Ok(VerifiedSignature {
            created: SIG_280_CREATED,
        });
        let with_headers = text.replacen(
            "-----\n\n",
            "-----\nVersion: GnuPG v2\nComment: a note\n\n",
            1,
        );
        assert_ne!(with_headers, text);
        assert_eq!(
            verify_anthropic(MANIFEST_280, with_headers.as_bytes()),
            expected
        );
        let crlf = text.replace('\n', "\r\n");
        assert_eq!(verify_anthropic(MANIFEST_280, crlf.as_bytes()), expected);
        let padded = format!("\n\n{text}\n\n");
        assert_eq!(verify_anthropic(MANIFEST_280, padded.as_bytes()), expected);
    }

    #[test]
    fn armor_that_is_not_exactly_one_signature_block_is_refused() {
        let text = sig_280_text();
        let cases = [
            ("empty", String::new()),
            ("two blocks", format!("{text}{text}")),
            ("prose before", format!("signed:\n{text}")),
            ("prose after", format!("{text}thanks\n")),
            ("no END", text.replace("-----END PGP SIGNATURE-----", "")),
            ("another kind", text.replace("PGP SIGNATURE", "PGP MESSAGE")),
            (
                "a nested BEGIN",
                text.replacen("\n\n", "\n\n-----BEGIN PGP SIGNATURE-----\n", 1),
            ),
            ("unpadded base64", text.replacen("iQIz", "iQI", 1)),
            ("a space in the body", text.replacen("iQIz", "iQ Iz", 1)),
        ];
        for (what, case) in cases {
            assert_ne!(case, text, "{what}");
            assert_eq!(
                verify_anthropic(MANIFEST_280, case.as_bytes()),
                Err(PgpError::Armor),
                "{what}"
            );
        }
    }

    #[test]
    fn every_packet_length_encoding_is_read() {
        let body = sign_body(DOC, &Spec::good());
        let len = u32::try_from(body.len()).unwrap().to_be_bytes();
        let new_two = packet(TAG_SIGNATURE, &body);
        assert_eq!(new_two[1] & 0xE0, 0xC0, "a two-octet new-format length");
        let framings = [
            ("new, two octets", new_two.clone()),
            (
                "new, five octets",
                [&[0xC0 | TAG_SIGNATURE, 0xFF][..], &len, &body].concat(),
            ),
            ("old, two octets", old_packet(TAG_SIGNATURE, &body)),
            (
                "old, four octets",
                [&[0x80 | (TAG_SIGNATURE << 2) | 2][..], &len, &body].concat(),
            ),
            (
                "old, one octet (a signature never fits one, so a marker carries it)",
                [&[0x80 | (TAG_MARKER << 2), 3][..], b"PGP", &new_two].concat(),
            ),
        ];
        for (what, sig) in framings {
            assert_eq!(verify_test(DOC, &sig), TEST_OK, "{what}");
        }
        let truncated_headers: [&[u8]; 5] = [
            &[0xC0 | TAG_SIGNATURE, 0xC0],
            &[0xC0 | TAG_SIGNATURE, 0xFF, 0, 0],
            &[0x80 | (TAG_MARKER << 2)],
            &[0x80 | (TAG_SIGNATURE << 2) | 1, 0],
            &[0x80 | (TAG_SIGNATURE << 2) | 2, 0, 0],
        ];
        for header in truncated_headers {
            assert_eq!(
                verify_test(DOC, header),
                Err(PgpError::Truncated),
                "{header:02x?}"
            );
        }
    }

    #[test]
    fn sha256_signature_verifies() {
        let spec = Spec {
            hash: HASH_SHA256,
            ..Spec::good()
        };
        assert_eq!(verify_test(DOC, &signed(DOC, &spec)), TEST_OK);
    }

    /// Signs `short-mpi-0`, `short-mpi-1`, … until a signature's leading byte is zero (about
    /// one document in 220), so its MPI is shorter than the modulus.
    #[test]
    fn a_short_mpi_is_left_padded_to_the_modulus() {
        let modulus_len = TEST_KEY.pinned.n.len();
        let (doc, body) = (0..4096)
            .map(|i| {
                let doc = format!("short-mpi-{i}");
                let body = sign_body(doc.as_bytes(), &Spec::good());
                (doc, body)
            })
            .find(|(_, body)| body.len() - layout(body).1 - 4 < modulus_len)
            .expect("a short signature among 4096 documents");
        let (_, left16) = layout(&body);
        let mpi_bytes = &body[left16 + 4..];
        assert_eq!(
            verify_test(doc.as_bytes(), &packet(TAG_SIGNATURE, &body)),
            TEST_OK
        );
        let bits = u16::try_from(8 * modulus_len).unwrap().to_be_bytes();
        let pad = vec![0; modulus_len - mpi_bytes.len()];
        let fixed_length = [&bits[..], &pad, mpi_bytes].concat();
        assert_eq!(
            verify_test(
                doc.as_bytes(),
                &packet(TAG_SIGNATURE, &with_mpi_field(&body, &fixed_length))
            ),
            TEST_OK,
            "a fixed-length encoding (overstated count, leading zero byte) is v4-legal"
        );
        let unpadded = RsaPublicKeyComponents {
            n: TEST_KEY.pinned.n,
            e: TEST_KEY.pinned.e,
        }
        .verify(
            &signature::RSA_PKCS1_2048_8192_SHA512,
            &verifier_message(&body, doc.as_bytes()),
            mpi_bytes,
        );
        assert!(unpadded.is_err(), "the padding is load-bearing");
    }

    #[test]
    fn a_flipped_document_byte_is_refused() {
        let body = sign_body(DOC, &Spec::good());
        let mut doc = DOC.to_vec();
        doc[3] ^= 0x01;
        assert_eq!(
            verify_test(&doc, &packet(TAG_SIGNATURE, &body)),
            Err(PgpError::DigestPrefixMismatch)
        );
        assert_eq!(
            verify_test(&doc, &packet(TAG_SIGNATURE, &with_left16_for(&body, &doc))),
            Err(PgpError::BadSignature)
        );
    }

    #[test]
    fn a_flipped_mpi_byte_is_refused() {
        let mut body = sign_body(DOC, &Spec::good());
        *body.last_mut().unwrap() ^= 0x01;
        assert_eq!(
            verify_test(DOC, &packet(TAG_SIGNATURE, &body)),
            Err(PgpError::BadSignature)
        );
    }

    #[test]
    fn a_flipped_hashed_subpacket_byte_is_refused() {
        let spec = Spec::hashed([good_hashed(), subpacket(101, b"release notes")].concat());
        let body = sign_body(DOC, &spec);
        assert_eq!(verify_test(DOC, &packet(TAG_SIGNATURE, &body)), TEST_OK);
        let (hashed_end, _) = layout(&body);
        let mut flipped = body.clone();
        flipped[hashed_end - 1] ^= 0x01;
        assert_eq!(
            verify_test(DOC, &packet(TAG_SIGNATURE, &flipped)),
            Err(PgpError::DigestPrefixMismatch)
        );
        assert_eq!(
            verify_test(DOC, &packet(TAG_SIGNATURE, &with_left16_for(&flipped, DOC))),
            Err(PgpError::BadSignature)
        );
    }

    #[test]
    fn the_unhashed_area_is_never_read() {
        let body = sign_body(DOC, &Spec::good());
        let mutations = [
            Vec::new(),
            vec![0xFF; 40],
            subpacket(0x80 | 101, b"critical and unknown"),
            issuer(&[0x11; 20]),
            [created_at(0), created_at(u32::MAX)].concat(),
            expires_after(1),
        ];
        for unhashed in mutations {
            assert_eq!(
                verify_test(
                    DOC,
                    &packet(TAG_SIGNATURE, &with_unhashed(&body, &unhashed))
                ),
                TEST_OK,
                "{unhashed:02x?}"
            );
        }
    }

    #[test]
    fn a_signature_by_another_key_is_refused() {
        let sig = signed(DOC, &Spec::good());
        assert_eq!(verify_anthropic(DOC, &sig), Err(PgpError::IssuerMismatch));
        assert_eq!(
            verify_detached(MANIFEST_280, SIG_280, &TEST_KEY.pinned, NOW),
            Err(PgpError::IssuerMismatch)
        );
        let impostor = PinnedRsaKey {
            n: ANTHROPIC_CLAUDE_CODE_RELEASE_KEY.n,
            e: ANTHROPIC_CLAUDE_CODE_RELEASE_KEY.e,
            ..TEST_KEY.pinned
        };
        assert_eq!(
            verify_detached(DOC, &sig, &impostor, NOW),
            Err(PgpError::BadSignature),
            "the modulus, not just the fingerprint, decides"
        );
    }

    #[test]
    fn unsupported_header_fields_are_refused() {
        let cases = [
            (
                Spec {
                    sig_type: 0x01,
                    ..Spec::good()
                },
                PgpError::UnsupportedSignatureType(0x01),
            ),
            (
                Spec {
                    version: 5,
                    ..Spec::good()
                },
                PgpError::UnsupportedVersion(5),
            ),
            (
                Spec {
                    version: 6,
                    ..Spec::good()
                },
                PgpError::UnsupportedVersion(6),
            ),
            (
                Spec {
                    pkalg: 3,
                    ..Spec::good()
                },
                PgpError::UnsupportedPublicKeyAlgorithm(3),
            ),
            (
                Spec {
                    hash: 2,
                    ..Spec::good()
                },
                PgpError::UnsupportedHash(2),
            ),
        ];
        for (spec, expected) in cases {
            assert_eq!(refusal(&spec), expected);
        }
    }

    #[test]
    fn a_v3_signature_is_refused() {
        // v3: hashed-material length 5, type, creation time, key id, pkalg, hash, left-16, MPI.
        let mut body = vec![3, 5, SIG_TYPE_BINARY];
        body.extend_from_slice(&TEST_SIG_CREATED.to_be_bytes());
        body.extend_from_slice(&test_fingerprint()[12..]);
        body.extend_from_slice(&[PKALG_RSA, HASH_SHA512, 0, 0]);
        body.extend(mpi(&[0x42; 512]));
        assert_eq!(
            verify_test(DOC, &packet(TAG_SIGNATURE, &body)),
            Err(PgpError::UnsupportedVersion(3))
        );
    }

    #[test]
    fn exactly_one_creation_time_within_the_key_and_clock_bounds() {
        let fpr = test_fingerprint();
        let skew_edge = u32::try_from(NOW + FUTURE_SKEW_SECS).unwrap();
        let cases = [
            (issuer(&fpr), Err(PgpError::MissingCreationTime)),
            (
                [
                    issuer(&fpr),
                    created_at(TEST_SIG_CREATED),
                    created_at(TEST_SIG_CREATED),
                ]
                .concat(),
                Err(PgpError::DuplicateSubpacket(2)),
            ),
            (
                [issuer(&fpr), created_at(TEST_KEY_CREATED - 1)].concat(),
                Err(PgpError::CreatedBeforeKey),
            ),
            (
                [issuer(&fpr), created_at(TEST_KEY_CREATED)].concat(),
                Ok(VerifiedSignature {
                    created: TEST_KEY_CREATED,
                }),
            ),
            (
                [issuer(&fpr), created_at(skew_edge + 1)].concat(),
                Err(PgpError::CreatedInFuture),
            ),
            (
                [issuer(&fpr), created_at(skew_edge)].concat(),
                Ok(VerifiedSignature { created: skew_edge }),
            ),
        ];
        for (hashed, expected) in cases {
            assert_eq!(
                verify_test(DOC, &signed(DOC, &Spec::hashed(hashed))),
                expected
            );
        }
        let only_unhashed = Spec {
            hashed: issuer(&fpr),
            unhashed: created_at(TEST_SIG_CREATED),
            ..Spec::good()
        };
        assert_eq!(refusal(&only_unhashed), PgpError::MissingCreationTime);
    }

    #[test]
    fn every_expiration_is_honoured() {
        let lapse = u32::try_from(NOW - u64::from(TEST_SIG_CREATED)).unwrap();
        let cases = [
            (expires_after(lapse), Err(PgpError::Expired)),
            (expires_after(lapse + 1), TEST_OK),
            (expires_after(0), TEST_OK),
            (
                [expires_after(lapse + 100), expires_after(lapse)].concat(),
                Err(PgpError::Expired),
            ),
            (
                [expires_after(0), expires_after(lapse)].concat(),
                Err(PgpError::Expired),
            ),
        ];
        for (extra, expected) in cases {
            let spec = Spec::hashed([good_hashed(), extra].concat());
            assert_eq!(verify_test(DOC, &signed(DOC, &spec)), expected);
        }
    }

    #[test]
    fn unknown_critical_subpackets_are_refused() {
        let fpr = test_fingerprint();
        let cases = [
            (
                [good_hashed(), subpacket(0x80 | 101, b"x")].concat(),
                Err(PgpError::UnknownCriticalSubpacket(101)),
            ),
            (
                [good_hashed(), subpacket(0x80 | 16, &fpr[12..])].concat(),
                Err(PgpError::UnknownCriticalSubpacket(16)),
            ),
            ([good_hashed(), subpacket(101, b"x")].concat(), TEST_OK),
            (
                [
                    issuer(&fpr),
                    subpacket(
                        0x80 | SUBPACKET_CREATION_TIME,
                        &TEST_SIG_CREATED.to_be_bytes(),
                    ),
                ]
                .concat(),
                TEST_OK,
            ),
        ];
        for (hashed, expected) in cases {
            assert_eq!(
                verify_test(DOC, &signed(DOC, &Spec::hashed(hashed))),
                expected
            );
        }
    }

    #[test]
    fn subpacket_lengths_in_all_three_encodings() {
        let two_octet = {
            let len = 300 - 192;
            let mut sp = vec![
                u8::try_from((len >> 8) + 192).unwrap(),
                u8::try_from(len & 0xFF).unwrap(),
                101,
            ];
            sp.extend_from_slice(&[0x5A; 299]);
            sp
        };
        let five_octet = [
            &[0xFF, 0, 0, 0, 5, SUBPACKET_CREATION_TIME][..],
            &TEST_SIG_CREATED.to_be_bytes(),
        ]
        .concat();
        let hashed = [issuer(&test_fingerprint()), five_octet, two_octet].concat();
        assert_eq!(
            verify_test(DOC, &signed(DOC, &Spec::hashed(hashed))),
            TEST_OK
        );
    }

    #[test]
    fn malformed_hashed_subpackets_are_refused() {
        let fpr = test_fingerprint();
        let cases = [
            [good_hashed(), vec![0x00]].concat(),
            [good_hashed(), vec![10, 101, 1, 2]].concat(),
            [good_hashed(), vec![0xC0]].concat(),
            [good_hashed(), vec![0xFF, 0, 0]].concat(),
            [issuer(&fpr), subpacket(SUBPACKET_CREATION_TIME, &[1, 2, 3])].concat(),
            [good_hashed(), subpacket(SUBPACKET_EXPIRATION, &[0; 5])].concat(),
        ];
        for hashed in cases {
            assert_eq!(
                refusal(&Spec::hashed(hashed.clone())),
                PgpError::BadSubpacket,
                "{hashed:02x?}"
            );
        }
    }

    #[test]
    fn the_issuer_fingerprint_must_name_the_pinned_key() {
        let fpr = test_fingerprint();
        let created = created_at(TEST_SIG_CREATED);
        let v5_style = subpacket(SUBPACKET_ISSUER_FINGERPRINT, &[&[5][..], &fpr].concat());
        let short = subpacket(
            SUBPACKET_ISSUER_FINGERPRINT,
            &[&[4][..], &fpr[..19]].concat(),
        );
        let cases = [
            (created.clone(), PgpError::MissingIssuer),
            (
                [issuer(&[0x11; 20]), created.clone()].concat(),
                PgpError::IssuerMismatch,
            ),
            (
                [v5_style, created.clone()].concat(),
                PgpError::IssuerMismatch,
            ),
            ([short, created.clone()].concat(), PgpError::IssuerMismatch),
            (
                [issuer(&fpr), issuer(&fpr), created.clone()].concat(),
                PgpError::DuplicateSubpacket(33),
            ),
        ];
        for (hashed, expected) in cases {
            assert_eq!(refusal(&Spec::hashed(hashed)), expected);
        }
        let only_unhashed = Spec {
            hashed: created,
            unhashed: issuer(&fpr),
            ..Spec::good()
        };
        assert_eq!(refusal(&only_unhashed), PgpError::MissingIssuer);
    }

    #[test]
    fn the_hashed_length_trailer_is_exact() {
        for delta in [-1, 1] {
            let body = sign_body(
                DOC,
                &Spec {
                    trailer_delta: delta,
                    ..Spec::good()
                },
            );
            assert_eq!(
                verify_test(DOC, &packet(TAG_SIGNATURE, &body)),
                Err(PgpError::DigestPrefixMismatch),
                "{delta}"
            );
            assert_eq!(
                verify_test(DOC, &packet(TAG_SIGNATURE, &with_left16_for(&body, DOC))),
                Err(PgpError::BadSignature),
                "{delta}"
            );
        }
    }

    #[test]
    fn partial_and_indeterminate_lengths_are_refused() {
        let body = sign_body(DOC, &Spec::good());
        let partial = [&[0xC0 | TAG_SIGNATURE, 0xE9][..], &body].concat();
        assert_eq!(verify_test(DOC, &partial), Err(PgpError::PartialLength));
        let indeterminate = [&[0x80 | (TAG_SIGNATURE << 2) | 3][..], &body].concat();
        assert_eq!(
            verify_test(DOC, &indeterminate),
            Err(PgpError::IndeterminateLength)
        );
    }

    #[test]
    fn leftover_and_missing_bytes_are_refused() {
        let body = sign_body(DOC, &Spec::good());
        let sig = packet(TAG_SIGNATURE, &body);
        let cases = [
            ([&sig[..], &[0x00, 0x01]].concat(), PgpError::TrailingBytes),
            (
                packet(TAG_SIGNATURE, &[&body[..], &[0x00]].concat()),
                PgpError::TrailingBytes,
            ),
            (
                [&sig[..], &[0xC0 | TAG_SIGNATURE]].concat(),
                PgpError::Truncated,
            ),
            (sig[..sig.len() - 1].to_vec(), PgpError::Truncated),
            (
                packet(TAG_SIGNATURE, &body[..body.len() - 1]),
                PgpError::Truncated,
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(verify_test(DOC, &input), Err(expected));
        }
    }

    #[test]
    fn only_signature_and_marker_packets_are_accepted() {
        let sig = signed(DOC, &Spec::good());
        let marker = packet(TAG_MARKER, b"PGP");
        assert_eq!(verify_test(DOC, &[&marker[..], &sig].concat()), TEST_OK);
        assert_eq!(verify_test(DOC, &marker), Err(PgpError::NoSignature));
        let literal = packet(11, b"b\x00\x00\x00\x00\x00hi");
        assert_eq!(
            verify_test(DOC, &[&literal[..], &sig].concat()),
            Err(PgpError::UnexpectedPacket(11))
        );
        let key = packet(
            6,
            &key_packet_body(TEST_KEY_CREATED, TEST_KEY.pinned.n, TEST_KEY.pinned.e),
        );
        assert_eq!(
            verify_test(DOC, &[&sig[..], &key].concat()),
            Err(PgpError::UnexpectedPacket(6))
        );
    }

    #[test]
    fn one_verifying_signature_among_several_is_enough() {
        let ours = signed(DOC, &Spec::good());
        let foreign = foreign_packet();
        assert_eq!(verify_test(DOC, &[&foreign[..], &ours].concat()), TEST_OK);
        assert_eq!(verify_test(DOC, &[&ours[..], &foreign].concat()), TEST_OK);
        let eight = [foreign.repeat(MAX_SIGNATURES - 1), ours.clone()].concat();
        assert_eq!(verify_test(DOC, &eight), TEST_OK);
        let nine = [foreign.repeat(MAX_SIGNATURES), ours].concat();
        assert_eq!(verify_test(DOC, &nine), Err(PgpError::TooManySignatures));
    }

    #[test]
    fn the_furthest_refusal_is_the_one_reported() {
        let mut body = sign_body(DOC, &Spec::good());
        *body.last_mut().unwrap() ^= 0x01;
        let broken = packet(TAG_SIGNATURE, &body);
        let foreign = foreign_packet();
        assert_eq!(
            verify_test(DOC, &[&foreign[..], &broken].concat()),
            Err(PgpError::BadSignature)
        );
        assert_eq!(
            verify_test(DOC, &[&broken[..], &foreign].concat()),
            Err(PgpError::BadSignature)
        );
    }

    #[test]
    fn a_packet_claiming_the_pinned_issuer_outranks_a_time_refusal() {
        let lapse = u32::try_from(NOW - u64::from(TEST_SIG_CREATED)).unwrap();
        let expired = signed(
            DOC,
            &Spec::hashed([good_hashed(), expires_after(lapse)].concat()),
        );
        let foreign = foreign_packet();
        assert_eq!(
            verify_test(DOC, &[&foreign[..], &expired].concat()),
            Err(PgpError::Expired),
            "another issuer never masks a time refusal"
        );
        let claims_ours = packet(
            TAG_SIGNATURE,
            &with_mpi_field(&sign_body(DOC, &Spec::good()), &mpi(&[0x42; 512])),
        );
        assert_eq!(verify_test(DOC, &claims_ours), Err(PgpError::BadSignature));
        for sig in [
            [&expired[..], &claims_ours].concat(),
            [&claims_ours[..], &expired].concat(),
        ] {
            assert_eq!(
                verify_test(DOC, &sig),
                Err(PgpError::BadSignature),
                "the report is unauthenticated"
            );
        }
    }

    #[test]
    fn the_mpi_must_fit_its_bit_count_and_the_modulus() {
        let body = sign_body(DOC, &Spec::good());
        let cases = [
            [&[0x00, 0x00][..]].concat(),
            [&[0x0F, 0xF9][..], &[0xFF; 512]].concat(),
            [&[0x10, 0x08][..], &[0x01; 513]].concat(),
        ];
        for field in cases {
            assert_eq!(
                verify_test(DOC, &packet(TAG_SIGNATURE, &with_mpi_field(&body, &field))),
                Err(PgpError::BadMpi),
                "{:02x?}",
                &field[..2]
            );
        }
        let fits = [&[0x10, 0x00][..], &[0x00], &[0x01; 511]].concat();
        assert_eq!(
            verify_test(DOC, &packet(TAG_SIGNATURE, &with_mpi_field(&body, &fits))),
            Err(PgpError::BadSignature),
            "a leading zero byte inside the bit count is v4-legal"
        );
    }

    #[test]
    fn oversized_input_is_refused_before_parsing() {
        let big = vec![b'\n'; MAX_SIGNATURE_LEN + 1];
        assert_eq!(verify_test(DOC, &big), Err(PgpError::TooLarge));
        let at_cap = vec![b'\n'; MAX_SIGNATURE_LEN];
        assert_eq!(verify_test(DOC, &at_cap), Err(PgpError::Armor));
    }

    #[test]
    fn every_refusal_has_its_own_message() {
        let all = [
            PgpError::TooLarge,
            PgpError::Armor,
            PgpError::Truncated,
            PgpError::TrailingBytes,
            PgpError::IndeterminateLength,
            PgpError::PartialLength,
            PgpError::UnexpectedPacket(11),
            PgpError::NoSignature,
            PgpError::TooManySignatures,
            PgpError::UnsupportedVersion(3),
            PgpError::UnsupportedSignatureType(1),
            PgpError::UnsupportedPublicKeyAlgorithm(3),
            PgpError::UnsupportedHash(2),
            PgpError::BadSubpacket,
            PgpError::UnknownCriticalSubpacket(101),
            PgpError::DuplicateSubpacket(2),
            PgpError::MissingIssuer,
            PgpError::IssuerMismatch,
            PgpError::MissingCreationTime,
            PgpError::CreatedBeforeKey,
            PgpError::CreatedInFuture,
            PgpError::Expired,
            PgpError::BadMpi,
            PgpError::DigestPrefixMismatch,
            PgpError::BadSignature,
        ];
        let messages: HashSet<String> = all.iter().map(ToString::to_string).collect();
        assert_eq!(messages.len(), all.len());
        assert!(messages.iter().all(|m| m.starts_with("openpgp: ")));
        assert_eq!(
            PgpError::UnsupportedHash(2).to_string(),
            "openpgp: unsupported hash algorithm 2"
        );
        assert_eq!(
            PgpError::TooLarge.to_string(),
            format!(
                "openpgp: signature input is larger than {} KiB",
                MAX_SIGNATURE_LEN / 1024
            )
        );
    }
}
