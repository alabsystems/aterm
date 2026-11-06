// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Client and approver bindings for `aterm-swarm-roster` and `aterm-swarm-policy`
//! (`docs/DESIGN-alab-swarm-2026-10-27.md` §3.2, §4.1, §4.6, §12.3 C11).

use super::*;
use crate::membership_log::Log;
use crate::swarm_roster::{
    Newcomer, POLICY_ENTRY, ROSTER_ENTRY, SeedAdmission, SheetHints, SwarmPolicy, SwarmRoster,
    assign_newcomer, leading_m_label, seed_roster,
};

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
    match StateLock::observe_narrowed(paths)? {
        StateObservation::Held(lock) => read(RosterObservation::Held(&lock)).map(Some),
        StateObservation::Busy => Ok(None),
        StateObservation::Absent => {
            if config(paths)?.is_some() {
                return Ok(None);
            }
            let value = read(RosterObservation::Standalone)?;
            Ok((matches!(
                StateLock::observe_classified(paths)?,
                StateObservation::Absent
            ) && config(paths)?.is_none())
            .then_some(value))
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
    let (labels, nodes, _) = restored_by_log_parts(log);
    (labels, nodes)
}

fn restored_by_log_parts(
    log: &crate::membership_log::Log,
) -> (BTreeSet<String>, BTreeSet<String>, BTreeSet<String>) {
    let membership = log.membership();
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
                    || membership.get(&active).is_some_and(|m| {
                        log.ancestor_closure(&[m.entry]).iter().any(|r| {
                            matches!(
                                r.body(),
                                crate::membership_log::Body::Remove { member, .. }
                                    if member == actor
                            )
                        })
                    })
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
            for r in log.ancestor_closure(&[member.entry]) {
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
    (labels, nodes, recipients)
}

pub(super) fn lift_restored_from_roster(
    log: &crate::membership_log::Log,
    roster: &mut SwarmRoster,
) -> Vec<String> {
    let (mut labels, mut nodes, recipients) = restored_by_log_parts(log);
    if labels.is_empty() && nodes.is_empty() && recipients.is_empty() {
        return Vec::new();
    }
    let membership = log.membership();
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
    let membership = log.membership();
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
    let table: aterm_toml::Table = aterm_toml::from_str(text)
        .map_err(|e| Error::new(format!("invalid {}: {e}", crate::SWARM_LEASE_ENTRY)))?;
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
            .membership()
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
            .membership()
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

fn swarm_roster_enabled(admin: &Admin, doc: &Document) -> bool {
    alab_vault(&admin.config)
        || doc.entry(&Audience::Everyone, ROSTER_ENTRY).is_some()
        || doc.entries().iter().any(|entry| entry.name == POLICY_ENTRY)
}

pub(super) fn load_or_seed_swarm_roster_with_log(
    admin: &Admin,
    doc: &Document,
    log: &(BTreeMap<String, String>, Vec<String>),
) -> crate::Result<SwarmRoster> {
    if let Some(existing) = SwarmRoster::from_document(doc)? {
        return Ok(existing);
    }
    let policy = SwarmPolicy::from_document(doc)?.unwrap_or_default();
    let sheet = SheetHints::from_document(doc);
    let admissions = admissions_map(admin, &admin.published_doc);
    Ok(seed_roster(
        &admin.published_doc,
        &admissions,
        &policy,
        &sheet,
        &log.1,
        &log.0,
    ))
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

/// Update `aterm-swarm-roster` in `doc` when the approver admits `newly_approved`
/// (`(recipient, requested_host, vault_name)`), as C11 under F15 has it: the allowing member
/// assigned the `m<N>` in its `Admit`, so the roster follows the membership log (`log`, from
/// [`log_labels`]).
///
/// - A Mac the log labels with a bare `m<N>` takes that label as given, the sheet's 24-hour hold
///   included (the person's Allow settled it), unless another live row holds it (one of a Mac
///   the vault still names, which the deny-lists do not name), the deny-list names it, or the
///   policy reserves it; then it gets the roster's own label.
/// - A Mac the log does not label (F10's fallback, a reinstalled Mac, a code approval) never
///   takes an `m<N>` the log holds for another Mac: asking under one, it gets the next free one,
///   which counts the log's labels.
/// - A Mac whose log label carries a split's suffix (`m40-3fa2`) gets the next free one: the
///   roster holds bare `m<N>` labels only.
///
/// No-op on personal vaults that carry neither `aterm-swarm-roster` nor `aterm-swarm-policy`.
pub(super) fn maintain_swarm_roster_on_approve(
    admin: &Admin,
    doc: &mut Document,
    newly_approved: &[(String, String, String)],
    log: &(BTreeMap<String, String>, Vec<String>),
    now_ms: u64,
) -> crate::Result<()> {
    if newly_approved.is_empty() || !swarm_roster_enabled(admin, doc) {
        return Ok(());
    }
    let policy = SwarmPolicy::from_document(doc)?.unwrap_or_default();
    let sheet = SheetHints::from_document(doc);
    let mut roster = load_or_seed_swarm_roster_with_log(admin, doc, log)?;
    let (log_by_recipient, log_held) = log;
    let log_numbers: BTreeSet<String> = log_held
        .iter()
        .filter_map(|label| leading_m_label(label).map(|(_, canonical)| canonical))
        .collect();
    let held_by_log =
        |host: &str| leading_m_label(host).is_some_and(|(_, label)| log_numbers.contains(&label));
    // The Macs the vault still names: a row of one it removed holds no label, as in
    // [`assign_newcomer`].
    let active: BTreeSet<String> = doc.machines().into_iter().map(|m| m.recipient).collect();
    for (recipient, requested_host, vault_name) in newly_approved {
        let fresh = !roster.members.iter().any(|row| &row.recipient == recipient);
        let logged = log_by_recipient
            .get(recipient)
            .filter(|label| crate::membership_log::is_label(label));
        let requested: &str = match (logged, log_by_recipient.get(recipient)) {
            (Some(label), _) => label,
            (None, Some(_)) => "",
            (None, None) if held_by_log(requested_host) => "",
            (None, None) => requested_host,
        };
        let assigned = assign_newcomer(
            &mut roster,
            doc,
            &policy,
            &sheet,
            log_held,
            Newcomer {
                recipient,
                requested_host: requested,
                vault_name,
                now_ms,
            },
        );
        let Some(label) = logged.filter(|label| fresh && **label != assigned) else {
            continue;
        };
        let held_elsewhere = roster.members.iter().any(|row| {
            &row.recipient != recipient
                && !roster.is_member_denied(row)
                && active.contains(&row.recipient)
                && (row.label == *label || row.aliases.contains(label))
        });
        if held_elsewhere
            || roster.is_label_denied(label)
            || policy
                .effective_reserved(doc, Some(&roster))
                .contains(label)
        {
            continue;
        }
        if let Some(row) = roster
            .members
            .iter_mut()
            .find(|row| &row.recipient == recipient)
        {
            row.label.clone_from(label);
            row.claimed_label = None;
            row.claimed_at_ms = None;
            row.normalize_aliases();
        }
        roster.canonicalize();
    }
    doc.set_entry(
        &Audience::Everyone,
        ROSTER_ENTRY,
        roster.to_toml().as_bytes(),
    )
    .map_err(|e| Error::new(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_read_before_an_authority_change_cannot_supply_the_new_roster() {
        let macs = super::super::log_entries_tests::two_macs(&[]);
        let paths = &macs.m22;
        let store = load_records(paths).unwrap();
        let log = store.log().unwrap();
        assert!(
            log_entries::local_swarm_roster_for_log(paths, log)
                .unwrap()
                .is_some()
        );
        {
            let lock = StateLock::take(paths).unwrap();
            let mut next = config(paths).unwrap().unwrap();
            next.signer = Some([77; 32]);
            next.followed.clear();
            let mut state = load_state(paths).unwrap();
            state.source_identity = Some(source_identity(&next));
            save_state(paths, &state).unwrap();
            save_config(paths, &next).unwrap();
            assert!(log_entries::local_swarm_roster_for_log_held(paths, log, &lock).is_err());
        }
        assert!(log_entries::local_swarm_roster_for_log(paths, log).is_err());
        macs.server.stop();
    }

    #[test]
    fn an_unreadable_membership_log_cannot_admit_a_cached_admin() {
        struct FailedReads(PathBuf);
        impl Drop for FailedReads {
            fn drop(&mut self) {
                fsx::fail_reads(&self.0, 0);
            }
        }
        let macs = super::super::log_entries_tests::two_macs(&[]);
        let paths = &macs.m22;
        let admin = admin_open(paths).unwrap();
        let identity = read_identity(paths).unwrap().unwrap();
        let bytes = fsx::read_private(&paths.cache_file(), envelope::MAX_ENVELOPE)
            .unwrap()
            .unwrap();
        let envelope = envelope::parse(bytes.as_slice()).unwrap();
        assert!(
            log_entries::trusted_seeded_log(paths, &admin.config, &envelope)
                .unwrap()
                .is_some()
        );
        let refused = || {
            super::super::admin_removed_in_log(
                paths,
                &admin.config,
                &envelope,
                &identity,
                &admin.doc,
            )
        };
        assert!(!refused());
        let fault = FailedReads(paths.records_file());
        fsx::fail_reads(&fault.0, usize::MAX);
        assert!(refused());
        drop(fault);
        assert!(!refused());
        macs.server.stop();
    }

    /// A roster read holds the state lock from before its one `Reads` to after it, so no writer
    /// moves the source under it, and what it reads carries the log's removal over an older entry.
    /// No source moves here: the lock excludes that, and the reader the hold is watched on is
    /// [`with_roster_observation`]. Until 2026-11-05 this was
    /// `a_source_move_during_a_roster_read_cannot_lose_log_removals`, whose firing with a writer
    /// planted at each cut `docs/measured/authorization-read-integration-2026-11-03.md` records.
    ///
    /// NEGATIVE CONTROLS (2026-11-05): with the `Held` arm of [`with_roster_observation`] releasing
    /// its lock, or with `file.try_lock_shared()` gone from `StateLock::classify`, it fails at
    /// "a writer took the state lock under a roster read".
    #[test]
    fn a_roster_read_keeps_a_writer_out_and_its_overlay_keeps_a_log_removal() {
        let macs = super::super::log_entries_tests::two_macs(&[]);
        let paths = &macs.m22;
        let before = admin_seed_swarm_roster(paths, &[]).unwrap();
        assert!(before.member_by_label_or_alias("m7").is_some());
        super::super::remove::test_remove_unposted(paths, "m7").unwrap();
        // Keep the earlier roster entry: its removed row is excluded by the log overlay,
        // independently of when a writer next refreshes that entry.
        super::super::remove::test_put_everyone(paths, ROSTER_ENTRY, before.to_toml().as_bytes())
            .unwrap();
        let safe = |roster: Option<&SwarmRoster>| {
            roster.is_none_or(|roster| {
                roster.is_label_denied("m7")
                    && roster
                        .member_by_label_or_alias("m7")
                        .is_none_or(|member| roster.is_member_denied(member))
            })
        };
        let raw = base_swarm_roster(paths).unwrap().unwrap();
        assert!(raw.member_by_label_or_alias("m7").is_some());
        assert!(!raw.is_label_denied("m7"));
        assert!(safe(Some(
            &swarm_roster_until(
                paths,
                std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN
            )
            .unwrap()
            .unwrap()
        )));

        let read = with_roster_observation(paths, |observation| {
            let lock = observation
                .state_lock()
                .expect("a vault's read holds its lock");
            assert!(
                StateLock::try_take(paths).unwrap().is_none(),
                "a writer took the state lock under a roster read"
            );
            swarm_roster_held(paths, lock)
        });
        assert!(safe(read.unwrap().unwrap().as_ref()));
        macs.server.stop();
    }

    #[test]
    fn overlay_keeps_a_members_denial_when_its_log_node_key_replaces_the_roster_key() {
        use crate::commons::binding::Request;
        use crate::membership_log::{Entrant, Founder, Log, Record, Root};

        let entrant = |tag, label: &str| {
            let identity = Identity::from_secret_bytes([tag; 32]);
            let request = Request::create(
                [2; 16], [3; 32], [tag; 16], [tag; 16], &identity, &[tag; 32],
            )
            .unwrap();
            Entrant {
                request,
                node_key: [tag; 32],
                name: label.to_owned(),
                label: label.to_owned(),
            }
        };
        for unproved in [false, true] {
            let first = entrant(1, "m1");
            let second = entrant(2, "m2");
            let root = Root::of(first.request.binding());
            let recipient = second.request.binding().recipient().to_bech32();
            let mut second = Founder::from(second);
            if unproved {
                second.binding = None;
            }
            let log = Log::new(
                Record::genesis(&[99; 32], root, vec![Founder::from(first), second], 1).unwrap(),
            )
            .unwrap();
            let old_key = "03".repeat(32);
            let new_key = "02".repeat(32);
            let node_id = "44".repeat(16);
            for denial in 0..4 {
                let mut roster = SwarmRoster {
                    members: vec![crate::swarm_roster::SwarmMember {
                        recipient: recipient.clone(),
                        vault_name: "m2".to_owned(),
                        label: "m2".to_owned(),
                        aliases: vec!["m20".to_owned()],
                        hint: None,
                        hw_hint: None,
                        admitted_ms: 1,
                        claimed_label: None,
                        claimed_at_ms: None,
                        node_id: Some(node_id.clone()),
                        node_key: Some(old_key.clone()),
                    }],
                    deny_labels: if denial == 1 {
                        vec!["m20".to_owned()]
                    } else {
                        Vec::new()
                    },
                    deny_nodes: match denial {
                        2 => vec![node_id.clone()],
                        3 => vec![old_key.clone()],
                        _ => Vec::new(),
                    },
                };
                if denial == 3 {
                    let mut lost = roster.clone();
                    lost.members[0].node_key = Some(new_key.clone());
                    assert!(
                        !steward::member_is_denied_in_roster(&lost, &lost.members[0]),
                        "negative control: blindly replacing the key loses its denial"
                    );
                }
                overlay_log_membership(&log, None, &mut roster);
                assert_eq!(
                    roster.is_node_denied(&new_key),
                    denial != 0,
                    "unproved={unproved}, denial={denial}"
                );
                assert_eq!(
                    steward::member_is_denied_in_roster(
                        &roster,
                        roster.member_by_recipient(&recipient).unwrap(),
                    ),
                    denial != 0
                );
                let denied = roster.deny_nodes.clone();
                overlay_log_membership(&log, None, &mut roster);
                assert_eq!(
                    roster.deny_nodes, denied,
                    "replaying the overlay preserves the denied keys"
                );
            }
        }
    }

    /// Seeding a vault's `aterm-swarm-roster` never materializes a file in `~/.secrets`,
    /// `resolve_local_swarm_label` reads the member's assigned `m<N>` label (ignoring
    /// `~/.aterm/machine.toml` `id = "m22"`), and approving a newcomer named `m3` or
    /// `Mac-mini-M4` through `approve_request` assigns `next_free` rather than `m3` or `m4`.
    #[test]
    fn swarm_roster_stays_unmaterialized_and_approver_assigns_labels_end_to_end() {
        // Every server it reads is its own on loopback, which answers.
        let _answering = ServersAnswering::set();
        let root = aterm_tempfile::tempdir().unwrap();
        let server_dir = root.path().join("server");
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let running = crate::server::start(
            socket,
            &crate::server::Options {
                listen: "127.0.0.1:0".parse().unwrap(),
                store: server_dir,
                advertise: None,
                pins: Vec::new(),
                hold: Duration::from_millis(200),
                keep_awake: false,
            },
        )
        .unwrap();
        let url = format!("http://{}/v1/test", running.addr);

        let admin_paths = Paths::for_home(&root.path().join("admin"));
        let created = create(&admin_paths, &url, None, Some("m22")).unwrap();
        let pinned_source = format!("{url}#{}", created.signer);

        // Poison `~/.aterm/machine.toml` on the member to prove swarm identity never reads it!
        let member_m4_home = root.path().join("member-m4");
        std::fs::create_dir_all(member_m4_home.join(".aterm")).unwrap();
        std::fs::write(member_m4_home.join(".aterm/machine.toml"), "id = \"m22\"\n").unwrap();

        // Seed `aterm-swarm-roster` on the admin.
        let seeded = admin_seed_swarm_roster(&admin_paths, &[]).unwrap();
        assert_eq!(seeded.members.len(), 1);
        assert_eq!(seeded.members[0].label, "m22");
        assert!(
            !admin_paths
                .home
                .join(".secrets")
                .join(ROSTER_ENTRY)
                .exists(),
            "reserved entry must never materialize into ~/.secrets"
        );

        // Put `aterm-swarm-policy` and `fleet-accounts.toml` in the vault so `m110` is in the sheet.
        {
            let adm = admin_open(&admin_paths).unwrap();
            let mut doc = adm.doc.clone();
            let policy = SwarmPolicy {
                network_id: Some("test-net".to_owned()),
                reserved_labels: vec!["m3".to_owned(), "m22".to_owned()],
                release_hosts: vec!["m3".to_owned(), "m22".to_owned()],
                servers: Vec::new(),
                stewards: Vec::new(),
            };
            let admins = doc.admins();
            doc.set_entry(
                &Audience::Hosts(admins),
                POLICY_ENTRY,
                policy.to_toml().as_bytes(),
            )
            .unwrap();
            doc.set_entry(
                &Audience::Everyone,
                "fleet-accounts.toml",
                br#"
[[account]]
label = "acct-1"
machines = ["m110"]
class = "max"
email = "one@example.org"
"#,
            )
            .unwrap();
            doc.set_entry(
                &Audience::Everyone,
                crate::SWARM_LEASE_ENTRY,
                b"holder = \"m22\"\nepoch = 1\n",
            )
            .unwrap();
            doc.set_open(false).unwrap();
            admin_publish(&admin_paths, &adm, &doc).unwrap();
        }

        // Join two newcomers by code: one named `mac-mini-m4` (from `Mac-mini-M4`) and one named `m3`.
        let member_m4_paths = Paths::for_home(&member_m4_home);
        let m4_id = machine_identity(&member_m4_paths).unwrap();
        let m4_joined = join(
            &member_m4_paths,
            &JoinOptions {
                source: pinned_source.clone(),
                host: Some("mac-mini-m4".to_owned()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .unwrap();
        assert!(matches!(m4_joined, JoinOutcome::Pending { .. }));
        approve_request(&admin_paths, &m4_id.recipient().to_bech32()).unwrap();
        sync(&member_m4_paths, None).unwrap();

        let member_m3_paths = Paths::for_home(&root.path().join("member-m3"));
        let m3_id = machine_identity(&member_m3_paths).unwrap();
        let m3_joined = join(
            &member_m3_paths,
            &JoinOptions {
                source: pinned_source,
                host: Some("m3".to_owned()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .unwrap();
        assert!(matches!(m3_joined, JoinOutcome::Pending { .. }));
        approve_request(&admin_paths, &m3_id.recipient().to_bech32()).unwrap();
        sync(&member_m3_paths, None).unwrap();

        // Neither member has `aterm-swarm-roster` materialized as a file!
        assert!(
            !member_m4_paths
                .home
                .join(".secrets")
                .join(ROSTER_ENTRY)
                .exists()
        );
        assert!(
            !member_m3_paths
                .home
                .join(".secrets")
                .join(ROSTER_ENTRY)
                .exists()
        );

        // `Mac-mini-M4` (`mac-mini-m4`) got m111 (above sheet's m110), NOT m4 and NOT `~/.aterm/machine.toml`'s m22!
        let resolved_m4 =
            resolve_local_swarm_label(&member_m4_paths, &["Mac-mini-M4".to_owned()]).unwrap();
        assert_eq!(resolved_m4.as_deref(), Some("m111"));

        // Newcomer named `m3` got m112, NOT reserved `m3`!
        let resolved_m3 = resolve_local_swarm_label(&member_m3_paths, &["m3".to_owned()]).unwrap();
        assert_eq!(resolved_m3.as_deref(), Some("m112"));
        assert!(
            !member_m4_paths
                .home
                .join(".secrets")
                .join(crate::SWARM_LEASE_ENTRY)
                .exists()
        );
        assert_eq!(
            swarm_lease_holder(&member_m4_paths).unwrap().as_deref(),
            Some("m22")
        );
        {
            let lock = StateLock::take(&member_m4_paths).unwrap();
            assert_eq!(
                swarm_roster_until(&member_m4_paths, std::time::Instant::now())
                    .unwrap_err()
                    .kind(),
                crate::ErrorKind::VaultBusy,
            );
            let under_lock = swarm_roster_held(&member_m4_paths, &lock)
                .unwrap()
                .expect("roster remains readable by same-thread StateLock holder");
            assert!(
                under_lock.member_by_label_or_alias("m111").is_some(),
                "expected m111 in roster under StateLock, got {under_lock:?}"
            );
        }

        // A Mac the log labels m110 takes it over a row that still says m110 when that row is
        // of a Mac the vault removed, or one whose node is denied. Control: a live row of a Mac
        // the vault names keeps m110, and the newcomer keeps the roster's own label.
        let adm = admin_open(&admin_paths).unwrap();
        for (case, named, denied, takes) in [
            ("removed", false, false, true),
            ("denied", true, true, true),
            ("live", true, false, false),
        ] {
            let mut doc = adm.doc.clone();
            let stale = Identity::generate().unwrap().recipient().to_bech32();
            if named {
                doc.add_machine(&format!("stale-{case}"), &stale).unwrap();
            }
            let mut roster = SwarmRoster::from_document(&doc).unwrap().unwrap();
            roster.members.push(crate::swarm_roster::SwarmMember {
                recipient: stale,
                vault_name: format!("stale-{case}"),
                label: "m110".to_owned(),
                aliases: Vec::new(),
                hint: None,
                hw_hint: None,
                admitted_ms: 1_000,
                claimed_label: None,
                claimed_at_ms: None,
                node_id: Some("stale-node".to_owned()),
                node_key: None,
            });
            if denied {
                roster.deny_nodes.push("stale-node".to_owned());
            }
            doc.set_entry(
                &Audience::Everyone,
                ROSTER_ENTRY,
                roster.to_toml().as_bytes(),
            )
            .unwrap();
            let newcomer = Identity::generate().unwrap().recipient().to_bech32();
            doc.add_machine(&format!("logged-{case}"), &newcomer)
                .unwrap();
            let log = (
                BTreeMap::from([(newcomer.clone(), "m110".to_owned())]),
                vec!["m110".to_owned()],
            );
            maintain_swarm_roster_on_approve(
                &adm,
                &mut doc,
                &[(
                    newcomer.clone(),
                    "m110".to_owned(),
                    format!("logged-{case}"),
                )],
                &log,
                9_000,
            )
            .unwrap();
            let after = SwarmRoster::from_document(&doc).unwrap().unwrap();
            let label = &after.member_by_recipient(&newcomer).unwrap().label;
            assert_eq!(label == "m110", takes, "{case}: {label}");
        }
        running.stop();
    }

    #[test]
    fn overlay_log_membership_moves_log_removed_members_to_deny_labels_and_deny_nodes() {
        use crate::commons::binding::Request;
        use crate::membership_log::{Body, Entrant, Evidence, Log, Record};

        let mk_entrant = |tag: u8, label: &str| {
            let id = Identity::from_secret_bytes([tag ^ 0x55; 32]);
            let req = Request::create([2; 16], [3; 32], [tag; 16], [tag; 16], &id, &[tag; 32])
                .expect("request");
            (
                id,
                Entrant {
                    request: req,
                    node_key: [tag; 32],
                    name: format!("{label}.local"),
                    label: label.to_owned(),
                },
            )
        };

        let (m1_id, m1_ent) = mk_entrant(1, "m1");
        let (m2_id, m2_ent) = mk_entrant(2, "m2");
        let (m7_id, m7_ent) = mk_entrant(7, "m7");
        let m7_rebound_req =
            Request::create([2; 16], [3; 32], [17; 16], [17; 16], &m7_id, &[17; 32])
                .expect("m7 rebound request");
        let m7_rebound_ent = Entrant {
            request: m7_rebound_req,
            node_key: [17; 32],
            name: "m7.local".to_owned(),
            label: "m7".to_owned(),
        };
        let m1_actor = m1_ent.request.binding().actor();
        let m2_actor = m2_ent.request.binding().actor();
        let m7_actor = m7_rebound_ent.request.binding().actor();

        let genesis = Record::found(&[99; 32], vec![m1_ent, m2_ent], 1_000).expect("genesis");
        let genesis_id = *genesis.id();
        let mut log = Log::new(genesis).expect("log");

        let admit_m7_old = log
            .draft(
                &m1_actor,
                2_000,
                Body::Admit {
                    entrant: m7_ent,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&[1; 32], [1; 16])
            .expect("sign admit old m7");
        log.insert(admit_m7_old).expect("insert admit old m7");

        let admit_m7 = log
            .draft(
                &m1_actor,
                2_500,
                Body::Admit {
                    entrant: m7_rebound_ent,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&[1; 32], [1; 16])
            .expect("sign admit");
        let admit_m7_id = *admit_m7.id();
        log.insert(admit_m7).expect("insert admit");

        let remove_m2 = log
            .draft(
                &m1_actor,
                3_000,
                Body::Remove {
                    member: m2_actor,
                    cutoff: genesis_id,
                },
            )
            .sign(&[1; 32], [1; 16])
            .expect("sign remove m2");
        log.insert(remove_m2).expect("insert remove m2");

        let remove_m7 = log
            .draft(
                &m1_actor,
                4_000,
                Body::Remove {
                    member: m7_actor,
                    cutoff: admit_m7_id,
                },
            )
            .sign(&[1; 32], [1; 16])
            .expect("sign remove m7");
        log.insert(remove_m7).expect("insert remove m7");
        assert!(log.recipient_is_removed(&m7_id.recipient()));

        // Removed m7 attempts to self-Admit (claiming another recipient and label "m41") and
        // rename itself to "m43.local"; neither un-removes m7's recipient nor poisons m41/m43 in
        // `roster.deny_labels`.
        let (_m41_id, mut m41_ent) = mk_entrant(41, "m41");
        let self_forged_req = Request::create(
            [2; 16],
            [3; 32],
            [17; 16],
            [17; 16],
            &Identity::from_secret_bytes([0x99; 32]),
            &[17; 32],
        )
        .expect("forged self request");
        m41_ent.request = self_forged_req;
        let bogus_self_admit = log
            .draft(
                &m7_actor,
                4_500,
                Body::Admit {
                    entrant: m41_ent,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&[17; 32], [17; 16])
            .expect("sign bogus self admit");
        log.insert(bogus_self_admit)
            .expect("insert bogus self admit");
        let bogus_name = log
            .draft(
                &m7_actor,
                4_600,
                Body::Name {
                    name: "m43.local".to_owned(),
                },
            )
            .sign(&[17; 32], [17; 16])
            .expect("sign bogus name");
        log.insert(bogus_name).expect("insert bogus name");

        let mut roster = SwarmRoster {
            members: vec![
                crate::swarm_roster::SwarmMember {
                    recipient: m1_id.recipient().to_bech32(),
                    vault_name: "m1".to_owned(),
                    label: "m1".to_owned(),
                    aliases: Vec::new(),
                    hint: None,
                    hw_hint: None,
                    admitted_ms: 1_000,
                    claimed_label: None,
                    claimed_at_ms: None,
                    node_id: Some("11111111111111111111111111111111".to_owned()),
                    node_key: None,
                },
                crate::swarm_roster::SwarmMember {
                    recipient: m2_id.recipient().to_bech32(),
                    vault_name: "m2".to_owned(),
                    label: "m2".to_owned(),
                    aliases: vec!["m9".to_owned()],
                    hint: None,
                    hw_hint: None,
                    admitted_ms: 1_000,
                    claimed_label: Some("m10".to_owned()),
                    claimed_at_ms: Some(1_000),
                    node_id: Some("22222222222222222222222222222222".to_owned()),
                    node_key: None,
                },
            ],
            deny_labels: Vec::new(),
            deny_nodes: Vec::new(),
        };
        overlay_log_membership(&log, None, &mut roster);

        assert_eq!(roster.members.len(), 1);
        assert_eq!(roster.members[0].label, "m1");
        assert_eq!(
            roster.deny_labels,
            vec![
                "m10".to_owned(),
                "m2".to_owned(),
                "m7".to_owned(),
                "m9".to_owned()
            ]
        );
        assert_eq!(
            roster.deny_nodes,
            vec![
                "0202020202020202020202020202020202020202020202020202020202020202".to_owned(),
                "0707070707070707070707070707070707070707070707070707070707070707".to_owned(),
                "1111111111111111111111111111111111111111111111111111111111111111".to_owned(),
                "22222222222222222222222222222222".to_owned(),
            ]
        );
    }

    #[test]
    fn overlay_log_membership_cascades_key_denied_members_to_deny_labels_and_deny_nodes() {
        use crate::commons::binding::Request;
        use crate::membership_log::{Entrant, Founder, Log, Record, Root};

        let entrant = |tag: u8, label: &str| {
            let identity = Identity::from_secret_bytes([tag; 32]);
            let request = Request::create(
                [2; 16], [3; 32], [tag; 16], [tag; 16], &identity, &[tag; 32],
            )
            .unwrap();
            Entrant {
                request,
                node_key: [tag; 32],
                name: format!("{label}.local"),
                label: label.to_owned(),
            }
        };
        for unproved in [false, true] {
            let first = entrant(1, "m1");
            let second = entrant(2, "m2");
            let third = entrant(3, "m3");
            let root = Root::of(first.request.binding());
            let second_rec = second.request.binding().recipient().to_bech32();
            let mut second_founder = Founder::from(second);
            let mut third_founder = Founder::from(third);
            if unproved {
                second_founder.binding = None;
                third_founder.binding = None;
            }
            let log = Log::new(
                Record::genesis(
                    &[99; 32],
                    root,
                    vec![Founder::from(first), second_founder, third_founder],
                    1,
                )
                .unwrap(),
            )
            .unwrap();
            let second_nk = "02".repeat(32);
            let third_nk = "03".repeat(32);
            let prior_nid = "aa".repeat(16);
            let prior_nk = "bb".repeat(32);
            let mut roster = SwarmRoster {
                members: vec![crate::swarm_roster::SwarmMember {
                    recipient: second_rec,
                    vault_name: "m2".to_owned(),
                    label: "m2".to_owned(),
                    aliases: vec!["m20".to_owned()],
                    hint: None,
                    hw_hint: None,
                    admitted_ms: 1,
                    claimed_label: Some("m21".to_owned()),
                    claimed_at_ms: Some(1),
                    node_id: Some(prior_nid.clone()),
                    node_key: Some(prior_nk.clone()),
                }],
                deny_labels: Vec::new(),
                deny_nodes: vec![second_nk.clone(), third_nk.clone()],
            };
            overlay_log_membership(&log, None, &mut roster);
            assert_eq!(roster.members.len(), 1);
            assert_eq!(roster.members[0].label, "m1");
            assert_eq!(
                roster.deny_labels,
                vec![
                    "m2".to_owned(),
                    "m20".to_owned(),
                    "m21".to_owned(),
                    "m3".to_owned()
                ],
                "unproved={unproved}"
            );
            assert_eq!(
                roster.deny_nodes,
                vec![second_nk, third_nk, prior_nid, prior_nk],
                "unproved={unproved}"
            );
        }
    }

    #[test]
    fn overlay_log_membership_does_not_deny_renumbered_member_or_suppress_removed_colliding_label()
    {
        use crate::commons::binding::Request;
        use crate::membership_log::{Body, Entrant, Log, Record};

        let mk_entrant = |tag: u8, label: &str, name: &str| {
            let id = Identity::from_secret_bytes([tag ^ 0x33; 32]);
            let req = Request::create([2; 16], [3; 32], [tag; 16], [tag; 16], &id, &[tag; 32])
                .expect("request");
            (
                id,
                Entrant {
                    request: req,
                    node_key: [tag; 32],
                    name: name.to_owned(),
                    label: label.to_owned(),
                },
            )
        };

        let (m1_id, m1_ent) = mk_entrant(1, "m1", "m1.local");
        let (m2_id, m2_ent) = mk_entrant(2, "m2", "m2.local");
        // m70 collided on hostname `m2.local` when admitted, so its log `name` is `m2.local`
        // while its assigned label is `m70` and its `[machines]` vault_name is `m2-2`.
        let (m70_id, m70_ent) = mk_entrant(70, "m70", "m2.local");
        let m1_actor = m1_ent.request.binding().actor();
        let m2_actor = m2_ent.request.binding().actor();
        let m70_actor = m70_ent.request.binding().actor();

        let genesis =
            Record::found(&[99; 32], vec![m1_ent, m2_ent, m70_ent], 1_000).expect("genesis");
        let genesis_id = *genesis.id();
        let mut log = Log::new(genesis).expect("log");

        let remove_m2 = log
            .draft(
                &m1_actor,
                2_000,
                Body::Remove {
                    member: m2_actor,
                    cutoff: genesis_id,
                },
            )
            .sign(&[1; 32], [1; 16])
            .expect("sign remove m2");
        log.insert(remove_m2).expect("insert remove m2");

        let mut roster = SwarmRoster {
            members: vec![
                crate::swarm_roster::SwarmMember {
                    recipient: m1_id.recipient().to_bech32(),
                    vault_name: "m1".to_owned(),
                    label: "m1".to_owned(),
                    aliases: Vec::new(),
                    hint: None,
                    hw_hint: None,
                    admitted_ms: 1_000,
                    claimed_label: None,
                    claimed_at_ms: None,
                    node_id: None,
                    node_key: None,
                },
                crate::swarm_roster::SwarmMember {
                    recipient: m2_id.recipient().to_bech32(),
                    vault_name: "m2".to_owned(),
                    label: "m2".to_owned(),
                    aliases: Vec::new(),
                    hint: None,
                    hw_hint: None,
                    admitted_ms: 1_000,
                    claimed_label: None,
                    claimed_at_ms: None,
                    node_id: None,
                    node_key: None,
                },
                crate::swarm_roster::SwarmMember {
                    recipient: m70_id.recipient().to_bech32(),
                    vault_name: "m2-2".to_owned(),
                    label: "m70".to_owned(),
                    aliases: Vec::new(),
                    hint: None,
                    hw_hint: None,
                    admitted_ms: 1_000,
                    claimed_label: None,
                    claimed_at_ms: None,
                    node_id: None,
                    node_key: None,
                },
            ],
            deny_labels: Vec::new(),
            deny_nodes: Vec::new(),
        };

        overlay_log_membership(&log, None, &mut roster);
        assert!(
            roster.is_label_denied("m2"),
            "removed m2 must be added to deny_labels despite m70's colliding m2.local hostname"
        );
        let m70_row = roster
            .member_by_recipient(&m70_id.recipient().to_bech32())
            .expect("m70 remains in roster");
        assert_eq!(m70_row.label, "m70");
        assert!(
            !roster.is_member_denied(m70_row),
            "surviving m70 must not be denied when colliding m2 is in deny_labels"
        );
        let membership = log.membership();
        let m70_log_member = membership.members().get(&m70_actor).expect("m70 in log");
        assert!(
            !log_entries::member_denied_by_roster(&roster, m70_log_member),
            "member_denied_by_roster must honor m70's active roster row over m2.local hostname"
        );
    }

    #[test]
    fn swarm_roster_and_resolve_local_swarm_label_refuse_failed_pending_or_unpinned_state() {
        let root = aterm_tempfile::tempdir().unwrap();
        let vault_path = root.path().join("alab.vault");
        let admin_paths = Paths::for_home(&root.path().join("admin"));
        let _ = create(
            &admin_paths,
            vault_path.to_str().unwrap(),
            None,
            Some("m22"),
        )
        .unwrap();
        let _ = admin_seed_swarm_roster(&admin_paths, &[]).unwrap();

        assert!(
            swarm_roster_until(
                &admin_paths,
                std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN
            )
            .unwrap()
            .is_some()
        );
        assert_eq!(
            resolve_local_swarm_label(&admin_paths, &["m22.local".to_owned()])
                .unwrap()
                .as_deref(),
            Some("m22")
        );

        let clean_state = load_state(&admin_paths).unwrap();
        let clean_cfg = config(&admin_paths).unwrap().unwrap();

        let mut failed_state = clean_state.clone();
        failed_state.failed = true;
        save_state(&admin_paths, &failed_state).unwrap();
        assert!(
            swarm_roster_until(
                &admin_paths,
                std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN
            )
            .unwrap()
            .is_none(),
            "failed apply state must not expose un-overlaid base_swarm_roster"
        );
        assert_eq!(
            resolve_local_swarm_label(&admin_paths, &["m22.local".to_owned()]).unwrap(),
            None
        );

        let mut pending_state = clean_state.clone();
        pending_state
            .pending
            .insert("other".to_owned(), "00".repeat(32));
        save_state(&admin_paths, &pending_state).unwrap();
        assert!(
            swarm_roster_until(
                &admin_paths,
                std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN
            )
            .unwrap()
            .is_none(),
            "pending apply state must not expose un-overlaid base_swarm_roster"
        );
        assert_eq!(
            resolve_local_swarm_label(&admin_paths, &["m22.local".to_owned()]).unwrap(),
            None
        );

        save_state(&admin_paths, &clean_state).unwrap();
        let mut unpinned_cfg = clean_cfg.clone();
        unpinned_cfg.signer = None;
        save_config(&admin_paths, &unpinned_cfg).unwrap();
        assert!(
            swarm_roster_until(
                &admin_paths,
                std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(
            resolve_local_swarm_label(&admin_paths, &["m22.local".to_owned()]).unwrap(),
            None
        );

        save_config(&admin_paths, &clean_cfg).unwrap();
        assert_eq!(
            resolve_local_swarm_label(&admin_paths, &["m22.local".to_owned()])
                .unwrap()
                .as_deref(),
            Some("m22")
        );
    }

    #[test]
    fn overlay_and_lift_restored_ignore_forged_or_suffixed_claimed_labels() {
        use crate::commons::binding::Request;
        use crate::membership_log::{Body, Entrant, Evidence, Log, Record};

        let mk_entrant = |tag: u8, node_tag: u8, label: &str, name: &str| {
            let id = Identity::from_secret_bytes([tag ^ 0x55; 32]);
            let req = Request::create([2; 16], [3; 32], [tag; 16], [tag; 16], &id, &[tag; 32])
                .expect("request");
            (
                id,
                Entrant {
                    request: req,
                    node_key: [node_tag; 32],
                    name: name.to_owned(),
                    label: label.to_owned(),
                },
            )
        };

        let (m1_id, m1_ent) = mk_entrant(1, 1, "m1", "m1.local");
        let (m2_id, m2_ent) = mk_entrant(2, 2, "m2", "m2.local");
        let (m3_id, m3_ent) = mk_entrant(3, 3, "m3", "m3.local");
        let m1_actor = m1_ent.request.binding().actor();
        let m2_actor = m2_ent.request.binding().actor();
        let m3_actor = m3_ent.request.binding().actor();

        let genesis =
            Record::found(&[99; 32], vec![m1_ent, m2_ent, m3_ent], 1_000).expect("genesis");
        let genesis_id = *genesis.id();
        let mut log = Log::new(genesis).expect("log");

        // Remove m2, which carried a suffixed stale `claimed_label = Some("m1-3fa2")` colliding
        // with surviving `m1`'s canonical label.
        let remove_m2 = log
            .draft(
                &m1_actor,
                2_000,
                Body::Remove {
                    member: m2_actor,
                    cutoff: genesis_id,
                },
            )
            .sign(&[1; 32], [1; 16])
            .expect("sign remove m2");
        log.insert(remove_m2).expect("insert remove m2");

        let mut roster = SwarmRoster {
            members: vec![
                crate::swarm_roster::SwarmMember {
                    recipient: m1_id.recipient().to_bech32(),
                    vault_name: "m1".to_owned(),
                    label: "m1".to_owned(),
                    aliases: Vec::new(),
                    hint: None,
                    hw_hint: None,
                    admitted_ms: 1_000,
                    claimed_label: None,
                    claimed_at_ms: None,
                    node_id: None,
                    node_key: Some(aterm_codec::hex::encode_lower(&[1u8; 32])),
                },
                crate::swarm_roster::SwarmMember {
                    recipient: m2_id.recipient().to_bech32(),
                    vault_name: "m2".to_owned(),
                    label: "m2".to_owned(),
                    aliases: Vec::new(),
                    hint: None,
                    hw_hint: None,
                    admitted_ms: 1_000,
                    claimed_label: Some("m1-3fa2".to_owned()),
                    claimed_at_ms: Some(1_500),
                    node_id: None,
                    node_key: Some(aterm_codec::hex::encode_lower(&[2u8; 32])),
                },
                crate::swarm_roster::SwarmMember {
                    recipient: m3_id.recipient().to_bech32(),
                    vault_name: "m3".to_owned(),
                    label: "m3".to_owned(),
                    aliases: Vec::new(),
                    hint: None,
                    hw_hint: None,
                    admitted_ms: 1_000,
                    claimed_label: None,
                    claimed_at_ms: None,
                    node_id: None,
                    node_key: Some(aterm_codec::hex::encode_lower(&[3u8; 32])),
                },
            ],
            deny_labels: Vec::new(),
            deny_nodes: Vec::new(),
        };

        overlay_log_membership(&log, None, &mut roster);
        assert!(
            roster.is_label_denied("m2"),
            "removed m2 must be added to deny_labels: {:?}",
            roster.deny_labels
        );
        assert!(
            !roster.is_label_denied("m1"),
            "surviving m1 must not be collateral-denied by m2's suffixed claimed_label m1-3fa2: {:?}",
            roster.deny_labels
        );

        // Now remove m3 and restore m3 under a fresh secrets key with the same node_key `[3; 32]`,
        // while a denied row for m9 in `roster.members` carries a forged `claimed_label = Some("m3")`.
        let remove_m3 = log
            .draft(
                &m1_actor,
                3_000,
                Body::Remove {
                    member: m3_actor,
                    cutoff: genesis_id,
                },
            )
            .sign(&[1; 32], [1; 16])
            .expect("sign remove m3");
        log.insert(remove_m3).expect("insert remove m3");

        let (_m3_restored_id, m3_restored_ent) = mk_entrant(30, 3, "m3", "m3.local");
        let admit_m3 = log
            .draft(
                &m1_actor,
                4_000,
                Body::Admit {
                    entrant: m3_restored_ent,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&[1; 32], [1; 16])
            .expect("sign admit m3");
        log.insert(admit_m3).expect("insert admit m3");

        let (m9_id, _) = mk_entrant(9, 9, "m9", "m9.local");
        let m9_nk = aterm_codec::hex::encode_lower(&[9u8; 32]);
        let m3_nk = aterm_codec::hex::encode_lower(&[3u8; 32]);
        let mut restore_roster = SwarmRoster {
            members: vec![crate::swarm_roster::SwarmMember {
                recipient: m9_id.recipient().to_bech32(),
                vault_name: "m9".to_owned(),
                label: "m9".to_owned(),
                aliases: Vec::new(),
                hint: None,
                hw_hint: None,
                admitted_ms: 1_000,
                claimed_label: Some("m3".to_owned()),
                claimed_at_ms: Some(1_500),
                node_id: None,
                node_key: Some(m9_nk.clone()),
            }],
            deny_labels: vec!["m3".to_owned(), "m9".to_owned()],
            deny_nodes: vec![m3_nk.clone(), m9_nk.clone()],
        };
        let _ = lift_restored_from_roster(&log, &mut restore_roster);
        assert!(
            !restore_roster.is_label_denied("m3") && !restore_roster.is_node_denied(&m3_nk),
            "restored m3 must be lifted from deny_labels and deny_nodes"
        );
        assert!(
            restore_roster.is_label_denied("m9") && restore_roster.is_node_denied(&m9_nk),
            "denied m9 with forged claimed_label='m3' must not be lifted when m3 is restored: labels={:?}, nodes={:?}",
            restore_roster.deny_labels,
            restore_roster.deny_nodes
        );

        // A stale label-denied row (`m40`) sharing `node_id` / `node_key` with active `m1`
        // must not push `m1`'s node into `deny_nodes` or wipe `m1` during `overlay_log_membership`.
        let m1_nk = aterm_codec::hex::encode_lower(&[1u8; 32]);
        let mut stale_row_roster = SwarmRoster {
            members: vec![
                crate::swarm_roster::SwarmMember {
                    recipient: "age1m40stale".to_owned(),
                    vault_name: "m40".to_owned(),
                    label: "m40".to_owned(),
                    aliases: Vec::new(),
                    hint: None,
                    hw_hint: None,
                    admitted_ms: 500,
                    claimed_label: None,
                    claimed_at_ms: None,
                    node_id: Some("00000000000000000000000000000001".to_owned()),
                    node_key: Some(m1_nk.clone()),
                },
                crate::swarm_roster::SwarmMember {
                    recipient: m1_id.recipient().to_bech32(),
                    vault_name: "m1".to_owned(),
                    label: "m1".to_owned(),
                    aliases: Vec::new(),
                    hint: None,
                    hw_hint: None,
                    admitted_ms: 1_000,
                    claimed_label: None,
                    claimed_at_ms: None,
                    node_id: Some("00000000000000000000000000000001".to_owned()),
                    node_key: Some(m1_nk.clone()),
                },
            ],
            deny_labels: vec!["m40".to_owned()],
            deny_nodes: Vec::new(),
        };
        overlay_log_membership(&log, None, &mut stale_row_roster);
        assert!(
            !stale_row_roster.is_node_denied(&m1_nk)
                && !stale_row_roster.is_node_denied("00000000000000000000000000000001")
                && !stale_row_roster.is_label_denied("m1"),
            "stale denied row sharing node_id/node_key with active m1 must not poison m1: labels={:?}, nodes={:?}",
            stale_row_roster.deny_labels,
            stale_row_roster.deny_nodes
        );
        assert!(
            stale_row_roster.members.iter().any(|m| m.label == "m1"),
            "active m1 must remain in roster.members: {:?}",
            stale_row_roster.members
        );
    }
}
