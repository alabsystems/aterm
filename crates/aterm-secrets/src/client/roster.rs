// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Client and approver bindings for `aterm-swarm-roster` and `aterm-swarm-policy`
//! (`docs/DESIGN-alab-swarm-2026-10-27.md` §3.2, §4.1, §4.6, §12.3 C11).

use super::*;
use crate::membership_log::Log;
use crate::swarm_roster::{ROSTER_ENTRY, SwarmRoster, leading_m_label};
#[cfg(any(test, debug_assertions))]
use crate::swarm_roster::{SeedAdmission, SheetHints, SwarmPolicy, seed_roster};

/// Read the verified `aterm-swarm-roster` from the locally applied vault copy, if present.
///
/// # Errors
/// Local state or the verified cache cannot be read safely, or `aterm-swarm-roster` is malformed.
#[cfg(test)]
fn base_swarm_roster(paths: &Paths) -> crate::Result<Option<SwarmRoster>> {
    let Some(lock) = StateLock::observe(paths)? else {
        return Ok(None);
    };
    base_swarm_roster_for(&Reads::new(paths), &lock)
}

fn base_swarm_roster_for(
    reads: &Reads<'_>,
    _lock: &StateLock,
) -> crate::Result<Option<SwarmRoster>> {
    let Some(expected) = reads.config()? else {
        return Ok(None);
    };
    if expected.signer.is_none() {
        return Ok(None);
    }
    let state = reads.state()?;
    let log_only_bound = log_only_bound(reads, expected, state)?;
    let cached = envelope_held(reads, expected, state)?;
    let envelope_bound = cached.is_some();
    if state.membership == Stored::In
        && !state.failed
        && state.pending.is_empty()
        && pin_admitted(expected, None)
        && (log_only_bound || envelope_bound)
        && let Some(identity) = reads.identity()?
    {
        if let Some(env) = cached {
            let seeded = log_entries::seeded_under(reads, expected, &env.signer, &env.handoff)?;
            if !log_entries::applied_log_available(state, seeded.as_ref().map(HeldLog::digest)) {
                return Ok(None);
            }
            if let Ok((read, log_digest)) = log_entries::roster_with_log(
                seeded.as_ref().map(|held| (held.log(), held.digest())),
                env,
                identity,
            ) {
                let visible = read.visible;
                if let Some(roster) = read.into_roster()? {
                    return Ok(Some(roster));
                }
                if visible && log_digest.is_some() {
                    return Ok(None);
                }
            }
        }
        if let Some(held) = log_entries::local_log_under(reads, expected)?
            && !held.log().recipient_is_removed(&identity.recipient())
            && !log_entries::recipient_denied_by_roster_or_log(
                None,
                held.log(),
                &identity.recipient(),
            )
            && !held.log().audience_left_out_recipient(
                &crate::membership_log::EpochAudience::Everyone,
                &identity.recipient(),
            )
            && held.log().wraps_to(&identity.recipient())
            && let Some(roster) =
                log_entries::roster_from_log(held.log(), identity)?.into_roster()?
        {
            return Ok(Some(roster));
        }
    }
    Ok(None)
}

/// The authenticated historical label table, without synthetic log members. Registration
/// migrates only this stored snapshot; a log display hint must never become an owner label.
///
/// # Errors
/// The applied vault is unreadable, malformed, or held by a writer.
pub fn historical_swarm_roster(paths: &Paths) -> crate::Result<Option<SwarmRoster>> {
    with_roster_observation(paths, |observation| match observation {
        RosterObservation::Held(lock) => base_swarm_roster_for(&Reads::new(paths), lock),
        RosterObservation::Standalone => Ok(None),
    })?
    .ok_or_else(|| {
        Error::of_kind(
            crate::ErrorKind::VaultBusy,
            "historical roster migration waits for the vault writer",
        )
    })
}

/// Whether `state` applied no version of `expected` and the vault file is absent: the log alone
/// binds this Mac to its source.
fn log_only_bound(reads: &Reads<'_>, expected: &Config, state: &State) -> crate::Result<bool> {
    Ok(state.digest.is_none()
        && fsx::kind(&reads.paths().cache_file())? == fsx::Kind::Missing
        && state.source_identity == Some(source_identity(expected)))
}

/// The vault's envelope, where `state` holds this Mac in the vault under `expected` and it is the
/// version `state` applied.
fn envelope_held<'r>(
    reads: &'r Reads<'_>,
    expected: &Config,
    state: &State,
) -> crate::Result<Option<&'r crate::envelope::Envelope>> {
    if state.membership != Stored::In || !source_bound(expected, state) {
        return Ok(None);
    }
    reads.verified_under(expected, state)
}

/// Read authenticated roster evidence without granting a current membership view.
/// The caller overlays its authenticated log before using these historical rows.
fn base_denial_roster(reads: &Reads<'_>) -> crate::Result<Option<SwarmRoster>> {
    reads.state()?;
    let Some(identity) = reads.identity()? else {
        return Ok(None);
    };
    let custody = reads.custody()?;
    log_entries::denial_roster(
        custody.as_ref().map(HeldLog::log),
        reads.verified()?,
        identity,
    )
}

/// Negative control restoring the old read that confuses a writer with an absent roster.
///
/// # Errors
/// Local state or the verified cache cannot be read safely, or the roster is malformed.
#[cfg(any(test, debug_assertions))]
pub fn test_unwaited_roster(paths: &Paths) -> crate::Result<Option<SwarmRoster>> {
    with_roster_observation(paths, |observation| match observation {
        RosterObservation::Held(lock) => swarm_roster_held(paths, lock),
        RosterObservation::Standalone => Ok(None),
    })
    .map(Option::flatten)
}

pub(crate) enum RosterObservation<'a> {
    Held(&'a StateLock),
    Standalone,
}

impl<'a> RosterObservation<'a> {
    pub(crate) fn state_lock(self) -> Option<&'a StateLock> {
        match self {
            Self::Held(lock) => Some(lock),
            Self::Standalone => None,
        }
    }
}

