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
                let peer_binding = session.peer_writer(self.peer)?;
                let context = context(&facts, session, scope, now)?;
                let log = Log::read_with(session, store, scope, decode)?;
                let eligible = log.eligible();
                let book = Book::from_eligible(&eligible, &context);
                #[cfg(test)]
                let lapsed = LAPSED.with(std::cell::Cell::get);
                #[cfg(not(test))]
                let lapsed = false;
                let thread = book
                    .threads
                    .get(&request)
                    .filter(|thread| {
                        !thread.ended()
                            && !thread.version_pending
                            && thread.published.is_none()
                            && thread.offers.contains(&holder)
                            && now.saturating_sub(thread.heard_ms) < need::LAPSE_SECS * MS
                            && !lapsed
                    })
                    .ok_or_else(|| Error::new("credential request is no longer open"))?;
                if !context.policy.allows(&thread.want) {
                    return Err(Error::new("credential policy refuses this contribution"));
                }
                let offered = !book.inert.iter().any(|(id, _)| *id == offer)
                    && eligible.iter().any(|&(operation, command)| {
                        operation.header.id == offer
                            && operation.header.actor == holder
                            && decoded_record(command).is_some_and(|record| {
                                record.kind == "offer"
                                    && ids(&record.key).is_ok_and(|key| key == request)
                            })
                    });
                if !offered {
                    return Err(Error::new("the exact holder Offer is unavailable"));
                }
                delivery::destination(session, thread)?;
                let local_binding = session.peer_writer(self.local)?;
                let (custodian_binding, holder_binding) = if holder == self.local {
                    (&peer_binding, &local_binding)
                } else {
                    (&local_binding, &peer_binding)
                };
                let recipient = custodian_binding.recipient();
                let writer = *holder_binding.writer();
                let sight = facts
                    .sight
                    .as_ref()
                    .ok_or_else(|| Error::new("the vault has no applied signer"))?;
                #[cfg(test)]
                let signer = if SIGNER_MOVED.with(std::cell::Cell::get) {
                    sight.signer.map(|byte| !byte)
                } else {
                    sight.signer
                };
                #[cfg(not(test))]
                let signer = sight.signer;
                Ok(Admission {
                    thread: thread.clone(),
                    signer,
                    vault: sight.vault.clone(),
                    recipient,
                    writer,
                })
            },
        )
    }

    /// The Offer by the peer a take tries first, if any, with whether a take of it by this owner
    /// was refused before ([`CredentialOwner::was_refused`]): of the requests still open that
    /// policy allows and the administrator here does not hold the first entry of, skipping one a
    /// round is taking now, the least by Offer id of those no take was refused, else the least of
    /// those refused. An Offer whose take came to nothing waits behind each one the holder offers
    /// that no take was refused yet. `unrefused_first` false orders by id alone (a mutant's).
    fn candidate(
        &self,
        admin: &client::Admin,
        unrefused_first: bool,
    ) -> Result<Option<([u8; 16], Digest, bool)>> {
        self.check()?;
        let now = now_ms();
        let facts = facts(&self.logical_paths()?, now)?;
        crate::documents::application::observe_store_if_declared(
            self.paths,
            descriptor,
            |session, store, scope| {
                session.may_write()?;
                session.peer_writer(self.peer)?;
                let context = context(&facts, session, scope, now)?;
                let log = Log::read_with(session, store, scope, decode)?;
                let eligible = log.eligible();
                let book = Book::from_eligible(&eligible, &context);
                let inert_ids: BTreeSet<Digest> = book.inert.iter().map(|(id, _)| *id).collect();
                let mut best: Option<((bool, Digest), [u8; 16], bool)> = None;
                for (operation, command) in eligible {
                    if operation.header.actor != self.peer
                        || self.owner.offer_in_flight(&operation.header.id)
                        || inert_ids.contains(&operation.header.id)
                        || !command.changes().is_some_and(
                            |changes| matches!(changes, [change] if change.collection == "offer"),
                        )
                    {
                        continue;
                    }
                    let Some(record) = decoded_record(command) else {
                        continue;
                    };
                    let request = ids(&record.key)?;
                    if book.threads.get(&request).is_some_and(|thread| {
                        !thread.ended()
                            && !thread.version_pending
                            && thread.published.is_none()
                            && context.policy.allows(&thread.want)
                            && thread.want.entries().first().is_none_or(|first| {
                                !admin.doc.entries().iter().any(|entry| entry.name == *first)
                            })
                    }) {
                        let refused = self.owner.was_refused(&operation.header.id);
                        let rank = (refused && unrefused_first, operation.header.id);
                        if best.is_none_or(|(least, ..)| rank < least) {
                            best = Some((rank, request, refused));
                        }
                    }
                }
                Ok(best.map(|((_, offer), request, refused)| (request, offer, refused)))
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

/// Whether the side that answered a handoff owes the peer a round that it begins, in which it
/// takes ([`Handed::owing`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Owing {
    /// It owes none: its own take ran to its end in the round, it administers no vault (no
    /// custodian takes), or it holds no Offer by the peer that a round it begins would try.
    #[default]
    Nothing,
    /// Its take did not run to its end in the round (the wire lets only the side that dials
    /// take, or the exchange broke off), and it holds this Offer by the peer, the one a round it
    /// begins would try ([`takes_from_peer`]).
    Take(Digest),
    /// It could not tell: its store, policy or administrator could not be read now. A round owed
    /// before stays owed, and none is owed for this one; the dial scheduler judges again, off the
    /// serving loop ([`owed_take`]), until a judgement tells (`crate::runtime::discovery::
    /// Hints::judged`).
    Unknown,
}

/// What one handoff exchange did ([`exchange`]).
#[derive(Debug)]
pub(crate) struct Handed {
    /// Holder values handed across and published, or why the exchange failed.
    pub(crate) count: Result<usize>,
    /// Whether this side, where it answered, owes the peer a round it begins: the peer's own
    /// rounds cannot hand across what this round left. A dialling side owes none.
    pub(crate) owing: Owing,
}

#[cfg(test)]
thread_local! {
    /// MUTANT, on for one test thread: an answering custodian never tells that it holds an Offer
    /// by the peer to take ([`Handed::owing`]).
    pub(crate) static TAKE_UNTOLD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT, on for one test thread: an answering custodian tells it owes the peer a round for
    /// any candidate Offer ([`takes_from_peer`]), though [`take`] refuses one it cannot admit.
    pub(crate) static TAKE_TOLD_UNADMITTED: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// MUTANT, on for one test thread: an answering custodian tells it owes the peer a round for
    /// an Offer it admits whatever signer the admission names ([`takes_from_peer`]), though
    /// [`take`] refuses one whose signer is not the one this Mac's administrator signs with.
    pub(crate) static TAKE_TOLD_UNSIGNED: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// MUTANT, on for one test thread: an answering custodian tells it owes the peer a round for
    /// an Offer whose take a round of its own already saw refused
    /// ([`CredentialOwner::was_refused`]), so an Offer whose take came to nothing has its holder
    /// dialled after each of its rounds.
    pub(crate) static TAKE_RETOLD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT, on for one test thread: a take tries the least Offer by id, a refused one
    /// included, while the owed check ([`takes_from_peer`]) names the least one no take was
    /// refused ([`Round::candidate`]): the round owed for one Offer tries another, which its
    /// holder refuses again, and the owed one is never taken.
    pub(crate) static TAKE_BY_ID: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT, on for one test thread: a Mac whose cached administrator does not open tells it
    /// could not tell ([`takes_from_peer`]) whether or not it administers its vault, so every
    /// Mac but the custodian answers a one-way round with `Owing::Unknown` and judges the take
    /// again, six times, for nothing.
    pub(crate) static NO_ADMIN_UNTOLD: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// MUTANT, on for one test thread: an answering side whose exchange broke off owes nothing
    /// ([`Handed::owing`]), as when [`exchange`] returned at the error: the Offer its round
    /// brought waits for the pair's next round.
    pub(crate) static OWING_LOST_ON_ERROR: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// MUTANT, on for one test thread: a take records its Offer refused once it holds it
    /// ([`CredentialOwner::refused`]), before the holder answers, so a take that broke off with
    /// no answer leaves the Offer owed no round after the holder's.
    pub(crate) static REFUSED_AT_TRY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// SEAM, on for one test thread: [`Round::admit`] reads every request as unheard past its
    /// lapse ([`need::LAPSE_SECS`]), as a request is from its lapse until the custodian's next
    /// pass retires it. [`Round::candidate`] does not read the lapse.
    pub(crate) static LAPSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// SEAM, on for one test thread: [`Round::admit`] names a vault signer other than the one
    /// this Mac's administrator signs with (`client::Admin::custodian_signer`), as an admission
    /// does once the vault applied a signer this Mac's cached administrator does not hold.
    pub(crate) static SIGNER_MOVED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Both callers are the runtime transport's admitted, exporter-bound endpoints. A
/// detached endpoint has no owner permit and cannot negotiate this exchange.
///
/// The side that dials takes ([`take`]), and where `bidirectional` (`atdb/6` and later) the
/// side that answers then takes too. Where the answering side's take did not run to its end
/// (the wire is one way, or the exchange broke off before or during it), it says whether it
/// holds an Offer of the peer's that a round it begins would try ([`Handed::owing`]): its own
/// round with the peer takes it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn exchange(
    paths: &Paths,
    config: &Path,
    owner: &CredentialOwner,
    network: [u8; 16],
    local: Actor,
    peer: Actor,
    side: Side,
    bidirectional: bool,
    stream: &mut (impl Read + Write),
) -> Handed {
    let round = Round {
        paths,
        config,
        owner,
        network,
        local,
        peer,
    };
    if side == Side::Initiator {
        let count = take(&round, stream).and_then(|took| {
            Ok(took
                + if bidirectional {
                    give(&round, stream)?
                } else {
                    0
                })
        });
        return Handed {
            count,
            owing: Owing::Nothing,
        };
    }
    let given = give(&round, stream);
    let count = match given {
        Ok(given) if bidirectional => match take(&round, stream) {
            Ok(took) => {
                return Handed {
                    count: Ok(given + took),
                    owing: Owing::Nothing,
                };
            }
            Err(error) => Err(error),
        },
        given => given,
    };
    #[cfg(test)]
    if count.is_err() && OWING_LOST_ON_ERROR.with(std::cell::Cell::get) {
        return Handed {
            count,
            owing: Owing::Nothing,
        };
    }
    Handed {
        count,
        owing: takes_from_peer(&round),
    }
}

/// Whether this side, the custodian, holds an Offer by the round's peer that a round it begins
/// would try ([`take`]) and whose take no round of its own saw refused: the candidate
/// [`Round::candidate`] finds for [`take`], which tries one no take was refused before any that
/// one was, admitted as [`take`] admits it ([`Round::admit`]: the request heard within its
/// lapse, its requester a member, a vault signer applied) and under the signer this Mac's
/// administrator signs with. An Offer [`take`] would refuse before trying it (a request that
/// lapsed and that the custodian's next pass has not retired yet, a signer that moved) is owed
/// no round, for that round would take nothing, nor is one whose take was refused.
///
/// What it cannot read now tells [`Owing::Unknown`]: the logical paths, the cached
/// administrator of a Mac that administers its vault or cannot tell whether it does (its vault
/// state being changed, `client::can_administer`), the candidate's store or policy, the signer,
/// or an admission that met a busy store or its deadline (another admission failure is the
/// refusal a take meets too). A Mac that administers no vault takes nothing, ever, and tells
/// [`Owing::Nothing`]. It reads, and takes no lease.
fn takes_from_peer(round: &Round<'_>) -> Owing {
    #[cfg(test)]
    let (untold, unadmitted, unsigned, retold, no_admin_untold) = (
        TAKE_UNTOLD.with(std::cell::Cell::get),
        TAKE_TOLD_UNADMITTED.with(std::cell::Cell::get),
        TAKE_TOLD_UNSIGNED.with(std::cell::Cell::get),
        TAKE_RETOLD.with(std::cell::Cell::get),
        NO_ADMIN_UNTOLD.with(std::cell::Cell::get),
    );
    #[cfg(not(test))]
    let (untold, unadmitted, unsigned, retold, no_admin_untold) =
        (false, false, false, false, false);
    if untold {
        return Owing::Nothing;
    }
    let Ok(paths) = round.logical_paths() else {
        return Owing::Unknown;
    };
    let Ok(admin) = client::admin_cached(&paths) else {
        // Every member but the custodian lands here on every one-way round it answers: one that
        // administers no vault owes nothing and is judged no more. Only one that does, or cannot
        // tell now, could not tell.
        return match client::can_administer(&paths) {
            Ok(false) if !no_admin_untold => Owing::Nothing,
            _ => Owing::Unknown,
        };
    };
    let (request, offer, refused) = match round.candidate(&admin, true) {
        Ok(Some(candidate)) => candidate,
        Ok(None) => return Owing::Nothing,
        Err(_) => return Owing::Unknown,
    };
    if refused && !retold {
        return Owing::Nothing;
    }
    if unadmitted {
        return Owing::Take(offer);
    }
    let admission = match round.admit(request, offer, round.peer) {
        Ok(admission) => admission,
        Err(error) if error.is_busy() || error.is_out_of_time() => return Owing::Unknown,
        Err(_) => return Owing::Nothing,
    };
    match admin.custodian_signer() {
        Ok(signer) if unsigned || signer == admission.signer => Owing::Take(offer),
        Ok(_) => Owing::Nothing,
        Err(_) => Owing::Unknown,
    }
}

/// Whether this Mac, the custodian, owes `peer` a round that it begins, in which it takes
/// ([`takes_from_peer`]), judged with no round: the judgement a handoff that answered `peer`
/// makes as it ends, made again where that one could not tell ([`Owing::Unknown`]). It reads,
/// and takes no lease, so the dial scheduler makes it off the serving loop
/// (`crate::runtime::transport::Transport::check_takes`).
pub(crate) fn owed_take(
    paths: &Paths,
    config: &Path,
    owner: &CredentialOwner,
    network: [u8; 16],
    local: Actor,
    peer: Actor,
) -> Owing {
    takes_from_peer(&Round {
        paths,
        config,
        owner,
        network,
        local,
        peer,
    })
}

fn take(round: &Round<'_>, stream: &mut (impl Read + Write)) -> Result<usize> {
    let Ok(admin) = client::admin_cached(&round.logical_paths()?) else {
        send(stream, b"AQN1")?;
        return Ok(0);
    };
    #[cfg(test)]
    let by_id = TAKE_BY_ID.with(std::cell::Cell::get);
    #[cfg(not(test))]
    let by_id = false;
    let candidate = round.candidate(&admin, !by_id);
    let Some((request, offer, _)) = candidate.as_ref().ok().copied().flatten() else {
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
    let Some(_offer) = round.owner.try_offer(offer) else {
        send(stream, b"AQN1")?;
        return Ok(0);
    };
    // The Offer is recorded refused once the holder or this side refuses it
    // ([`CredentialOwner::refused`]), never when the exchange breaks off with no answer.
    #[cfg(test)]
    if REFUSED_AT_TRY.with(std::cell::Cell::get) {
        round.owner.refused(offer);
    }
    let refuse = |stream: &mut _| {
        round.owner.refused(offer);
        refused(stream)
    };
    let started = Instant::now();
    let nonce = crate::crypto::random::<32>()?;
    let mut hello = [0u8; 100];
    hello[..4].copy_from_slice(b"ACQ1");
    hello[4..20].copy_from_slice(&round.network);
    hello[20..36].copy_from_slice(&request);
    hello[36..68].copy_from_slice(&offer);
    hello[68..100].copy_from_slice(&nonce);
    send(stream, &hello)?;
    let reply = receive(stream, 36)?;
    if reply == b"AQR1" {
        round.owner.refused(offer);
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
    let mut proof = [0u8; 100];
    proof[..4].copy_from_slice(b"ACP1");
    proof[4..36].copy_from_slice(&exchange.recipient.0);
    proof[36..100].copy_from_slice(
        &admin
            .prove_credential_custody(&exchange)
            .map_err(crate::table::failure)?,
    );
    send(stream, &proof)?;
    let delivery = receive(stream, MAX_DELIVERY)?;
    if delivery == b"AQR1" {
        round.owner.refused(offer);
        return Ok(0);
    }
    // All authorization is re-read after the holder's response, before decryption.
    let current = round.admit(request, offer, round.peer)?;
    if current.signer != admission.signer || current.recipient != exchange.recipient {
        return refuse(stream);
    }
    let identity = client::read_machine_identity(round.paths)
        .map_err(crate::table::failure)?
        .ok_or_else(|| Error::new("the custodian identity is unavailable"))?;
    let values = match exchange.open(&current.writer, identity, started.elapsed(), &delivery) {
        Ok(values) => values,
        Err(_) => return refuse(stream),
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
            let mut ok = [0u8; 12];
            ok[..4].copy_from_slice(b"AQOK");
            ok[4..12].copy_from_slice(&seq.to_be_bytes());
            send(stream, &ok)?;
            Ok(1)
        }
        Err(_) => refuse(stream),
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
    let mut reply = [0u8; 36];
    reply[..4].copy_from_slice(b"AQN2");
    reply[4..36].copy_from_slice(&nonce);
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
    let (network, recipient, floor, versioned) = open(paths, round, |session, store, scope| {
        session.writer()?;
        if let Some(contributor) = contributor {
            session.peer_writer(contributor)?;
        }
        let context = context(&facts, session, scope, now_ms())?;
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
                    && !current.version_pending
                    && current.published.is_none()
                    && current.want == thread.want
                    && current.requester == thread.requester
                    && current.origin == thread.origin
                    && current.operation == thread.operation
            })
            .ok_or_else(|| Error::new("credential request ended before publication"))?;
        Ok((
            scope.trust().network,
            delivery::destination(session, current)?.recipient(),
            current.refused.clone(),
            current.versioned,
        ))
    })?;
    copy_source(&admin, &recipient, &thread.want, floor.as_ref())?;
    admin
        .contribute_to_requester(&recipient, &thread.want, values)
        .map_err(crate::table::failure)?;
    if let Some(round) = round {
        round.check()?;
    }
    let published =
        client::admin_publish(paths, &admin, &admin.doc).map_err(crate::table::failure)?;
    let (mut records, _) = publication(
        &thread.id,
        published.version(),
        published.version().envelope().map(|envelope| envelope.seq),
        &recipient,
        contributor,
        versioned,
    )?;
    records.record = records
        .record
        .attest(&|record| admin.attest_credential_record(&network, record))?;
    open(paths, round, |session, store, scope| {
        session.writer()?;
        let log = Log::read_with(session, store, scope, decode)?;
        log.writable(session.actor())?;
        records.commit(session, store, &log)?;
        Ok(())
    })?;
    Ok(published.seq)
}

/// The table open for a publication. A pass ([`publish_given`]) waits out the window's journal
/// writer as every pass does ([`with_store`]); a round runs inside the window's own repair
/// exchange, so it takes the ordinary admission rather than hold that exchange up.
fn open<T>(
    paths: &Paths,
    round: Option<&Round<'_>>,
    action: impl FnOnce(&Session<'_>, &mut Store, Scope) -> Result<T>,
) -> Result<T> {
    match round {
        Some(_) => crate::documents::application::with_store(paths, true, descriptor, action),
        None => with_store(paths, action),
    }
}

pub(super) fn publish_given(paths: &Paths, thread: &Thread, values: &Values) -> Result<u64> {
    publish(paths, thread, values, None, None)
}

#[cfg(test)]
mod tests;
