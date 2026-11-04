// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Any member admits (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.3.6, F15).
//!
//! Where this Mac's vault has a membership log ([`log_entries::trusted_seeded_store`]: a genesis
//! its pin trusts, holding epochs) and this Mac holds a seat in it and the keys of every epoch a
//! new Mac needs, a person's Allow and the phrase let the new Mac in with no admin:
//!
//! - [`hand_over`] signs the new Mac's `Admit` (its label the next free `m<N>`, C11) and a `Wrap`
//!   of every `[everyone]` and Commons epoch that still seals a current entry, keeps them in this
//!   Mac's records, and hands the whole record store in the bundle
//!   ([`members::bundle::Handed::Log`]). The caller posts the records to every server it reaches
//!   ([`post_records_to_servers`]).
//! - [`write_from_log_bundle`] is the new Mac's side: it checks the path from the genesis its pin
//!   trusts to the effective `Admit` that names its own key, opens its files from the log, and
//!   writes `~/.secrets`.
//!
//! Everywhere else (a vault with no log, ALab until F12b; a member with no seat; a new Mac that
//! does not read the log bundle) [`hand_over`] falls back to F10's interim: the member's
//! `[everyone]` files ([`handover_bundle`]) and its `Allowed`, which an admin's approver turns into
//! an admission. The `Allowed` is still signed and posted on the log path: an admin's approver
//! is still what names a Mac in `[machines]`, the envelope and `aterm-swarm-roster` that 0.145
//! Macs read, but no longer what lets it read the vault, Commons control (F18 enrolls by the
//! `Admit`) or the fleet (F17 admits by the log).
//!
//! Where the member holds a log, neither path lets in a Mac the log removed or a Mac its newest
//! `[everyone]` epoch left out on purpose ([`ByLog::KeptOut`]): the Allow fails with the reason,
//! until F16's "removed, Allow again?" row asks the person that question.

use super::*;
use crate::commons::binding::Request;
use crate::members::bundle::{Handed, Reader};
use crate::members::desk::{Handing, Handout};
use crate::members::{Allowed, Bundle};
use crate::membership_log::{
    self, Entrant, EpochAudience, Log, OpenedAudienceEntries, Record, RecordId, Signer,
    admission_audiences, encode_records,
};

/// What the membership log says of an Allow ([`admit_by_phrase`]).
#[derive(Debug)]
pub enum ByLog {
    /// This Mac let the new Mac in through the log.
    Admitted(LogAdmission),
    /// The log path does not apply here: no trusted log with epochs, no seat for this Mac's
    /// device key in it, or an `[everyone]` epoch it cannot hand on. F10's interim hands the
    /// bundle.
    NotHere,
    /// A vault pass on this Mac held its state past [`super::HANDOVER_LOCK_WAIT`]. The run ends
    /// saying to Allow again, as F10's handover does: falling back would wait as long again, and
    /// the whole `ask` exchange runs inside the fleet listener's handshake watchdog.
    Busy,
    /// The log removed the new Mac (its actor, or the age recipient it asks with), or a causally
    /// newest `[everyone]` epoch left it out on purpose ([`Log::left_out`]): no Allow lets it in on
    /// either path, since F10's would hand it the current files. The reason, for the person.
    KeptOut(String),
}

/// What this Mac signed to let a new Mac in on the log path ([`admit_by_phrase`]).
#[derive(Debug)]
pub struct LogAdmission {
    /// The capacity and crossed state captured with this exact records snapshot.
    pub source: crate::crossing::RecordsBound,
    /// The `Admit` of the new Mac: the one this Mac signed, or the one the log already held.
    pub admit: RecordId,
    /// The label the log gives the new Mac.
    pub label: String,
    /// The records this call signed (`Admit` first, then `Wrap`s), possibly none.
    pub signed: Vec<Record>,
    /// This Mac's complete record history afterwards, genesis first. Waiting records stay local.
    pub records: Vec<u8>,
}

/// What C11 keeps a newcomer's label off, from the entries this member reads (`by_audience`,
/// every audience it opens): the labels it may not take (every label, alias, claim and
/// deny-listed label in `aterm-swarm-roster`, each row's `[machines]` name with a leading `m<N>`,
/// and the reserved ones: `m3`, the policy's reserved labels, release hosts and stewards, from
/// whichever audience holds `aterm-swarm-policy`), and the labels it only counts toward "one above
/// the highest" (each `m<N>` a fleet-accounts sheet names, as the approver's `next_free_number`
/// counts them).
pub(super) fn labels_taken(
    by_audience: &BTreeMap<Audience, BTreeMap<String, Entry>>,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut taken =
        BTreeSet::from([crate::swarm_roster::DEFAULT_RESERVED_RELEASE_LABEL.to_owned()]);
    let mut counted = BTreeSet::new();
    let canonical = |raw: &str| crate::swarm_roster::leading_m_label(raw).map(|(_, label)| label);
    let entries = || by_audience.values().flat_map(BTreeMap::values);
    for entry in entries() {
        let Ok(text) = std::str::from_utf8(entry.value.as_slice()) else {
            continue;
        };
        let base = entry
            .name
            .strip_prefix("~/.secrets/")
            .unwrap_or(&entry.name);
        if entry.name == crate::SWARM_ROSTER_ENTRY
            && let Ok(roster) = crate::swarm_roster::SwarmRoster::from_toml(text)
        {
            for member in &roster.members {
                taken.insert(member.label.clone());
                taken.extend(canonical(&member.label));
                taken.extend(member.aliases.iter().cloned());
                taken.extend(member.aliases.iter().filter_map(|a| canonical(a)));
                if !roster.is_member_denied(member) {
                    if let Some(claim) = member.claimed_label.as_deref().filter(|c| {
                        crate::swarm_roster::leading_m_label(c)
                            .is_some_and(|(_, canon)| canon == *c)
                    }) {
                        taken.insert(claim.to_owned());
                    }
                    taken.extend(canonical(&member.vault_name));
                }
            }
            taken.extend(roster.deny_labels.iter().filter_map(|raw| canonical(raw)));
        } else if entry.name == crate::SWARM_POLICY_ENTRY
            && let Ok(policy) = crate::swarm_roster::SwarmPolicy::from_toml(text)
        {
            for raw in policy
                .reserved_labels
                .iter()
                .chain(policy.release_hosts.iter())
                .chain(policy.stewards.iter().map(|steward| &steward.label))
            {
                taken.extend(canonical(raw));
            }
        } else if base == "fleet-accounts.toml"
            || (base.starts_with("fleet-accounts-") && base.ends_with(".toml"))
        {
            let sheet = crate::swarm_roster::SheetHints::from_fleet_toml(text);
            counted.extend(sheet.numbers.iter().map(|n| format!("m{n}")));
        }
    }
    (taken, counted)
}

/// Why the log keeps the Mac `request` names out of an Allow, read by the member that opened
/// `opened`: the log removed its actor, the age recipient it asks with (a new device key with a
/// removed Mac's machine key is the same Mac) or its writer key under another incarnation, or it
/// is a member that a causally newest `[everyone]` epoch left out on purpose (its minter knew the
/// Mac and wrapped nothing to it: an admin's revoke rotation), or its key is bound to another
/// seat. F16's restore asks the person first ("removed, Allow again?"): with `restore` (they
/// pressed it) a plain removal keeps nothing out, and the run signs a restoring `Admit`. A
/// quorum's removal keeps the Mac out whatever one person presses.
pub(super) fn kept_out(
    log: &Log,
    binding: &crate::commons::binding::Binding,
    restore: bool,
) -> Option<String> {
    kept_out_with_node(log, binding, [0; 32], restore)
}

