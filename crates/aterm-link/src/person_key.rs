// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A person's key (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §11 C3c): what makes a person's post
//! to a session on another Mac trusted there as the person's, and nothing a Mac can forge.
//!
//! - **The key.** Each Mac holds one Ed25519 key its persons sign with ([`KEY_FILE`] in aterm's
//!   state folder, beside the node key: 64 lowercase hex digits of its seed and a newline, mode
//!   0600). It never leaves the Mac. The node service makes it the first time it runs with the
//!   mesh on, asking nothing ([`vouch`]). A key that is lost, or no longer reads, is made anew the
//!   same way on the node service's next pass, and the old key's certificate lapses
//!   ([`CERT_LIFE_MS`]).
//! - **Its registration.** The Mac's node key vouches for it. The membership log binds that key
//!   to the Mac's seat (its `Node` record), and every member reads it there. The vouching is a
//!   [`Cert`]: the node key, and its signature over the person key and when it signed, under the
//!   node key's record framing (`aterm_net::fleet::record_message`, domain [`CERT_DOMAIN`]). The
//!   node service writes it beside the key ([`CERT_FILE`]), signs it again each day
//!   ([`CERT_RENEW_MS`]), and at once when either key changed. So a person adds no record to the
//!   membership log: the seat already registers the key that vouches.
//! - **The seal.** A person's post to another Mac carries the certificate ([`CERT_TOKEN`]) and the
//!   person key's signature ([`SEAL_TOKEN`]) over the session it is for, the person and its kind
//!   ([`post_address`]), and what a session is handed of it: its time, the record it answers, its
//!   deadline and its text ([`seal_message`]).
//! - **Who checks it.** This Mac's mesh carries a sealed post onto its own lane to the addressed
//!   Mac, naming the person there ([`BY_TOKEN`]), only once the seal verifies under this Mac's own
//!   node key ([`Signer::Key`], `federate::Routes`). The addressed Mac's bridge hands it to the
//!   session as the person only once it verifies under the node key that names the Mac whose lane
//!   brought it ([`Signer::Node`]), and refuses it otherwise. That Mac's whole node key is what the
//!   mesh's link proved, as the membership log binds it to the seat, before the peer ring let that
//!   Mac write a lane naming it as the source; so the certificate carries the key it is checked
//!   under, and the bridge needs nothing more of the node service to check it.
//!
//! On one Mac every process of its user can read both keys, as it can the node key and the
//! fabric's `mint.secret` (§4.11, "What a halt does not stop"). The seal is what keeps every
//! other Mac from writing that Mac's person's mail.

use std::io;
use std::path::Path;

use crate::body::Body;
use crate::person::PERSON_KINDS;

/// The person key's file in aterm's state folder.
pub const KEY_FILE: &str = "fabric-person-key";

/// The node key's certificate of the person key, beside it.
pub const CERT_FILE: &str = "fabric-person-cert";

/// A day, in milliseconds.
const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// How long a certificate is taken after it was signed: a key lost or replaced stops vouching
/// for posts within this.
pub const CERT_LIFE_MS: u64 = 30 * DAY_MS;

/// How old a certificate the node service keeps may be before it signs a fresh one.
pub const CERT_RENEW_MS: u64 = DAY_MS;

/// How far ahead of a reader's clock a certificate's signing may lie, for the clocks of two Macs.
pub const CERT_AHEAD_MS: u64 = DAY_MS;

/// The domain the node key signs a certificate under, inside its record framing.
pub const CERT_DOMAIN: &str = "fabric person key";

/// What starts every record a node key signs (`aterm_net::fleet::RECORD_LABEL`), so no other
/// signature of the node key reads as a certificate.
const RECORD_LABEL: &[u8] = b"aterm-fleet/v3 record ";

/// What starts every message a person key signs.
const SEAL_DOMAIN: &[u8] = b"aterm/fabric/person-post/1\0";

/// The body token holding the certificate.
pub const CERT_TOKEN: &str = "pc";

/// The body token holding the seal.
pub const SEAL_TOKEN: &str = "ps";

