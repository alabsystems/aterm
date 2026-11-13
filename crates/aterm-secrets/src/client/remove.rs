// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Any member removes (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.3.7, F16).
//!
//! Where this Mac's vault has a membership log its pin trusts, holding epochs
//! ([`log_entries::trusted_seeded_store`]), and the log seats this Mac's device key, a person's
//! act removes any member with no admin:
//!
//! - [`remove_member`] (by a person's word; [`remove_seat`] by the seat a person's front end
//!   resolved) signs `Remove{member, cutoff}` (the cutoff is the removed Mac's last record
//!   this log holds, else the record that let it in), mints a new epoch for every audience the Mac
//!   was in that this Mac opens (`[everyone]`, Commons, and each host list whose newest epoch
//!   wraps it), wraps each to the members who remain and reseals the current entries under it.
//!   The `[everyone]` reseal carries the revoke cascade (the Mac's roster row and pool slots, a new
//!   fleet code, and on `--stolen` the roster's deny-lists). It returns the entries the Mac could read, for rotation
//!   at the provider, and on `--stolen` the one `NEEDS YOU:` line (C25). It refuses up front,
//!   signing nothing, where its batch and every re-key owed after it (its own, and those earlier
//!   removals made due that some Mac can still sign) would pass the log's keyed budget or its
//!   byte bound ([`RemovalFill`]).
//! - [`tend_epochs`] is the coverage rule on every vault pass, and the only automatic re-key:
//!   where removals on two sides of a split left two newest epochs, or a newest epoch is still
//!   wrapped to a Mac the log removed (a quorum `Remove` voided the remover's rotation, or the
//!   remover's rotation has not arrived), one member mints one higher epoch that covers them
//!   ([`log_entries::coalesce_concurrent_commons_epochs`]), in the turn
//!   [`Log::rekey_dues`] gives it: the remover first, everyone else after a grace. A Commons
//!   exchange signs nothing. A re-key no removal made due waits, unsigned, where it would take
//!   the log's keyed records past [`crate::membership_log::AUTOMATIC_KEYED_STOP`], or into the
//!   room, in records or bytes, that the re-keys a held `Remove` made due still owe
//!   ([`within_automatic_stop`]).
//! - [`removed_notes`] names each Mac the log removed, by whom and when, so a removed Mac that asks
//!   again shows as removed, not new, and a person's "Allow again" restores a plain removal
//!   ([`super::admit`]). A Mac a quorum removed is not restored by one person's Allow.
//! - [`undo_offers`] and [`undo_removals`] are the "Undo m7's removals" row: a Mac m7 removed signs
//!   its share of the quorum `Remove` of m7 at [`Log::undo_cutoff`], which every honest Mac
//!   computes alike, with its seat in the log ([`open_local_seat_signer`]) though it is out.
//!
//! A vault with no log (ALab until F12b) keeps the admin's revoke (`aterm secrets revoke`
//! through `admin_open`): [`removes_by_log`] says which path applies.

use super::log_entries::{self, AuthorCtx, Rotation};
use super::*;
use crate::commons::bootstrap::Device;
use crate::membership_log::{
    Body, EpochAudience, Kind, Log, MemberId, Record, RecordId, RekeyDue, Store, Turn, Undo,
    audience_tag, entries::OpenedAudienceEntries,
};
use std::os::fd::{AsFd, BorrowedFd};
use std::sync::Arc;

/// What [`remove_member`] or [`remove_seat`] did.
#[derive(Debug)]
pub struct MemberRemoval {
    /// The removed Mac's label in the log.
    pub label: String,
    /// The records this Mac signed: the `Remove`, then each new epoch's `Epoch`, `Wrap`s and
    /// `Put`s.
    pub signed: usize,
    /// The audiences it minted a new epoch for: `everyone`, `commons`, or a host list's header.
    pub rotated: Vec<String>,
    /// The entries the removed Mac could read, as this Mac opens them: each to rotate at the
    /// provider where it was lost.
    pub could_read: Vec<String>,
    /// `aterm-fleet-code` was rotated: on every removal where `[everyone]` holds one (a plain one
    /// too, since 0.148, so the removed Mac's copy proves nothing to the fleet or the vault).
    pub fleet_code_rotated: bool,
    /// The one `NEEDS YOU:` line for the provider rotations, on `--stolen` alone (C25).
    pub needs_you: Option<String>,
    /// What the vault's envelope, which 0.145 Macs read, still says, where this Mac opens the
    /// admin locker and so keeps it in step: the removed Mac was one of its admins and keeps the
    /// envelope's signing key ([`vault_admin_named`]), or the mirror failed. Empty on a member's
    /// Mac, which opens no admin locker.
    pub envelope_notes: Vec<String>,
}

/// What [`undo_removals`] did.
#[derive(Debug)]
pub struct UndoShare {
    /// The label of the Mac whose removals the share undoes.
    pub remover: String,
    /// This Mac signed a share now (`false`: it had signed one at this cutoff already, or the
    /// quorum had taken effect).
    pub signed: bool,
    /// Where the quorum stands afterwards.
    pub undo: Undo,
}

/// The "Undo <label>'s removals" row a Mac `remover` removed shows ([`undo_offers`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UndoOffer {
    /// The Mac whose removals the row undoes.
    pub remover: MemberId,
    /// Its label.
    pub label: String,
    /// Its join code, which [`undo_removals`] takes.
    pub code: String,
    /// How many other Macs its removals took out with this one.
    pub others: usize,
    /// This Mac signed its share at the current cutoff already.
    pub signed: bool,
    /// How many more shares the quorum needs.
    pub needed: usize,
}

/// Why the log keeps a Mac out, as a removed Mac's row says it ([`removed_notes`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemovedNote {
    /// Its label in the log.
    pub label: String,
    /// The labels of the Macs whose removals took it out.
    pub by: Vec<String>,
    /// When the earliest of those removals was signed (UTC milliseconds, as its signer stamped).
    pub on_ms: u64,
    /// A quorum `Remove` took it out: one person's Allow does not bring it back.
    pub quorum: bool,
}

impl RemovedNote {
    /// The row's title: "m12 was removed by m7 on Oct 30. Allow again?", or for a quorum's
    /// removal "m12 was removed by a quorum of m7, m9 on Oct 30", which offers no Allow.
    #[must_use]
    pub fn title(&self) -> String {
        let by = self.by.join(", ");
        let on = day(self.on_ms);
        if self.quorum {
            format!("{} was removed by a quorum of {by} on {on}", self.label)
        } else {
            format!("{} was removed by {by} on {on}. Allow again?", self.label)
        }
    }
}

/// What the Mac `label` could read while it was out (F21 rule 8), `names` as `[audience] name`,
/// as a line a person reads: "m40 could read [everyone] deploy-key, [everyone] shared: written
/// while it was out; rotate them where they came from". `None` where it could read nothing new.
fn could_read_line(label: &str, names: &[String]) -> Option<String> {
    (!names.is_empty()).then(|| {
        format!(
            "{label} could read {}: written while it was out; rotate them where they came from",
            names.join(", ")
        )
    })
}

/// Every Mac the log removed, by its age recipient (bech32), by its writer key, and by its fleet
/// node key, so a removed Mac that asks again under a new device key with the same machine key,
/// a new machine key with the same writer key, or a fresh secrets identity with its existing
/// fleet node key, is still named.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemovedNotes {
    /// By age recipient.
    pub by_recipient: BTreeMap<String, RemovedNote>,
    /// By Ed25519 writer key.
    pub by_writer: BTreeMap<[u8; 32], RemovedNote>,
    /// By fleet Ed25519 node key.
    pub by_node: BTreeMap<[u8; 32], RemovedNote>,
}

impl RemovedNotes {
    /// The note for the Mac a request binds, by recipient, else by writer key.
    #[must_use]
    pub fn of(&self, recipient: &Recipient, writer: &[u8; 32]) -> Option<&RemovedNote> {
        self.of_with_node(recipient, writer, &[0; 32])
    }

    /// [`RemovedNotes::of`] with the fleet node key `node` (`[0; 32]`: none) as a third lookup
    /// key.
    #[must_use]
    pub fn of_with_node(
        &self,
        recipient: &Recipient,
        writer: &[u8; 32],
        node: &[u8; 32],
    ) -> Option<&RemovedNote> {
        let candidates = [
            self.by_recipient.get(&recipient.to_bech32()),
            self.by_writer.get(writer),
            (*node != [0; 32]).then(|| self.by_node.get(node)).flatten(),
        ];
        candidates
            .into_iter()
            .flatten()
            .find(|note| note.quorum)
            .or_else(|| candidates.into_iter().flatten().next())
    }
}

/// `ms` (UTC) as a month and day: `Oct 30`.
pub(super) fn day(ms: u64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let days = i64::try_from(ms / 86_400_000).unwrap_or(0) + 719_468;
    let doe = days.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let month = usize::try_from(m - 1).unwrap_or(0).min(11);
    format!("{} {d}", MONTHS[month])
}

/// The trusted log this Mac's vault holds: a genesis its pin trusts, holding epochs. `None` where
/// it holds none (ALab until F12b, or a vault nobody founded a log for).
pub(super) fn trusted_store(paths: &Paths) -> crate::Result<Option<Store>> {
    let Some(config) = config(paths)? else {
        return Ok(None);
    };
    let Some(pin) = config.signer else {
        return Ok(None);
    };
    Ok(log_entries::trusted_seeded_store(paths, &config, &pin, &[])?.map(|(store, _)| store))
}

/// Whether this Mac removes through the membership log ([`remove_member`]): its vault has a log
/// its pin trusts, holding epochs, and the log seats this Mac's device key as a member. Elsewhere
/// an admin's `aterm secrets revoke` is the way (the no-log fallback).
#[must_use]
pub fn removes_by_log(paths: &Paths) -> bool {
    (|| -> crate::Result<bool> {
        let Some(store) = trusted_store(paths)? else {
            return Ok(false);
        };
        let Some(log) = store.log() else {
            return Ok(false);
        };
        Ok(log_entries::open_local_member_signer(paths, log)?.is_some())
    })()
    .unwrap_or(false)
}

/// This Mac's device key where the log gives it a seat (a founder, or an effective `Admit`), even
/// while a removal keeps it out: what a removed Mac signs its share of an Undo with. `None` with
/// no device key or no seat. A Mac a quorum took out for good has none.
///
/// # Errors
/// The device key cannot be read.
pub(super) fn open_local_seat_signer(
    paths: &Paths,
    log: &Log,
) -> crate::Result<Option<(MemberId, Device, [u8; 16])>> {
    if fsx::kind(&paths.dir.join("commons").join("device.age"))? != fsx::Kind::File {
        return Ok(None);
    }
    let Ok(device) = Device::open_existing(paths) else {
        return Ok(None);
    };
    let Body::Genesis { root, .. } = log.genesis().body() else {
        return Ok(None);
    };
    let request = device.request(root.network, root.authority)?;
    let binding = request.binding();
    if !log.has_historical_seat(binding) {
        return Ok(None);
    }
    Ok(Some((binding.actor(), device, *binding.incarnation())))
}

/// The causally earliest non-self-signed `Admit` for `actor` in `log` (preferring non-voided
/// records, at a tie the lowest id).
pub(super) fn earliest_admit_of<'a>(
    log: &'a Log,
    actor: &MemberId,
) -> Option<(&'a RecordId, &'a crate::membership_log::Entrant)> {
    let voided = log.membership_ref().voided();
    let collect = |skip_voided: bool| -> Vec<(&'a RecordId, &'a crate::membership_log::Entrant)> {
        log.records()
            .filter_map(|r| match r.body() {
                Body::Admit { entrant, .. }
                    if entrant.member() == *actor
                        && r.actor() != actor
                        && (!skip_voided || !voided.contains(r.id())) =>
                {
                    Some((r.id(), entrant))
                }
                _ => None,
            })
            .collect()
    };
    let mut admits = collect(true);
    if admits.is_empty() {
        admits = collect(false);
    }
    // The earliest are those with no other of them in their past: a bit test of the log's reach
    // index for each other `Admit`, not a copy of each one's causal past.
    admits
        .iter()
        .filter(|(id, _)| {
            !admits
                .iter()
                .any(|(other, _)| other != id && log.reaches(&[**id], other))
        })
        .min_by_key(|(id, _)| **id)
        .copied()
}

/// What a founder or `Admit` says of the Mac `actor`: its age recipient, label and name, from
/// the genesis, else its earliest `Admit` (at a tie, the lowest id).
fn seat_of(log: &Log, actor: &MemberId) -> Option<(Recipient, String, String)> {
    if let Body::Genesis { founders, .. } = log.genesis().body()
        && let Some(f) = founders.iter().find(|f| f.member() == Some(*actor))
    {
        return Some((f.recipient, f.label.clone(), f.name.clone()));
    }
    if let Some(m) = log.membership_ref().get(actor)
        && let Some(r) = log.get(&m.entry)
        && let Body::Admit { entrant, .. } = r.body()
    {
        return Some((
            entrant.request.binding().recipient(),
            entrant.label.clone(),
            entrant.name.clone(),
        ));
    }
    earliest_admit_of(log, actor).map(|(_, entrant)| {
        (
            entrant.request.binding().recipient(),
            entrant.label.clone(),
            entrant.name.clone(),
        )
    })
}

