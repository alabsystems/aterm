// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ACQ1: a runtime-only handoff on the already authenticated Commons connection.
//! Every disk read and publication rechecks current membership, the request, policy
//! and the transport owner's lease. No value or ciphertext is retained in the table.

use super::*;
use crate::{control::Side, runtime::CredentialOwner};
use aterm_secrets::{
    age,
    need::transfer::{Exchange, MAX_DELIVERY, Values},
};
use std::{
    io::{Read, Write},
    path::Path,
    time::Instant,
};

struct Round<'a> {
    paths: &'a Paths,
    config: &'a Path,
    owner: &'a CredentialOwner,
    network: [u8; 16],
    local: Actor,
    peer: Actor,
}

struct Admission {
    thread: Thread,
    signer: [u8; 32],
    vault: String,
    recipient: age::Recipient,
    writer: [u8; 32],
}

impl Round<'_> {
    fn logical_paths(&self) -> Result<Paths> {
        crate::generation::logical_paths(self.paths)
    }

    fn check(&self) -> Result<()> {
        self.owner.recheck()?;
        let policy = crate::catalog::policy::Policy::read(self.config)?;
        let bootstrap = Bootstrap::required(self.paths).map_err(crate::table::failure)?;
        let scope = descriptor(&bootstrap)?.scope;
        if !policy.enabled
            || policy.excluded.contains(&scope.digest())
            || bootstrap.trust().network != self.network
            || bootstrap
                .request()
                .map_err(crate::table::failure)?
                .binding()
                .actor()
                != self.local
        {
            return Err(Error::new(
                "credential handoff is unavailable under current local policy",
            ));
        }
        let session = Session::observe(&bootstrap)?;
        session.may_write()?;
        session.peer_writer(self.peer)?;
        Ok(())
    }

    fn admit(&self, request: [u8; 16], offer: Digest, holder: Actor) -> Result<Admission> {
        self.check()?;
        let now = now_ms();
        let facts = facts(&self.logical_paths()?, now)?;
        crate::documents::application::observe_store(
            self.paths,
            descriptor,
            |session, store, scope| {
                session.may_write()?;
                session.peer_writer(self.peer)?;
                let context = context(&facts, session, scope, now);
                let log = Log::read_with(session, store, scope, decode)?;
                let book = Book::read(&log, &context);
                let thread = book
                    .threads
                    .get(&request)
                    .filter(|thread| {
                        !thread.ended()
                            && thread.published.is_none()
                            && thread.offers.contains(&holder)
                            && now.saturating_sub(thread.heard_ms) < need::LAPSE_SECS * MS
                    })
                    .ok_or_else(|| Error::new("credential request is no longer open"))?;
                if !context.policy.allows(&thread.want) {
                    return Err(Error::new("credential policy refuses this contribution"));
                }
                let offered = log.eligible().into_iter().any(|(operation, command)| {
                    operation.header.id == offer
                        && operation.header.actor == holder
                        && !book.inert.iter().any(|(id, _)| *id == offer)
                        && record_of(&command).is_ok_and(|record| {
                            record.kind == "offer" && record.key == hex::encode_lower(&request)
                        })
                });
                if !offered {
                    return Err(Error::new("the exact holder Offer is unavailable"));
                }
                delivery::destination(session, thread)?;
                let custodian = if holder == self.local {
                    self.peer
                } else {
                    self.local
                };
                let recipient = session.peer_writer(custodian)?.recipient();
                let writer = *session.peer_writer(holder)?.writer();
                let sight = facts
                    .sight
                    .as_ref()
                    .ok_or_else(|| Error::new("the vault has no applied signer"))?;
                Ok(Admission {
                    thread: thread.clone(),
                    signer: sight.signer,
                    vault: sight.vault.clone(),
                    recipient,
                    writer,
                })
            },
        )
    }

    fn candidate(&self, admin: &client::Admin) -> Result<Option<([u8; 16], Digest)>> {
        self.check()?;
        let now = now_ms();
        let facts = facts(&self.logical_paths()?, now)?;
        crate::documents::application::observe_store_if_declared(
            self.paths,
            descriptor,
            |session, store, scope| {
                session.may_write()?;
                session.peer_writer(self.peer)?;
                let context = context(&facts, session, scope, now);
                let log = Log::read_with(session, store, scope, decode)?;
                let book = Book::read(&log, &context);
                let mut offers = Vec::new();
                for (operation, command) in log.eligible() {
                    if operation.header.actor != self.peer
                        || self.owner.offered_before(&operation.header.id)
                        || book.inert.iter().any(|(id, _)| *id == operation.header.id)
                    {
                        continue;
                    }
                    let record = record_of(&command)?;
                    if record.kind != "offer" {
                        continue;
                    }
                    let request = ids(&record.key)?;
                    if book.threads.get(&request).is_some_and(|thread| {
                        !thread.ended()
                            && thread.published.is_none()
                            && context.policy.allows(&thread.want)
                            && !admin
                                .doc
                                .entries()
                                .iter()
                                .any(|entry| thread.want.entries().first() == Some(&entry.name))
                    }) {
                        offers.push((operation.header.id, request));
                    }
                }
                offers.sort_unstable();
                Ok(offers.first().map(|(offer, request)| (*request, *offer)))
            },
        )
        .map(Option::flatten)
    }
}

