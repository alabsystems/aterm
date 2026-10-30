// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Any member removes (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.3.7, F16).
//!
//! Where this Mac's vault has a membership log its pin trusts, holding epochs
//! ([`log_entries::trusted_seeded_store`]), and the log seats this Mac's device key, a person's
//! act removes any member with no admin:
//!
//! - [`remove_member`] signs `Remove{member, cutoff}` (the cutoff is the removed Mac's last record
//!   this log holds, else the record that let it in), mints a new epoch for every audience the Mac
//!   was in that this Mac opens (`[everyone]`, Commons, and each host list whose newest epoch
//!   wraps it), wraps each to the members who remain and reseals the current entries under it.
//!   The `[everyone]` reseal carries the revoke cascade (the roster's deny-lists, the pool's slots,
//!   and on `--stolen` a new fleet code). It returns the entries the Mac could read, for rotation
//!   at the provider, and on `--stolen` the one `NEEDS YOU:` line (C25).
//! - [`tend_epochs`] is the coverage rule on every vault pass: where removals on two sides of a
//!   split left two newest epochs, or a newest epoch is still wrapped to a Mac the log removed
//!   (a quorum `Remove` voided the remover's rotation), one member mints one higher epoch that
//!   covers them ([`log_entries::coalesce_concurrent_commons_epochs`], the rule F18's Commons
//!   handshake runs too).
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
    Body, EpochAudience, Kind, Log, MemberId, Record, RecordId, Store, Undo,
};
use aterm_digest::Sha256;

/// What [`remove_member`] did.
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
    /// `aterm-fleet-code` was rotated (`--stolen`).
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

/// Every Mac the log removed, by its age recipient (bech32) and by its writer key, so a removed
/// Mac that asks again under a new device key with the same machine key, or a new machine key
/// with the same writer key, is still named.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemovedNotes {
    /// By age recipient.
    pub by_recipient: BTreeMap<String, RemovedNote>,
    /// By Ed25519 writer key.
    pub by_writer: BTreeMap<[u8; 32], RemovedNote>,
}

impl RemovedNotes {
    /// The note for the Mac a request binds, by recipient, else by writer key.
    #[must_use]
    pub fn of(&self, recipient: &Recipient, writer: &[u8; 32]) -> Option<&RemovedNote> {
        self.by_recipient
            .get(&recipient.to_bech32())
            .or_else(|| self.by_writer.get(writer))
    }
}

