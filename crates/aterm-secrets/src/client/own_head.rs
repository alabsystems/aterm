// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The last version this Mac sealed that a server took, kept while it carries an approval the
//! servers lost (`docs/DESIGN-fluid-swarm-2026-10-28.md` §4.2, F14).
//!
//! Versions have no parents (F1), so at a partition heal every server takes the higher-seq side's
//! version and drops the lower side's. An approval an admin made on the lower side then goes with
//! it: a Mac let in by its code, by a password or by an Allow off the home LAN signs no `Admit`
//! (its join request has no Commons binding), so that approval lives only in the envelope, and no
//! merge of the servers' heads ever sees it. The Mac reads the higher version, finds itself out,
//! and the next mirror reseals the vault without it.
//!
//! The approving admin keeps the raw bytes of its own last version a server took ([`keep`]). Its
//! mirror reads that version back and finds the Macs it let in there that no newer server's
//! version names, that no server's version keeps on a `code-only` line, and that the log has not
//! removed or denied ([`judge`], [`lost_to`]). It merges the servers' heads alone and then adds
//! exactly those Macs to the merge ([`carry_lost`]): the machine, its roster row, its admission
//! evidence and its Commons grant. Nothing else of the older version comes back: not its
//! `code-only` lines, entries, deny rows, lease or policy, which the other side may since have
//! changed on purpose. Those Macs then pass the log's "left out on purpose" checks in that one
//! mirror (`readmit`), so the draft wraps the newest `[everyone]` and Commons epochs to them.
//!
//! A Mac it carried stays carried until every newest `[everyone]` epoch wraps it: a covering epoch
//! another member mints at the same time can still leave it out, and the next pass carries it
//! again ([`Paths::carried_file`]). The loss is judged against the servers' highest-seq versions,
//! the ones a server keeps, never a lagging server's older one that still lists the Mac. Once
//! every Mac this one let in holds a seat in the log (its `Admit` arrived) or was decided against
//! (a `code-only` line or a deny in a server's version, a removal or a deny in the log), the
//! version kept has done its work and is forgotten ([`judge`]). A newer version that merely lists
//! those Macs is not enough: another admin on the same side of a split can seal one, and it loses
//! to the other side at the heal as this one does, while its author carries nothing it did not
//! approve. A Mac that never takes a seat (a `0.145` joiner) keeps the version armed, so a person
//! who takes it out by a means that leaves no trace (a Commons retirement by its key, a revoke by
//! a `<= 0.144` admin) sees it carried back at the next heal that loses it.

use super::ledger_publish::{MergedHeads, ObservedHead};
use super::*;
use crate::membership_log::{Body, EpochAudience, Log};
use crate::swarm_roster::SwarmRoster;

/// The most the attempt mark ([`Paths::carry_tried_file`]) holds.
const MAX_TRIED: usize = 4096;
/// The most the record of Macs carried back ([`Paths::carried_file`]) holds: far more lines than
/// one heal ever carries.
const MAX_CARRIED: usize = 64 * 1024;

/// This Mac's own last version a server took, verified as any server's head is (the vault's
/// name, the pin, this Mac's admin locker, this Mac still an admin in the log): `None` when there
/// is none, or it fails any of that (another vault's file, a key rotated since).
pub(super) fn own_head(
    paths: &Paths,
    config: &Config,
    identity: &Identity,
) -> Option<ObservedHead> {
    let pin = config.signer?;
    let bytes = fsx::read_private(&paths.published_file(), envelope::MAX_ENVELOPE)
        .ok()
        .flatten()?;
    ledger_publish::decode_verified_head(
        paths,
        "published".to_owned(),
        bytes.as_slice().to_vec(),
        config,
        &pin,
        identity,
    )
    .ok()
}

/// Whether `log` holds an `[everyone]` epoch at all: with none, no Mac can be wrapped in one.
fn has_everyone_epochs(log: &Log) -> bool {
    log.records().skip(1).any(|record| {
        matches!(
            record.body(),
            Body::Epoch {
                audience: EpochAudience::Everyone,
                ..
            }
        )
    })
}