fn send(stream: &mut (impl Read + Write), bytes: &[u8]) -> Result<()> {
    crate::network::send_frame(stream, bytes)
}
fn receive(stream: &mut (impl Read + Write), cap: usize) -> Result<Vec<u8>> {
    crate::network::receive_frame(stream, cap)
}
fn refused(stream: &mut (impl Read + Write)) -> Result<usize> {
    send(stream, b"AQR1")?;
    Ok(0)
}

/// Both callers are the runtime transport's admitted, exporter-bound endpoints. A
/// detached endpoint has no owner permit and cannot negotiate this exchange.
#[allow(clippy::too_many_arguments)]
pub(crate) fn exchange(
    paths: &Paths,
    config: &Path,
    owner: &CredentialOwner,
    network: [u8; 16],
    local: Actor,
    peer: Actor,
    side: Side,
    stream: &mut (impl Read + Write),
) -> Result<usize> {
    let round = Round {
        paths,
        config,
        owner,
        network,
        local,
        peer,
    };
    match side {
        Side::Initiator => take(&round, stream),
        Side::Responder => give(&round, stream),
    }
}

fn take(round: &Round<'_>, stream: &mut (impl Read + Write)) -> Result<usize> {
    let Ok(admin) = client::admin_cached(&round.logical_paths()?) else {
        send(stream, b"AQN1")?;
        return Ok(0);
    };
    let candidate = round.candidate(&admin);
    let Some((request, offer)) = candidate.as_ref().ok().copied().flatten() else {
        send(stream, b"AQN1")?;
        return candidate.map(|_| 0);
    };
    let admission = round.admit(request, offer, round.peer)?;
    if admin.custodian_signer().map_err(crate::table::failure)? != admission.signer {
        send(stream, b"AQN1")?;
        return Err(Error::new(
            "the vault signer changed before credential custody",
        ));
    }
    if !round.owner.try_offer(offer) {
        send(stream, b"AQN1")?;
        return Ok(0);
    }
    let started = Instant::now();
    let nonce = crate::crypto::random::<32>()?;
    let mut hello = b"ACQ1".to_vec();
    hello.extend_from_slice(&round.network);
    hello.extend_from_slice(&request);
    hello.extend_from_slice(&offer);
    hello.extend_from_slice(&nonce);
    send(stream, &hello)?;
    let reply = receive(stream, 36)?;
    if reply == b"AQR1" {
        return Ok(0);
    }
    if reply.len() != 36 || &reply[..4] != b"AQN2" {
        return Err(Error::new("invalid holder nonce"));
    }
    let exchange = Exchange {
        vault: admission.vault,
        network: round.network,
        request,
        offer,
        custodian_nonce: nonce,
        holder_nonce: reply[4..].try_into().unwrap(),
        custodian: round.local,
        holder: round.peer,
        recipient: admission.recipient,
    };
    let mut proof = b"ACP1".to_vec();
    proof.extend_from_slice(&exchange.recipient.0);
    proof.extend_from_slice(
        &admin
            .prove_credential_custody(&exchange)
            .map_err(crate::table::failure)?,
    );
    send(stream, &proof)?;
    let delivery = receive(stream, MAX_DELIVERY)?;
    if delivery == b"AQR1" {
        return Ok(0);
    }
    // All authorization is re-read after the holder's response, before decryption.
    let current = round.admit(request, offer, round.peer)?;
    if current.signer != admission.signer || current.recipient != exchange.recipient {
        return refused(stream);
    }
    let identity = client::read_machine_identity(round.paths)
        .map_err(crate::table::failure)?
        .ok_or_else(|| Error::new("the custodian identity is unavailable"))?;
    let values = match exchange.open(&current.writer, identity, started.elapsed(), &delivery) {
        Ok(values) => values,
        Err(_) => return refused(stream),
    };
    let published = publish(
        round.paths,
        &current.thread,
        &values,
        Some(round.peer),
        Some(round),
    );
    match published {
        Ok(seq) => {
            let mut ok = b"AQOK".to_vec();
            ok.extend_from_slice(&seq.to_be_bytes());
            send(stream, &ok)?;
            Ok(1)
        }
        Err(_) => refused(stream),
    }
}