/// Every non-zero fleet node key bound to `actor` in `log`.
fn seat_nodes_of(
    log: &Log,
    membership: &crate::membership_log::Membership,
    actor: &MemberId,
) -> Vec<[u8; 32]> {
    let mut nodes = BTreeSet::new();
    for (nk, standing) in membership.nodes() {
        if *nk != [0; 32]
            && matches!(standing, crate::membership_log::NodeStanding::Removed(out) if out == actor)
        {
            nodes.insert(*nk);
        }
    }
    let node_bound_to_actor = |nk: [u8; 32]| -> bool {
        if nk == [0; 32] {
            return false;
        }
        match membership.node(&nk) {
            crate::membership_log::NodeStanding::Removed(_) => true,
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
    if let Body::Genesis { founders, .. } = log.genesis().body()
        && let Some(nk) = founders
            .iter()
            .find(|f| f.member() == Some(*actor))
            .and_then(|f| f.node_key)
            .filter(|k| node_bound_to_actor(*k))
    {
        nodes.insert(nk);
    }
    if let Some((_, entrant)) = earliest_admit_of(log, actor)
        && node_bound_to_actor(entrant.node_key)
    {
        nodes.insert(entrant.node_key);
    }
    nodes.into_iter().collect()
}

/// The label of the Mac whose seat `actor` is, as `log` shows it, a Mac now out included; `a Mac`
/// where the log names none (`claude_token`'s signer).
pub(crate) fn seat_label(log: &Log, actor: &MemberId) -> String {
    label_of(log, &log.membership(), actor)
}

/// Every seat `log` has held, a Mac now out included: its actor, the Commons machine its binding
/// names, and its label as the log shows it. What names the writer of a record a removed Mac
/// signed while it was a member.
#[must_use]
pub fn seats_ever(log: &Log) -> Vec<(MemberId, [u8; 16], String)> {
    let membership = log.membership_ref();
    let mut seats: Vec<(MemberId, [u8; 16], String)> = membership
        .members()
        .iter()
        .map(|(actor, member)| (*actor, *member.binding.machine(), member.label.clone()))
        .collect();
    for actor in membership.removed().keys() {
        if let Some((_, entrant)) = earliest_admit_of(log, actor) {
            seats.push((
                *actor,
                *entrant.request.binding().machine(),
                entrant.label.clone(),
            ));
        }
    }
    seats
}

/// The label of the Mac whose age recipient is `recipient`, as `log` shows it: a member, a Mac
/// now out or a founder the genesis lists; `a Mac` where the log names none (`claude_token`'s
/// exposure).
pub(crate) fn recipient_label(log: &Log, recipient: &Recipient) -> String {
    let membership = log.membership();
    if let Some((actor, _)) = membership
        .members()
        .iter()
        .find(|(_, member)| member.binding.recipient() == *recipient)
    {
        return label_of(log, &membership, actor);
    }
    membership
        .removed()
        .keys()
        .find_map(|actor| {
            seat_of(log, actor).and_then(|(seat, label, _)| (seat == *recipient).then_some(label))
        })
        .or_else(|| match log.genesis().body() {
            Body::Genesis { founders, .. } => founders
                .iter()
                .find(|founder| founder.recipient == *recipient)
                .map(|founder| founder.label.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "a Mac".to_owned())
}

/// A Mac's label as this log shows it: its member label, else its seat's.
fn label_of(log: &Log, membership: &crate::membership_log::Membership, actor: &MemberId) -> String {
    membership
        .label(actor)
        .map(str::to_owned)
        .or_else(|| seat_of(log, actor).map(|(_, label, _)| label))
        .unwrap_or_else(|| "a Mac".to_owned())
}

/// The Macs `who` names among `candidates` (actor, recipient, label, name): by label (a bare
/// `m<N>` also names each Mac whose clash suffixed it, `m40-3fa2`), by host name, by join code,
/// or by age recipient. Refuses a word that names none or more than one: 4-hex suffixes can
/// collide, so a label can name two Macs, and a bare `m<N>` that names suffixed Macs and is
/// another Mac's host name names them all. The refusal names each by its join code.
fn resolve(
    who: &str,
    candidates: &[(MemberId, Recipient, String, String)],
) -> crate::Result<(MemberId, Recipient, String)> {
    let word = who.trim();
    let named = matching(who, candidates);
    match named.as_slice() {
        [(actor, recipient, label, _)] => Ok((*actor, *recipient, label.clone())),
        [] => Err(Error::new(format!(
            "no Mac in this Mac's membership log is named {word}"
        ))),
        many => Err(Error::new(format!(
            "{word} names {} Macs ({}): name one by its join code",
            many.len(),
            many.iter()
                .map(|(_, recipient, label, _)| format!("{label} {}", join_code(recipient)))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// Every candidate `who` names, by the rules of [`resolve`].
fn matching<'c>(
    who: &str,
    candidates: &'c [(MemberId, Recipient, String, String)],
) -> Vec<&'c (MemberId, Recipient, String, String)> {
    let word = who.trim();
    let lower = word.to_ascii_lowercase();
    candidates
        .iter()
        .filter(|(_, recipient, label, name)| {
            label.eq_ignore_ascii_case(word)
                || name.eq_ignore_ascii_case(word)
                || join_code(recipient) == lower
                || recipient.to_bech32() == word
                || label
                    .split_once('-')
                    .is_some_and(|(bare, _)| bare.eq_ignore_ascii_case(word))
        })
        .collect()
}

/// The members `who` can name for a removal.
fn member_candidates(log: &Log) -> Vec<(MemberId, Recipient, String, String)> {
    log.membership()
        .members()
        .iter()
        .map(|(actor, m)| {
            (
                *actor,
                m.binding.recipient(),
                m.label.clone(),
                m.name.clone(),
            )
        })
        .collect()
}

/// The target of a removal that `who` names in `log`, which the Mac `me` signs: one member,
/// never `me`. A founder the genesis lists with no binding has no actor until it proves its device
/// key, so no `Remove` can name it, and the vault's envelope still lists it (an admin's mirror
/// wraps the log's epochs to every Mac the envelope lists): the refusal says the admin's revoke,
/// which takes it out of both, removes it.
fn removal_target(
    log: &Log,
    who: &str,
    me: &MemberId,
) -> crate::Result<(MemberId, Recipient, String)> {
    let candidates = member_candidates(log);
    let resolved = resolve(who, &candidates);
    if resolved.is_err() {
        let membership = log.membership();
        let lower = who.trim().to_ascii_lowercase();
        if membership.unproved().iter().any(|f| {
            f.label.eq_ignore_ascii_case(who.trim())
                || f.name.eq_ignore_ascii_case(who.trim())
                || join_code(&f.recipient) == lower
                || f.recipient.to_bech32() == who.trim()
        }) {
            return Err(Error::new(format!(
                "{} is a founder that has proved no device key yet, so no member's removal can \
                 name it: on an admin's Mac, `aterm secrets revoke {}` removes it with the admin \
                 key, from the vault 0.145 Macs read and from the log's keys",
                who.trim(),
                who.trim()
            )));
        }
        let removed = log
            .records()
            .filter_map(|r| match r.body() {
                Body::Admit { entrant, .. } => Some(entrant.member()),
                _ => None,
            })
            .chain(match log.genesis().body() {
                Body::Genesis { founders, .. } => founders
                    .iter()
                    .filter_map(crate::membership_log::Founder::member)
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .filter(|actor| membership.removal(actor).is_some())
            .filter_map(|actor| {
                seat_of(log, &actor).map(|(recipient, label, name)| (actor, recipient, label, name))
            })
            .collect::<Vec<_>>();
        let matched_active = matching(who, &candidates);
        // A Mac's writer successors carry its name (`super::successor::Lineages`), so its name
        // names them too: the word names the Mac, whose removal takes them out with it
        // ([`draft_removal`]). A successor alone is named by its own label or join code.
        let below = super::successor::Lineages::of(log).under(
            &matched_active
                .iter()
                .chain(&matching(who, &removed))
                .map(|(actor, ..)| *actor)
                .collect(),
        );
        let macs: Vec<_> = matched_active
            .iter()
            .copied()
            .filter(|(actor, ..)| !below.contains(actor))
            .collect();
        if macs.len() < matched_active.len()
            && let [(actor, recipient, label, _)] = macs.as_slice()
        {
            if actor == me {
                return Err(Error::new("a Mac does not remove itself"));
            }
            return Ok((*actor, *recipient, label.clone()));
        }
        if let Some((_, first_rec, _, _)) = macs.first()
            && macs.len() > 1
            && macs
                .iter()
                .all(|(actor, rec, _, _)| rec == first_rec && actor != me)
            && let Some(&&(actor, recipient, ref label, _)) =
                macs.iter().max_by_key(|(actor, _, _, _)| {
                    (
                        membership
                            .get(actor)
                            .and_then(|m| log.record_index(&m.entry))
                            .unwrap_or(0),
                        *actor,
                    )
                })
        {
            return Ok((actor, recipient, label.clone()));
        }
        // The removed Mac's writer successors, out with it, carry its name too.
        let matched_removed: Vec<_> = matching(who, &removed)
            .into_iter()
            .filter(|(actor, ..)| !below.contains(actor))
            .collect();
        if matched_active.is_empty()
            && let Some((_, first_rec, label, _)) = matched_removed.first()
            && matched_removed
                .iter()
                .all(|(_, rec, _, _)| rec == first_rec)
        {
            return Err(Error::new(format!("{label} is removed already")));
        }
    }
    let target = resolved?;
    if target.0 == *me {
        return Err(Error::new("a Mac does not remove itself"));
    }
    Ok(target)
}

/// Whether `aterm secrets revoke <who>` removes through this Mac's membership log
/// ([`remove_member`]): [`removes_by_log`] holds and the log names `who` as a member or a Mac
/// it removed (so the member path's own refusals answer). A founder that has proved no device
/// key, or a Mac only the vault's `[machines]` names, is the admin's revoke: the vault's envelope,
/// which 0.145 Macs read, still lists it, and only the admin key takes it out of there.
#[must_use]
pub fn revoke_by_log(paths: &Paths, who: &str) -> bool {
    removes_by_log(paths)
        && (|| -> crate::Result<bool> {
            let Some(store) = trusted_store(paths)? else {
                return Ok(false);
            };
            let Some(log) = store.log() else {
                return Ok(false);
            };
            let mut seated = member_candidates(log);
            for actor in log.membership().removed().keys() {
                if let Some((recipient, label, name)) = seat_of(log, actor) {
                    seated.push((*actor, recipient, label, name));
                }
            }
            Ok(!matching(who, &seated).is_empty())
        })()
        .unwrap_or(false)
}

/// The vault admin `who` names, where this Mac opens the admin locker and `who` names a Mac of
/// the envelope's `[admins]` other than this one. A log `Remove` and the admin's mirror take such
/// a Mac out of the envelope's admins with the envelope's signing key unchanged, and it holds that
/// key, so 0.145 Macs keep taking what it signs; the admin's revoke keeps its guards for it (it
/// rotates the key, or refuses while Commons authority may be pinned to it), so
/// `aterm secrets revoke` takes that path for an admin (F16's review).
#[must_use]
pub fn vault_admin_named(paths: &Paths, who: &str) -> Option<String> {
    let admin = admin_cached(paths).ok()?;
    let name = machine_named_in_vault(paths, &admin, who).ok()?;
    other_vault_admin(paths, &admin.published_doc, name)
}

/// [`vault_admin_named`] of the Mac holding `recipient`: the envelope machine bound to that age
/// recipient, matched exactly, never through a word a machine could also be named.
fn vault_admin_holding(paths: &Paths, recipient: &Recipient) -> Option<String> {
    let admin = admin_cached(paths).ok()?;
    let doc = &admin.published_doc;
    let recipient = recipient.to_bech32();
    let name = doc
        .machines()
        .iter()
        .find(|machine| machine.recipient == recipient)?
        .name
        .clone();
    other_vault_admin(paths, doc, name)
}

/// `name` where `doc`'s `[admins]` lists it and its machine is not this Mac.
fn other_vault_admin(paths: &Paths, doc: &Document, name: String) -> Option<String> {
    let mine = read_identity(paths).ok()??.recipient().to_bech32();
    let is_me = doc
        .machines()
        .iter()
        .any(|machine| machine.name == name && machine.recipient == mine);
    (doc.admins().contains(&name) && !is_me).then_some(name)
}

/// Remove the member `who` names (a label, host name, join code or age recipient), as this Mac,
/// by a person's act (`aterm members remove`, F16): sign its `Remove`, mint a new epoch for every
/// audience it was in that this Mac opens, wrap each to the members who remain, reseal the
/// current entries with the revoke cascade, keep the records, and post them to every server this
/// Mac reaches. The fleet code rotates too; with `stolen` the result carries the `NEEDS YOU:`
/// line (C25). It runs under this Mac's state lock, never inside the `ask` exchange.
///
/// # Errors
/// This Mac's vault has no trusted log or this Mac no secrets identity; the log seats no device
/// key of this Mac as a member, or this Mac cannot open its epochs; `who` names no member, more
/// than one, this Mac itself or a seat holding this Mac's age recipient, a Mac removed already or
/// a founder that has proved no key; the removal would pass the log's keyed budget or byte
/// bound, or strand an audience; or a record fails to sign or keep.
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "GapHeal",
        action = "Remove",
        project = "aterm_secrets::client::remove::tests::gap_heal::gap_heal_transition"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "GapHeal",
        action = "RemoveByB",
        project = "aterm_secrets::client::remove::tests::gap_heal::gap_heal_transition"
    )
)]
pub fn remove_member(paths: &Paths, who: &str, stolen: bool) -> crate::Result<MemberRemoval> {
    remove_person(paths, Target::Word(who), stolen)
}

/// [`remove_member`] of the one seat `seat`, which a person's front end resolved
/// (`aterm fleet remove` and `aterm members remove` of a node id or a registry label, through
/// B2's registry resolver; docs/DECISION-b2-registry-binding-2026-11-06.md): this crate receives
/// the actor and never resolves a label by itself. It is the same person's act as the word's:
/// the trusted-log gate, `--stolen`, the other seats of the Mac's age recipient removed with it,
/// the posts and the admin's mirror, and the note where the Mac is another admin of the envelope
/// 0.145 Macs read, found by its recipient.
///
/// # Errors
/// This Mac's vault has no trusted log or this Mac no secrets identity; the log seats no device
/// key of this Mac as a member, or this Mac cannot open its epochs; `seat` is this Mac's own or
/// holds this Mac's age recipient, a seat removed already or no member's; the removal would pass
/// the log's keyed budget or byte bound, or strand an audience; or a record fails to sign or
/// keep.
pub fn remove_seat(paths: &Paths, seat: MemberId, stolen: bool) -> crate::Result<MemberRemoval> {
    remove_person(paths, Target::Seat(seat), stolen)
}

/// A person's removal of `target`, a [`Target::Word`] or a [`Target::Seat`]: [`remove_member`].
fn remove_person(paths: &Paths, target: Target<'_>, stolen: bool) -> crate::Result<MemberRemoval> {
    if trusted_store(paths)?.is_none() {
        return Err(Error::new(
            "this Mac's vault has no membership log yet: an admin removes a Mac with `aterm secrets revoke`",
        ));
    }
    let identity =
        read_identity(paths)?.ok_or_else(|| Error::new("this Mac has no secrets identity"))?;
    // Read under the draft's observation: a pass that held the state when the person asked
    // delays this with the draft, rather than hiding the admin behind a busy read.
    cut_tables_first(paths, target);
    let mut vault_admin = None;
    let (minted, _, report) = log_entries::author_and_commit_records(paths, |store| {
        vault_admin = match target {
            Target::Word(who) => vault_admin_named(paths, who),
            Target::Seat(seat) | Target::Actor(seat) => store
                .log()
                .and_then(|log| log.membership().get(&seat).map(|m| m.binding.recipient()))
                .and_then(|recipient| vault_admin_holding(paths, &recipient)),
        };
        draft_removal(paths, store, &identity, target, stolen)
    })?;
    let report = report?;
    let mut envelope_notes = Vec::new();
    if !minted.is_empty() {
        let _ = post_records_to_servers(paths);
        if admin_cached(paths).is_ok() {
            // 0.145 Macs read the envelope alone: an admin keeps it in step at once.
            if let Err(error) = log_entries::admin_mirror_log(paths, &[]) {
                envelope_notes.push(format!(
                    "the vault 0.145 Macs read is not in step with the removal yet ({error}): \
                     this Mac's next vault pass mirrors it again"
                ));
            }
        }
    }
    if let Some(name) = vault_admin {
        envelope_notes.push(format!(
            "{name} was an admin of the vault 0.145 Macs read and keeps that vault's signing key: \
             they take what it signs until the key is rotated (F20 retires that vault)"
        ));
    }
    Ok(MemberRemoval {
        signed: minted.len(),
        envelope_notes,
        ..report
    })
}

/// [`remove_member`] of the member `target`, with no `--stolen` and no trusted-log gate, posting
/// to the vault's source alone: [`super::remove_member_in_log`], which tests call by actor.
/// Where another member's removal of `target` is here already, this Mac's share at that
/// removal's cutoff instead. Returns the records it signed.
///
/// # Errors
/// As [`remove_member`], for a member named by its actor; for a share, as
/// `log_entries::draft_retire_in_log`.
pub(super) fn remove_actor(paths: &Paths, target: MemberId) -> crate::Result<Vec<Record>> {
    let identity =
        read_identity(paths)?.ok_or_else(|| Error::new("this Mac has no secrets identity"))?;
    cut_tables_first(paths, Target::Actor(target));
    let (minted, _, report) = log_entries::author_and_commit_records(paths, |store| {
        // Another member's removal of `target` reached this Mac first: sign this Mac's share at
        // that removal's cutoff instead, so the two form a quorum `Remove` (F18's runtime path,
        // a3f925dde3). A person's `aterm members remove` refuses a Mac removed already.
        if store
            .log()
            .is_some_and(|log| log.membership().removal(&target).is_some())
        {
            let shares =
                log_entries::draft_retire_in_log(paths, store, &[target], &BTreeSet::new())?;
            return Ok((shares, Ok(())));
        }
        draft_removal(paths, store, &identity, Target::Actor(target), false)
            .map(|(records, report)| (records, report.map(|_| ())))
    })?;
    report?;
    Ok(minted)
}

/// Which pass of a removal's table cutoffs [`TableCuts`] runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutPass {
    /// Before the `Remove` is drafted: sign each table's cutoff at the removed writer's last
    /// operation held now, and keep that frontier for the repair where a table cannot be cut.
    BeforeRemove,
    /// The remover's repair once the `Remove` stands ([`cut_own_removals`]): sign only the
    /// cutoffs still missing, at the frontier kept before the `Remove` where there is one.
    Repair,
    /// A share of a removal another member made first: sign no cutoff of this Mac's own, only
    /// replace one an earlier attempt of its left, which would otherwise count as its newest.
    Share,
}

/// What signs a removal's table cutoffs (docs/DESIGN-alab-one-mesh-2026-11-05.md §4.10, §11
/// B11): for each lab table this Mac holds, a cutoff naming the removed writer's last operation
/// held here, signed as this Mac's own table operation, returning how many it signed. aterm-db
/// writes table operations and depends on this crate, so the `aterm` executable installs its one
/// entry point here ([`on_table_cuts`]); with none installed (a test of this crate alone) a
/// removal signs no table cutoff.
pub type TableCuts = fn(&Paths, &MemberId, CutPass) -> usize;

static TABLE_CUTS: std::sync::OnceLock<TableCuts> = std::sync::OnceLock::new();

/// Install what signs a removal's table cutoffs. The first call wins.
pub fn on_table_cuts(cut: TableCuts) {
    let _ = TABLE_CUTS.set(cut);
}

/// Sign `target`'s table cutoffs before the `Remove` is drafted, so a crash between the two never
/// leaves a `Remove` with none. A reader counts only each remover's newest cutoff of a writer, and
/// only once its log shows that writer removed, so cutoffs signed before a refused or failed
/// `Remove` change nothing once a later one is signed. Only the first remover cuts: once this
/// Mac's log shows `target` removed, its `Remove` is a share at that removal's cutoff, and a
/// cutoff of its own, read from a replica that may be behind, would cut honest operations the
/// first remover kept; the share only replaces a cutoff an earlier attempt of its left
/// ([`CutPass::Share`]). A table this pass could not cut is one [`cut_own_removals`] signs at the
/// node service's next start.
fn cut_tables_first(paths: &Paths, target: Target<'_>) {
    let Some(cut) = TABLE_CUTS.get() else {
        return;
    };
    let Ok(Some(log)) = trusted_store(paths).map(|s| s.and_then(|s| s.log().cloned())) else {
        return;
    };
    let membership = log.membership();
    let (target, heirs) = match target {
        Target::Actor(actor) => (actor, BTreeSet::new()),
        Target::Word(_) | Target::Seat(_) => {
            let Ok(Some((me, _, _))) = log_entries::open_local_member_signer(paths, &log) else {
                return;
            };
            let Ok((target, recipient, _)) = named_target(&log, target, &me) else {
                return;
            };
            (target, heirs_of(&log, &membership, &target, &recipient))
        }
    };
    let pass = if membership.removal(&target).is_none() {
        CutPass::BeforeRemove
    } else {
        CutPass::Share
    };
    cut(paths, &target, pass);
    // The Mac's writer successors are the writers its tables have now: a person's removal
    // takes their seats out too ([`draft_removal`]), so their tables are cut with them.
    for heir in heirs {
        if membership.contains(&heir) {
            cut(paths, &heir, CutPass::BeforeRemove);
        }
    }
}

/// [`cut_tables_first`] for a vault revocation or retirement, which names its targets by
/// recipient: each current seat bound to one of `recipients`.
pub(super) fn cut_tables_for_recipients(paths: &Paths, recipients: &[Recipient]) {
    if TABLE_CUTS.get().is_none() {
        return;
    }
    let Ok(Some(log)) = trusted_store(paths).map(|s| s.and_then(|s| s.log().cloned())) else {
        return;
    };
    let targets: Vec<MemberId> = log
        .membership()
        .members()
        .iter()
        .filter(|(_, member)| recipients.contains(&member.binding.recipient()))
        .map(|(actor, _)| *actor)
        .collect();
    for target in targets {
        cut_tables_first(paths, Target::Actor(target));
    }
}

/// The repair of [`cut_tables_first`]: sign each table cutoff a removal this Mac made still
/// lacks. Only the remover repairs its own, and only a removal it made first (its `Remove` the
/// first of that member's effective ones in log order), not a share it signed at another's
/// cutoff: cutoffs intersect, so a Mac that is behind must not cut a writer earlier than its
/// remover did. The node service runs it once as it starts.
pub fn cut_own_removals(paths: &Paths) -> usize {
    let Some(cut) = TABLE_CUTS.get() else {
        return 0;
    };
    let Ok(Some(log)) = trusted_store(paths).map(|s| s.and_then(|s| s.log().cloned())) else {
        return 0;
    };
    let Ok(Some((me, _, _))) = log_entries::open_local_member_signer(paths, &log) else {
        return 0;
    };
    let membership = log.membership();
    let mut first = BTreeMap::new();
    for record in log.records() {
        if let Body::Remove { member, .. } = record.body()
            && membership
                .removal(member)
                .is_some_and(|removal| removal.by.contains(record.actor()))
        {
            first.entry(*member).or_insert(*record.actor());
        }
    }
    first
        .into_iter()
        .filter(|(_, by)| *by == me)
        .map(|(member, _)| cut(paths, &member, CutPass::Repair))
        .sum()
}

/// The writer successors a person's removal of the Mac whose seat is `target` (its age recipient
/// `recipient`) takes out with it: every seat below the Mac's own seats, the one named and each
/// other seat of its recipient, in or out, in a succession the log holds
/// (`super::successor::Lineages`). A successor is the same Mac's writer under an age identity of
/// its own, on the same disk: left in, it would be wrapped every epoch minted after the removal.
fn heirs_of(
    log: &Log,
    membership: &crate::membership_log::Membership,
    target: &MemberId,
    recipient: &Recipient,
) -> BTreeSet<MemberId> {
    let mut seats = BTreeSet::from([*target]);
    seats.extend(
        membership
            .members()
            .iter()
            .filter(|(_, member)| member.binding.recipient() == *recipient)
            .map(|(actor, _)| *actor),
    );
    seats.extend(
        membership
            .removed()
            .keys()
            .filter(|actor| seat_of(log, actor).is_some_and(|(seat, _, _)| seat == *recipient))
            .copied(),
    );
    let mut heirs = super::successor::Lineages::of(log).under(&seats);
    heirs.retain(|heir| !seats.contains(heir));
    heirs
}

/// What a draft signed, beside what it reports to the person (its refusal, where it refused).
type Drafted<T> = crate::Result<(Vec<Record>, crate::Result<T>)>;

/// Who a removal names.
#[derive(Clone, Copy)]
enum Target<'a> {
    /// The word a person typed, which [`removal_target`] resolves.
    Word(&'a str),
    /// The seat a person's front end resolved ([`remove_seat`]): a person's act, refused and
    /// swept as the word's is.
    Seat(MemberId),
    /// A member's actor, by [`remove_actor`].
    Actor(MemberId),
}

/// The member `target` names in `log`, as the Mac `me` removes it (its actor, age recipient and
/// label): never `me`, and for a person's removal ([`Target::Word`], [`Target::Seat`]) never
/// another seat of `me`'s age recipient either: a person's removal takes out every seat of the
/// recipient and revokes it, so removing this Mac's twin seat would take this Mac out too. A seat
/// removed already is refused by its label, as the word's [`removal_target`] refuses it.
fn named_target(
    log: &Log,
    target: Target<'_>,
    me: &MemberId,
) -> crate::Result<(MemberId, Recipient, String)> {
    let membership = log.membership();
    let named = match target {
        Target::Word(who) => removal_target(log, who, me)?,
        Target::Seat(seat) | Target::Actor(seat) if seat == *me => {
            return Err(Error::new("a Mac does not remove itself"));
        }
        Target::Seat(seat) | Target::Actor(seat) => match membership.get(&seat) {
            Some(member) => (seat, member.binding.recipient(), member.label.clone()),
            None => {
                return Err(Error::new(match target {
                    Target::Seat(_) if membership.removal(&seat).is_some() => {
                        format!("{} is removed already", label_of(log, &membership, &seat))
                    }
                    Target::Seat(_) => {
                        "that seat is no member of this Mac's membership log".to_owned()
                    }
                    _ => "target member is not active in the membership log".to_owned(),
                }));
            }
        },
    };
    if !matches!(target, Target::Actor(_))
        && membership
            .get(me)
            .is_some_and(|mine| mine.binding.recipient() == named.1)
    {
        return Err(Error::new(format!(
            "{} holds this Mac's own age recipient, and a person's removal takes out every seat \
             of it: a Mac does not remove itself",
            named.2
        )));
    }
    Ok(named)
}

fn draft_removal(
    paths: &Paths,
    store: &Store,
    identity: &Identity,
    target: Target<'_>,
    stolen: bool,
) -> Drafted<MemberRemoval> {
    let refuse = |error: Error| Ok((Vec::new(), Err(error)));
    let Some(mut log) = store.log().cloned() else {
        return refuse(Error::new("this Mac holds no membership log"));
    };
    let Some((actor, device, incarnation)) = log_entries::open_local_member_signer(paths, &log)?
    else {
        return refuse(Error::new(
            "the membership log seats no device key of this Mac as a member",
        ));
    };
    // A person's removal takes the Mac out, every seat of its age recipient; the runtime's
    // removal of an actor takes that one seat.
    let by_person = !matches!(target, Target::Actor(_));
    let named = named_target(&log, target, &actor);
    let (target, recipient, label) = match named {
        Ok(target) => target,
        Err(error) => return refuse(error),
    };
    let Some(opened) = log.open_audience_entries(identity)? else {
        return refuse(Error::new(
            "this Mac cannot open the membership log's epochs",
        ));
    };
    let membership = log.membership();
    // A person's removal also takes out the Mac's writer successors ([`heirs_of`]): `gone` is
    // every age recipient the batch removes, the Mac's and each successor seat's.
    let heirs: Vec<(MemberId, Recipient)> = if by_person {
        let heirs = heirs_of(&log, &membership, &target, &recipient);
        if heirs.contains(&actor) {
            return refuse(Error::new(format!(
                "this Mac's seat is a writer successor of {label}, and a person's removal takes                  a Mac's successors out with it: a Mac does not remove itself"
            )));
        }
        heirs
            .into_iter()
            .filter_map(|heir| {
                membership
                    .get(&heir)
                    .map(|member| (heir, member.binding.recipient()))
            })
            .collect()
    } else {
        Vec::new()
    };
    let gone: BTreeSet<Recipient> = std::iter::once(recipient)
        .chain(heirs.iter().map(|(_, recipient)| *recipient))
        .collect();
    let entry = membership
        .get(&target)
        .map(|m| m.entry)
        .ok_or_else(|| Error::new("that Mac is no member"))?;
    let cutoff = log
        .records()
        .filter(|r| r.kind() != Kind::Genesis && *r.actor() == target)
        .max_by(|a, b| a.sequence().cmp(&b.sequence()).then(b.id().cmp(a.id())))
        .map_or(entry, |r| *r.id());

    // What the Mac could read: `[everyone]`, Commons control, and each host list a newest epoch
    // of which wraps it.
    let mut could_read: Vec<String> = opened
        .by_audience
        .get(&Audience::Everyone)
        .map(|entries| entries.keys().cloned().collect())
        .unwrap_or_default();
    for (audience, max_ids) in &opened.maximal_epochs {
        let EpochAudience::Hosts(hosts) = audience else {
            continue;
        };
        let wrapped = max_ids.iter().any(|id| {
            opened
                .wrapped_recipients
                .get(id)
                .is_some_and(|set| set.contains(&recipient))
        });
        if wrapped && let Some(entries) = opened.by_audience.get(&Audience::Hosts(hosts.clone())) {
            could_read.extend(
                entries
                    .keys()
                    .map(|name| format!("{name} {}", Audience::Hosts(hosts.clone()).header())),
            );
        }
    }
    if opened.commons_control.is_some() {
        could_read.push(crate::commons::ENTRY.to_owned());
    }
    // A host list whose newest epoch wraps the Mac but that this Mac cannot open: its entries are
    // named by the list alone, and one of its own members rotates it ([`tend_epochs`]).
    let unopened: Vec<String> = opened
        .maximal_epochs
        .iter()
        .filter_map(|(audience, max_ids)| match audience {
            EpochAudience::Hosts(hosts)
                if !max_ids
                    .iter()
                    .all(|id| opened.unwrapped_keys.contains_key(id))
                    && max_ids.iter().any(|id| {
                        opened
                            .wrapped_recipients
                            .get(id)
                            .is_some_and(|set| set.contains(&recipient))
                    }) =>
            {
                Some(Audience::Hosts(hosts.clone()).header())
            }
            _ => None,
        })
        .collect();
    could_read.extend(
        unopened
            .iter()
            .map(|header| format!("every entry of {header} (this Mac cannot open it)")),
    );

    let mut everyone = opened
        .by_audience
        .get(&Audience::Everyone)
        .cloned()
        .unwrap_or_default();
    let is_other_active_member = |m: &crate::membership_log::Member| {
        let rec = m.binding.recipient();
        !gone.contains(&rec)
            && !log.recipient_is_removed(&rec)
            && !membership.member_node_removed(m)
    };
    let other_active_labels: BTreeSet<String> = membership
        .members()
        .values()
        .filter(|m| is_other_active_member(m))
        .flat_map(|m| {
            crate::swarm_roster::leading_m_label(&m.label)
                .into_iter()
                .chain(crate::swarm_roster::leading_m_label(&m.name))
                .map(|(_, canon)| canon)
        })
        .chain(
            membership
                .unproved()
                .iter()
                .filter(|f| f.recipient != recipient && !log.recipient_is_removed(&f.recipient))
                .flat_map(|f| {
                    crate::swarm_roster::leading_m_label(&f.label)
                        .into_iter()
                        .chain(crate::swarm_roster::leading_m_label(&f.name))
                        .map(|(_, canon)| canon)
                }),
        )
        .collect();
    let other_active_nodes: BTreeSet<[u8; 32]> = membership
        .members()
        .values()
        .filter(|m| is_other_active_member(m))
        .filter_map(|m| m.node_key.filter(|nk| *nk != [0; 32]))
        .chain(
            membership
                .unproved()
                .iter()
                .filter(|f| f.recipient != recipient && !log.recipient_is_removed(&f.recipient))
                .filter_map(|f| f.node_key.filter(|nk| *nk != [0; 32])),
        )
        .collect();
    let held_by_other_active = |lbl: &str| {
        other_active_labels.contains(lbl)
            || crate::swarm_roster::leading_m_label(lbl)
                .is_some_and(|(_, canon)| other_active_labels.contains(&canon))
    };
    let mut labels = BTreeSet::from([label.clone()]);
    if let Some((_, seat_label, _)) = seat_of(&log, &target)
        && crate::membership_log::is_label(&seat_label)
        && !held_by_other_active(&seat_label)
    {
        labels.insert(seat_label);
    }
    let mut node_keys = BTreeSet::new();
    let mut target_actors = BTreeSet::from([target]);
    if let Some(member) = membership.get(&target)
        && let Some(nk) = member
            .node_key
            .filter(|k| *k != [0; 32] && !other_active_nodes.contains(k))
    {
        node_keys.insert(aterm_codec::hex::encode_lower(&nk));
    }
    for (member_actor, member) in membership
        .members()
        .iter()
        .filter(|(_, m)| gone.contains(&m.binding.recipient()))
    {
        target_actors.insert(*member_actor);
        if crate::membership_log::is_label(&member.label) && !held_by_other_active(&member.label) {
            labels.insert(member.label.clone());
        }
        if let Some(nk) = member
            .node_key
            .filter(|k| *k != [0; 32] && !other_active_nodes.contains(k))
        {
            node_keys.insert(aterm_codec::hex::encode_lower(&nk));
        }
    }
    for removed_actor in membership.removed().keys() {
        if let Some((rec, seat_label, _)) = seat_of(&log, removed_actor)
            && gone.contains(&rec)
        {
            target_actors.insert(*removed_actor);
            if crate::membership_log::is_label(&seat_label) && !held_by_other_active(&seat_label) {
                labels.insert(seat_label);
            }
        }
    }
    for (nk, standing) in membership.nodes() {
        if *nk != [0; 32]
            && !other_active_nodes.contains(nk)
            && matches!(
                standing,
                crate::membership_log::NodeStanding::Member(a)
                    | crate::membership_log::NodeStanding::Retired(a)
                    | crate::membership_log::NodeStanding::Removed(a)
                    if target_actors.contains(a)
            )
        {
            node_keys.insert(aterm_codec::hex::encode_lower(nk));
        }
    }
    labels.retain(|lbl| !held_by_other_active(lbl));
    let cascade = super::retirement::cascade_member_removal(
        &mut everyone,
        &recipient,
        &labels,
        &node_keys,
        stolen,
    )?;
    let needs_you = stolen.then(|| {
        let names: Vec<String> = could_read
            .iter()
            .filter(|name| !name.starts_with("every entry of "))
            .map(|name| name.split(' ').next().unwrap_or_default().to_owned())
            .collect();
        let line = super::retirement::stolen_needs_you_line(&names, &label, &cascade.warm_accounts);
        if unopened.is_empty() {
            line
        } else {
            format!(
                "{line}; and whatever {} holds (gh tokens, release ring, Developer ID, notary, \
                 mailbox), which this Mac cannot read",
                unopened.join(", ")
            )
        }
    });

    // The batch rotates `[everyone]` and Commons where this Mac opens their newest epochs, and
    // each host list `rotate_host_lists` mints (`Rotation { hosts: true }` below). Once the fold's
    // adoption gate holds here, `[everyone]` alone: tables derive their key from it (M6).
    let fold = crate::commons::fold::on(paths);
    let fill = RemovalFill::of(
        &log,
        &opened,
        &actor,
        &gone,
        Some(&everyone),
        |audience, newest, gone| {
            let mints = match audience {
                EpochAudience::Hosts(_) => {
                    log_entries::rotates_host_list(&log, &opened, audience, newest, gone)
                }
                EpochAudience::Commons if fold => false,
                EpochAudience::Everyone | EpochAudience::Commons => {
                    log_entries::opens_newest(&opened, audience)
                }
            };
            if mints { Mints::Surely } else { Mints::No }
        },
    );
    if fill.passes() {
        return refuse(Error::new(fill.refusal()));
    }

    let mut seats_to_remove = vec![(target, cutoff)];
    if by_person {
        for (&seat_actor, seat_member) in membership.members() {
            if seat_actor != target
                && seat_actor != actor
                && gone.contains(&seat_member.binding.recipient())
            {
                let seat_cutoff = log
                    .records()
                    .filter(|r| r.kind() != Kind::Genesis && *r.actor() == seat_actor)
                    .max_by(|a, b| a.sequence().cmp(&b.sequence()).then(b.id().cmp(a.id())))
                    .map_or(seat_member.entry, |r| *r.id());
                seats_to_remove.push((seat_actor, seat_cutoff));
            }
        }
    }
    let denies = log_entries::Denies::read(paths, &log, &opened, None)?;
    let authored_ms = aterm_time::unix_millis().unwrap_or(0);
    let mut minted = Vec::new();
    let rotated = device.with_seed(
        |writer_seed| -> crate::Result<Result<Vec<String>, Vec<EpochAudience>>> {
            for (seat_actor, seat_cutoff) in seats_to_remove {
                let remove = log
                    .draft(
                        &actor,
                        authored_ms,
                        Body::Remove {
                            member: seat_actor,
                            cutoff: seat_cutoff,
                        },
                    )
                    .sign(writer_seed, incarnation)?;
                let _ = log.insert(remove.clone())?;
                minted.push(remove);
            }
            // The guard (one-mesh proofs P2): a removal that leaves a newest `[everyone]` or Commons
            // epoch with no current holder, which this Mac cannot re-key, is refused. A stolen Mac is
            // removed whatever it strands: keeping it in is worse, and `commons restore` re-keys.
            let stranded = log_entries::stranded_audiences(&log, &opened);
            if !stolen && !stranded.is_empty() {
                return Ok(Err(stranded));
            }
            log_entries::rotate_after_removal(
                &mut log,
                &opened,
                &AuthorCtx {
                    actor: &actor,
                    writer_seed,
                    incarnation,
                    authored_ms,
                },
                &BTreeSet::new(),
                &Rotation {
                    everyone: Some(&everyone),
                    hosts: true,
                    fold,
                },
                &denies,
                &mut minted,
            )
            .map(Ok)
        },
    )?;
    let rotated = match rotated {
        Ok(rotated) => rotated,
        Err(stranded) => return refuse(Error::new(strands_refusal(&label, &stranded))),
    };
    Ok((
        minted,
        Ok(MemberRemoval {
            label,
            signed: 0,
            rotated,
            could_read,
            fleet_code_rotated: cascade.fleet_code_rotated,
            needs_you,
            envelope_notes: Vec::new(),
        }),
    ))
}

/// The refusal of a removal that would strand `stranded` (`remove_member`'s guard).
fn strands_refusal(label: &str, stranded: &[EpochAudience]) -> String {
    let names: Vec<&str> = stranded
        .iter()
        .map(|audience| match audience {
            EpochAudience::Everyone => "[everyone]",
            EpochAudience::Commons => "Commons",
            EpochAudience::Hosts(_) => "a host list",
        })
        .collect();
    format!(
        "removing {label} would leave the vault's newest {} key with no current Mac holding it, \
         and this Mac does not hold it to re-key it: remove {label} from a Mac that holds the \
         newest keys, or let the vault pass heal this Mac in first",
        names.join(" and ")
    )
}

/// How a removal's own batch re-keys an audience the removed Mac may be in ([`RemovalFill`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mints {
    /// The batch mints the audience's next epoch: its re-key is in the batch, and what it makes
    /// due there is at worst one coalesce of the heads a concurrent removal leaves.
    Surely,
    /// The batch may mint it (a writer's departure, which mints only for the sections its plan
    /// reaches): its re-key is counted in the batch, and the worst case is still a ranked
    /// holder's rotation and a coalesce.
    Maybe,
    /// The batch mints nothing there: a ranked holder's rotation and a coalesce at worst.
    No,
}

/// What a removal would add to the membership log's keyed records and their bytes, beside what
/// the log holds (one-mesh §8, "The 8,500 stop"): the 8,500 stop never holds a re-key a removal
/// made due, since writers wait on it ([`log_entries::Waiting`]), so a removal reserves that room
/// up front and refuses, signing nothing, where the log could not take it all
/// ([`RemovalFill::passes`]). Every way a person removes a Mac weighs it before its `Remove` is
/// signed: `aterm members remove` ([`draft_removal`]), `aterm secrets revoke`
/// (`log_entries::draft_retire_in_log_with`) and a publish that puts a Mac's key on a `code-only`
/// line (`log_entries`' writer). A seat move (`super::readmit`'s `Replace`) is signed only below
/// its proof-admission stop (`super::readmit::PROVEN_FILL` of the keyed budget, 9,216 records), a
/// quarter of the budget below the hard bound. Every count is a bound of
/// [`log_entries::rekey_bound`] over the log as this Mac holds it when the removal is drafted,
/// never below what is signed.
///
/// One-mesh proofs P6 (`docs/design/one-mesh-proofs/P6-log-capacity.md`): the room reserved is
/// what every held `Remove` still owes, not only this one's (F1), in wire bytes as well as
/// records (F6), and a heal keeps the same room free ([`log_entries::owed_rekeys`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RemovalFill {
    /// The keyed records the log holds.
    keyed: usize,
    /// The keyed records of the removal's own re-keys: the new epoch of every audience its batch
    /// mints or may mint ([`Mints`]), its `Wrap`s and re-`Put`s with the cascade. Its `Remove`s
    /// are weighed records, not keyed ones.
    batch: usize,
    /// The keyed records of the re-keys owed after it at worst, for each audience whose newest
    /// epoch wraps a Mac it removes, or that an earlier removal left owing a re-key some Mac can
    /// sign and nobody has yet (`log_entries::owed_audiences`): a ranked holder's rotation where
    /// the batch does not surely mint there, and one coalesce of the heads a concurrent removal
    /// leaves.
    worst: usize,
    /// The log's keyed budget ([`crate::membership_log::RecordBudgets::keyed`]).
    limit: usize,
    /// The wire bytes of the records the log holds.
    bytes: usize,
    /// The most wire bytes of `batch`'s records.
    batch_bytes: usize,
    /// The most wire bytes of `worst`'s records.
    worst_bytes: usize,
    /// The log's byte bound ([`crate::membership_log::budgets::held_bytes_bound`]).
    byte_limit: usize,
}

#[cfg(test)]
thread_local! {
    /// MUTANT of [`RemovalFill::of`], on for one test thread: the worst case counts only the
    /// audiences whose newest epoch wraps a Mac this removal removes, as before one-mesh proofs
    /// P6 F1, so a re-key an earlier removal made due loses its room.
    pub(super) static FILL_OWN_TARGET_ONLY: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// MUTANT of [`RemovalFill::passes`], on for one test thread: the fill weighs records alone,
    /// as before one-mesh proofs P6 F6, so a re-key's re-`Put`s can pass the byte bound.
    pub(super) static FILL_IGNORES_BYTES: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn fill_mutants() -> (bool, bool) {
    (
        FILL_OWN_TARGET_ONLY.with(std::cell::Cell::get),
        FILL_IGNORES_BYTES.with(std::cell::Cell::get),
    )
}

#[cfg(not(test))]
const fn fill_mutants() -> (bool, bool) {
    (false, false)
}

/// The room check of a removal's fill ([`RemovalFill::passes`]), a heal (`log_entries`'
/// `heal_room`) and the tender's re-keys no removal made due ([`within_automatic_stop`], and its
/// 8,500 stop with no `need`): one-mesh proofs P6's kernel, whose island proves the rule that asks
/// it never wedges. `Log::insert`'s hard bound and a proof admission's line keep their own checks.
pub(super) use aterm_mesh_core::capacity::keeps_room;

/// `count` as a [`keeps_room`] operand.
#[must_use]
pub(super) fn wide(count: usize) -> u64 {
    u64::try_from(count).unwrap_or(u64::MAX)
}

impl RemovalFill {
    /// The fill of `me`'s removal of the Macs whose recipients are `removed`, over `log` and
    /// `opened` as they are before its `Remove`s, with `everyone` the `[everyone]` entries its
    /// batch puts (`None`: the ones `opened` reads), and `mints` how the batch re-keys an
    /// audience, given its newest epochs and every recipient the log and the batch remove.
    pub(super) fn of(
        log: &Log,
        opened: &OpenedAudienceEntries,
        me: &MemberId,
        removed: &BTreeSet<Recipient>,
        everyone: Option<&BTreeMap<String, Entry>>,
        mints: impl Fn(&EpochAudience, &[RecordId], &BTreeSet<Recipient>) -> Mints,
    ) -> Self {
        let flight = log_entries::Flight::of_batch(log, opened, me);
        let mut gone = log.removed_recipients();
        gone.extend(removed.iter().copied());
        // The re-keys earlier removals made due that some Mac can still sign (P6 F1), beside
        // the ones this batch makes due.
        let owed = if fill_mutants().0 {
            BTreeSet::new()
        } else {
            log_entries::owed_audiences(log)
        };
        let mut seats = log.current_recipients(&EpochAudience::Everyone);
        seats.retain(|seat| !removed.contains(seat));
        let no_seats = BTreeSet::new();
        let mut batch = log_entries::RekeyCost::default();
        let mut worst = log_entries::RekeyCost::default();
        for (audience, newest) in &opened.maximal_epochs {
            let mints = mints(audience, newest, &gone);
            let seats = if matches!(audience, EpochAudience::Hosts(_)) {
                &no_seats
            } else {
                &seats
            };
            let next = everyone.filter(|_| *audience == EpochAudience::Everyone);
            let cost = log_entries::rekey_bound(log, opened, &flight, audience, seats, &gone, next);
            if mints != Mints::No {
                batch = batch.plus(cost);
            }
            if log_entries::wraps_removed(opened, newest, removed) || owed.contains(audience) {
                let rekeys = if mints == Mints::Surely { 1 } else { 2 };
                worst = worst.plus(cost.times(rekeys));
            }
        }
        Self {
            keyed: log.counts().keyed,
            batch: batch.keyed,
            worst: worst.keyed,
            limit: log.budgets().keyed,
            bytes: log.held_bytes(),
            batch_bytes: batch.bytes,
            worst_bytes: worst.bytes,
            byte_limit: crate::membership_log::budgets::held_bytes_bound(),
        }
    }

    /// This fill with `wraps` more `Wrap`s in its batch: what a batch signs beside the removal
    /// (a seat move's `Admit` `Wrap`s and the moved seat's `Wrap` in each epoch it mints,
    /// `super::readmit::seat_move_refusal`).
    pub(super) fn with_more(self, wraps: usize) -> Self {
        let wrap = crate::membership_log::Kind::Wrap.most_bytes();
        Self {
            batch: self.batch.saturating_add(wraps),
            batch_bytes: self.batch_bytes.saturating_add(wraps.saturating_mul(wrap)),
            ..self
        }
    }

    /// Whether the log's records, the batch and the worst case together would pass its keyed
    /// budget.
    fn passes_records(self) -> bool {
        !keeps_room(
            wide(self.keyed),
            wide(self.batch),
            wide(self.worst),
            wide(self.limit),
        )
    }

    /// Whether the log's records, the batch and the worst case together would pass its keyed
    /// budget or its byte bound.
    pub(super) fn passes(self) -> bool {
        self.passes_records()
            || (!fill_mutants().1
                && !keeps_room(
                    wide(self.bytes),
                    wide(self.batch_bytes),
                    wide(self.worst_bytes),
                    wide(self.byte_limit),
                ))
    }

    /// The refusal a person reads: the fill of the bound it passes, records first, with no value
    /// of any entry. The worst case is every re-key owed after it, its own and those earlier
    /// removals made due that are not signed yet.
    pub(super) fn refusal(self) -> String {
        let (counts, held, batch, worst, limit) = if self.passes_records() {
            (
                "keyed records",
                self.keyed,
                self.batch,
                self.worst,
                self.limit,
            )
        } else {
            (
                "record bytes",
                self.bytes,
                self.batch_bytes,
                self.worst_bytes,
                self.byte_limit,
            )
        };
        format!(
            "the membership log is full: it holds {held} {counts}, and this removal's {batch} with \
             the at most {worst} of the re-keys owed after it (its own, and those earlier removals \
             made due that are not signed yet) would pass its limit of {limit}, so none of them \
             is signed"
        )
    }
}

/// Every Mac this Mac's trusted log removed, by recipient and writer key ([`RemovedNotes`]): who
/// removed it, when, and whether a quorum did. Empty where this Mac holds no trusted log.
///
/// # Errors
/// The records file cannot be read.
pub fn removed_notes(paths: &Paths) -> crate::Result<RemovedNotes> {
    let Some(store) = trusted_store(paths)? else {
        return Ok(RemovedNotes::default());
    };
    let Some(log) = store.log() else {
        return Ok(RemovedNotes::default());
    };
    Ok(notes_of(log))
}

/// Each removed Mac's [`Log::read_after_cutoff_in`] names over `opened`, as `[audience] name`.
fn reads_after_cutoff(
    log: &Log,
    opened: &crate::membership_log::entries::OpenedAudienceEntries,
) -> BTreeMap<MemberId, Vec<String>> {
    log.membership()
        .removed()
        .keys()
        .map(|actor| {
            let names = log
                .read_after_cutoff_in(opened, actor)
                .into_iter()
                .map(|(audience, name)| format!("{} {name}", audience.header()))
                .collect();
            (*actor, names)
        })
        .collect()
}

pub(super) fn notes_of(log: &Log) -> RemovedNotes {
    let membership = log.membership();
    let is_active_member = |m: &crate::membership_log::Member| {
        !log.recipient_is_removed(&m.binding.recipient()) && !membership.member_node_removed(m)
    };
    let active_recipients: BTreeSet<Recipient> = membership
        .members()
        .values()
        .filter(|m| is_active_member(m))
        .map(|m| m.binding.recipient())
        .collect();
    let active_writers: BTreeSet<[u8; 32]> = membership
        .members()
        .iter()
        .filter(|(_, m)| is_active_member(m))
        .map(|(actor, _)| {
            let mut w = [0u8; 32];
            w.copy_from_slice(&actor[..32]);
            w
        })
        .collect();
    let active_nodes: BTreeSet<[u8; 32]> = membership
        .members()
        .values()
        .filter(|m| is_active_member(m))
        .filter_map(|m| m.node_key.filter(|nk| *nk != [0; 32]))
        .collect();
    let mut staged = Vec::new();
    for (actor, removal) in membership.removed() {
        let Some((recipient, seat_label, _)) = seat_of(log, actor) else {
            continue;
        };
        let mut writer = [0u8; 32];
        writer.copy_from_slice(&actor[..32]);
        let node_keys = seat_nodes_of(log, &membership, actor);
        if !removal.quorum
            && (active_recipients.contains(&recipient)
                || active_writers.contains(&writer)
                || node_keys.iter().any(|nk| active_nodes.contains(nk)))
        {
            continue;
        }
        let on_ms = log
            .records()
            .filter(|r| {
                removal.by.contains(r.actor())
                    && matches!(r.body(), Body::Remove { member, .. } if member == actor)
            })
            .map(Record::authored_ms)
            .min()
            .unwrap_or(0);
        let seat_idx = earliest_admit_of(log, actor)
            .and_then(|(id, _)| log.record_index(id))
            .unwrap_or(0);
        let note = RemovedNote {
            label: seat_label,
            by: removal
                .by
                .iter()
                .map(|by| label_of(log, &membership, by))
                .collect(),
            on_ms,
            quorum: removal.quorum,
        };
        staged.push((
            (removal.quorum, seat_idx, on_ms, *actor),
            recipient,
            writer,
            node_keys,
            note,
        ));
    }
    staged.sort_by_key(|(priority, ..)| *priority);
    let mut notes = RemovedNotes::default();
    for (_, recipient, writer, node_keys, note) in staged {
        notes.by_writer.insert(writer, note.clone());
        for nk in node_keys {
            if !active_nodes.contains(&nk) {
                notes.by_node.insert(nk, note.clone());
            }
        }
        notes.by_recipient.insert(recipient.to_bech32(), note);
    }
    notes
}

/// The "Undo <label>'s removals" rows this Mac shows while a removal keeps it out: one per Mac
/// whose removal of it has an Undo ([`Log::undo`]) that has not taken effect and at whose cutoff
/// this Mac votes. Empty where this Mac holds no trusted log or is in.
///
/// # Errors
/// The records file or this Mac's device key cannot be read.
pub fn undo_offers(paths: &Paths) -> crate::Result<Vec<UndoOffer>> {
    let Some(store) = trusted_store(paths)? else {
        return Ok(Vec::new());
    };
    let Some(log) = store.log() else {
        return Ok(Vec::new());
    };
    let Some((me, _, _)) = open_local_seat_signer(paths, log)? else {
        return Ok(Vec::new());
    };
    let membership = log.membership_ref();
    let Some(removal) = membership.removal(&me) else {
        return Ok(Vec::new());
    };
    let mut offers = Vec::new();
    for remover in &removal.by {
        // A seat moved by its node key to a new vault key (`readmit`): undoing the move would
        // seat this key again beside the new one on one node key, which the log then holds out
        // for both. Which of the two Macs keeps the seat is a person's to sort out, with no
        // alert: this Mac asks to join as any other.
        if super::readmit::seat_moved_by(log, remover, &me) {
            continue;
        }
        let Some(undo) = log.undo(remover) else {
            continue;
        };
        if undo.took_effect || !undo.voters.contains(&me) {
            continue;
        }
        let others = membership
            .removed()
            .iter()
            .filter(|(actor, r)| **actor != me && r.by.contains(remover))
            .count();
        offers.push(UndoOffer {
            remover: *remover,
            label: label_of(log, membership, remover),
            code: seat_of(log, remover)
                .map(|(recipient, _, _)| join_code(&recipient))
                .unwrap_or_default(),
            others,
            signed: undo.signed.contains(&me),
            needed: undo.needed(),
        });
    }
    Ok(offers)
}

/// Sign this Mac's share of the quorum `Remove` of the Mac `who` names, at [`Log::undo_cutoff`]
/// (F16's "Undo <label>'s removals"), with this Mac's seat in the log though a removal keeps it
/// out; keep it and post it to every server this Mac reaches. Once the quorum takes effect here,
/// the epoch tender mints the epoch that leaves the voided remover out ([`tend_epochs`]).
///
/// # Errors
/// This Mac's vault has no trusted log, this Mac has no seat in it, `who` names no Mac or more
/// than one, that Mac has no removal to undo, or this Mac does not vote at its cutoff.
pub fn undo_removals(paths: &Paths, who: &str) -> crate::Result<UndoShare> {
    let Some(held) = trusted_store(paths)? else {
        return Err(Error::new("this Mac's vault has no membership log yet"));
    };
    // The quorum this share joins takes the remover out: each signer cuts its tables first, as
    // every removal path does (one-mesh §4.10, B11). Before the quorum is met each cuts from its
    // own replica, and cutoffs intersect, so one that is behind withholds more of the remover's
    // operations, never fewer.
    if let Some(log) = held.log()
        && let Ok(Some((me, _, _))) = open_local_seat_signer(paths, log)
        && let Ok((remover, _, _)) = undo_target(log, &me, who)
    {
        cut_tables_first(paths, Target::Actor(remover));
    }
    let (minted, _, outcome) =
        log_entries::author_and_commit_records(paths, |store| draft_undo_share(paths, store, who))?;
    let (remover, label) = outcome?;
    if !minted.is_empty() {
        let _ = post_records_to_servers(paths);
    }
    let store = load_records(paths)?;
    let log = store
        .log()
        .ok_or_else(|| Error::new("this Mac holds no membership log"))?;
    let undo = log
        .undo(&remover)
        .ok_or_else(|| Error::new("that Mac has no removal to undo"))?;
    if undo.took_effect {
        let _ = tend_epochs(paths);
    }
    Ok(UndoShare {
        remover: label,
        signed: !minted.is_empty(),
        undo,
    })
}

fn draft_undo_share(paths: &Paths, store: &Store, who: &str) -> Drafted<(MemberId, String)> {
    let refuse = |error: Error| Ok((Vec::new(), Err(error)));
    let Some(mut log) = store.log().cloned() else {
        return refuse(Error::new("this Mac holds no membership log"));
    };
    let Some((me, device, incarnation)) = open_local_seat_signer(paths, &log)? else {
        return refuse(Error::new(
            "the membership log gives this Mac's device key no seat",
        ));
    };
    let (remover, _, label) = match undo_target(&log, &me, who) {
        Ok(found) => found,
        Err(error) => return refuse(error),
    };
    if remover == me {
        return refuse(Error::new("a Mac does not undo its own removals"));
    }
    let Some(undo) = log.undo(&remover) else {
        return refuse(Error::new(format!(
            "{label} has no removal the lab did not carry on from, so there is nothing to undo"
        )));
    };
    if !undo.voters.contains(&me) {
        return refuse(Error::new(format!(
            "this Mac was no member when {label}'s contested removals began, so its share would not count"
        )));
    }
    if undo.took_effect || undo.signed.contains(&me) {
        return Ok((Vec::new(), Ok((remover, label))));
    }
    let authored_ms = aterm_time::unix_millis().unwrap_or(0);
    let share = device.with_seed(|seed| {
        log.draft(
            &me,
            authored_ms,
            Body::Remove {
                member: remover,
                cutoff: undo.cutoff,
            },
        )
        .sign(seed, incarnation)
    })?;
    let _ = log.insert(share.clone())?;
    Ok((vec![share], Ok((remover, label))))
}

/// The Mac whose removals `who` names for an Undo the Mac `me` signs: its actor in hex, else a
/// word [`resolve`] reads among every Mac the log seats, members and removed Macs. One machine
/// key across two incarnations (an old seat removed, a new one in, as Allow again restores a Mac
/// that kept its machine key) names two seats by code, label and name; of a word that names more
/// than one, the one Mac with an Undo at whose cutoff `me` votes is meant.
fn undo_target(
    log: &Log,
    me: &MemberId,
    who: &str,
) -> crate::Result<(MemberId, Recipient, String)> {
    let membership = log.membership();
    let mut seated: Vec<(MemberId, Recipient, String, String)> = member_candidates(log);
    for actor in membership.removed().keys() {
        if let Some((recipient, label, name)) = seat_of(log, actor) {
            seated.push((*actor, recipient, label, name));
        }
    }
    let word = who.trim();
    if let Some((actor, recipient, label, _)) = seated
        .iter()
        .find(|(actor, ..)| aterm_codec::hex::encode_lower(actor) == word)
    {
        return Ok((*actor, *recipient, label.clone()));
    }
    resolve(who, &seated).or_else(|error| {
        let undoable: Vec<(MemberId, Recipient, String, String)> = seated
            .iter()
            .filter(|(actor, recipient, label, name)| {
                resolve(who, &[(*actor, *recipient, label.clone(), name.clone())]).is_ok()
                    && log.undo(actor).is_some_and(|undo| undo.voters.contains(me))
            })
            .cloned()
            .collect();
        match undoable.as_slice() {
            [(actor, recipient, label, _)] => Ok((*actor, *recipient, label.clone())),
            _ => Err(error),
        }
    })
}

/// What the epoch tender remembers between passes ([`TendMark`]): when each audience first became
/// due here; for each newest epoch another Mac minted that this Mac first saw as a prefix of its
/// minter's batch, when it first saw it and when it let the audience go (once the batch settled,
/// or past the settle grace); the same for each newest `[everyone]` epoch that held the other
/// audiences while its batch could still decide a Mac with no seat; and when it first saw each
/// removal a due waits on. A head that arrived settled has no line.
fn epoch_mark_file(paths: &Paths) -> PathBuf {
    paths.dir.join("epoch-tend")
}

/// F21 rule 8's journal record: what each removed Mac could read while it was out, as the last
/// reseal on this Mac found it, one line per Mac (`<label>` then each `[audience] name`,
/// tab-separated), never a value. A fact for later, read on demand by `aterm secrets status`;
/// never an alert.
fn read_after_cutoff_file(paths: &Paths) -> PathBuf {
    paths.dir.join("read-after-cutoff")
}

/// Write [`read_after_cutoff_file`] from `log` as `identity` opens it after a reseal; with
/// nothing to name, remove it.
fn record_read_after_cutoff(paths: &Paths, log: &Log, identity: &Identity) -> crate::Result<()> {
    let Some(opened) = log.open_audience_entries(identity)? else {
        return Ok(());
    };
    let membership = log.membership();
    let mut rows: Vec<String> = reads_after_cutoff(log, &opened)
        .into_iter()
        .filter(|(_, names)| !names.is_empty())
        .map(|(actor, names)| {
            std::iter::once(label_of(log, &membership, &actor))
                .chain(names)
                .map(|field| field.replace(['\t', '\n'], " "))
                .collect::<Vec<_>>()
                .join("\t")
        })
        .collect();
    rows.sort();
    let file = read_after_cutoff_file(paths);
    if rows.is_empty() {
        match std::fs::remove_file(&file) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        }
    } else {
        fsx::write_atomic(&file, format!("{}\n", rows.join("\n")).as_bytes(), 0o600)?;
        Ok(())
    }
}

/// F21 rule 8's journal record on this Mac ([`read_after_cutoff_file`]), as lines a person reads:
/// "m40 could read [everyone] shared: written while it was out; rotate them where they came
/// from". Empty where no reseal here found anything to name.
///
/// # Errors
/// The record exists and cannot be read.
pub fn read_after_cutoff_lines(paths: &Paths) -> crate::Result<Vec<String>> {
    let Some(bytes) = fsx::read_private(&read_after_cutoff_file(paths), 1 << 20)? else {
        return Ok(Vec::new());
    };
    let text = String::from_utf8_lossy(bytes.as_slice()).into_owned();
    Ok(text
        .lines()
        .filter_map(|row| {
            let mut fields = row.split('\t');
            let label = fields.next()?;
            let names: Vec<String> = fields.map(str::to_owned).collect();
            could_read_line(label, &names)
        })
        .collect())
}

/// F21 rule 7's waiting line for `aterm secrets status`: for each uncovered keyed epoch that a
/// current member holds and this Mac cannot open, "3 writes m7 made while m40 was out wait for a
/// Mac that holds their key", one per signer of its dependents (this Mac cannot read their
/// names). Empty where this Mac waits on none.
///
/// # Errors
/// The records file or this Mac's identity cannot be read, or a record addressed to it does not
/// open.
pub fn reseal_waits(paths: &Paths) -> crate::Result<Vec<String>> {
    let Some(store) = trusted_store(paths)? else {
        return Ok(Vec::new());
    };
    let Some(log) = store.log() else {
        return Ok(Vec::new());
    };
    let Some(identity) = read_identity(paths)? else {
        return Ok(Vec::new());
    };
    let Some(opened) = log.open_audience_entries(&identity)? else {
        return Ok(Vec::new());
    };
    let membership = log.membership();
    let mut lines = Vec::new();
    for keyed in log.keyed_epochs() {
        if keyed.covered
            || opened.keyed_keys.contains_key(&keyed.epoch)
            || !keyed.holders.iter().any(|h| membership.contains(h))
        {
            continue;
        }
        let minter = log.get(&keyed.epoch).map_or_else(
            || "a Mac".to_owned(),
            |r| label_of(log, &membership, r.actor()),
        );
        let mut by_signer: BTreeMap<MemberId, usize> = BTreeMap::new();
        for dependent in &keyed.dependents {
            if let Some(record) = log.get(dependent) {
                *by_signer.entry(*record.actor()).or_default() += 1;
            }
        }
        for (signer, n) in by_signer {
            lines.push(format!(
                "{n} {} {} made while {minter} was out wait for a Mac that holds their key",
                if n == 1 { "write" } else { "writes" },
                label_of(log, &membership, &signer),
            ));
        }
    }
    Ok(lines)
}

/// [`tend_epochs_at`] at the wall clock, and what it signed posted to every server this Mac
/// reaches within one exchange ([`super::admit::post_records_to_servers_by`]).
///
/// # Errors
/// As [`tend_epochs_at`].
pub fn tend_epochs(paths: &Paths) -> crate::Result<Vec<Record>> {
    let now_ms = aterm_time::unix_millis().unwrap_or(0);
    let signed = tend_epochs_at(paths, now_ms)?;
    if !signed.is_empty() {
        let _ = super::admit::post_records_to_servers_by(paths, super::fan_out::one_exchange());
    }
    Ok(signed)
}

/// One vault pass's re-keys at `now_ms`: the ranked wrap-gap closer
/// ([`log_entries::close_wrap_gaps`]), then the ranked tender ([`tend_epochs_at`]), then the
/// `[everyone]` writes that waited for a removal's new key ([`log_entries::sign_waiting_writes`],
/// one-mesh M4), each whether or not one before it fails. Every log member's subscriber runs it
/// after each records pull, whatever its vault's source ([`vault_pass`]): an exchange signs
/// nothing, so outside a person's act this is where a wrap gap closes, a due re-key is minted and
/// a waiting write is signed. Returns what they signed, already in this Mac's records, nothing
/// posted.
///
/// # Errors
/// As [`log_entries::close_wrap_gaps`], [`tend_epochs_at`] and
/// [`log_entries::sign_waiting_writes`]; of their errors the first, once the waiting writes and the
/// tender ran.
pub fn vault_pass_at(paths: &Paths, now_ms: u64) -> crate::Result<Vec<Record>> {
    let closed = log_entries::close_wrap_gaps(paths, now_ms);
    let tended = tend_epochs_at(paths, now_ms);
    let waited = log_entries::sign_waiting_writes(paths, now_ms);
    let mut signed = closed?;
    signed.extend(tended?);
    signed.extend(waited?);
    Ok(signed)
}

/// [`vault_pass_at`] at the wall clock, what it signed posted to every server this Mac reaches
/// by `posts_by` (one exchange from when it posts where none is given,
/// [`super::admit::post_records_to_servers_by`]): a deadline the subscriber pass's other posts
/// share. Best effort: a pass that fails here tries again on the next.
pub(super) fn vault_pass(paths: &Paths, posts_by: Option<Instant>) {
    let now_ms = aterm_time::unix_millis().unwrap_or(0);
    if vault_pass_at(paths, now_ms).is_ok_and(|signed| !signed.is_empty()) {
        let by = posts_by.unwrap_or_else(super::fan_out::one_exchange);
        let _ = super::admit::post_records_to_servers_by(paths, by);
    }
}

/// What the node service's vault pass keeps to (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.13,
/// §11 B10). [`NodePassPace::LIVE`] is the node service's.
#[derive(Clone, Copy, Debug)]
pub struct NodePassPace {
    /// A pass runs at least this often, a new record or not.
    pub every: Duration,
    /// A pass that was busy or failed runs again after this.
    pub again: Duration,
    /// The least time between two looks. A loop nothing rings ([`NodePassHost::rung`] `None`)
    /// looks this often: at its switches, the records file and its stop.
    pub look: Duration,
    /// How long one pass waits out a writer of the vault's state before it says busy.
    pub lock_wait: Duration,
}

impl NodePassPace {
    /// The node service's pace: every 60 seconds, its looks at least a second apart.
    pub const LIVE: Self = Self {
        every: Duration::from_secs(60),
        again: Duration::from_secs(5),
        look: Duration::from_secs(1),
        lock_wait: Duration::from_secs(10),
    };
}

/// How one node pass ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodePassSaid {
    /// It looked at everything; what it signed and applied is beside this.
    Done,
    /// A writer still held the vault's state when its wait ended: it read nothing, which says
    /// nothing about gaps or files.
    Busy,
    /// A step failed; another pass tries again.
    Failed,
}

impl NodePassSaid {
    const fn word(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Busy => "busy",
            Self::Failed => "failed",
        }
    }

    fn of_word(word: &str) -> Option<Self> {
        match word {
            "done" => Some(Self::Done),
            "busy" => Some(Self::Busy),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// What one node pass did, kept for `aterm secrets status` ([`node_pass_receipt`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodePassReceipt {
    /// When it ran, by the pass's clock.
    pub ran_ms: u64,
    /// How it ended.
    pub said: NodePassSaid,
    /// How many records it signed.
    pub signed: usize,
    /// The version it wrote to this Mac's files, where it wrote one.
    pub applied: Option<u64>,
}

impl NodePassReceipt {
    fn encode(&self) -> String {
        let applied = self
            .applied
            .map_or_else(|| "-".to_owned(), |seq| seq.to_string());
        format!(
            "1 {} {} {} {applied}\n",
            self.ran_ms,
            self.said.word(),
            self.signed
        )
    }

    fn parse(text: &str) -> Option<Self> {
        let mut words = text.split_ascii_whitespace();
        if words.next()? != "1" {
            return None;
        }
        let ran_ms = words.next()?.parse().ok()?;
        let said = NodePassSaid::of_word(words.next()?)?;
        let signed = words.next()?.parse().ok()?;
        let applied = match words.next()? {
            "-" => None,
            seq => Some(seq.parse().ok()?),
        };
        words.next().is_none().then_some(Self {
            ran_ms,
            said,
            signed,
            applied,
        })
    }
}

/// The node service's newest pass on this Mac: `None` where none ran, or its file is unreadable.
#[must_use]
pub fn node_pass_receipt(paths: &Paths) -> Option<NodePassReceipt> {
    let bytes = fsx::read_private(&node_pass_file(paths), MAX_NODE_PASS_FILE)
        .ok()
        .flatten()?;
    NodePassReceipt::parse(std::str::from_utf8(bytes.as_slice()).ok()?)
}

const MAX_NODE_PASS_FILE: usize = 256;

fn node_pass_file(paths: &Paths) -> PathBuf {
    paths.dir.join("node-pass")
}

/// One Mac's node passes: what its repairs held across them ([`repair_due`]), so a file a program
/// keeps rewriting is put back once, as the subscriber's pass does.
#[derive(Default)]
pub struct NodePass {
    repaired: Repaired,
    /// Something it signed has reached no server yet.
    unposted: bool,
}

impl NodePass {
    /// One pass at `now_ms`: the heal, the re-key tender and the waiting writes
    /// ([`vault_pass_at`]'s steps), what they signed posted once `may_post` lets this Mac use the
    /// network (a post it could not make is owed to a later pass), then the materializer from what
    /// this Mac holds, with no network.
    ///
    /// Each wait for the vault's locks ends within `lock_wait`, and a writer still there then makes
    /// the pass [`NodePassSaid::Busy`], never a pass that found nothing. `None` where this Mac has
    /// no vault: nothing is created for one.
    pub fn once(
        &mut self,
        paths: &Paths,
        now_ms: u64,
        lock_wait: Duration,
        may_post: &dyn Fn() -> bool,
    ) -> Option<(NodePassReceipt, Option<Error>)> {
        match config(paths) {
            Ok(None) => return None,
            Ok(Some(_)) => {}
            Err(error) => {
                let receipt = node_receipt(now_ms, NodePassSaid::Failed, 0, None);
                return Some((receipt, Some(error)));
            }
        }
        let (signed, healing) = vault_pass_until(paths, now_ms, Instant::now() + lock_wait);
        self.unposted |= !signed.is_empty();
        if self.unposted && may_post() {
            let by = super::fan_out::one_exchange();
            self.unposted = super::admit::post_records_to_servers_by(paths, by).is_err();
        }
        // A writer that outlasted the wait holds the files too: their turn comes with the next pass.
        let busy = healing.as_ref().map(said_of) == Some(NodePassSaid::Busy);
        let (applied, applying) = if busy {
            (None, None)
        } else {
            match self.materialize(paths, Instant::now() + lock_wait) {
                Ok(applied) => (applied, None),
                Err(error) => (None, Some(error)),
            }
        };
        let first = healing.or(applying);
        let said = first.as_ref().map_or(NodePassSaid::Done, said_of);
        Some((node_receipt(now_ms, said, signed.len(), applied), first))
    }

    /// Write this Mac's files from the records and the envelope it holds, where its log moved, a
    /// repair is due ([`repair_due`]), or an apply failed or was cut short: the local half of the
    /// subscriber's sync.
    fn materialize(&mut self, paths: &Paths, deadline: Instant) -> crate::Result<Option<u64>> {
        let config = configured(paths)?;
        let before = load_state(paths)?;
        let kept = self.repaired.clone();
        let unfinished =
            before.membership == Stored::In && (before.failed || !before.pending.is_empty());
        let repair = !unfinished && self.repair_due(paths, &before);
        let moved = log_entries::current_records_digest(paths, &config, &before)
            .is_some_and(|digest| Some(digest) != before.records_digest);
        if !unfinished && !repair && !moved {
            return Ok(None);
        }
        let outcome = apply_until(
            paths,
            config,
            before,
            ApplyInput::Cached,
            None,
            Some(deadline),
            repair,
        );
        match outcome {
            Ok(SyncOutcome::Applied { seq, report }) => {
                if apply_failed(&report) {
                    self.repaired = kept;
                    return Err(Error::new("a file of this version could not be written"));
                }
                Ok(Some(seq))
            }
            Ok(_) => Ok(None),
            Err(error) => {
                self.repaired = kept;
                Err(error)
            }
        }
    }

    /// [`repair_due`]. A Mac that holds its vault as a log alone keeps no envelope for that rule's
    /// cache check to read, which would call every pass a repair, so its rule is the rest of it:
    /// once per version and set of files not as written.
    fn repair_due(&mut self, paths: &Paths, state: &State) -> bool {
        if state.digest.is_some() {
            return repair_due(paths, &mut self.repaired);
        }
        let Some(source) = state.source_identity else {
            return false;
        };
        if state.membership != Stored::In {
            return false;
        }
        let version = (source, state.seq);
        let changed = unsettled(paths, state);
        let due = (!changed.is_empty() || state.own.is_none())
            && self
                .repaired
                .as_ref()
                .is_none_or(|(last, files)| *last != version || *files != changed);
        if due {
            self.repaired = Some((version, changed));
        }
        due
    }
}

/// [`vault_pass_at`]'s three steps at `now_ms`, each whether or not one before it fails, every wait
/// for the vault's locks ending at `deadline`: what they signed, and the first thing that kept one
/// from finishing ([`crate::ErrorKind::VaultBusy`] for a writer still there at `deadline`).
fn vault_pass_until(paths: &Paths, now_ms: u64, deadline: Instant) -> (Vec<Record>, Option<Error>) {
    let mut signed = Vec::new();
    let mut first = None;
    for step in [
        log_entries::close_wrap_gaps_until(paths, now_ms, deadline),
        tend_epochs_until(paths, now_ms, deadline),
        log_entries::sign_waiting_writes_by(paths, now_ms, Some(deadline)),
    ] {
        match step {
            Ok(Some(records)) => signed.extend(records),
            Ok(None) => {
                first.get_or_insert_with(|| {
                    Error::of_kind(
                        crate::ErrorKind::VaultBusy,
                        "a vault writer still holds this Mac's state",
                    )
                });
            }
            Err(error) => {
                first.get_or_insert(error);
            }
        }
    }
    (signed, first)
}

const fn node_receipt(
    ran_ms: u64,
    said: NodePassSaid,
    signed: usize,
    applied: Option<u64>,
) -> NodePassReceipt {
    NodePassReceipt {
        ran_ms,
        said,
        signed,
        applied,
    }
}

fn said_of(error: &Error) -> NodePassSaid {
    match error.kind() {
        crate::ErrorKind::VaultBusy | crate::ErrorKind::TimedOut => NodePassSaid::Busy,
        _ => NodePassSaid::Failed,
    }
}

/// What tells one records file's contents from another's: every writer renames a new file over it
/// or appends to it ([`fsx::FileStamp`]). `None` with no readable file.
fn records_stamp(paths: &Paths) -> Option<fsx::FileStamp> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::metadata(paths.records_data_file().ok()?).ok()?;
    let ns = |secs: i64, nsec: i64| i128::from(secs) * 1_000_000_000 + i128::from(nsec);
    Some(fsx::FileStamp {
        dev: meta.dev(),
        ino: meta.ino(),
        len: meta.len(),
        mtime_ns: ns(meta.mtime(), meta.mtime_nsec()),
        ctime_ns: ns(meta.ctime(), meta.ctime_nsec()),
    })
}

/// When the next node pass is due: at once for a records file not seen before (a record arrived,
/// by whichever path), otherwise when its time comes, or at once where the clock was set back.
#[derive(Debug, Default)]
struct NodePassDue {
    stamp: Option<fsx::FileStamp>,
    ran_ms: Option<u64>,
    wait_ms: u64,
}

impl NodePassDue {
    fn due(&self, stamp: Option<fsx::FileStamp>, now_ms: u64) -> bool {
        let Some(ran_ms) = self.ran_ms else {
            return true;
        };
        stamp != self.stamp || now_ms < ran_ms || now_ms - ran_ms >= self.wait_ms
    }

    /// A pass that began with the file at `stamp` ran at `now_ms`, and the next waits `wait`. A
    /// record that came during the pass differs from `stamp`, so it is due at the next look.
    fn ran(&mut self, stamp: Option<fsx::FileStamp>, now_ms: u64, wait: Duration) {
        self.stamp = stamp;
        self.ran_ms = Some(now_ms);
        self.wait_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX);
    }

    /// When time alone next makes a pass due, the clock having said `now_ms` at `read`: at once
    /// before the first pass or where the clock was set back. Counted from `read`, so however
    /// long the look that read it took, its pass and the converge after it, the next pass comes
    /// its wait after the pass looked. A new records file is heard, not timed. `None` past what
    /// an [`Instant`] holds.
    fn next_at(&self, now_ms: u64, read: Instant) -> Option<Instant> {
        let left = match self.ran_ms {
            Some(ran_ms) if now_ms >= ran_ms => {
                ran_ms.saturating_add(self.wait_ms).saturating_sub(now_ms)
            }
            _ => 0,
        };
        read.checked_add(Duration::from_millis(left))
    }
}

/// What a node service gives [`run_node_pass`].
pub struct NodePassHost<'a> {
    /// This Mac's vault.
    pub paths: &'a Paths,
    /// Ends the loop, and wakes it from its wait between looks.
    pub stop: &'a aterm_uds::stop::Stop,
    /// Whether the node's switches hold sharing off now (`[swarm] enabled = false`, this Mac left
    /// the fleet): no pass runs while one does.
    pub held: &'a dyn Fn() -> bool,
    /// Whether this Mac's LAN broker answers now, so the pass may post on the LAN.
    pub may_post: &'a dyn Fn() -> bool,
    /// Routes whose changes request a new shared refresh boundary.
    pub routes: &'a dyn Fn() -> crate::converge::Routes,
    /// Make this runtime's local observer after its sharing and subscriber-Off gates admit work.
    pub make_inputs: &'a dyn Fn() -> super::ConvergenceInputs,
    /// Start the converge pass's retained request (`crate::converge::start_requested`).
    pub start_converge:
        &'a dyn Fn(crate::converge::RepositoryRequest) -> Option<aterm_uds::spawnfd::Child>,
    /// The clock the pass and its timer read.
    pub now_ms: &'a dyn Fn() -> u64,
    /// Where each change in how passes end is noted.
    pub log: &'a dyn Fn(&str),
    /// Its pace.
    pub pace: NodePassPace,
    /// Rung when an input the loop reads changes, by whoever hears it: the host's watch over what
    /// `held` reads, the observer `make_inputs` makes (hand it this bell), a test that moves
    /// `now_ms`. The loop rings it too, from its own watch over the records file and the
    /// subscriber's off switch. `None`: nothing rings, and the loop looks every
    /// [`NodePassPace::look`], as one whose inputs no watch hears must.
    pub rung: Option<Arc<aterm_uds::ready::Doorbell>>,
    /// Whether `may_post` reads this thread's LAN broker ([`aterm_uds::lan::dialled_socket`]), so
    /// a loop [`Self::rung`] rings hears it arrive (macOS; elsewhere nothing is heard). A host
    /// whose `may_post` reads a stand-in hears no broker: a test's, which a real one's arrival
    /// would only wake for nothing.
    pub broker: bool,
}

