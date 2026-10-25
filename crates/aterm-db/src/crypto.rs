// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Domain-separated Ed25519 authorization and ChaCha20-Poly1305 operation envelopes.
//! Signatures cover exact wire bytes; no JSON reserialization is signed. Nonces are
//! salted with 256 random bits, independent of a restored sequence. Each record gets
//! a domain-separated HKDF subkey. Restoring rights still needs recovery.

use crate::{Error, Result};
use astream_wire::sync::{Actor, Digest, Header, MAX_PARENTS};
use aterm_digest::Sha256;
use ring::{
    aead,
    rand::{SecureRandom, SystemRandom},
    signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey},
};

/// Maximum plaintext transaction size; checked before allocation or decryption.
pub const MAX_BODY: usize = 64 * 1024;
/// Maximum complete operation frame size.
pub const MAX_OPERATION: usize = MAX_BODY + 4096;
const CAP_LEN: usize = 145;

/// Fresh private bytes from the operating system.
pub fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| Error::new("operating system randomness failed"))?;
    Ok(bytes)
}
fn pair(seed: &[u8; 32]) -> Result<Ed25519KeyPair> {
    Ed25519KeyPair::from_seed_unchecked(seed)
        .map_err(|_| Error::new("invalid database signing key"))
}
/// Public key corresponding to a local seed.
pub fn public(seed: &[u8; 32]) -> Result<[u8; 32]> {
    pair(seed)?
        .public_key()
        .as_ref()
        .try_into()
        .map_err(|_| Error::new("invalid public key size"))
}
fn signed(seed: &[u8; 32], domain: &[u8], body: &[u8]) -> Result<[u8; 64]> {
    let mut message = domain.to_vec();
    message.extend_from_slice(body);
    pair(seed)?
        .sign(&message)
        .as_ref()
        .try_into()
        .map_err(|_| Error::new("invalid signature size"))
}
fn verify(key: &[u8; 32], domain: &[u8], body: &[u8], signature: &[u8]) -> Result<()> {
    let mut message = domain.to_vec();
    message.extend_from_slice(body);
    UnparsedPublicKey::new(&ED25519, key)
        .verify(&message, signature)
        .map_err(|_| Error::new("database signature did not verify"))
}

