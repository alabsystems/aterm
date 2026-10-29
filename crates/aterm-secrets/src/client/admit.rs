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
use crate::members::bundle::Handed;
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
    /// The `Admit` of the new Mac: the one this Mac signed, or the one the log already held.
    pub admit: RecordId,
    /// The label the log gives the new Mac.
    pub label: String,
    /// The records this call signed (`Admit` first, then `Wrap`s), possibly none.
    pub signed: Vec<Record>,
    /// This Mac's whole record store afterwards, genesis first.
    pub records: Vec<u8>,
}

/// What C11 keeps a newcomer's label off, from the entries this member reads (`by_audience`,
/// every audience it opens): the labels it may not take (every label, alias, claim and
/// deny-listed label in `aterm-swarm-roster`, each row's `[machines]` name with a leading `m<N>`,
/// and the reserved ones: `m3`, the policy's reserved labels, release hosts and stewards, from
/// whichever audience holds `aterm-swarm-policy`), and the labels it only counts toward "one above
/// the highest" (each `m<N>` a fleet-accounts sheet names, as the approver's `next_free_number`
/// counts them).
fn labels_taken(
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
                taken.extend(member.aliases.iter().cloned());
                taken.extend(member.claimed_label.iter().cloned());
                taken.extend(canonical(&member.vault_name));
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
/// `opened`: the log removed its actor or the age recipient it asks with (a new device key with a
/// removed Mac's machine key is the same Mac), or it is a member that a causally newest
/// `[everyone]` epoch left out on purpose (its minter knew the Mac and wrapped nothing to it: an
/// admin's revoke rotation). F16's restore asks the person first ("removed, Allow again?"); until
/// it does, an Allow does not undo a removal.
pub(super) fn kept_out(log: &Log, binding: &crate::commons::binding::Binding) -> Option<String> {
    let member = binding.actor();
    let recipient = binding.recipient();
    let membership = log.membership();
    if membership.removal(&member).is_some() || log.recipient_is_removed(&recipient) {
        return Some(
            "the vault removed this Mac, and an Allow does not let a removed Mac back in"
                .to_owned(),
        );
    }
    if membership.members().iter().any(|(actor, m)| {
        (*actor == member || m.binding.recipient() == recipient)
            && log.audience_left_out(&EpochAudience::Everyone, actor)
    }) || log.audience_left_out_recipient(&EpochAudience::Everyone, &recipient)
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
            (*actor == member || m.binding.recipient() == recipient) && m.binding != *binding
        })
        .then(|| {
            "this Mac's key is already bound to a different seat in the membership log".to_owned()
        })
}

fn trusted_log_keeps_out(
    paths: &Paths,
    binding: &crate::commons::binding::Binding,
) -> crate::Result<Option<String>> {
    let Some(config) = config(paths)? else {
        return Ok(None);
    };
    let Some(log) = log_entries::trusted_local_log(paths, &config)? else {
        return Ok(None);
    };
    Ok(kept_out(&log, binding))
}

/// The log, keys and opened entries the member at `paths` admits from: its vault's log where the
/// pin trusts it and it holds epochs, and what this Mac's identity opens of it. `None` where it
/// holds no such log or opens nothing of it.
fn held_log(
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
    log_path(paths, request, node, name, now_ms, true)
}