/// The vault pass of a node service (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.13): one pass on
/// every new log record, by whichever path it came (the relay's ingest, a pull, another process's
/// write, this pass's own), and one every [`NodePassPace::every`]. It runs until `stop`, each
/// pass's end kept where `aterm secrets status` reads it. A person's `aterm secrets agent off`
/// holds it too ([`crate::service::subscriber_turned_off`]), as it holds all automatic vault work.
///
/// Between looks it waits on what owes the next one ([`NodeHeard`]): a ring of
/// [`NodePassHost::rung`] (a new records file, a switch, an input the observer reads), its
/// converge runner's exit, a change of network, the LAN broker's arrival, or the first deadline
/// it keeps: the next timed pass, the runner's retry or recheck, the LAN's retry of a start it
/// held, and the observer's own next look. Looks stay [`NodePassPace::look`] apart. While held,
/// what lifts the hold rings, and it looks again each [`NodePassPace::every`] even so: a watch
/// tells a change against the last state it told, so a switch flipped and flipped back inside
/// its settle, read midway by a look, is told by none. With no bell it looks every
/// [`NodePassPace::look`].
pub fn run_node_pass(host: &NodePassHost<'_>) {
    run_node_pass_hearing(host, super::member_routes::NativeChanges::new, |_| {});
}