/// The body token naming the person on the carrying Mac's own lane.
pub const BY_TOKEN: &str = "by";

/// A certificate's bytes: the node key that signed it, the person key, when it was signed (ms,
/// little-endian), the signature.
const CERT_LEN: usize = 32 + 32 + 8 + 64;

/// The key file's length: 64 hex digits and a newline.
const KEY_FILE_LEN: usize = 65;

/// The node key's word for a person key: the node key, the person key, and when it was signed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cert {
    /// The public half of the node key that signed it.
    pub node: [u8; 32],
    /// The person key's public half.
    pub person: [u8; 32],
    /// When the node key signed it, unix ms.
    pub issued_ms: u64,
    /// The node key's signature over [`Cert::message`].
    pub sig: [u8; 64],
}

impl Cert {
    /// What the node key signs: its record framing, [`CERT_DOMAIN`], a NUL, the person key and
    /// when. The node key is not in it: an Ed25519 signature already binds the key that made it.
    #[must_use]
    pub fn message(person: &[u8; 32], issued_ms: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(RECORD_LABEL.len() + CERT_DOMAIN.len() + 1 + 40);
        out.extend_from_slice(RECORD_LABEL);
        out.extend_from_slice(CERT_DOMAIN.as_bytes());
        out.push(0);
        out.extend_from_slice(person);
        out.extend_from_slice(&issued_ms.to_le_bytes());
        out
    }

    /// The certificate the node key of `node_seed` signs for `person` at `issued_ms`.
    #[must_use]
    pub fn issue(node_seed: &[u8; 32], person: [u8; 32], issued_ms: u64) -> Self {
        Self {
            node: astream_aead::signature::public_key(node_seed),
            person,
            issued_ms,
            sig: astream_aead::signature::sign(node_seed, &Self::message(&person, issued_ms)),
        }
    }

    fn bytes(&self) -> [u8; CERT_LEN] {
        let mut out = [0u8; CERT_LEN];
        out[..32].copy_from_slice(&self.node);
        out[32..64].copy_from_slice(&self.person);
        out[64..72].copy_from_slice(&self.issued_ms.to_le_bytes());
        out[72..].copy_from_slice(&self.sig);
        out
    }

    /// Its text: its bytes in lowercase hex.
    #[must_use]
    pub fn encode(&self) -> String {
        aterm_codec::hex::encode_lower(&self.bytes())
    }

    /// A certificate's text read back; `None` for anything [`Cert::encode`] does not write.
    #[must_use]
    pub fn decode(text: &str) -> Option<Self> {
        let mut bytes = [0u8; CERT_LEN];
        if !(aterm_codec::hex::is_lower(text, 2 * CERT_LEN)
            && aterm_codec::hex::decode_into(text.as_bytes(), &mut bytes))
        {
            return None;
        }
        let mut node = [0u8; 32];
        node.copy_from_slice(&bytes[..32]);
        let mut person = [0u8; 32];
        person.copy_from_slice(&bytes[32..64]);
        let mut issued = [0u8; 8];
        issued.copy_from_slice(&bytes[64..72]);
        let mut sig = [0u8; 64];
        sig.copy_from_slice(&bytes[72..]);
        Some(Self {
            node,
            person,
            issued_ms: u64::from_le_bytes(issued),
            sig,
        })
    }

    /// Whether its node key is the one `signer` names, that key signed it, and it is in force at
    /// `now_ms`.
    ///
    /// # Errors
    /// [`Refusal::NotVouched`] for another node key, or a signature its key did not make;
    /// [`Refusal::Lapsed`] for one signed more than [`CERT_LIFE_MS`] ago or more than
    /// [`CERT_AHEAD_MS`] ahead.
    pub fn check(&self, signer: Signer<'_>, now_ms: u64) -> Result<(), Refusal> {
        if !(signer.names(&self.node)
            && astream_aead::signature::verify(
                &self.node,
                &Self::message(&self.person, self.issued_ms),
                &self.sig,
            ))
        {
            return Err(Refusal::NotVouched);
        }
        let lapsed = now_ms.saturating_sub(self.issued_ms) > CERT_LIFE_MS;
        let ahead = self.issued_ms.saturating_sub(now_ms) > CERT_AHEAD_MS;
        if lapsed || ahead {
            return Err(Refusal::Lapsed);
        }
        Ok(())
    }
}