/// [`kept_out`] of a Mac that asks with the node key `node` (`[0; 32]`: none): a node key the log
/// removed is the removed Mac too, and one bound to another seat is refused.
pub(super) fn kept_out_with_node(
    log: &Log,
    binding: &crate::commons::binding::Binding,
    node: [u8; 32],
    restore: bool,
) -> Option<String> {
    kept_out_with_node_at(log, binding, node, restore, true)
}

fn kept_out_with_node_at(
    log: &Log,
    binding: &crate::commons::binding::Binding,
    node: [u8; 32],
    restore: bool,
    check_epoch_omission: bool,
) -> Option<String> {
    let member = binding.actor();
    let recipient = binding.recipient();
    let membership = log.membership();
    let removal = removal_naming(log, binding);
    let node_rem = match (
        node_removal(log, node),
        node_removal(log, *binding.writer()),
    ) {
        (Some(a), Some(b)) => Some(a || b),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };
    if removal.is_some() || log.recipient_is_removed(&recipient) || node_rem.is_some() {
        let quorum = removal == Some(true)
            || node_rem == Some(true)
            || super::remove::notes_of(log)
                .of_with_node(&recipient, binding.writer(), &node)
                .is_some_and(|note| note.quorum);
        if quorum {
            return Some(
                "a quorum of members removed this Mac, and one person's Allow does not bring it \
                 back"
                    .to_owned(),
            );
        }
        if !restore {
            return Some(
                "the vault removed this Mac: \"Allow again\" restores it, a plain Allow does not"
                    .to_owned(),
            );
        }
        // "Allow again" (F16): a person restores what one member's removal took out, unless
        // the presented key is already bound to a different active (non-removed) seat.
        return membership
            .members()
            .iter()
            .any(|(actor, m)| {
                !log.recipient_is_removed(&m.binding.recipient())
                    && !m.node_key.is_some_and(|nk| node_removal(log, nk).is_some())
                    && (((*actor == member || m.binding.recipient() == recipient)
                        && m.binding != *binding)
                        || (node != [0; 32]
                            && m.node_key == Some(node)
                            && (*actor != member || m.binding != *binding)))
            })
            .then(|| {
                "this Mac's key is already bound to a different seat in the membership log"
                    .to_owned()
            });
    }
    let active_node_holder = node != [0; 32]
        && membership.members().iter().any(|(actor, m)| {
            m.node_key == Some(node)
                && !log.audience_left_out(&EpochAudience::Everyone, actor)
                && !log.recipient_is_removed(&m.binding.recipient())
                && node_removal(log, node).is_none()
        });
    if check_epoch_omission
        && (membership.members().iter().any(|(actor, m)| {
            (*actor == member
                || (!active_node_holder && node != [0; 32] && m.node_key == Some(node)))
                && log.audience_left_out(&EpochAudience::Everyone, actor)
        }) || log.audience_left_out_recipient(&EpochAudience::Everyone, &recipient))
    {
        return Some(
            "the vault's newest key leaves this Mac out on purpose, and an Allow does not let it \
             back in"
                .to_owned(),
        );
    }
    membership
        .members()
        .iter()
        .any(|(actor, m)| {
            !log.recipient_is_removed(&m.binding.recipient())
                && !m.node_key.is_some_and(|nk| node_removal(log, nk).is_some())
                && !log.audience_left_out(&EpochAudience::Everyone, actor)
                && (((*actor == member || m.binding.recipient() == recipient)
                    && m.binding != *binding)
                    || (node != [0; 32]
                        && m.node_key == Some(node)
                        && (*actor != member || m.binding != *binding)))
        })
        .then(|| {
            "this Mac's key is already bound to a different seat in the membership log".to_owned()
        })
}

/// Whether the log removed the Mac `binding` names, by its actor or its device's writer key under
/// another incarnation (each is the Mac a removal took out), and if so whether a quorum did.
pub(super) fn removal_naming(
    log: &Log,
    binding: &crate::commons::binding::Binding,
) -> Option<bool> {
    let member = binding.actor();
    let writer = binding.writer();
    let membership = log.membership();
    let active_writer = membership
        .members()
        .keys()
        .any(|actor| actor[..32] == writer[..]);
    let mut removals = membership
        .removed()
        .iter()
        .filter(|(actor, removal)| {
            **actor == member || (actor[..32] == writer[..] && (removal.quorum || !active_writer))
        })
        .map(|(_, removal)| removal.quorum)
        .peekable();
    removals.peek()?;
    Some(removals.any(|quorum| quorum))
}

/// Whether the log removed the node key `node` (`[0; 32]`: none), and if so whether a quorum did.
pub(super) fn node_removal(log: &Log, node: [u8; 32]) -> Option<bool> {
    if node == [0; 32] {
        return None;
    }
    let membership = log.membership();
    let Some(crate::membership_log::NodeStanding::Removed(actor)) = membership.nodes().get(&node)
    else {
        return None;
    };
    Some(membership.removed().get(actor).is_some_and(|r| r.quorum))
}

/// Whether "Allow again" restores the Mac `binding` and `node` name: the log removed it (not by a
/// quorum, which [`kept_out_with_node`] refuses first), so the roster's deny rows naming it are
/// its removal's, which the restoring `Admit` lifts (`roster::restored_by_log`).
fn restoring(
    log: &Log,
    binding: &crate::commons::binding::Binding,
    node: [u8; 32],
    restore: bool,
) -> bool {
    restore
        && (removal_naming(log, binding).is_some()
            || log.recipient_is_removed(&binding.recipient())
            || node_removal(log, node).is_some()
            || node_removal(log, *binding.writer()).is_some())
}

pub(super) fn roster_keeps_out(
    paths: &Paths,
    opened: Option<&OpenedAudienceEntries>,
    binding: &crate::commons::binding::Binding,
    node: [u8; 32],
    lock: Option<&StateLock>,
) -> crate::Result<Option<String>> {
    let hex_node = (node != [0; 32]).then(|| aterm_codec::hex::encode_lower(&node));
    let hex_writer = aterm_codec::hex::encode_lower(binding.writer());
    let hex_incarnation = aterm_codec::hex::encode_lower(binding.incarnation());
    let hex_machine = aterm_codec::hex::encode_lower(binding.machine());
    let rec_str = binding.recipient().to_bech32();
    let check_roster = |r: &crate::swarm_roster::SwarmRoster| {
        hex_node.as_deref().is_some_and(|h| r.is_node_denied(h))
            || r.is_node_denied(&hex_writer)
            || r.is_node_denied(&hex_incarnation)
            || r.is_node_denied(&hex_machine)
            || r.member_by_recipient(&rec_str)
                .is_some_and(|m| steward::member_is_denied_in_roster(r, m))
    };
    let roster = match lock {
        Some(lock) => roster::swarm_roster_for_denial_held(paths, lock),
        None => roster::swarm_roster_for_denial(paths),
    }?;
    let local_log = exclusion_log(paths)?.map(|(log, _)| log);
    let denied = local_log.as_ref().is_some_and(|log| {
        removal_naming(log, binding).is_some()
            || log.recipient_is_removed(&binding.recipient())
            || node_removal(log, node).is_some()
            || node_removal(log, *binding.writer()).is_some()
    }) || roster.as_ref().is_some_and(check_roster)
        || log_entries::roster_from_doc_or_opened(None, opened, local_log.as_ref()).is_some_and(
            |mut r| {
                if let Ok(Some(denied_nodes)) = swarm_denied_nodes(paths) {
                    r.deny_nodes.extend(denied_nodes);
                    r.canonicalize();
                }
                check_roster(&r)
            },
        );
    Ok(denied.then(|| {
        "the vault removed this Mac, and an Allow does not let a removed Mac back in".to_owned()
    }))
}