/// [`run_node_pass`], hearing changes of network through what `changes` makes once a bell rings
/// the loop, and telling `told` each key its own watch tells ([`RECORDS_WATCH`], …) just before
/// that tell rings it.
fn run_node_pass_hearing<C: super::member_routes::Changes>(
    host: &NodePassHost<'_>,
    changes: impl FnOnce() -> C,
    told: impl Fn(u64) + Send + 'static,
) {
    // A removal's table cutoffs come before its `Remove`; one a crash or a busy table left
    // unsigned is signed as the node service starts.
    cut_own_removals(host.paths);
    let mut pass = NodePass::default();
    let mut due = NodePassDue::default();
    let mut last_said = None;
    let mut unfinished = 0_u32;
    let mut converge = NodeConverge::default();
    let mut inputs = None;
    let mut heard = host
        .rung
        .as_ref()
        .and_then(|rung| NodeHeard::start(host, rung, changes(), told));
    while !host.stop.is_set() {
        let looked = Instant::now();
        // What rang before this look, this look reads; what rings during it owes the next.
        if let Some(heard) = &heard {
            heard.rung.clear();
        }
        let open = !(host.held)() && !crate::service::subscriber_turned_off(&host.paths.home);
        let next = if open {
            let inputs = inputs.get_or_insert_with(host.make_inputs);
            let stamp = records_stamp(host.paths);
            let now_ms = (host.now_ms)();
            let read = Instant::now();
            if due.due(stamp, now_ms) {
                let ended = pass.once(host.paths, now_ms, host.pace.lock_wait, host.may_post);
                let done = !matches!(&ended, Some((r, _)) if r.said != NodePassSaid::Done);
                unfinished = if done {
                    0
                } else {
                    unfinished.saturating_add(1)
                };
                // The next pass counts from when this one looked, not from when it ended: a clock
                // that moved during a pass would otherwise skip the time that pass never saw.
                due.ran(stamp, now_ms, node_pass_wait(&host.pace, unfinished));
                if let Some((receipt, error)) = ended {
                    if receipt.applied.is_some() {
                        converge
                            .due
                            .request(crate::converge::RepositoryRequest::Repair);
                    }
                    keep_node_pass_end(host, &receipt, error.as_ref(), &mut last_said);
                }
            }
            converge.tend(host, inputs);
            [
                due.next_at(now_ms, read),
                converge.next_at(),
                inputs.next_look(),
            ]
            .into_iter()
            .flatten()
            .min()
        } else {
            converge.due.paused();
            Instant::now().checked_add(host.pace.every)
        };
        match &mut heard {
            // A held loop reaps no runner, so its exit, ready until reaped, is not waited on.
            Some(heard) => heard.wait(host, next, converge.exit().filter(|_| open), looked),
            None => {
                host.stop.sleep(host.pace.look);
            }
        }
    }
}