/// Immutable dataset/audience generation. Each audience has a separate key and journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    /// Random dataset identity, not a human label or address.
    pub id: Digest,
    /// Bootstrap authority's Ed25519 public key.
    pub root: [u8; 32],
    /// Application namespace, visible only to admitted dataset peers.
    pub namespace: String,
    control_incarnation: [u8; 16],
    raw: Vec<u8>,
}
impl Manifest {
    /// Create and sign a dataset descriptor.
    pub fn create(seed: &[u8; 32], namespace: &str) -> Result<Self> {
        if !name(namespace) {
            return Err(Error::new("invalid database namespace"));
        }
        let mut bytes = b"ATDB1".to_vec();
        bytes.extend_from_slice(&random::<32>()?);
        bytes.extend_from_slice(&public(seed)?);
        bytes.extend_from_slice(&random::<16>()?);
        bytes.push(namespace.len() as u8);
        bytes.extend_from_slice(namespace.as_bytes());
        bytes.extend_from_slice(&signed(seed, b"aterm-db/manifest/1", &bytes)?);
        Self::decode(&bytes)
    }
    /// Verify a bounded descriptor. Bootstrap trust must independently pin `root`.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 256 {
            return Err(Error::new("database descriptor too large"));
        }
        let mut r = Reader::new(bytes);
        if r.take(5)? != b"ATDB1" {
            return Err(Error::new("unknown database descriptor version"));
        }
        let id = r.array()?;
        let root = r.array()?;
        let control_incarnation = r.array()?;
        let n = r.byte()? as usize;
        let namespace = std::str::from_utf8(r.take(n)?)
            .map_err(|_| Error::new("invalid database namespace"))?
            .to_owned();
        if !name(&namespace) {
            return Err(Error::new("invalid database namespace"));
        }
        let unsigned = r.pos;
        let signature = r.take(64)?;
        r.end()?;
        verify(&root, b"aterm-db/manifest/1", &bytes[..unsigned], signature)?;
        Ok(Self {
            id,
            root,
            namespace,
            control_incarnation,
            raw: bytes.to_vec(),
        })
    }
    /// Signed wire descriptor.
    pub fn bytes(&self) -> &[u8] {
        &self.raw
    }
    /// The independent authority stream cannot inherit a catalog writer's poisoned
    /// causal tail. Administrative decisions depend only on prior authority decisions.
    pub fn control_actor(&self) -> Actor {
        let mut actor = [0; 48];
        actor[..32].copy_from_slice(&self.root);
        actor[32..].copy_from_slice(&self.control_incarnation);
        actor
    }
}
/// A root-signed writer admission for exactly one dataset and installation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capability {
    /// Actor public key and incarnation.
    pub actor: Actor,
    pub(crate) control: bool,
    raw: Vec<u8>,
}
impl Capability {
    /// Admit a writer. Only a holder of the bootstrap authority seed can do this.
    pub fn issue(manifest: &Manifest, root_seed: &[u8; 32], actor: Actor) -> Result<Self> {
        Self::issue_role(manifest, root_seed, actor, false)
    }
    /// Certificate for the dataset's separate owner-authorized control stream.
    pub fn control(manifest: &Manifest, root_seed: &[u8; 32]) -> Result<Self> {
        Self::issue_role(manifest, root_seed, manifest.control_actor(), true)
    }
    fn issue_role(
        manifest: &Manifest,
        root_seed: &[u8; 32],
        actor: Actor,
        control: bool,
    ) -> Result<Self> {
        if public(root_seed)? != manifest.root {
            return Err(Error::new(
                "this replica does not hold database admission authority",
            ));
        }
        let mut raw = manifest.id.to_vec();
        raw.extend_from_slice(&actor);
        raw.push(if control { 2 } else { 1 });
        raw.extend_from_slice(&signed(root_seed, b"aterm-db/writer/1", &raw)?);
        Self::decode(manifest, &raw)
    }
    /// Verify a writer's dataset-bound certificate.
    pub fn decode(manifest: &Manifest, raw: &[u8]) -> Result<Self> {
        if raw.len() != CAP_LEN || raw[..32] != manifest.id || !matches!(raw[80], 1 | 2) {
            return Err(Error::new("invalid database writer certificate"));
        }
        verify(&manifest.root, b"aterm-db/writer/1", &raw[..81], &raw[81..])?;
        if raw[80] == 1 && raw[32..64] == manifest.root {
            return Err(Error::new(
                "administrative keys cannot act as ordinary writers or TLS peers",
            ));
        }
        if raw[80] == 2 && raw[32..80] != manifest.control_actor() {
            return Err(Error::new("authority certificate names another stream"));
        }
        Ok(Self {
            actor: raw[32..80]
                .try_into()
                .map_err(|_| Error::new("invalid writer identity"))?,
            raw: raw.to_vec(),
            control: raw[80] == 2,
        })
    }
    /// Exact certificate bytes; contains no secret.
    pub fn bytes(&self) -> &[u8] {
        &self.raw
    }
}

/// An authenticated and decrypted operation, ready for typed validation and persistence.
#[derive(Clone)]
pub struct Opened {
    pub(crate) control: bool,
    /// Signed author claim only. Wall time never orders or authorizes an operation.
    pub(crate) authored_at_unix_ms: u64,
    /// Exact causal header.
    pub(crate) header: Header,
    /// Plaintext is local only; journals and replication carry `wire`.
    pub(crate) plaintext: Vec<u8>,
    /// Original signed ciphertext.
    pub(crate) wire: Vec<u8>,
}

/// Causal placement within one certified installation's hash chain.
pub struct Causal<'a> {
    /// Dense sequence within this certified installation's stream.
    pub sequence: u64,
    /// Exact preceding operation in that stream.
    pub previous: Option<Digest>,
    /// Sorted, unique causal frontier including the exact predecessor.
    pub parents: &'a [Digest],
}

/// Seal with a fresh per-record subkey and a domain-separated writer signature.
pub fn seal(
    manifest: &Manifest,
    key: &[u8; 32],
    seed: &[u8; 32],
    cap: &Capability,
    causal: Causal<'_>,
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    seal_at(
        manifest,
        key,
        seed,
        cap,
        causal,
        plaintext,
        aterm_time::unix_millis()
            .ok_or_else(|| Error::new("system clock cannot represent an authored timestamp"))?,
    )
}