/// Epoch omissions need a current log; explicit signed removals remain evidence behind a cache.
fn exclusion_log(paths: &Paths) -> crate::Result<Option<(Log, bool)>> {
    let Some(config) = config(paths)? else {
        return Ok(None);
    };
    let state = load_state(paths)?;
    let Some((log, _)) = log_entries::authenticated_custody(paths, &config, &state)? else {
        return Ok(None);
    };
    let current = match (
        read_identity(paths)?,
        read_verified_cached_envelope(paths, &config, &state)?,
    ) {
        (Some(identity), Some(bytes)) => {
            log_entries::log_base(Some(&log), &envelope::parse(&bytes)?, &identity)?
                != log_entries::LogBase::Behind
        }
        _ => true,
    };
    Ok(Some((log, current)))
}

fn trusted_log_keeps_out(
    paths: &Paths,
    binding: &crate::commons::binding::Binding,
    node: [u8; 32],
    restore: bool,
    lock: Option<&StateLock>,
) -> crate::Result<Option<String>> {
    let evidence = exclusion_log(paths)?;
    if !evidence
        .as_ref()
        .is_some_and(|(log, _)| restoring(log, binding, node, restore))
        && let Some(why) = roster_keeps_out(paths, None, binding, node, lock)?
    {
        return Ok(Some(why));
    }
    let Some((log, current)) = evidence else {
        return Ok(None);
    };
    Ok(kept_out_with_node_at(&log, binding, node, restore, current))
}

/// The log, keys and opened entries the member at `paths` admits from: its vault's log where the
/// pin trusts it and it holds epochs, and what this Mac's identity opens of it. `None` where it
/// holds no such log or opens nothing of it.
pub(super) fn held_log(
    paths: &Paths,
) -> crate::Result<Option<(membership_log::Store, Log, Identity, OpenedAudienceEntries)>> {
    let Some(config) = config(paths)? else {
        return Ok(None);
    };
    let Some(pin) = config.signer else {
        return Ok(None);
    };
    let Some((store, _)) = log_entries::trusted_seeded_store(paths, &config, &pin, &[])? else {
        return Ok(None);
    };
    let Some(log) = store.log().cloned() else {
        return Ok(None);
    };
    let Some(identity) = read_identity(paths)? else {
        return Ok(None);
    };
    let Some(opened) = log.open_audience_entries(&identity)? else {
        return Ok(None);
    };
    Ok(Some((store, log, identity, opened)))
}

/// Let the Mac `request` names in through the membership log, as the member at `paths`, after a
/// person's Allow and the phrase: sign its `Admit` under the next free label and a `Wrap` of
/// every `[everyone]` and Commons epoch that still seals a current entry, and keep them in this
/// Mac's records. A Mac the log already lets in gets only the `Wrap`s it lacks, under its first
/// `Admit`. [`ByLog::KeptOut`] where the log removed the Mac or left it out on purpose
/// ([`kept_out`]); [`ByLog::NotHere`] where the log path does not apply here: no trusted log with
/// epochs, no seat for this Mac's device key in it, no `[everyone]` epoch this Mac can open, or
/// one it cannot open that is no wrap gap of its own ([`Log::is_wrap_gap`]; a gap of its own the
/// same holders close for the new Mac too).
///
/// # Errors
/// Local state cannot be read or written, or signing fails.
pub fn admit_by_phrase(
    paths: &Paths,
    request: &Request,
    node: [u8; 32],
    name: &str,
    now_ms: u64,
) -> crate::Result<ByLog> {
    log_path(paths, request, node, name, now_ms, (None, false), None)
}

/// [`admit_by_phrase`] for a local request or a log reader; a files reader asks only whether
/// the log keeps the Mac out, read
/// without the state lock (the records file is written whole), for a new Mac that takes F10's
/// files: [`ByLog::KeptOut`] or [`ByLog::NotHere`]. Signing takes the state lock, unless `held`
/// holds it already for this run (a member's desk took it before the run touched its phrase).
fn log_path(
    paths: &Paths,
    request: &Request,
    node: [u8; 32],
    name: &str,
    now_ms: u64,
    (reader, restore): (Option<Reader>, bool),
    held: Option<&super::HandoverHold>,
) -> crate::Result<ByLog> {
    let held = held.filter(|held| held.holds(paths));
    if reader.is_some_and(|reader| !reader.reads_log()) {
        return Ok(trusted_log_keeps_out(
            paths,
            request.binding(),
            node,
            restore,
            held.map(super::HandoverHold::state_lock),
        )?
        .map_or(ByLog::NotHere, ByLog::KeptOut));
    }
    let owned_lock = match held {
        Some(_) => None,
        None => match StateLock::try_take_until(paths, Instant::now() + super::HANDOVER_LOCK_WAIT)?
        {
            Some(lock) => Some(lock),
            None => return Ok(ByLog::Busy),
        },
    };
    let lock = held
        .map(super::HandoverHold::state_lock)
        .or(owned_lock.as_ref())
        .ok_or_else(|| Error::new("the admission has no held vault state"))?;
    let Some((mut store, mut log, _, opened)) = held_log(paths)? else {
        return Ok(
            trusted_log_keeps_out(paths, request.binding(), node, restore, Some(lock))?
                .map_or(ByLog::NotHere, ByLog::KeptOut),
        );
    };
    let source = records_bound(paths, &store)?;
    if let Some(reader) = reader.filter(|reader| !reader.can_take(source)) {
        return Err(Error::new(reader.refusal(source)));
    }
    if let Some(why) = kept_out_with_node(&log, request.binding(), node, restore) {
        return Ok(ByLog::KeptOut(why));
    }
    if !restoring(&log, request.binding(), node, restore)
        && let Some(why) =
            roster_keeps_out(paths, Some(&opened), request.binding(), node, Some(lock))?
    {
        return Ok(ByLog::KeptOut(why));
    }
    if let Some(already) = log.membership().get(&request.binding().actor())
        && (log_entries::local_swarm_roster_for_log_held(paths, &log, lock)?
            .is_some_and(|r| log_entries::member_denied_by_roster(&r, already))
            || log_entries::roster_from_doc_or_opened(None, Some(&opened), Some(&log))
                .is_some_and(|r| log_entries::member_denied_by_roster(&r, already)))
    {
        return Ok(ByLog::KeptOut(
            "the vault removed this Mac, and an Allow does not let a removed Mac back in"
                .to_owned(),
        ));
    }
    let Some((actor, device, incarnation)) =
        log_entries::open_local_member_signer_held(paths, &log, lock)?
    else {
        return Ok(ByLog::NotHere);
    };
    let Some(audiences) = held_audiences(&log, &opened, &actor) else {
        return Ok(ByLog::NotHere);
    };

    let member = request.binding().actor();
    let recipient = request.binding().recipient();
    let membership = log.membership();
    let signed = device.with_seed(|seed| -> crate::Result<(RecordId, String, Vec<Record>)> {
        let signer = Signer {
            actor: &actor,
            seed,
            incarnation,
        };
        if let Some(already) = membership.get(&member) {
            // Let in already (a run whose bundle never arrived, or another member's Admit, which
            // the new Mac takes as it would this Mac's): only the Wraps it lacks.
            let signed = wraps_lacking(&mut log, &opened, &audiences, &signer, &member, now_ms)?;
            return Ok((already.entry, already.label.clone(), signed));
        }
        // A Mac "Allow again" restores keeps the label it had, where no member holds it now.
        let restored = restore
            .then(|| super::remove::notes_of(&log))
            .and_then(|notes| {
                notes
                    .of_with_node(&recipient, request.binding().writer(), &node)
                    .map(|note| note.label.clone())
            })
            .filter(|label| {
                let label_canon =
                    crate::swarm_roster::leading_m_label(label).map(|(_, canon)| canon);
                let matches_label = |other: &str| {
                    other == label
                        || crate::swarm_roster::leading_m_label(other).is_some_and(|(_, canon)| {
                            canon == *label || label_canon.as_deref() == Some(canon.as_str())
                        })
                };
                membership_log::is_label(label)
                    && !membership.members().values().any(|m| {
                        m.binding.recipient() != recipient
                            && !log.recipient_is_removed(&m.binding.recipient())
                            && !m
                                .node_key
                                .is_some_and(|nk| node_removal(&log, nk).is_some())
                            && (node == [0; 32] || m.node_key != Some(node))
                            && (matches_label(&m.label) || matches_label(&m.name))
                    })
                    && !membership.unproved().iter().any(|f| {
                        f.recipient != recipient
                            && !log.recipient_is_removed(&f.recipient)
                            && (node == [0; 32] || f.node_key != Some(node))
                            && (matches_label(&f.label) || matches_label(&f.name))
                    })
            });
        let (taken, counted) = labels_taken(&opened.by_audience);
        let label = match restored {
            Some(label) => label,
            None => log
                .next_label(name, &taken, &counted)
                .ok_or_else(|| Error::new("no m<N> label up to m99999 is free"))?,
        };
        let entrant = Entrant {
            request: request.clone(),
            node_key: node,
            name: name.to_owned(),
            label: label.clone(),
        };
        let signed = log.admit(&signer, entrant, &opened, &audiences, now_ms)?;
        Ok((*signed[0].id(), label, signed))
    })?;
    let (admit, label, signed) = signed;
    if !signed.is_empty() {
        store.ingest(&encode_records(&signed))?;
        if signed
            .iter()
            .any(|record| store.get_bytes(record.id()).is_none())
        {
            return Err(Error::new("the admission cannot be retained in full"));
        }
    }
    let source = records_bound(paths, &store)?;
    if let Some(reader) = reader.filter(|reader| !reader.can_take(source)) {
        return Err(Error::new(reader.refusal(source)));
    }
    if !signed.is_empty() {
        super::write_records(paths, &store)?;
    }
    Ok(ByLog::Admitted(LogAdmission {
        source,
        admit,
        label,
        signed,
        records: store.encode_held(),
    }))
}