/// Whether `recipient` holds a seat in `log`.
fn log_member(log: &Log, recipient: &Recipient) -> bool {
    log.membership()
        .members()
        .values()
        .any(|member| member.binding.recipient() == *recipient)
}

/// The Macs `own` let in that the versions `served` lost: each machine `own` lists whose
/// recipient
///
/// - no `served` version keeps on a `code-only` line (a revoke, or a let-go, by any admin since
///   0.145), and no `served` version's roster denies;
/// - `log` neither removed nor denies;
/// - in a vault with Commons control, has its admission evidence in `own` naming `me` as its
///   author (an approval this Mac made, not one another admin made that a removal since dropped);
/// - and either no `served` version lists in `[machines]`, or it is one this Mac `carried` back,
///   holds no seat in `log`, and a newest `[everyone]` epoch there leaves it unwrapped (a covering
///   epoch minted without it while its carry was under way).
pub(super) fn lost_to(
    own: &ObservedHead,
    served: &[&Document],
    log: Option<&Log>,
    me: &Recipient,
    carried: &BTreeSet<Recipient>,
) -> BTreeSet<Recipient> {
    let listed: BTreeSet<String> = served
        .iter()
        .flat_map(|doc| doc.machines())
        .map(|machine| machine.recipient)
        .collect();
    let code_only: BTreeSet<String> = served.iter().flat_map(|doc| doc.code_only()).collect();
    let attributed = own.admissions().trusted();
    let everyone_epochs = log.is_some_and(has_everyone_epochs);
    own.doc
        .machines()
        .into_iter()
        .filter_map(|machine| {
            let recipient = Recipient::parse(&machine.recipient).ok()?;
            let kept = recipient != *me
                && !code_only.contains(&machine.recipient)
                && !served.iter().any(|doc| {
                    enrollment::machine_denied_in_doc(doc, &machine.name, &machine.recipient)
                })
                && log.is_none_or(|log| {
                    !log.recipient_is_removed(&recipient)
                        && !log_entries::recipient_denied_by_roster_or_log(None, log, &recipient)
                })
                && (!attributed
                    || own
                        .admissions()
                        .evidence(&recipient)
                        .is_some_and(|evidence| evidence.author == *me));
            let lost = !listed.contains(&machine.recipient)
                || (carried.contains(&recipient)
                    && log.is_some_and(|log| {
                        everyone_epochs
                            && !log_member(log, &recipient)
                            && !log.wraps_maximal_audience_to(&EpochAudience::Everyone, &recipient)
                    }));
            (kept && lost).then_some(recipient)
        })
        .collect()
}

/// The Macs this Mac's own last version `own` carries that the servers' versions `heads` lost
/// ([`lost_to`]): judged against the heads at the highest seq any server serves, other than `own`
/// itself, which are the versions a server keeps and a merge builds on. A lagging server's older
/// head (another admin's version from this side of a split, still listing the Mac) does not
/// count: it loses to the highest one as `own` did. Where `own` is the only head at that seq
/// (every server that serves the highest seq took it), it is judged against itself, so only a Mac
/// it `carried` that a newest `[everyone]` epoch leaves unwrapped counts. Where the highest seq is
/// below its own, `own` has reached no server yet and carries nothing.
pub(super) fn carrying(
    own: &ObservedHead,
    heads: &[ObservedHead],
    log: Option<&Log>,
    me: &Recipient,
    carried: &BTreeSet<Recipient>,
) -> BTreeSet<Recipient> {
    let own_digest = own.envelope.digest();
    let Some(top) = heads.iter().map(|head| head.envelope.seq).max() else {
        return BTreeSet::new();
    };
    if top < own.envelope.seq {
        return BTreeSet::new();
    }
    let kept: Vec<&Document> = heads
        .iter()
        .filter(|head| head.envelope.seq == top && head.envelope.digest() != own_digest)
        .map(|head| &head.doc)
        .collect();
    if kept.is_empty() {
        lost_to(own, &[&own.doc], log, me, carried)
    } else {
        lost_to(own, &kept, log, me, carried)
    }
}