/// [`NodeHeard`]'s keys in its watch: the records ([`Records`]: the data file, or the entry while
/// its layout names none), the subscriber's off switch, and, while the layout names no data file,
/// its readiness pin and its preparation intent.
const RECORDS_WATCH: u64 = 0;
const OFF_WATCH: u64 = 1;
const PIN_WATCH: u64 = 2;
const INTENT_WATCH: u64 = 3;

/// What a node's loop watches of its records.
#[derive(Debug, PartialEq, Eq)]
enum Records {
    /// The data file the layout names.
    Data(std::path::PathBuf),
    /// The entry, while its layout names no data file this build can read (a transition another
    /// writer has yet to finish, a layout this build refuses): the entry, its pin and its intent,
    /// what the selection is judged by, so the writer that finishes it is heard
    /// ([`crate::records_storage::data_path`]). The next look names the data file again.
    Unnamed(std::path::PathBuf),
}

/// What wakes a node's loop between looks where a bell rings it ([`NodePassHost::rung`]).
struct NodeHeard<C> {
    rung: Arc<aterm_uds::ready::Doorbell>,
    /// The loop's own watch over what it reads: the records and the subscriber's off switch.
    watch: aterm_uds::file_watch::FileWatch,
    /// The records watched now: the layout names them, and can name others.
    records: Option<Records>,
    changes: C,
    /// The LAN broker's arrival, where [`NodePassHost::broker`] says `may_post` reads it.
    #[cfg(target_os = "macos")]
    broker: Option<aterm_uds::lan::Arrivals>,
    #[cfg(target_os = "macos")]
    broker_error: Option<String>,
}