/// The audiences the member `actor`, which opened `opened` of `log`, hands a new Mac: `[everyone]`
/// and Commons where it holds every epoch of them a new Mac needs. An epoch it lacks as a wrap
/// gap of its own (admitted on one side of a split while the other minted it, before a holder
/// wrapped it to this Mac) is no reason to hand F10's files: the new Mac takes the log with the
/// epochs this Mac holds, and the same holders close that epoch for both ([`close_wrap_gaps`]).
/// A Commons epoch it cannot open stays closed to the new Mac, and a member that holds it closes
/// that gap. `None` where it cannot hand `[everyone]` at all: no epoch of it opened here, or one
/// missing that is no wrap gap of its own.
pub(super) fn held_audiences(
    log: &Log,
    opened: &OpenedAudienceEntries,
    actor: &crate::membership_log::MemberId,
) -> Option<Vec<EpochAudience>> {
    let holds = |audience: &EpochAudience| {
        Log::admission_epochs(opened, std::slice::from_ref(audience))
            .iter()
            .all(|(_, epoch)| {
                opened.unwrapped_keys.contains_key(epoch) || log.is_wrap_gap(opened, epoch, actor)
            })
    };
    if !holds(&EpochAudience::Everyone)
        || !Log::admission_epochs(opened, &[EpochAudience::Everyone])
            .iter()
            .any(|(_, epoch)| opened.unwrapped_keys.contains_key(epoch))
    {
        return None;
    }
    let mut audiences = admission_audiences(opened);
    audiences.retain(|audience| holds(audience));
    Some(audiences)
}

/// The `Wrap`s of `audiences`' epochs that the member `member`, let in already, lacks, signed by
/// `signer` into `log`: none of an epoch that left it out on purpose ([`Log::left_out`]), and
/// none of one this Mac lacks as its own wrap gap, which a holder closes for both.
///
/// # Errors
/// A `Wrap` fails to draft, sign or insert.
pub(super) fn wraps_lacking(
    log: &mut Log,
    opened: &OpenedAudienceEntries,
    audiences: &[EpochAudience],
    signer: &Signer<'_>,
    member: &crate::membership_log::MemberId,
    now_ms: u64,
) -> crate::Result<Vec<Record>> {
    let Some(recipient) = log.membership().get(member).map(|m| m.binding.recipient()) else {
        return Ok(Vec::new());
    };
    let mut signed = Vec::new();
    for (audience, epoch) in Log::admission_epochs(opened, audiences) {
        if log.left_out(opened, &audience, member) {
            continue;
        }
        if opened
            .wrapped_recipients
            .get(&epoch)
            .is_some_and(|wrapped| wrapped.contains(&recipient))
        {
            continue;
        }
        let Some(key) = opened.unwrapped_keys.get(&epoch) else {
            continue;
        };
        let wrap = log
            .draft_wrap(signer.actor, now_ms, epoch, key, recipient)?
            .sign(signer.seed, signer.incarnation)?;
        log.insert(wrap.clone())?;
        signed.push(wrap);
    }
    Ok(signed)
}

/// Whether this Mac admits through the membership log ([`admit_by_phrase`]): its vault has a log
/// its pin trusts, holding epochs, and the log seats this Mac's device key.
#[must_use]
pub fn admits_by_log(paths: &Paths) -> bool {
    (|| -> crate::Result<bool> {
        let Some(config) = config(paths)? else {
            return Ok(false);
        };
        let Some(pin) = config.signer else {
            return Ok(false);
        };
        let Some((store, _)) = log_entries::trusted_seeded_store(paths, &config, &pin, &[])? else {
            return Ok(false);
        };
        let Some(log) = store.log() else {
            return Ok(false);
        };
        Ok(log_entries::open_local_member_signer(paths, log)?
            .is_some_and(|(actor, _, _)| !log.audience_left_out(&EpochAudience::Everyone, &actor)))
    })()
    .unwrap_or(false)
}

