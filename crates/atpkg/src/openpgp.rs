// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pure OpenPGP detached-signature verification.

use std::borrow::Cow;

use ring::digest;
use ring::signature::{self, RsaParameters, RsaPublicKeyComponents};

/// The largest signature input accepted, armored or binary.
pub const MAX_SIGNATURE_LEN: usize = 16 * 1024;
const MAX_SIGNATURES: usize = 8;
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
const FINGERPRINT_V4: u8 = 4;

const ARMOR_BEGIN: &[u8] = b"-----BEGIN PGP SIGNATURE-----";
const ARMOR_END: &[u8] = b"-----END PGP SIGNATURE-----";

/// An RSA signing key compiled into the binary.
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

/// Anthropic's Claude Code release signing key.
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

/// Why a signature was refused.
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
    const fn stage(self) -> u8 {
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

/// Verify `signature`.
/// # Errors
/// A framing refusal, or the refusal of the signature packet that got furthest.
pub fn verify_detached(
    document: &[u8],
    signature: &[u8],
    key: &PinnedRsaKey,
    now_unix: u64,
) -> Result<VerifiedSignature, PgpError> {
    if signature.len() > MAX_SIGNATURE_LEN {
        return Err(PgpError::TooLarge);
    }
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

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    const fn is_empty(&self) -> bool {
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
#[path = "openpgp_testkit_tests.rs"]
pub(crate) mod testkit;

#[cfg(test)]
#[path = "openpgp_tests.rs"]
mod tests;