impl<C: super::member_routes::Changes> NodeHeard<C> {
    /// `None` where the watch cannot start; the loop then looks at its pace. It watches before
    /// the first look reads, so each file's first look, which always tells, comes once, together.
    fn start(
        host: &NodePassHost<'_>,
        rung: &Arc<aterm_uds::ready::Doorbell>,
        changes: C,
        told: impl Fn(u64) + Send + 'static,
    ) -> Option<Self> {
        let paths = host.paths;
        let ringing = Arc::clone(rung);
        let watch = aterm_uds::file_watch::FileWatch::platform(
            "node-pass",
            Box::new(move |key| {
                told(key);
                ringing.ring();
            }),
        )
        .ok()?;
        watch.watch(OFF_WATCH, crate::service::subscriber_off_path(&paths.home));
        let mut heard = Self {
            rung: Arc::clone(rung),
            watch,
            records: None,
            changes,
            #[cfg(target_os = "macos")]
            broker: host.broker.then(aterm_uds::lan::Arrivals::new),
            #[cfg(target_os = "macos")]
            broker_error: None,
        };
        heard.arm(paths);
        Some(heard)
    }

    /// Watch the records the layout names now ([`Records`]), and no others.
    fn arm(&mut self, paths: &Paths) {
        let records = match paths.records_data_file() {
            Ok(data) => Records::Data(data),
            Err(_) => Records::Unnamed(paths.records_file()),
        };
        if self.records.as_ref() == Some(&records) {
            return;
        }
        match &records {
            Records::Data(data) => {
                self.watch.watch(RECORDS_WATCH, data.clone());
                self.watch.unwatch(PIN_WATCH);
                self.watch.unwatch(INTENT_WATCH);
            }
            Records::Unnamed(entry) => {
                self.watch.watch(RECORDS_WATCH, entry.clone());
                // Each refuses only an entry with no valid name, which `Paths` never makes.
                for (key, side) in [
                    (PIN_WATCH, crate::records_storage::pin_path(entry)),
                    (INTENT_WATCH, crate::records_storage::intent_path(entry)),
                ] {
                    match side {
                        Ok(side) => self.watch.watch(key, side),
                        Err(_) => self.watch.unwatch(key),
                    }
                }
            }
        }
        self.records = Some(records);
    }

    /// Whether a broker answered at a socket made since the last poll: one the LAN may now be
    /// used through. A watch that fails is said once and heard no more until it works again.
    #[cfg(target_os = "macos")]
    fn broker_arrived(&mut self, log: &dyn Fn(&str)) -> bool {
        let Some(broker) = &mut self.broker else {
            return false;
        };
        match broker.poll() {
            Ok(arrived) => {
                self.broker_error = None;
                arrived
            }
            Err(error) => {
                let why = error.to_string();
                if self.broker_error.as_ref() != Some(&why) {
                    log(&format!("cannot watch the LAN broker's arrival: {why}"));
                    self.broker_error = Some(why);
                }
                false
            }
        }
    }

    /// Wait until the next look is owed: a ring, the runner's `exit`, a change of network, the
    /// broker's arrival, or `deadline` (`None`: none); then until [`NodePassPace::look`] after
    /// the last look (`looked`). The stop ends either wait. Whatever else ends one (a message that
    /// names no change, a broker generation that answers nothing new, the broker's retry, which
    /// dials again) waits again.
    fn wait(
        &mut self,
        host: &NodePassHost<'_>,
        deadline: Option<Instant>,
        exit: Option<BorrowedFd<'_>>,
        looked: Instant,
    ) {
        self.arm(host.paths);
        // A route socket that failed is opened again; until one works, changes of network go
        // unheard and the routes are read each look, so a look is owed a pace after this one.
        // One that opens again owes that look too: a change may have come before it opened.
        let deadline = if self.changes.check().is_err() {
            self.changes.reset();
            deadline.into_iter().chain([looked + host.pace.look]).min()
        } else {
            deadline
        };
        loop {
            if host.stop.is_set() {
                return;
            }
            #[cfg(target_os = "macos")]
            if self.broker_arrived(host.log) {
                break;
            }
            // The broker's retry bounds this wait, not the look: past it, the poll above dials
            // again, and only a broker that answers owes the look.
            #[cfg(target_os = "macos")]
            let wake = match self
                .broker
                .as_ref()
                .map_or(Duration::MAX, |broker| broker.wait_for(Duration::MAX))
            {
                Duration::MAX => deadline,
                left => deadline
                    .into_iter()
                    .chain(Instant::now().checked_add(left))
                    .min(),
            };
            #[cfg(not(target_os = "macos"))]
            let wake = deadline;
            let Ok(stop) = host.stop.bell() else {
                // Out of descriptors: rest a look's while, as a loop nothing rings does.
                host.stop.sleep(host.pace.look);
                return;
            };
            let mut also = vec![stop, self.rung.fd()];
            also.extend(exit);
            #[cfg(target_os = "macos")]
            also.extend(
                self.broker
                    .as_ref()
                    .and_then(aterm_uds::lan::Arrivals::watch_fd),
            );
            let changed = self.changes.wait(wake, &also);
            if host.stop.is_set() {
                return;
            }
            let exited = exit.is_some_and(|fd| {
                aterm_uds::ready::any_now(&[fd]).is_ok_and(|ready| !ready.is_empty())
            });
            if changed
                || self.rung.clear()
                || exited
                || deadline.is_some_and(|at| Instant::now() >= at)
            {
                break;
            }
        }
        let spaced = looked + host.pace.look;
        if let Some(left) = spaced.checked_duration_since(Instant::now()) {
            host.stop.sleep(left);
        }
    }
}

/// The wait after a pass, `unfinished` of them in a row busy or failed: [`NodePassPace::every`]
/// after one that finished, otherwise [`NodePassPace::again`], doubling up to `every`.
fn node_pass_wait(pace: &NodePassPace, unfinished: u32) -> Duration {
    if unfinished == 0 {
        return pace.every;
    }
    pace.again
        .saturating_mul(1_u32 << (unfinished - 1).min(16))
        .min(pace.every)
}

/// One owned runner uses the subscriber's same request and retry policy. Node-local
/// materialization and source observations progress while HTTP stands unchanged; a start
/// waits until the LAN is admitted. Pacing uses elapsed time, independently of the vault clock.
#[derive(Default)]
struct NodeConverge {
    due: crate::converge::Due,
    runner: Option<aterm_uds::spawnfd::Child>,
    /// Readable once `runner` has exited ([`aterm_uds::exitwatch::exit_fd`]); `None` with no
    /// runner, or where none could be made.
    exit: Option<std::os::fd::OwnedFd>,
    /// A start the LAN held while one was owed, tried again on its own backoff; the broker's
    /// arrival is heard sooner where it can be.
    lan: crate::converge::RunnerRetry,
    clock: Option<Instant>,
}

impl NodeConverge {
    fn tend(&mut self, host: &NodePassHost<'_>, inputs: &mut super::ConvergenceInputs) {
        let clock = self.clock.get_or_insert_with(Instant::now);
        let now_ms = u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX);
        let snapshot = inputs.snapshot();
        self.due.observe(snapshot.refresh);
        if let Some(request) = snapshot.repositories {
            self.due.request(request);
        }
        if snapshot.identity == super::projection::IdentityDebt::Changed {
            self.due.identity_changed();
        }
        self.tend_at(now_ms, (host.routes)(), host.may_post, host.start_converge);
    }

    fn tend_at(
        &mut self,
        now_ms: u64,
        routes: crate::converge::Routes,
        may_post: &dyn Fn() -> bool,
        start: &dyn Fn(crate::converge::RepositoryRequest) -> Option<aterm_uds::spawnfd::Child>,
    ) {
        use crate::converge::Last;
        // Each look reaps a runner that ended, and lets its exit go.
        let last = Last::of(&mut self.runner);
        if self.runner.is_none() {
            self.exit = None;
        }
        if !may_post() {
            self.due.hold(now_ms, routes, last);
            if !self.due.owes_start(now_ms) {
                self.lan = crate::converge::RunnerRetry::default();
            } else if self.lan.remaining(now_ms).is_none_or(|left| left.is_zero()) {
                self.lan.reaped(Last::Failed, now_ms);
            }
        } else {
            self.lan = crate::converge::RunnerRetry::default();
            if self.due.poll(now_ms, routes, last) {
                self.runner = start(self.due.repository_request());
                self.exit = self
                    .runner
                    .as_ref()
                    .and_then(|child| aterm_uds::exitwatch::exit_fd(child.id()));
                if self.runner.is_none() {
                    self.due.reaped(Last::Failed, now_ms);
                }
            }
        }
    }

    /// When this owes a look with nothing heard: none while a runner runs whose exit is heard,
    /// at once while one runs whose exit is not, the LAN's retry while it holds a start that is
    /// owed (the due's own times, past while it holds, start nothing until the LAN opens),
    /// otherwise the due's next start ([`crate::converge::Due::next_at`]). `None` before the
    /// first look.
    fn next_at(&self) -> Option<Instant> {
        let clock = self.clock?;
        if self.runner.is_some() {
            return self.exit.is_none().then(Instant::now);
        }
        let now_ms = u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX);
        if let Some(left) = self.lan.remaining(now_ms) {
            return Instant::now().checked_add(left);
        }
        self.due
            .next_at()
            .and_then(|at| clock.checked_add(Duration::from_millis(at)))
    }

    /// The runner's exit, ready once it ended and until a look reaps it.
    fn exit(&self) -> Option<BorrowedFd<'_>> {
        self.exit.as_ref().map(AsFd::as_fd)
    }
}

fn keep_node_pass_end(
    host: &NodePassHost<'_>,
    receipt: &NodePassReceipt,
    error: Option<&Error>,
    last_said: &mut Option<NodePassSaid>,
) {
    if last_said.replace(receipt.said) != Some(receipt.said) {
        (host.log)(&match error {
            Some(error) => format!("vault pass {}: {error}", receipt.said.word()),
            None => format!("vault pass {}", receipt.said.word()),
        });
    }
    let _ = fsx::write_atomic(
        &node_pass_file(host.paths),
        receipt.encode().as_bytes(),
        0o600,
    );
}