/// The members the trusted membership log at `paths` currently lets in (`Body::Genesis` founders
/// and `Body::Admit` entrants not removed by `Body::Remove`).
#[must_use]
pub fn log_admitted_actors(paths: &Paths) -> BTreeSet<crate::membership_log::MemberId> {
    (|| -> crate::Result<BTreeSet<crate::membership_log::MemberId>> {
        let Some(config) = config(paths)? else {
            return Ok(BTreeSet::new());
        };
        let Some(log) = log_entries::trusted_local_log(paths, &config)? else {
            return Ok(BTreeSet::new());
        };
        let roster = log_entries::local_swarm_roster_for_log(paths, &log)?;
        let membership = log.membership();
        Ok(membership
            .members()
            .iter()
            .filter(|(actor, member)| {
                !log.recipient_is_removed(&member.binding.recipient())
                    && !membership.member_node_removed(member)
                    && !log.audience_left_out(&EpochAudience::Everyone, actor)
                    && !log.audience_left_out_recipient(
                        &EpochAudience::Everyone,
                        &member.binding.recipient(),
                    )
                    && !roster
                        .as_ref()
                        .is_some_and(|r| log_entries::member_denied_by_roster(r, member))
            })
            .map(|(actor, _)| *actor)
            .collect())
    })()
    .unwrap_or_default()
}

/// The bundle this member hands the Mac a run proved ([`members::desk::serve`]'s `bundle`): the
/// membership log with its `Admit` ([`admit_by_phrase`]) where the new Mac reads it and the log
/// path applies, else F10's `[everyone]` files ([`handover_bundle`]). Where this Mac holds a log
/// that removed the new Mac or left it out on purpose, neither: the run ends with the reason. A
/// log path that fails here (a records file this Mac cannot read, a store at its bound or a
/// receiver with incompatible capacity) refuses the handover. Both payloads are built
/// under [`Handing::held`] where the desk holds this Mac's state for the run (it took it before
/// the run touched its phrase), which neither takes again.
///
/// # Errors
/// The log keeps the new Mac out ([`ByLog::KeptOut`]), a vault pass holds this Mac's state past
/// [`super::HANDOVER_LOCK_WAIT`] where the desk holds none for the run ([`ByLog::Busy`]), or
/// the receiver cannot hold the whole log, or neither path can hand a bundle from this Mac's state.
pub fn hand_over(paths: &Paths, handing: &Handing<'_>) -> crate::Result<Handout> {
    match log_path(
        paths,
        handing.request,
        handing.node,
        handing.name,
        aterm_time::unix_millis().unwrap_or(0),
        (Some(handing.reader), handing.restore),
        handing.held,
    )? {
        ByLog::Admitted(admission) => {
            let policy = handing.held.filter(|held| held.holds(paths)).map_or_else(
                || handover::policy_servers(paths),
                |held| held.policy.clone(),
            );
            let bundle = log_bundle(paths, handing.allowed, admission, policy, handing.reader)?;
            return Ok(Handout {
                bundle: bundle.encode()?,
                logged: true,
                note: None,
            });
        }
        ByLog::KeptOut(why) => return Err(Error::new(why)),
        ByLog::Busy => {
            return Err(Error::new(
                "a vault pass on this Mac still holds its state; Allow the new Mac again once it ends",
            ));
        }
        ByLog::NotHere => {}
    }
    Ok(Handout {
        bundle: handover::handover_bundle_held(paths, handing.allowed, handing.held)?.encode()?,
        logged: false,
        note: None,
    })
}

pub(super) fn log_bundle(
    paths: &Paths,
    allowed: &Allowed,
    admission: LogAdmission,
    policy: Vec<String>,
    reader: Reader,
) -> crate::Result<Bundle> {
    if !reader.can_take(admission.source) {
        return Err(Error::new(reader.refusal(admission.source)));
    }
    let config = config(paths)?.ok_or_else(|| Error::new("no vault configured"))?;
    let signer = config
        .signer
        .ok_or_else(|| Error::new("this Mac's vault has no pinned signer"))?;
    let Source::Url(url) = &config.source else {
        return Err(Error::new(
            "this Mac reads its vault from a path, which a new Mac cannot reach",
        ));
    };
    Ok(Bundle {
        vault: config.name.clone(),
        signer,
        followed: config.followed.clone(),
        servers: handover::bundle_servers(url, policy),
        allowed: allowed.clone(),
        handed: Handed::Log {
            source: matches!(reader, Reader::BoundedLog(_)).then_some(admission.source),
            admit: admission.admit,
            records: admission.records,
        },
    })
}

/// Post every record this Mac holds to every vault server it reaches (its configured and browsed
/// servers and the policy's), all of them at once ([`super::fan_out::post_records_to`]): how an
/// admission and a closed wrap gap reach the Macs that do not share its log yet. Servers keep
/// what they lack and ignore the rest. However many servers take the post and never answer, the
/// posts end within one exchange deadline.
///
/// # Errors
/// No vault is configured, the configuration or the records file cannot be read, the crossed
/// mark cannot be looked for ([`super::records_bound`]), or every server refused the records.
pub fn post_records_to_servers(paths: &Paths) -> crate::Result<usize> {
    post_records_to_servers_by(paths, super::fan_out::one_exchange())
}

/// [`post_records_to_servers`], every post ending by `by`: a deadline the caller's other posts
/// share, so that however many rounds of posts a command makes, servers that never answer hold
/// it for that one deadline.
///
/// # Errors
/// As [`post_records_to_servers`].
pub(super) fn post_records_to_servers_by(paths: &Paths, by: Instant) -> crate::Result<usize> {
    let config = config(paths)?.ok_or_else(|| Error::new("no vault configured"))?;
    let store = load_records(paths)?;
    if store.is_empty() {
        return Ok(0);
    }
    let wire = store.encode();
    let mut urls = ledger_publish::candidate_server_urls(paths, &config, &[]);
    for url in policy_peer_urls(paths, &config) {
        if !urls.contains(&url) {
            urls.push(url);
        }
    }
    let bound = super::records_bound(paths, &store)?;
    let mut posted = 0;
    let mut first_err = None;
    for answered in super::fan_out::post_records_to(&urls, &wire, bound, by) {
        match answered {
            Ok(()) => posted += 1,
            Err(err) if first_err.is_none() => first_err = Some(err),
            Err(_) => {}
        }
    }
    if posted == 0
        && let Some(err) = first_err
    {
        return Err(err);
    }
    Ok(posted)
}

/// Write this Mac's vault configuration, records and `~/.secrets` from a member's log bundle
/// ([`Handed::Log`]), once the path to the genesis holds: the records verify into a store for
/// the bundle's vault, its genesis is one the bundle's pin trusts (for ALab, only what this build
/// compiles: the digest or, until F12b, the vault keys it names, never a key the bundle alone
/// names), the log lets this Mac's own key in by the bundle's `Admit`,
/// and the log hands this Mac at least one epoch key. Whichever member signed that `Admit`, the
/// log's own rules made it effective; the member this Mac ran the phrase with may hand an `Admit`
/// another member signed for this Mac on an earlier run. This Mac is then in: it reads its files
/// from the log, with no admin. It keeps the bundle's `Allowed` and its subscriber posts it to its
/// source each hour (`ask_again_if_due`) until an admin's approver names it in `[machines]` and
/// its locker arrives: that approver still names it in `aterm-swarm-roster` and the envelope
/// 0.145 Macs read (the fleet admits it by the log since F17), and a member's one post of it can
/// miss (a split, or a queue that expires before an admin reads it).
///
/// # Errors
/// This Mac is already in a vault, the bundle is not a log bundle for this Mac's key, any check
/// above fails, the source capacity is incompatible, the whole offered and retained history
/// cannot fit, or a write failed. A required crossed mark is stored before any new custody.
pub fn write_from_log_bundle(paths: &Paths, bundle: &Bundle) -> crate::Result<Report> {
    let _lock = StateLock::take(paths)?;
    let CheckedLog {
        config,
        state,
        store,
        entries,
        digest,
        requires_crossed_mark,
    } = checked_log_bundle(paths, bundle)?;
    let publish = || {
        let mut state = state;
        super::records_file_for_write(paths, &store)?;
        save_config(paths, &config)?;
        super::write_records(paths, &store)?;
        let report = apply_entries(paths, &mut state, &entries)?;
        state.membership = Stored::In;
        state.failed = apply_failed(&report);
        state.records_digest = Some(digest);
        state.proof = None;
        state.refused = None;
        state.source_identity = (!state.failed).then(|| source_identity(&config));
        // The first subscriber pass posts the `Allowed` at once.
        state.requested = 0;
        save_state(paths, &state)?;
        handover::keep_allowed(paths, &bundle.allowed)?;
        Ok(report)
    };
    if requires_crossed_mark {
        super::after_crossed_mark(&super::crossing_mark(paths), publish)
    } else {
        publish()
    }
}

