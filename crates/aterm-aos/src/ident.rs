// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Domain-separated SHA-256 digests, versioned artifacts, and task identity types.

use core::fmt;

use aterm_digest::Sha256;

use crate::wire::Enc;

/// The domain under which [`ArtifactId::new`] digests an artifact's declaration.
pub const ARTIFACT_DOMAIN: &[u8] = b"aterm-aos-artifact-v1\0";

/// The domain under which [`TaskIdentity::digest`] digests an identity's encoding.
pub const IDENTITY_DOMAIN: &[u8] = b"aterm-aos-identity-v1\0";

/// A SHA-256 digest.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest(pub [u8; 32]);

impl Digest {
    /// SHA-256 over `domain` then, per part, its length (u64 LE) and its bytes.
    #[must_use]
    pub fn of(domain: &[u8], parts: &[&[u8]]) -> Digest {
        let mut b = DigestBuilder::new(domain);
        for part in parts {
            b.part(part);
        }
        b.finish()
    }

    /// The digest as 64 lowercase hex digits.
    #[must_use]
    pub fn hex(&self) -> String {
        let mut s = String::with_capacity(64);
        push_hex(&mut s, &self.0);
        s
    }

    /// First 12 lowercase hex digits of the digest.
    #[must_use]
    pub fn short(&self) -> String {
        let mut s = String::with_capacity(12);
        push_hex(&mut s, self.0.get(..6).unwrap_or(&[]));
        s
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex())
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex())
    }
}

#[inline]
fn push_hex(out: &mut String, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &b in bytes {
        out.push(char::from(HEX[usize::from(b >> 4)]));
        out.push(char::from(HEX[usize::from(b & 0x0f)]));
    }
}

/// A part length as the u64 the framing writes.
#[inline]
pub(crate) fn len_u64(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// Streaming form of [`Digest::of`].
pub(crate) struct DigestBuilder(Sha256);

impl DigestBuilder {
    #[inline]
    pub(crate) fn new(domain: &[u8]) -> Self {
        let mut h = Sha256::new();
        h.update(domain);
        DigestBuilder(h)
    }

    #[inline]
    pub(crate) fn part(&mut self, bytes: &[u8]) {
        self.0.update(len_u64(bytes.len()).to_le_bytes());
        self.0.update(bytes);
    }

    #[inline]
    pub(crate) const fn finish(self) -> Digest {
        Digest(self.0.finalize())
    }
}

/// What an [`ArtifactId`] names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum ArtifactKind {
    /// Hosted task identifier.
    Task = 1,
    /// One implementation of the task's step function.
    Code = 2,
    /// The behavioural contract a run is judged by.
    Contract = 3,
    /// The state/input/observation schema (the task ABI).
    Schema = 4,
    /// The execution profile: admission bounds and the numerical host profile.
    Profile = 5,
}

impl ArtifactKind {
    /// The kind's wire byte.
    #[inline]
    #[must_use]
    pub const fn byte(self) -> u8 {
        self as u8
    }

    /// The kind a wire byte names, or `None` for a byte no kind uses.
    #[inline]
    #[must_use]
    pub const fn from_byte(b: u8) -> Option<ArtifactKind> {
        match b {
            1 => Some(ArtifactKind::Task),
            2 => Some(ArtifactKind::Code),
            3 => Some(ArtifactKind::Contract),
            4 => Some(ArtifactKind::Schema),
            5 => Some(ArtifactKind::Profile),
            _ => None,
        }
    }
}

/// One versioned artifact.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArtifactId {
    /// What the artifact is.
    pub kind: ArtifactKind,
    /// Artifact name (`demo-stranded-settle`).
    pub name: String,
    /// Its version.
    pub version: u32,
    /// `Digest::of(ARTIFACT_DOMAIN, [kind, name, version, descriptor])`.
    pub digest: Digest,
}

impl ArtifactId {
    /// Computes the [`ArtifactId`] digest over `kind`, `name`, `version`, and `descriptor`.
    #[must_use]
    pub fn new(kind: ArtifactKind, name: &str, version: u32, descriptor: &[u8]) -> ArtifactId {
        let digest = Digest::of(
            ARTIFACT_DOMAIN,
            &[
                &[kind.byte()],
                name.as_bytes(),
                &version.to_le_bytes(),
                descriptor,
            ],
        );
        ArtifactId {
            kind,
            name: name.to_owned(),
            version,
            digest,
        }
    }
}

impl fmt::Display for ArtifactId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}@{} ({})",
            self.name,
            self.version,
            self.digest.short()
        )
    }
}

/// The five artifacts a hosted task runs under.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskIdentity {
    /// The task (kind [`ArtifactKind::Task`]).
    pub task: ArtifactId,
    /// The code running it (kind [`ArtifactKind::Code`]).
    pub code: ArtifactId,
    /// The contract it is judged by (kind [`ArtifactKind::Contract`]).
    pub contract: ArtifactId,
    /// Its schema (kind [`ArtifactKind::Schema`]).
    pub schema: ArtifactId,
    /// Its execution profile (kind [`ArtifactKind::Profile`]).
    pub profile: ArtifactId,
}

impl TaskIdentity {
    /// `Digest::of(b"aterm-aos-identity-v1\0", [the identity's canonical wire encoding])`.
    #[must_use]
    pub fn digest(&self) -> Digest {
        let mut e = Enc::new();
        e.identity(self);
        Digest::of(IDENTITY_DOMAIN, &[e.as_bytes()])
    }

    /// The same identity running other code: the one field a same-schema repair changes.
    #[must_use]
    pub fn with_code(&self, code: ArtifactId) -> TaskIdentity {
        TaskIdentity {
            code,
            ..self.clone()
        }
    }
}

/// A revision of a hosted task's live pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision(pub u64);

/// A branch of a hosted task's history.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BranchId(pub u64);