#[cfg(test)]
thread_local! {
    /// The negative control of the ranks: while set, every member's turn is the first, at once,
    /// and every holder of a wrap gap closes it at its first look, as every Mac's exchange-time
    /// repair once signed.
    pub(super) static UNRANKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
thread_local! {
    /// The negative control of the removal's grace: while set, every other Mac's grace counts from
    /// the audience's clock, however long before the removal it waits on that started.
    pub(super) static GRACE_FROM_DUE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The negative control of a settled head: while set, every newest epoch another Mac minted
    /// moves the ranks to when this Mac first saw it, as a head arriving mid-countdown once did.
    pub(super) static HEADS_MOVE_RANKS: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// The negative control of the authoring recheck: while set, the signing look checks no
    /// rank, grace or settledness and re-keys every audience [`Log::rekey_dues`] names, however
    /// its heads changed since the scheduling observation.
    pub(super) static STALE_AUTHORING: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// The negative control of the bounded settle grace: while set, the grace does not let an
    /// audience go while [`Log::undecided`] names a Mac there, as before, so a whole batch that
    /// ends in `Wrap`s and could still wrap that Mac above its last one holds the audience until
    /// its minter signs again.
    pub(super) static UNDECIDED_HOLDS: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// The negative control of `[everyone]`'s hold on other audiences: while set, a re-key waits
    /// only on its own audience's newest epochs, as before, so a re-key of a host list or Commons
    /// over a prefix of `[everyone]`'s batch drops a Mac with no seat that batch may still wrap.
    pub(super) static EVERYONE_UNREAD: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// The negative control of the 8,500 stop's exemption (one-mesh §8): while set, the stop
    /// holds every re-key past it, a rotation a held `Remove` made due too, so the writers that
    /// wait on that rotation ([`log_entries::Waiting`]) wait for checkpoints with it. A test sets
    /// it for its thread; a lab instance turns it on for its whole process with
    /// `stop-holds-removals` in `ATERM_TEST_NEGATIVE_CONTROL`
    /// ([`aterm_types::negative_control::on`]).
    pub(super) static STOP_HOLDS_REMOVALS: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Whether [`STOP_HOLDS_REMOVALS`] is on: set on this thread, or named for this process
/// (`stop-holds-removals`); never in a release build, which has neither switch.
fn stop_holds_removals() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        STOP_HOLDS_REMOVALS.with(std::cell::Cell::get)
            || aterm_types::negative_control::on("stop-holds-removals")
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

/// The audiences of `ready` the tender signs at the log's fill (one-mesh §8, "The 8,500 stop"):
/// - every one a held `Remove` made due ([`RekeyDue::rotation`]: a newest epoch still wraps a
///   Mac the log removed, the rotation a remover could not mint, and the coalesce of the heads
///   concurrent removals leave, each of whose epochs wraps the Mac the other removed), at any
///   fill: writers wait on it ([`log_entries::Waiting`]), and the person's removal reserved its
///   room ([`RemovalFill`]; a seat move, below the proof-admission stop);
/// - of the rest (a coalesce of heads no removal left, an F21 reseal's re-`Put`s), each in turn
///   where the log's keyed records and the bound of what it and the re-keys kept before it sign
///   ([`log_entries::rekey_bound`], over the seats `active` they carry) stay within
///   [`crate::membership_log::RecordBudgets::automatic_keyed_stop`], and leave free, in keyed
///   records and in wire bytes, what the re-keys a held `Remove` made due still owe
///   ([`log_entries::owed_rekeys`]; one-mesh proofs P6, F2 and F6: the stop alone left a large
///   lab's reserve to an automatic batch, and bytes no room at all). One that would pass either
///   stays due and waits for checkpoints or for the owed re-keys, nothing signed.
fn within_automatic_stop(
    log: &Log,
    opened: &OpenedAudienceEntries,
    flight: &log_entries::Flight,
    active: &BTreeSet<Recipient>,
    ready: Vec<EpochAudience>,
) -> Vec<EpochAudience> {
    // The stop judges a re-key only where no held `Remove` made it due (one-mesh proofs P6's
    // kernel), and every one under the `STOP_HOLDS_REMOVALS` mutant.
    let judged =
        |rotation: bool| stop_holds_removals() || aterm_mesh_core::capacity::stop_judges(rotation);
    let made_by_removal: BTreeSet<EpochAudience> = log
        .rekey_dues()
        .into_iter()
        .filter(|due| !judged(due.rotation))
        .map(|due| due.audience)
        .collect();
    let removed = log.removed_recipients();
    let no_seats = BTreeSet::new();
    let cost = |audience: &EpochAudience| {
        let seats = if matches!(audience, EpochAudience::Hosts(_)) {
            &no_seats
        } else {
            active
        };
        log_entries::rekey_bound(log, opened, flight, audience, seats, &removed, None)
    };
    let (mut kept, rest): (Vec<EpochAudience>, Vec<EpochAudience>) = ready
        .into_iter()
        .partition(|audience| made_by_removal.contains(audience));
    let mut planned = kept.iter().fold(
        log_entries::RekeyCost {
            keyed: log.counts().keyed,
            bytes: log.held_bytes(),
        },
        |sum, audience| sum.plus(cost(audience)),
    );
    if rest.is_empty() {
        return kept;
    }
    // What the owed re-keys need beside the plan: those kept above are counted in both, which
    // only holds a re-key no removal made due a little earlier near the bound.
    let owed = if automatic_ignores_owed() {
        log_entries::RekeyCost::default()
    } else {
        log_entries::owed_rekeys(log, opened, flight)
    };
    let stop = log.budgets().automatic_keyed_stop();
    let (limit, byte_limit) = (
        log.budgets().keyed,
        crate::membership_log::budgets::held_bytes_bound(),
    );
    for audience in rest {
        let more = cost(&audience);
        if keeps_room(wide(planned.keyed), wide(more.keyed), 0, wide(stop))
            && keeps_room(
                wide(planned.keyed),
                wide(more.keyed),
                wide(owed.keyed),
                wide(limit),
            )
            && keeps_room(
                wide(planned.bytes),
                wide(more.bytes),
                wide(owed.bytes),
                wide(byte_limit),
            )
        {
            planned = planned.plus(more);
            kept.push(audience);
        }
    }
    kept
}

#[cfg(test)]
thread_local! {
    /// MUTANT of [`within_automatic_stop`], on for one test thread: a re-key no removal made due
    /// keeps no owed room free (the 8,500 stop and the log's hard bounds alone), as before
    /// one-mesh proofs P6's F2 and F6 fixes, so its records and bytes can take the room an owed
    /// re-key needs.
    pub(super) static AUTOMATIC_IGNORES_OWED: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn automatic_ignores_owed() -> bool {
    AUTOMATIC_IGNORES_OWED.with(std::cell::Cell::get)
}

#[cfg(not(test))]
const fn automatic_ignores_owed() -> bool {
    false
}

/// Whether [`UNDECIDED_HOLDS`] holds `audience` past the settle grace: it is set and
/// [`Log::undecided`] names a Mac there for `mine`.
#[cfg(test)]
fn undecided_holds(log: &Log, audience: &EpochAudience, mine: Option<&Recipient>) -> bool {
    UNDECIDED_HOLDS.with(std::cell::Cell::get)
        && mine.is_some_and(|mine| !log.undecided(audience, mine).is_empty())
}

#[cfg(not(test))]
const fn undecided_holds(_: &Log, _: &EpochAudience, _: Option<&Recipient>) -> bool {
    false
}

#[cfg(test)]
fn everyone_unread() -> bool {
    EVERYONE_UNREAD.with(std::cell::Cell::get)
}

#[cfg(not(test))]
const fn everyone_unread() -> bool {
    false
}

#[cfg(test)]
fn grace_from_due() -> bool {
    GRACE_FROM_DUE.with(std::cell::Cell::get)
}

#[cfg(not(test))]
const fn grace_from_due() -> bool {
    false
}

#[cfg(test)]
fn heads_move_ranks(log: &Log, epoch: &RecordId, me: &MemberId) -> bool {
    HEADS_MOVE_RANKS.with(std::cell::Cell::get)
        && log.get(epoch).is_some_and(|record| record.actor() != me)
}

#[cfg(not(test))]
const fn heads_move_ranks(_: &Log, _: &RecordId, _: &MemberId) -> bool {
    false
}

/// `me`'s turn for `due` ([`RekeyDue::turn`]).
fn turn_of(
    due: &RekeyDue,
    membership: &crate::membership_log::Membership,
    me: &MemberId,
) -> Option<Turn> {
    #[cfg(test)]
    if UNRANKED.with(std::cell::Cell::get) {
        return Some(Turn {
            rank: 0,
            after_grace: false,
        });
    }
    due.turn(membership, me)
}

/// The re-keys of a vault pass (§3.3.7, keychain "Epochs"; F21 rule 5), one minter per due:
/// for each audience [`Log::rekey_dues`] names (a newest epoch still wrapped to a Mac the log
/// removed, two or more newest epochs that are not twins, or an uncovered keyed epoch to
/// reseal), this Mac mints one higher epoch covering the newest ones once its turn
/// ([`RekeyDue::turn`]) comes:
/// - the audience's clock starts when this Mac first sees it due, and holds while it stays due,
///   whatever new head arrives; it clears only once the audience is due no more;
/// - a remover acts its rank's [`log_entries::GAP_STEP_MS`] after the clock; every other Mac
///   waits [`log_entries::REMOVER_GRACE_MS`] from when it first saw the removals the due waits
///   on ([`RekeyDue::removals`]), for the remover's own rotation to arrive, so a removal that
///   finds the audience due already still gets its grace; with no remover that can carry the
///   audience, its carriers rank with no grace. A removal's clock starts at the first sight of
///   its `Remove` ([`RekeyDue::removed`]), so one whose remover can carry only once a heal wraps
///   it does not restart its grace at the heal;
/// - an epoch another Mac minted whose batch this Mac may hold only a prefix of
///   ([`Log::unsettled`]) holds the audience, until it settles or
///   [`log_entries::SETTLE_GRACE_MS`] after this Mac first saw it (a minter whose batch ends in
///   `Wrap`s and falls silent: no record marks a batch's end, so a whole one that could still
///   wrap a Mac above its last `Wrap` reads as a prefix until its minter signs again); a rank's
///   steps then count from when it let the audience go, so a head that settles late does not put
///   every rank past due at once. A head that arrives settled moves no rank;
/// - every audience but `[everyone]` is held the same way by `[everyone]`'s newest epochs while
///   their batch may still decide a Mac with no seat ([`Log::undecided`]): a re-key reads
///   `[everyone]`'s leave-out for each audience and would drop that Mac for good. Past the grace
///   what it mints leaves such a Mac out, as a removal's own rotation does.
///
/// A re-key no removal made due waits, unsigned and still due, where it would take the log's keyed
/// records past [`crate::membership_log::AUTOMATIC_KEYED_STOP`] ([`within_automatic_stop`]).
///
/// A ranked Mac that cannot open every newest epoch signs nothing, and the next rank acts a step
/// later. What it mints carries the recipients a batch may still wrap ([`Log::in_flight`]). It
/// looks without the state lock and takes it only to sign, looking again under it. Returns the
/// records it signed, already in this Mac's records.
///
/// # Errors
/// Local state cannot be read or written, or a record fails to open or sign.
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "EpochConvergence",
        action = "RotationPasses",
        project = "aterm_secrets::client::remove::tests::convergence::epoch_convergence_transition"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "EpochConvergence",
        action = "PrefixPasses",
        project = "aterm_secrets::client::remove::tests::convergence::epoch_convergence_transition"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "EpochConvergence",
        action = "SettledPasses",
        project = "aterm_secrets::client::remove::tests::convergence::epoch_convergence_transition"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "GapHeal",
        action = "Rotate",
        project = "aterm_secrets::client::remove::tests::gap_heal::gap_heal_transition"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "GapHeal",
        action = "RotateByB",
        project = "aterm_secrets::client::remove::tests::gap_heal::gap_heal_transition"
    )
)]
pub fn tend_epochs_at(paths: &Paths, now_ms: u64) -> crate::Result<Vec<Record>> {
    let Some((identity, mark, ready)) = look_at_dues(paths, now_ms)? else {
        return Ok(Vec::new());
    };
    if ready.is_empty() {
        return Ok(Vec::new());
    }
    tend_ready_epochs(paths, now_ms, &identity, &mark)
}

/// [`tend_epochs_at`], its waits for the vault's locks ending at `deadline`: `None` where a writer
/// still held one then, which says nothing of what is due.
///
/// # Errors
/// As [`tend_epochs_at`].
fn tend_epochs_until(
    paths: &Paths,
    now_ms: u64,
    deadline: Instant,
) -> crate::Result<Option<Vec<Record>>> {
    let Some(look) = log_entries::look_until(paths, deadline)? else {
        return Ok(None);
    };
    let dues = look_at_dues(paths, now_ms)?;
    drop(look);
    let Some((identity, mark, ready)) = dues else {
        return Ok(Some(Vec::new()));
    };
    if ready.is_empty() {
        return Ok(Some(Vec::new()));
    }
    tend_ready_epochs_by(paths, now_ms, &identity, &mark, Some(deadline))
}

/// The tender's scheduling look of a vault pass at `now_ms` ([`ready_rekeys`]), its clocks kept
/// in the epoch mark: this Mac's identity, the mark, and the audiences whose re-key is this Mac's
/// to mint now. `None` with no trusted log, identity or seat, or nothing due (the mark is
/// dropped then).
///
/// # Errors
/// Local state cannot be read, or the mark cannot be written.
fn look_at_dues(
    paths: &Paths,
    now_ms: u64,
) -> crate::Result<Option<(Identity, TendMark, Vec<EpochAudience>)>> {
    let Some(store) = trusted_store(paths)? else {
        return Ok(None);
    };
    let Some(log) = store.log() else {
        return Ok(None);
    };
    let Some(identity) = read_identity(paths)? else {
        return Ok(None);
    };
    let dues = log.rekey_dues();
    if dues.is_empty() {
        let _ = std::fs::remove_file(epoch_mark_file(paths));
        return Ok(None);
    }
    let Some((me, _, _)) = log_entries::open_local_member_signer(paths, log)? else {
        return Ok(None);
    };
    let earlier = read_tend_mark(paths);
    let (mark, ready) = ready_rekeys(log, &me, &earlier, now_ms);
    write_tend_mark(paths, &mark)?;
    Ok(Some((identity, mark, ready)))
}

/// Compute ranks and settledness on the same log snapshot that authoring will use.
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "EpochConvergence",
        action = "Observe",
        project = "aterm_secrets::client::remove::tests::convergence::epoch_convergence_transition"
    )
)]
fn ready_rekeys(
    log: &Log,
    me: &MemberId,
    earlier: &TendMark,
    now_ms: u64,
) -> (TendMark, Vec<EpochAudience>) {
    let mut mark = TendMark::default();
    let membership = log.membership();
    let mine = membership.get(me).map(|member| member.binding.recipient());
    let dues = log.rekey_dues();
    let (waited_to, waits) = if dues
        .iter()
        .any(|due| due.audience != EpochAudience::Everyone)
    {
        everyone_wait(log, me, mine.as_ref(), earlier, &mut mark, now_ms)
    } else {
        (0, false)
    };
    let mut ready = Vec::new();
    for due in &dues {
        let tag = audience_tag(&due.audience);
        // A clock set back since never puts first sight in the future.
        let first = earlier.due.get(&tag).copied().unwrap_or(now_ms).min(now_ms);
        mark.due.insert(tag, first);
        let unsettled = log_entries::unsettled_of_others(log, &due.newest, me);
        let mut from = first;
        let mut held = false;
        for epoch in &due.newest {
            let holding = unsettled.contains(epoch);
            let Some((seen, start)) = earlier.seen.get(epoch).copied().or_else(|| {
                if holding {
                    Some((now_ms, 0))
                } else {
                    heads_move_ranks(log, epoch, me).then_some((now_ms, now_ms))
                }
            }) else {
                // Settled at first sight: it never held the audience, so it moves no rank.
                continue;
            };
            let (seen, start) =
                if start == 0 && holding && undecided_holds(log, &due.audience, mine.as_ref()) {
                    (seen.min(now_ms), 0)
                } else {
                    settle_clock(seen, start, holding, now_ms)
                };
            mark.seen.insert(*epoch, (seen, start));
            held |= start == 0;
            from = from.max(start);
        }
        // Its re-key reads `[everyone]`'s leave-out too ([`everyone_wait`]).
        if due.audience != EpochAudience::Everyone && !everyone_unread() {
            held |= waits;
            from = from.max(waited_to);
        }
        // Each removal's clock starts at this Mac's first sight of its `Remove`, carried or not:
        // a removal whose remover can carry the audience only once a heal wraps it waits from
        // there, not from the heal (`GapHeal`'s `Rotate`).
        let mut removed_at = first;
        for target in &due.removed {
            let at = earlier
                .removal
                .get(target)
                .copied()
                .unwrap_or(now_ms)
                .min(now_ms);
            mark.removal.insert(*target, at);
            if due.removals.contains(target) {
                removed_at = removed_at.max(at);
            }
        }
        if grace_from_due() {
            removed_at = first;
        }
        let Some(turn) = turn_of(due, &membership, me) else {
            continue;
        };
        let spread =
            log_entries::GAP_STEP_MS.saturating_mul(u64::try_from(turn.rank).unwrap_or(u64::MAX));
        let after_grace = if turn.after_grace {
            removed_at.saturating_add(log_entries::REMOVER_GRACE_MS)
        } else {
            0
        };
        let at = from.max(after_grace).saturating_add(spread);
        if !held && now_ms >= at {
            ready.push(due.audience.clone());
        }
    }
    (mark, ready)
}

/// A clock of [`TendMark`] for an epoch another Mac minted, carried on to `now_ms` from first
/// sight `seen` and `start`: `start` is when the epoch let the audience go, set once to `now_ms`
/// when it no longer holds (`holding`: its batch may still be a prefix here) or to first sight
/// plus [`log_entries::SETTLE_GRACE_MS`] once that passed, and zero while it holds. A clock set
/// back since never puts either in the future.
fn settle_clock(seen: u64, start: u64, holding: bool, now_ms: u64) -> (u64, u64) {
    let seen = seen.min(now_ms);
    let start = if start != 0 {
        start.min(now_ms)
    } else if !holding {
        now_ms
    } else if now_ms >= seen.saturating_add(log_entries::SETTLE_GRACE_MS) {
        seen.saturating_add(log_entries::SETTLE_GRACE_MS)
    } else {
        0
    };
    (seen, start)
}

/// How `[everyone]`'s newest epochs hold every other audience's re-key: one another Mac minted,
/// whose batch this Mac may hold only a prefix of, holds them while that batch may still wrap a
/// Mac with no seat ([`Log::undecided`] for `mine`), since a re-key reads `[everyone]`'s
/// leave-out for its own audience too ([`log_entries::carried_audience_recipients`]) and would
/// drop that Mac for good from an audience that wraps it. It holds until the batch settles or
/// no Mac is left undecided, or [`log_entries::SETTLE_GRACE_MS`] after this Mac first saw it, as
/// an audience's own newest epoch does; each such epoch's clock is kept in `mark`'s `wait`.
/// Returns when the last of them let go (the re-keys' ranks count from then) and whether one
/// still holds.
fn everyone_wait(
    log: &Log,
    me: &MemberId,
    mine: Option<&Recipient>,
    earlier: &TendMark,
    mark: &mut TendMark,
    now_ms: u64,
) -> (u64, bool) {
    let newest = log.newest_epochs(&EpochAudience::Everyone);
    let unsettled = log_entries::unsettled_of_others(log, &newest, me);
    let undecided = !unsettled.is_empty()
        && mine.is_some_and(|mine| !log.undecided(&EpochAudience::Everyone, mine).is_empty());
    let mut waited_to = 0;
    let mut waits = false;
    for epoch in &newest {
        let holding = undecided && unsettled.contains(epoch);
        let Some((seen, start)) = earlier
            .wait
            .get(epoch)
            .copied()
            .or_else(|| holding.then_some((now_ms, 0)))
        else {
            continue;
        };
        let (seen, start) = settle_clock(seen, start, holding, now_ms);
        mark.wait.insert(*epoch, (seen, start));
        waits |= start == 0;
        waited_to = waited_to.max(start);
    }
    (waited_to, waits)
}

#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "EpochConvergence",
        action = "Author",
        project = "aterm_secrets::client::remove::tests::convergence::epoch_convergence_transition"
    )
)]
fn tend_ready_epochs(
    paths: &Paths,
    now_ms: u64,
    identity: &Identity,
    mark: &TendMark,
) -> crate::Result<Vec<Record>> {
    Ok(tend_ready_epochs_by(paths, now_ms, identity, mark, None)?.unwrap_or_default())
}