/// [`admit_by_phrase`] where `sign`; otherwise only whether the log keeps the Mac out, read
/// without the state lock (the records file is written whole), for a new Mac that takes F10's
/// files: [`ByLog::KeptOut`] or [`ByLog::NotHere`].
fn log_path(
    paths: &Paths,
    request: &Request,
    node: [u8; 32],
    name: &str,
    now_ms: u64,
    sign: bool,
) -> crate::Result<ByLog> {
    if !sign {
        return Ok(
            trusted_log_keeps_out(paths, request.binding())?.map_or(ByLog::NotHere, ByLog::KeptOut)
        );
    }
    let Some(_lock) = StateLock::try_take_until(paths, Instant::now() + super::HANDOVER_LOCK_WAIT)?
    else {
        return Ok(ByLog::Busy);
    };
    let Some((mut store, mut log, _, opened)) = held_log(paths)? else {
        return Ok(
            trusted_log_keeps_out(paths, request.binding())?.map_or(ByLog::NotHere, ByLog::KeptOut)
        );
    };
    if let Some(why) = kept_out(&log, request.binding()) {
        return Ok(ByLog::KeptOut(why));
    }
    let Some((actor, device, incarnation)) = log_entries::open_local_member_signer(paths, &log)?
    else {
        return Ok(ByLog::NotHere);
    };
    let mut audiences = admission_audiences(&opened);
    // An epoch this Mac lacks as a wrap gap of its own (admitted on one side of a split while
    // the other minted it, before a holder wrapped it to this Mac) is no reason to hand F10's
    // files: the new Mac takes the log with the epochs this Mac holds, and the same holders
    // close that epoch for both ([`close_wrap_gaps`]).
    let holds = |audience: &EpochAudience| {
        Log::admission_epochs(&opened, std::slice::from_ref(audience))
            .iter()
            .all(|(_, epoch)| {
                opened.unwrapped_keys.contains_key(epoch) || log.is_wrap_gap(&opened, epoch, &actor)
            })
    };
    if !holds(&EpochAudience::Everyone)
        || !Log::admission_epochs(&opened, &[EpochAudience::Everyone])
            .iter()
            .any(|(_, epoch)| opened.unwrapped_keys.contains_key(epoch))
    {
        return Ok(ByLog::NotHere);
    }
    // A Commons epoch this Mac cannot open stays closed to the new Mac; a member that holds it
    // closes that gap ([`close_wrap_gaps`]).
    audiences.retain(|audience| holds(audience));

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
            let mut signed = Vec::new();
            for (audience, epoch) in Log::admission_epochs(&opened, &audiences) {
                if log.left_out(&opened, &audience, &member) {
                    continue;
                }
                if opened
                    .wrapped_recipients
                    .get(&epoch)
                    .is_some_and(|wrapped| wrapped.contains(&recipient))
                {
                    continue;
                }
                // One this Mac lacks as its own wrap gap: a holder closes it for both.
                let Some(key) = opened.unwrapped_keys.get(&epoch) else {
                    continue;
                };
                let wrap = log
                    .draft_wrap(&actor, now_ms, epoch, key, recipient)?
                    .sign(signer.seed, signer.incarnation)?;
                log.insert(wrap.clone())?;
                signed.push(wrap);
            }
            return Ok((already.entry, already.label.clone(), signed));
        }
        let (taken, counted) = labels_taken(&opened.by_audience);
        let label = log
            .next_label(name, &taken, &counted)
            .ok_or_else(|| Error::new("no m<N> label up to m99999 is free"))?;
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
        fsx::write_atomic(&paths.records_file(), &store.encode(), 0o600)?;
    }
    Ok(ByLog::Admitted(LogAdmission {
        admit,
        label,
        signed,
        records: store.encode(),
    }))
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
        Ok(log
            .membership()
            .members()
            .keys()
            .copied()
            .filter(|actor| !log.audience_left_out(&EpochAudience::Everyone, actor))
            .collect())
    })()
    .unwrap_or_default()
}

