// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ALab's Genesis v2 (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.3.8, F12).
//!
//! - **The record.** m22 signs it once, with the `ALAB_SIGNER` lineage, listing every current
//!   `[machines]` recipient, with its Commons binding where Commons holds one
//!   ([`alab_founders`]; `aterm secrets genesis` writes it).
//! - **The digest.** A build checks ALab's genesis by its digest ([`crate::ALAB_GENESIS`]), never
//!   by a signer, so a second genesis is another network that no build accepts, whoever signed it
//!   ([`Log::alab`]). While no digest is compiled the check is dormant.
//! - **The possession challenge.** A founder listed with no binding proves its age identity and
//!   its writer key together to any member ([`Log::challenge`]), and that member lets it in with
//!   an `Admit` of [`Evidence::Listed`], with no person step.
//!
//! Which network a Mac belongs to is the genesis's [`Root`](super::Root): every binding the
//! genesis or an `Admit` carries must name it ([`Log::insert`]). What lets a Mac in is the
//! genesis's digest for a founder it binds, and an effective `Admit` for every other Mac, a listed
//! founder included.

use super::{Body, Draft, Entrant, Evidence, Founder, Log, MemberId, Record, RecordId};
use crate::age::Recipient;
use crate::commons::binding::{Binding, Pending, Request};
use crate::{Error, Result};
use std::collections::{BTreeMap, BTreeSet};

/// The ALab genesis this build compiles: [`crate::ALAB_GENESIS`] read as a digest
/// ([`digest_compiled`]). This crate's own unit tests read none: they build scratch ALab logs,
/// whose genesis no compiled digest can name since no test holds the ALab vault's key, so their
/// stores stay dormant as every build's did before F12b; a pinned store is tested through
/// [`super::Store::with_compiled`].
///
/// # Errors
/// The compiled text is not 64 hex digits, so no genesis is ALab's.
pub fn compiled() -> Result<Option<RecordId>> {
    if cfg!(test) {
        return Ok(None);
    }
    digest_compiled()
}

/// [`crate::ALAB_GENESIS`] read as a digest, in every build.
///
/// # Errors
/// The compiled text is not 64 hex digits.
pub(crate) fn digest_compiled() -> Result<Option<RecordId>> {
    crate::ALAB_GENESIS.map(parse_digest).transpose()
}

fn parse_digest(text: &str) -> Result<RecordId> {
    let mut digest = [0; 32];
    if aterm_codec::hex::decode_into(text.as_bytes(), &mut digest) {
        Ok(digest)
    } else {
        Err(Error::new(
            "this build's ALab genesis digest is not 64 hex digits",
        ))
    }
}

impl Log {
    /// A log of ALab, which `genesis` founds: the genesis whose digest this build compiles
    /// ([`compiled`]). It is checked by that digest alone, never by the key that signed it, so a
    /// second genesis is refused even when the ALab vault's key signed it. While this build
    /// compiles no digest (until F12b) the check is dormant, and this is [`Log::new`].
    ///
    /// # Errors
    /// `genesis` is not a genesis, or its digest is not the compiled one, or the compiled digest
    /// is malformed.
    pub fn alab(genesis: Record) -> Result<Self> {
        let lineage = crate::b64::decode_padded(crate::ALAB_SIGNER)
            .and_then(|key| <[u8; 32]>::try_from(key.as_slice()).ok())
            .unwrap_or([0; 32]);
        Self::against(genesis, compiled()?.as_ref(), &lineage)
    }

    /// [`Log::alab`] against the `compiled` digest. `lineage`, the ALab vault's key, only words
    /// a refusal: it never decides one.
    pub(super) fn against(
        genesis: Record,
        compiled: Option<&RecordId>,
        lineage: &[u8; 32],
    ) -> Result<Self> {
        let Some(compiled) = compiled else {
            return Self::new(genesis);
        };
        if genesis.id() != compiled {
            let ours = matches!(genesis.body(), Body::Genesis { signer, .. } if signer == lineage);
            return Err(Error::new(if ours {
                "a second genesis signed with the ALab vault's key: this build knows ALab by its genesis's digest, and this is another genesis"
            } else {
                "a genesis whose digest is not the ALab genesis this build compiles"
            }));
        }
        Self::new(genesis)
    }