/// [`tend_ready_epochs`], its waits for the vault's locks ending at `deadline` where one is given:
/// `None` where a writer still held one then.
fn tend_ready_epochs_by(
    paths: &Paths,
    now_ms: u64,
    identity: &Identity,
    mark: &TendMark,
    deadline: Option<Instant>,
) -> crate::Result<Option<Vec<Record>>> {
    let me_recipient = identity.recipient();
    // The re-key's carry is the current seats (one-mesh M1, M4): a seat a newest epoch omitted
    // is a gap the heal closes, never a Mac to drop from the next epoch.
    let active_recipients_of = |log: &Log| -> crate::Result<BTreeSet<Recipient>> {
        let local_roster = log_entries::local_roster_denies_for_log(paths, log, None)?;
        Ok(log.current_recipients_where(&EpochAudience::Everyone, |m| {
            local_roster
                .as_ref()
                .is_some_and(|r| log_entries::member_denied_by_roster(r, m))
        }))
    };
    let draft = |store: &Store| -> crate::Result<(Vec<Record>, bool)> {
        let Some(mut log) = store.log().cloned() else {
            return Ok((Vec::new(), false));
        };
        let active = active_recipients_of(&log)?;
        if !active.contains(&me_recipient) {
            return Ok((Vec::new(), false));
        }
        let Some((actor, device, incarnation)) =
            log_entries::open_local_member_signer(paths, &log)?
        else {
            return Ok((Vec::new(), false));
        };
        // A pull or a retry may have changed the due's heads, carriers or membership.
        // Recheck every scheduling prerequisite before signing from this observation.
        let (_, still) = ready_rekeys(&log, &actor, mark, now_ms);
        #[cfg(test)]
        let still = if STALE_AUTHORING.with(std::cell::Cell::get) {
            log.rekey_dues()
                .into_iter()
                .map(|due| due.audience)
                .collect()
        } else {
            still
        };
        // Once the fold is on here, `[everyone]` is the one keyed audience that gets new epochs
        // (one-mesh M6): a Commons due is a Mac's whose gate has not held yet, never this one's.
        let still = if crate::commons::fold::on(paths) {
            still
                .into_iter()
                .filter(|audience| *audience != EpochAudience::Commons)
                .collect()
        } else {
            still
        };
        if still.is_empty() {
            return Ok((Vec::new(), false));
        }
        let Some(opened) = log.open_audience_entries(identity)? else {
            return Ok((Vec::new(), false));
        };
        let denies = log_entries::Denies::read(paths, &log, &opened, None)?;
        // Read before the first epoch this pass mints, as `opened` was.
        let flight = log_entries::Flight::of_batch(&log, &opened, &actor);
        let still = within_automatic_stop(&log, &opened, &flight, &active, still);
        if still.is_empty() {
            return Ok((Vec::new(), false));
        }
        let resealing = still
            .iter()
            .any(|audience| log_entries::reseal_ready(&opened, audience));
        let mut minted = Vec::new();
        device.with_seed(|writer_seed| {
            let author = AuthorCtx {
                actor: &actor,
                writer_seed,
                incarnation,
                authored_ms: now_ms,
            };
            log_entries::coalesce_concurrent_commons_epochs(
                &mut log,
                &opened,
                &flight,
                &author,
                &active,
                &denies,
                &mut minted,
                &still,
            )?;
            log_entries::rotate_host_lists(
                &mut log,
                &opened,
                &flight,
                &author,
                &BTreeSet::new(),
                &denies,
                &mut minted,
                Some(&still),
            )
            .map(|_| ())
        })?;
        Ok((minted, resealing))
    };
    let authored = match deadline {
        Some(deadline) => log_entries::author_and_commit_records_until(paths, deadline, draft)?,
        None => Some(log_entries::author_and_commit_records(paths, draft)?),
    };
    let Some((minted, _, resealed)) = authored else {
        return Ok(None);
    };
    if !minted.is_empty() {
        // F21 rule 8: a reseal of an uncovered keyed epoch records what each removed Mac could
        // read while it was out, for a person to rotate at its provider.
        if resealed
            && let Some(store) = trusted_store(paths)?
            && let Some(log) = store.log()
        {
            record_read_after_cutoff(paths, log, identity)?;
        }
    }
    Ok(Some(minted))
}

/// A held record is an epoch, which can name one due audience, one seen clock and one wait
/// clock, a `Remove`, which can name one removal clock, or neither. A `due` line takes at most
/// 90 bytes, a `seen` or `wait` line 112 and a `removal` line 126 (64 hex digits per id, 96 per
/// actor, 20 decimal digits per timestamp, prefixes, spaces and newline), so an epoch's 314
/// bytes bound either kind. Keep every clock a bounded log can produce: forgetting an oversized
/// mark would restart its grace on every pass.
const MAX_TEND_MARK: usize = crate::membership_log::MAX_HELD_RECORDS * (90 + 112 + 112);

/// One read of [`epoch_mark_file`] ([`tend_epochs_at`]): each due audience's clock by
/// [`audience_tag`]; each newest epoch another Mac minted that this Mac first saw as a prefix of
/// its minter's batch, by id, with when it first saw it and when it let the audience go (zero
/// while it holds it); the same two for each newest `[everyone]` epoch that held the other
/// audiences while its batch could still decide a Mac with no seat ([`everyone_wait`]); and
/// when this Mac first saw each removal a due waits on, by the Mac taken out.
#[derive(Clone, Default)]
struct TendMark {
    due: BTreeMap<[u8; 32], u64>,
    seen: BTreeMap<RecordId, (u64, u64)>,
    wait: BTreeMap<RecordId, (u64, u64)>,
    removal: BTreeMap<MemberId, u64>,
}

/// [`TendMark`] from its lines, `due <tag> <ms>`, `seen <epoch> <ms> <ms>`,
/// `wait <epoch> <ms> <ms>` and `removal <actor> <ms>`; a line of another shape is skipped.
fn read_tend_mark(paths: &Paths) -> TendMark {
    let mut mark = TendMark::default();
    let Ok(Some(bytes)) = fsx::read_private(&epoch_mark_file(paths), MAX_TEND_MARK) else {
        return mark;
    };
    let Ok(text) = std::str::from_utf8(bytes.as_slice()) else {
        return mark;
    };
    let hex = |word: &str| {
        let mut out = [0u8; 32];
        (word.len() == 64 && aterm_codec::hex::decode_into(word.as_bytes(), &mut out))
            .then_some(out)
    };
    let actor = |word: &str| {
        let mut out = [0u8; 48];
        (word.len() == 96 && aterm_codec::hex::decode_into(word.as_bytes(), &mut out))
            .then_some(out)
    };
    let ms = |word: &str| {
        (!word.is_empty() && word.bytes().all(|b| b.is_ascii_digit()))
            .then(|| word.parse::<u64>().ok())
            .flatten()
    };
    for line in text.lines() {
        let words: Vec<&str> = line.split_ascii_whitespace().collect();
        match words.as_slice() {
            ["due", tag, at] => {
                if let (Some(tag), Some(at)) = (hex(tag), ms(at)) {
                    mark.due.insert(tag, at);
                }
            }
            [kind @ ("seen" | "wait"), epoch, seen, start] => {
                if let (Some(epoch), Some(seen), Some(start)) = (hex(epoch), ms(seen), ms(start)) {
                    let clocks = if *kind == "seen" {
                        &mut mark.seen
                    } else {
                        &mut mark.wait
                    };
                    clocks.insert(epoch, (seen, start));
                }
            }
            ["removal", target, at] => {
                if let (Some(target), Some(at)) = (actor(target), ms(at)) {
                    mark.removal.insert(target, at);
                }
            }
            _ => {}
        }
    }
    mark
}

fn write_tend_mark(paths: &Paths, mark: &TendMark) -> crate::Result<()> {
    let mut text = String::new();
    for (tag, at) in &mark.due {
        text.push_str(&format!(
            "due {} {at}\n",
            aterm_codec::hex::encode_lower(tag)
        ));
    }
    for (kind, clocks) in [("seen", &mark.seen), ("wait", &mark.wait)] {
        for (epoch, (seen, start)) in clocks {
            text.push_str(&format!(
                "{kind} {} {seen} {start}\n",
                aterm_codec::hex::encode_lower(epoch)
            ));
        }
    }
    for (target, at) in &mark.removal {
        text.push_str(&format!(
            "removal {} {at}\n",
            aterm_codec::hex::encode_lower(target)
        ));
    }
    fsx::write_atomic(&epoch_mark_file(paths), text.as_bytes(), 0o600)
}

/// A person's `aterm members remove <who>` on this Mac, kept on this Mac: [`remove_member`] with
/// nothing posted to a server and no envelope mirrored. The stand-in F8's lane removes with on one
/// side of a split the lab holds itself (its Macs' subscribers would carry a post across), as
/// [`test_put_everyone`] puts. Returns how many records it signed.
///
/// # Errors
/// When this Mac's vault has no membership log or this Mac has no secrets identity, when `who`
/// names no Mac to remove (or two), and when the records cannot be signed or committed.
#[cfg(any(test, debug_assertions))]
pub fn test_remove_unposted(paths: &Paths, who: &str) -> crate::Result<usize> {
    if trusted_store(paths)?.is_none() {
        return Err(Error::new("this Mac's vault has no membership log"));
    }
    let identity =
        read_identity(paths)?.ok_or_else(|| Error::new("this Mac has no secrets identity"))?;
    let (minted, _, report) = log_entries::author_and_commit_records(paths, |store| {
        draft_removal(paths, store, &identity, Target::Word(who), false)
    })?;
    report?;
    Ok(minted.len())
}

/// A member's write of the `[everyone]` entry `name` through the log, sealed under the one newest
/// `[everyone]` epoch this Mac holds, or the lowest id among newest [`secure_twins`] as a writer
/// seals, and signed with its device key: the stand-in for a member's writer that F8's lane and
/// the tests put a value with after a removal. Returns the epoch it sealed under. A vault pass
/// that holds this Mac's state delays the write, the hang detector at most
/// ([`log_entries::author_and_commit_records_until`]: the snapshot and the draft's reads share one
/// observation), as a person's write is not refused because its own subscriber is applying.
///
/// # Errors
/// This Mac holds no trusted log, is no member, opens no `[everyone]` epoch, or holds two newest
/// ones that are not twins (the coverage rule has not run), a record fails to sign or keep, or a
/// pass or another local writer still held the state at the hang detector.
#[cfg(any(test, debug_assertions))]
pub fn test_put_everyone(paths: &Paths, name: &str, value: &[u8]) -> crate::Result<RecordId> {
    test_put_everyone_draft(paths, &[(name, value)])
}

/// [`test_put_everyone`] of each `(name, value)` of `puts`, in one draft: one actor, one
/// `authored_ms` and one epoch (a Claude Code token's pair, `claude_token`'s tests).
///
/// # Errors
/// As [`test_put_everyone`].
#[cfg(any(test, debug_assertions))]
pub fn test_put_everyone_draft(paths: &Paths, puts: &[(&str, &[u8])]) -> crate::Result<RecordId> {
    if trusted_store(paths)?.is_none() {
        return Err(Error::new("this Mac's vault has no membership log"));
    }
    let identity =
        read_identity(paths)?.ok_or_else(|| Error::new("this Mac has no secrets identity"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let authored = log_entries::author_and_commit_records_until(paths, deadline, |store| {
        let log = store
            .log()
            .ok_or_else(|| Error::new("this Mac holds no membership log"))?;
        let (actor, device, incarnation) = log_entries::open_local_member_signer(paths, log)?
            .ok_or_else(|| Error::new("this Mac is no member"))?;
        let opened = log
            .open_audience_entries(&identity)?
            .ok_or_else(|| Error::new("this Mac opens no epoch"))?;
        let newest = opened
            .maximal_epochs
            .get(&EpochAudience::Everyone)
            .cloned()
            .unwrap_or_default();
        let removed = log.removed_recipients();
        let epoch = match newest.as_slice() {
            [one] => one,
            twins
                if crate::membership_log::secure_twins(
                    twins,
                    &opened.wrapped_recipients,
                    &removed,
                ) =>
            {
                twins.iter().min().unwrap_or(&twins[0])
            }
            _ => {
                return Err(Error::new(format!(
                    "{} newest [everyone] epochs: the coverage rule has not run",
                    newest.len()
                )));
            }
        };
        let key = opened
            .unwrapped_keys
            .get(epoch)
            .ok_or_else(|| Error::new("this Mac does not hold the newest [everyone] epoch"))?;
        let authored_ms = aterm_time::unix_millis().unwrap_or(0);
        let mut log = log.clone();
        let mut signed = Vec::with_capacity(puts.len());
        for (name, value) in puts {
            let entry = Entry {
                name: (*name).to_owned(),
                value: SecretBytes::from(*value),
                audience: Audience::Everyone,
                if_absent: false,
                anchor: None,
            };
            let put = device.with_seed(|seed| {
                log.draft_put(&actor, authored_ms, *epoch, key, &entry)?
                    .sign(seed, incarnation)
            })?;
            let _ = log.insert(put.clone())?;
            signed.push(put);
        }
        Ok((signed, *epoch))
    })?;
    let Some((minted, _, epoch)) = authored else {
        return Err(Error::new(
            "a vault pass or another local writer still held this Mac's state after a minute",
        ));
    };
    if !minted.is_empty() {
        let _ = post_records_to_servers(paths);
    }
    Ok(epoch)
}

/// One member as `aterm members list` prints it ([`member_list`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedMac {
    /// Its label in the log.
    pub label: String,
    /// Its host name.
    pub name: String,
    /// The label of the member that let it in; `None` for a founder.
    pub admitted_by: Option<String>,
    /// When its entry was signed (UTC milliseconds, as its signer stamped).
    pub admitted_ms: u64,
    /// Its join code.
    pub code: String,
    /// Why it is out, where it is.
    pub removed: Option<RemovedNote>,
}

/// Membership as this Mac's trusted log evaluates it, members first by label, then the Macs it
/// removed. `None` where this Mac holds no trusted log.
///
/// # Errors
/// The records file cannot be read.
pub fn member_list(paths: &Paths) -> crate::Result<Option<Vec<ListedMac>>> {
    let Some(store) = trusted_store(paths)? else {
        return Ok(None);
    };
    let Some(log) = store.log() else {
        return Ok(None);
    };
    let local_roster = log_entries::local_swarm_roster_for_log(paths, log)?;
    let membership = log.membership();
    let mut out: Vec<ListedMac> = membership
        .members()
        .iter()
        .filter(|(actor, m)| {
            !log.recipient_is_removed(&m.binding.recipient())
                && !membership.member_node_removed(m)
                && !log.audience_left_out(&EpochAudience::Everyone, actor)
                && !log
                    .audience_left_out_recipient(&EpochAudience::Everyone, &m.binding.recipient())
                && !local_roster
                    .as_ref()
                    .is_some_and(|r| log_entries::member_denied_by_roster(r, m))
        })
        .map(|(_, m)| ListedMac {
            label: m.label.clone(),
            name: m.name.clone(),
            admitted_by: m.admitted_by.map(|by| label_of(log, &membership, &by)),
            admitted_ms: log.get(&m.entry).map_or(0, Record::authored_ms),
            code: join_code(&m.binding.recipient()),
            removed: None,
        })
        .collect();
    out.sort_by(|a, b| a.label.cmp(&b.label));
    let notes = notes_of(log);
    let mut seen_gone = BTreeSet::new();
    let mut gone: Vec<ListedMac> = membership
        .removed()
        .keys()
        .filter_map(|actor| {
            let (recipient, label, name) = seat_of(log, actor)?;
            let mut writer = [0u8; 32];
            writer.copy_from_slice(&actor[..32]);
            let node = seat_nodes_of(log, &membership, actor)
                .into_iter()
                .next()
                .unwrap_or([0; 32]);
            let removed = notes.of_with_node(&recipient, &writer, &node)?.clone();
            if !seen_gone.insert((recipient.to_bech32(), label.clone())) {
                return None;
            }
            Some(ListedMac {
                label,
                name,
                admitted_by: None,
                admitted_ms: 0,
                code: join_code(&recipient),
                removed: Some(removed),
            })
        })
        .collect();
    gone.sort_by(|a, b| a.label.cmp(&b.label));
    out.extend(gone);
    Ok(Some(out))
}

/// Host for `EpochConvergence`'s environment markers, so `spec_xref_closure` accounts for every
/// model action: which lab shape it is, and when records reach a Mac, are what the exchange and
/// the Macs around the tender do.
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "EpochConvergence",
        action = "Rotation",
        reason = "Environment: a remover's batch reaches every seated Mac but one, which holds its Remove"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "EpochConvergence",
        action = "Prefix",
        reason = "Environment: a Mac removes one while it holds a prefix of another's removal batch"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "EpochConvergence",
        action = "Settled",
        reason = "Environment: three concurrent re-keys, of which a carrier holds two"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "EpochConvergence",
        action = "Recheck",
        reason = "Environment: a Mac removes one while it holds another's Remove alone, past every grace"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "EpochConvergence",
        action = "Deliver",
        reason = "Environment: the rest of the remover's batch reaches the Mac within the grace"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "EpochConvergence",
        action = "Heal",
        reason = "Environment: every seated Mac takes every other's records"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "EpochConvergence",
        action = "Rest",
        reason = "Environment: the rest of the other Mac's batch arrives"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "EpochConvergence",
        action = "Head",
        reason = "Environment: the third re-key arrives, settled"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "EpochConvergence",
        action = "Arrive",
        reason = "Environment: a prefix of the other Mac's batch arrives between the look and the signing"
    )
)]
#[doc(hidden)]
pub fn epoch_convergence_environment() {}

/// Host for `GapHeal`'s environment markers (one-mesh §9; A2's model, A3's conformance test), so
/// `spec_xref_closure` accounts for every model action: what the lab's shape is, what an old
/// build mints, when records reach a Mac, a person's roster deny, a Mac's sleep and the clock
/// are what the Macs around the closer, the tender, the remover and the desk do.
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "GapHeal",
        action = "OldMint",
        reason = "Environment: a pre-R1 Mac (C) mints by the intersection carry over a partial view, leaving out a Mac whose Admit it holds"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "GapHeal",
        action = "OldMintOmitsB",
        reason = "Environment: a pre-R1 Mac (C) mints by the intersection carry, leaving out a current seat it knows"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "GapHeal",
        action = "OldMintUnder",
        reason = "Environment: a pre-R1 Mac (C) mints as in OldMint, the Mac it leaves out sorting below its last Wrap"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "GapHeal",
        action = "Churn",
        reason = "Environment: the lab shape where one Mac removes another while a second admits a newcomer"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "GapHeal",
        action = "Deny",
        reason = "Environment: a person denies a seat in the swarm roster (or its node is removed), reaching every Mac at once"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "GapHeal",
        action = "DeliverToA",
        reason = "Environment: a minter's batch reaches A, as a prefix or whole"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "GapHeal",
        action = "DeliverToB",
        reason = "Environment: a minter's batch reaches B whole"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "GapHeal",
        action = "ToA",
        reason = "Environment: B's records reach A"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "GapHeal",
        action = "ToB",
        reason = "Environment: A's records reach B: its batch as a prefix, then the rest, then its heals"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "GapHeal",
        action = "Sleep",
        reason = "Environment: A sleeps after its pass ran over what it last received"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "GapHeal",
        action = "Wake",
        reason = "Environment: A wakes and pulls every record before its next pass"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "GapHeal",
        action = "Tick",
        reason = "Environment: the injected clock passes the next deadline (a rank step, the settle grace, the remover's grace)"
    )
)]
#[doc(hidden)]
pub fn gap_heal_environment() {}

#[cfg(test)]
#[path = "node_pass_tests.rs"]
mod node_pass_tests;

#[cfg(test)]
#[path = "remove_tests.rs"]
pub(crate) mod tests;