/// The node key a certificate must be signed by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signer<'a> {
    /// This key: the Mac's own node key, which its mesh carries its persons' posts under.
    Key(&'a [u8; 32]),
    /// The key that names this node (`federate::node_of`): the Mac whose lane brought a post. Only
    /// that Mac writes a lane naming it as the source, under a link that proved its whole key.
    Node(&'a str),
}

impl Signer<'_> {
    fn names(self, key: &[u8; 32]) -> bool {
        match self {
            Self::Key(want) => want == key,
            Self::Node(node) => crate::federate::node_of(key) == node,
        }
    }
}

/// Why a person's post is not taken as the person's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// It carries no certificate or no seal.
    Unsealed,
    /// Its certificate or seal is not one [`Cert::encode`] or a seal writes, or it names no
    /// person.
    Malformed,
    /// Its certificate is not signed by the node key it must be ([`Signer`]).
    NotVouched,
    /// Its certificate was signed too long ago, or too far ahead.
    Lapsed,
    /// Its seal is not the certified person key's signature over what it says.
    BadSeal,
    /// It is of a kind a person does not post.
    Kind,
    /// It came by a relay (`via=`), which a person's post never does.
    Relayed,
}

impl Refusal {
    /// Its `reason=` word.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Unsealed => "person-unsealed",
            Self::Malformed => "person-malformed",
            Self::NotVouched => "person-not-vouched",
            Self::Lapsed => "person-cert-lapsed",
            Self::BadSeal => "person-bad-seal",
            Self::Kind => "person-kind",
            Self::Relayed => "person-relayed",
        }
    }

    /// Why, in words.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Unsealed => "it carries no person key's certificate and seal",
            Self::Malformed => "its certificate or seal does not read",
            Self::NotVouched => "its person key is not vouched for by the sending Mac's node key",
            Self::Lapsed => "its person key's certificate is out of date",
            Self::BadSeal => "its seal is not the person key's signature over it",
            Self::Kind => "a person posts only ask, answer, task, report or note",
            Self::Relayed => "a person's post is never relayed",
        }
    }
}

/// The person key of this Mac: its seed, scrubbed when dropped, and its public half.
pub struct PersonKey {
    seed: [u8; 32],
    public: [u8; 32],
}

impl Drop for PersonKey {
    fn drop(&mut self) {
        aterm_uds::secret::scrub(&mut self.seed);
    }
}

impl std::fmt::Debug for PersonKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersonKey")
            .field("public", &aterm_codec::hex::encode_lower(&self.public))
            .finish_non_exhaustive()
    }
}

impl PersonKey {
    fn of_seed(seed: [u8; 32]) -> Self {
        Self {
            public: astream_aead::signature::public_key(&seed),
            seed,
        }
    }

    /// Its public half.
    #[must_use]
    pub const fn public(&self) -> [u8; 32] {
        self.public
    }

