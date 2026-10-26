// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Root-authenticated account decisions. Verification establishes historical
//! evidence only: the application must retain the full decision DAG, refuse
//! forks and stops, and check current membership before acting.

use super::{binding::Binding, journal::Trust};
use crate::{Error, Result, SecretBytes, age};
use astream_wire::sync::{Digest, MAX_PARENTS};
use aterm_digest::Sha256;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};

const DOMAIN: &[u8] = b"aterm/commons/account-decision/1\0";
const CONTEXT: &[u8] = b"aterm/commons/account-binding-context/1\0";
const MAX_CAPSULE: usize = 2048;
/// Reject larger frames before signature verification or allocation.
pub const MAX_RECORD: usize = 8192;

/// Public selector only. It is not permission to read the named entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretRef {
    pub vault: String,
    pub signer: [u8; 32],
    pub entry: String,
}

/// Untrusted preparation terms until a root signs them and the application
/// establishes their current, unambiguous place in the account's history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preparation {
    pub table: Digest,
    pub key: String,
    pub proposal: Digest,
    pub configuration: Digest,
    pub provider: String,
    pub method: String,
    pub device: Binding,
    pub secret: Option<SecretRef>,
}

/// A preparation capability never grants metered requests or refresh-token
/// sharing. A stop withdraws preparation; it cannot undo a completed login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Prepare(Box<Preparation>),
    Stop,
}

/// Exact requested ancestry. Times are absent: they cannot order decisions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub account: Digest,
    pub generation: u64,
    pub parents: Vec<Digest>,
    pub action: Action,
}

/// Immutable root-signed evidence, not a live capability. The secret-content
/// commitment, when present, is sealed to the one named age recipient.
#[derive(Clone, Debug)]
pub struct Record {
    trust: Trust,
    request: Request,
    control: Digest,
    authored_ms: u64,
    capsule: Vec<u8>,
    wire: Vec<u8>,
    id: Digest,
}