    /// Challenge a Mac that claims a founder this log's genesis lists with no binding: `request`
    /// is its self-signed device request, `node_key` and `name` what it says of itself. The
    /// challenge is sealed to the founder's recipient, and only the Mac that holds that age
    /// identity and the request's writer key answers it (`Request::answer`); [`Proof::admit`]
    /// checks the answer.
    ///
    /// # Errors
    /// The request names another Commons root than the genesis's, or its recipient is no unproved
    /// founder ([`super::Membership::unproved`]): none the genesis lists with no binding, or one
    /// already let in or taken out.
    pub fn challenge(
        &self,
        request: &Request,
        node_key: [u8; 32],
        name: String,
    ) -> Result<(Proof, Vec<u8>)> {
        let Body::Genesis { root, .. } = self.genesis().body() else {
            unreachable!("a log starts at a genesis");
        };
        let binding = request.binding();
        let membership = self.membership();
        let founder = membership
            .unproved()
            .iter()
            .find(|f| f.recipient == binding.recipient())
            .ok_or_else(|| {
                Error::new(
                    "no founder this lab's genesis lists with no binding is waiting for this key",
                )
            })?;
        let (pending, challenge) = Pending::issue(request, root.network, root.authority)?;
        let proof = Proof {
            pending,
            entrant: Entrant {
                request: request.clone(),
                node_key,
                name,
                label: founder.label.clone(),
            },
        };
        Ok((proof, challenge))
    }
}

/// A listed founder's proof in progress, held by the member that challenged it
/// ([`Log::challenge`]). Dropping it ends the challenge.
#[derive(Debug)]
pub struct Proof {
    pending: Pending,
    entrant: Entrant,
}

impl Proof {
    /// Check the founder's `answer` to the challenge and draft the `Admit` that lets it in, under
    /// the label the genesis lists it with, for the member `by` to sign. A challenge takes one
    /// valid answer, within its lifetime.
    ///
    /// # Errors
    /// The answer does not prove both of the founder's keys, the challenge expired or was
    /// answered, `by` is no member of `log`, or the founder is no longer waiting in `log`.
    pub fn admit(
        mut self,
        log: &Log,
        answer: &[u8],
        by: &MemberId,
        authored_ms: u64,
    ) -> Result<Draft> {
        let possession = self.pending.verify(answer)?;
        let membership = log.membership();
        if !membership.contains(by) {
            return Err(Error::new("only a member lets a listed founder in"));
        }
        let recipient = possession.binding().recipient();
        if !membership
            .unproved()
            .iter()
            .any(|f| f.recipient == recipient)
        {
            return Err(Error::new(
                "this founder is no longer waiting: a member let it in or took it out",
            ));
        }
        let body = Body::Admit {
            entrant: self.entrant,
            evidence: Evidence::Listed,
        };
        Ok(log.draft(by, authored_ms, body))
    }
}

/// One `[machines]` row: its name and recipient.
#[derive(Clone, Debug)]
pub struct Machine {
    /// The host name `[machines]` gives it.
    pub name: String,
    /// Its age recipient.
    pub recipient: Recipient,
}

/// ALab's Genesis v2 founders: every machine in `machines`' order, each with the binding in
/// `bindings` that names its recipient (Commons' current roster) and the label `labels` gives its
/// recipient, else the next number from `next_free` up that no label takes.
///
/// # Errors
/// Two bindings name one machine's recipient, two machines share a recipient, or a label is not
/// `m<N>`.
pub fn alab_founders(
    machines: &[Machine],
    bindings: &[Binding],
    labels: &BTreeMap<Recipient, String>,
    next_free: u32,
) -> Result<Vec<Founder>> {
    let mut bound: BTreeMap<Recipient, &Binding> = BTreeMap::new();
    for binding in bindings {
        if bound.insert(binding.recipient(), binding).is_some() {
            return Err(Error::new(format!(
                "two Commons bindings name {}: retire the stale one first",
                binding.recipient().to_bech32()
            )));
        }
    }
    let taken: BTreeSet<&str> = labels.values().map(String::as_str).collect();
    let mut next = next_free.max(1);
    let mut founders = Vec::with_capacity(machines.len());
    for machine in machines {
        let label = match labels.get(&machine.recipient) {
            Some(label) if super::is_label(label) => label.clone(),
            Some(label) => {
                return Err(Error::new(format!(
                    "{}'s label {label} is not m<N> with one to five digits",
                    machine.name
                )));
            }
            None => loop {
                let label = format!("m{next}");
                next += 1;
                if !taken.contains(label.as_str()) {
                    break label;
                }
            },
        };
        founders.push(Founder {
            recipient: machine.recipient,
            binding: bound.get(&machine.recipient).map(|b| (*b).clone()),
            node_key: None,
            name: machine.name.clone(),
            label,
        });
    }
    let recipients: BTreeSet<Recipient> = founders.iter().map(|f| f.recipient).collect();
    if recipients.len() != founders.len() {
        return Err(Error::new("two machines share one recipient"));
    }
    Ok(founders)
}

#[cfg(test)]
mod tests;