    /// The key in `dir` ([`KEY_FILE`]): `None` while there is none.
    ///
    /// # Errors
    /// A file that is not private to this user, cannot be read, or holds no seed.
    pub fn read(dir: &Path) -> io::Result<Option<Self>> {
        let mut bytes = match aterm_uds::private_file::read_private(dir, KEY_FILE, KEY_FILE_LEN) {
            Ok(bytes) => bytes,
            Err(aterm_uds::private_file::PrivateFileError::Missing) => return Ok(None),
            Err(e) => {
                return Err(io::Error::other(format!(
                    "{}: {e}",
                    dir.join(KEY_FILE).display()
                )));
            }
        };
        let mut seed = [0u8; 32];
        let decoded = std::str::from_utf8(&bytes).ok().is_some_and(|text| {
            let digits = text.strip_suffix('\n').unwrap_or(text);
            aterm_codec::hex::is_lower(digits, 64)
                && aterm_codec::hex::decode_into(digits.as_bytes(), &mut seed)
        });
        aterm_uds::secret::scrub(&mut bytes);
        if !decoded {
            aterm_uds::secret::scrub(&mut seed);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} holds no person key", dir.join(KEY_FILE).display()),
            ));
        }
        Ok(Some(Self::of_seed(seed)))
    }

    /// A new key, written to `dir` in place of any there.
    fn make(dir: &Path) -> io::Result<Self> {
        let mut seed = [0u8; 32];
        aterm_uds::rand::fill(&mut seed)?;
        let mut text = format!("{}\n", aterm_codec::hex::encode_lower(&seed)).into_bytes();
        let written = aterm_uds::private_file::write_private(dir, KEY_FILE, &text);
        aterm_uds::secret::scrub(&mut text);
        if let Err(e) = written {
            aterm_uds::secret::scrub(&mut seed);
            return Err(e);
        }
        Ok(Self::of_seed(seed))
    }

    /// Seal `body`, bound for `subject` (for a post, its [`post_address`]), under `cert`: its
    /// [`CERT_TOKEN`] and [`SEAL_TOKEN`] are set, and no [`BY_TOKEN`] is left, since the lane names
    /// the person.
    pub fn seal(&self, cert: &Cert, subject: &str, body: &mut Body) {
        body.unknown.remove(BY_TOKEN);
        body.unknown.remove(SEAL_TOKEN);
        body.unknown.insert(CERT_TOKEN.to_owned(), cert.encode());
        let sig = astream_aead::signature::sign(&self.seed, &seal_message(subject, body, cert));
        body.unknown
            .insert(SEAL_TOKEN.to_owned(), aterm_codec::hex::encode_lower(&sig));
    }
}

/// The certificate in `dir` ([`CERT_FILE`]): `None` while there is none.
///
/// # Errors
/// A file that is not private to this user, cannot be read, or holds no certificate.
pub fn read_cert(dir: &Path) -> io::Result<Option<Cert>> {
    let bytes = match aterm_uds::private_file::read_private(dir, CERT_FILE, 2 * CERT_LEN + 1) {
        Ok(bytes) => bytes,
        Err(aterm_uds::private_file::PrivateFileError::Missing) => return Ok(None),
        Err(e) => {
            return Err(io::Error::other(format!(
                "{}: {e}",
                dir.join(CERT_FILE).display()
            )));
        }
    };
    std::str::from_utf8(&bytes)
        .ok()
        .and_then(|text| Cert::decode(text.strip_suffix('\n').unwrap_or(text)))
        .map(Some)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} holds no certificate", dir.join(CERT_FILE).display()),
            )
        })
}

/// What [`vouch`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vouched {
    /// The key and its certificate stand as they were.
    Kept,
    /// The node key signed a new certificate; `made` when for a key it made just now, there being
    /// none that read.
    Signed {
        /// A new person key was made.
        made: bool,
    },
}

/// The node service's part (§11 C3c), each pass, with this Mac's node key (`node_seed`) at
/// `now_ms`: make the person key in `dir` where none reads (none yet, or one lost or damaged),
/// and sign its certificate where none stands for it under this node key, or the one there is a
/// day old ([`CERT_RENEW_MS`]).
///
/// # Errors
/// No randomness to make a new key from, a folder that is not private, or a write that fails.
pub fn vouch(dir: &Path, node_seed: &[u8; 32], now_ms: u64) -> io::Result<Vouched> {
    let node_key = astream_aead::signature::public_key(node_seed);
    let (key, made) = match PersonKey::read(dir) {
        Ok(Some(key)) => (key, false),
        Ok(None) | Err(_) => (PersonKey::make(dir)?, true),
    };
    let stands = read_cert(dir).ok().flatten().is_some_and(|cert| {
        cert.person == key.public()
            && cert.issued_ms <= now_ms
            && now_ms - cert.issued_ms < CERT_RENEW_MS
            && cert.check(Signer::Key(&node_key), now_ms).is_ok()
    });
    if stands && !made {
        return Ok(Vouched::Kept);
    }
    let cert = Cert::issue(node_seed, key.public(), now_ms);
    aterm_uds::private_file::write_private(
        dir,
        CERT_FILE,
        format!("{}\n", cert.encode()).as_bytes(),
    )?;
    Ok(Vouched::Signed { made })
}