/// Run a local read under the vault lock, or a checked absence of a configured vault.
pub(crate) fn with_roster_observation<T>(
    paths: &Paths,
    read: impl FnOnce(RosterObservation<'_>) -> crate::Result<T>,
) -> crate::Result<Option<T>> {
    Ok(match observe_roster(paths, read)? {
        Observed::Read(value) => Some(value),
        Observed::Busy | Observed::Unseen => None,
    })
}

/// [`with_roster_observation`] for a caller that may observe again: a vault writer that holds
/// the state lock at this moment is [`crate::ErrorKind::VaultBusy`], not `Ok(None)`, which
/// still says the vault was seen and gave no observation.
pub(crate) fn with_roster_observation_or_busy<T>(
    paths: &Paths,
    read: impl FnOnce(RosterObservation<'_>) -> crate::Result<T>,
) -> crate::Result<Option<T>> {
    match observe_roster(paths, read)? {
        Observed::Read(value) => Ok(Some(value)),
        Observed::Busy => Err(vault_busy()),
        Observed::Unseen => Ok(None),
    }
}

/// A vault writer holds this Mac's state lock: nothing was observed.
pub(crate) fn vault_busy() -> Error {
    Error::of_kind(
        crate::ErrorKind::VaultBusy,
        format!(
            "this {}'s vault is busy: a vault writer holds its state lock",
            aterm_types::machine_name::OS_NOUN
        ),
    )
}

enum Observed<T> {
    Read(T),
    /// A vault writer held the state lock.
    Busy,
    /// A configured vault with no lock, or a vault that appeared while it was read.
    Unseen,
}

#[cfg(any(test, debug_assertions))]
std::thread_local! {
    /// How many roster observations of their own this thread made ([`observe_roster`]).
    static ROSTER_OBSERVATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many roster observations of their own this thread made ([`with_roster_observation`],
/// [`with_roster_observation_or_busy`]): a roster access's currency or key check on its own call
/// (`commons::roster::Access::is_current_observed`, `Access::with_key`, `Historical::with_key`),
/// an access opened on its own call (`Bootstrap::access_in`, `Bootstrap::access_observed_in`), a
/// swarm roster read on its own (`historical_swarm_roster`, a denial's roster), each one; a check
/// judged through a look ([`super::VaultLook`]) adds none.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn test_roster_observations() -> usize {
    ROSTER_OBSERVATIONS.with(std::cell::Cell::get)
}

fn observe_roster<T>(
    paths: &Paths,
    read: impl FnOnce(RosterObservation<'_>) -> crate::Result<T>,
) -> crate::Result<Observed<T>> {
    #[cfg(any(test, debug_assertions))]
    ROSTER_OBSERVATIONS.with(|observations| observations.set(observations.get() + 1));
    match StateLock::observe_narrowed(paths)? {
        StateObservation::Held(lock) => read(RosterObservation::Held(&lock)).map(Observed::Read),
        StateObservation::Busy => Ok(Observed::Busy),
        StateObservation::Absent => {
            if config(paths)?.is_some() {
                return Ok(Observed::Unseen);
            }
            let value = read(RosterObservation::Standalone)?;
            Ok(match StateLock::observe_classified(paths)? {
                StateObservation::Absent if config(paths)?.is_none() => Observed::Read(value),
                StateObservation::Busy => Observed::Busy,
                StateObservation::Absent | StateObservation::Held(_) => Observed::Unseen,
            })
        }
    }
}

/// Read while the caller retains the state lock for this vault, shared or exclusive.
pub(super) fn swarm_roster_held(
    paths: &Paths,
    lock: &StateLock,
) -> crate::Result<Option<SwarmRoster>> {
    // One read of the vault under the caller's lock: no writer moves these files until it drops.
    let reads = Reads::new(paths);
    let Some(expected) = reads.config()? else {
        return Ok(None);
    };
    let base = base_swarm_roster_for(&reads, lock)?;
    swarm_roster_with_base(&reads, expected, base)
}

/// Authenticated denial evidence remains readable when a positive snapshot is unavailable.
pub(super) fn swarm_roster_for_denial_held(
    paths: &Paths,
    _lock: &StateLock,
) -> crate::Result<Option<SwarmRoster>> {
    denial_swarm_roster(&Reads::new(paths))
}

pub(super) fn swarm_roster_for_denial(paths: &Paths) -> crate::Result<Option<SwarmRoster>> {
    with_roster_observation(paths, |observation| match observation {
        RosterObservation::Held(lock) => swarm_roster_for_denial_held(paths, lock),
        RosterObservation::Standalone => Ok(None),
    })
    .and_then(|observed| {
        observed.ok_or_else(|| Error::new("the vault is unavailable while reading denial evidence"))
    })
}

/// An authenticated negative-only snapshot, including membership removals and
/// restorations. Absence or an error must never be used as positive authority.
pub(super) fn denial_swarm_roster(reads: &Reads<'_>) -> crate::Result<Option<SwarmRoster>> {
    if reads.config()?.is_none() {
        return Ok(None);
    }
    let base = base_denial_roster(reads)?;
    let custody = reads.custody()?;
    let mut evidence = custody.as_ref().map(HeldLog::log);
    if let Some(log) = evidence
        && let Some(identity) = reads.identity()?
        && let Some(envelope) = reads.verified()?
        && log_entries::log_base(Some(log), envelope, identity)? == log_entries::LogBase::Behind
    {
        // An older log cannot turn omission from its epoch into denial of a newer
        // envelope's admitted reader. Explicit removals are checked separately.
        evidence = None;
    }
    if let Some(roster) = base {
        return Ok(Some(roster_under_evidence(roster, evidence)));
    }
    let Some(log) = evidence else {
        return Ok(None);
    };
    let mut roster = SwarmRoster::default();
    overlay_log_membership(log, None, &mut roster);
    Ok((!roster.members.is_empty()
        || !roster.deny_labels.is_empty()
        || !roster.deny_nodes.is_empty())
    .then_some(roster))
}

/// `roster` with `evidence`'s membership over it or, with no log, its denied members' nodes denied.
fn roster_under_evidence(mut roster: SwarmRoster, evidence: Option<&Log>) -> SwarmRoster {
    if let Some(log) = evidence {
        overlay_log_membership(log, None, &mut roster);
    } else {
        let active_nodes: BTreeSet<String> = roster
            .members
            .iter()
            .filter(|m| !steward::member_is_denied_in_roster(&roster, m))
            .flat_map(|m| m.node_id.iter().chain(m.node_key.iter()))
            .map(|n| n.trim().to_ascii_lowercase())
            .collect();
        for m in &roster.members {
            if steward::member_is_denied_in_roster(&roster, m) {
                if let Some(node_id) = &m.node_id
                    && !active_nodes.contains(&node_id.trim().to_ascii_lowercase())
                {
                    roster.deny_nodes.push(node_id.clone());
                }
                if let Some(node_key) = &m.node_key
                    && !active_nodes.contains(&node_key.trim().to_ascii_lowercase())
                {
                    roster.deny_nodes.push(node_key.clone());
                }
            }
        }
        roster.canonicalize();
    }
    roster
}

fn swarm_roster_with_base(
    reads: &Reads<'_>,
    expected: &Config,
    base: Option<SwarmRoster>,
) -> crate::Result<Option<SwarmRoster>> {
    let evidence = log_entries::local_log_under(reads, expected)?;
    if let Some(roster) = base {
        return Ok(Some(roster_under_evidence(
            roster,
            evidence.as_ref().map(HeldLog::log),
        )));
    }
    let state = reads.state()?;
    let log_only_bound = log_only_bound(reads, expected, state)?;
    let envelope_bound = envelope_held(reads, expected, state)?.is_some();
    if state.membership == Stored::In
        && !state.failed
        && state.pending.is_empty()
        && pin_admitted(expected, None)
        && (log_only_bound || envelope_bound)
        && let Some(identity) = reads.identity()?
        && let Some(held) = &evidence
        && !held.log().recipient_is_removed(&identity.recipient())
        && !log_entries::recipient_denied_by_roster_or_log(None, held.log(), &identity.recipient())
        && !held.log().audience_left_out_recipient(
            &crate::membership_log::EpochAudience::Everyone,
            &identity.recipient(),
        )
        && held.log().wraps_to(&identity.recipient())
    {
        let mut roster = SwarmRoster::default();
        overlay_log_membership(held.log(), Some(&identity.recipient()), &mut roster);
        if !roster.members.is_empty()
            || !roster.deny_labels.is_empty()
            || !roster.deny_nodes.is_empty()
        {
            return Ok(Some(roster));
        }
    }
    Ok(None)
}

/// The canonical `m<N>` labels and node keys (lowercase hex) of the Macs `log` let back in after
/// a removal: members now, not left out of `[everyone]`, that a `Remove` once named (a person's
/// Allow again signed a restoring `Admit`, or a quorum `Remove` voided their remover's). The deny
/// rows their removal wrote (a removal's cascade, the steward's repair) no longer hold for them:
/// the log, which every Mac evaluates alike, says they are in.
pub(super) fn restored_by_log(
    log: &crate::membership_log::Log,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let restored = log.restored(derive_restored);
    (restored.labels.clone(), restored.nodes.clone())
}

#[cfg(test)]
thread_local! {
    /// The [`derive_restored`] runs on this thread, counted (never timed).
    pub(super) static RESTORED_DERIVED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// What [`restored_by_log`] reads of `log`, with the restored Macs' recipients: derived once for
/// each set of held records, which [`Log::restored`] keeps with the log's evaluations, since every
/// roster read and denial check of an unchanged log asks it again. Whether a removal lies in a
/// seat's past is a bit test of the log's reach index ([`Log::removes_upto`]), not a copy of the
/// seat's whole causal past.
fn derive_restored(log: &crate::membership_log::Log) -> crate::membership_log::Restored {
    #[cfg(test)]
    RESTORED_DERIVED.set(RESTORED_DERIVED.get() + 1);
    let membership = log.membership_ref();
    let voided = membership.voided();
    let mut seat_recipients: BTreeMap<crate::membership_log::MemberId, Recipient> = BTreeMap::new();
    let mut seat_labels: BTreeMap<crate::membership_log::MemberId, Vec<String>> = BTreeMap::new();
    let mut seat_nodes: BTreeMap<crate::membership_log::MemberId, Vec<[u8; 32]>> = BTreeMap::new();
    if let crate::membership_log::Body::Genesis { founders, .. } = log.genesis().body() {
        for f in founders {
            if let Some(actor) = f.member() {
                seat_recipients.entry(actor).or_insert(f.recipient);
                if let Some((_, canon)) =
                    leading_m_label(&f.label).or_else(|| leading_m_label(&f.name))
                {
                    seat_labels.entry(actor).or_default().push(canon);
                }
                if let Some(nk) = f.node_key.filter(|k| *k != [0; 32]) {
                    seat_nodes.entry(actor).or_default().push(nk);
                }
            }
        }
    }
    let mut voided_removed: BTreeSet<crate::membership_log::MemberId> = BTreeSet::new();
    let mut admitted_actors: BTreeSet<crate::membership_log::MemberId> = BTreeSet::new();
    for record in log.records().skip(1) {
        if voided.contains(record.id()) {
            if let crate::membership_log::Body::Remove { member, .. } = record.body()
                && record.actor() != member
            {
                voided_removed.insert(*member);
            }
            continue;
        }
        match record.body() {
            crate::membership_log::Body::Admit { entrant, .. }
                if record.actor() != &entrant.member() =>
            {
                admitted_actors.insert(entrant.member());
            }
            crate::membership_log::Body::Node { node_key, .. } if *node_key != [0; 32] => {
                seat_nodes
                    .entry(*record.actor())
                    .or_default()
                    .push(*node_key);
            }
            _ => {}
        }
    }
    let node_bound_to_actor = |actor: &crate::membership_log::MemberId, nk: [u8; 32]| -> bool {
        if nk == [0; 32] {
            return false;
        }
        match membership.node(&nk) {
            crate::membership_log::NodeStanding::Member(active)
            | crate::membership_log::NodeStanding::Retired(active) => {
                active == *actor
                    || membership
                        .get(&active)
                        .is_some_and(|m| log.removal_upto(&m.entry, actor))
            }
            _ => false,
        }
    };
    let founder_actors: BTreeSet<crate::membership_log::MemberId> =
        seat_recipients.keys().copied().collect();
    let mut record_entrant_seat =
        |actor: crate::membership_log::MemberId, entrant: &crate::membership_log::Entrant| {
            seat_recipients
                .entry(actor)
                .or_insert_with(|| entrant.request.binding().recipient());
            if let Some((_, canon)) =
                leading_m_label(&entrant.label).or_else(|| leading_m_label(&entrant.name))
            {
                let list = seat_labels.entry(actor).or_default();
                if !list.contains(&canon) {
                    list.push(canon);
                }
            }
            if node_bound_to_actor(&actor, entrant.node_key) {
                let list = seat_nodes.entry(actor).or_default();
                if !list.contains(&entrant.node_key) {
                    list.push(entrant.node_key);
                }
            }
        };
    for actor in admitted_actors {
        if !founder_actors.contains(&actor)
            && let Some((_, entrant)) = super::remove::earliest_admit_of(log, &actor)
        {
            record_entrant_seat(actor, entrant);
        }
        if let Some(m) = membership.get(&actor)
            && let Some(r) = log.get(&m.entry)
            && let crate::membership_log::Body::Admit { entrant, .. } = r.body()
        {
            record_entrant_seat(actor, entrant);
        }
    }

    let mut labels = BTreeSet::new();
    let mut nodes = BTreeSet::new();
    let mut recipients = BTreeSet::new();
    for (actor, member) in membership.members() {
        let recipient = member.binding.recipient();
        if log.audience_left_out(&crate::membership_log::EpochAudience::Everyone, actor)
            || log.audience_left_out_recipient(
                &crate::membership_log::EpochAudience::Everyone,
                &recipient,
            )
            || log.recipient_is_removed(&recipient)
            || membership.member_node_removed(member)
        {
            continue;
        }
        let same_mac = |rem: &crate::membership_log::MemberId| {
            *rem == *actor
                || rem[..32] == actor[..32]
                || seat_recipients.get(rem) == Some(&recipient)
                || member
                    .node_key
                    .filter(|k| *k != [0; 32])
                    .is_some_and(|nk| seat_nodes.get(rem).is_some_and(|nks| nks.contains(&nk)))
        };
        let mut matched: BTreeSet<crate::membership_log::MemberId> = voided_removed
            .iter()
            .copied()
            .filter(|rem| {
                same_mac(rem)
                    && (*rem == *actor
                        || membership
                            .removal(rem)
                            .is_some_and(|removal| !removal.quorum))
            })
            .collect();
        if member.entry != *log.network() {
            for r in log.removes_upto(&member.entry) {
                if !voided.contains(r.id())
                    && let crate::membership_log::Body::Remove { member: rem, .. } = r.body()
                    && r.actor() != rem
                    && (*rem == *actor
                        || membership
                            .removal(rem)
                            .is_some_and(|removal| !removal.quorum))
                    && same_mac(rem)
                {
                    matched.insert(*rem);
                }
            }
        }
        if matched.is_empty() {
            continue;
        }
        recipients.insert(recipient.to_bech32());
        if let Some((_, canon)) =
            leading_m_label(&member.label).or_else(|| leading_m_label(&member.name))
            && !membership.removed_labels().contains(&canon)
        {
            labels.insert(canon);
        }
        if let Some(key) = member.node_key.filter(|k| *k != [0; 32])
            && matches!(
                membership.node(&key),
                crate::membership_log::NodeStanding::Member(_)
            )
        {
            nodes.insert(aterm_codec::hex::encode_lower(&key));
        }
        for rem in matched {
            if let Some(rem_labels) = seat_labels.get(&rem) {
                for canon in rem_labels {
                    if !membership.removed_labels().contains(canon) {
                        labels.insert(canon.clone());
                    }
                }
            }
            if let Some(rem_nodes) = seat_nodes.get(&rem) {
                for key in rem_nodes {
                    if matches!(
                        membership.node(key),
                        crate::membership_log::NodeStanding::Member(_)
                    ) {
                        nodes.insert(aterm_codec::hex::encode_lower(key));
                    }
                }
            }
        }
    }
    crate::membership_log::Restored {
        labels,
        nodes,
        recipients,
    }
}

pub(super) fn lift_restored_from_roster(
    log: &crate::membership_log::Log,
    roster: &mut SwarmRoster,
) -> Vec<String> {
    let restored = log.restored(derive_restored);
    if restored.labels.is_empty() && restored.nodes.is_empty() && restored.recipients.is_empty() {
        return Vec::new();
    }
    let (mut labels, mut nodes, recipients) = (
        restored.labels.clone(),
        restored.nodes.clone(),
        &restored.recipients,
    );
    let membership = log.membership_ref();
    for m in &roster.members {
        let same_node = m
            .node_key
            .as_ref()
            .is_some_and(|nk| nodes.contains(&nk.trim().to_ascii_lowercase()))
            || m.node_id
                .as_ref()
                .is_some_and(|nid| nodes.contains(&nid.trim().to_ascii_lowercase()));
        let other_node = m
            .node_key
            .as_ref()
            .is_some_and(|nk| !nodes.contains(&nk.trim().to_ascii_lowercase()));
        let matches_restored = recipients.contains(m.recipient.trim())
            || same_node
            || (!other_node
                && (labels.contains(&m.label) || m.aliases.iter().any(|a| labels.contains(a))));
        if !matches_restored {
            continue;
        }
        let m_removed = Recipient::parse(&m.recipient).is_ok_and(|rec| {
            log.recipient_is_removed(&rec)
                || log.audience_left_out_recipient(
                    &crate::membership_log::EpochAudience::Everyone,
                    &rec,
                )
        });
        if m_removed {
            continue;
        }
        if !membership.removed_labels().contains(&m.label) {
            labels.insert(m.label.clone());
        }
        for alias in &m.aliases {
            if !membership.removed_labels().contains(alias) {
                labels.insert(alias.clone());
            }
        }
        let roster_key_held_out = m.node_key.as_deref().is_some_and(|nk| {
            let trimmed = nk.trim();
            let mut key = [0u8; 32];
            trimmed.len() == 64
                && aterm_codec::hex::decode_into(trimmed.as_bytes(), &mut key)
                && matches!(
                    membership.node(&key),
                    crate::membership_log::NodeStanding::Removed(_)
                        | crate::membership_log::NodeStanding::Retired(_)
                )
        });
        if !roster_key_held_out {
            if let Some(node_id) = &m.node_id {
                nodes.insert(node_id.trim().to_ascii_lowercase());
            }
            if let Some(node_key) = &m.node_key {
                nodes.insert(node_key.trim().to_ascii_lowercase());
            }
        }
    }
    let matches_label = |raw: &str| {
        labels.contains(raw)
            || leading_m_label(raw).is_some_and(|(_, canon)| labels.contains(&canon))
    };
    let lifted: Vec<String> = roster
        .deny_labels
        .iter()
        .filter(|label| matches_label(label))
        .cloned()
        .collect();
    roster.deny_labels.retain(|label| !matches_label(label));
    roster
        .deny_nodes
        .retain(|node| !nodes.contains(&node.trim().to_ascii_lowercase()));
    lifted
}

/// Fold the log's membership into `roster`: a Mac the log removed, or left out of `[everyone]`
/// on purpose, moves to the deny lists. Where `reader` names the Mac reading it, a Mac whose
/// `[everyone]` `Wrap` another Mac's batch may still deliver there ([`Log::in_flight`]) is not
/// left out; with none, every missing `Wrap` reads as left out on purpose.
///
/// [`Log::in_flight`]: crate::membership_log::Log::in_flight
pub(super) fn overlay_log_membership(
    log: &crate::membership_log::Log,
    reader: Option<&Recipient>,
    roster: &mut SwarmRoster,
) {
    let _ = lift_restored_from_roster(log, roster);
    let membership = log.membership_ref();
    let flight = reader
        .map(|mine| log.in_flight(&crate::membership_log::EpochAudience::Everyone, mine))
        .unwrap_or_default();
    let left_out = |rec: &Recipient| {
        log.audience_left_out_recipient(&crate::membership_log::EpochAudience::Everyone, rec)
            && !flight.contains(rec)
    };
    let log_removed_node_hexes: BTreeSet<String> = membership
        .nodes()
        .iter()
        .filter(|&(nk, standing)| {
            matches!(standing, crate::membership_log::NodeStanding::Removed(_)) && *nk != [0; 32]
        })
        .map(|(nk, _)| aterm_codec::hex::encode_lower(nk))
        .collect();
    let mut removed_labels = BTreeSet::new();
    let mut log_removed_rows = Vec::new();
    let mut kept = Vec::with_capacity(roster.members.len());
    for m in std::mem::take(&mut roster.members) {
        let node_removed_in_log = m
            .node_key
            .as_deref()
            .is_some_and(|nk| log_removed_node_hexes.contains(&nk.trim().to_ascii_lowercase()))
            || m.node_id.as_deref().is_some_and(|nid| {
                log_removed_node_hexes.contains(&nid.trim().to_ascii_lowercase())
            });
        if node_removed_in_log
            || Recipient::parse(&m.recipient).is_ok_and(|rec| {
                log.recipient_is_removed(&rec)
                    || log_entries::recipient_denied_by_roster_or_log(None, log, &rec)
                    || left_out(&rec)
            })
        {
            log_removed_rows.push(m);
        } else {
            kept.push(m);
        }
    }
    let active_nodes: BTreeSet<String> = kept
        .iter()
        .filter(|m| {
            !steward::member_is_denied_in_roster(roster, m)
                && !m.node_key.as_deref().is_some_and(|nk| {
                    log_removed_node_hexes.contains(&nk.trim().to_ascii_lowercase())
                })
                && !Recipient::parse(&m.recipient).is_ok_and(|rec| {
                    log_entries::recipient_denied_by_roster_or_log(Some(roster), log, &rec)
                })
        })
        .flat_map(|m| m.node_id.iter().chain(m.node_key.iter()))
        .map(|n| n.trim().to_ascii_lowercase())
        .collect();
    // The node keys current seats hold in the log: a removed row that names one is the seat that
    // held it before a move (one-mesh A15), whose node key is the moved seat's now, so that row
    // denies it nothing. A seat the roster denies otherwise stays denied by its own rows.
    let seated_nodes: BTreeSet<String> = membership
        .nodes()
        .iter()
        .filter(|(_, standing)| {
            matches!(standing, crate::membership_log::NodeStanding::Member(actor)
            if membership.get(actor).is_some_and(|m| {
                !log.recipient_is_removed(&m.binding.recipient())
                    && !left_out(&m.binding.recipient())
            }))
        })
        .map(|(nk, _)| aterm_codec::hex::encode_lower(nk))
        .collect();
    for m in log_removed_rows {
        removed_labels.insert(m.label);
        for alias in m.aliases {
            removed_labels.insert(alias);
        }
        if let Some(claimed) = m
            .claimed_label
            .filter(|c| crate::membership_log::is_label(c))
        {
            removed_labels.insert(claimed);
        }
        if let Some(node_id) = m.node_id
            && !active_nodes.contains(&node_id.trim().to_ascii_lowercase())
        {
            roster.deny_nodes.push(node_id);
        }
        if let Some(node_key) = m.node_key
            && !active_nodes.contains(&node_key.trim().to_ascii_lowercase())
            && !seated_nodes.contains(&node_key.trim().to_ascii_lowercase())
        {
            roster.deny_nodes.push(node_key);
        }
    }
    for m in &kept {
        if steward::member_is_denied_in_roster(roster, m) {
            if let Some(node_id) = &m.node_id
                && !active_nodes.contains(&node_id.trim().to_ascii_lowercase())
            {
                roster.deny_nodes.push(node_id.clone());
            }
            if let Some(node_key) = &m.node_key
                && !active_nodes.contains(&node_key.trim().to_ascii_lowercase())
            {
                roster.deny_nodes.push(node_key.clone());
            }
        }
    }
    roster.members = kept;

    for label in membership.removed_labels() {
        if let Some((_, canon)) = leading_m_label(label) {
            removed_labels.insert(canon);
        }
    }
    for nk_hex in &log_removed_node_hexes {
        roster.deny_nodes.push(nk_hex.clone());
    }
    for founder in membership.unproved() {
        if log.recipient_is_removed(&founder.recipient) || left_out(&founder.recipient) {
            if let Some((_, canon)) = leading_m_label(&founder.label)
                .filter(|(_, c)| c == &founder.label)
                .or_else(|| leading_m_label(&founder.name).filter(|(_, c)| c == &founder.name))
            {
                removed_labels.insert(canon);
            }
            if let Some(nk) = founder.node_key.filter(|k| *k != [0; 32]) {
                let nk_hex = aterm_codec::hex::encode_lower(&nk);
                if !active_nodes.contains(&nk_hex) {
                    roster.deny_nodes.push(nk_hex);
                }
            }
            continue;
        }
        let rec = founder.recipient.to_bech32();
        let node_key = founder
            .node_key
            .filter(|k| *k != [0; 32])
            .map(|k| aterm_codec::hex::encode_lower(&k));
        let key_denied = node_key
            .as_deref()
            .is_some_and(|nk| roster.is_node_denied(nk));
        let canonical = leading_m_label(&founder.label)
            .or_else(|| leading_m_label(&founder.name))
            .map(|(_, c)| c);
        if let Some(pos) = roster
            .members
            .iter()
            .enumerate()
            .filter(|(_, m)| m.recipient == rec)
            .max_by_key(|(idx, m)| {
                (
                    !steward::member_is_denied_in_roster(roster, m),
                    canonical.as_deref() == Some(m.label.as_str()),
                    m.node_key.is_some() || m.node_id.is_some(),
                    m.admitted_ms,
                    std::cmp::Reverse(*idx),
                )
            })
            .map(|(idx, _)| idx)
        {
            if key_denied {
                if let Some(canon) = &canonical {
                    removed_labels.insert(canon.clone());
                }
                roster.members.retain(|m| {
                    if m.recipient == rec {
                        removed_labels.insert(m.label.clone());
                        for alias in &m.aliases {
                            removed_labels.insert(alias.clone());
                        }
                        if let Some(claimed) = &m
                            .claimed_label
                            .as_deref()
                            .filter(|c| crate::membership_log::is_label(c))
                        {
                            removed_labels.insert((*claimed).to_owned());
                        }
                        if let Some(node_id) = &m.node_id
                            && !active_nodes.contains(&node_id.trim().to_ascii_lowercase())
                        {
                            roster.deny_nodes.push(node_id.clone());
                        }
                        if let Some(nk) = &m.node_key
                            && !active_nodes.contains(&nk.trim().to_ascii_lowercase())
                        {
                            roster.deny_nodes.push(nk.clone());
                        }
                        false
                    } else {
                        true
                    }
                });
            } else if let Some(key) = node_key {
                if steward::member_is_denied_in_roster(roster, &roster.members[pos])
                    && !active_nodes.contains(&key.trim().to_ascii_lowercase())
                {
                    roster.deny_nodes.push(key.clone());
                }
                roster.members[pos].node_key = Some(key);
            }
            continue;
        }
        let Some(canonical) = canonical else {
            continue;
        };
        if key_denied {
            removed_labels.insert(canonical);
            continue;
        }
        if roster.is_label_denied(&canonical) {
            if let Some(key) = node_key
                && !active_nodes.contains(&key.trim().to_ascii_lowercase())
            {
                roster.deny_nodes.push(key);
            }
            continue;
        }
        if roster.members.iter().any(|m| m.label == canonical) {
            continue;
        }
        roster.members.push(crate::swarm_roster::SwarmMember {
            recipient: rec,
            vault_name: founder.name.clone(),
            label: canonical,
            aliases: Vec::new(),
            hint: None,
            hw_hint: None,
            admitted_ms: log.genesis().authored_ms(),
            claimed_label: None,
            claimed_at_ms: None,
            node_id: None,
            node_key,
        });
    }

    for (actor, member) in membership.members() {
        let recipient = member.binding.recipient();
        if log.recipient_is_removed(&recipient)
            || (log.audience_left_out(&crate::membership_log::EpochAudience::Everyone, actor)
                && !flight.contains(&recipient))
            || left_out(&recipient)
        {
            if let Some((_, canon)) = leading_m_label(&member.label)
                .filter(|(_, c)| c == &member.label)
                .or_else(|| leading_m_label(&member.name).filter(|(_, c)| c == &member.name))
            {
                removed_labels.insert(canon);
            }
            if let Some(nk) = member.node_key.filter(|k| *k != [0; 32]) {
                let nk_hex = aterm_codec::hex::encode_lower(&nk);
                if !active_nodes.contains(&nk_hex) {
                    roster.deny_nodes.push(nk_hex);
                }
            }
            continue;
        }
        let my_rank = (log.record_index(&member.entry).unwrap_or(0), member.entry);
        if membership.members().values().any(|other| {
            other.binding.recipient() == recipient
                && (log.record_index(&other.entry).unwrap_or(0), other.entry) > my_rank
        }) {
            continue;
        }
        let rec = recipient.to_bech32();
        let node_key = member
            .node_key
            .filter(|k| *k != [0; 32])
            .map(|k| aterm_codec::hex::encode_lower(&k));
        let key_denied = node_key
            .as_deref()
            .is_some_and(|nk| roster.is_node_denied(nk))
            || roster.is_node_denied(&aterm_codec::hex::encode_lower(member.binding.writer()))
            || roster.is_node_denied(&aterm_codec::hex::encode_lower(
                member.binding.incarnation(),
            ))
            || roster.is_node_denied(&aterm_codec::hex::encode_lower(member.binding.machine()))
            || membership.member_node_removed(member);
        let canonical = leading_m_label(&member.label)
            .or_else(|| leading_m_label(&member.name))
            .map(|(_, c)| c);
        if let Some(pos) = roster
            .members
            .iter()
            .enumerate()
            .filter(|(_, m)| m.recipient == rec)
            .max_by_key(|(idx, m)| {
                (
                    !steward::member_is_denied_in_roster(roster, m),
                    canonical.as_deref() == Some(m.label.as_str()),
                    m.node_key.is_some() || m.node_id.is_some(),
                    m.admitted_ms,
                    std::cmp::Reverse(*idx),
                )
            })
            .map(|(idx, _)| idx)
        {
            if key_denied {
                if let Some(canon) = &canonical {
                    removed_labels.insert(canon.clone());
                }
                roster.members.retain(|m| {
                    if m.recipient == rec {
                        removed_labels.insert(m.label.clone());
                        for alias in &m.aliases {
                            removed_labels.insert(alias.clone());
                        }
                        if let Some(claimed) = &m
                            .claimed_label
                            .as_deref()
                            .filter(|c| crate::membership_log::is_label(c))
                        {
                            removed_labels.insert((*claimed).to_owned());
                        }
                        if let Some(node_id) = &m.node_id
                            && !active_nodes.contains(&node_id.trim().to_ascii_lowercase())
                        {
                            roster.deny_nodes.push(node_id.clone());
                        }
                        if let Some(nk) = &m.node_key
                            && !active_nodes.contains(&nk.trim().to_ascii_lowercase())
                        {
                            roster.deny_nodes.push(nk.clone());
                        }
                        false
                    } else {
                        true
                    }
                });
            } else if let Some(key) = node_key {
                if steward::member_is_denied_in_roster(roster, &roster.members[pos])
                    && !active_nodes.contains(&key.trim().to_ascii_lowercase())
                {
                    roster.deny_nodes.push(key.clone());
                }
                roster.members[pos].node_key = Some(key);
            }
            continue;
        }
        let Some(canonical) = canonical else {
            continue;
        };
        if key_denied {
            removed_labels.insert(canonical);
            continue;
        }
        if roster.is_label_denied(&canonical) {
            if let Some(key) = node_key
                && !active_nodes.contains(&key.trim().to_ascii_lowercase())
            {
                roster.deny_nodes.push(key);
            }
            continue;
        }
        if roster.members.iter().any(|m| m.label == canonical) {
            continue;
        }
        let admitted_ms = log
            .get(&member.entry)
            .map_or(0, crate::membership_log::Record::authored_ms);
        roster.members.push(crate::swarm_roster::SwarmMember {
            recipient: rec,
            vault_name: member.name.clone(),
            label: canonical,
            aliases: Vec::new(),
            hint: None,
            hw_hint: None,
            admitted_ms,
            claimed_label: None,
            claimed_at_ms: None,
            node_id: None,
            node_key,
        });
    }
    let mut active_log_labels = BTreeSet::new();
    for founder in membership.unproved() {
        let rec_str = founder.recipient.to_bech32();
        let key_denied = founder
            .node_key
            .filter(|k| *k != [0; 32])
            .is_some_and(|k| roster.is_node_denied(&aterm_codec::hex::encode_lower(&k)));
        if !key_denied
            && !log.recipient_is_removed(&founder.recipient)
            && !log.audience_left_out_recipient(
                &crate::membership_log::EpochAudience::Everyone,
                &founder.recipient,
            )
            && roster.member_by_recipient(&rec_str).is_none()
            && let Some((_, canon)) =
                leading_m_label(&founder.label).or_else(|| leading_m_label(&founder.name))
        {
            active_log_labels.insert(canon);
        }
    }
    for (actor, member) in membership.members() {
        let rec = member.binding.recipient();
        let rec_str = rec.to_bech32();
        let key_denied = member
            .node_key
            .filter(|k| *k != [0; 32])
            .is_some_and(|k| roster.is_node_denied(&aterm_codec::hex::encode_lower(&k)))
            || roster.is_node_denied(&aterm_codec::hex::encode_lower(member.binding.writer()))
            || roster.is_node_denied(&aterm_codec::hex::encode_lower(
                member.binding.incarnation(),
            ))
            || roster.is_node_denied(&aterm_codec::hex::encode_lower(member.binding.machine()))
            || membership.member_node_removed(member);
        if !key_denied
            && !log.recipient_is_removed(&rec)
            && !log.audience_left_out(&crate::membership_log::EpochAudience::Everyone, actor)
            && !log
                .audience_left_out_recipient(&crate::membership_log::EpochAudience::Everyone, &rec)
            && roster.member_by_recipient(&rec_str).is_none()
            && let Some((_, canon)) =
                leading_m_label(&member.label).or_else(|| leading_m_label(&member.name))
        {
            active_log_labels.insert(canon);
        }
    }
    overlay_chosen_labels(membership, roster);
    for lbl in removed_labels {
        let canon = leading_m_label(&lbl).map_or(lbl, |(_, c)| c);
        if !active_log_labels.contains(&canon)
            && !roster.members.iter().any(|m| {
                !steward::member_is_denied_in_roster(roster, m)
                    && (m.matches_label_or_alias(&canon)
                        || leading_m_label(&m.label).is_some_and(|(_, c)| c == canon)
                        || m.aliases
                            .iter()
                            .any(|a| leading_m_label(a).is_some_and(|(_, c)| c == canon)))
            })
        {
            roster.deny_labels.push(canon);
        }
    }
    roster.canonicalize();
}

/// Project holder-signed number choices over historical roster rows, retaining only aliases
/// that still belong to this Mac. The signed log grants no right to another roster holder.
fn overlay_chosen_labels(membership: &crate::membership_log::Membership, roster: &mut SwarmRoster) {
    let choices: BTreeMap<_, _> = membership
        .members()
        .iter()
        .filter_map(|(actor, member)| {
            Some((
                member.binding.recipient().to_bech32(),
                membership.label_after_choice(actor)?,
            ))
        })
        .collect();
    for (actor, member) in membership.members() {
        let Some(label) = membership.label_after_choice(actor) else {
            continue;
        };
        let rec = member.binding.recipient().to_bech32();
        let Some(pos) = roster.members.iter().position(|row| row.recipient == rec) else {
            continue;
        };
        if roster.is_member_denied(&roster.members[pos]) {
            continue;
        }
        if roster.members.iter().any(|other| {
            other.recipient != rec
                && (choices
                    .get(&other.recipient)
                    .copied()
                    .unwrap_or(&other.label)
                    == label
                    || other.aliases.iter().any(|alias| alias == label))
        }) {
            continue;
        }
        let old = roster.members[pos].label.clone();
        let held: BTreeSet<_> = roster
            .members
            .iter()
            .filter(|row| row.recipient != rec)
            .flat_map(|row| {
                std::iter::once(
                    choices
                        .get(&row.recipient)
                        .copied()
                        .unwrap_or(&row.label)
                        .to_owned(),
                )
                .chain(row.aliases.clone())
            })
            .chain(
                membership
                    .members()
                    .iter()
                    .filter(|(other, _)| *other != actor)
                    .map(|(_, member)| member.label.clone()),
            )
            .collect();
        let row = &mut roster.members[pos];
        row.label = label.to_owned();
        row.aliases
            .retain(|alias| alias != label && !held.contains(alias));
        if old != label && !held.contains(&old) && !row.aliases.contains(&old) {
            row.aliases.push(old);
        }
    }
}

/// Read the verified `aterm-swarm-lease` `holder` label from the locally applied vault copy, if present.
///
/// # Errors
/// Local state or the verified cache cannot be read safely, the roster remains busy, or
/// `aterm-swarm-lease` or `aterm-swarm-roster` is malformed.
pub fn swarm_lease_holder(paths: &Paths) -> crate::Result<Option<String>> {
    let roster = swarm_roster_until(paths, Instant::now() + Duration::from_millis(250))?;
    let Some(expected) = config(paths)? else {
        return Ok(None);
    };
    let Some(entry) = entry::read_reserved_entry(paths, &expected, crate::SWARM_LEASE_ENTRY)?
    else {
        return Ok(None);
    };
    let (bytes, _) = entry.into_parts();
    let text = std::str::from_utf8(bytes.as_slice())
        .map_err(|_| Error::new(format!("{} is not UTF-8", crate::SWARM_LEASE_ENTRY)))?;
    let table: aterm_toml::Table = aterm_toml::from_str(text).map_err(|e| {
        Error::new(format!(
            "invalid {}: {}",
            crate::SWARM_LEASE_ENTRY,
            e.log_words()
        ))
    })?;
    let Some(holder) = table
        .get("holder")
        .and_then(aterm_toml::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
    else {
        return Ok(None);
    };
    if table
        .get("until")
        .and_then(aterm_toml::Value::as_integer)
        .is_some_and(|u| u <= 0)
    {
        return Ok(None);
    }
    if let Some(roster) = roster {
        if roster.is_label_denied(&holder) || roster.is_node_denied(&holder) {
            return Ok(None);
        }
        if let Some(member) = roster.member_by_label_or_alias(&holder)
            && roster.is_member_denied(member)
        {
            return Ok(None);
        }
    }
    Ok(Some(holder))
}

/// Resolve this Mac's canonical `m<N>` swarm label (`DESIGN-alab-swarm` §3.2):
///
/// 1. When `aterm-swarm-roster` carries a row for this Mac's age recipient, return its `label`.
/// 2. Otherwise, when joined to the ALab vault (or a vault that already publishes
///    `aterm-swarm-roster` / `aterm-swarm-policy`), fall back to the vault-recorded name for this
///    Mac (if it starts with `m<N>`), then `source.toml`'s `host`, then `hostnames`
///    (LocalHostName / ComputerName / hostname) that **start with** `m<N>`.
/// 3. Personal vaults without `aterm-swarm-roster` return `Ok(None)`.
///
/// Never reads `~/.aterm/machine.toml`.
///
/// # Errors
/// Local vault state, identity, or a present `aterm-swarm-roster` cannot be read or parsed.
pub fn resolve_local_swarm_label(
    paths: &Paths,
    hostnames: &[String],
) -> crate::Result<Option<String>> {
    let roster_opt = swarm_roster_until(paths, Instant::now() + Duration::from_millis(250))?;
    let Some(expected) = config(paths)? else {
        return Ok(None);
    };
    let Some(identity) = read_identity(paths)? else {
        return Ok(None);
    };
    if local_recipient_denied_in_roster(paths) {
        return Ok(None);
    }
    let recipient = identity.recipient().to_bech32();
    if let Some(roster) = &roster_opt
        && let Some(member) = roster.member_by_recipient(&recipient)
    {
        if roster.is_member_denied(member) {
            return Ok(None);
        }
        return Ok(Some(member.label.clone()));
    }
    if roster_opt.is_none() && !alab_vault(&expected) {
        return Ok(None);
    }

    let state = load_state(paths)?;
    let local_log = log_entries::trusted_local_log(paths, &expected)?;
    let log_only_bound = state.digest.is_none()
        && fsx::kind(&paths.cache_file())? == fsx::Kind::Missing
        && state.source_identity == Some(source_identity(&expected))
        && local_log.is_some();
    if state.membership != Stored::In
        || state.failed
        || !state.pending.is_empty()
        || expected.signer.is_none()
        || !pin_admitted(&expected, None)
        || (!source_bound(&expected, &state) && !log_only_bound)
    {
        return Ok(None);
    }

    let fallback_allowed = |canonical: &str| {
        roster_opt.as_ref().is_none_or(|roster| {
            !roster.is_label_denied(canonical)
                && roster
                    .member_by_label_or_alias(canonical)
                    .is_none_or(|m| m.recipient == recipient)
        })
    };

    if let Some(log) = &local_log {
        if log.recipient_is_removed(&identity.recipient())
            || log_entries::recipient_denied_by_roster_or_log(
                roster_opt.as_ref(),
                log,
                &identity.recipient(),
            )
            || log.audience_left_out_recipient(
                &crate::membership_log::EpochAudience::Everyone,
                &identity.recipient(),
            )
        {
            return Ok(None);
        }
        if let Some((_, member)) = log
            .membership_ref()
            .members()
            .iter()
            .filter(|(_, m)| m.binding.recipient() == identity.recipient())
            .max_by_key(|(actor, m)| (log.record_index(&m.entry).unwrap_or(0), *actor))
            && let Some((_, canonical)) =
                leading_m_label(&member.label).or_else(|| leading_m_label(&member.name))
        {
            if roster_opt
                .as_ref()
                .is_some_and(|r| log_entries::member_denied_by_roster(r, member))
            {
                return Ok(None);
            }
            if fallback_allowed(&canonical) {
                return Ok(Some(canonical));
            }
        }
        if let Some(founder) = log
            .membership_ref()
            .unproved()
            .iter()
            .find(|f| f.recipient == identity.recipient())
            && let Some((_, canonical)) =
                leading_m_label(&founder.label).or_else(|| leading_m_label(&founder.name))
        {
            if roster_opt.as_ref().is_some_and(|r| {
                r.is_label_denied(&canonical)
                    || founder.node_key.is_some_and(|nk| {
                        nk != [0; 32] && r.is_node_denied(&aterm_codec::hex::encode_lower(&nk))
                    })
            }) {
                return Ok(None);
            }
            if fallback_allowed(&canonical) {
                return Ok(Some(canonical));
            }
        }
    }

    // Fallback 1: name recorded in the vault for this recipient (admin locker or single-host
    // locker, then `source.toml`'s recorded `host`), only when it starts with `m<N>`.
    if let Some(cached) = held_copy(paths, &expected, &state)
        && let Ok(envelope) = envelope::parse(&cached)
        && let Some(recorded) = vault_recorded_name(&envelope, &identity, &recipient)
        && let Some((_, canonical)) = leading_m_label(&recorded)
    {
        if roster_opt
            .as_ref()
            .is_some_and(|r| r.is_label_denied(&canonical))
        {
            return Ok(None);
        }
        if fallback_allowed(&canonical) {
            return Ok(Some(canonical));
        }
    }
    if let Some((_, canonical)) = leading_m_label(&expected.host) {
        if roster_opt
            .as_ref()
            .is_some_and(|r| r.is_label_denied(&canonical))
        {
            return Ok(None);
        }
        if fallback_allowed(&canonical) {
            return Ok(Some(canonical));
        }
    }

    // Fallback 2: a hostname that starts with `m<N>` (never mid-string tokens).
    for host in hostnames {
        if let Some((_, canonical)) = leading_m_label(host)
            && fallback_allowed(&canonical)
        {
            return Ok(Some(canonical));
        }
    }
    Ok(None)
}

fn vault_recorded_name(
    envelope: &Envelope,
    identity: &Identity,
    recipient: &str,
) -> Option<String> {
    let identities = std::slice::from_ref(identity);
    if let Some(admin_locker) = envelope.admin_locker()
        && let Ok(plain) = age::decrypt(identities, admin_locker.armored.as_bytes())
        && let Ok(parsed) = parse_admin(&plain)
        && let Some(machine) = parsed
            .doc
            .machines()
            .into_iter()
            .find(|m| m.recipient == recipient)
    {
        return Some(machine.name);
    }
    for locker in envelope
        .lockers
        .iter()
        .filter(|locker| locker.label != "admin")
    {
        if let Ok(plain) = age::decrypt(identities, locker.armored.as_bytes())
            && let Ok(entries) = text::parse_locker(plain.as_slice())
        {
            for entry in entries {
                if let Audience::Hosts(hosts) = entry.audience
                    && hosts.len() == 1
                {
                    return Some(hosts[0].clone());
                }
            }
        }
    }
    None
}

/// Seed `aterm-swarm-roster` from the vault's current members in admission-date order and publish
/// it in `[everyone]` (`DESIGN-alab-swarm` §3.2, §12.5 step 1).
///
/// Preserves any existing `deny_labels` and `deny_nodes` if `aterm-swarm-roster` was already present.
///
/// # Errors
/// This Mac is not an admin of a configured vault, an existing policy/roster is malformed, or the
/// publication fails.
#[cfg(any(test, debug_assertions))]
pub fn admin_seed_swarm_roster(
    paths: &Paths,
    bonjour_hosts: &[String],
) -> crate::Result<SwarmRoster> {
    let admin = admin_open(paths)?;
    let mut doc = admin.doc.clone();
    let log = log_labels(paths)?;
    let roster = build_seeded_roster(&admin, &doc, bonjour_hosts, &log)?;
    doc.set_entry(
        &Audience::Everyone,
        ROSTER_ENTRY,
        roster.to_toml().as_bytes(),
    )
    .map_err(|e| Error::new(e.to_string()))?;
    admin_publish(paths, &admin, &doc)?;
    Ok(roster)
}

#[cfg(any(test, debug_assertions))]
fn admissions_map(admin: &Admin, doc: &Document) -> BTreeMap<String, SeedAdmission> {
    let mut map = BTreeMap::new();
    for machine in doc.machines() {
        if let Ok(rec) = Recipient::parse(&machine.recipient)
            && let Some(ev) = admin.admissions.evidence(&rec)
        {
            map.insert(
                machine.recipient,
                SeedAdmission {
                    admitted_ms: ev.authored_ms,
                    sequence: ev.sequence,
                },
            );
        }
    }
    map
}

#[cfg(any(test, debug_assertions))]
fn build_seeded_roster(
    admin: &Admin,
    doc: &Document,
    bonjour_hosts: &[String],
    log: &(BTreeMap<String, String>, Vec<String>),
) -> crate::Result<SwarmRoster> {
    let policy = SwarmPolicy::from_document(doc)?.unwrap_or_default();
    let sheet = SheetHints::from_document(doc);
    let prior = SwarmRoster::from_document(doc)?;
    let admissions = admissions_map(admin, doc);
    let mut all_hosts = bonjour_hosts.to_vec();
    all_hosts.extend(log.1.iter().cloned());
    let mut roster = seed_roster(doc, &admissions, &policy, &sheet, &all_hosts, &log.0);
    if let Some(existing) = prior {
        roster.merge_deny_lists(&existing);
    }
    Ok(roster)
}

pub(super) fn historical_roster(doc: &Document) -> crate::Result<SwarmRoster> {
    Ok(SwarmRoster::from_document(doc)?.unwrap_or_default())
}

/// The labels the membership log at `paths` holds (F15: the allowing member assigns `m<N>` in
/// its `Admit`): each member's by its age recipient, and every label any founder or `Admit`
/// claims. Empty where this Mac holds no log.
pub(super) fn log_labels(paths: &Paths) -> crate::Result<(BTreeMap<String, String>, Vec<String>)> {
    let Some(config) = config(paths)? else {
        return Ok((BTreeMap::new(), Vec::new()));
    };
    let store = super::load_records_for(&Reads::new(paths), &config)?;
    let store = log_entries::checked_author_projection_for(paths, &config, &store)?;
    let Some(log) = store.log() else {
        return Ok((BTreeMap::new(), Vec::new()));
    };
    let membership = log.membership();
    let mut active_members: Vec<_> = membership
        .members()
        .iter()
        .filter(|(actor, member)| {
            let rec = member.binding.recipient();
            !log.recipient_is_removed(&rec)
                && !log.audience_left_out(&crate::membership_log::EpochAudience::Everyone, actor)
                && !log.audience_left_out_recipient(
                    &crate::membership_log::EpochAudience::Everyone,
                    &rec,
                )
                && !membership.member_node_removed(member)
        })
        .collect();
    active_members.sort_by_key(|(_, m)| (log.record_index(&m.entry).unwrap_or(0), m.entry));
    let by_recipient = membership
        .unproved()
        .iter()
        .filter(|founder| {
            !log.recipient_is_removed(&founder.recipient)
                && !log.audience_left_out_recipient(
                    &crate::membership_log::EpochAudience::Everyone,
                    &founder.recipient,
                )
                && !founder
                    .node_key
                    .filter(|k| *k != [0; 32])
                    .is_some_and(|nk| {
                        matches!(
                            membership.node(&nk),
                            crate::membership_log::NodeStanding::Removed(_)
                        )
                    })
        })
        .map(|founder| (founder.recipient.to_bech32(), founder.label.clone()))
        .chain(
            active_members
                .into_iter()
                .map(|(_, member)| (member.binding.recipient().to_bech32(), member.label.clone())),
        )
        .collect();
    let all = membership.claimed_labels().iter().cloned().collect();
    Ok((by_recipient, all))
}

/// Keep only existing historical denial evidence aligned with envelope renames.
/// Registry registration owns labels; an approval never creates a roster label row.
pub(super) fn maintain_swarm_roster_on_approve(
    doc: &mut Document,
    newly_approved: &[(String, String, String)],
) -> crate::Result<()> {
    let Some(mut roster) = SwarmRoster::from_document(doc)? else {
        return Ok(());
    };
    let mut changed = false;
    for (recipient, _, vault_name) in newly_approved {
        if let Some(row) = roster
            .members
            .iter_mut()
            .find(|row| row.recipient == *recipient)
            && row.vault_name != *vault_name
        {
            row.vault_name.clone_from(vault_name);
            changed = true;
        }
    }
    if changed {
        doc.set_entry(
            &Audience::Everyone,
            ROSTER_ENTRY,
            roster.to_toml().as_bytes(),
        )
        .map_err(|e| Error::new(e.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "roster_tests.rs"]
mod tests;