/// Whether this Mac takes a member's join bundle, by every check its write runs
/// ([`write_from_log_bundle`] for a log bundle, [`write_from_bundle`] for F10's files), with
/// nothing written: a joining Mac acknowledges a bundle only once this holds, so a bundle it
/// would refuse leaves the member's row allowed and a person's Allow again works at once. The
/// write checks again under the state lock.
///
/// # Errors
/// The write would refuse the bundle: why.
pub fn accepts_bundle(paths: &Paths, bundle: &Bundle) -> crate::Result<()> {
    match &bundle.handed {
        Handed::Log { .. } => checked_log_bundle(paths, bundle).map(drop),
        Handed::Files(_) => handover::checked_files_bundle(paths, bundle).map(drop),
    }
}

/// What a log bundle this Mac takes writes ([`checked_log_bundle`]).
struct CheckedLog {
    config: Config,
    state: State,
    store: membership_log::Store,
    entries: Vec<Entry>,
    digest: [u8; 32],
    requires_crossed_mark: bool,
}

/// [`write_from_log_bundle`]'s checks, writing nothing.
fn checked_log_bundle(paths: &Paths, bundle: &Bundle) -> crate::Result<CheckedLog> {
    let Handed::Log {
        source,
        admit,
        records,
    } = &bundle.handed
    else {
        return Err(Error::new("the member's bundle carries no membership log"));
    };
    let previous = load_records(paths)?;
    let local = records_bound(paths, &previous)?;
    if let Some(why) = crate::members::bundle::source_refusal(local, *source) {
        return Err(Error::new(why));
    }
    let limit = source.map_or(crate::membership_log::LEGACY_RECORDS, |source| {
        (source.held as usize).min(if source.crossed {
            local.held as usize
        } else {
            (local.held as usize).min(crate::membership_log::LEGACY_RECORDS)
        })
    });
    let decoded = crate::membership_log::store::decode_records_bounded(records, limit)?;
    let offered: std::collections::BTreeSet<_> =
        decoded.iter().map(|record| *record.id()).collect();
    let current = config(paths)?;
    let state = load_state(paths)?;
    if current.is_some() && state.membership == Stored::In {
        return Err(Error::new(
            "this Mac is already in a vault; leave it first (aterm secrets leave)",
        ));
    }
    let identity = machine_identity(paths)?;
    if identity.recipient() != bundle.allowed.binding().recipient() {
        return Err(Error::new("the member's bundle allows another Mac's key"));
    }
    if !crate::envelope::valid_name(&bundle.vault)
        || !text::valid_machine_name(bundle.allowed.name())
    {
        return Err(Error::new("the member's join bundle is malformed"));
    }
    let server = bundle
        .servers
        .first()
        .filter(|url| {
            (url.starts_with("http://") || url.starts_with("https://"))
                && url.len() <= 512
                && url.bytes().all(|b| b.is_ascii_graphic())
        })
        .cloned()
        .ok_or_else(|| Error::new("the member's bundle names no vault server"))?;
    let config = Config {
        source: Source::Url(server),
        name: bundle.vault.clone(),
        signer: Some(bundle.signer),
        followed: bundle.followed.clone(),
        host: bundle.allowed.name().to_owned(),
    };
    if !pin_admitted(&config, None) {
        return Err(Error::new(
            "the member's bundle pins a retired vault signer",
        ));
    }
    // ALab's genesis comes only from what this build compiles (`Store::for_vault`: the digest,
    // or until F12b the vault keys it names). `records_store_for` also takes a genesis under the
    // pin and the keys it was reached through, which on an applied config are the envelope's;
    // here they are what the member's bundle says, which nothing has checked yet.
    let mut store = if alab_vault(&config) {
        crate::membership_log::Store::for_vault(crate::ALAB_VAULT)?
    } else {
        records_store_for(Some(&config))?.1
    };
    store
        .ingest_records(decoded)
        .map_err(|error| Error::new(format!("the member's records: {error}")))?;
    let mut carried = std::collections::BTreeSet::new();
    if let (Some(own), Some(incoming)) = (previous.log(), store.log())
        && own.network() == incoming.network()
    {
        carried = previous.inventory();
        let records = own
            .records()
            .chain(carried.iter().filter_map(|id| previous.uncomplete(id)))
            .cloned()
            .collect();
        store.ingest_records(records)?;
    }
    let retained = store.inventory();
    if !offered.is_subset(&retained) || !carried.is_subset(&retained) {
        return Err(Error::new(
            "the member's join bundle cannot be retained in full",
        ));
    }
    let learned_complete = store
        .complete_inventory()
        .difference(&previous.complete_inventory())
        .next()
        .is_some();
    let crossed_source = source.is_some_and(|source| source.crossed);
    if crossed_source && !learned_complete && !retained.is_subset(&previous.inventory()) {
        return Err(Error::new(
            "the crossed join bundle adds no complete records",
        ));
    }
    let log = store
        .log()
        .ok_or_else(|| Error::new("the member's records hold no genesis"))?;
    if !log.has_epochs() || !log_entries::genesis_matches_lineage(log, &config, &bundle.signer, &[])
    {
        return Err(Error::new(
            "the member's records start at a genesis this vault's pin does not trust",
        ));
    }
    let me = bundle.allowed.binding().actor();
    let membership = log.membership();
    let entry = membership
        .get(&me)
        .ok_or_else(|| Error::new("the member's records do not let this Mac in"))?;
    if entry.entry != *admit || &entry.binding != bundle.allowed.binding() {
        return Err(Error::new(
            "the member's records let this Mac in by another Admit than the one it handed",
        ));
    }
    if !log_entries::log_alone_admits(log, &identity)? {
        return Err(Error::new("the member's records hand this Mac no key"));
    }
    let opened = log
        .open_audience_entries(&identity)?
        .ok_or_else(|| Error::new("the member's records hand this Mac no key"))?;
    let mut r =
        log_entries::roster_from_doc_or_opened(None, Some(&opened), Some(log)).unwrap_or_default();
    roster::overlay_log_membership(log, Some(&identity.recipient()), &mut r);
    if log_entries::member_denied_by_roster(&r, entry)
        || log_entries::recipient_denied_by_roster_or_log(Some(&r), log, &identity.recipient())
    {
        return Err(Error::new(
            "the member's records hold a swarm roster that denies this Mac",
        ));
    }
    if *admit != *log.network()
        && let Some(admit_rec) = log.get(admit)
        && membership.get(admit_rec.actor()).is_some_and(|admitter| {
            log_entries::member_denied_by_roster(&r, admitter)
                || log.recipient_is_removed(&admitter.binding.recipient())
                || admitter.node_key.is_some_and(|nk| {
                    nk != [0; 32]
                        && matches!(
                            membership.node(&nk),
                            crate::membership_log::NodeStanding::Removed(_)
                        )
                })
                || log.audience_left_out(&EpochAudience::Everyone, admit_rec.actor())
                || log.audience_left_out_recipient(
                    &EpochAudience::Everyone,
                    &admitter.binding.recipient(),
                )
        })
    {
        return Err(Error::new(
            "the member that signed this Admit is revoked in the vault",
        ));
    }
    let entries = log
        .open_entries(&identity)?
        .ok_or_else(|| Error::new("the member's records hand this Mac no key"))?;
    let digest = log_entries::records_heads_digest(log);
    let requires_crossed_mark =
        (crossed_source && learned_complete) || crate::crossing::past_legacy(log.held_count());
    Ok(CheckedLog {
        config,
        state,
        store,
        entries,
        digest,
        requires_crossed_mark,
    })
}