/// What a person's post to the session `sid` is sealed for: the fleet's lane to that session from
/// `person`, of `kind`, naming no Mac (`/f/<fleet>/in/*/<sid>/<person>/<kind>`). A session id names
/// one session in the fleet, and a key rotation moves a post to the successor of the session's
/// Mac (§11 C3), so the seal holds wherever the post lands.
#[must_use]
pub fn post_address(fleet: &str, sid: &str, person: &str, kind: &str) -> String {
    format!("/f/{fleet}/in/*/{sid}/{person}/{kind}")
}

/// What a person key signs for a record bound for `subject` (for a post, its [`post_address`]):
/// [`SEAL_DOMAIN`], then the subject, the body's time, the record it answers and its deadline (each
/// present or not), its text, and the certificate, each length-framed where its length varies.
/// Nothing else a body holds reaches a session.
#[must_use]
pub fn seal_message(subject: &str, body: &Body, cert: &Cert) -> Vec<u8> {
    fn framed(out: &mut Vec<u8>, bytes: &[u8]) {
        let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(bytes);
    }
    fn either(out: &mut Vec<u8>, value: Option<u64>) {
        match value {
            Some(v) => {
                out.push(1);
                out.extend_from_slice(&v.to_le_bytes());
            }
            None => out.push(0),
        }
    }
    let mut out = Vec::with_capacity(SEAL_DOMAIN.len() + subject.len() + body.text.len() + 160);
    out.extend_from_slice(SEAL_DOMAIN);
    framed(&mut out, subject.as_bytes());
    out.extend_from_slice(&body.t.to_le_bytes());
    either(&mut out, body.re);
    either(&mut out, body.dl);
    framed(&mut out, body.text.as_bytes());
    out.extend_from_slice(&cert.bytes());
    out
}

/// The person key that sealed `body`, a post of `kind` bound for `address` (its
/// [`post_address`]), when the node key `signer` names vouched for that key at `now_ms`:
/// [`verify_seal`], for a kind a person posts.
///
/// # Errors
/// The [`Refusal`]: a kind no person posts, or any of [`verify_seal`]'s.
pub fn verify(
    signer: Signer<'_>,
    address: &str,
    kind: &str,
    body: &Body,
    now_ms: u64,
) -> Result<[u8; 32], Refusal> {
    if !PERSON_KINDS.contains(&kind) {
        return Err(Refusal::Kind);
    }
    verify_seal(signer, address, body, now_ms)
}

/// The person key that sealed `body`, bound for `subject`, when the node key `signer` names
/// vouched for it at `now_ms`: the check of any record a person seals, a post or another (§11 C4's
/// halt-off from a person).
///
/// # Errors
/// The [`Refusal`]: a relayed record, no certificate or seal, one that does not read, a
/// certificate of another node key, one its key did not sign or one out of date, or a seal that
/// does not verify.
pub fn verify_seal(
    signer: Signer<'_>,
    subject: &str,
    body: &Body,
    now_ms: u64,
) -> Result<[u8; 32], Refusal> {
    if body.via.is_some() {
        return Err(Refusal::Relayed);
    }
    let (Some(cert), Some(seal)) = (body.unknown.get(CERT_TOKEN), body.unknown.get(SEAL_TOKEN))
    else {
        return Err(Refusal::Unsealed);
    };
    let cert = Cert::decode(cert).ok_or(Refusal::Malformed)?;
    let mut sig = [0u8; 64];
    if !(aterm_codec::hex::is_lower(seal, 128)
        && aterm_codec::hex::decode_into(seal.as_bytes(), &mut sig))
    {
        return Err(Refusal::Malformed);
    }
    cert.check(signer, now_ms)?;
    if !astream_aead::signature::verify(&cert.person, &seal_message(subject, body, &cert), &sig) {
        return Err(Refusal::BadSeal);
    }
    Ok(cert.person)
}

#[cfg(test)]
#[path = "person_key_tests.rs"]
mod tests;