/// `ms` (UTC) as a month and day: `Oct 30`.
fn day(ms: u64) -> String {
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
fn trusted_store(paths: &Paths) -> crate::Result<Option<Store>> {
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

/// What a founder or `Admit` says of the Mac `actor`: its age recipient, label and name, from
/// the genesis, else its `Admit` with the lowest id.
fn seat_of(log: &Log, actor: &MemberId) -> Option<(Recipient, String, String)> {
    if let Body::Genesis { founders, .. } = log.genesis().body()
        && let Some(f) = founders.iter().find(|f| f.member() == Some(*actor))
    {
        return Some((f.recipient, f.label.clone(), f.name.clone()));
    }
    log.records()
        .filter_map(|r| match r.body() {
            Body::Admit { entrant, .. } if entrant.member() == *actor => Some((r.id(), entrant)),
            _ => None,
        })
        .min_by_key(|(id, _)| **id)
        .map(|(_, entrant)| {
            (
                entrant.request.binding().recipient(),
                entrant.label.clone(),
                entrant.name.clone(),
            )
        })
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
        if let Ok((_, _, label)) = resolve(who, &removed) {
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
    let mine = read_identity(paths).ok()??.recipient().to_bech32();
    let doc = &admin.published_doc;
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
/// Mac reaches. With `stolen` the fleet code rotates too, and the result carries the `NEEDS YOU:`
/// line (C25). It runs under this Mac's state lock, never inside the `ask` exchange.
///
/// # Errors
/// This Mac's vault has no trusted log, the log seats no device key of this Mac as a member, `who`
/// names no member, more than one, this Mac itself or a founder that has proved no key, or a
/// record fails to sign or keep.
pub fn remove_member(paths: &Paths, who: &str, stolen: bool) -> crate::Result<MemberRemoval> {
    if trusted_store(paths)?.is_none() {
        return Err(Error::new(
            "this Mac's vault has no membership log yet: an admin removes a Mac with `aterm secrets revoke`",
        ));
    }
    let identity =
        read_identity(paths)?.ok_or_else(|| Error::new("this Mac has no secrets identity"))?;
    let vault_admin = vault_admin_named(paths, who);
    let (minted, _, report) = log_entries::author_and_commit_records(paths, |store| {
        draft_removal(paths, store, &identity, Target::Word(who), stolen)
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
/// to the vault's source alone: [`super::remove_member_in_log`], which the Commons runtime and
/// the tests call by actor. Where another member's removal of `target` is here already, this
/// Mac's share at that removal's cutoff instead. Returns the records it signed.
///
/// # Errors
/// As [`remove_member`], for a member named by its actor; for a share, as
/// `log_entries::draft_retire_in_log`.
pub(super) fn remove_actor(paths: &Paths, target: MemberId) -> crate::Result<Vec<Record>> {
    let identity =
        read_identity(paths)?.ok_or_else(|| Error::new("this Mac has no secrets identity"))?;
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

/// What a draft signed, beside what it reports to the person (its refusal, where it refused).
type Drafted<T> = crate::Result<(Vec<Record>, crate::Result<T>)>;

/// Who a removal names: the word a person typed, or a member's actor.
#[derive(Clone, Copy)]
enum Target<'a> {
    Word(&'a str),
    Actor(MemberId),
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
    let named = match target {
        Target::Word(who) => removal_target(&log, who, &actor),
        Target::Actor(target) if target == actor => Err(Error::new("a Mac does not remove itself")),
        Target::Actor(target) => log
            .membership()
            .get(&target)
            .map(|m| (target, m.binding.recipient(), m.label.clone()))
            .ok_or_else(|| Error::new("target member is not active in the membership log")),
    };
    let (target, recipient, label) = match named {
        Ok(target) => target,
        Err(error) => return refuse(error),
    };
    let Some(opened) = log.open_audience_entries(identity)? else {
        return refuse(Error::new(
            "this Mac cannot open the membership log's epochs",
        ));
    };
    let entry = log
        .membership()
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
    let mut labels = BTreeSet::from([label.clone()]);
    if let Some((_, seat_label, _)) = seat_of(&log, &target) {
        labels.insert(seat_label);
    }
    let mut node_keys = BTreeSet::new();
    let membership = log.membership();
    let mut target_actors = BTreeSet::from([target]);
    if let Some(member) = membership.get(&target)
        && let Some(nk) = member.node_key.filter(|k| *k != [0; 32])
    {
        node_keys.insert(aterm_codec::hex::encode_lower(&nk));
    }
    for (actor, member) in membership
        .members()
        .iter()
        .filter(|(_, m)| m.binding.recipient() == recipient)
    {
        target_actors.insert(*actor);
        if let Some((_, canon)) = crate::swarm_roster::leading_m_label(&member.label) {
            labels.insert(canon);
        }
        if let Some(nk) = member.node_key.filter(|k| *k != [0; 32]) {
            node_keys.insert(aterm_codec::hex::encode_lower(&nk));
        }
    }
    for (nk, standing) in membership.nodes() {
        if *nk != [0; 32]
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

    let authored_ms = aterm_time::unix_millis().unwrap_or(0);
    let mut minted = Vec::new();
    let rotated = device.with_seed(|writer_seed| -> crate::Result<Vec<String>> {
        let remove = log
            .draft(
                &actor,
                authored_ms,
                Body::Remove {
                    member: target,
                    cutoff,
                },
            )
            .sign(writer_seed, incarnation)?;
        let _ = log.insert(remove.clone())?;
        minted.push(remove);
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
            },
            &mut minted,
        )
    })?;
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

pub(super) fn notes_of(log: &Log) -> RemovedNotes {
    let membership = log.membership();
    let active_recipients: BTreeSet<Recipient> = membership
        .members()
        .values()
        .filter(|m| !log.recipient_is_removed(&m.binding.recipient()))
        .map(|m| m.binding.recipient())
        .collect();
    let active_writers: BTreeSet<[u8; 32]> = membership
        .members()
        .iter()
        .filter(|(_, m)| !log.recipient_is_removed(&m.binding.recipient()))
        .map(|(actor, _)| {
            let mut w = [0u8; 32];
            w.copy_from_slice(&actor[..32]);
            w
        })
        .collect();
    let mut notes = RemovedNotes::default();
    for (actor, removal) in membership.removed() {
        let Some((recipient, seat_label, _)) = seat_of(log, actor) else {
            continue;
        };
        let mut writer = [0u8; 32];
        writer.copy_from_slice(&actor[..32]);
        if !removal.quorum
            && (active_recipients.contains(&recipient) || active_writers.contains(&writer))
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
        notes.by_writer.insert(writer, note.clone());
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
    let membership = log.membership();
    let Some(removal) = membership.removal(&me) else {
        return Ok(Vec::new());
    };
    let mut offers = Vec::new();
    for remover in &removal.by {
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
            label: label_of(log, &membership, remover),
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
    if trusted_store(paths)?.is_none() {
        return Err(Error::new("this Mac's vault has no membership log yet"));
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

/// What the epoch tender remembers between passes: which newest epochs it last saw due, and when
/// it first saw them.
fn epoch_mark_file(paths: &Paths) -> PathBuf {
    paths.dir.join("epoch-tend")
}

const EPOCH_RANK_DOMAIN: &[u8] = b"aterm/members/epoch-rank/1\0";

/// [`tend_epochs_at`] at the wall clock, and what it signed posted to every server this Mac
/// reaches: the subscriber's pass after it pulls records. Best effort: a pass that fails here
/// tries again on the next.
pub fn tend_epochs(paths: &Paths) -> crate::Result<Vec<Record>> {
    let now_ms = aterm_time::unix_millis().unwrap_or(0);
    let signed = tend_epochs_at(paths, now_ms)?;
    if !signed.is_empty() {
        let _ = post_records_to_servers(paths);
    }
    Ok(signed)
}

/// The coverage rule (§3.3.7, keychain "Epochs") on a vault pass: where `[everyone]` or Commons
/// has two or more newest epochs, or a newest epoch is still wrapped to a Mac the log removed
/// ([`Log::rotation_due`]), the member ranked `k` among those that hold the newest epochs (by
/// `SHA-256(domain ‖ actor ‖ the newest epochs)`) mints one higher epoch covering them once `k`
/// [`log_entries::GAP_STEP_MS`] have passed since this Mac first saw them due, as wrap gaps are
/// spread. It looks without the state lock and takes it only to sign. Returns the records it
/// signed, already in this Mac's records.
///
/// # Errors
/// Local state cannot be read or written, or a record fails to open or sign.
pub fn tend_epochs_at(paths: &Paths, now_ms: u64) -> crate::Result<Vec<Record>> {
    let Some(store) = trusted_store(paths)? else {
        return Ok(Vec::new());
    };
    let Some(log) = store.log() else {
        return Ok(Vec::new());
    };
    let Some(identity) = read_identity(paths)? else {
        return Ok(Vec::new());
    };
    let me_recipient = identity.recipient();
    let active: BTreeSet<Recipient> = log
        .membership()
        .members()
        .values()
        .map(|m| m.binding.recipient())
        .collect();
    let hosts_due = log.hosts_rotation_due(&me_recipient);
    if !log.rotation_due(&me_recipient, &active) && hosts_due.is_empty() {
        let _ = std::fs::remove_file(epoch_mark_file(paths));
        return Ok(Vec::new());
    }
    let Some((me, _, _)) = log_entries::open_local_member_signer(paths, log)? else {
        return Ok(Vec::new());
    };
    let Some(opened) = log.open_audience_entries(&identity)? else {
        return Ok(Vec::new());
    };
    let mut newest: Vec<RecordId> = [EpochAudience::Everyone, EpochAudience::Commons]
        .into_iter()
        .chain(hosts_due.into_iter().map(EpochAudience::Hosts))
        .flat_map(|aud| opened.maximal_epochs.get(&aud).cloned().unwrap_or_default())
        .collect();
    newest.sort_unstable();
    let key = Sha256::digest(newest.concat());
    let membership = log.membership();
    let mut holders: Vec<([u8; 32], MemberId)> = membership
        .members()
        .iter()
        .filter(|(_, m)| {
            newest.iter().any(|id| {
                opened
                    .wrapped_recipients
                    .get(id)
                    .is_some_and(|set| set.contains(&m.binding.recipient()))
            })
        })
        .map(|(actor, _)| {
            let mut buf = EPOCH_RANK_DOMAIN.to_vec();
            buf.extend_from_slice(actor);
            buf.extend_from_slice(&key);
            (Sha256::digest(&buf), *actor)
        })
        .collect();
    holders.sort();
    let rank = holders
        .iter()
        .position(|(_, actor)| *actor == me)
        .unwrap_or(holders.len());
    let first = read_epoch_mark(paths)
        .filter(|(seen, _)| *seen == key)
        .map_or(now_ms, |(_, at)| at.min(now_ms));
    fsx::write_atomic(
        &epoch_mark_file(paths),
        format!("{} {first}\n", aterm_codec::hex::encode_lower(&key)).as_bytes(),
        0o600,
    )?;
    let due = first.saturating_add(
        log_entries::GAP_STEP_MS.saturating_mul(u64::try_from(rank).unwrap_or(u64::MAX)),
    );
    if now_ms < due {
        return Ok(Vec::new());
    }
    let (minted, _, ()) = log_entries::author_and_commit_records(paths, |store| {
        let Some(mut log) = store.log().cloned() else {
            return Ok((Vec::new(), ()));
        };
        let active: BTreeSet<Recipient> = log
            .membership()
            .members()
            .values()
            .map(|m| m.binding.recipient())
            .collect();
        let coalesce = log.rotation_due(&me_recipient, &active);
        if !coalesce && log.hosts_rotation_due(&me_recipient).is_empty() {
            return Ok((Vec::new(), ()));
        }
        let Some((actor, device, incarnation)) =
            log_entries::open_local_member_signer(paths, &log)?
        else {
            return Ok((Vec::new(), ()));
        };
        let Some(opened) = log.open_audience_entries(&identity)? else {
            return Ok((Vec::new(), ()));
        };
        let mut minted = Vec::new();
        device.with_seed(|writer_seed| {
            let author = AuthorCtx {
                actor: &actor,
                writer_seed,
                incarnation,
                authored_ms: now_ms,
            };
            if coalesce {
                log_entries::coalesce_concurrent_commons_epochs(
                    &mut log,
                    &opened,
                    &author,
                    &active,
                    &mut minted,
                )?;
            }
            log_entries::rotate_host_lists(
                &mut log,
                &opened,
                &author,
                &BTreeSet::new(),
                &mut minted,
            )
            .map(|_| ())
        })?;
        Ok((minted, ()))
    })?;
    if !minted.is_empty() {
        let _ = std::fs::remove_file(epoch_mark_file(paths));
    }
    Ok(minted)
}

fn read_epoch_mark(paths: &Paths) -> Option<([u8; 32], u64)> {
    let bytes = fsx::read_private(&epoch_mark_file(paths), 256).ok()??;
    let text = std::str::from_utf8(bytes.as_slice()).ok()?;
    let (key, at) = text.trim().split_once(' ')?;
    let mut out = [0u8; 32];
    (key.len() == 64 && aterm_codec::hex::decode_into(key.as_bytes(), &mut out)).then_some(())?;
    Some((out, at.parse().ok()?))
}

/// A member's write of the `[everyone]` entry `name` through the log, sealed under the one newest
/// `[everyone]` epoch this Mac holds and signed with its device key: the stand-in for a member's
/// writer that F8's lane and the tests put a value with after a removal. Returns the epoch it
/// sealed under.
///
/// # Errors
/// This Mac holds no trusted log, is no member, opens no `[everyone]` epoch, or holds two newest
/// ones (the coverage rule has not run), or a record fails to sign or keep.
#[cfg(any(test, debug_assertions))]
pub fn test_put_everyone(paths: &Paths, name: &str, value: &[u8]) -> crate::Result<RecordId> {
    if trusted_store(paths)?.is_none() {
        return Err(Error::new("this Mac's vault has no membership log"));
    }
    let identity =
        read_identity(paths)?.ok_or_else(|| Error::new("this Mac has no secrets identity"))?;
    let (minted, _, epoch) = log_entries::author_and_commit_records(paths, |store| {
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
        let [epoch] = newest.as_slice() else {
            return Err(Error::new(format!(
                "{} newest [everyone] epochs: the coverage rule has not run",
                newest.len()
            )));
        };
        let key = opened
            .unwrapped_keys
            .get(epoch)
            .ok_or_else(|| Error::new("this Mac does not hold the newest [everyone] epoch"))?;
        let entry = Entry {
            name: name.to_owned(),
            value: SecretBytes::from(value),
            audience: Audience::Everyone,
            if_absent: false,
            anchor: None,
        };
        let put = device.with_seed(|seed| {
            log.draft_put(
                &actor,
                aterm_time::unix_millis().unwrap_or(0),
                *epoch,
                key,
                &entry,
            )?
            .sign(seed, incarnation)
        })?;
        Ok((vec![put], *epoch))
    })?;
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
    let membership = log.membership();
    let mut out: Vec<ListedMac> = membership
        .members()
        .values()
        .filter(|m| !log.recipient_is_removed(&m.binding.recipient()))
        .map(|m| ListedMac {
            label: m.label.clone(),
            name: m.name.clone(),
            admitted_by: m.admitted_by.map(|by| label_of(log, &membership, &by)),
            admitted_ms: log.get(&m.entry).map_or(0, Record::authored_ms),
            code: join_code(&m.binding.recipient()),
            removed: None,
        })
        .collect();
    out.sort_by(|a, b| a.label.cmp(&b.label));
    let active_recipients: BTreeSet<Recipient> = membership
        .members()
        .values()
        .filter(|m| !log.recipient_is_removed(&m.binding.recipient()))
        .map(|m| m.binding.recipient())
        .collect();
    let notes = notes_of(log);
    let mut gone: Vec<ListedMac> = membership
        .removed()
        .iter()
        .filter_map(|(actor, removal)| {
            let (recipient, label, name) = seat_of(log, actor)?;
            if !removal.quorum
                && (active_recipients.contains(&recipient)
                    || membership.members().iter().any(|(active, m)| {
                        active[..32] == actor[..32]
                            && !log.recipient_is_removed(&m.binding.recipient())
                    }))
            {
                return None;
            }
            Some(ListedMac {
                label,
                name,
                admitted_by: None,
                admitted_ms: 0,
                code: join_code(&recipient),
                removed: notes.by_recipient.get(&recipient.to_bech32()).cloned(),
            })
        })
        .collect();
    gone.sort_by(|a, b| a.label.cmp(&b.label));
    out.extend(gone);
    Ok(Some(out))
}

#[cfg(test)]
#[path = "remove_tests.rs"]
mod tests;