/// Whether every Mac `own` let in (by this Mac, in a vault with Commons control) is settled: it
/// holds a seat in `log` (its `Admit` arrived, so its approval no longer lives in the envelope
/// alone), or a version in `heads` keeps it on a `code-only` line or denies it, or `log` removed
/// or denies it. Then `own` has nothing left to carry. A Mac a newer version merely lists is not
/// settled: another admin on this side of a split can seal that version, which loses to the other
/// side at the heal as `own` does, and its author carries no approval it did not make.
fn settled(own: &ObservedHead, heads: &[ObservedHead], log: Option<&Log>, me: &Recipient) -> bool {
    let code_only: BTreeSet<String> = heads.iter().flat_map(|head| head.doc.code_only()).collect();
    let attributed = own.admissions().trusted();
    own.doc.machines().into_iter().all(|machine| {
        let Ok(recipient) = Recipient::parse(&machine.recipient) else {
            return true;
        };
        let approved_here = recipient != *me
            && (!attributed
                || own
                    .admissions()
                    .evidence(&recipient)
                    .is_some_and(|evidence| evidence.author == *me));
        !approved_here
            || code_only.contains(&machine.recipient)
            || heads.iter().any(|head| {
                enrollment::machine_denied_in_doc(&head.doc, &machine.name, &machine.recipient)
            })
            || log.is_some_and(|log| {
                log_member(log, &recipient)
                    || log.recipient_is_removed(&recipient)
                    || log_entries::recipient_denied_by_roster_or_log(None, log, &recipient)
            })
    })
}

/// The Macs this Mac's own last version `own` carries back into a merge of `heads` under `log`
/// ([`carrying`]); where it carries none and every Mac it let in is settled ([`settled`]), it is
/// forgotten with the Macs it carried, so nothing a person does after is ever undone by it.
pub(super) fn judge(
    paths: &Paths,
    own: &ObservedHead,
    heads: &[ObservedHead],
    log: Option<&Log>,
    me: &Recipient,
) -> BTreeSet<Recipient> {
    let carried = carried(paths);
    let lost = carrying(own, heads, log, me, &carried);
    if lost.is_empty() && settled(own, heads, log, me) {
        forget(paths);
    }
    lost
}