fn give(round: &Round<'_>, stream: &mut (impl Read + Write)) -> Result<usize> {
    let hello = receive(stream, 100)?;
    if hello == b"AQN1" {
        return Ok(0);
    }
    if hello.len() != 100 || &hello[..4] != b"ACQ1" || hello[4..20] != round.network {
        return refused(stream);
    }
    let request = hello[20..36].try_into().unwrap();
    let offer = hello[36..68].try_into().unwrap();
    let Ok(admission) = round.admit(request, offer, round.local) else {
        return refused(stream);
    };
    let nonce = crate::crypto::random::<32>()?;
    let mut reply = b"AQN2".to_vec();
    reply.extend_from_slice(&nonce);
    send(stream, &reply)?;
    let proof = receive(stream, 100)?;
    if proof.len() != 100 || &proof[..4] != b"ACP1" {
        return refused(stream);
    }
    let exchange = Exchange {
        vault: admission.vault,
        network: round.network,
        request,
        offer,
        custodian_nonce: hello[68..].try_into().unwrap(),
        holder_nonce: nonce,
        custodian: round.peer,
        holder: round.local,
        recipient: age::Recipient(proof[4..36].try_into().unwrap()),
    };
    let delivery = match holder_delivery(
        round,
        &exchange,
        &proof[36..].try_into().unwrap(),
        |thread| source::read(&round.logical_paths()?, round.network, thread, now_ms()),
    ) {
        Ok(delivery) => delivery,
        Err(_) => return refused(stream),
    };
    send(stream, &delivery)?;
    let reply = receive(stream, 12)?;
    if reply == b"AQR1" {
        return Ok(0);
    }
    if reply.len() != 12 || &reply[..4] != b"AQOK" {
        return Err(Error::new("invalid credential publication receipt"));
    }
    round.owner.recheck()?;
    source::forget(&round.logical_paths()?, round.network, request)?;
    Ok(1)
}