#[cfg(test)]
#[path = "admit_bounded_tests.rs"]
mod bounded_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm_roster::{SwarmMember, SwarmPolicy, SwarmRoster};

    fn put(
        by_audience: &mut BTreeMap<Audience, BTreeMap<String, Entry>>,
        audience: Audience,
        name: &str,
        value: &str,
    ) {
        by_audience.entry(audience.clone()).or_default().insert(
            name.to_owned(),
            Entry {
                name: name.to_owned(),
                value: crate::SecretBytes::from(value.as_bytes()),
                audience,
                if_absent: false,
                anchor: None,
            },
        );
    }

    /// The label an allowing member gives keeps off what the roster's approver keeps off: a
    /// deny-listed label (a stolen Mac's, which `repair_half_done_revokes` moves off the members),
    /// a `[machines]` name, and the policy's reserved labels wherever the policy lives (a host
    /// list of the admins, as ALab keeps it). The fleet-accounts sheet's numbers are counted
    /// toward "one above the highest" without holding a label by themselves.
    ///
    /// NEGATIVE CONTROL: skip `deny_labels` and `vault_name` and read the policy from
    /// `[everyone]` alone (the code before this fix): `m9`, `m40` and `m50` read free and the
    /// first `taken` assert fails.
    #[test]
    fn a_newcomer_label_keeps_off_denied_reserved_and_machines_names_and_counts_the_sheet() {
        let roster = SwarmRoster {
            members: vec![
                SwarmMember {
                    recipient: "age1synthetic".to_owned(),
                    vault_name: "m40-studio".to_owned(),
                    label: "m41".to_owned(),
                    aliases: Vec::new(),
                    hint: None,
                    hw_hint: None,
                    admitted_ms: 0,
                    claimed_label: Some("m42".to_owned()),
                    claimed_at_ms: Some(1_000),
                    node_id: None,
                    node_key: None,
                },
                SwarmMember {
                    recipient: "age1denied".to_owned(),
                    vault_name: "m98-forged".to_owned(),
                    label: "m9".to_owned(),
                    aliases: Vec::new(),
                    hint: None,
                    hw_hint: None,
                    admitted_ms: 0,
                    claimed_label: Some("m99".to_owned()),
                    claimed_at_ms: Some(1_000),
                    node_id: None,
                    node_key: None,
                },
            ],
            deny_labels: vec!["m9".to_owned()],
            deny_nodes: Vec::new(),
        };
        let policy = SwarmPolicy {
            network_id: None,
            reserved_labels: vec!["m50".to_owned()],
            release_hosts: Vec::new(),
            servers: Vec::new(),
            stewards: Vec::new(),
        };
        let mut by_audience = BTreeMap::new();
        put(
            &mut by_audience,
            Audience::Everyone,
            crate::SWARM_ROSTER_ENTRY,
            &roster.to_toml(),
        );
        put(
            &mut by_audience,
            Audience::Hosts(vec!["m22".to_owned()]),
            crate::SWARM_POLICY_ENTRY,
            &policy.to_toml(),
        );
        put(
            &mut by_audience,
            Audience::Everyone,
            "fleet-accounts.toml",
            "[[account]]\nlabel = \"a\"\nmachines = [\"m70\"]\n",
        );
        let (taken, counted) = labels_taken(&by_audience);
        for held in ["m9", "m40", "m50", "m41", "m42", "m3"] {
            assert!(taken.contains(held), "{held} is taken: {taken:?}");
        }
        assert!(
            !taken.contains("m98") && !taken.contains("m99"),
            "denied row's forged claimed_label or vault_name must not be taken: {taken:?}"
        );
        assert!(!taken.contains("m70"), "a sheet's number holds no label");
        assert!(counted.contains("m70"), "{counted:?}");
    }

    #[test]
    fn kept_out_with_node_refuses_removed_or_already_bound_node_key() {
        use crate::membership_log::{Body, Entrant, Evidence};

        let network = [0x51; 16];
        let authority = [0x52; 32];
        let make_req = |tag: u8| {
            let seed = [tag; 32];
            let inc = [tag; 16];
            let id = Identity::from_secret_bytes([tag.wrapping_add(0x40); 32]);
            let req = Request::create(
                network,
                authority,
                [tag.wrapping_add(0x20); 16],
                inc,
                &id,
                &seed,
            )
            .expect("request");
            (seed, inc, req)
        };

        let (seed1, inc1, req1) = make_req(1);
        let actor1 = req1.binding().actor();
        let node1 = [0xAA; 32];
        let ent1 = Entrant {
            request: req1,
            node_key: node1,
            name: "m1".to_owned(),
            label: "m1".to_owned(),
        };
        let genesis = Record::found(&[9; 32], vec![ent1], 1_000).expect("genesis");
        let mut log = Log::new(genesis).expect("log");

        // A newcomer presenting m1's active node_key under a fresh binding is refused.
        let (_, _, req2) = make_req(2);
        let conflict = kept_out_with_node(&log, req2.binding(), node1, false)
            .expect("reusing active node_key under another binding must be refused");
        assert!(
            conflict.contains("already bound to a different seat"),
            "{conflict}"
        );

        // Admit m2 with node2, then remove m2; a fresh binding reusing node2 must be refused.
        let node2 = [0xBB; 32];
        let actor2 = req2.binding().actor();
        let admit2 = log
            .draft(
                &actor1,
                2_000,
                Body::Admit {
                    entrant: Entrant {
                        request: req2,
                        node_key: node2,
                        name: "m2".to_owned(),
                        label: "m2".to_owned(),
                    },
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&seed1, inc1)
            .unwrap();
        let admit2_id = *admit2.id();
        log.insert(admit2).unwrap();
        let rem2 = log
            .draft(
                &actor1,
                3_000,
                Body::Remove {
                    member: actor2,
                    cutoff: admit2_id,
                },
            )
            .sign(&seed1, inc1)
            .unwrap();
        log.insert(rem2).unwrap();

        let (_, _, req3) = make_req(3);
        let removed_why = kept_out_with_node(&log, req3.binding(), node2, false)
            .expect("removed node_key must be kept out even with a fresh binding");
        assert!(
            removed_why.contains("the vault removed this Mac"),
            "{removed_why}"
        );

        // Even under `restore = true` ("Allow again"), a removed Mac cannot claim a node_key
        // (`node1`) that is already bound to a different active member (`m1`).
        let steal_active = kept_out_with_node(&log, req3.binding(), node1, true)
            .expect("restore must not allow claiming another active member's node_key");
        assert!(
            steal_active.contains("already bound to a different seat"),
            "{steal_active}"
        );

        // Restoring single-member-removed `node2` under `req3` (`restore = true`) is permitted
        // and supersedes the removed seat so `node2` becomes `NodeStanding::Member`.
        assert!(kept_out_with_node(&log, req3.binding(), node2, true).is_none());
        let admit3 = log
            .draft(
                &actor1,
                4_000,
                Body::Admit {
                    entrant: Entrant {
                        request: req3,
                        node_key: node2,
                        name: "m3".to_owned(),
                        label: "m3".to_owned(),
                    },
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&seed1, inc1)
            .unwrap();
        log.insert(admit3).unwrap();
        assert!(
            matches!(
                log.membership().node(&node2),
                crate::membership_log::NodeStanding::Member(_)
            ),
            "restored node_key must become Member, got: {:?}",
            log.membership().node(&node2)
        );
    }

    #[test]
    fn write_from_log_bundle_refuses_admitter_left_out_without_roster_entry() {
        use crate::membership_log::{Body, Entrant, Evidence, encode_records};
        use ring::signature::{Ed25519KeyPair, KeyPair as _};

        let root = aterm_tempfile::tempdir().unwrap();
        let vault_seed = [9u8; 32];
        let vault_signer: [u8; 32] = Ed25519KeyPair::from_seed_unchecked(&vault_seed)
            .unwrap()
            .public_key()
            .as_ref()
            .try_into()
            .unwrap();
        let network = [0x61; 16];
        let authority = [0x62; 32];
        let make_req = |tag: u8, id: &Identity| {
            let seed = [tag; 32];
            let inc = [tag; 16];
            let req = Request::create(
                network,
                authority,
                [tag.wrapping_add(0x20); 16],
                inc,
                id,
                &seed,
            )
            .expect("request");
            (seed, inc, req)
        };

        let id1 = Identity::from_secret_bytes([0x41; 32]);
        let id2 = Identity::from_secret_bytes([0x42; 32]);
        let (seed1, inc1, req1) = make_req(1, &id1);
        let (seed2, inc2, req2) = make_req(2, &id2);
        let actor1 = req1.binding().actor();
        let actor2 = req2.binding().actor();

        let genesis = Record::found(
            &vault_seed,
            vec![
                Entrant {
                    request: req1.clone(),
                    node_key: [0x11; 32],
                    name: "m1".to_owned(),
                    label: "m1".to_owned(),
                },
                Entrant {
                    request: req2.clone(),
                    node_key: [0x22; 32],
                    name: "m2".to_owned(),
                    label: "m2".to_owned(),
                },
            ],
            1_000,
        )
        .expect("genesis");
        let mut log = Log::new(genesis).expect("log");

        // m1 mints `[everyone]` epoch 1, wraps to m1 and m2, and puts `"shared"` (no roster entry).
        let (ep1_draft, ep1_key) = log
            .draft_epoch(&actor1, 1_500, Audience::Everyone, 1)
            .unwrap();
        let ep1 = ep1_draft.sign(&seed1, inc1).unwrap();
        let ep1_id = *ep1.id();
        log.insert(ep1).unwrap();
        for rec in [id1.recipient(), id2.recipient()] {
            let w = log
                .draft_wrap(&actor1, 1_501, ep1_id, &ep1_key, rec)
                .unwrap()
                .sign(&seed1, inc1)
                .unwrap();
            log.insert(w).unwrap();
        }
        let put = log
            .draft_put(
                &actor1,
                1_502,
                ep1_id,
                &ep1_key,
                &Entry {
                    name: "shared".to_owned(),
                    value: crate::SecretBytes::from(&b"v1"[..]),
                    audience: Audience::Everyone,
                    if_absent: false,
                    anchor: None,
                },
            )
            .unwrap()
            .sign(&seed1, inc1)
            .unwrap();
        log.insert(put).unwrap();

        // m2 admits m3 and wraps epoch 1 to m3.
        let m3_ok = Paths::for_home(&root.path().join("m3-ok"));
        let id3 = machine_identity(&m3_ok).unwrap();
        let (_, _, req3) = make_req(3, &id3);
        let admit3 = log
            .draft(
                &actor2,
                2_000,
                Body::Admit {
                    entrant: Entrant {
                        request: req3.clone(),
                        node_key: [0x33; 32],
                        name: "m3".to_owned(),
                        label: "m3".to_owned(),
                    },
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&seed2, inc2)
            .unwrap();
        let admit3_id = *admit3.id();
        log.insert(admit3).unwrap();
        let wrap3_ep1 = log
            .draft_wrap(&actor2, 2_001, ep1_id, &ep1_key, id3.recipient())
            .unwrap()
            .sign(&seed2, inc2)
            .unwrap();
        log.insert(wrap3_ep1).unwrap();

        let allowed = Allowed::sign(req3.binding(), "m3", &seed2).unwrap();
        let make_bundle = |l: &Log| Bundle {
            vault: "test-vault".to_owned(),
            signer: vault_signer,
            followed: Vec::new(),
            servers: vec!["http://127.0.0.1:9/v1/test-vault".to_owned()],
            allowed: allowed.clone(),
            handed: Handed::Log {
                source: None,
                admit: admit3_id,
                records: encode_records(l.records()),
            },
        };

        // Positive control: before m2 is left out of `[everyone]`, m3 accepts the log bundle.
        write_from_log_bundle(&m3_ok, &make_bundle(&log)).expect("active admitter succeeds");

        // Negative control: m1 rotates `[everyone]` to epoch 2, wrapping to m1 and m3 while
        // leaving admitter m2 out (still with no `aterm-swarm-roster` entry in the log).
        let (ep2_draft, ep2_key) = log
            .draft_epoch(&actor1, 3_000, Audience::Everyone, 2)
            .unwrap();
        let ep2 = ep2_draft.sign(&seed1, inc1).unwrap();
        let ep2_id = *ep2.id();
        log.insert(ep2).unwrap();
        for rec in [id1.recipient(), id3.recipient()] {
            let w = log
                .draft_wrap(&actor1, 3_001, ep2_id, &ep2_key, rec)
                .unwrap()
                .sign(&seed1, inc1)
                .unwrap();
            log.insert(w).unwrap();
        }
        let put2 = log
            .draft_put(
                &actor1,
                3_002,
                ep2_id,
                &ep2_key,
                &Entry {
                    name: "shared".to_owned(),
                    value: crate::SecretBytes::from(&b"v2"[..]),
                    audience: Audience::Everyone,
                    if_absent: false,
                    anchor: None,
                },
            )
            .unwrap()
            .sign(&seed1, inc1)
            .unwrap();
        log.insert(put2).unwrap();

        let m3_revoked = Paths::for_home(&root.path().join("m3-revoked"));
        fsx::ensure_private_dir(&m3_revoked.dir).unwrap();
        std::fs::copy(m3_ok.dir.join("identity"), m3_revoked.dir.join("identity")).unwrap();
        let err = write_from_log_bundle(&m3_revoked, &make_bundle(&log))
            .expect_err("admitter left out of Everyone must be refused even without roster entry")
            .to_string();
        assert!(
            err.contains("the member that signed this Admit is revoked in the vault"),
            "{err}"
        );
    }
}