/// Seal with an explicit signed author-time claim. Offline drift and hostile times
/// do not affect causal order, conflict resolution or resource admission.
pub fn seal_at(
    manifest: &Manifest,
    key: &[u8; 32],
    seed: &[u8; 32],
    cap: &Capability,
    causal: Causal<'_>,
    plaintext: &[u8],
    authored_at_unix_ms: u64,
) -> Result<Vec<u8>> {
    let Causal {
        sequence,
        previous,
        parents,
    } = causal;
    if plaintext.len() > MAX_BODY || parents.len() > MAX_PARENTS || public(seed)? != cap.actor[..32]
    {
        return Err(Error::new(
            "operation exceeds bounds or signing identity differs",
        ));
    }
    let mut wire = b"ATDO2".to_vec();
    wire.extend_from_slice(&manifest.id);
    wire.extend_from_slice(cap.bytes());
    wire.extend_from_slice(&sequence.to_le_bytes());
    wire.extend_from_slice(&authored_at_unix_ms.to_le_bytes());
    wire.extend_from_slice(&previous.unwrap_or([0; 32]));
    wire.push(parents.len() as u8);
    for parent in parents {
        wire.extend_from_slice(parent);
    }
    let salt = random::<32>()?;
    wire.extend_from_slice(&salt);
    wire.extend_from_slice(&((plaintext.len() + 16) as u32).to_le_bytes());
    let mut body = plaintext.to_vec();
    cipher(key, &salt, &wire)?
        .seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key([0; 12]),
            aead::Aad::from(&wire),
            &mut body,
        )
        .map_err(|_| Error::new("database encryption failed"))?;
    wire.extend_from_slice(&body);
    wire.extend_from_slice(&signed(seed, b"aterm-db/operation/2", &wire)?);
    Ok(wire)
}
/// Verify dataset, writer admission, signature, exact lengths, and AEAD before exposing payload.
pub fn open(manifest: &Manifest, key: &[u8; 32], wire: &[u8]) -> Result<Opened> {
    if wire.len() > MAX_OPERATION {
        return Err(Error::new("database operation exceeds limit"));
    }
    let mut r = Reader::new(wire);
    if r.take(5)? != b"ATDO2" || r.take(32)? != manifest.id {
        return Err(Error::new("operation is for another database or protocol"));
    }
    let cap = Capability::decode(manifest, r.take(CAP_LEN)?)?;
    let sequence = u64::from_le_bytes(r.array()?);
    let authored_at_unix_ms = u64::from_le_bytes(r.array()?);
    let prev = r.array()?;
    let previous = if prev == [0; 32] { None } else { Some(prev) };
    let count = r.byte()? as usize;
    if count > MAX_PARENTS {
        return Err(Error::new("too many causal parents"));
    }
    let mut parents = Vec::with_capacity(count);
    for _ in 0..count {
        parents.push(r.array()?);
    }
    let salt = r.array()?;
    let n = u32::from_le_bytes(r.array()?) as usize;
    if !(16..=MAX_BODY + 16).contains(&n) {
        return Err(Error::new("invalid encrypted payload length"));
    }
    let aad_end = r.pos;
    let body = r.take(n)?;
    let signed_end = r.pos;
    let signature = r.take(64)?;
    r.end()?;
    let pubkey = cap.actor[..32]
        .try_into()
        .map_err(|_| Error::new("invalid writer key"))?;
    verify(
        &pubkey,
        b"aterm-db/operation/2",
        &wire[..signed_end],
        signature,
    )?;
    let mut plaintext = body.to_vec();
    let len = cipher(key, &salt, &wire[..aad_end])?
        .open_in_place(
            aead::Nonce::assume_unique_for_key([0; 12]),
            aead::Aad::from(&wire[..aad_end]),
            &mut plaintext,
        )
        .map_err(|_| Error::new("database payload authentication failed"))?
        .len();
    plaintext.truncate(len);
    Ok(Opened {
        control: cap.control,
        authored_at_unix_ms,
        header: Header {
            id: Sha256::digest(wire),
            actor: cap.actor,
            sequence,
            previous,
            parents,
        },
        plaintext,
        wire: wire.to_vec(),
    })
}
fn cipher(key: &[u8; 32], salt: &[u8; 32], header: &[u8]) -> Result<aead::LessSafeKey> {
    let prk = aterm_digest::hkdf_extract(salt, key);
    let mut info = b"aterm-db/payload/1".to_vec();
    info.extend_from_slice(header);
    let subkey = aterm_digest::hkdf_expand_32(&prk, &info);
    Ok(aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::CHACHA20_POLY1305, &subkey)
            .map_err(|_| Error::new("invalid encryption key"))?,
    ))
}
/// Safe identifier grammar shared by dataset, collection and record names.
pub fn name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }
    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| Error::new("invalid database frame length"))?;
        let bytes = self
            .bytes
            .get(self.pos..end)
            .ok_or_else(|| Error::new("truncated database frame"))?;
        self.pos = end;
        Ok(bytes)
    }
    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.take(N)?
            .try_into()
            .map_err(|_| Error::new("invalid database field length"))
    }
    pub(crate) fn byte(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }
    pub(crate) fn end(&self) -> Result<()> {
        if self.pos == self.bytes.len() {
            Ok(())
        } else {
            Err(Error::new("trailing database frame bytes"))
        }
    }
}