fn name(word: &str) -> bool {
    !word.is_empty()
        && word.len() <= 64
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

impl Request {
    pub(crate) fn validate(&self, trust: Trust) -> Result<()> {
        if trust.network == [0; 16]
            || trust.authority == [0; 32]
            || trust.genesis == [0; 32]
            || self.account == [0; 32]
            || self.generation == 0
            || self.parents.len() > MAX_PARENTS
            || self.parents.contains(&[0; 32])
            || self.parents.windows(2).any(|p| p[0] >= p[1])
            || (self.generation == 1) != self.parents.is_empty()
        {
            return Err(Error::new("invalid account decision ancestry"));
        }
        if let Action::Prepare(p) = &self.action {
            if p.table == [0; 32]
                || p.proposal == [0; 32]
                || p.configuration == [0; 32]
                || !name(&p.key)
                || !name(&p.provider)
                || !matches!(
                    p.method.as_str(),
                    "oauth"
                        | "device"
                        | "api-key"
                        | "browser"
                        | "cli"
                        | "email-link"
                        | "phone"
                        | "passkey"
                )
                || p.device.network() != &trust.network
                || p.device.authority() != &trust.authority
            {
                return Err(Error::new("invalid account preparation terms"));
            }
            if let Some(s) = &p.secret
                && (!name(&s.vault)
                    || s.signer == [0; 32]
                    || s.entry.len() > 1024
                    || !crate::text::valid_entry_name(&s.entry)
                    || super::reserved_name(&s.entry))
            {
                return Err(Error::new("invalid account secret selector"));
            }
        }
        Ok(())
    }
}

impl Record {
    /// Verify only against an independently pinned Commons identity.
    /// This does not validate ancestry, current control or current device access.
    pub fn verify(wire: &[u8], trust: Trust) -> Result<Self> {
        if wire.len() > MAX_RECORD || wire.len() < 4 + 80 + 32 + 8 + 1 + 32 + 8 + 1 + 2 + 64 {
            return Err(Error::new("account decision exceeds its wire bounds"));
        }
        let signed = wire.len() - 64;
        UnparsedPublicKey::new(&ED25519, trust.authority)
            .verify(&[DOMAIN, &wire[..signed]].concat(), &wire[signed..])
            .map_err(|_| Error::new("account decision signature failed"))?;
        let mut r = Reader {
            bytes: &wire[..signed],
            at: 0,
        };
        if r.take(4)? != b"AAC1"
            || r.array::<16>()? != trust.network
            || r.array::<32>()? != trust.authority
            || r.array::<32>()? != trust.genesis
        {
            return Err(Error::new("account decision has another trust context"));
        }
        let account = r.array()?;
        let generation = u64::from_le_bytes(r.array()?);
        let count = usize::from(r.byte()?);
        if count > MAX_PARENTS {
            return Err(Error::new("too many account decision parents"));
        }
        let mut parents = Vec::with_capacity(count);
        for _ in 0..count {
            parents.push(r.array()?);
        }
        let control = r.array()?;
        let authored_ms = u64::from_le_bytes(r.array()?);
        let action = match r.byte()? {
            0 => Action::Stop,
            1 => {
                let table = r.array()?;
                let key = r.word(64)?;
                let proposal = r.array()?;
                let configuration = r.array()?;
                let provider = r.word(64)?;
                let method = r.word(64)?;
                let device = Binding::decode(r.take(super::binding::BINDING_LEN)?)?;
                let secret = match r.byte()? {
                    0 => None,
                    1 => Some(SecretRef {
                        vault: r.word(64)?,
                        signer: r.array()?,
                        entry: r.word(1024)?,
                    }),
                    _ => return Err(Error::new("unknown account binding kind")),
                };
                Action::Prepare(Box::new(Preparation {
                    table,
                    key,
                    proposal,
                    configuration,
                    provider,
                    method,
                    device,
                    secret,
                }))
            }
            _ => return Err(Error::new("unknown account decision kind")),
        };
        let capsule = r.blob(MAX_CAPSULE)?.to_vec();
        if r.at != r.bytes.len() || control == [0; 32] {
            return Err(Error::new("invalid account decision body"));
        }
        let request = Request {
            account,
            generation,
            parents,
            action,
        };
        request.validate(trust)?;
        let has_secret = matches!(&request.action, Action::Prepare(p) if p.secret.is_some());
        if has_secret == capsule.is_empty() {
            return Err(Error::new(
                "account decision has an inconsistent secret binding",
            ));
        }
        Ok(Self {
            trust,
            request,
            control,
            authored_ms,
            capsule,
            wire: wire.to_vec(),
            id: Sha256::digest(wire),
        })
    }

    /// Prepare exact immutable bytes; the caller must durably retain these
    /// before publication and reuse them on a retry.
    pub(crate) fn issue(
        seed: &[u8; 32],
        trust: Trust,
        request: Request,
        control: Digest,
        authored_ms: u64,
        commitment: Option<&SecretBytes>,
    ) -> Result<Self> {
        request.validate(trust)?;
        let pair = Ed25519KeyPair::from_seed_unchecked(seed)
            .map_err(|_| Error::new("invalid account authority seed"))?;
        if pair.public_key().as_ref() != trust.authority || control == [0; 32] {
            return Err(Error::new("account decision requires its pinned authority"));
        }
        let mut bytes = body(trust, &request, control, authored_ms);
        let capsule = match (&request.action, commitment) {
            (Action::Prepare(p), Some(hash)) if p.secret.is_some() && hash.len() == 32 => {
                let plain = SecretBytes::new(
                    [b"AAEB1".as_slice(), &context(&bytes), hash.as_slice()].concat(),
                );
                let encrypted = age::encrypt(&[p.device.recipient()], plain.as_slice())
                    .map_err(|_| Error::new("cannot seal account entry binding"))?;
                // Keep the plaintext lifetime bounded to this sealing step.
                drop(plain);
                encrypted
            }
            (Action::Prepare(p), None) if p.secret.is_none() => Vec::new(),
            (Action::Stop, None) => Vec::new(),
            _ => return Err(Error::new("account decision needs its exact entry binding")),
        };
        blob(&mut bytes, &capsule);
        let signature = pair.sign(&[DOMAIN, &bytes].concat());
        bytes.extend_from_slice(signature.as_ref());
        Self::verify(&bytes, trust)
    }

    pub const fn trust(&self) -> Trust {
        self.trust
    }
    pub fn request(&self) -> &Request {
        &self.request
    }
    pub const fn control(&self) -> Digest {
        self.control
    }
    /// A signed author claim, never an ordering or expiry input.
    pub const fn authored_ms(&self) -> u64 {
        self.authored_ms
    }
    pub const fn id(&self) -> Digest {
        self.id
    }
    pub fn bytes(&self) -> &[u8] {
        &self.wire
    }

    pub(crate) fn matches_entry(
        &self,
        identity: &age::Identity,
        commitment: &[u8],
    ) -> Result<bool> {
        let Action::Prepare(p) = &self.request.action else {
            return Ok(false);
        };
        if p.secret.is_none() || identity.recipient() != p.device.recipient() {
            return Ok(false);
        }
        let plain = age::decrypt(std::slice::from_ref(identity), &self.capsule)
            .map_err(|_| Error::new("cannot open account entry binding"))?;
        let expected = [
            b"AAEB1".as_slice(),
            &context(&body(
                self.trust,
                &self.request,
                self.control,
                self.authored_ms,
            )),
            commitment,
        ]
        .concat();
        let expected = SecretBytes::new(expected);
        Ok(aterm_digest::ct_eq(plain.as_slice(), expected.as_slice()))
    }
}

fn context(body: &[u8]) -> Digest {
    Sha256::digest([CONTEXT, body].concat())
}
fn blob(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
    out.extend_from_slice(bytes);
}
fn word(out: &mut Vec<u8>, value: &str) {
    blob(out, value.as_bytes());
}
fn body(trust: Trust, request: &Request, control: Digest, authored_ms: u64) -> Vec<u8> {
    let mut out = b"AAC1".to_vec();
    out.extend_from_slice(&trust.network);
    out.extend_from_slice(&trust.authority);
    out.extend_from_slice(&trust.genesis);
    out.extend_from_slice(&request.account);
    out.extend_from_slice(&request.generation.to_le_bytes());
    out.push(request.parents.len() as u8);
    for parent in &request.parents {
        out.extend_from_slice(parent);
    }
    out.extend_from_slice(&control);
    out.extend_from_slice(&authored_ms.to_le_bytes());
    match &request.action {
        Action::Stop => out.push(0),
        Action::Prepare(p) => {
            out.push(1);
            out.extend_from_slice(&p.table);
            word(&mut out, &p.key);
            out.extend_from_slice(&p.proposal);
            out.extend_from_slice(&p.configuration);
            word(&mut out, &p.provider);
            word(&mut out, &p.method);
            out.extend_from_slice(&p.device.bytes());
            out.push(u8::from(p.secret.is_some()));
            if let Some(s) = &p.secret {
                word(&mut out, &s.vault);
                out.extend_from_slice(&s.signer);
                word(&mut out, &s.entry);
            }
        }
    }
    out
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .at
            .checked_add(n)
            .ok_or_else(|| Error::new("invalid account decision length"))?;
        let bytes = self
            .bytes
            .get(self.at..end)
            .ok_or_else(|| Error::new("truncated account decision"))?;
        self.at = end;
        Ok(bytes)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into().unwrap())
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }
    fn blob(&mut self, max: usize) -> Result<&'a [u8]> {
        let n = usize::from(u16::from_le_bytes(self.array()?));
        if n > max {
            return Err(Error::new("account field exceeds its bound"));
        }
        self.take(n)
    }
    fn word(&mut self, max: usize) -> Result<String> {
        String::from_utf8(self.blob(max)?.to_vec())
            .map_err(|_| Error::new("account field is not UTF-8"))
    }
}

#[cfg(test)]
mod tests;