fn holder_delivery(
    round: &Round<'_>,
    exchange: &Exchange,
    proof: &[u8; 64],
    read: impl FnOnce(&Thread) -> Result<Values>,
) -> Result<Vec<u8>> {
    let current = round.admit(exchange.request, exchange.offer, round.local)?;
    if exchange.network != round.network
        || exchange.holder != round.local
        || exchange.vault != current.vault
        || !exchange.proven_for(&current.signer, proof, round.peer, current.recipient)
    {
        return Err(Error::new(
            "credential custody does not match this connection and current binding",
        ));
    }
    // This closure is reached only after current custody, binding, request and policy checks.
    let values = read(&current.thread)?;
    value::validate(&current.thread.want, &values)?;
    let latest = round.admit(exchange.request, exchange.offer, round.local)?;
    if !exchange.proven_for(&latest.signer, proof, round.peer, latest.recipient) {
        return Err(Error::new(
            "credential custody changed while the source was read",
        ));
    }
    crate::documents::application::observe_store(round.paths, descriptor, |session, _, _| {
        let binding = session.peer_writer(round.peer)?;
        if binding.recipient() != exchange.recipient {
            return Err(Error::new("the custodian binding changed before sealing"));
        }
        session.sign_credential_delivery(exchange, &values)
    })
}

/// Publish immediately through the existing vault path, then retain the attested metadata.
/// An acknowledgement is sent only after both have succeeded.
fn publish(
    paths: &Paths,
    thread: &Thread,
    values: &Values,
    contributor: Option<Actor>,
    round: Option<&Round<'_>>,
) -> Result<u64> {
    let logical = crate::generation::logical_paths(paths)?;
    let paths = &logical;
    value::validate(&thread.want, values)?;
    if let Some(round) = round {
        round.check()?;
    }
    let mut admin = client::admin_cached(paths).map_err(crate::table::failure)?;
    let facts = facts(paths, now_ms())?;
    let (network, recipient) = with_store(paths, |session, store, scope| {
        session.writer()?;
        if let Some(contributor) = contributor {
            session.peer_writer(contributor)?;
        }
        let context = context(&facts, session, scope, now_ms());
        if !context.policy.allows(&thread.want) {
            return Err(Error::new("credential policy refuses publication"));
        }
        let log = Log::read_with(session, store, scope, decode)?;
        log.writable(session.actor())?;
        let book = Book::read(&log, &context);
        let current = book
            .threads
            .get(&thread.id)
            .filter(|current| {
                !current.ended()
                    && current.published.is_none()
                    && current.want == thread.want
                    && current.requester == thread.requester
                    && current.origin == thread.origin
            })
            .ok_or_else(|| Error::new("credential request ended before publication"))?;
        Ok((
            scope.trust().network,
            delivery::destination(session, current)?.recipient(),
        ))
    })?;
    admin
        .contribute_to_requester(&recipient, &thread.want, values)
        .map_err(crate::table::failure)?;
    if let Some(round) = round {
        round.check()?;
    }
    let published =
        client::admin_publish(paths, &admin, &admin.doc).map_err(crate::table::failure)?;
    let mut body = Map::new();
    body.insert("vault".into(), Value::from(admin.config.name.clone()));
    body.insert("seq".into(), Value::from(published.seq));
    body.insert(
        "recipient".into(),
        Value::from(hex::encode_lower(&Sha256::digest(recipient.0))),
    );
    if let Some(contributor) = contributor {
        body.insert(
            "contributor".into(),
            Value::from(hex::encode_lower(&contributor)),
        );
    }
    let record = Record::new("published", &thread.id, body)
        .attest(&|record| admin.attest_credential_record(&network, record))?;
    with_store(paths, |session, store, scope| {
        session.writer()?;
        let log = Log::read_with(session, store, scope, decode)?;
        log.writable(session.actor())?;
        commit(session, store, &log, &record)?;
        Ok(())
    })?;
    Ok(published.seq)
}

pub(super) fn publish_given(paths: &Paths, thread: &Thread, values: &Values) -> Result<u64> {
    publish(paths, thread, values, None, None)
}

#[cfg(test)]
mod tests;