/// The Macs this Mac carried back and may carry again ([`Paths::carried_file`]): one recipient a
/// line; a file that does not read is none.
fn carried(paths: &Paths) -> BTreeSet<Recipient> {
    let Ok(Some(bytes)) = fsx::read_private(&paths.carried_file(), MAX_CARRIED) else {
        return BTreeSet::new();
    };
    std::str::from_utf8(bytes.as_slice())
        .map(|text| {
            text.lines()
                .filter_map(|line| Recipient::parse(line.trim()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Add `readmit` to the Macs this Mac carried back ([`carried`]), once a server took the version
/// that carried them. Best effort.
pub(super) fn note_carried(paths: &Paths, readmit: &BTreeSet<Recipient>) {
    let mut all = carried(paths);
    all.extend(readmit.iter().copied());
    let text: String = all
        .iter()
        .map(|recipient| format!("{}\n", recipient.to_bech32()))
        .collect();
    if text.len() <= MAX_CARRIED {
        let _ = fsx::write_atomic(&paths.carried_file(), text.as_bytes(), 0o600);
    }
}

/// Forget this Mac's own last version, the Macs it carried, and the attempt mark.
fn forget(paths: &Paths) {
    for file in [
        paths.published_file(),
        paths.carried_file(),
        paths.carry_tried_file(),
    ] {
        let _ = std::fs::remove_file(file);
    }
}

/// Add the Macs `readmit` names that `merged` lacks back into it from `own`, and nothing else: the
/// machine (under its name there, or the next free one), its roster row ([`carry_rows`]), its
/// admission evidence, and its Commons grant where `own` granted it. Returns whether `merged`
/// changed. A Mac `merged` already lists keeps its entry there.
///
/// # Errors
/// A name, roster or admission cannot be written into the merged document, or `own`'s roster
/// does not parse.
pub(super) fn carry_lost(
    merged: &mut MergedHeads,
    own: &ObservedHead,
    readmit: &BTreeSet<Recipient>,
) -> crate::Result<bool> {
    let mut doc = merged.doc.clone();
    let mut admissions = merged.admissions.clone();
    let eligibility = crate::commons::membership::Eligibility::from_document(&own.doc);
    let mut granted = Vec::new();
    let mut changed = false;
    for recipient in readmit {
        let text = recipient.to_bech32();
        if doc.machines().iter().any(|m| m.recipient == text) {
            continue;
        }
        let Some(machine) = own.doc.machines().into_iter().find(|m| m.recipient == text) else {
            continue;
        };
        let name = free_name(&doc, &machine.name);
        doc.add_machine(&name, &text)
            .map_err(|e| Error::new(e.to_string()))?;
        if let Some(evidence) = own.admissions().evidence(recipient) {
            admissions.record(&doc, &text, evidence)?;
            if eligibility.includes(&machine.name, *recipient) {
                granted.push((text, evidence.method));
            }
        }
        changed = true;
    }
    if !changed {
        return Ok(false);
    }
    carry_rows(&mut doc, own, readmit)?;
    if let Some(shared) = enrollment::include_approved(&doc, &granted)? {
        doc = shared;
    }
    merged.doc = doc;
    merged.admissions = admissions.retained(&merged.doc)?;
    merged.diverged = true;
    Ok(true)
}

/// Give each Mac `readmit` names that `doc` lists, and whose row `doc`'s swarm roster lacks, its row
/// from `own` again ([`carry_roster_row`]): into the merge, and again after the mirror's overlay
/// writes the log's roster over it (the side that dropped the approval may have written it last,
/// with no row for that Mac), before the merge of heads would give the Mac a fresh row as it does
/// any Mac it admits; `readmit` keeps the row from being pruned as left out. Returns whether `doc`
/// changed.
///
/// # Errors
/// The roster in `own` or `doc` does not parse, or cannot be written back.
pub(super) fn carry_rows(
    doc: &mut Document,
    own: &ObservedHead,
    readmit: &BTreeSet<Recipient>,
) -> crate::Result<bool> {
    let own_roster = SwarmRoster::from_document(&own.doc)?;
    let Some(mut roster) = SwarmRoster::from_document(doc)? else {
        return Ok(false);
    };
    let before = roster.to_toml();
    for machine in doc.machines() {
        if Recipient::parse(&machine.recipient).is_ok_and(|r| readmit.contains(&r)) {
            carry_roster_row(
                &mut roster,
                doc,
                own_roster.as_ref(),
                &machine.recipient,
                &machine.name,
            );
        }
    }
    let after = roster.to_toml();
    if after == before {
        return Ok(false);
    }
    doc.set_entry(
        &Audience::Everyone,
        crate::SWARM_ROSTER_ENTRY,
        after.as_bytes(),
    )
    .map_err(|e| Error::new(e.to_string()))?;
    Ok(true)
}

/// Put `recipient`'s historical row from `own_roster` into `roster` under the name `name` where
/// no deny list names it or its node. No new label is assigned:
/// only the immutable historical evidence is carried until the denial migration.
fn carry_roster_row(
    roster: &mut SwarmRoster,
    _doc: &Document,
    own_roster: Option<&SwarmRoster>,
    recipient: &str,
    name: &str,
) {
    if roster.member_by_recipient(recipient).is_some() {
        return;
    }
    let row = own_roster.and_then(|own| own.member_by_recipient(recipient).cloned());
    if let Some(mut row) = row {
        let not_denied = !roster.is_label_denied(&row.label)
            && row
                .aliases
                .iter()
                .all(|alias| !roster.is_label_denied(alias))
            && !row
                .node_id
                .iter()
                .chain(row.node_key.iter())
                .any(|node| roster.is_node_denied(node));
        if not_denied {
            row.vault_name = name.to_owned();
            roster.members.push(row);
            roster.canonicalize();
        }
    }
}

/// Keep `sealed`, a version this Mac sealed that a server took, as its own last version, unless
/// the one kept still carries a Mac `sealed` lost ([`lost_to`] against `sealed_doc`, under `log`):
/// an unrelated edit made on the higher side's version must not drop the approval its carry is
/// bringing back. Best effort: a failed write keeps what was there.
pub(super) fn keep(
    paths: &Paths,
    config: &Config,
    identity: &Identity,
    sealed: &[u8],
    sealed_doc: &Document,
    log: Option<&Log>,
) {
    if let Some(kept) = own_head(paths, config, identity)
        && kept.raw.as_slice() != sealed
        && !lost_to(
            &kept,
            &[sealed_doc],
            log,
            &identity.recipient(),
            &carried(paths),
        )
        .is_empty()
    {
        return;
    }
    let _ = fsx::write_atomic(&paths.published_file(), sealed, 0o600);
}

/// The attempt mark: this Mac's own version, the served version and the records it last carried
/// from, so a carry that cannot complete (a head whose records are missing, every server refusing,
/// this Mac left out) is not tried again on every pass until one of them changes.
fn tried_mark(own: &[u8; 32], served: &[u8; 32], records: Option<&[u8; 32]>) -> String {
    format!(
        "{} {} {}\n",
        aterm_codec::hex::encode_lower(own),
        aterm_codec::hex::encode_lower(served),
        records.map_or_else(|| "-".to_owned(), |d| aterm_codec::hex::encode_lower(d)),
    )
}

/// Whether a vault pass should run the mirror for this Mac's own last version: it is not the
/// version `served` (the bytes the pass read, or the copy it applied), it carries a Mac `served`
/// lost ([`judge`]), and no look at the same three digests (its own version, `served`, and
/// `records`, the records it holds) found nothing to carry or ran a carry already ([`tried`]).
/// Equal bytes stop at once; a look costs two decryptions and a read of the log.
pub(super) fn due(
    paths: &Paths,
    config: &Config,
    served: &[u8],
    records: Option<&[u8; 32]>,
) -> bool {
    let Some(mark) = mark(paths, served, records) else {
        return false;
    };
    if fsx::read_private(&paths.carry_tried_file(), MAX_TRIED)
        .ok()
        .flatten()
        .is_some_and(|held| held.as_slice() == mark.as_bytes())
    {
        return false;
    }
    let (Ok(Some(identity)), Some(pin)) = (read_identity(paths), config.signer) else {
        return false;
    };
    let Some(own) = own_head(paths, config, &identity) else {
        return false;
    };
    let Ok(head) = ledger_publish::decode_verified_head(
        paths,
        "served".to_owned(),
        served.to_vec(),
        config,
        &pin,
        &identity,
    ) else {
        return false;
    };
    let log = log_entries::trusted_seeded_log(paths, config, &head.envelope)
        .ok()
        .flatten()
        .map(|(log, _)| log);
    let lost = judge(
        paths,
        &own,
        std::slice::from_ref(&head),
        log.as_ref(),
        &identity.recipient(),
    );
    if lost.is_empty() {
        tried(paths, served, records);
    }
    !lost.is_empty()
}

/// The attempt mark of this Mac's own last version against `served` and `records`, where it
/// keeps one other than `served`.
fn mark(paths: &Paths, served: &[u8], records: Option<&[u8; 32]>) -> Option<String> {
    let own = fsx::read_private(&paths.published_file(), envelope::MAX_ENVELOPE)
        .ok()
        .flatten()?;
    if own.as_slice() == served {
        return None;
    }
    let own_env = envelope::parse(own.as_slice()).ok()?;
    let served_env = envelope::parse(served).ok()?;
    Some(tried_mark(&own_env.digest(), &served_env.digest(), records))
}

/// Record that a pass looked at, or ran the mirror for, this Mac's own last version against
/// `served` and `records` (the records it holds after the mirror's own exchange), so the same
/// three digests are not looked at again ([`due`]). Best effort.
pub(super) fn tried(paths: &Paths, served: &[u8], records: Option<&[u8; 32]>) {
    if let Some(mark) = mark(paths, served, records) {
        let _ = fsx::write_atomic(&paths.carry_tried_file(), mark.as_bytes(), 0o600);
    }
}