/// The bundle this member hands the Mac a run proved ([`members::desk::serve`]'s `bundle`): the
/// membership log with its `Admit` ([`admit_by_phrase`]) where the new Mac reads it and the log
/// path applies, else F10's `[everyone]` files ([`handover_bundle`]). Where this Mac holds a log
/// that removed the new Mac or left it out on purpose, neither: the run ends with the reason. A
/// log path that fails here (a records file this Mac cannot read, a store at its bound) hands
/// F10's files too, as where no log applies, and says why in [`Handout::note`].
///
/// # Errors
/// The log keeps the new Mac out ([`ByLog::KeptOut`]), a vault pass holds this Mac's state past
/// [`super::HANDOVER_LOCK_WAIT`] ([`ByLog::Busy`]), or neither path can hand a bundle from this
/// Mac's state.
pub fn hand_over(paths: &Paths, handing: &Handing<'_>) -> crate::Result<Handout> {
    let note = match log_path(
        paths,
        handing.request,
        handing.node,
        handing.name,
        aterm_time::unix_millis().unwrap_or(0),
        handing.reads_log,
    ) {
        Ok(ByLog::Admitted(admission)) => {
            let bundle = log_bundle(paths, handing.allowed, admission)?;
            return Ok(Handout {
                bundle: bundle.encode()?,
                logged: true,
                note: None,
            });
        }
        Ok(ByLog::KeptOut(why)) => return Err(Error::new(why)),
        Ok(ByLog::Busy) => {
            return Err(Error::new(
                "a vault pass on this Mac still holds its state; Allow the new Mac again once it ends",
            ));
        }
        Ok(ByLog::NotHere) => None,
        Err(error) => Some(format!("the membership log path failed here: {error}")),
    };
    Ok(Handout {
        bundle: handover_bundle(paths, handing.allowed)?.encode()?,
        logged: false,
        note,
    })
}

fn log_bundle(paths: &Paths, allowed: &Allowed, admission: LogAdmission) -> crate::Result<Bundle> {
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
        servers: handover::bundle_servers(paths, &config, url),
        allowed: allowed.clone(),
        handed: Handed::Log {
            admit: admission.admit,
            records: admission.records,
        },
    })
}

/// Post every record this Mac holds to every vault server it reaches (its configured and browsed
/// servers and the policy's): how an admission and a closed wrap gap reach the Macs that do not
/// share its log yet. Servers keep what they lack and ignore the rest.
///
/// # Errors
/// No vault is configured, or every server refused the records.
pub fn post_records_to_servers(paths: &Paths) -> crate::Result<usize> {
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
    let mut posted = 0;
    let mut first_err = None;
    for url in &urls {
        match transport::post_records(url, &wire) {
            Ok(_) => posted += 1,
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
/// above fails, or a write failed.
pub fn write_from_log_bundle(paths: &Paths, bundle: &Bundle) -> crate::Result<Report> {
    let Handed::Log { admit, records } = &bundle.handed else {
        return Err(Error::new("the member's bundle carries no membership log"));
    };
    let _lock = StateLock::take(paths)?;
    let current = config(paths)?;
    let mut state = load_state(paths)?;
    if current.is_some() && state.membership == Stored::In {
        return Err(Error::new(
            "this Mac is already in a vault; leave it first (aterm secrets leave)",
        ));
    }
    let identity = machine_identity(paths)?;
    if identity.recipient() != bundle.allowed.binding().recipient() {
        return Err(Error::new("the member's bundle allows another Mac's key"));
    }
    let server = bundle
        .servers
        .first()
        .cloned()
        .ok_or_else(|| Error::new("the member's bundle names no vault server"))?;
    let config = Config {
        source: Source::Url(server),
        name: bundle.vault.clone(),
        signer: Some(bundle.signer),
        followed: bundle.followed.clone(),
        host: bundle.allowed.name().to_owned(),
    };
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
        .ingest(records)
        .map_err(|error| Error::new(format!("the member's records: {error}")))?;
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
    let entries = log
        .open_entries(&identity)?
        .ok_or_else(|| Error::new("the member's records hand this Mac no key"))?;
    let digest = log_entries::records_heads_digest(log);
    save_config(paths, &config)?;
    fsx::write_atomic(&paths.records_file(), &store.encode(), 0o600)?;
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
}

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
            members: vec![SwarmMember {
                recipient: "age1synthetic".to_owned(),
                vault_name: "m40-studio".to_owned(),
                label: "m41".to_owned(),
                aliases: Vec::new(),
                hint: None,
                admitted_ms: 0,
                claimed_label: None,
                claimed_at_ms: None,
                node_id: None,
                node_key: None,
            }],
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
        for held in ["m9", "m40", "m50", "m41", "m3"] {
            assert!(taken.contains(held), "{held} is taken: {taken:?}");
        }
        assert!(!taken.contains("m70"), "a sheet's number holds no label");
        assert!(counted.contains("m70"), "{counted:?}");
    }
}
